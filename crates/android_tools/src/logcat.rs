use anyhow::{Context as _, Result, bail, ensure};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone as _};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Arc};

pub const DEFAULT_CAPACITY: usize = 16 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    #[default]
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Assert,
}

impl Level {
    pub const ALL: [Self; 6] = [
        Self::Verbose,
        Self::Debug,
        Self::Info,
        Self::Warn,
        Self::Error,
        Self::Assert,
    ];

    pub fn letter(self) -> &'static str {
        match self {
            Self::Verbose => "V",
            Self::Debug => "D",
            Self::Info => "I",
            Self::Warn => "W",
            Self::Error => "E",
            Self::Assert => "A",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "v" | "verbose" => Ok(Self::Verbose),
            "d" | "debug" => Ok(Self::Debug),
            "i" | "info" => Ok(Self::Info),
            "w" | "warn" | "warning" => Ok(Self::Warn),
            "e" | "error" => Ok(Self::Error),
            "a" | "f" | "assert" | "fatal" => Ok(Self::Assert),
            _ => bail!("Invalid log level: {value}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub id: u64,
    pub timestamp_millis: i64,
    #[serde(default)]
    pub timestamp_nanos: u32,
    pub pid: u32,
    pub tid: u32,
    pub uid: Option<u32>,
    pub level: Level,
    pub tag: String,
    pub message: String,
    #[serde(default)]
    pub process: String,
    #[serde(default)]
    pub package: String,
    #[serde(default)]
    pub buffer: u32,
}

impl Entry {
    pub fn timestamp(&self) -> String {
        DateTime::from_timestamp_millis(self.timestamp_millis)
            .map(|time| {
                time.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_else(|| self.timestamp_millis.to_string())
    }

    pub fn line(&self) -> String {
        format!(
            "{} {:5} {:5} {} {} {} {}: {}",
            self.timestamp(),
            self.pid,
            self.tid,
            self.level.letter(),
            self.package,
            self.process,
            self.tag,
            self.message
        )
    }

    pub fn is_crash(&self) -> bool {
        (self.level == Level::Error
            && self.tag == "AndroidRuntime"
            && self.message.starts_with("FATAL EXCEPTION"))
            || (self.level == Level::Assert && matches!(self.tag.as_str(), "DEBUG" | "libc"))
    }

    pub fn is_stacktrace(&self) -> bool {
        self.message.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("at ") && line.contains('(') && line.contains(')')
        })
    }

    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.tag.len()
            + self.message.len()
            + self.process.len()
            + self.package.len()
    }
}

// Binary logcat preserves message boundaries, including embedded newlines and blank lines.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Entry>> {
        self.pending.extend_from_slice(bytes);
        let mut entries = Vec::new();
        let mut offset = 0;
        while let Some(bytes) = self.pending.get(offset..) {
            if bytes.len() < 4 {
                break;
            }
            let payload_length = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
            let declared_header = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
            let header_length = if declared_header == 0 {
                20
            } else {
                declared_header
            };
            ensure!(
                (20..=64).contains(&header_length),
                "Invalid Logcat header size: {header_length}"
            );
            let length = header_length + payload_length;
            if bytes.len() < length {
                break;
            }
            entries.push(decode_entry(&bytes[..length], header_length)?);
            offset += length;
        }
        self.pending.drain(..offset);
        Ok(entries)
    }

    pub fn finish(&self) -> Result<()> {
        ensure!(
            self.pending.is_empty(),
            "Logcat ended with an incomplete record"
        );
        Ok(())
    }
}

fn word(bytes: &[u8], offset: usize) -> Result<u32> {
    let bytes: [u8; 4] = bytes
        .get(offset..offset + 4)
        .context("Truncated Logcat record")?
        .try_into()?;
    Ok(u32::from_le_bytes(bytes))
}

