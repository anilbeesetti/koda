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

// test_tokenize_command_line adapts the two ordered assertions in AOSP
// StringHelperTest.testTokenizeCommandLine at tools/base revision
// 4a5d2ec9571e2021fc9a4621e24a195f966ee6dd. Other tests supplement the pinned
// TokenizedCommandLine algorithms; they are not additional upstream test ports.

use android_tools::string_helper::{
    CommandLinePlatform, TokenMode, tokenize_command_line_to_escaped,
    tokenize_command_line_to_raw, tokenize_command_line_utf16,
};

const PLATFORMS: [CommandLinePlatform; 2] = [
    CommandLinePlatform::Posix,
    CommandLinePlatform::Windows,
];

fn utf16(input: &str) -> Vec<u16> {
    input.encode_utf16().collect()
}

fn expected(tokens: &[&str]) -> Vec<Vec<u16>> {
    tokens.iter().map(|token| utf16(token)).collect()
}

fn assert_tokens(
    platform: CommandLinePlatform,
    input: &str,
    raw: &[&str],
    escaped: &[&str],
) {
    assert_eq!(
        tokenize_command_line_utf16(&utf16(input), TokenMode::Raw, platform),
        expected(raw),
        "raw {platform:?}: {input:?}"
    );
    assert_eq!(
        tokenize_command_line_utf16(&utf16(input), TokenMode::Escaped, platform),
        expected(escaped),
        "escaped {platform:?}: {input:?}"
    );
}

#[test]
fn test_tokenize_command_line() -> Result<(), std::string::FromUtf16Error> {
    for platform in PLATFORMS {
        // raw = true includes quotes; Vec equality preserves .inOrder().
        assert_eq!(
            tokenize_command_line_to_raw("foo bar \"baz qux\"", platform)?,
            vec!["foo", "bar", "\"baz qux\""]
        );
        // raw = false (escaped) removes quotes.
        assert_eq!(
            tokenize_command_line_to_escaped("foo bar \"baz qux\"", platform)?,
            vec!["foo", "bar", "baz qux"]
        );
    }
    Ok(())
}

#[test]
fn empty_quotes_and_unmatched_double_quotes() {
    for platform in PLATFORMS {
        assert_tokens(platform, "", &[], &[]);
        assert_tokens(platform, " \t\r\n", &[], &[]);
        assert_tokens(platform, "\"\"", &["\"\""], &[]);
        assert_tokens(platform, "\"", &["\""], &[]);
        assert_tokens(platform, "a\"\"b", &["a\"\"b"], &["ab"]);
        assert_tokens(
            platform,
            "\"unterminated space",
            &["\"unterminated space"],
            &["unterminated space"],
        );
        assert_tokens(platform, "x\"a b\"y", &["x\"a b\"y"], &["xa by"]);
    }
}

#[test]
fn single_quotes_follow_the_selected_platform() {
    assert_tokens(CommandLinePlatform::Posix, "''", &["''"], &[]);
    assert_tokens(CommandLinePlatform::Windows, "''", &["''"], &["''"]);
    assert_tokens(
        CommandLinePlatform::Posix,
        "'unterminated space",
        &["'unterminated space"],
        &["unterminated space"],
    );
    assert_tokens(
        CommandLinePlatform::Windows,
        "'unterminated space",
        &["'unterminated", "space"],
        &["'unterminated", "space"],
    );
    assert_tokens(
        CommandLinePlatform::Posix,
        "'a\"b' \"c'd\"",
        &["'a\"b'", "\"c'd\""],
        &["a\"b", "c'd"],
    );
}

