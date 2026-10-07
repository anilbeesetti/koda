// Discovery and registration behavior adapted from Android Open Source Project
// GradleModuleImporter, GradleSiblingLookup and GradleProjectDependencyParser
// (Copyright 2014 AOSP, Apache-2.0). Unchanged sources and license are retained in
// ../test_data/gradle_import. This adapter reads literal declarations, not arbitrary
// Gradle programs; unsupported settings/dependency expressions return errors.

use anyhow::{Context as _, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Write as _,
    path::{Component, Path, PathBuf},
};

/// A pre-sync import selection. Missing sources remain visible until import validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradleImportPlan {
    pub modules: BTreeMap<String, Option<PathBuf>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GradleImportResult {
    pub modules: BTreeMap<String, PathBuf>,
    pub settings_file: PathBuf,
}

/// Discover literal Gradle includes and project dependencies without invoking Gradle.
///
/// Settings support `include` and `project(...).projectDir = new File(...)`/`file(...)`,
/// plus a literal root project name. Dependencies support literal external artifacts
/// and literal `project(...)` references in a top-level dependencies block. Dynamic
/// declarations and applied scripts require the future evaluated Gradle adapter.
/// Other accepted build statements are literal plugin application and the opaque
/// `plugins`, `android`, and `repositories` blocks. Other top-level code is rejected.
pub fn discover_gradle_modules(source: &Path) -> Result<GradleImportPlan> {
    let source = source
        .canonicalize()
        .context("Cannot locate import source")?;
    ensure!(source.is_dir(), "Import source is not a directory");
    if let Some(settings_file) = settings_file(&source)? {
        let settings = read_settings(&settings_file)?;
        let modules = settings.locations(&source)?;
        // Upstream root discovery returns exactly the included modules, rather than
        // expanding the root project's own dependencies or adding the root itself.
        return Ok(GradleImportPlan { modules });
    }

    let mut siblings = None;
    let mut dependencies = BTreeMap::new();
    let mut pending = VecDeque::from([source.clone()]);
    let mut analyzed = BTreeSet::new();
    while let Some(directory) = pending.pop_front() {
        if !analyzed.insert(directory.clone()) {
            continue;
        }
        for dependency in read_dependencies(&directory)? {
            if dependencies.contains_key(&dependency) {
                continue;
            }
            if siblings.is_none() {
                siblings = Some(find_siblings(&source)?);
            }
            let location = siblings
                .as_ref()
                .and_then(|siblings| siblings.get(&dependency))
                .cloned()
                .flatten();
            if let Some(location) = &location {
                pending.push_back(location.clone());
            }
            dependencies.insert(dependency, location);
        }
    }
    // Preserve upstream's lazy sibling lookup: without dependencies, a nested
    // source is named after its final directory, even if settings assigns a name.
    let name = siblings
        .as_ref()
        .and_then(|siblings| {
            siblings
                .iter()
                .find(|(_, location)| location.as_ref() == Some(&source))
                .map(|(name, _)| name.clone())
        })
        .map(Ok)
        .unwrap_or_else(|| {
            module_name(
                source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("Import source has no UTF-8 module name")?,
            )
        })?;
    dependencies.insert(name, Some(source));
    Ok(GradleImportPlan {
        modules: dependencies,
    })
}