fn decode_entry(bytes: &[u8], header_length: usize) -> Result<Entry> {
    let buffer = if header_length >= 24 {
        word(bytes, 20)?
    } else {
        0
    };
    let uid = if header_length >= 28 {
        Some(word(bytes, 24)?)
    } else {
        None
    };
    let payload = bytes
        .get(header_length..)
        .context("Missing Logcat payload")?;
    let (level, tag, message) = if matches!(buffer, 2 | 5 | 6) {
        let tag = word(payload, 0)?.to_string();
        let mut cursor = 4;
        let message = match decode_event(payload, &mut cursor, 0) {
            Ok(message) => message,
            Err(error) => format!(
                "Undecoded event ({error}): {}",
                payload
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
        };
        (Level::Info, tag, message)
    } else {
        let priority = *payload.first().context("Empty Logcat payload")?;
        let level = match priority {
            2 => Level::Verbose,
            3 => Level::Debug,
            4 => Level::Info,
            5 => Level::Warn,
            6 => Level::Error,
            7 => Level::Assert,
            _ => bail!("Invalid Logcat priority: {priority}"),
        };
        let payload = payload.get(1..).context("Missing Logcat tag")?;
        let separator = payload
            .iter()
            .position(|byte| *byte == 0)
            .context("Unterminated Logcat tag")?;
        let message = payload
            .get(separator + 1..)
            .context("Missing Logcat message")?;
        (
            level,
            String::from_utf8_lossy(&payload[..separator]).into_owned(),
            String::from_utf8_lossy(message.strip_suffix(&[0]).unwrap_or(message)).into_owned(),
        )
    };
    let nanoseconds = word(bytes, 16)?;
    ensure!(nanoseconds < 1_000_000_000, "Invalid Logcat timestamp");
    Ok(Entry {
        id: 0,
        timestamp_millis: i64::from(word(bytes, 12)?) * 1000 + i64::from(nanoseconds / 1_000_000),
        timestamp_nanos: nanoseconds,
        pid: word(bytes, 4)?,
        tid: word(bytes, 8)?,
        uid,
        level,
        tag,
        message,
        process: String::new(),
        package: String::new(),
        buffer,
    })
}

fn decode_event(bytes: &[u8], cursor: &mut usize, depth: usize) -> Result<String> {
    ensure!(depth < 32, "Logcat event nesting is too deep");
    let kind = *bytes.get(*cursor).context("Truncated event")?;
    *cursor += 1;
    let length = match kind {
        0 | 4 => 4,
        1 => 8,
        2 => {
            let length = word(bytes, *cursor)? as usize;
            *cursor += 4;
            length
        }
        3 => {
            let count = *bytes.get(*cursor).context("Truncated event list")?;
            *cursor += 1;
            let values = (0..count)
                .map(|_| decode_event(bytes, cursor, depth + 1))
                .collect::<Result<Vec<_>>>()?;
            return Ok(format!("[{}]", values.join(", ")));
        }
        _ => bail!("Unknown Logcat event type: {kind}"),
    };
    let end = cursor
        .checked_add(length)
        .context("Event length overflow")?;
    let value = bytes.get(*cursor..end).context("Truncated event value")?;
    *cursor = end;
    Ok(match kind {
        0 => i32::from_le_bytes(value.try_into()?).to_string(),
        1 => i64::from_le_bytes(value.try_into()?).to_string(),
        2 => String::from_utf8_lossy(value).into_owned(),
        4 => f32::from_le_bytes(value.try_into()?).to_string(),
        _ => unreachable!(),
    })
}

pub struct Buffer {
    pub entries: VecDeque<Arc<Entry>>,
    pub dropped: u64,
    capacity: usize,
    bytes: usize,
    next_id: u64,
}

impl Buffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            dropped: 0,
            capacity,
            bytes: 0,
            next_id: 1,
        }
    }

    pub fn push(&mut self, mut entry: Entry) {
        entry.id = self.next_id;
        self.next_id += 1;
        self.bytes += entry.size();
        self.entries.push_back(Arc::new(entry));
        self.trim();
    }

    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        self.trim();
    }

    fn trim(&mut self) {
        while self.bytes > self.capacity || self.entries.len() > MAX_ENTRIES {
            if let Some(entry) = self.entries.pop_front() {
                self.bytes -= entry.size();
                self.dropped += 1;
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.dropped = 0;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub uid: u32,
    pub name: String,
    pub package: String,
}

pub fn parse_processes(output: &str) -> Result<Vec<Process>> {
    let mut lines = output.lines();
    let header = lines.next().context("ADB returned no process list")?;
    ensure!(
        header.split_whitespace().eq(["PID", "UID", "ARGS"]),
        "Unexpected process list columns: {header}"
    );
    lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next().context("Missing process ID")?.parse()?;
            let uid = fields.next().context("Missing process UID")?.parse()?;
            let name = fields.next().context("Missing process name")?.to_string();
            let package = name.split(':').next().unwrap_or_default();
            let package = if package.contains('.') && !package.contains('/') {
                package.to_string()
            } else {
                String::new()
            };
            Ok(Process {
                pid,
                uid,
                name,
                package,
            })
        })
        .collect()
}

pub fn parse_package_uids(output: &str) -> Result<std::collections::HashMap<u32, Vec<String>>> {
    let mut packages = std::collections::HashMap::<u32, Vec<String>>::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let package = fields
            .next()
            .and_then(|field| field.strip_prefix("package:"))
            .context("Unexpected package list output")?;
        ensure!(
            package == "android" || valid_package(package),
            "Invalid package in package list"
        );
        let uid = fields
            .find_map(|field| field.strip_prefix("uid:"))
            .context("Missing package UID")?
            .parse()?;
        packages.entry(uid).or_default().push(package.into());
    }
    for names in packages.values_mut() {
        names.sort();
        names.dedup();
    }
    Ok(packages)
}

