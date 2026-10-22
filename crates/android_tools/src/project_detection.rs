use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectEcosystems {
    pub android: bool,
    pub multiplatform: bool,
}

impl ProjectEcosystems {
    pub fn qualifies(self) -> bool {
        self.android || self.multiplatform
    }

    fn include(&mut self, other: Self) {
        self.android |= other.android;
        self.multiplatform |= other.multiplatform;
    }
}

pub fn is_project_context_path(path: &str) -> bool {
    !path.split('/').any(|part| {
        matches!(
            part,
            "build" | ".gradle" | ".kotlin" | ".git" | "node_modules"
        )
    }) && (path.ends_with(".gradle")
        || path.ends_with(".gradle.kts")
        || path.ends_with(".toml")
        || (path == "AndroidManifest.xml" || path.ends_with("/AndroidManifest.xml"))
        || matches!(path, "gradlew" | "gradlew.bat" | "gradle.properties"))
}

pub fn detect_project(files: &HashMap<String, String>) -> ProjectEcosystems {
    if ![
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
    ]
    .iter()
    .any(|name| files.contains_key(*name))
    {
        return ProjectEcosystems::default();
    }
    let mut modules = vec![String::new()];
    let mut included_builds = Vec::new();
    for name in ["settings.gradle", "settings.gradle.kts"] {
        if let Some(settings) = files.get(name) {
            let settings_tokens = tokens(settings);
            for (index, token) in settings_tokens.iter().enumerate() {
                if token == &Token::Word("includeBuild".into()) {
                    let argument = index
                        + if settings_tokens.get(index + 1) == Some(&Token::Symbol('(')) {
                            2
                        } else {
                            1
                        };
                    if let Some(Token::Text(path)) = settings_tokens.get(argument)
                        && !path.starts_with('/')
                        && !path.split('/').any(|part| part == "..")
                    {
                        let path = path.trim_start_matches("./").trim_end_matches('/');
                        if !path.is_empty() {
                            included_builds.push(path.to_owned());
                        }
                    }
                }
                if token != &Token::Word("include".into()) {
                    continue;
                }
                for token in settings_tokens.iter().skip(index + 1) {
                    match token {
                        Token::Text(module) => {
                            modules.push(module.trim_start_matches(':').replace(':', "/"))
                        }
                        Token::Symbol('(' | ',') => {}
                        _ => break,
                    }
                }
            }
            for window in settings_tokens.windows(10) {
                if let [
                    Token::Word(project),
                    Token::Symbol('('),
                    Token::Text(module),
                    Token::Symbol(')'),
                    Token::Symbol('.'),
                    Token::Word(directory),
                    Token::Symbol('='),
                    Token::Word(file),
                    Token::Symbol('('),
                    Token::Text(path),
                ] = window
                    && project == "project"
                    && directory == "projectDir"
                    && file == "file"
                    && !path.starts_with('/')
                    && !path.split('/').any(|part| part == "..")
                {
                    let original = module.trim_start_matches(':').replace(':', "/");
                    modules.retain(|module| module != &original);
                    modules.push(
                        path.trim_start_matches("./")
                            .trim_end_matches('/')
                            .to_owned(),
                    );
                }
            }
        }
    }
    let owned_build = |path: &str| {
        modules.iter().any(|module| {
            let prefix = if module.is_empty() {
                String::new()
            } else {
                format!("{module}/")
            };
            path == format!("{prefix}build.gradle") || path == format!("{prefix}build.gradle.kts")
        })
    };
    let mut aliases = HashMap::new();
    for (path, text) in files {
        if !path.ends_with(".toml")
            || path
                .rsplit_once('/')
                .is_some_and(|(parent, _)| parent != "gradle")
        {
            continue;
        }
        let Ok(catalog) = text.parse::<toml::Value>() else {
            continue;
        };
        let Some(plugins) = catalog.get("plugins").and_then(toml::Value::as_table) else {
            continue;
        };
        let name = path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".versions.toml")
            .trim_end_matches(".toml");
        let catalog_name = name;
        for (alias, plugin) in plugins {
            let id = plugin
                .get("id")
                .and_then(toml::Value::as_str)
                .or_else(|| plugin.as_str().and_then(|plugin| plugin.split(':').next()));
            if let Some(id) = id {
                aliases.insert(
                    format!("{catalog_name}.plugins.{}", alias.replace(['-', '_'], ".")),
                    id.to_owned(),
                );
            }
        }
    }
    let mut conventions = HashMap::new();
    for (path, text) in files {
        if (path.starts_with("buildSrc/src/main/")
            || included_builds
                .iter()
                .any(|build| path.starts_with(&format!("{build}/")) && path.contains("/src/main/")))
            && (path.ends_with(".gradle.kts") || path.ends_with(".gradle"))
        {
            let name = path
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .trim_end_matches(".kts")
                .trim_end_matches(".gradle");
            conventions.insert(name.to_owned(), applied_plugins(text, &aliases));
        }
    }
    let mut result = ProjectEcosystems::default();
    let mut has_build = false;
    for (path, text) in files {
        if !owned_build(path) {
            continue;
        }
        if path.starts_with("buildSrc/") || path.starts_with("build-logic/") {
            continue;
        }
        has_build = true;
        let mut pending = applied_plugins(text, &aliases);
        let mut seen = std::collections::HashSet::new();
        while let Some(plugin) = pending.pop() {
            if !seen.insert(plugin.clone()) {
                continue;
            }
            result.include(plugin_ecosystems(&plugin));
            if let Some(plugins) = conventions.get(&plugin) {
                pending.extend(plugins.iter().cloned());
            }
        }
    }
    // Source manifests also cover projects whose Android plugin is applied by build logic.
    result.android |= has_build
        && files.keys().any(|path| {
            modules.iter().any(|module| {
                let prefix = if module.is_empty() {
                    String::new()
                } else {
                    format!("{module}/")
                };
                path.strip_prefix(&prefix).is_some_and(|relative| {
                    relative == "AndroidManifest.xml"
                        || relative.starts_with("src/")
                            && relative.ends_with("/AndroidManifest.xml")
                })
            })
        });
    for build in included_builds {
        let prefix = format!("{build}/");
        let included = files
            .iter()
            .filter_map(|(path, text)| {
                path.strip_prefix(&prefix)
                    .map(|path| (path.to_owned(), text.clone()))
            })
            .collect();
        result.include(detect_project(&included));
    }
    result
}