/// Copy external modules and register modules already inside the destination.
///
/// Existing unrelated destination directories and symlinks are rejected. All
/// copies are staged before publishing; a failure rolls back published copies.
/// The caller must schedule project sync after successful registration.
/// Run this filesystem work on a background executor and serialize destination edits.
pub fn import_gradle_modules(
    plan: &GradleImportPlan,
    destination: &Path,
) -> Result<GradleImportResult> {
    let missing = plan
        .modules
        .iter()
        .filter(|(_, location)| location.is_none())
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    match missing.as_slice() {
        [] => {}
        [name] => bail!("Sources for module '{name}' were not found"),
        names => bail!(
            "Sources were not found for modules '{}'",
            names.join("', '")
        ),
    }
    let destination = destination
        .canonicalize()
        .context("Cannot locate destination project")?;
    ensure!(destination.is_dir(), "Destination is not a directory");
    let settings_file =
        settings_file(&destination)?.unwrap_or_else(|| destination.join("settings.gradle"));
    ensure!(
        !settings_file.is_symlink(),
        "Settings file must not be a symlink"
    );
    let settings_existed = settings_file.exists();
    let original_settings = if settings_existed {
        fs::read_to_string(&settings_file)?
    } else {
        String::new()
    };
    let settings = parse_settings(&original_settings)
        .with_context(|| format!("Cannot register modules in {}", settings_file.display()))?;
    let mut modules = BTreeMap::new();
    let mut copies = Vec::new();
    for (name, source) in &plan.modules {
        let name = module_name(name)?;
        let source = source
            .as_ref()
            .context("Missing module source")?
            .canonicalize()?;
        ensure!(source.is_dir(), "Module {name} is not a directory");
        ensure!(source != destination, "Cannot import a project into itself");
        ensure!(
            !destination.starts_with(&source),
            "Import source contains the destination project"
        );
        read_dependencies(&source)?;
        let target = if source.starts_with(&destination) {
            source.clone()
        } else {
            let target = destination.join(module_relative_path(&name)?);
            ensure!(
                !target.exists() && !target.is_symlink(),
                "Import destination already exists: {}",
                target.display()
            );
            validate_parent_paths(&target, &destination)?;
            copies.push((source, target.clone()));
            target
        };
        if settings.includes.contains(&name) {
            let registered = destination.join(
                settings
                    .directories
                    .get(&name)
                    .cloned()
                    .map(Ok)
                    .unwrap_or_else(|| module_relative_path(&name))?,
            );
            let registered = if registered.exists() {
                registered.canonicalize()?
            } else {
                registered
            };
            ensure!(
                registered == target,
                "Module {name} is already registered at another location: {}",
                registered.display()
            );
        }
        ensure!(
            modules.insert(name.clone(), target).is_none(),
            "Duplicate module name: {name}"
        );
    }
    for (_, target) in &copies {
        ensure!(
            !modules
                .values()
                .any(|other| other != target
                    && (other.starts_with(target) || target.starts_with(other))),
            "Imported module destinations overlap: {}",
            target.display()
        );
    }
    let updated_settings = register_settings(
        &original_settings,
        &settings,
        &modules,
        &destination,
        settings_file
            .extension()
            .is_some_and(|extension| extension == "kts"),
    )?;
    let staging = tempfile::Builder::new()
        .prefix(".koda-module-import-")
        .tempdir_in(&destination)?;
    let mut staged_copies = Vec::new();
    for (index, (source, target)) in copies.iter().enumerate() {
        let staged = staging.path().join(index.to_string());
        copy_directory(source, &staged)?;
        staged_copies.push((staged, target.clone()));
    }
    let mut settings_temporary = tempfile::NamedTempFile::new_in(&destination)?;
    settings_temporary.write_all(updated_settings.as_bytes())?;
    settings_temporary.flush()?;
    if settings_file.exists() {
        settings_temporary
            .as_file()
            .set_permissions(fs::metadata(&settings_file)?.permissions())?;
    }
    publish_import(
        &destination,
        staged_copies,
        settings_temporary,
        &settings_file,
        &original_settings,
        settings_existed,
        updated_settings != original_settings,
    )?;
    Ok(GradleImportResult {
        modules,
        settings_file,
    })
}

enum PublishedPath {
    Module(PathBuf),
    Parent(PathBuf),
}