pub fn parse_event_tags(output: &str) -> Result<std::collections::HashMap<u32, String>> {
    let mut tags = std::collections::HashMap::new();
    for line in output
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        let mut fields = line.split_whitespace();
        let id = fields.next().context("Missing event tag ID")?.parse()?;
        let name = fields.next().context("Missing event tag name")?;
        tags.insert(id, name.to_string());
    }
    Ok(tags)
}

#[derive(Clone, Debug)]
enum Expression {
    All,
    Term(Term),
    And(Vec<Expression>),
    Or(Vec<Expression>),
    Not(Box<Expression>),
}

#[derive(Clone, Debug)]
struct Term {
    field: String,
    value: String,
    matcher: Matcher,
    negated: bool,
}

#[derive(Clone, Debug)]
enum Matcher {
    Contains,
    Exact,
    Regex(Regex),
    Level(Level),
    Age(i64),
    Is(String),
    Mine,
}

#[derive(Clone, Debug)]
pub struct Query {
    expression: Expression,
    match_case: bool,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            expression: Expression::All,
            match_case: false,
        }
    }
}

pub struct FilterContext<'a> {
    pub now_millis: i64,
    pub project_packages: &'a [String],
}

#[derive(Clone)]
pub struct Search(Regex);

impl Search {
    pub fn new(text: &str, match_case: bool, regular_expression: bool) -> Result<Option<Self>> {
        if text.is_empty() {
            return Ok(None);
        }
        ensure!(text.len() <= 16_384, "Search is too long");
        let pattern = if regular_expression {
            text.to_string()
        } else {
            regex::escape(text)
        };
        Ok(Some(Self(
            RegexBuilder::new(&pattern)
                .case_insensitive(!match_case)
                .size_limit(1 << 20)
                .build()?,
        )))
    }

    pub fn matches(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    pub fn ranges(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        self.0
            .find_iter(text)
            .filter(|found| !found.is_empty())
            .take(1000)
            .map(|found| found.range())
            .collect()
    }
}

impl Query {
    pub fn parse(text: &str, match_case: bool) -> Result<Self> {
        let tokens = tokenize(text)?;
        ensure!(tokens.len() <= 512, "Filter is too complex");
        let mut parser = Parser {
            tokens,
            cursor: 0,
            match_case,
            depth: 0,
        };
        let expression = if parser.tokens.is_empty() {
            Expression::All
        } else {
            parser.sequence()?
        };
        ensure!(
            parser.cursor == parser.tokens.len(),
            "Unexpected closing parenthesis"
        );
        Ok(Self {
            expression,
            match_case,
        })
    }

