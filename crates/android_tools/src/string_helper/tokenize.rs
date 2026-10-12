/*
 * Copyright (C) 2020 The Android Open Source Project
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

//! UTF-16 adaptation of AOSP `TokenizedCommandLine.toTokenList`, Windows and
//! POSIX tokenizers at tools/base revision
//! `4a5d2ec9571e2021fc9a4621e24a195f966ee6dd`.
//!
//! These routines tokenize text; they do not evaluate or execute a command.
//! The reference drops empty tokens, tolerates unmatched quotes, and has some
//! escaping behavior that differs from a shell. Its code-unit behavior is
//! retained, including isolated UTF-16 surrogates.

/// Selects the reference tokenizer's rules independently of the current host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandLinePlatform {
    Posix,
    Windows,
}

impl CommandLinePlatform {
    /// Match the pinned wrapper's Windows-versus-other host selection.
    pub const fn native() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Posix }
    }
}

/// Controls whether the reference's quoting and escaping characters are kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenMode {
    /// Corresponds to `TokenizedCommandLine(raw = true)`.
    Raw,
    /// Corresponds to `TokenizedCommandLine(raw = false)`.
    Escaped,
}

const DOUBLE_QUOTE: u16 = 0x0022;
const SINGLE_QUOTE: u16 = 0x0027;
const BACKSLASH: u16 = 0x005c;
const CARET: u16 = 0x005e;
const CR: u16 = 0x000d;
const LF: u16 = 0x000a;

/// Tokenizes arbitrary UTF-16, preserving code units that the reference emits.
///
/// Empty tokens are omitted, including empty quotes in escaped mode. Raw mode
/// preserves emitted syntax, rather than simply splitting the original text.
/// Both platforms use Java `Character.isWhitespace(char)` semantics.
///
/// Each input unit is inspected a bounded number of times. The total number of
/// output units and the number of nonempty tokens are at most `command.len()`;
/// time and allocation are O(n). Individual code units do not need temporary
/// strings; allocation is for the returned tokens and their code-unit buffers.
pub fn tokenize_command_line_utf16(
    command: &[u16],
    mode: TokenMode,
    platform: CommandLinePlatform,
) -> Vec<Vec<u16>> {
    let raw = mode == TokenMode::Raw;
    match platform {
        CommandLinePlatform::Posix => tokenize_posix(command, raw),
        CommandLinePlatform::Windows => tokenize_windows(command, raw),
    }
}

/// String wrapper for the reference's `tokenizeCommandLineToRaw` behavior.
pub fn tokenize_command_line_to_raw(command: &str, platform: CommandLinePlatform) -> Result<Vec<String>, std::string::FromUtf16Error> {
    tokenize_string(command, TokenMode::Raw, platform)
}

/// String wrapper for the reference's `tokenizeCommandLineToEscaped` behavior.
pub fn tokenize_command_line_to_escaped(command: &str, platform: CommandLinePlatform) -> Result<Vec<String>, std::string::FromUtf16Error> {
    tokenize_string(command, TokenMode::Escaped, platform)
}

fn tokenize_string(command: &str, mode: TokenMode, platform: CommandLinePlatform) -> Result<Vec<String>, std::string::FromUtf16Error> {
    let command: Vec<u16> = command.encode_utf16().collect();
    tokenize_command_line_utf16(&command, mode, platform)
        .into_iter()
        .map(|token| {
            // Only BMP whitespace and ASCII syntax can be removed or split.
            // Adjacent surrogate pairs from a valid string stay adjacent.
            String::from_utf16(&token)
        })
        .collect()
}

// The reference calls java.lang.Character.isWhitespace(Char), not Kotlin's or
// Rust's whitespace predicate. Java includes Unicode Zs/Zl/Zp except U+00A0,
// U+2007 and U+202F, plus TAB through CR and U+001C through U+001F. This BMP table
// matches the modern JDK 17/21 contract; U+180E has been non-whitespace since
// Unicode 6.3. Surrogate code units and U+0085 are not Java whitespace.
const fn java_char_is_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x001c..=0x0020
            | 0x1680
            | 0x2000..=0x2006
            | 0x2008..=0x200a
            | 0x2028..=0x2029
            | 0x205f
            | 0x3000
    )
}

// Equivalent to toTokenList's end-of-token branch: it publishes only nonempty
// accumulated tokens. No generation/index-buffer or token-removal API is used.
fn finish_token(tokens: &mut Vec<Vec<u16>>, token: &mut Vec<u16>) {
    if !token.is_empty() {
        tokens.push(std::mem::take(token));
    }
}

fn tokenize_windows(command: &[u16], raw: bool) -> Vec<Vec<u16>> {
    let mut tokens = Vec::new();
    let mut token = Vec::new();
    let mut quoting = false;
    let mut index = 0;

    while index < command.len() && java_char_is_whitespace(command[index]) {
        index += 1;
    }

    while index < command.len() {
        match command[index] {
            DOUBLE_QUOTE => {
                if raw {
                    token.push(DOUBLE_QUOTE);
                }
                quoting = !quoting;
                index += 1;
            }
            BACKSLASH => {
                let mut forward = index + 1;
                while command.get(forward) == Some(&BACKSLASH) {
                    forward += 1;
                }
                let slash_count = forward - index;
                let odd = slash_count % 2 == 1;
                let followed_by_quote = command.get(forward) == Some(&DOUBLE_QUOTE);
                let emitted = if !raw && followed_by_quote {
                    slash_count / 2
                } else {
                    slash_count
                };
                token.extend_from_slice(&command[index..index + emitted]);
                if odd && followed_by_quote {
                    token.push(DOUBLE_QUOTE);
                    forward += 1;
                }
                index = forward;
            }
            CARET if !quoting => {
                index += 1;
                let mut next = command.get(index).copied();
                if raw || next.is_none() {
                    token.push(CARET);
                }
                if next == Some(CARET) {
                    token.push(CARET);
                    index += 1;
                }
                while matches!(next, Some(CR | LF)) {
                    index += 1;
                    next = command.get(index).copied();
                }
            }
            unit if !quoting && java_char_is_whitespace(unit) => {
                finish_token(&mut tokens, &mut token);
                index += 1;
                while index < command.len() && java_char_is_whitespace(command[index]) {
                    index += 1;
                }
            }
            unit => {
                token.push(unit);
                index += 1;
            }
        }
    }
    finish_token(&mut tokens, &mut token);
    tokens
}

fn tokenize_posix(command: &[u16], raw: bool) -> Vec<Vec<u16>> {
    let mut tokens = Vec::new();
    let mut token = Vec::new();
    let mut quoting = false;
    let mut quote = 0;
    let mut escaping = false;
    let mut skipping = true;

    for &unit in command {
        if skipping {
            if java_char_is_whitespace(unit) {
                continue;
            }
            skipping = false;
        }
        if raw && (quoting || !java_char_is_whitespace(unit)) {
            token.push(unit);
        }
        if escaping {
            escaping = false;
            if unit != LF && !raw {
                token.push(unit);
            }
            continue;
        } else if unit == BACKSLASH && (!quoting || quote == DOUBLE_QUOTE) {
            escaping = true;
            continue;
        } else if !quoting && (unit == DOUBLE_QUOTE || unit == SINGLE_QUOTE) {
            quoting = true;
            quote = unit;
            continue;
        } else if quoting && unit == quote {
            quoting = false;
            quote = 0;
            continue;
        }
        if !quoting && java_char_is_whitespace(unit) {
            skipping = true;
            finish_token(&mut tokens, &mut token);
            continue;
        }
        if !raw {
            token.push(unit);
        }
    }
    finish_token(&mut tokens, &mut token);
    tokens
}