#[test]
fn windows_backslash_runs_and_quotes() {
    let platform = CommandLinePlatform::Windows;
    assert_tokens(platform, r"one\two tail\", &[r"one\two", r"tail\"], &[r"one\two", r"tail\"]);
    assert_tokens(platform, r#"a\"b"#, &[r#"a\"b"#], &["a\"b"]);
    assert_tokens(platform, r#"a\\"b c"#, &[r#"a\\"b c"#], &[r"a\b c"]);
    assert_tokens(platform, r#"a\\\"b c"#, &[r#"a\\\"b"#, "c"], &[r#"a\"b"#, "c"]);
    assert_tokens(platform, r#"a\\\\"b c"#, &[r#"a\\\\"b c"#], &[r"a\\b c"]);
}

#[test]
fn windows_carets_and_line_endings() {
    let platform = CommandLinePlatform::Windows;
    assert_tokens(platform, "a^b c^^d e^", &["a^b", "c^^d", "e^"], &["ab", "c^d", "e^"]);
    assert_tokens(platform, "a^\r\nb c^\nd", &["a^b", "c^d"], &["ab", "cd"]);
    assert_tokens(platform, "a^ b", &["a^", "b"], &["a", "b"]);
    assert_tokens(platform, "\"a^b\r\nc\"", &["\"a^b\r\nc\""], &["a^b\r\nc"]);
    assert_tokens(platform, "^ \r\n^", &["^", "^"], &["^"]);
    assert_tokens(platform, "^^\r\nb", &["^^", "b"], &["^", "b"]);
}

#[test]
fn posix_backslashes_follow_the_reference_including_raw_whitespace() {
    let platform = CommandLinePlatform::Posix;
    assert_tokens(platform, "a\\ b", &["a\\b"], &["a b"]);
    assert_tokens(platform, "a\\\tb", &["a\\b"], &["a\tb"]);
    assert_tokens(platform, "a\\\nb", &["a\\b"], &["ab"]);
    assert_tokens(platform, "\"a\\\nb\"", &["\"a\\\nb\""], &["ab"]);
    assert_tokens(platform, "\"a\\qb\"", &["\"a\\qb\""], &["aqb"]);
    assert_tokens(platform, "'a\\ b'", &["'a\\ b'"], &["a\\ b"]);
    assert_tokens(platform, "\"a\\\"b\"", &["\"a\\\"b\""], &["a\"b"]);
    assert_tokens(platform, "a\\", &["a\\"], &["a"]);
    assert_tokens(platform, "\\", &["\\"], &[]);
    assert_tokens(platform, "\\\n", &["\\"], &[]);
    assert_tokens(platform, "\\\r\nb", &["\\", "b"], &["\r", "b"]);
    assert_tokens(platform, "a^b", &["a^b"], &["a^b"]);
}

#[test]
fn java_whitespace_separates_unquoted_units_and_preserves_quoted_units() {
    // Java Character's 25 BMP whitespace values, including its four extra
    // information separators. This is not Rust's Unicode White_Space property.
    let whitespace = [
        0x0009, 0x000a, 0x000b, 0x000c, 0x000d,
        0x001c, 0x001d, 0x001e, 0x001f, 0x0020,
        0x1680, 0x2000, 0x2001, 0x2002, 0x2003,
        0x2004, 0x2005, 0x2006, 0x2008, 0x2009,
        0x200a, 0x2028, 0x2029, 0x205f, 0x3000,
    ];
    for platform in PLATFORMS {
        for separator in whitespace {
            for mode in [TokenMode::Raw, TokenMode::Escaped] {
                assert_eq!(
                    tokenize_command_line_utf16(&[separator, 0x61, separator, separator, 0x62, separator], mode, platform),
                    vec![vec![0x61], vec![0x62]],
                    "{platform:?} {mode:?} separator {separator:#06x}"
                );
                let quoted = [0x22, 0x61, separator, 0x62, 0x22];
                assert_eq!(
                    tokenize_command_line_utf16(&quoted, mode, platform),
                    vec![if mode == TokenMode::Raw { quoted.to_vec() } else { vec![0x61, separator, 0x62] }],
                    "quoted {platform:?} {mode:?} separator {separator:#06x}"
                );
            }
        }
    }
}

#[test]
fn nbsp_and_other_non_java_whitespace_are_retained() {
    let retained = [0x0085, 0x00a0, 0x180e, 0x2007, 0x200b, 0x202f, 0x2060, 0xfeff, 0x0000];
    for platform in PLATFORMS {
        for unit in retained {
            for mode in [TokenMode::Raw, TokenMode::Escaped] {
                let command = [0x61, unit, 0x62];
                assert_eq!(
                    tokenize_command_line_utf16(&command, mode, platform),
                    vec![command.to_vec()],
                    "{platform:?} {mode:?} retained {unit:#06x}"
                );
            }
        }
    }
}

#[test]
fn isolated_surrogates_pairs_and_nul_preserve_code_units() {
    for platform in PLATFORMS {
        for mode in [TokenMode::Raw, TokenMode::Escaped] {
            assert_eq!(
                tokenize_command_line_utf16(&[0xd800, 0x20, 0xdc00, 0xd83d, 0xde00, 0xdfff, 0x20, 0x0000], mode, platform),
                vec![vec![0xd800], vec![0xdc00, 0xd83d, 0xde00, 0xdfff], vec![0x0000]]
            );
            let quoted = [0x22, 0xd800, 0x20, 0xdc00, 0x22];
            assert_eq!(
                tokenize_command_line_utf16(&quoted, mode, platform),
                vec![if mode == TokenMode::Raw { quoted.to_vec() } else { vec![0xd800, 0x20, 0xdc00] }]
            );
        }
    }
    assert_eq!(
        tokenize_command_line_utf16(&[0x5c, 0xd800, 0xdc00], TokenMode::Escaped, CommandLinePlatform::Posix),
        vec![vec![0xd800, 0xdc00]]
    );
    assert_eq!(
        tokenize_command_line_utf16(&[0x5c, 0xd800, 0xdc00], TokenMode::Escaped, CommandLinePlatform::Windows),
        vec![vec![0x5c, 0xd800, 0xdc00]]
    );
}

#[test]
fn string_wrappers_preserve_non_bmp_text() -> Result<(), std::string::FromUtf16Error> {
    for platform in PLATFORMS {
        assert_eq!(
            tokenize_command_line_to_raw("😀 \"𐐀 text\"", platform)?,
            vec!["😀", "\"𐐀 text\""]
        );
        assert_eq!(
            tokenize_command_line_to_escaped("😀 \"𐐀 text\"", platform)?,
            vec!["😀", "𐐀 text"]
        );
    }
    Ok(())
}

#[test]
fn output_is_a_nonexpanding_ordered_subsequence() {
    let alphabet = [0x61, 0x20, 0x09, 0x0a, 0x0d, 0x22, 0x27, 0x5c, 0x5e, 0x00, 0xd800, 0xdc00];
    for first in alphabet {
        for second in alphabet {
            for third in alphabet {
                let command = [first, second, third];
                for platform in PLATFORMS {
                    for mode in [TokenMode::Raw, TokenMode::Escaped] {
                        let tokens = tokenize_command_line_utf16(&command, mode, platform);
                        assert!(tokens.iter().all(|token| !token.is_empty()));
                        assert!(tokens.iter().map(Vec::len).sum::<usize>() <= command.len());
                        let mut position = 0;
                        for unit in tokens.iter().flatten() {
                            while position < command.len() && command[position] != *unit {
                                position += 1;
                            }
                            assert!(position < command.len(), "{command:?} {platform:?} {mode:?}");
                            position += 1;
                        }
                    }
                }
            }
        }
    }
}
