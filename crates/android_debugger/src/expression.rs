//! Side-effect-free debugger expressions.
//!
//! This parser intentionally accepts an ASCII Java/JVM identifier subset:
//! [A-Za-z_$][A-Za-z0-9_$]*. Unicode identifiers, Kotlin backtick identifiers,
//! calls, operators, casts, getters, and assignments are unsupported. Evaluation
//! must read a local/this value, then explicit JVM fields or array elements only;
//! accepting a field name never authorizes invoking a property accessor.

use anyhow::{Context, Result, bail, ensure};

pub const MAX_EXPRESSION_BYTES: usize = 4_096;
pub const MAX_EXPRESSION_ACCESSES: usize = 64;
pub const MAX_ARRAY_INDEX: u32 = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Access {
    Field(String),
    Index(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expression {
    /// "this" is the current instance. Other roots are exact local identifiers.
    pub root: String,
    pub accesses: Vec<Access>,
}

impl Expression {
    pub fn parse(text: &str) -> Result<Self> {
        checked_input(text)?;
        let mut parser = Parser {
            text: text.as_bytes(),
            cursor: 0,
        };
        parser.skip_spaces();
        let root = parser.identifier()?.to_owned();
        let mut accesses = Vec::new();
        loop {
            parser.skip_spaces();
            let Some(next) = parser.peek() else {
                return Ok(Self { root, accesses });
            };
            ensure!(
                accesses.len() < MAX_EXPRESSION_ACCESSES,
                "expression access limit exceeded"
            );
            let access = match next {
                b'.' => {
                    parser.cursor += 1;
                    parser.skip_spaces();
                    Access::Field(parser.identifier()?.to_owned())
                }
                b'[' => {
                    parser.cursor += 1;
                    parser.skip_spaces();
                    let start = parser.cursor;
                    while parser.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                        parser.cursor += 1;
                    }
                    ensure!(
                        parser.cursor > start,
                        "array index must be nonnegative decimal digits"
                    );
                    let number = std::str::from_utf8(&parser.text[start..parser.cursor])
                        .context("invalid array index encoding")?
                        .parse::<u32>()
                        .context("array index overflow")?;
                    ensure!(number <= MAX_ARRAY_INDEX, "array index limit exceeded");
                    parser.skip_spaces();
                    ensure!(
                        parser.peek() == Some(b']'),
                        "missing array index closing bracket"
                    );
                    parser.cursor += 1;
                    Access::Index(number)
                }
                _ => bail!("only field and array access are permitted"),
            };
            accesses.push(access);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    /// None requests local names; Some requests this value's direct JVM fields.
    pub receiver: Option<Expression>,
    pub prefix: String,
}

impl Completion {
    /// Parse a single-line DAP completion request. Columns are one-based UTF-16
    /// code-unit offsets. The cursor must be on a character boundary; text after
    /// the cursor is ignored, while the prefix obeys the same ASCII policy.
    pub fn parse(text: &str, column_one_based: usize) -> Result<Self> {
        ensure!(
            text.len() <= MAX_EXPRESSION_BYTES,
            "completion byte limit exceeded"
        );
        ensure!(
            !text.contains('\n') && !text.contains('\r'),
            "completion expects a single line"
        );
        ensure!(column_one_based > 0, "completion column must be one-based");
        let wanted_units = column_one_based - 1;
        let mut units = 0usize;
        let mut byte_cursor = None;
        for (byte, character) in text.char_indices() {
            if units == wanted_units {
                byte_cursor = Some(byte);
                break;
            }
            units = units
                .checked_add(character.len_utf16())
                .context("completion column overflow")?;
            ensure!(
                units <= wanted_units,
                "completion column splits a UTF-16 character"
            );
        }
        if byte_cursor.is_none() && units == wanted_units {
            byte_cursor = Some(text.len());
        }
        let byte_cursor = byte_cursor.context("completion column is outside the line")?;
        Self::parse_prefix(&text[..byte_cursor])
    }

    pub fn parse_prefix(text: &str) -> Result<Self> {
        checked_input(text)?;
        let text = text.trim_matches(|character| character == ' ' || character == '\t');
        if let Some((receiver, prefix)) = text.rsplit_once('.') {
            let prefix = prefix.trim_matches(|character| character == ' ' || character == '\t');
            ensure!(
                prefix.is_empty() || is_identifier(prefix),
                "invalid completion field prefix"
            );
            return Ok(Self {
                receiver: Some(Expression::parse(receiver)?),
                prefix: prefix.into(),
            });
        }
        ensure!(
            text.is_empty() || is_identifier(text),
            "invalid completion root prefix"
        );
        Ok(Self {
            receiver: None,
            prefix: text.into(),
        })
    }
}

pub fn parse_read_only_expression(text: &str) -> Result<Expression> {
    Expression::parse(text)
}

pub fn parse_completion_prefix(text: &str) -> Result<Completion> {
    Completion::parse_prefix(text)
}

fn checked_input(text: &str) -> Result<()> {
    ensure!(
        text.len() <= MAX_EXPRESSION_BYTES,
        "expression byte limit exceeded"
    );
    ensure!(text.is_ascii(), "Unicode identifiers are unsupported");
    ensure!(
        !text
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t'),
        "control character in expression"
    );
    Ok(())
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

fn identifier_continue(byte: u8) -> bool {
    identifier_start(byte) || byte.is_ascii_digit()
}

fn is_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(identifier_start) && bytes.all(identifier_continue)
}

struct Parser<'a> {
    text: &'a [u8],
    cursor: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.cursor).copied()
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.cursor += 1;
        }
    }

    fn identifier(&mut self) -> Result<&'a str> {
        let start = self.cursor;
        ensure!(
            self.peek().is_some_and(identifier_start),
            "expected an ASCII identifier"
        );
        self.cursor += 1;
        while self.peek().is_some_and(identifier_continue) {
            self.cursor += 1;
        }
        std::str::from_utf8(&self.text[start..self.cursor]).context("invalid identifier encoding")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_jvm_read_only_examples_are_preserved() {
        assert_eq!(
            Expression::parse("values[1]").unwrap(),
            Expression {
                root: "values".into(),
                accesses: vec![Access::Index(1)]
            }
        );
        assert_eq!(
            Expression::parse("this.field").unwrap(),
            Expression {
                root: "this".into(),
                accesses: vec![Access::Field("field".into())]
            }
        );
        assert_eq!(
            Expression::parse("field").unwrap(),
            Expression {
                root: "field".into(),
                accesses: vec![]
            }
        );
        assert_eq!(
            Expression::parse("large[2000]").unwrap(),
            Expression {
                root: "large".into(),
                accesses: vec![Access::Index(2000)]
            }
        );
    }

    #[test]
    fn calls_operators_assignment_negative_indexes_and_empty_are_rejected() {
        for text in [
            "field+1",
            "toString()",
            "values[-1]",
            "",
            " ",
            "\t",
            "field = 1",
            "field += 1",
            "field++",
            "--field",
            "field * 2",
            "field == other",
            "field?.name",
            "field!!",
            "new Object()",
            "this.getField()",
            "(field)",
            "field as String",
            "values[+1]",
            "values[1 + 1]",
            "values[0x1]",
            "values[]",
            "values[1",
            "values[1]]",
            "this.",
            "this..field",
            ".field",
            "0field",
            "field/other",
            "field // comment",
            "field\n",
            "field\0",
        ] {
            assert!(Expression::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn nested_explicit_fields_and_indexes_preserve_access_order() {
        assert_eq!(
            Expression::parse(" \tthis . values [ 1 ] . nested [ 0 ] \t").unwrap(),
            Expression {
                root: "this".into(),
                accesses: vec![
                    Access::Field("values".into()),
                    Access::Index(1),
                    Access::Field("nested".into()),
                    Access::Index(0),
                ],
            }
        );
        assert_eq!(
            Expression::parse("matrix[1][2]").unwrap().accesses,
            vec![Access::Index(1), Access::Index(2)]
        );
        assert_eq!(Expression::parse("$outer._field2").unwrap().root, "$outer");
        assert_eq!(
            Expression::parse("values[0002]").unwrap().accesses,
            vec![Access::Index(2)]
        );
    }

    #[test]
    fn indexes_have_checked_numeric_and_operational_bounds() {
        assert_eq!(
            Expression::parse(&format!("values[{MAX_ARRAY_INDEX}]"))
                .unwrap()
                .accesses,
            vec![Access::Index(MAX_ARRAY_INDEX)]
        );
        assert!(Expression::parse(&format!("values[{}]", MAX_ARRAY_INDEX + 1)).is_err());
        assert!(Expression::parse("values[4294967296]").is_err());
        assert!(Expression::parse("values[999999999999999999999999]").is_err());
    }

    #[test]
    fn expression_size_and_access_depth_are_bounded() {
        assert!(Expression::parse(&"a".repeat(MAX_EXPRESSION_BYTES)).is_ok());
        assert!(Expression::parse(&"a".repeat(MAX_EXPRESSION_BYTES + 1)).is_err());
        let at_limit = format!("root{}", ".field".repeat(MAX_EXPRESSION_ACCESSES));
        assert_eq!(
            Expression::parse(&at_limit).unwrap().accesses.len(),
            MAX_EXPRESSION_ACCESSES
        );
        assert!(Expression::parse(&format!("{at_limit}.extra")).is_err());
    }

    #[test]
    fn unicode_and_backtick_identifiers_are_explicitly_unsupported() {
        for text in [
            "café",
            "λ",
            "this.字段",
            "\u{0060}two words\u{0060}",
            "this.\u{0060}when\u{0060}",
            "a\u{00a0}.b",
        ] {
            assert!(Expression::parse(text).is_err(), "{text:?}");
            assert!(Completion::parse_prefix(text).is_err(), "{text:?}");
        }
        assert!(Expression::parse("ascii_$1").is_ok());
    }

    #[test]
    fn completion_distinguishes_local_prefix_and_explicit_field_receiver() {
        assert_eq!(
            Completion::parse_prefix("this.").unwrap(),
            Completion {
                receiver: Some(Expression {
                    root: "this".into(),
                    accesses: vec![]
                }),
                prefix: "".into()
            }
        );
        assert_eq!(
            Completion::parse_prefix("identifier.").unwrap(),
            Completion {
                receiver: Some(Expression {
                    root: "identifier".into(),
                    accesses: vec![]
                }),
                prefix: "".into()
            }
        );
        assert_eq!(
            Completion::parse_prefix("this.fi").unwrap(),
            Completion {
                receiver: Some(Expression {
                    root: "this".into(),
                    accesses: vec![]
                }),
                prefix: "fi".into()
            }
        );
        assert_eq!(
            Completion::parse_prefix("values[1].nested.fi").unwrap(),
            Completion {
                receiver: Some(Expression {
                    root: "values".into(),
                    accesses: vec![Access::Index(1), Access::Field("nested".into())]
                }),
                prefix: "fi".into(),
            }
        );
        assert_eq!(
            Completion::parse_prefix("field").unwrap(),
            Completion {
                receiver: None,
                prefix: "field".into()
            }
        );
        assert_eq!(
            Completion::parse_prefix("").unwrap(),
            Completion {
                receiver: None,
                prefix: "".into()
            }
        );
        assert_eq!(
            Completion::parse_prefix(" this . fi \t").unwrap().prefix,
            "fi"
        );
    }

    #[test]
    fn completion_rejects_executable_or_malformed_receivers_and_prefixes() {
        for text in [
            "toString().",
            "field+1.",
            "values[-1].",
            "this..",
            ".field",
            "field=",
            "values[1]",
            "this.1",
            "this.fi()",
            "this.f i",
            "this.\n",
            "this.field = other.",
            "values[1].x[",
            "values[1].field+",
        ] {
            assert!(Completion::parse_prefix(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn completion_columns_are_one_based_and_ignore_text_after_cursor() {
        assert_eq!(
            Completion::parse("this.field", 6).unwrap(),
            Completion::parse_prefix("this.").unwrap()
        );
        assert_eq!(Completion::parse("this.field", 8).unwrap().prefix, "fi");
        assert_eq!(Completion::parse("this.field", 11).unwrap().prefix, "field");
        assert_eq!(Completion::parse("field", 1).unwrap().prefix, "");
        assert_eq!(Completion::parse("", 1).unwrap().prefix, "");
        assert!(Completion::parse("field", 0).is_err());
        assert!(Completion::parse("field", 7).is_err());
        assert!(Completion::parse("this.\n", 6).is_err());
        assert!(Completion::parse("this.\u{1f600}", 7).is_err());
        assert!(Completion::parse("this.\u{1f600}", 8).is_err());
        assert_eq!(Completion::parse("this.\u{1f600}", 6).unwrap().prefix, "");
    }

    #[test]
    fn compatibility_functions_return_the_same_structural_ast() {
        assert_eq!(
            parse_read_only_expression("this.field").unwrap(),
            Expression::parse("this.field").unwrap()
        );
        assert_eq!(
            parse_completion_prefix("this.fi").unwrap(),
            Completion::parse_prefix("this.fi").unwrap()
        );
    }
}
