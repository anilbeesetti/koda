use crate::{
    AndroidTarget,
    project_model::{SelectedProject, SourceKind, SourceScope},
};
use anyhow::{Context as _, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

pub const MAX_TESTS: usize = 20_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DETAIL_BYTES: usize = 16 * 1024;

#[derive(Clone, Default)]
pub struct ScanControl(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl ScanControl {
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn check(&self) -> Result<()> {
        ensure!(
            !self.0.load(std::sync::atomic::Ordering::Relaxed),
            "Test scanning cancelled"
        );
        Ok(())
    }
}

#[derive(Default)]
struct WalkBudget {
    directories: usize,
    entries: usize,
}

#[derive(Default)]
struct DiscoveryBudget {
    cases: usize,
    identity_bytes: usize,
}

fn push_discovered(
    cases: &mut Vec<TestCase>,
    case: TestCase,
    budget: &mut DiscoveryBudget,
) -> Result<()> {
    ensure!(budget.cases < MAX_TESTS, "Test discovery limit exceeded");
    ensure!(
        case.id.class.len() <= 4096 && case.id.method.len() <= 4096,
        "Test identity limit exceeded"
    );
    let identity_bytes = case.id.class.len()
        + case.id.method.len()
        + case
            .source
            .as_ref()
            .map_or(0, |source| source.path.as_os_str().len());
    ensure!(
        identity_bytes <= MAX_BYTES.saturating_sub(budget.identity_bytes),
        "Test source identity size limit exceeded"
    );
    budget.cases += 1;
    budget.identity_bytes += identity_bytes;
    cases.push(case);
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestKind {
    #[default]
    Unit,
    Device,
}
impl TestKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unit => "Local unit tests",
            Self::Device => "Device / Compose tests",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestStatus {
    #[default]
    NotRun,
    Passed,
    Failed,
    Skipped,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TestId {
    pub class: String,
    pub method: String,
}
impl TestId {
    pub fn selector(&self, kind: TestKind) -> String {
        // JUnit parameter/display names are not method selectors. Rerun the owning
        // class for parameterized invocations and arbitrary display names.
        if self.method.contains('[') {
            return self.class.clone();
        }
        let method = self.method.split('(').next().unwrap_or_default();
        if method.is_empty()
            || !method
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | ' '))
        {
            return self.class.clone();
        }
        format!(
            "{}{}{method}",
            self.class,
            if kind == TestKind::Unit { "." } else { "#" }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub line: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestCase {
    pub id: TestId,
    #[serde(default)]
    pub status: TestStatus,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub source: Option<SourceLocation>,
    #[serde(default)]
    pub parameterized: bool,
}
impl TestCase {
    pub fn selector(&self, kind: TestKind) -> String {
        if self.parameterized {
            self.id.class.clone()
        } else {
            self.id.selector(kind)
        }
    }
}
pub fn component_sources(
    selected: &SelectedProject,
    target: &AndroidTarget,
    kind: TestKind,
) -> Result<Vec<PathBuf>> {
    selected.validate_target(target)?;
    let (_, variant) = selected
        .modules()
        .find(|(module, variant)| module.path == target.module && variant.name == target.variant)
        .context("Selected test module is absent from the Android model")?;
    let (scope, suffix) = match kind {
        TestKind::Unit => (SourceScope::UnitTest, "UnitTest"),
        TestKind::Device => (SourceScope::AndroidTest, "AndroidTest"),
    };
    let name = format!("{}{suffix}", target.variant);
    let component = variant.components.iter().find(|component| component.scope == scope && component.name == name).context("Standard tests are disabled or unavailable for the selected variant; custom test suites are unsupported")?;
    Ok(component
        .sources
        .iter()
        .filter(|source| matches!(source.kind, SourceKind::Java | SourceKind::Kotlin))
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

// Reports must stay isolated from previous runs and the project's own reports.
pub const INIT_SCRIPT: &str = r#"
def module = System.getProperty('koda.test.module')
def variantName = System.getProperty('koda.test.variant')
def destination = new File(System.getProperty('koda.test.output'))
def deviceTests = System.getProperty('koda.test.kind') == 'device'
gradle.beforeProject { project ->
    if (project.path == module) {
        ['com.android.application', 'com.android.library'].each { plugin ->
            project.plugins.withId(plugin) {
                def components = project.extensions.getByName('androidComponents')
                components.finalizeDsl { android ->
                    if (deviceTests) android.testOptions.resultsDir = new File(destination, 'device').absolutePath
                }

            }
        }
        project.tasks.withType(org.gradle.api.tasks.testing.Test).configureEach { task ->
            def capitalized = variantName.substring(0, 1).toUpperCase(java.util.Locale.ROOT) + variantName.substring(1)
            if (task.name == 'test' + capitalized + 'UnitTest') {
                task.outputs.upToDateWhen { false }
                task.outputs.cacheIf { false }
                task.reports.junitXml.required = true
                task.reports.junitXml.outputLocation.set(new File(destination, 'unit'))
                task.addTestListener(new org.gradle.api.tasks.testing.TestListener() {
                    int recorded = 0
                    void beforeSuite(org.gradle.api.tasks.testing.TestDescriptor descriptor) {}
                    void afterSuite(org.gradle.api.tasks.testing.TestDescriptor descriptor, org.gradle.api.tasks.testing.TestResult result) {}
                    void beforeTest(org.gradle.api.tasks.testing.TestDescriptor descriptor) {}
                    synchronized void afterTest(org.gradle.api.tasks.testing.TestDescriptor descriptor, org.gradle.api.tasks.testing.TestResult result) {
                        def events = new File(destination, 'events.jsonl')
                        if (recorded < 20000) {
                            recorded++
                            def status = [SUCCESS:'Passed', FAILURE:'Failed', SKIPPED:'Skipped'][result.resultType.name()]
                            def detail = result.exceptions.collect { exception ->
                                def writer = new StringWriter()
                                exception.printStackTrace(new PrintWriter(writer))
                                writer.toString().take(16384)
                            }.join('\n').take(16384)
                            def record = groovy.json.JsonOutput.toJson([id:[class:(descriptor.className ?: descriptor.parent?.name ?: '').take(4096), method:descriptor.name.take(4096)], status:status, duration_ms:Math.max(0, result.endTime - result.startTime), detail:detail]) + '\n'
                            if (events.length() + record.getBytes('UTF-8').length <= 8 * 1024 * 1024) events.append(record, 'UTF-8')
                            else new File(destination, 'results-truncated').text = 'partial'
                        } else { new File(destination, 'results-truncated').text = 'partial' }
                    }
                })
            }
        }
    }
}
gradle.taskGraph.whenReady { graph ->
    def capitalized = variantName.substring(0, 1).toUpperCase(java.util.Locale.ROOT) + variantName.substring(1)
    graph.allTasks.findAll { it instanceof org.gradle.api.tasks.testing.Test && it.project.path == module && it.name == 'test' + capitalized + 'UnitTest' }.each { task ->
        // AGP configures its report convention after the init script's early
        // configureEach action; set the run directory after task configuration.
        task.reports.junitXml.outputLocation.set(new File(destination, 'unit'))
    }
}
"#;

pub fn arguments(
    target: &AndroidTarget,
    kind: TestKind,
    output: &Path,
    selectors: &[String],
) -> Result<Vec<String>> {
    ensure!(
        !target.variant.is_empty()
            && target
                .variant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')),
        "Invalid test variant"
    );
    ensure!(
        target.module == ":"
            || target.module.starts_with(':')
                && target.module.split(':').skip(1).all(|part| !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$'))),
        "Invalid test module"
    );
    ensure!(selectors.len() <= MAX_TESTS, "Too many test selectors");
    for selector in selectors {
        ensure!(
            !selector.is_empty()
                && !selector.starts_with('-')
                && !selector.contains([',', '\n', '\r', '*', '?', '\0']),
            "Test name cannot be represented by an exact runner filter: {selector}"
        );
    }
    let task = match kind {
        TestKind::Unit => target.gradle_task("test", "UnitTest"),
        TestKind::Device => target.gradle_task("connected", "AndroidTest"),
    };
    let mut arguments = vec![
        task,
        "--init-script".into(),
        output.join("runner.gradle").to_string_lossy().into_owned(),
        format!("-Dkoda.test.module={}", target.module),
        format!("-Dkoda.test.variant={}", target.variant),
        format!("-Dkoda.test.output={}", output.display()),
        format!(
            "-Dkoda.test.kind={}",
            if kind == TestKind::Device {
                "device"
            } else {
                "unit"
            }
        ),
        "--console=plain".into(),
        "--no-configuration-cache".into(),
        "--no-daemon".into(),
    ];
    match kind {
        TestKind::Unit => {
            for selector in selectors {
                arguments.extend(["--tests".into(), selector.clone()]);
            }
        }
        TestKind::Device => {
            // Force fresh instrumentation reports even when Gradle considers
            // the connected task up to date. ANDROID_SERIAL scopes the device.
            arguments.push("--rerun-tasks".into());
            if !selectors.is_empty() {
                arguments.push(format!(
                    "-Pandroid.testInstrumentationRunnerArguments.class={}",
                    selectors.join(",")
                ));
            }
        }
    }
    Ok(arguments)
}

fn read_bounded(path: &Path, limit: usize, control: &ScanControl) -> Result<String> {
    let mut data = Vec::new();
    control.check()?;
    let mut file = fs::File::open(path)?.take((limit + 1) as u64);
    let mut chunk = [0; 64 * 1024];
    loop {
        control.check()?;
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..count]);
    }
    control.check()?;
    ensure!(
        data.len() <= limit,
        "{} exceeds the {} byte test data limit",
        path.display(),
        limit
    );
    String::from_utf8(data).context("Test data is not UTF-8")
}

pub fn discover(directories: &[PathBuf]) -> Result<Vec<TestCase>> {
    discover_with_control(directories, &ScanControl::default())
}

pub fn discover_with_control(
    directories: &[PathBuf],
    control: &ScanControl,
) -> Result<Vec<TestCase>> {
    let mut cases = BTreeMap::new();
    let mut identity_bytes = 0;
    let mut files = 0;
    let mut bytes = 0;
    let mut walk_budget = WalkBudget::default();
    let mut discovery_budget = DiscoveryBudget::default();
    for directory in directories {
        control.check()?;
        if directory.exists() {
            walk(directory, 0, &mut walk_budget, control, &mut |path| {
                files += 1;
                ensure!(files <= MAX_TESTS, "Test source file limit exceeded");
                if matches!(
                    path.extension().and_then(|v| v.to_str()),
                    Some("kt" | "java")
                ) {
                    let text = read_bounded(path, MAX_SOURCE_BYTES, control)?;
                    bytes += text.len();
                    ensure!(bytes <= MAX_BYTES * 8, "Test source size limit exceeded");
                    for case in
                        discover_source_with_budget(path, &text, &mut discovery_budget, control)?
                    {
                        identity_bytes +=
                            case.id.class.len() + case.id.method.len() + path.as_os_str().len();
                        ensure!(
                            identity_bytes <= MAX_BYTES,
                            "Test source identity size limit exceeded"
                        );
                        cases.insert(case.id.clone(), case);
                        ensure!(cases.len() <= MAX_TESTS, "Test discovery limit exceeded");
                    }
                }
                Ok(())
            })?;
        }
    }
    control.check()?;
    Ok(cases.into_values().collect())
}

fn walk(
    path: &Path,
    depth: usize,
    budget: &mut WalkBudget,
    control: &ScanControl,
    visit: &mut impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    control.check()?;
    ensure!(depth < 64, "Test directory nesting limit exceeded");
    ensure!(
        budget.directories < MAX_TESTS,
        "Test directory count limit exceeded"
    );
    budget.directories += 1;
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        control.check()?;
        ensure!(
            budget.entries < MAX_TESTS * 2,
            "Test directory traversal limit exceeded"
        );
        ensure!(
            entries.len() < MAX_TESTS,
            "Test directory entry limit exceeded"
        );
        budget.entries += 1;
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        control.check()?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&entry.path(), depth + 1, budget, control, visit)?;
        } else if kind.is_file() {
            visit(&entry.path())?;
        }
    }
    Ok(())
}

pub fn discover_source(path: &Path, source: &str) -> Result<Vec<TestCase>> {
    discover_source_with_budget(
        path,
        source,
        &mut DiscoveryBudget::default(),
        &ScanControl::default(),
    )
}

fn discover_source_with_budget(
    path: &Path,
    source: &str,
    budget: &mut DiscoveryBudget,
    control: &ScanControl,
) -> Result<Vec<TestCase>> {
    control.check()?;
    ensure!(
        source.len() <= MAX_SOURCE_BYTES,
        "Test source size limit exceeded"
    );
    let clean = strip_comments(source);
    let package = Regex::new(r"(?m)^\s*package\s+([\w.]+)")?
        .captures(&clean)
        .and_then(|captures| captures.get(1))
        .map(|v| v.as_str())
        .unwrap_or_default();
    ensure!(package.len() <= 4096, "Test package name limit exceeded");
    let declarations = Regex::new(
        r"\b(class|object|interface)\s+(\w+)|@(\w+[.\w]*)(?:\([^()]*?(?:\([^()]*\)[^()]*?)*\))?|\bfun\s+(`[^`]+`|\w+)\s*\(|\b(?:(?:public|protected|private)\s+)?(?:final\s+|static\s+)*(?:void|[A-Z]\w*)\s+(\w+)\s*\(|[{}]",
    )?;
    let mut classes: Vec<(String, usize, bool)> = Vec::new();
    let mut pending_class = None;
    let mut depth: usize = 0;
    let mut annotated = false;
    let mut parameterized = false;
    let mut suite = false;
    let mut cases = Vec::new();
    let mut line = 1;
    let mut line_offset = 0;
    for captures in declarations.captures_iter(&clean) {
        control.check()?;
        let token = captures.get(0).context("Missing source token")?;
        line += clean[line_offset..token.start()]
            .bytes()
            .filter(|&byte| byte == b'\n')
            .count() as u32;
        line_offset = token.start();
        if let Some(name) = captures.get(2) {
            ensure!(
                name.as_str().len() <= 1024,
                "Test class name limit exceeded"
            );
            if suite {
                let enclosing = classes
                    .iter()
                    .map(|(name, _, _)| name.as_str())
                    .chain(std::iter::once(name.as_str()))
                    .collect::<Vec<_>>()
                    .join("$");
                push_discovered(
                    &mut cases,
                    TestCase {
                        id: TestId {
                            class: if package.is_empty() {
                                enclosing
                            } else {
                                format!("{package}.{enclosing}")
                            },
                            method: "<suite>".into(),
                        },
                        status: TestStatus::NotRun,
                        duration_ms: 0,
                        detail: String::new(),
                        parameterized: false,
                        source: Some(SourceLocation {
                            path: path.into(),
                            line,
                        }),
                    },
                    budget,
                )?;
                suite = false;
            }
            pending_class = Some(name.as_str().to_owned());
            annotated = false;
        } else if let Some(annotation) = captures.get(3) {
            let name = annotation.as_str().rsplit('.').next().unwrap_or_default();
            suite |= matches!(
                name,
                "SuiteClasses" | "SelectClasses" | "SelectPackages" | "Suite"
            );
            parameterized |= matches!(
                name,
                "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate"
            ) || name == "RunWith" && token.as_str().contains("Parameterized");
            annotated |= matches!(
                name,
                "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate"
            );
        } else if let Some(method) = captures.get(4).or_else(|| captures.get(5)) {
            if annotated && !classes.is_empty() {
                ensure!(
                    method.as_str().len() <= 4096 && classes.len() <= 32,
                    "Test source identity limit exceeded"
                );
                let class = classes
                    .iter()
                    .map(|(name, _, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join("$");
                ensure!(
                    class.len() + package.len() <= 4096,
                    "Test class identity limit exceeded"
                );
                push_discovered(
                    &mut cases,
                    TestCase {
                        id: TestId {
                            class: if package.is_empty() {
                                class
                            } else {
                                format!("{package}.{class}")
                            },
                            method: method.as_str().trim_matches('`').into(),
                        },
                        status: TestStatus::NotRun,
                        duration_ms: 0,
                        detail: String::new(),
                        parameterized: parameterized
                            || classes.iter().any(|(_, _, parameterized)| *parameterized),
                        source: Some(SourceLocation {
                            path: path.to_path_buf(),
                            line,
                        }),
                    },
                    budget,
                )?;
            }
            annotated = false;
            parameterized = false;
        } else if token.as_str() == "{" {
            depth += 1;
            if let Some(name) = pending_class.take() {
                classes.push((name, depth, parameterized));
                parameterized = false;
            }
            annotated = false;
        } else if token.as_str() == "}" {
            if classes.last().is_some_and(|(_, level, _)| *level == depth) {
                classes.pop();
            }
            depth = depth.saturating_sub(1);
            annotated = false;
        }
    }
    control.check()?;
    Ok(cases)
}

fn strip_comments(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            let mut depth = 1;
            while index < bytes.len() && depth > 0 {
                if bytes.get(index..index + 2) == Some(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
        } else if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            let triple = bytes.get(index..index + 3) == Some(b"\"\"\"");
            index += if triple { 3 } else { 1 };
            while index < bytes.len() {
                if triple && bytes.get(index..index + 3) == Some(b"\"\"\"") {
                    index += 3;
                    break;
                }
                if !triple && bytes[index] == quote {
                    index += 1;
                    break;
                }
                if !triple && bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else {
                    index += 1;
                }
            }
        } else {
            index += 1;
            continue;
        }
        for byte in &mut bytes[start..index] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[derive(Default)]
pub struct TestResults {
    pub cases: Vec<TestCase>,
    pub diagnostics: Vec<String>,
}
pub fn read_results(output: &Path, sources: &[TestCase]) -> TestResults {
    read_results_with_control(output, sources, &ScanControl::default())
}

pub fn read_results_with_control(
    output: &Path,
    sources: &[TestCase],
    control: &ScanControl,
) -> TestResults {
    let mut cases = BTreeMap::new();
    let mut diagnostics = Vec::new();
    if let Err(error) = control.check() {
        return TestResults {
            cases: Vec::new(),
            diagnostics: vec![error.to_string()],
        };
    }
    let events = output.join("events.jsonl");
    if events.is_file() {
        match read_bounded(&events, MAX_BYTES, control) {
            Ok(text) => {
                for line in text
                    .split_inclusive('\n')
                    .filter(|line| line.ends_with('\n'))
                {
                    if let Err(error) = control.check() {
                        diagnostics.push(error.to_string());
                        break;
                    }
                    match serde_json::from_str::<TestCase>(line) {
                        Ok(case)
                            if cases.len() < MAX_TESTS
                                && case.id.class.len() <= 4096
                                && case.id.method.len() <= 4096 =>
                        {
                            cases.insert(case.id.clone(), case);
                        }
                        Ok(_) => {
                            diagnostics.push("Test result limit exceeded".into());
                            break;
                        }
                        Err(error) => {
                            diagnostics.push(format!("Invalid test event: {error}"));
                            break;
                        }
                    }
                }
            }
            Err(error) => diagnostics.push(format!("{error:#}")),
        }
    }
    let mut bytes = 0;
    let mut count = 0;
    let mut walk_budget = WalkBudget::default();
    for directory in ["unit", "device"] {
        if let Err(error) = control.check() {
            diagnostics.push(error.to_string());
            break;
        }
        let path = output.join(directory);
        if !path.exists() {
            continue;
        }
        if let Err(error) = walk(&path, 0, &mut walk_budget, control, &mut |path| {
            count += 1;
            ensure!(count <= MAX_TESTS, "Test report file limit exceeded");
            if path.extension().is_some_and(|value| value == "xml") {
                let parsed = (|| {
                    let text = read_bounded(path, MAX_BYTES, control)?;
                    bytes += text.len();
                    ensure!(bytes <= MAX_BYTES, "Test report size limit exceeded");
                    parse_junit_with_control(&text, control)
                })();
                match parsed {
                    Ok(reported) => {
                        for case in reported {
                            ensure!(
                                cases.contains_key(&case.id) || cases.len() < MAX_TESTS,
                                "Test result limit exceeded"
                            );
                            cases.insert(case.id.clone(), case);
                        }
                    }
                    Err(error) => {
                        if diagnostics.len() < 32 {
                            diagnostics.push(format!("{}: {error:#}", path.display()));
                        }
                        ensure!(bytes <= MAX_BYTES, "Test report size limit exceeded");
                    }
                }
            }
            Ok(())
        }) {
            if diagnostics.len() < 32 {
                diagnostics.push(format!("{error:#}"));
            }
        }
    }
    if output.join("results-truncated").exists() {
        diagnostics.push("Test event limit exceeded; results are partial".into());
    }
    let mut source_methods = BTreeMap::new();
    let mut source_classes = BTreeMap::new();
    for source in sources {
        if let Err(error) = control.check() {
            diagnostics.push(error.to_string());
            break;
        }
        source_methods.insert(
            (source.id.class.as_str(), source.id.method.as_str()),
            source,
        );
        source_classes
            .entry(source.id.class.as_str())
            .or_insert(source);
    }
    let mut detail_bytes = 0;
    for case in cases.values_mut() {
        if let Err(error) = control.check() {
            diagnostics.push(error.to_string());
            break;
        }
        truncate(&mut case.detail, MAX_DETAIL_BYTES);
        detail_bytes += case.detail.len();
        if detail_bytes > MAX_BYTES {
            case.detail.clear();
        }
        let method = case.id.method.split(['[', '(']).next().unwrap_or_default();
        if let Some(source) = source_methods
            .get(&(case.id.class.as_str(), method))
            .or_else(|| source_classes.get(case.id.class.as_str()))
        {
            case.source = source_for(case, source);
            case.parameterized |= source.parameterized;
        }
    }
    if detail_bytes > MAX_BYTES {
        diagnostics.push("Test failure detail limit exceeded; additional details omitted".into());
    }
    TestResults {
        cases: cases.into_values().collect(),
        diagnostics,
    }
}

pub fn parse_junit(xml: &str) -> Result<Vec<TestCase>> {
    parse_junit_with_control(xml, &ScanControl::default())
}

fn parse_junit_with_control(xml: &str, control: &ScanControl) -> Result<Vec<TestCase>> {
    control.check()?;
    ensure!(xml.len() <= MAX_BYTES, "Test report size limit exceeded");
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 200_000,
            ..Default::default()
        },
    )?;
    let mut cases = Vec::new();
    let mut identity_bytes = 0;
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
    {
        control.check()?;
        ensure!(cases.len() < MAX_TESTS, "Test result limit exceeded");
        let failure = node
            .children()
            .find(|node| node.has_tag_name("failure") || node.has_tag_name("error"));
        let mut detail = failure
            .map(|node| {
                format!(
                    "{}\n{}",
                    node.attribute("message").unwrap_or_default(),
                    node.text().unwrap_or_default()
                )
            })
            .unwrap_or_default();
        truncate(&mut detail, MAX_DETAIL_BYTES);
        let class = node
            .attribute("classname")
            .or_else(|| node.parent().and_then(|parent| parent.attribute("name")))
            .context("Test report has no class name")?;
        let method = node
            .attribute("name")
            .context("Test report has no test name")?;
        ensure!(
            class.len() <= 4096 && method.len() <= 4096,
            "Test identity limit exceeded"
        );
        identity_bytes += class.len() + method.len();
        ensure!(
            identity_bytes <= MAX_BYTES,
            "Test identity size limit exceeded"
        );
        cases.push(TestCase {
            id: TestId {
                class: node
                    .attribute("classname")
                    .or_else(|| node.parent().and_then(|parent| parent.attribute("name")))
                    .context("Test report has no class name")?
                    .into(),
                method: node
                    .attribute("name")
                    .context("Test report has no test name")?
                    .into(),
            },
            status: if failure.is_some() {
                TestStatus::Failed
            } else if node.children().any(|node| node.has_tag_name("skipped")) {
                TestStatus::Skipped
            } else {
                TestStatus::Passed
            },
            duration_ms: node
                .attribute("time")
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.)
                .map(|value| (value * 1000.) as u64)
                .unwrap_or_default(),
            detail,
            source: None,
            parameterized: false,
        });
    }
    Ok(cases)
}
fn truncate(value: &mut String, limit: usize) {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}
fn source_for(case: &TestCase, source: &TestCase) -> Option<SourceLocation> {
    let source = source.source.clone()?;
    let filename = source.path.file_name()?.to_str()?;
    for line in case.detail.lines() {
        if line.contains(&case.id.class)
            && let Some((_, remainder)) = line.split_once(&format!("({filename}:"))
            && let Some(number) = remainder.strip_suffix(')')
            && let Ok(line) = number.parse::<u32>()
            && line > 0
        {
            return Some(SourceLocation { line, ..source });
        }
    }
    Some(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_component_sources_preserve_custom_and_generated_roots_without_dependency_suites() {
        let root = tempfile::tempdir().unwrap();
        for (directory, class) in [
            ("custom-tests", "Custom"),
            ("generated-tests", "Generated"),
            ("main", "Main"),
            ("library-tests", "Library"),
            ("fixtures", "Fixture"),
        ] {
            fs::create_dir(root.path().join(directory)).unwrap();
            fs::write(
                root.path().join(directory).join(format!("{class}.java")),
                format!("package dev; class {class} {{ @Test void works() {{}} }}"),
            )
            .unwrap();
        }
        let model: crate::project_model::ProjectModel = serde_json::from_value(serde_json::json!({
            "version":1,"root":root.path(),"diagnostics":[],"modules":[
                {"path":":app","directory":root.path(),"kind":"application","variants":[
                    {"name":"demoDebug","outputListing":root.path().join("output.json"),"components":[
                        {"name":"demoDebug","scope":"main","sources":[{"path":root.path().join("main"),"kind":"java","generated":false}],"dependencies":[{"kind":"project","module":":library","variant":"release"}]},
                        {"name":"demoDebugUnitTest","scope":"unitTest","sources":[{"path":root.path().join("custom-tests"),"kind":"java","generated":false},{"path":root.path().join("custom-tests"),"kind":"kotlin","generated":false},{"path":root.path().join("generated-tests"),"kind":"kotlin","generated":true},{"path":root.path().join("main"),"kind":"resources","generated":false}],"dependencies":[]},
                        {"name":"demoDebugTestFixtures","scope":"testFixtures","sources":[{"path":root.path().join("fixtures"),"kind":"java","generated":false}],"dependencies":[]}
                    ]}
                ]},
                {"path":":library","directory":root.path(),"kind":"library","variants":[{"name":"release","components":[{"name":"releaseUnitTest","scope":"unitTest","sources":[{"path":root.path().join("library-tests"),"kind":"java","generated":false}],"dependencies":[]}]}]}
            ]
        })).unwrap();
        let model = std::sync::Arc::new(model);
        let target = model.targets().remove(0);
        let selected = model
            .select(crate::project_model::VariantId::from(&target))
            .unwrap();
        let roots = component_sources(&selected, &target, TestKind::Unit).unwrap();
        assert_eq!(roots.len(), 2);
        let cases = discover(&roots).unwrap();
        assert_eq!(
            cases
                .iter()
                .map(|case| case.id.class.as_str())
                .collect::<Vec<_>>(),
            ["dev.Custom", "dev.Generated"]
        );
        assert!(component_sources(&selected, &target, TestKind::Device).is_err());
        let wrong_target = AndroidTarget {
            variant: "fullDebug".into(),
            ..target
        };
        assert!(component_sources(&selected, &wrong_target, TestKind::Unit).is_err());
    }
    #[test]
    fn discovers_java_kotlin_nested_and_ignores_comments_strings() {
        let text = "package dev.tests\nclass Example {\n// @Test fun fake() {}\nval sample = \"@Test fun fake() {}\"\n@Test fun `with spaces`() {}\nclass Inner { @org.junit.Test public void javaTest() {} }\n@ParameterizedTest\n@ValueSource(strings = [\"a\"])\nfun parameter(value: String) {}\n}";
        let cases = discover_source(Path::new("Example.kt"), text).unwrap();
        assert_eq!(
            cases
                .iter()
                .map(|case| (&*case.id.class, &*case.id.method))
                .collect::<Vec<_>>(),
            vec![
                ("dev.tests.Example", "with spaces"),
                ("dev.tests.Example$Inner", "javaTest"),
                ("dev.tests.Example", "parameter")
            ]
        );
        assert_eq!(cases[0].source.as_ref().unwrap().line, 5);
    }
    #[test]
    fn junit_handles_failures_skips_entities_and_parameterized_selectors() {
        let xml = r#"<testsuites><testsuite name="suite"><testcase classname="dev.Example" name="parameter[0]" time="0.125"><failure message="a &amp; b"><![CDATA[at dev.Example.parameter(Example.kt:42)]]></failure></testcase><testcase classname="dev.Example" name="skip"><skipped/></testcase><testcase classname="dev.Example" name="ok" time="NaN"/></testsuite></testsuites>"#;
        let cases = parse_junit(xml).unwrap();
        assert_eq!(cases[0].status, TestStatus::Failed);
        assert_eq!(cases[0].duration_ms, 125);
        assert_eq!(cases[0].id.selector(TestKind::Unit), "dev.Example");
        assert_eq!(cases[0].id.selector(TestKind::Device), "dev.Example");
        assert_eq!(cases[1].status, TestStatus::Skipped);
        assert_eq!(cases[2].duration_ms, 0);
        assert!(parse_junit("<!DOCTYPE x [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><x/>").is_err());
        assert!(parse_junit("<testsuite><testcase").is_err());
    }
    #[test]
    fn results_are_isolated_and_keep_complete_partial_events() {
        let directory = tempfile::tempdir().unwrap();
        let sources = discover_source(
            Path::new("Example.kt"),
            "package dev\nclass Example { @Test fun failed() {} }",
        )
        .unwrap();
        let case = TestCase {
            id: TestId {
                class: "dev.Example".into(),
                method: "failed".into(),
            },
            status: TestStatus::Failed,
            duration_ms: 1,
            detail: "at dev.Example.failed(Example.kt:18)".into(),
            source: None,
            parameterized: false,
        };
        fs::write(
            directory.path().join("events.jsonl"),
            format!("{}\n{{", serde_json::to_string(&case).unwrap()),
        )
        .unwrap();
        let results = read_results(directory.path(), &sources).cases;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source.as_ref().unwrap().line, 18);
        fs::create_dir(directory.path().join("unit")).unwrap();
        fs::write(
            directory.path().join("unit/TEST-truncated.xml"),
            "<testsuite><testcase",
        )
        .unwrap();
        let partial = read_results(directory.path(), &sources);
        assert_eq!(partial.cases.len(), 1);
        assert_eq!(partial.diagnostics.len(), 1);
        let other = tempfile::tempdir().unwrap();
        assert!(read_results(other.path(), &sources).cases.is_empty());
    }
    #[test]
    fn runner_filters_scope_flavors_module_and_device() {
        let target = AndroidTarget {
            module: ":mobile:application".into(),
            variant: "freeDebug".into(),
            output_listing: PathBuf::new(),
        };
        let unit = arguments(
            &target,
            TestKind::Unit,
            Path::new("/tmp/run"),
            &["dev.Test.test".into()],
        )
        .unwrap();
        assert_eq!(unit[0], ":mobile:application:testFreeDebugUnitTest");
        assert!(
            unit.windows(2)
                .any(|pair| pair == ["--tests", "dev.Test.test"])
        );
        let device = arguments(
            &target,
            TestKind::Device,
            Path::new("/tmp/run"),
            &["dev.Test#test".into()],
        )
        .unwrap();
        assert_eq!(
            device[0],
            ":mobile:application:connectedFreeDebugAndroidTest"
        );
        assert!(device.iter().any(|argument| argument
            == "-Pandroid.testInstrumentationRunnerArguments.class=dev.Test#test"));
        assert!(
            arguments(
                &target,
                TestKind::Unit,
                Path::new("/tmp"),
                &["dev.*".into()],
            )
            .is_err()
        );
        let root = AndroidTarget {
            module: ":".into(),
            ..target
        };
        assert_eq!(
            arguments(&root, TestKind::Unit, Path::new("/tmp"), &[]).unwrap()[0],
            ":testFreeDebugUnitTest"
        );
    }

    #[test]
    fn discovery_handles_annotation_arguments_junit5_and_suites() {
        let cases = discover_source(Path::new("Tests.java"), "package dev;\nclass Tests { @Test(expected = Exception.class) public void fails() {} @Test void packagePrivate() {} }\n@RunWith(Parameterized.class) class Parameters { @Test public void parameter() {} }\n@RunWith(Suite.class) @Suite.SuiteClasses({Tests.class}) class TestSuite {} ").unwrap();
        assert_eq!(cases.len(), 4);
        assert_eq!(cases[1].id.method, "packagePrivate");
        assert_eq!(cases[2].selector(TestKind::Unit), "dev.Parameters");
        assert_eq!(cases[3].selector(TestKind::Unit), "dev.TestSuite");
        let cases = discover_source(Path::new("Tests.kt"), "package dev\nclass Tests { @ParameterizedTest @ValueSource(strings = [\"a\"]) fun parameter(value: String) {} }").unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].selector(TestKind::Unit), "dev.Tests");
    }

    #[test]
    fn directory_and_entry_budgets_are_shared_across_scan_roots() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let control = ScanControl::default();
        let mut budget = WalkBudget {
            directories: MAX_TESTS - 1,
            entries: 0,
        };
        walk(first.path(), 0, &mut budget, &control, &mut |_| Ok(())).unwrap();
        assert!(
            walk(second.path(), 0, &mut budget, &control, &mut |_| Ok(()))
                .unwrap_err()
                .to_string()
                .contains("directory count")
        );
        fs::write(first.path().join("first.kt"), "").unwrap();
        fs::write(second.path().join("second.kt"), "").unwrap();
        let mut budget = WalkBudget {
            directories: 0,
            entries: MAX_TESTS * 2 - 1,
        };
        let mut visited = 0;
        let mut visitor = |_: &Path| {
            visited += 1;
            Ok(())
        };
        walk(first.path(), 0, &mut budget, &control, &mut visitor).unwrap();
        assert!(
            walk(second.path(), 0, &mut budget, &control, &mut visitor)
                .unwrap_err()
                .to_string()
                .contains("traversal")
        );
        assert_eq!(
            visited, 1,
            "No file beyond the aggregate budget may be visited"
        );
    }

    #[test]
    fn cancellation_stops_directory_visits_and_report_reads() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("one.kt"), "class One {}").unwrap();
        fs::write(directory.path().join("two.kt"), "class Two {}").unwrap();
        let control = ScanControl::default();
        let mut visited = 0;
        let result = walk(
            directory.path(),
            0,
            &mut WalkBudget::default(),
            &control,
            &mut |_| {
                visited += 1;
                control.cancel();
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert_eq!(visited, 1);
        assert!(
            discover_with_control(&[directory.path().into()], &control)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        fs::write(directory.path().join("events.jsonl"), "invalid report data").unwrap();
        let reports = read_results_with_control(directory.path(), &[], &control);
        assert!(reports.cases.is_empty());
        assert_eq!(reports.diagnostics, vec!["Test scanning cancelled"]);
    }

    #[test]
    fn large_source_preserves_every_test_line_and_bounds_case_allocation() {
        let mut source = "class Bulk {\n".to_owned();
        for index in 0..4096 {
            source.push_str(&format!("// préserve\n@Test\nfun test{index}() {{}}\n"));
        }
        source.push_str("}\n");
        let cases = discover_source(Path::new("Bulk.kt"), &source).unwrap();
        assert_eq!(cases.len(), 4096);
        for (index, case) in cases.iter().enumerate() {
            assert_eq!(case.id.method, format!("test{index}"));
            assert_eq!(case.source.as_ref().unwrap().line, 4 + index as u32 * 3);
        }
        let source = format!(
            "class Many {{ {} }}",
            "@Test fun method() {} ".repeat(MAX_TESTS + 1)
        );
        assert!(
            discover_source(Path::new("Many.kt"), &source)
                .unwrap_err()
                .to_string()
                .contains("discovery limit")
        );
        let source = format!(
            "class {} {{ {} }}",
            "C".repeat(900),
            "@Test fun method() {} ".repeat(10_000)
        );
        assert!(
            discover_source(Path::new("Identity.kt"), &source)
                .unwrap_err()
                .to_string()
                .contains("identity size")
        );
    }
}