fn publish_import(
    destination: &Path,
    staged_copies: Vec<(PathBuf, PathBuf)>,
    settings_temporary: tempfile::NamedTempFile,
    settings_file: &Path,
    original_settings: &str,
    settings_existed: bool,
    settings_changed: bool,
) -> Result<()> {
    let mut created = Vec::new();
    let publish = (|| -> Result<()> {
        ensure!(
            settings_file.exists() == settings_existed
                && (!settings_existed || fs::read_to_string(settings_file)? == original_settings),
            "Gradle settings changed during import; retry discovery"
        );
        for (staged, target) in staged_copies {
            create_parent_paths(&target, destination, &mut created)?;
            ensure!(
                !target.exists() && !target.is_symlink(),
                "Import destination appeared during import"
            );
            fs::rename(staged, &target)?;
            created.push(PublishedPath::Module(target));
        }
        if settings_changed || !settings_existed {
            settings_temporary
                .persist(settings_file)
                .map_err(|error| error.error)?;
        }
        Ok(())
    })();
    if let Err(error) = publish {
        let mut failures = Vec::new();
        for published in created.iter().rev() {
            let (path, removal) = match published {
                PublishedPath::Module(path) => (path, fs::remove_dir_all(path)),
                PublishedPath::Parent(path) => (path, fs::remove_dir(path)),
            };
            if let Err(removal) = removal {
                failures.push(format!("{}: {removal}", path.display()));
            }
        }
        if !failures.is_empty() {
            return Err(error.context(format!("Import rollback failed: {}", failures.join("; "))));
        }
        return Err(error);
    }
    Ok(())
}

fn settings_file(directory: &Path) -> Result<Option<PathBuf>> {
    let groovy = directory.join("settings.gradle");
    let kotlin = directory.join("settings.gradle.kts");
    for path in [&groovy, &kotlin] {
        ensure!(
            !(path.exists() || path.is_symlink()) || path.is_file(),
            "Gradle settings is not a readable regular file: {}",
            path.display()
        );
    }
    ensure!(
        !(groovy.exists() && kotlin.exists()),
        "Ambiguous Gradle settings: both Groovy and Kotlin files exist"
    );
    Ok([groovy, kotlin].into_iter().find(|path| path.is_file()))
}

fn find_siblings(source: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
    for directory in source.ancestors() {
        if let Some(settings_file) = settings_file(directory)? {
            return read_settings(&settings_file)?.locations(directory);
        }
    }
    Ok(BTreeMap::new())
}

fn module_name(name: &str) -> Result<String> {
    let name = name.strip_prefix(':').unwrap_or(name);
    ensure!(
        !name.is_empty()
            && name.split(':').all(|component| {
                !component.is_empty()
                    && !matches!(component, "." | "..")
                    && !component
                        .chars()
                        .any(|character| character.is_control() || matches!(character, '/' | '\\'))
            }),
        "Invalid Gradle module name: {name}"
    );
    Ok(format!(":{name}"))
}

fn module_relative_path(name: &str) -> Result<PathBuf> {
    Ok(module_name(name)?
        .trim_start_matches(':')
        .split(':')
        .collect())
}

#[derive(Default)]
struct Settings {
    includes: BTreeSet<String>,
    directories: BTreeMap<String, PathBuf>,
}

impl Settings {
    fn locations(&self, directory: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
        self.includes
            .iter()
            .filter(|name| name.as_str() != ":")
            .map(|name| {
                let path = self
                    .directories
                    .get(name)
                    .cloned()
                    .map(Ok)
                    .unwrap_or_else(|| module_relative_path(name))?;
                let path = directory.join(path);
                let location = if path.is_dir() {
                    Some(path.canonicalize()?)
                } else {
                    None
                };
                Ok((name.clone(), location))
            })
            .collect()
    }
}

