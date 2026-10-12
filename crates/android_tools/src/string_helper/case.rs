//! Java-compatible casing for the first UTF-16 unit/code point.
// Copyright (C) 2013 The Android Open Source Project
// Licensed under the Apache License, Version 2.0.
// Adapted from platform/tools/base@4a5d2ec9571e2021fc9a4621e24a195f966ee6dd.

#[path = "case_data.rs"]
mod data;

/// Explicit Unicode data selected from an observed JDK version.
///
/// These are the Character mappings used by JDK 17–25. There is deliberately
/// no ambient Rust/JVM default: the pinned IDE's bundled JBR is not yet bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseMapping {
    Unicode13,
    Unicode14,
    Unicode15,
    Unicode15_1,
    Unicode16,
}

impl CaseMapping {
    pub const fn for_jdk_major(major: u32) -> Option<Self> {
        match major {
            17 | 18 => Some(Self::Unicode13),
            19 => Some(Self::Unicode14),
            20 | 21 => Some(Self::Unicode15),
            22 | 23 => Some(Self::Unicode15_1),
            24 | 25 => Some(Self::Unicode16),
            _ => None,
        }
    }

    const fn mask(self) -> u8 {
        1 << self as u8
    }

    fn mapped(self, input: u32, mappings: &[(u32, u32, u8)]) -> u32 {
        let first = mappings.partition_point(|entry| entry.0 < input);
        for &(source, target, versions) in &mappings[first..] {
            if source != input {
                break;
            }
            if versions & self.mask() != 0 {
                return target;
            }
        }
        input
    }
}

/// Append a word with Java's simple BMP uppercase or leading supplementary
/// code-point uppercase. Isolated surrogates and the suffix remain unchanged.
pub fn append_capitalized_utf16(destination: &mut Vec<u16>, word: &[u16], cases: CaseMapping) {
    let Some(&first) = word.first() else { return };
    let mut consumed = 1;
    let mut code_point = u32::from(first);
    if (0xd800..=0xdbff).contains(&first)
        && let Some(&second) = word.get(1).filter(|unit| (0xdc00..=0xdfff).contains(*unit))
    {
        consumed = 2;
        code_point = 0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00;
    }
    // The attributed Unicode13–16 SpecialCasing data has no supplementary
    // full-uppercase overrides; its supplementary full/simple maps coincide.
    let upper = cases.mapped(code_point, data::UPPERCASE);
    append_code_point(destination, upper);
    destination.extend_from_slice(&word[consumed..]);
}

fn append_code_point(destination: &mut Vec<u16>, code_point: u32) {
    if code_point <= 0xffff {
        destination.push(code_point as u16);
    } else {
        let supplementary = code_point - 0x10000;
        destination.push((0xd800 + (supplementary >> 10)) as u16);
        destination.push((0xdc00 + (supplementary & 0x3ff)) as u16);
    }
}

pub fn us_locale_capitalize_utf16(word: &[u16], cases: CaseMapping) -> Vec<u16> {
    let mut output = Vec::new();
    append_capitalized_utf16(&mut output, word, cases);
    output
}

/// ASCII task names have identical mappings in every supported JDK table.
/// Non-ASCII input needs an explicitly captured case-data version instead.
pub fn capitalize_ascii(word: &str) -> Option<String> {
    if !word.is_ascii() {
        return None;
    }
    let mut output = word.to_owned();
    if let Some(first) = output.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Some(output)
}

/// Lowercase exactly the first UTF-16 unit, matching substring(0, 1) in US locale.
pub fn us_locale_decapitalize_utf16(word: &[u16], cases: CaseMapping) -> Vec<u16> {
    let Some(&first) = word.first() else { return Vec::new() };
    let mut output = Vec::new();
    // This is the only unconditional full-lowercase expansion in Unicode13–16.
    // Conditional final-sigma never applies to this one-unit substring.
    if first == 0x130 {
        output.extend_from_slice(&[u16::from(b'i'), 0x307]);
    } else {
        append_code_point(&mut output, cases.mapped(u32::from(first), data::LOWERCASE));
    }
    output.extend_from_slice(&word[1..]);
    output
}

pub fn append_capitalized_words_utf16<'a>(
    prefix: &[u16],
    words: impl IntoIterator<Item = &'a [u16]>,
    cases: CaseMapping,
) -> Vec<u16> {
    let mut output = prefix.to_vec();
    for word in words {
        append_capitalized_utf16(&mut output, word, cases);
    }
    output
}

pub fn capitalize_and_append_utf16(word: &[u16], suffix: &[u16], cases: CaseMapping) -> Vec<u16> {
    let mut output = us_locale_capitalize_utf16(word, cases);
    output.extend_from_slice(suffix);
    output
}

pub fn append_camel_case_utf16(destination: &mut Vec<u16>, word: &[u16], cases: CaseMapping) {
    if destination.is_empty() {
        destination.extend_from_slice(word);
    } else {
        append_capitalized_utf16(destination, word, cases);
    }
}

pub fn combine_as_camel_case_utf16<'a>(
    words: impl IntoIterator<Item = &'a [u16]>,
    cases: CaseMapping,
) -> Vec<u16> {
    let mut output = Vec::new();
    for (index, word) in words.into_iter().enumerate() {
        if index == 0 {
            output.extend_from_slice(word);
        } else {
            append_capitalized_utf16(&mut output, word, cases);
        }
    }
    output
}

pub fn combine_as_camel_case_mapped_into_utf16<T>(
    destination: &mut Vec<u16>,
    items: impl IntoIterator<Item = T>,
    mut map: impl FnMut(T) -> Vec<u16>,
    cases: CaseMapping,
) {
    for (index, item) in items.into_iter().enumerate() {
        let word = map(item);
        if index == 0 {
            destination.extend_from_slice(&word);
        } else {
            append_capitalized_utf16(destination, &word, cases);
        }
    }
}

pub fn combine_as_camel_case_mapped_utf16<T>(
    items: impl IntoIterator<Item = T>,
    map: impl FnMut(T) -> Vec<u16>,
    cases: CaseMapping,
) -> Vec<u16> {
    let mut output = Vec::new();
    combine_as_camel_case_mapped_into_utf16(&mut output, items, map, cases);
    output
}