    pub fn matches(&self, entry: &Entry, context: &FilterContext<'_>) -> bool {
        self.expression.matches(entry, context, self.match_case)
    }
}

impl Expression {
    fn matches(&self, entry: &Entry, context: &FilterContext<'_>, match_case: bool) -> bool {
        match self {
            Self::All => true,
            Self::And(expressions) => expressions
                .iter()
                .all(|expression| expression.matches(entry, context, match_case)),
            Self::Or(expressions) => expressions
                .iter()
                .any(|expression| expression.matches(entry, context, match_case)),
            Self::Not(expression) => !expression.matches(entry, context, match_case),
            Self::Term(term) => {
                let matched = match &term.matcher {
                    Matcher::Level(level) => entry.level >= *level,
                    Matcher::Age(age) => {
                        entry.timestamp_millis >= context.now_millis.saturating_sub(*age)
                    }
                    Matcher::Mine => context
                        .project_packages
                        .iter()
                        .any(|package| package == &entry.package),
                    Matcher::Is(value) => match value.as_str() {
                        "crash" => entry.is_crash(),
                        "stacktrace" => entry.is_stacktrace(),
                        "firebase" => {
                            matches!(
                                entry.tag.as_str(),
                                "AppInstallOperation"
                                    | "AppInviteActivity"
                                    | "AppInviteAgent"
                                    | "AppInviteAnalytics"
                                    | "AppInviteLogger"
                                    | "BackgroundTask"
                                    | "ClassMapper"
                                    | "Connection"
                                    | "DataOperation"
                                    | "EventRaiser"
                                    | "FA"
                                    | "FirebaseAppIndex"
                                    | "FirebaseDatabase"
                                    | "FirebaseInstanceId"
                                    | "FirebaseMessaging"
                                    | "FirebaseRemoteConfig"
                                    | "NetworkRequest"
                                    | "Persistence"
                                    | "PersistentConnection"
                                    | "RepoOperation"
                                    | "RunLoop"
                                    | "StorageTask"
                                    | "SyncTree"
                                    | "Transaction"
                                    | "WebSocket"
                            )
                        }
                        _ => Level::parse(value).is_ok_and(|level| entry.level == level),
                    },
                    _ => {
                        let value = match term.field.as_str() {
                            "tag" => entry.tag.clone(),
                            "package" => entry.package.clone(),
                            "process" => entry.process.clone(),
                            "message" => entry.message.clone(),
                            "pid" => entry.pid.to_string(),
                            "tid" => entry.tid.to_string(),
                            "uid" => entry.uid.map(|uid| uid.to_string()).unwrap_or_default(),
                            "name" => return true,
                            _ => entry.line(),
                        };
                        match &term.matcher {
                            Matcher::Regex(regex) => regex.is_match(&value),
                            Matcher::Exact => {
                                if match_case {
                                    value == term.value
                                } else {
                                    value.to_lowercase() == term.value.to_lowercase()
                                }
                            }
                            _ => {
                                if match_case {
                                    value.contains(&term.value)
                                } else {
                                    value.to_lowercase().contains(&term.value.to_lowercase())
                                }
                            }
                        }
                    }
                };
                matched != term.negated
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Value(String),
    Left,
    Right,
    And,
    Or,
    Not,
}

fn tokenize(text: &str) -> Result<Vec<Token>> {
    ensure!(text.len() <= 16_384, "Filter is too long");
    let mut tokens = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character.is_whitespace() {
            continue;
        }
        match character {
            '(' => tokens.push(Token::Left),
            ')' => tokens.push(Token::Right),
            '&' => {
                if characters.peek() == Some(&'&') {
                    characters.next();
                }
                tokens.push(Token::And);
            }
            '|' => {
                if characters.peek() == Some(&'|') {
                    characters.next();
                }
                tokens.push(Token::Or);
            }
            '!' => tokens.push(Token::Not),
            _ => {
                let mut value = String::new();
                let mut current = Some(character);
                let mut quote = None;
                while let Some(character) = current {
                    if let Some(delimiter) = quote {
                        if character == delimiter {
                            quote = None;
                        } else if character == '\\'
                            && characters.peek().is_some_and(|character| {
                                *character == delimiter || *character == '\\'
                            })
                        {
                            value.push(characters.next().context("Missing escaped quote")?);
                        } else {
                            value.push(character);
                        }
                    } else if character == '"' || character == '\'' {
                        quote = Some(character);
                    } else {
                        value.push(character);
                    }
                    if quote.is_none()
                        && characters.peek().is_none_or(|character| {
                            character.is_whitespace() || matches!(character, '(' | ')' | '&' | '|')
                        })
                    {
                        break;
                    }
                    current = characters.next();
                }
                ensure!(quote.is_none(), "Unclosed quoted value");
                tokens.push(match value.as_str() {
                    "AND" => Token::And,
                    "OR" => Token::Or,
                    "NOT" => Token::Not,
                    _ => Token::Value(value),
                });
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    match_case: bool,
    depth: usize,
}

impl Parser {
    // Android Studio groups positive top-level terms by key; exclusions and bare words remain conjunctive.
    fn sequence(&mut self) -> Result<Expression> {
        self.depth += 1;
        ensure!(self.depth <= 32, "Filter nesting is too deep");
        let mut groups: Vec<(Option<String>, Vec<Expression>)> = Vec::new();
        while self.cursor < self.tokens.len() && self.tokens.get(self.cursor) != Some(&Token::Right)
        {
            let expression = self.or()?;
            let key = match &expression {
                Expression::Term(term)
                    if !term.negated && term.field != "line-implicit" && term.field != "name" =>
                {
                    Some(match &term.matcher {
                        Matcher::Is(value) if Level::parse(value).is_ok() => "level".into(),
                        _ => term.field.clone(),
                    })
                }
                _ => None,
            };
            if let Some(group) = key.as_ref().and_then(|key| {
                groups
                    .iter_mut()
                    .find(|group| group.0.as_ref() == Some(key))
            }) {
                group.1.push(expression);
            } else {
                groups.push((key, vec![expression]));
            }
        }
        self.depth -= 1;
        ensure!(!groups.is_empty(), "Expected a filter expression");
        Ok(Expression::And(
            groups
                .into_iter()
                .map(|(_, expressions)| {
                    if expressions.len() == 1 {
                        expressions.into_iter().next().unwrap_or(Expression::All)
                    } else {
                        Expression::Or(expressions)
                    }
                })
                .collect(),
        ))
    }

    fn or(&mut self) -> Result<Expression> {
        let mut expressions = vec![self.and()?];
        while self.tokens.get(self.cursor) == Some(&Token::Or) {
            self.cursor += 1;
            expressions.push(self.and()?);
        }
        Ok(if expressions.len() == 1 {
            expressions.remove(0)
        } else {
            Expression::Or(expressions)
        })
    }

    fn and(&mut self) -> Result<Expression> {
        let mut expressions = vec![self.atom()?];
        while self.tokens.get(self.cursor) == Some(&Token::And) {
            self.cursor += 1;
            expressions.push(self.atom()?);
        }
        Ok(if expressions.len() == 1 {
            expressions.remove(0)
        } else {
            Expression::And(expressions)
        })
    }

    fn atom(&mut self) -> Result<Expression> {
        let token = self
            .tokens
            .get(self.cursor)
            .cloned()
            .context("Expected a filter after the operator")?;
        self.cursor += 1;
        match token {
            Token::Left => {
                let expression = self.sequence()?;
                ensure!(
                    self.tokens.get(self.cursor) == Some(&Token::Right),
                    "Missing closing parenthesis"
                );
                self.cursor += 1;
                Ok(expression)
            }
            Token::Not => {
                self.depth += 1;
                ensure!(self.depth <= 32, "Filter nesting is too deep");
                let expression = self.atom()?;
                self.depth -= 1;
                Ok(Expression::Not(Box::new(expression)))
            }
            Token::Value(value) => Ok(Expression::Term(self.term(value)?)),
            _ => bail!("Expected a filter value"),
        }
    }

    fn term(&self, text: String) -> Result<Term> {
        let Some((key, value)) = text.split_once(':') else {
            return Ok(Term {
                field: "line-implicit".into(),
                value: text,
                matcher: Matcher::Contains,
                negated: false,
            });
        };
        let negated = key.starts_with('-');
        let key = key.trim_start_matches('-');
        let regex = key.ends_with('~');
        let exact = key.ends_with('=');
        let field = key.trim_end_matches(['~', '=']);
        ensure!(!value.is_empty(), "Missing value for {field}:");
        let matcher = match field {
            "level" => {
                ensure!(!regex && !exact && !negated, "Use is: for an exact level");
                Matcher::Level(Level::parse(value)?)
            }
            "age" => {
                ensure!(!regex && !exact && !negated, "Invalid age modifier");
                let amount = value
                    .get(..value.len().saturating_sub(1))
                    .context("Invalid age")?
                    .parse::<i64>()
                    .context("Use age:30s, age:5m, age:2h or age:1d")?;
                let multiplier = match value.chars().last() {
                    Some('s') => 1000,
                    Some('m') => 60_000,
                    Some('h') => 3_600_000,
                    Some('d') => 86_400_000,
                    _ => bail!("Invalid age unit"),
                };
                ensure!(amount >= 0, "Age must be positive");
                Matcher::Age(amount.checked_mul(multiplier).context("Age is too large")?)
            }
            "is" => {
                ensure!(!regex && !exact, "Invalid is: modifier");
                ensure!(
                    matches!(value, "crash" | "stacktrace" | "firebase")
                        || Level::parse(value).is_ok(),
                    "Unknown is: filter: {value}"
                );
                Matcher::Is(value.into())
            }
            "package" if value == "mine" && !regex && !exact => Matcher::Mine,
            "tag" | "package" | "process" | "message" | "line" | "pid" | "tid" | "uid" | "name" => {
                if regex {
                    Matcher::Regex(
                        RegexBuilder::new(value)
                            .case_insensitive(!self.match_case)
                            .size_limit(1 << 20)
                            .build()
                            .context("Invalid regular expression")?,
                    )
                } else if exact || matches!(field, "pid" | "tid" | "uid") {
                    Matcher::Exact
                } else {
                    Matcher::Contains
                }
            }
            _ => bail!("Unknown filter field: {field}"),
        };
        Ok(Term {
            field: field.into(),
            value: value.into(),
            matcher,
            negated,
        })
    }
}

pub fn export(entries: impl IntoIterator<Item = impl AsRef<Entry>>) -> Result<String> {
    entries
        .into_iter()
        .map(|entry| Ok(serde_json::to_string(entry.as_ref())? + "\n"))
        .collect()
}

pub fn import(text: &str) -> Result<Vec<Entry>> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(messages) = value
            .get("logcatMessages")
            .and_then(|messages| messages.as_array())
    {
        ensure!(
            messages.len() <= MAX_ENTRIES,
            "Logcat file contains too many entries"
        );
        return messages.iter().map(import_studio_message).collect();
    }
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with("---------") {
            continue;
        }
        if line.trim_start().starts_with('{') {
            entries.push(
                serde_json::from_str(line)
                    .with_context(|| format!("Invalid Logcat JSON at line {}", index + 1))?,
            );
            ensure!(
                entries.len() <= MAX_ENTRIES,
                "Logcat file contains too many entries"
            );
            continue;
        }
        if let Some(entry) = parse_threadtime(line) {
            entries.push(entry);
        } else if let Some(entry) = entries.last_mut() {
            entry.message.push('\n');
            entry.message.push_str(line);
        } else {
            bail!(
                "Unrecognized Logcat file at line {}. Use an exported JSONL or year/threadtime log.",
                index + 1
            );
        }
        ensure!(
            entries.len() <= MAX_ENTRIES,
            "Logcat file contains too many entries"
        );
    }
    Ok(entries)
}

fn import_studio_message(message: &serde_json::Value) -> Result<Entry> {
    let header = message
        .get("header")
        .context("Android Studio log is missing a header")?;
    let string = |key| -> Result<String> {
        Ok(header
            .get(key)
            .and_then(|value| value.as_str())
            .with_context(|| format!("Missing Logcat header field {key}"))?
            .into())
    };
    let number = |key| -> Result<u32> {
        Ok(header
            .get(key)
            .and_then(|value| value.as_u64())
            .with_context(|| format!("Missing Logcat header field {key}"))?
            .try_into()?)
    };
    let timestamp = header.get("timestamp").context("Missing timestamp")?;
    let timestamp = if let Some(text) = timestamp.as_str() {
        DateTime::parse_from_rfc3339(text)?.with_timezone(&chrono::Utc)
    } else {
        let seconds = timestamp
            .get("seconds")
            .and_then(|value| value.as_i64())
            .context("Missing timestamp seconds")?;
        let nanos: u32 = timestamp
            .get("nanos")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .try_into()?;
        ensure!(nanos < 1_000_000_000, "Invalid timestamp nanoseconds");
        DateTime::from_timestamp(seconds, nanos).context("Invalid timestamp")?
    };
    Ok(Entry {
        id: 0,
        timestamp_millis: timestamp.timestamp_millis(),
        timestamp_nanos: timestamp.timestamp_subsec_nanos(),
        pid: number("pid")?,
        tid: number("tid")?,
        uid: None,
        level: Level::parse(&string("logLevel")?)?,
        tag: string("tag")?,
        process: string("processName")?,
        package: string("applicationId")?,
        buffer: 0,
        message: message
            .get("message")
            .and_then(|value| value.as_str())
            .context("Missing Logcat message")?
            .into(),
    })
}

pub fn export_studio(entries: &[Arc<Entry>]) -> Result<String> {
    let messages = entries.iter().map(|entry| serde_json::json!({
        "header": {"logLevel": format!("{:?}", entry.level).to_uppercase(), "pid": entry.pid, "tid": entry.tid,
            "applicationId": entry.package, "processName": entry.process, "tag": entry.tag,
            "timestamp": {"seconds": entry.timestamp_millis.div_euclid(1000), "nanos": entry.timestamp_nanos}},
        "message": entry.message,
    })).collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(
        &serde_json::json!({"metadata": null, "logcatMessages": messages}),
    )?)
}

pub fn valid_package(package: &str) -> bool {
    package.contains('.')
        && package.split('.').all(|part| {
            !part.is_empty()
                && part.starts_with(|character: char| {
                    character.is_ascii_alphabetic() || character == '_'
                })
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}

fn parse_threadtime(line: &str) -> Option<Entry> {
    let mut fields = line.split_whitespace();
    let date = fields.next()?;
    let date = if date.len() == 5 {
        format!("{}-{date}", Local::now().format("%Y"))
    } else {
        date.into()
    };
    let time = fields.next()?;
    let timestamp =
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S%.f").ok()?;
    let pid = fields.next()?.parse().ok()?;
    let tid = fields.next()?.parse().ok()?;
    let level = Level::parse(fields.next()?).ok()?;
    let prefix_length = line.find(time)? + time.len();
    let remainder = line.get(prefix_length..)?.trim_start();
    let mut remainder = remainder;
    for _ in 0..3 {
        let end = remainder.find(char::is_whitespace)?;
        remainder = remainder.get(end..)?.trim_start();
    }
    let (tag, message) = remainder.split_once(':')?;
    Some(Entry {
        id: 0,
        timestamp_millis: Local
            .from_local_datetime(&timestamp)
            .earliest()?
            .timestamp_millis(),
        timestamp_nanos: timestamp.and_utc().timestamp_subsec_nanos(),
        pid,
        tid,
        uid: None,
        level,
        tag: tag.trim_end().into(),
        message: message.strip_prefix(' ').unwrap_or(message).into(),
        process: String::new(),
        package: String::new(),
        buffer: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry {
            id: 0,
            timestamp_millis: 10_000,
            timestamp_nanos: 0,
            pid: 42,
            tid: 43,
            uid: Some(10123),
            level: Level::Error,
            tag: "AndroidRuntime".into(),
            message: "FATAL EXCEPTION: main\n\n    at app.Main.run(Main.kt:12)\n".into(),
            process: "com.example:worker".into(),
            package: "com.example".into(),
            buffer: 3,
        }
    }

    fn record(header_length: usize) -> Vec<u8> {
        let mut payload = vec![6];
        payload
            .extend(b"AndroidRuntime\0FATAL EXCEPTION: main\n\n at app.Main.run(Main.kt:12)\n\0");
        let mut bytes = Vec::new();
        bytes.extend((payload.len() as u16).to_le_bytes());
        bytes.extend(
            (if header_length == 20 {
                0
            } else {
                header_length as u16
            })
            .to_le_bytes(),
        );
        for value in [42u32, 43, 10, 123_000_000] {
            bytes.extend(value.to_le_bytes());
        }
        if header_length >= 24 {
            bytes.extend(3u32.to_le_bytes());
        }
        if header_length >= 28 {
            bytes.extend(10123u32.to_le_bytes());
        }
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn binary_records_survive_every_chunk_boundary_and_preserve_multiline() -> Result<()> {
        for header_length in [20, 24, 28] {
            let bytes = [record(header_length), record(header_length)].concat();
            for boundary in 0..=bytes.len() {
                let mut decoder = Decoder::default();
                let mut entries = decoder.push(&bytes[..boundary])?;
                entries.extend(decoder.push(&bytes[boundary..])?);
                decoder.finish()?;
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].timestamp_millis, 10_123);
                assert_eq!(entries[0].pid, 42);
                assert!(entries[0].message.contains("\n\n"));
                assert_eq!(entries[0].uid, (header_length >= 28).then_some(10123));
            }
        }
        let mut decoder = Decoder::default();
        assert!(decoder.push(&[0, 0, 1, 0]).is_err());
        let mut decoder = Decoder::default();
        decoder.push(&[1, 0])?;
        assert!(decoder.finish().is_err());
        Ok(())
    }

    #[test]
    fn studio_query_semantics_and_validation() -> Result<()> {
        let entry = entry();
        let packages = vec!["com.example".into()];
        let context = FilterContext {
            now_millis: 12_000,
            project_packages: &packages,
        };
        for query in [
            "",
            "package:mine level:WARN",
            "tag:nope tag:AndroidRuntime",
            "-tag:nope -tag:other",
            "(tag:nope | tag:AndroidRuntime) & is:crash",
            "message~:\"FATAL.*main\"",
            "package=:com.example process:worker",
            "age:3s",
            "is:error",
            "is:stacktrace",
            "pid:42 tid:43 uid:10123",
            "name:\"My filter\" is:crash",
            "NOT tag:other",
            "fatal main",
        ] {
            assert!(
                Query::parse(query, false)?.matches(&entry, &context),
                "{query}"
            );
        }
        for query in [
            "tag:nope & tag:AndroidRuntime",
            "-package:mine",
            "age:1s",
            "level:ASSERT",
            "is:debug",
            "pid:4",
            "tag:androidruntime",
        ] {
            assert!(
                !Query::parse(query, true)?.matches(&entry, &context),
                "{query}"
            );
        }
        for query in [
            "tag:",
            "level:nope",
            "message~:[",
            "(tag:a",
            "tag:a |",
            "tag:a)",
            "age:999999999999999999d",
            "unknown:a",
            "message:\"unclosed",
            "age:-3s",
        ] {
            assert!(Query::parse(query, false).is_err(), "{query}");
        }
        assert!(Query::parse(&"(".repeat(33), false).is_err());
        assert!(Query::parse(&"!".repeat(33), false).is_err());
        assert!(Query::parse("message~:\"\\d+\"", false)?.matches(&entry, &context));
        Ok(())
    }

    #[test]
    fn ring_buffer_enforces_byte_budget_and_stable_identity() {
        let mut buffer = Buffer::new(entry().size() * 2);
        buffer.push(entry());
        buffer.push(entry());
        buffer.push(entry());
        assert_eq!(buffer.entries.len(), 2);
        assert_eq!(buffer.entries.front().map(|entry| entry.id), Some(2));
        assert_eq!(buffer.dropped, 1);
        buffer.set_capacity(0);
        assert!(buffer.entries.is_empty());
        buffer.clear();
        assert_eq!(buffer.dropped, 0);
    }

    #[test]
    fn export_import_round_trip_and_threadtime() -> Result<()> {
        let original = entry();
        let text = export([Arc::new(original.clone())])?;
        let imported = import(&text)?;
        assert_eq!(
            serde_json::to_value(&original)?,
            serde_json::to_value(&imported[0])?
        );
        let imported = import(
            "--------- beginning of main\n2026-10-01 12:00:00.123 42 43 E AndroidRuntime: FATAL EXCEPTION\n    at app.Main.run(Main.kt:12)",
        )?;
        assert_eq!(imported[0].pid, 42);
        assert_eq!(imported[0].tag, "AndroidRuntime");
        assert!(imported[0].message.contains("\n    at"));
        assert!(import("not a log").is_err());
        assert!(import("{bad json}").is_err());
        Ok(())
    }

    #[test]
    fn process_names_keep_secondary_processes() -> Result<()> {
        let processes = parse_processes(
            "PID UID ARGS\n42 10123 com.example:worker\n99 1000 /system/bin/surfaceflinger\n",
        )?;
        assert_eq!(processes[0].package, "com.example");
        assert_eq!(processes[0].name, "com.example:worker");
        assert!(processes[1].package.is_empty());
        assert!(parse_processes("PID USER NAME\n42 u0_a123 app").is_err());
        let packages =
            parse_package_uids("package:com.example uid:10123\npackage:com.shared uid:10123\n")?;
        assert_eq!(
            packages.get(&10123),
            Some(&vec!["com.example".into(), "com.shared".into()])
        );
        assert!(parse_package_uids("Failure: no connection").is_err());
        assert_eq!(
            parse_event_tags("# tags\n30014 am_proc_start (User|1)\n")?
                .get(&30014)
                .map(String::as_str),
            Some("am_proc_start")
        );
        Ok(())
    }

    #[test]
    fn event_buffers_decode_nested_values_and_reject_truncation() -> Result<()> {
        let mut payload = vec![3, 3, 0];
        payload.extend(123_i32.to_le_bytes());
        payload.push(2);
        payload.extend(5_u32.to_le_bytes());
        payload.extend(b"hello");
        payload.extend([3, 1, 1]);
        payload.extend(456_i64.to_le_bytes());
        let mut cursor = 0;
        assert_eq!(
            decode_event(&payload, &mut cursor, 0)?,
            "[123, hello, [456]]"
        );
        assert_eq!(cursor, payload.len());
        for boundary in 0..payload.len() {
            assert!(decode_event(&payload[..boundary], &mut 0, 0).is_err());
        }
        assert!(decode_event(&[99], &mut 0, 0).is_err());
        let mut record = record(28);
        record.truncate(28);
        let mut event = 100_u32.to_le_bytes().to_vec();
        event.extend(payload);
        record[..2].copy_from_slice(&(event.len() as u16).to_le_bytes());
        record[20..24].copy_from_slice(&2_u32.to_le_bytes());
        record.extend(event);
        let decoded = Decoder::default().push(&record)?;
        assert_eq!(decoded[0].tag, "100");
        assert_eq!(decoded[0].message, "[123, hello, [456]]");
        Ok(())
    }

    #[test]
    fn studio_json_files_preserve_processes_and_multiline_timestamps() -> Result<()> {
        let original = Arc::new(entry());
        let imported = import(&export_studio(std::slice::from_ref(&original))?)?;
        assert_eq!(imported[0].message, original.message);
        assert_eq!(imported[0].process, original.process);
        assert_eq!(imported[0].package, original.package);
        assert_eq!(imported[0].timestamp_millis, original.timestamp_millis);
        assert_eq!(imported[0].level, original.level);
        let mut value: serde_json::Value = serde_json::from_str(&export_studio(&[original])?)?;
        value["logcatMessages"][0]["header"]["timestamp"] =
            serde_json::json!("2026-10-01T12:00:00.123456789Z");
        assert_eq!(import(&value.to_string())?[0].timestamp_nanos, 123_456_789);
        value["logcatMessages"][0]["header"]["pid"] = serde_json::json!(-1);
        assert!(import(&value.to_string()).is_err());
        Ok(())
    }

    #[test]
    fn regex_search_highlights_unicode_and_validates_patterns() -> Result<()> {
        let search = Search::new("résumé", false, false)?.expect("Nonempty search");
        assert!(search.matches("RÉSUMÉ"));
        assert_eq!(search.ranges("a résumé résumé"), [2..10, 11..19]);
        assert!(
            !Search::new("résumé", true, false)?
                .expect("Search")
                .matches("RÉSUMÉ")
        );
        assert!(Search::new("[", false, true).is_err());
        assert!(
            Search::new("[", false, false)?
                .expect("Literal bracket")
                .matches("[")
        );
        assert!(Search::new("", false, true)?.is_none());
        assert!(
            Search::new("^", false, true)?
                .expect("Zero width search")
                .ranges("hello")
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn package_actions_reject_shell_syntax_and_quoted_values_keep_backslashes() -> Result<()> {
        assert!(valid_package("dev.example.app"));
        assert!(valid_package("dev.example._app2"));
        for package in [
            "",
            "example",
            "dev..app",
            "dev.2app",
            "dev.app;id",
            "dev.$(id)",
            "dev.app other",
        ] {
            assert!(!valid_package(package));
        }
        let mut entry = entry();
        entry.tag = "ends\\".into();
        let context = FilterContext {
            now_millis: 10_000,
            project_packages: &[],
        };
        assert!(Query::parse(r#"tag=:"ends\\""#, true)?.matches(&entry, &context));
        assert!(Query::parse("level:ASSERT is:ERROR", true)?.matches(&entry, &context));
        Ok(())
    }
}