fn read_settings(path: &Path) -> Result<Settings> {
    parse_settings(&fs::read_to_string(path)?)
        .with_context(|| format!("Unsupported Gradle settings in {}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Name(String),
    String(String),
    Symbol(char),
    Newline,
}

fn lex(source: &str) -> Result<Vec<Token>> {
    let mut characters = source.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(character) = characters.next() {
        match character {
            '\n' => tokens.push(Token::Newline),
            character if character.is_whitespace() => {}
            '/' if characters.peek() == Some(&'/') => {
                characters.next();
                for character in characters.by_ref() {
                    if character == '\n' {
                        tokens.push(Token::Newline);
                        break;
                    }
                }
            }
            '/' if characters.peek() == Some(&'*') => {
                characters.next();
                let mut previous = '\0';
                let mut closed = false;
                for character in characters.by_ref() {
                    if previous == '*' && character == '/' {
                        closed = true;
                        break;
                    }
                    if character == '\n' {
                        tokens.push(Token::Newline);
                    }
                    previous = character;
                }
                ensure!(closed, "Unterminated Gradle block comment");
            }
            quote @ ('\'' | '"') => {
                let mut value = String::new();
                let mut closed = false;
                while let Some(character) = characters.next() {
                    if character == quote {
                        closed = true;
                        break;
                    }
                    if character == '\\' {
                        let escaped = characters.next().context("Unterminated Gradle string")?;
                        value.push(match escaped {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            '\\' | '\'' | '"' | '$' => escaped,
                            _ => bail!("Unsupported Gradle string escape: \\{escaped}"),
                        });
                    } else {
                        ensure!(
                            character != '\n',
                            "Multiline Gradle strings are unsupported"
                        );
                        ensure!(
                            quote != '"' || character != '$',
                            "Interpolated Gradle strings require evaluated discovery"
                        );
                        value.push(character);
                    }
                }
                ensure!(closed, "Unterminated Gradle string");
                tokens.push(Token::String(value));
            }
            character if character.is_alphanumeric() || matches!(character, '_' | '$') => {
                let mut name = String::from(character);
                while characters.peek().is_some_and(|character| {
                    character.is_alphanumeric() || matches!(character, '_' | '$')
                }) {
                    if let Some(character) = characters.next() {
                        name.push(character);
                    }
                }
                tokens.push(Token::Name(name));
            }
            character => tokens.push(Token::Symbol(character)),
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn new(source: &str) -> Result<Self> {
        Ok(Self {
            tokens: lex(source)?,
            position: 0,
        })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn consume(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn symbol(&mut self, symbol: char) -> bool {
        self.consume(&Token::Symbol(symbol))
    }

    fn name(&mut self, name: &str) -> bool {
        self.consume(&Token::Name(name.into()))
    }

    fn expect_symbol(&mut self, symbol: char) -> Result<()> {
        ensure!(
            self.symbol(symbol),
            "Expected '{symbol}', found {:?}",
            self.peek()
        );
        Ok(())
    }

    fn string(&mut self) -> Result<String> {
        match self.peek() {
            Some(Token::String(value)) => {
                let value = value.clone();
                self.position += 1;
                Ok(value)
            }
            token => bail!("Expected literal Gradle string, found {token:?}"),
        }
    }

    fn separators(&mut self) {
        while self.consume(&Token::Newline) || self.symbol(';') {}
    }

    fn newlines(&mut self) {
        while self.consume(&Token::Newline) {}
    }

    fn end_statement(&mut self) -> Result<()> {
        ensure!(
            self.peek().is_none()
                || matches!(self.peek(), Some(Token::Newline | Token::Symbol(';' | '}'))),
            "Unsupported Gradle expression following literal declaration: {:?}",
            self.peek()
        );
        self.separators();
        Ok(())
    }
}

fn parse_settings(source: &str) -> Result<Settings> {
    let mut parser = Parser::new(source)?;
    let mut settings = Settings::default();
    parser.separators();
    while parser.peek().is_some() {
        if parser.name("include") {
            let parenthesized = parser.symbol('(');
            parser.newlines();
            loop {
                let name = parser.string()?;
                settings.includes.insert(if name == ":" {
                    name
                } else {
                    module_name(&name)?
                });
                if parenthesized {
                    parser.newlines();
                }
                if !parser.symbol(',') {
                    break;
                }
                parser.newlines();
                if parenthesized && parser.peek() == Some(&Token::Symbol(')')) {
                    break;
                }
            }
            if parenthesized {
                parser.expect_symbol(')')?;
            }
        } else if parser.name("project") {
            parser.expect_symbol('(')?;
            let name = module_name(&parser.string()?)?;
            parser.expect_symbol(')')?;
            parser.expect_symbol('.')?;
            ensure!(
                parser.name("projectDir"),
                "Only projectDir assignments are supported"
            );
            parser.expect_symbol('=')?;
            let constructor = parser.name("new");
            ensure!(
                if constructor {
                    parser.name("File")
                } else {
                    parser.name("file") || parser.name("File")
                },
                "Expected literal file(...) or new File(...)"
            );
            parser.expect_symbol('(')?;
            let path = parser.string()?;
            parser.expect_symbol(')')?;
            ensure!(!path.is_empty(), "Empty Gradle project directory");
            settings.directories.insert(name, PathBuf::from(path));
        } else if parser.name("rootProject") {
            parser.expect_symbol('.')?;
            ensure!(
                parser.name("name"),
                "Only a literal rootProject.name is supported"
            );
            parser.expect_symbol('=')?;
            parser.string()?;
        } else {
            bail!(
                "Unsupported settings expression {:?}; evaluated Gradle discovery is required",
                parser.peek()
            );
        }
        parser.end_statement()?;
    }
    Ok(settings)
}

fn read_dependencies(directory: &Path) -> Result<BTreeSet<String>> {
    let groovy = directory.join("build.gradle");
    let kotlin = directory.join("build.gradle.kts");
    ensure!(
        !(groovy.exists() && kotlin.exists()),
        "Ambiguous Gradle build files"
    );
    let Some(path) = [groovy, kotlin].into_iter().find(|path| path.is_file()) else {
        return Ok(BTreeSet::new());
    };
    parse_dependencies(&fs::read_to_string(&path)?)
        .with_context(|| format!("Unsupported dependencies in {}", path.display()))
}

fn parse_dependencies(source: &str) -> Result<BTreeSet<String>> {
    let mut parser = Parser::new(source)?;
    let mut depth = 0usize;
    let mut parentheses = 0usize;
    let mut dependencies = BTreeSet::new();
    while let Some(token) = parser.peek().cloned() {
        if depth == 0 {
            match &token {
                Token::Name(name)
                    if matches!(name.as_str(), "plugins" | "android" | "repositories") =>
                {
                    parser.position += 1;
                    parser.newlines();
                    parser.expect_symbol('{')?;
                    depth = 1;
                    continue;
                }
                Token::Name(name) if matches!(name.as_str(), "dependencies" | "apply") => {}
                Token::Newline | Token::Symbol(';') => {}
                _ => bail!(
                    "Unsupported top-level Gradle statement {token:?}; evaluated dependency discovery is required"
                ),
            }
        }
        ensure!(
            depth != 0
                || !matches!(token, Token::Name(ref name) if matches!(name.as_str(), "if" | "else" | "for" | "while" | "when" | "return" | "throw" | "switch" | "try" | "do" | "assert")),
            "Conditional Gradle scripts require evaluated dependency discovery"
        );
        if parser.name("dependencies") {
            ensure!(
                depth == 0 && parentheses == 0,
                "Nested or conditional dependencies require evaluated Gradle discovery"
            );
            let preceding = parser.tokens.get(..parser.position - 1).unwrap_or_default();
            ensure!(
                preceding.is_empty()
                    || matches!(
                        preceding.last(),
                        Some(Token::Newline | Token::Symbol(';' | '}'))
                    ),
                "Qualified or expression-nested dependencies require evaluated Gradle discovery"
            );
            ensure!(
                !matches!(preceding.iter().rev().find(|token| **token != Token::Newline), Some(Token::Symbol(symbol)) if !matches!(symbol, ')' | ']' | '}' | ';')),
                "Continued dependency expressions require evaluated Gradle discovery"
            );
            parser.expect_symbol('{')?;
            parser.separators();
            while !parser.symbol('}') {
                let Some(Token::Name(configuration)) = parser.peek().cloned() else {
                    bail!("Expected a literal dependency configuration");
                };
                ensure!(
                    !matches!(
                        configuration.as_str(),
                        "if" | "for" | "while" | "def" | "val" | "var" | "add"
                    ),
                    "Dynamic dependencies require evaluated Gradle discovery"
                );
                parser.position += 1;
                let parenthesized = parser.symbol('(');
                if parser.name("project") {
                    parser.expect_symbol('(')?;
                    if parser.name("path") {
                        ensure!(
                            parser.symbol(':') || parser.symbol('='),
                            "Expected project path assignment"
                        );
                    }
                    dependencies.insert(module_name(&parser.string()?)?);
                    parser.expect_symbol(')')?;
                } else {
                    parser.string()?;
                }
                if parenthesized {
                    parser.expect_symbol(')')?;
                }
                parser.end_statement()?;
            }
        } else {
            if matches!(token, Token::Name(ref name) if name == "apply") {
                parser.position += 1;
                let parenthesized = parser.symbol('(');
                parser.newlines();
                ensure!(
                    parser.name("plugin"),
                    "Only literal plugin application is supported; applied scripts require evaluated dependency discovery"
                );
                ensure!(
                    parser.symbol(':') || parser.symbol('='),
                    "Expected literal plugin assignment"
                );
                parser.string()?;
                if parenthesized {
                    parser.newlines();
                    parser.expect_symbol(')')?;
                }
                parser.end_statement()?;
                continue;
            }
            match token {
                Token::Symbol('{') => depth += 1,
                Token::Symbol('}') => {
                    depth = depth
                        .checked_sub(1)
                        .context("Unmatched Gradle closing brace")?
                }
                Token::Symbol('(') => parentheses += 1,
                Token::Symbol(')') => {
                    parentheses = parentheses
                        .checked_sub(1)
                        .context("Unmatched Gradle closing parenthesis")?;
                }
                _ => {}
            }
            parser.position += 1;
        }
    }
    ensure!(depth == 0, "Unterminated Gradle block");
    ensure!(parentheses == 0, "Unterminated Gradle parenthesis");
    Ok(dependencies)
}

fn gradle_string(value: &str, kotlin: bool) -> String {
    let quote = if kotlin { '"' } else { '\'' };
    let escaped = value
        .replace('\\', "\\\\")
        .replace(quote, &format!("\\{quote}"));
    let escaped = if kotlin {
        escaped.replace('$', "\\$")
    } else {
        escaped
    };
    format!("{quote}{escaped}{quote}")
}

fn register_settings(
    original: &str,
    settings: &Settings,
    modules: &BTreeMap<String, PathBuf>,
    destination: &Path,
    kotlin: bool,
) -> Result<String> {
    let mut additions = String::new();
    for (name, path) in modules {
        let literal_name = gradle_string(name, kotlin);
        if !settings.includes.contains(name) {
            if kotlin {
                additions.push_str(&format!("include({literal_name})\n"));
            } else {
                additions.push_str(&format!("include {literal_name}\n"));
            }
        }
        let default = destination.join(module_relative_path(name)?);
        let existing = settings
            .directories
            .get(name)
            .map(|path| destination.join(path));
        if path != &default && existing.as_ref() != Some(path)
            || path == &default && existing.is_some_and(|existing| existing != *path)
        {
            let relative = path
                .strip_prefix(destination)
                .context("Module is outside destination")?;
            let relative = relative
                .components()
                .map(|component| {
                    let Component::Normal(component) = component else {
                        bail!("Module path is not relative to its destination");
                    };
                    let component = component.to_str().context("Module path is not UTF-8")?;
                    ensure!(
                        !component.chars().any(char::is_control),
                        "Module paths with control characters are unsupported"
                    );
                    Ok(component)
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            let literal_path = gradle_string(&relative, kotlin);
            additions.push_str(&format!(
                "project({literal_name}).projectDir = file({literal_path})\n"
            ));
        }
    }
    if additions.is_empty() {
        return Ok(original.to_owned());
    }
    let mut updated = original.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&additions);
    Ok(updated)
}

fn validate_parent_paths(target: &Path, destination: &Path) -> Result<()> {
    for parent in target
        .ancestors()
        .skip(1)
        .take_while(|parent| *parent != destination)
    {
        ensure!(
            !parent.is_symlink(),
            "Import destination parent is a symlink: {}",
            parent.display()
        );
        ensure!(
            !parent.exists() || parent.is_dir(),
            "Import destination parent is not a directory: {}",
            parent.display()
        );
    }
    Ok(())
}

fn create_parent_paths(
    target: &Path,
    destination: &Path,
    created: &mut Vec<PublishedPath>,
) -> Result<()> {
    validate_parent_paths(target, destination)?;
    let mut missing = target
        .ancestors()
        .skip(1)
        .take_while(|parent| *parent != destination && !parent.exists())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    missing.reverse();
    for path in missing {
        fs::create_dir(&path)?;
        created.push(PublishedPath::Parent(path));
    }
    Ok(())
}

fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    ensure!(
        !source.is_symlink(),
        "Import source symlinks are unsupported"
    );
    fs::create_dir(target)?;
    let mut pending = vec![(source.to_path_buf(), target.to_path_buf())];
    while let Some((source, target)) = pending.pop() {
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let output = target.join(entry.file_name());
            if file_type.is_dir() {
                fs::create_dir(&output)?;
                pending.push((entry.path(), output));
            } else if file_type.is_file() {
                fs::copy(entry.path(), output)?;
            } else {
                bail!(
                    "Import source contains an unsupported symlink or special file: {}",
                    entry.path().display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_failure_rolls_back_copies_and_new_parent_directories() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let staging = tempfile::tempdir()?;
        let first = staging.path().join("first");
        fs::create_dir(&first)?;
        fs::write(first.join("keep.txt"), "copied bytes")?;
        let original = "// untouched target\nrootProject.name = 'target'\n";
        let settings_file = directory.path().join("settings.gradle");
        fs::write(&settings_file, original)?;
        let mut settings_temporary = tempfile::NamedTempFile::new_in(directory.path())?;
        settings_temporary.write_all(b"include ':one', ':two'\n")?;
        let result = publish_import(
            directory.path(),
            vec![
                (first, directory.path().join("parent/one")),
                // A real failed rename after one successful publication verifies
                // rollback without changing production logic or timing a race.
                (
                    staging.path().join("missing"),
                    directory.path().join("parent/two"),
                ),
            ],
            settings_temporary,
            &settings_file,
            original,
            true,
            true,
        );
        assert!(result.is_err());
        assert!(!directory.path().join("parent").exists());
        assert_eq!(fs::read_to_string(&settings_file)?, original);
        assert_eq!(fs::read_dir(directory.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn changed_settings_are_rejected_before_publishing_copies() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let staging = tempfile::tempdir()?;
        let source = staging.path().join("module");
        fs::create_dir(&source)?;
        let settings_file = directory.path().join("settings.gradle");
        fs::write(&settings_file, "// concurrent edit\n")?;
        let settings_temporary = tempfile::NamedTempFile::new_in(directory.path())?;
        assert!(
            publish_import(
                directory.path(),
                vec![(source.clone(), directory.path().join("module"))],
                settings_temporary,
                &settings_file,
                "// previous content\n",
                true,
                true,
            )
            .is_err()
        );
        assert!(source.is_dir());
        assert!(!directory.path().join("module").exists());
        assert_eq!(fs::read_to_string(&settings_file)?, "// concurrent edit\n");
        Ok(())
    }
}