fn plugin_ecosystems(plugin: &str) -> ProjectEcosystems {
    ProjectEcosystems {
        android: matches!(
            plugin,
            "com.android.application"
                | "com.android.library"
                | "com.android.test"
                | "com.android.dynamic-feature"
                | "com.android.kotlin.multiplatform.library"
        ),
        multiplatform: matches!(
            plugin,
            "org.jetbrains.kotlin.multiplatform" | "org.jetbrains.compose"
        ),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Text(String),
    Symbol(char),
}

fn tokens(text: &str) -> Vec<Token> {
    let mut chars = text.chars().peekable();
    let mut result = Vec::new();
    while let Some(character) = chars.next() {
        match character {
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut depth = 1;
                while let Some(next) = chars.next() {
                    if next == '/' && chars.peek() == Some(&'*') {
                        chars.next();
                        depth += 1;
                    }
                    if next == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            '\'' | '"' => {
                let quote = character;
                let mut value = String::new();
                while let Some(next) = chars.next() {
                    if next == quote {
                        break;
                    }
                    if next == '\\' {
                        if let Some(escaped) = chars.next() {
                            value.push(escaped);
                        }
                    } else {
                        value.push(next);
                    }
                }
                result.push(Token::Text(value));
            }
            next if next.is_alphanumeric() || next == '_' => {
                let mut word = String::from(next);
                while chars
                    .peek()
                    .is_some_and(|next| next.is_alphanumeric() || matches!(next, '_' | '.' | '-'))
                {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                result.push(Token::Word(word));
            }
            next if next.is_whitespace() => {}
            next => result.push(Token::Symbol(next)),
        }
    }
    result
}

fn applied_plugins(text: &str, aliases: &HashMap<String, String>) -> Vec<String> {
    let tokens = tokens(text);
    let mut result = Vec::new();
    let mut blocks = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token == &Token::Symbol('{') {
            blocks.push(
                tokens
                    .get(index.wrapping_sub(1))
                    .and_then(|token| match token {
                        Token::Word(word) => Some(word.as_str()),
                        _ => None,
                    })
                    .unwrap_or_default(),
            );
        } else if token == &Token::Symbol('}') {
            blocks.pop();
        }
        if blocks.contains(&"pluginManagement") || blocks.contains(&"dependencies") {
            continue;
        }
        let Token::Word(word) = token else {
            continue;
        };
        let in_plugins = blocks.last() == Some(&"plugins");
        let parenthesized = tokens.get(index + 1) == Some(&Token::Symbol('('));
        let argument_index = index + if parenthesized { 2 } else { 1 };
        let Some(argument) = tokens.get(argument_index) else {
            continue;
        };
        if !((in_plugins && matches!(word.as_str(), "id" | "kotlin" | "alias"))
            || word == "apply"
            || (word == "plugin"
                && index > 0
                && tokens.get(index - 1) == Some(&Token::Word("apply".into()))))
        {
            continue;
        }
        let mut end = argument_index + 1;
        if parenthesized && tokens.get(end) == Some(&Token::Symbol(')')) {
            end += 1;
        }
        let mut depth = 0usize;
        let mut unapplied = false;
        for offset in end..tokens.len() {
            match tokens.get(offset) {
                Some(Token::Symbol('(')) => depth += 1,
                Some(Token::Symbol(')')) => depth = depth.saturating_sub(1),
                Some(Token::Symbol('{' | '}' | ';')) if depth == 0 => break,
                Some(Token::Word(word))
                    if depth == 0 && matches!(word.as_str(), "id" | "alias" | "kotlin") =>
                {
                    break;
                }
                Some(Token::Word(word)) if depth == 0 && word == "apply" => {
                    let next = offset
                        + if tokens.get(offset + 1) == Some(&Token::Symbol('(')) {
                            2
                        } else {
                            1
                        };
                    unapplied = tokens.get(next) == Some(&Token::Word("false".into()));
                    break;
                }
                _ => {}
            }
        }
        if unapplied {
            continue;
        }
        let plugin = match (word.as_str(), argument) {
            ("id", Token::Text(id)) if in_plugins => Some(id.clone()),
            ("apply", Token::Word(name))
                if parenthesized
                    && name == "plugin"
                    && tokens.get(argument_index + 1) == Some(&Token::Symbol('=')) =>
            {
                tokens.get(argument_index + 2).and_then(|token| {
                    if let Token::Text(id) = token {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
            }
            ("kotlin", Token::Text(name)) if in_plugins => {
                Some(format!("org.jetbrains.kotlin.{name}"))
            }
            ("alias", Token::Word(alias)) if in_plugins => aliases.get(alias).cloned(),
            ("plugin", Token::Symbol(':'))
                if index > 0 && tokens.get(index - 1) == Some(&Token::Word("apply".into())) =>
            {
                tokens.get(index + 2).and_then(|token| {
                    if let Token::Text(id) = token {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
            }
            _ => None,
        };
        if let Some(plugin) = plugin {
            result.push(plugin);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(build: &str) -> ProjectEcosystems {
        detect_project(&HashMap::from([("build.gradle.kts".into(), build.into())]))
    }

    #[test]
    fn generic_projects_and_unapplied_declarations_do_not_qualify() {
        for build in [
            "",
            "plugins { kotlin(\"jvm\") }",
            "// plugins { id(\"com.android.application\") }",
            "/* plugins { id(\"org.jetbrains.compose\") } */",
            "plugins { id(\"com.android.library\") version \"9\"\n apply false }",
            "plugins { id(\"org.jetbrains.compose\").version(\"1\").apply(false) }",
            "plugins { id(\"com.android.application\") version libs.versions.agp.get() apply false }",
            "plugins { id(\"com.android.application\").version(libs.versions.agp.get()).apply(false) }",
            "dependencies { implementation(\"com.android.application\") }",
            "pluginManagement { plugins { id(\"com.android.application\") } }",
        ] {
            assert_eq!(detect(build), ProjectEcosystems::default(), "{build}");
        }
        assert!(
            !detect_project(&HashMap::from([
                ("gradlew".into(), String::new()),
                ("settings.gradle.kts".into(), String::new()),
                ("main.py".into(), String::new())
            ]))
            .qualifies()
        );
    }

    #[test]
    fn nested_examples_do_not_qualify_non_gradle_roots() {
        let files = HashMap::from([
            ("Cargo.toml".into(), String::new()),
            (
                "examples/android/app/build.gradle.kts".into(),
                "plugins { id(\"com.android.application\") }".into(),
            ),
            (
                "examples/android/app/src/main/AndroidManifest.xml".into(),
                String::new(),
            ),
        ]);
        assert!(!detect_project(&files).qualifies());
        let mut files = files;
        files.insert(
            "build.gradle.kts".into(),
            "plugins { kotlin(\"jvm\") }".into(),
        );
        assert!(!detect_project(&files).qualifies());
        files.insert(
            "build.gradle.kts".into(),
            "plugins { alias(libs.plugins.application) }".into(),
        );
        files.insert(
            "gradle/libs.versions.toml".into(),
            "[plugins]\napplication = 'org.jetbrains.kotlin.jvm:2'".into(),
        );
        files.insert(
            "examples/android/gradle/libs.versions.toml".into(),
            "[plugins]\napplication = 'com.android.application:9'".into(),
        );
        files.insert(
            "examples/android/buildSrc/src/main/kotlin/jvm.gradle.kts".into(),
            "plugins { id(\"com.android.library\") }".into(),
        );
        assert!(!detect_project(&files).qualifies());
    }

    #[test]
    fn applied_android_and_multiplatform_plugins_qualify() {
        for build in [
            "plugins { id(\"com.android.application\") }",
            "plugins { id 'com.android.library' }",
            "apply plugin: 'com.android.test'",
        ] {
            assert!(detect(build).android, "{build}");
        }
        assert!(detect("apply(plugin = \"com.android.library\")").android);
        for build in [
            "plugins { kotlin(\"multiplatform\") }",
            "plugins { id(\"org.jetbrains.compose\") }",
            "plugins { id 'org.jetbrains.kotlin.multiplatform' }",
        ] {
            let result = detect(build);
            assert!(result.qualifies(), "{build}");
            assert!(!result.android, "{build}");
        }
    }

    #[test]
    fn aliases_conventions_and_manifest_projects_qualify() {
        let mut files = HashMap::from([
            ("settings.gradle.kts".into(), "include(\":app\")".into()),
            ("gradle/libs.versions.toml".into(), "[plugins]\nandroid-application = { id = 'com.android.application', version = '9' }\ncompose = 'org.jetbrains.compose:1'".into()),
            ("app/build.gradle.kts".into(), "plugins { alias(libs.plugins.android.application) }".into()),
        ]);
        assert!(detect_project(&files).android);
        files.insert(
            "app/build.gradle.kts".into(),
            "plugins { alias(libs.plugins.android.application) apply false }".into(),
        );
        assert!(!detect_project(&files).qualifies());
        files.insert(
            "app/build.gradle.kts".into(),
            "plugins { alias(libs.plugins.compose) }".into(),
        );
        assert!(detect_project(&files).multiplatform);
        assert!(!detect_project(&files).android);
        files.insert(
            "app/build.gradle.kts".into(),
            "plugins { id(\"koda.android\") }".into(),
        );
        files.insert(
            "buildSrc/src/main/kotlin/koda.android.gradle.kts".into(),
            "plugins { id(\"com.android.library\") }".into(),
        );
        assert!(detect_project(&files).android);
        files.insert("app/build.gradle.kts".into(), String::new());
        files.insert(
            "app/src/main/AndroidManifest.xml".into(),
            "<manifest/>".into(),
        );
        assert!(detect_project(&files).android);
    }
}
