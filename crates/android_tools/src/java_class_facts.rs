/*
 * Copyright (C) 2017 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Bounded Java declaration facts for the Android tree, without filename inference.
//!
//! This recognizes declarations and balanced lexical structure, not Java type
//! checking or statement grammar. Unsupported inputs produce no partial facts.
//! Callers must bind offsets to the exact source revision they passed here.
//! The original generated-class input is retained with attribution under
//! `test_data/java_class_facts`; its full Gradle-backed case remains unported.

use crate::project_tree::JavaClassFact;
use std::{error::Error, fmt};

/// Prevent a single source buffer from consuming unbounded background work.
pub const MAX_JAVA_FACT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_JAVA_TYPE_DECLARATIONS: usize = 16_384;
const MAX_DELIMITER_DEPTH: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaTypeKind {
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaTypeDeclaration {
    pub name: String,
    pub kind: JavaTypeKind,
    /// Start of the actual name token in the unchanged UTF-8 source.
    pub name_byte_offset: usize,
    /// First annotation, modifier or type keyword belonging to the declaration.
    pub declaration_byte_offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaSourceFacts {
    pub package: Option<String>,
    /// Top-level types in source encounter order; nested/local types are omitted.
    pub declarations: Vec<JavaTypeDeclaration>,
}

impl JavaSourceFacts {
    pub fn into_class_facts(self) -> Vec<JavaClassFact> {
        self.declarations
            .into_iter()
            .map(|declaration| JavaClassFact {
                name: declaration.name,
                byte_offset: Some(declaration.name_byte_offset),
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaFactsErrorKind {
    InputTooLarge,
    InvalidUtf8,
    UnicodeEscape,
    NonAsciiIdentifier,
    MalformedLexeme,
    UnsupportedTopLevelSyntax,
    MalformedDeclaration,
    UnterminatedComment,
    UnterminatedLiteral,
    MalformedLiteral,
    InvalidTextBlockOpening,
    UnbalancedDelimiter,
    NestingTooDeep,
    TooManyDeclarations,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JavaFactsError {
    pub kind: JavaFactsErrorKind,
    pub byte_offset: usize,
}

impl fmt::Display for JavaFactsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Java declaration facts unavailable: {:?} at byte {}",
            self.kind, self.byte_offset
        )
    }
}

impl Error for JavaFactsError {}

type FactsResult<T> = Result<T, JavaFactsError>;

/// Inspect actual Java source bytes. This deliberately has no filename input.
///
/// Unicode escapes are rejected even in comments and literals because Java
/// processes eligible escapes before tokenization. Non-ASCII identifiers need
/// Java's identifier tables; Unicode comments and literal contents are accepted.
pub fn parse_java_class_facts(source: &[u8]) -> FactsResult<JavaSourceFacts> {
    if source.len() > MAX_JAVA_FACT_BYTES {
        return Err(error(
            JavaFactsErrorKind::InputTooLarge,
            MAX_JAVA_FACT_BYTES,
        ));
    }
    let source = std::str::from_utf8(source)
        .map_err(|failure| error(JavaFactsErrorKind::InvalidUtf8, failure.valid_up_to()))?;
    if let Some(byte_offset) = source.find("\\u") {
        return Err(error(JavaFactsErrorKind::UnicodeEscape, byte_offset));
    }
    Parser::new(source).parse()
}

pub fn discover_top_level_java_classes(source: &[u8]) -> FactsResult<Vec<JavaClassFact>> {
    parse_java_class_facts(source).map(JavaSourceFacts::into_class_facts)
}

fn error(kind: JavaFactsErrorKind, byte_offset: usize) -> JavaFactsError {
    JavaFactsError { kind, byte_offset }
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    text: &'a str,
    byte_offset: usize,
    literal: bool,
}

impl Token<'_> {
    fn is_identifier(self) -> bool {
        !self.literal
            && self.text.bytes().next().is_some_and(identifier_start)
            && self.text.bytes().all(identifier_part)
            && !reserved_word(self.text)
    }
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

fn identifier_part(byte: u8) -> bool {
    identifier_start(byte) || byte.is_ascii_digit()
}

fn reserved_word(word: &str) -> bool {
    matches!(
        word,
        "abstract"
            | "assert"
            | "boolean"
            | "break"
            | "byte"
            | "case"
            | "catch"
            | "char"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "do"
            | "double"
            | "else"
            | "enum"
            | "extends"
            | "final"
            | "finally"
            | "float"
            | "for"
            | "goto"
            | "if"
            | "implements"
            | "import"
            | "instanceof"
            | "int"
            | "interface"
            | "long"
            | "native"
            | "new"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "short"
            | "static"
            | "strictfp"
            | "super"
            | "switch"
            | "synchronized"
            | "this"
            | "throw"
            | "throws"
            | "transient"
            | "try"
            | "void"
            | "volatile"
            | "while"
            | "_"
            | "true"
            | "false"
            | "null"
    )
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> FactsResult<Option<Token<'a>>> {
        loop {
            let Some(byte) = self.source.as_bytes().get(self.offset).copied() else {
                return Ok(None);
            };
            let remaining = &self.source[self.offset..];
            if matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0c) {
                self.offset += 1;
            } else if remaining.starts_with("//") {
                self.offset += remaining.find(['\r', '\n']).unwrap_or(remaining.len());
            } else if remaining.starts_with("/*") {
                let end = remaining[2..]
                    .find("*/")
                    .ok_or_else(|| error(JavaFactsErrorKind::UnterminatedComment, self.offset))?;
                self.offset += end + 4;
            } else {
                break;
            }
        }

        let start = self.offset;
        let remaining = &self.source[start..];
        let Some(first) = remaining.bytes().next() else {
            return Ok(None);
        };
        let literal = matches!(first, b'\'' | b'"');
        if remaining.starts_with("\"\"\"") {
            self.text_block()?;
        } else if literal {
            self.quoted(first)?;
        } else if first >= 0x80 {
            return Err(error(JavaFactsErrorKind::NonAsciiIdentifier, start));
        } else if identifier_start(first) {
            self.offset += remaining
                .bytes()
                .take_while(|byte| identifier_part(*byte))
                .count();
        } else if first.is_ascii_digit()
            || matches!(
                first,
                b'.' | b','
                    | b';'
                    | b'('
                    | b')'
                    | b'{'
                    | b'}'
                    | b'['
                    | b']'
                    | b'@'
                    | b':'
                    | b'?'
                    | b'~'
                    | b'+'
                    | b'-'
                    | b'*'
                    | b'/'
                    | b'%'
                    | b'&'
                    | b'|'
                    | b'^'
                    | b'!'
                    | b'='
                    | b'<'
                    | b'>'
            )
        {
            self.offset += 1;
        } else {
            return Err(error(JavaFactsErrorKind::MalformedLexeme, start));
        }
        Ok(Some(Token {
            text: &self.source[start..self.offset],
            byte_offset: start,
            literal,
        }))
    }

    fn quoted(&mut self, quote: u8) -> FactsResult<()> {
        let start = self.offset;
        self.offset += 1;
        let mut character_units = 0;
        while let Some(byte) = self.source.as_bytes().get(self.offset).copied() {
            if matches!(byte, b'\r' | b'\n') {
                return Err(error(JavaFactsErrorKind::UnterminatedLiteral, start));
            }
            if byte == quote {
                self.offset += 1;
                if quote == b'\'' && character_units != 1 {
                    return Err(error(JavaFactsErrorKind::MalformedLiteral, start));
                }
                return Ok(());
            }
            if byte == b'\\' {
                self.escape(false)?;
                character_units += 1;
            } else if let Some(character) = self.source[self.offset..].chars().next() {
                self.offset += character.len_utf8();
                character_units += character.len_utf16();
            }
        }
        Err(error(JavaFactsErrorKind::UnterminatedLiteral, start))
    }

    fn escape(&mut self, text_block: bool) -> FactsResult<()> {
        let start = self.offset;
        self.offset += 1;
        let byte = self
            .source
            .as_bytes()
            .get(self.offset)
            .copied()
            .ok_or_else(|| error(JavaFactsErrorKind::UnterminatedLiteral, start))?;
        self.offset += 1;
        match byte {
            b'b' | b't' | b'n' | b'f' | b'r' | b's' | b'"' | b'\'' | b'\\' => Ok(()),
            b'0'..=b'7' => {
                let extra_digits = if byte <= b'3' { 2 } else { 1 };
                for _ in 0..extra_digits {
                    if self
                        .source
                        .as_bytes()
                        .get(self.offset)
                        .is_some_and(|byte| matches!(byte, b'0'..=b'7'))
                    {
                        self.offset += 1;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            b'\n' if text_block => Ok(()),
            b'\r' if text_block => {
                if self.source.as_bytes().get(self.offset) == Some(&b'\n') {
                    self.offset += 1;
                }
                Ok(())
            }
            _ => Err(error(JavaFactsErrorKind::MalformedLiteral, start)),
        }
    }

    fn text_block(&mut self) -> FactsResult<()> {
        let start = self.offset;
        self.offset += 3;
        while self
            .source
            .as_bytes()
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | 0x0c))
        {
            self.offset += 1;
        }
        if !self
            .source
            .as_bytes()
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
        {
            return Err(error(JavaFactsErrorKind::InvalidTextBlockOpening, start));
        }
        while let Some(byte) = self.source.as_bytes().get(self.offset).copied() {
            if self.source.as_bytes()[self.offset..].starts_with(b"\"\"\"") {
                self.offset += 3;
                return Ok(());
            }
            if byte == b'\\' {
                self.escape(true)?;
            } else {
                self.offset += 1;
            }
        }
        Err(error(JavaFactsErrorKind::UnterminatedLiteral, start))
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    lookahead: Option<Token<'a>>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            lexer: Lexer { source, offset: 0 },
            lookahead: None,
        }
    }

    fn next(&mut self) -> FactsResult<Option<Token<'a>>> {
        match self.lookahead.take() {
            Some(token) => Ok(Some(token)),
            None => self.lexer.next(),
        }
    }

    fn required(&mut self, start: usize) -> FactsResult<Token<'a>> {
        self.next()?
            .ok_or_else(|| error(JavaFactsErrorKind::MalformedDeclaration, start))
    }

    fn parse(mut self) -> FactsResult<JavaSourceFacts> {
        let mut facts = JavaSourceFacts {
            package: None,
            declarations: Vec::new(),
        };
        let mut imports_started = false;
        while let Some(mut token) = self.next()? {
            if token.text == ";" {
                continue;
            }
            let declaration_start = token.byte_offset;
            let mut annotation_type = false;
            while token.text == "@" {
                let next = self.required(declaration_start)?;
                if next.text == "interface" {
                    annotation_type = true;
                    token = next;
                    break;
                }
                self.annotation(next)?;
                token = self.required(declaration_start)?;
            }
            if token.text == "package" {
                if facts.package.is_some() || imports_started || !facts.declarations.is_empty() {
                    return Err(error(
                        JavaFactsErrorKind::MalformedDeclaration,
                        token.byte_offset,
                    ));
                }
                facts.package = Some(self.qualified_name(false, token.byte_offset)?);
                continue;
            }
            if token.text == "import" && declaration_start == token.byte_offset {
                if !facts.declarations.is_empty() {
                    return Err(error(
                        JavaFactsErrorKind::MalformedDeclaration,
                        token.byte_offset,
                    ));
                }
                imports_started = true;
                let next = self.required(token.byte_offset)?;
                if next.text != "static" {
                    self.lookahead = Some(next);
                }
                self.qualified_name(true, token.byte_offset)?;
                continue;
            }
            loop {
                if matches!(
                    token.text,
                    "public" | "abstract" | "final" | "strictfp" | "sealed"
                ) {
                    token = self.required(declaration_start)?;
                } else if token.text == "non" {
                    let dash = self.required(declaration_start)?;
                    let sealed = self.required(declaration_start)?;
                    if dash.text != "-" || sealed.text != "sealed" {
                        return Err(error(
                            JavaFactsErrorKind::UnsupportedTopLevelSyntax,
                            token.byte_offset,
                        ));
                    }
                    token = self.required(declaration_start)?;
                } else if token.text == "@" {
                    let next = self.required(declaration_start)?;
                    if next.text == "interface" {
                        annotation_type = true;
                        token = next;
                        break;
                    }
                    self.annotation(next)?;
                    token = self.required(declaration_start)?;
                } else {
                    break;
                }
            }
            let kind = match token.text {
                "class" => JavaTypeKind::Class,
                "interface" if annotation_type => JavaTypeKind::Annotation,
                "interface" => JavaTypeKind::Interface,
                "enum" => JavaTypeKind::Enum,
                "record" => JavaTypeKind::Record,
                _ => {
                    return Err(error(
                        JavaFactsErrorKind::UnsupportedTopLevelSyntax,
                        token.byte_offset,
                    ));
                }
            };
            let name = self.required(declaration_start)?;
            if !name.is_identifier()
                || matches!(name.text, "var" | "yield" | "record" | "sealed" | "permits")
            {
                return Err(error(
                    JavaFactsErrorKind::MalformedDeclaration,
                    name.byte_offset,
                ));
            }
            self.type_body(kind, declaration_start)?;
            if facts.declarations.len() == MAX_JAVA_TYPE_DECLARATIONS {
                return Err(error(
                    JavaFactsErrorKind::TooManyDeclarations,
                    declaration_start,
                ));
            }
            facts.declarations.push(JavaTypeDeclaration {
                name: name.text.into(),
                kind,
                name_byte_offset: name.byte_offset,
                declaration_byte_offset: declaration_start,
            });
        }
        Ok(facts)
    }

    fn qualified_name(&mut self, wildcard: bool, start: usize) -> FactsResult<String> {
        let mut name = String::new();
        loop {
            let token = self.required(start)?;
            if !token.is_identifier() {
                return Err(error(
                    JavaFactsErrorKind::MalformedDeclaration,
                    token.byte_offset,
                ));
            }
            name.push_str(token.text);
            let separator = self.required(start)?;
            match separator.text {
                ";" => return Ok(name),
                "." => {
                    name.push('.');
                    let next = self.required(start)?;
                    if next.text == "*" && wildcard {
                        let end = self.required(start)?;
                        if end.text == ";" {
                            return Ok(name);
                        }
                        return Err(error(
                            JavaFactsErrorKind::MalformedDeclaration,
                            end.byte_offset,
                        ));
                    }
                    self.lookahead = Some(next);
                }
                _ => {
                    return Err(error(
                        JavaFactsErrorKind::MalformedDeclaration,
                        separator.byte_offset,
                    ));
                }
            }
        }
    }

    fn annotation(&mut self, mut name: Token<'a>) -> FactsResult<()> {
        loop {
            if !name.is_identifier() {
                return Err(error(
                    JavaFactsErrorKind::MalformedDeclaration,
                    name.byte_offset,
                ));
            }
            let Some(next) = self.next()? else {
                return Ok(());
            };
            match next.text {
                "." => name = self.required(name.byte_offset)?,
                "(" => return self.balanced(next),
                _ => {
                    self.lookahead = Some(next);
                    return Ok(());
                }
            }
        }
    }

    fn type_body(&mut self, kind: JavaTypeKind, start: usize) -> FactsResult<()> {
        let mut token = self.required(start)?;
        if token.text == "<"
            && matches!(
                kind,
                JavaTypeKind::Class | JavaTypeKind::Interface | JavaTypeKind::Record
            )
        {
            self.angles(token)?;
            token = self.required(start)?;
        }
        if kind == JavaTypeKind::Record {
            if token.text != "(" {
                return Err(error(
                    JavaFactsErrorKind::MalformedDeclaration,
                    token.byte_offset,
                ));
            }
            self.balanced(token)?;
            token = self.required(start)?;
        }
        let mut clauses = Vec::new();
        loop {
            if token.text == "{" {
                return self.balanced(token);
            }
            let allowed = match token.text {
                "extends" => {
                    matches!(kind, JavaTypeKind::Class | JavaTypeKind::Interface)
                        && clauses.is_empty()
                }
                "implements" => {
                    matches!(
                        kind,
                        JavaTypeKind::Class | JavaTypeKind::Enum | JavaTypeKind::Record
                    ) && !clauses.contains(&"implements")
                        && !clauses.contains(&"permits")
                }
                "permits" => {
                    matches!(kind, JavaTypeKind::Class | JavaTypeKind::Interface)
                        && !clauses.contains(&"permits")
                }
                _ => false,
            };
            if !allowed {
                return Err(error(
                    JavaFactsErrorKind::MalformedDeclaration,
                    token.byte_offset,
                ));
            }
            clauses.push(token.text);
            let multiple = token.text != "extends" || kind == JavaTypeKind::Interface;
            loop {
                self.reference_type(start)?;
                token = self.required(start)?;
                if token.text != "," {
                    break;
                }
                if !multiple {
                    return Err(error(
                        JavaFactsErrorKind::MalformedDeclaration,
                        token.byte_offset,
                    ));
                }
            }
        }
    }

    fn reference_type(&mut self, start: usize) -> FactsResult<()> {
        let mut token = self.required(start)?;
        loop {
            while token.text == "@" {
                let annotation = self.required(start)?;
                self.annotation(annotation)?;
                token = self.required(start)?;
            }
            if !token.is_identifier() {
                return Err(error(
                    JavaFactsErrorKind::MalformedDeclaration,
                    token.byte_offset,
                ));
            }
            let mut next = self.required(start)?;
            if next.text == "<" {
                self.angles(next)?;
                next = self.required(start)?;
            }
            if next.text != "." {
                self.lookahead = Some(next);
                return Ok(());
            }
            token = self.required(start)?;
        }
    }

    fn angles(&mut self, opening: Token<'a>) -> FactsResult<()> {
        let mut depth = 1;
        let mut any_content = false;
        while let Some(token) = self.next()? {
            match token.text {
                "<" => {
                    depth += 1;
                    if depth > MAX_DELIMITER_DEPTH {
                        return Err(error(JavaFactsErrorKind::NestingTooDeep, token.byte_offset));
                    }
                }
                ">" => {
                    depth -= 1;
                    if depth == 0 {
                        if !any_content {
                            return Err(error(
                                JavaFactsErrorKind::MalformedDeclaration,
                                token.byte_offset,
                            ));
                        }
                        return Ok(());
                    }
                }
                "@" => {
                    let annotation = self.required(opening.byte_offset)?;
                    self.annotation(annotation)?;
                }
                "?" => any_content = true,
                "," | "." | "&" | "extends" | "super" => {}
                "[" => {
                    let closing = self.required(opening.byte_offset)?;
                    if closing.text != "]" {
                        return Err(error(
                            JavaFactsErrorKind::MalformedDeclaration,
                            closing.byte_offset,
                        ));
                    }
                }
                _ if token.is_identifier() => any_content = true,
                _ => {
                    return Err(error(
                        JavaFactsErrorKind::MalformedDeclaration,
                        token.byte_offset,
                    ));
                }
            }
        }
        Err(error(
            JavaFactsErrorKind::UnbalancedDelimiter,
            opening.byte_offset,
        ))
    }

    fn balanced(&mut self, opening: Token<'a>) -> FactsResult<()> {
        let mut delimiters = vec![opening];
        while let Some(token) = self.next()? {
            if token.literal {
                continue;
            }
            match token.text {
                "{" | "(" | "[" => {
                    if delimiters.len() == MAX_DELIMITER_DEPTH {
                        return Err(error(JavaFactsErrorKind::NestingTooDeep, token.byte_offset));
                    }
                    delimiters.push(token);
                }
                "}" | ")" | "]" => {
                    let Some(previous) = delimiters.pop() else {
                        return Err(error(
                            JavaFactsErrorKind::UnbalancedDelimiter,
                            token.byte_offset,
                        ));
                    };
                    if !matches!(
                        (previous.text, token.text),
                        ("{", "}") | ("(", ")") | ("[", "]")
                    ) {
                        return Err(error(
                            JavaFactsErrorKind::UnbalancedDelimiter,
                            token.byte_offset,
                        ));
                    }
                    if delimiters.is_empty() {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err(error(
            JavaFactsErrorKind::UnbalancedDelimiter,
            opening.byte_offset,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context as _, Result};

    fn names(facts: &JavaSourceFacts) -> Vec<&str> {
        facts
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect()
    }

    #[test]
    fn extracts_the_exact_original_generated_build_config_input() -> Result<()> {
        let source = include_bytes!("../test_data/java_class_facts/BuildConfig.java");
        assert_eq!(
            source,
            b"package com.application; public final class BuildConfig {}"
        );
        let reference = include_str!(
            "../test_data/project_tree/reference/android/navigator/testSrc/com/android/tools/idea/navigator/AndroidProjectViewTest.java"
        );
        assert!(
            reference.contains("\"package com.application; public final class BuildConfig {}\"")
        );
        let facts = parse_java_class_facts(source)?;
        assert_eq!(facts.package.as_deref(), Some("com.application"));
        assert_eq!(
            facts.declarations,
            vec![JavaTypeDeclaration {
                name: "BuildConfig".into(),
                kind: JavaTypeKind::Class,
                name_byte_offset: 44,
                declaration_byte_offset: 25,
            }]
        );
        assert_eq!(
            facts.into_class_facts(),
            vec![JavaClassFact {
                name: "BuildConfig".into(),
                byte_offset: Some(44)
            }]
        );
        Ok(())
    }

    #[test]
    fn reads_the_unchanged_reference_activity_instead_of_a_filename() -> Result<()> {
        let source = include_bytes!(
            "../test_data/project_tree/simpleApplication/app/src/main/java/google/simpleapplication/MyActivity.java"
        );
        let facts = parse_java_class_facts(source)?;
        assert_eq!(facts.package.as_deref(), Some("google.simpleapplication"));
        assert_eq!(names(&facts), ["MyActivity"]);
        let declaration = facts.declarations.first().context("Activity declaration")?;
        assert_eq!(
            source.get(
                declaration.name_byte_offset..declaration.name_byte_offset + declaration.name.len()
            ),
            Some(b"MyActivity".as_slice())
        );
        Ok(())
    }

    #[test]
    fn preserves_multiple_top_level_type_kinds_and_source_order() -> Result<()> {
        let source = b"class First {} @Deprecated interface Second {} enum Third { ONE } record Fourth(int value) {} public @interface Fifth {}";
        let facts = parse_java_class_facts(source)?;
        assert_eq!(
            names(&facts),
            ["First", "Second", "Third", "Fourth", "Fifth"]
        );
        assert_eq!(
            facts
                .declarations
                .iter()
                .map(|declaration| declaration.kind)
                .collect::<Vec<_>>(),
            [
                JavaTypeKind::Class,
                JavaTypeKind::Interface,
                JavaTypeKind::Enum,
                JavaTypeKind::Record,
                JavaTypeKind::Annotation
            ]
        );
        for declaration in &facts.declarations {
            assert_eq!(
                source.get(
                    declaration.name_byte_offset
                        ..declaration.name_byte_offset + declaration.name.len()
                ),
                Some(declaration.name.as_bytes())
            );
        }
        Ok(())
    }

    #[test]
    fn nested_local_and_anonymous_types_do_not_become_top_level_facts() -> Result<()> {
        let source = br#"class Outer {
            interface Nested {}
            void work() { class Local {} new Object() { class AnonymousMember {} }; }
            Class<?> literal = Hidden.class;
        } class After {}"#;
        assert_eq!(names(&parse_java_class_facts(source)?), ["Outer", "After"]);
        Ok(())
    }

    #[test]
    fn annotations_with_class_literals_arrays_and_nested_annotations_are_not_declarations()
    -> Result<()> {
        let source = br#"@example.Annotation(types = {Fake.class, Other.class}, nested = @Inner("class Nope {}"))
            public final @TypeAnnotation class Actual {}"#;
        let facts = parse_java_class_facts(source)?;
        assert_eq!(names(&facts), ["Actual"]);
        assert_eq!(
            facts
                .declarations
                .first()
                .context("Actual")?
                .declaration_byte_offset,
            0
        );
        Ok(())
    }

    #[test]
    fn comments_and_escaped_literals_cannot_inject_class_facts() -> Result<()> {
        let source = br#"// class LineComment {}
            /* interface BlockComment {} */
            class Actual { String text = "\"class StringFake {}\""; char quote = '\''; char octal = '\141'; }
            // record Trailing(int ignored) {}"#;
        assert_eq!(names(&parse_java_class_facts(source)?), ["Actual"]);
        Ok(())
    }

    #[test]
    fn unicode_comments_and_literals_keep_actual_utf8_navigation_offsets() -> Result<()> {
        let source = "/* 日本語 🦀 */\n@Annotation(\"é😀\") class Actual { String text = \"שלום\"; char letter = 'é'; }";
        let facts = parse_java_class_facts(source.as_bytes())?;
        let declaration = facts.declarations.first().context("Actual")?;
        assert_eq!(declaration.name_byte_offset, 49);
        assert_eq!(
            source.get(declaration.name_byte_offset..declaration.name_byte_offset + 6),
            Some("Actual")
        );
        assert_ne!(
            source[..declaration.name_byte_offset].chars().count(),
            declaration.name_byte_offset
        );
        Ok(())
    }

    #[test]
    fn line_comments_end_at_cr_lf_or_crlf() -> Result<()> {
        for newline in ["\r", "\n", "\r\n"] {
            let source = format!("// class Fake {{}}{newline}class Actual {{}}");
            assert_eq!(
                names(&parse_java_class_facts(source.as_bytes())?),
                ["Actual"]
            );
        }
        Ok(())
    }

    #[test]
    fn text_blocks_hide_type_keywords_and_delimiters_after_unicode_content() -> Result<()> {
        let source = r####"class Actual { String text = """
            日本語 😀 class Fake { ( [ ] ) }
            escaped \""" is not the closing delimiter
            continuation \
            trailing\s
            """; } interface After {}"####;
        assert_eq!(
            names(&parse_java_class_facts(source.as_bytes())?),
            ["Actual", "After"]
        );
        Ok(())
    }

    #[test]
    fn annotated_package_and_static_or_wildcard_imports_preserve_package_facts() -> Result<()> {
        let source = b"@example.PackageAnnotation package dev.koda; import java.util.*; import static a.B.member; import static a.B.*; class Actual {}";
        let facts = parse_java_class_facts(source)?;
        assert_eq!(facts.package.as_deref(), Some("dev.koda"));
        assert_eq!(names(&facts), ["Actual"]);
        let package_only = parse_java_class_facts(b"@Deprecated package dev.packageinfo;")?;
        assert_eq!(package_only.package.as_deref(), Some("dev.packageinfo"));
        assert!(package_only.declarations.is_empty());
        Ok(())
    }

    #[test]
    fn modifiers_generic_bounds_and_sealed_hierarchies_keep_declaration_names() -> Result<()> {
        let source = b"public abstract sealed class Parent<T extends Number & Comparable<T>> implements Example<T> permits Child {} final class Child extends Parent<Integer> {} non-sealed interface Contract<T> extends Other<T>, Another {}";
        assert_eq!(
            names(&parse_java_class_facts(source)?),
            ["Parent", "Child", "Contract"]
        );
        Ok(())
    }

    #[test]
    fn generic_supertypes_and_records_accept_type_annotations_and_wildcards() -> Result<()> {
        let source = b"class Actual extends @Annotation(values={1,2}) Outer<String>.@InnerAnnotation Inner<java.util.List<? extends Number[]>> {} record Pair<T>(@Annotation T left, T right) implements Comparable<Pair<T>> {}";
        assert_eq!(names(&parse_java_class_facts(source)?), ["Actual", "Pair"]);
        Ok(())
    }

    #[test]
    fn empty_sources_and_empty_declarations_have_no_invented_classes() -> Result<()> {
        for source in [
            b"".as_slice(),
            b"// no declarations",
            b"; /* none */ ;",
            b"package dev.koda; import a.B;",
        ] {
            assert!(discover_top_level_java_classes(source)?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn dollar_identifiers_are_actual_declarations() -> Result<()> {
        assert_eq!(
            names(&parse_java_class_facts(
                b"class $Generated_2 {} interface A$B {}"
            )?),
            ["$Generated_2", "A$B"]
        );
        Ok(())
    }

    #[test]
    fn malformed_declaration_headers_return_no_partial_facts() {
        for source in [
            "class",
            "class {}",
            "class 123 {}",
            "class class {}",
            "class _ {}",
            "class record {}",
            "class Good {} class Missing;",
            "class A unknown {}",
            "class A class B {}",
            "record Missing {}",
            "class A extends {}",
            "class A extends B, C {}",
            "enum A extends B {}",
            "@interface A implements B {}",
            "class A<> {}",
            "class A<T {}",
            "package a..b;",
            "package a; package b;",
            "class A {} import a.B;",
            "import a.*.B;",
            "@import class A {}",
        ] {
            assert!(
                matches!(
                    parse_java_class_facts(source.as_bytes()),
                    Err(JavaFactsError {
                        kind: JavaFactsErrorKind::MalformedDeclaration
                            | JavaFactsErrorKind::UnbalancedDelimiter,
                        ..
                    })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn unsupported_top_level_shapes_are_explicit_not_empty_successes() {
        for source in [
            "module example {}",
            "open module example {}",
            "void method() {}",
            "String value;",
            "non sealed class A {}",
        ] {
            assert!(
                matches!(
                    parse_java_class_facts(source.as_bytes()),
                    Err(JavaFactsError {
                        kind: JavaFactsErrorKind::UnsupportedTopLevelSyntax,
                        ..
                    })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn unbalanced_nested_delimiters_return_the_actual_failure_offset() {
        for (source, offset) in [
            ("class A { ( ] }", 12),
            ("class A {", 8),
            ("class A {} }", 11),
            ("@Annotation(1] class A {}", 13),
        ] {
            let failure = parse_java_class_facts(source.as_bytes()).expect_err(source);
            assert!(matches!(
                failure.kind,
                JavaFactsErrorKind::UnbalancedDelimiter
                    | JavaFactsErrorKind::UnsupportedTopLevelSyntax
            ));
            assert_eq!(failure.byte_offset, offset, "{source}");
        }
    }

    #[test]
    fn unterminated_comments_and_literals_are_explicit() {
        for (source, kind) in [
            ("class A {} /*", JavaFactsErrorKind::UnterminatedComment),
            ("/*/", JavaFactsErrorKind::UnterminatedComment),
            (
                "class A { String text = \"missing; }",
                JavaFactsErrorKind::UnterminatedLiteral,
            ),
            (
                "class A { char letter = 'x; }",
                JavaFactsErrorKind::UnterminatedLiteral,
            ),
            (
                "class A { String text = \"line\nnext\"; }",
                JavaFactsErrorKind::UnterminatedLiteral,
            ),
            (
                "class A { String text = \"\"\"\nnever closes",
                JavaFactsErrorKind::UnterminatedLiteral,
            ),
        ] {
            assert_eq!(
                parse_java_class_facts(source.as_bytes())
                    .expect_err(source)
                    .kind,
                kind
            );
        }
    }

    #[test]
    fn malformed_escapes_character_literals_and_text_block_openings_are_rejected() {
        for source in [
            r"class A { char value = ''; }",
            r"class A { char value = 'ab'; }",
            "class A { char value = '😀'; }",
            r"class A { char value = '\400'; }",
            r#"class A { String value = "\q"; }"#,
            r#"class A { String value = """inline"""; }"#,
        ] {
            assert!(
                matches!(
                    parse_java_class_facts(source.as_bytes()),
                    Err(JavaFactsError {
                        kind: JavaFactsErrorKind::MalformedLiteral
                            | JavaFactsErrorKind::InvalidTextBlockOpening,
                        ..
                    })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn unicode_escape_preprocessing_is_explicit_even_inside_comments_or_literals() {
        for source in [
            r"class \u0041 {}",
            r"// \u000a class Hidden {}",
            r#"class A { String text = "\u0061"; }"#,
            r"/* \uuuu0041 */ class A {}",
        ] {
            let failure = parse_java_class_facts(source.as_bytes()).expect_err(source);
            assert_eq!(failure.kind, JavaFactsErrorKind::UnicodeEscape);
            assert_eq!(
                source.get(failure.byte_offset..failure.byte_offset + 2),
                Some("\\u")
            );
        }
    }

    #[test]
    fn non_ascii_identifiers_do_not_get_truncated_to_ascii_names() {
        for source in [
            "class Café {}",
            "package café; class A {}",
            "class Δ {}",
            "class A { int café; }",
            "class A\u{301} {}",
        ] {
            assert_eq!(
                parse_java_class_facts(source.as_bytes())
                    .expect_err(source)
                    .kind,
                JavaFactsErrorKind::NonAsciiIdentifier
            );
        }
    }

    #[test]
    fn invalid_utf8_reports_the_first_invalid_byte() {
        let failure = parse_java_class_facts(b"class A {}\xff").expect_err("Invalid UTF-8");
        assert_eq!(failure, error(JavaFactsErrorKind::InvalidUtf8, 10));
    }

    #[test]
    fn invalid_code_characters_inside_balanced_bodies_do_not_produce_facts() {
        for source in [
            b"class A { # }".as_slice(),
            b"class A { ` }",
            b"class A { \\ }",
            b"class A { \x0b }",
            b"class A { \0 }",
        ] {
            assert_eq!(
                parse_java_class_facts(source)
                    .expect_err("Invalid code character")
                    .kind,
                JavaFactsErrorKind::MalformedLexeme
            );
        }
    }

    #[test]
    fn source_size_limit_is_checked_before_utf8_or_tokenization() -> Result<()> {
        let suffix = "*/ class Actual {}";
        let mut source = String::from("/*");
        source.extend(std::iter::repeat_n(
            'x',
            MAX_JAVA_FACT_BYTES - 2 - suffix.len(),
        ));
        source.push_str(suffix);
        assert_eq!(
            names(&parse_java_class_facts(source.as_bytes())?),
            ["Actual"]
        );
        let mut too_large = source.into_bytes();
        too_large.push(0xff);
        assert_eq!(
            parse_java_class_facts(&too_large)
                .expect_err("Oversized source")
                .kind,
            JavaFactsErrorKind::InputTooLarge
        );
        Ok(())
    }

    #[test]
    fn delimiter_nesting_is_bounded_without_recursion() -> Result<()> {
        let source = format!(
            "class A {{ {}{} }}",
            "(".repeat(MAX_DELIMITER_DEPTH - 1),
            ")".repeat(MAX_DELIMITER_DEPTH - 1)
        );
        assert_eq!(names(&parse_java_class_facts(source.as_bytes())?), ["A"]);
        let too_deep = format!(
            "class A {{ {}{} }}",
            "(".repeat(MAX_DELIMITER_DEPTH),
            ")".repeat(MAX_DELIMITER_DEPTH)
        );
        assert_eq!(
            parse_java_class_facts(too_deep.as_bytes())
                .expect_err("Deep source")
                .kind,
            JavaFactsErrorKind::NestingTooDeep
        );
        Ok(())
    }

    #[test]
    fn large_multiple_declaration_inputs_keep_the_last_actual_navigation_target() -> Result<()> {
        let mut source = String::from("package generated;\n");
        for index in 0..10_000 {
            source.push_str(&format!("class Type{index} {{}}\n"));
        }
        let facts = parse_java_class_facts(source.as_bytes())?;
        assert_eq!(facts.declarations.len(), 10_000);
        assert_eq!(facts.declarations.first().context("First")?.name, "Type0");
        let last = facts.declarations.last().context("Last")?;
        assert_eq!(last.name, "Type9999");
        assert_eq!(
            source.get(last.name_byte_offset..last.name_byte_offset + last.name.len()),
            Some("Type9999")
        );
        Ok(())
    }

    #[test]
    fn declaration_count_limit_never_exposes_partial_results() -> Result<()> {
        let source = "class A {} ".repeat(MAX_JAVA_TYPE_DECLARATIONS);
        assert_eq!(
            parse_java_class_facts(source.as_bytes())?
                .declarations
                .len(),
            MAX_JAVA_TYPE_DECLARATIONS
        );
        let too_many = format!("{source}class Extra {{}}");
        assert_eq!(
            parse_java_class_facts(too_many.as_bytes()).expect_err("Too many declarations"),
            error(JavaFactsErrorKind::TooManyDeclarations, source.len())
        );
        Ok(())
    }
}
