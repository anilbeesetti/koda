//! UTF-16 text and explicit object-display adapters for AOSP StringHelper.
// Copyright (C) 2013 The Android Open Source Project
// Licensed under the Apache License, Version 2.0.
// Adapted from platform/tools/base@4a5d2ec9571e2021fc9a4621e24a195f966ee6dd,
// common/src/main/java/com/android/utils/StringHelper.kt.

/// The system line separators supported by the pinned JVM helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemLineSeparator {
    Lf,
    CrLf,
}

impl SystemLineSeparator {
    pub const fn native() -> Self {
        if cfg!(windows) { Self::CrLf } else { Self::Lf }
    }
}

/// Remove every carriage return, then expand line feeds for the requested host.
/// Other UTF-16 units, including isolated surrogates, remain unchanged.
pub fn to_line_separator_utf16(input: &[u16], separator: SystemLineSeparator) -> Vec<u16> {
    let mut output = Vec::new();
    for &unit in input {
        if unit == u16::from(b'\r') {
            continue;
        }
        if unit == u16::from(b'\n') && separator == SystemLineSeparator::CrLf {
            output.push(u16::from(b'\r'));
        }
        output.push(unit);
    }
    output
}

pub fn to_system_line_separator_utf16(input: &[u16]) -> Vec<u16> {
    to_line_separator_utf16(input, SystemLineSeparator::native())
}

pub fn to_system_line_separator(input: &str) -> String {
    // Filtering ASCII CR and inserting ASCII CR before LF preserves valid UTF-8.
    let mut output = String::new();
    for character in input.chars().filter(|character| *character != '\r') {
        if character == '\n' && cfg!(windows) {
            output.push('\r');
        }
        output.push(character);
    }
    output
}

/// An explicitly represented JVM object display; no Java objects are executed.
///
/// A collection's non-string items deliberately use the *enclosing* collection's
/// display, matching the pinned helper. Nested collections are not flattened.
pub enum StringHelperArgument<'a> {
    String(&'a [u16]),
    Collection {
        items: &'a [StringHelperArgument<'a>],
        display: &'a dyn Fn() -> Vec<u16>,
    },
    Other(&'a dyn Fn() -> Vec<u16>),
}

pub fn to_strings_utf16(arguments: &[StringHelperArgument<'_>]) -> Vec<Vec<u16>> {
    let mut output = Vec::new();
    for argument in arguments {
        match argument {
            StringHelperArgument::String(value) => output.push(value.to_vec()),
            StringHelperArgument::Collection { items, display } => {
                for item in *items {
                    output.push(match item {
                        StringHelperArgument::String(value) => value.to_vec(),
                        _ => display(),
                    });
                }
            }
            StringHelperArgument::Other(display) => output.push(display()),
        }
    }
    output
}
