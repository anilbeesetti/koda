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

use android_tools::string_helper::{separated_list_contains, separated_list_contains_utf16};

#[test]
fn string_helper_as_separated_list_contains() {
    assert!(separated_list_contains("foo", "foo", ","));
    assert!(!separated_list_contains("foo", "bar", ","));
    assert!(!separated_list_contains("foo,bar", "barge", ","));
    assert!(!separated_list_contains("fool,bar", "foo", ","));
    assert!(separated_list_contains("fool,bar baz", "bar", ", "));
    assert!(separated_list_contains("foo,bar baz", "baz", ", "));
}

#[test]
fn string_helper_empty_elements_preserve_scanned_boundaries() {
    assert!(!separated_list_contains("", "", ","));
    assert!(!separated_list_contains("foo", "", ","));
    assert!(separated_list_contains(",foo", "", ","));
    assert!(separated_list_contains("foo,,bar", "", ","));
    assert!(!separated_list_contains("foo,", "", ","));
    assert!(separated_list_contains(",", "", ","));
}

#[test]
fn string_helper_empty_separators_require_a_whole_input_match() {
    assert!(separated_list_contains("foo", "foo", ""));
    assert!(!separated_list_contains("foo,bar", "bar", ""));
    assert!(!separated_list_contains("fool", "foo", ""));
    assert!(!separated_list_contains("", "", ""));
}

#[test]
fn string_helper_case_and_whitespace_remain_significant() {
    assert!(!separated_list_contains("FOO,bar", "foo", ","));
    assert!(!separated_list_contains("foo, bar", "bar", ","));
    assert!(separated_list_contains("foo, bar", " bar", ","));
    assert!(separated_list_contains("foo, bar", "bar", ", "));
    assert!(!separated_list_contains("xfoo,bar", "foo", ","));
}

#[test]
fn string_helper_utf16_core_preserves_surrogate_separator_units() {
    let input = [u16::from(b'x'), 0xd83d, 0xde00, u16::from(b'y')];
    assert!(separated_list_contains_utf16(
        &input,
        &[u16::from(b'y')],
        &[0xd83d, 0xde00],
    ));
    assert!(separated_list_contains_utf16(
        &input,
        &[0xde00, u16::from(b'y')],
        &[0xd83d],
    ));
    assert!(separated_list_contains_utf16(
        &input,
        &[],
        &[0xd83d, 0xde00],
    ));
    assert!(separated_list_contains("x\u{1f600}y", "y", "\u{1f600}"));
    assert!(separated_list_contains("x\u{1f600}y", "", "\u{1f600}"));
}

#[test]
fn string_helper_utf16_core_preserves_isolated_surrogates() {
    assert!(separated_list_contains_utf16(
        &[0xd801, u16::from(b',')],
        &[0xd801],
        &[u16::from(b',')],
    ));
    assert!(separated_list_contains_utf16(
        &[0xdc00],
        &[0xdc00],
        &[u16::from(b',')],
    ));
    assert!(!separated_list_contains_utf16(
        &[0xdc00],
        &[0xd801],
        &[u16::from(b',')],
    ));
}

use android_tools::string_helper::{
    CaseMapping, StringHelperArgument, SystemLineSeparator, append_capitalized_utf16,
    append_capitalized_words_utf16, append_camel_case_utf16, capitalize_and_append_utf16,
    capitalize_ascii, combine_as_camel_case_mapped_into_utf16, combine_as_camel_case_mapped_utf16,
    combine_as_camel_case_utf16, to_line_separator_utf16, to_strings_utf16,
    to_system_line_separator, us_locale_capitalize_utf16, us_locale_decapitalize_utf16,
};
use std::cell::Cell;

fn utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

const CASES: [CaseMapping; 5] = [
    CaseMapping::Unicode13, CaseMapping::Unicode14, CaseMapping::Unicode15,
    CaseMapping::Unicode15_1, CaseMapping::Unicode16,
];

// Each test below retains the original method's ordered operands and predicates.
// The original fixtures have identical mappings in all supported JDK tables.
#[test]
fn string_helper_basic_capitalize() {
    for cases in CASES {
        assert_eq!(us_locale_capitalize_utf16(&utf16("foo"), cases), utf16("Foo"));
    }
}

#[test]
fn string_helper_non_letter_capitalize() {
    for cases in CASES {
        assert_eq!(us_locale_capitalize_utf16(&utf16("1-Foo"), cases), utf16("1-Foo"));
    }
}

#[test]
fn string_helper_unicode_capitalize() {
    for cases in CASES {
        assert_eq!(us_locale_capitalize_utf16(&utf16("ê-foo"), cases), utf16("Ê-foo"));
    }
}

#[test]
fn string_helper_surrogate_values_capitalize() {
    let value = [0xd801, 0xdc00, 0x2d, 0x66, 0x6f, 0x6f];
    for cases in CASES {
        assert_eq!(us_locale_capitalize_utf16(&value, cases), value);
    }
}

#[test]
fn string_helper_append_capitalized() {
    for cases in CASES {
        assert_eq!(
            append_capitalized_words_utf16(&utf16("assemble"), [&utf16("foo")[..]], cases),
            utf16("assembleFoo")
        );
    }
}

#[test]
fn string_helper_append_capitalized_var_args() {
    let words = [utf16("foo"), utf16("bar"), utf16("foo")];
    for cases in CASES {
        assert_eq!(
            append_capitalized_words_utf16(&utf16("assemble"), words.iter().map(Vec::as_slice), cases),
            utf16("assembleFooBarFoo")
        );
    }
}

#[test]
fn string_helper_decapitalize_empty() {
    for cases in CASES {
        assert_eq!(us_locale_decapitalize_utf16(&[], cases), utf16(""));
    }
}

#[test]
fn string_helper_decapitalize_non_letter() {
    for cases in CASES {
        assert_eq!(us_locale_decapitalize_utf16(&utf16("1-Foo"), cases), utf16("1-Foo"));
    }
}

#[test]
fn string_helper_decapitalize_unicode() {
    for cases in CASES {
        assert_eq!(us_locale_decapitalize_utf16(&utf16("Ê-foo"), cases), utf16("ê-foo"));
    }
}

#[test]
fn string_helper_surrogate_values_decapitalize() {
    let value = [0xd801, 0xdc00, 0x2d, 0x66, 0x6f, 0x6f];
    for cases in CASES {
        assert_eq!(us_locale_decapitalize_utf16(&value, cases), value);
    }
}

#[test]
fn string_helper_string_builder_append_capitalized() {
    for cases in CASES {
        let mut prefixed = utf16("prefix");
        append_capitalized_utf16(&mut prefixed, &utf16("word"), cases);
        assert_eq!(prefixed, utf16("prefixWord"));
        let mut empty = Vec::new();
        append_capitalized_utf16(&mut empty, &utf16("word"), cases);
        assert_eq!(empty, utf16("Word"));
        let mut empty = Vec::new();
        append_capitalized_utf16(&mut empty, &[], cases);
        assert_eq!(empty, utf16(""));
    }
}

#[test]
fn string_helper_append_capitalized_overloads() {
    let words = [utf16("one"), utf16("two")];
    for cases in CASES {
        assert_eq!(
            append_capitalized_words_utf16(&utf16("base"), [&utf16("word")[..]], cases),
            utf16("baseWord")
        );
        assert_eq!(
            append_capitalized_words_utf16(&utf16("base"), words.iter().map(Vec::as_slice), cases),
            utf16("baseOneTwo")
        );
    }
}

#[test]
fn string_helper_capitalize_and_append() {
    for cases in CASES {
        assert_eq!(capitalize_and_append_utf16(&utf16("word"), &utf16("Suffix"), cases), utf16("WordSuffix"));
        assert_eq!(capitalize_and_append_utf16(&[], &utf16("Suffix"), cases), utf16("Suffix"));
    }
}

#[test]
fn string_helper_append_camel_case() {
    for cases in CASES {
        let mut empty = Vec::new();
        append_camel_case_utf16(&mut empty, &utf16("foo"), cases);
        assert_eq!(empty, utf16("foo"));
        let mut prefix = utf16("foo");
        append_camel_case_utf16(&mut prefix, &utf16("bar"), cases);
        assert_eq!(prefix, utf16("fooBar"));
        let mut prefix = utf16("foo");
        append_camel_case_utf16(&mut prefix, &utf16("Bar"), cases);
        assert_eq!(prefix, utf16("fooBar"));
    }
}

#[test]
fn string_helper_combine_as_camel_case() {
    for cases in CASES {
        let words = [utf16("foo"), utf16("bar"), utf16("baz")];
        assert_eq!(combine_as_camel_case_utf16(words.iter().map(Vec::as_slice), cases), utf16("fooBarBaz"));
        let words = [utf16("Foo"), utf16("bar"), utf16("Baz")];
        assert_eq!(combine_as_camel_case_utf16(words.iter().map(Vec::as_slice), cases), utf16("FooBarBaz"));
        assert_eq!(combine_as_camel_case_utf16(std::iter::empty(), cases), utf16(""));
        let objects = [1, 2, 3];
        assert_eq!(combine_as_camel_case_mapped_utf16(objects, |value| utf16(&format!("item{value}")), cases), utf16("item1Item2Item3"));
    }
}

#[test]
fn string_helper_to_strings() {
    let a = utf16("a");
    let b = utf16("b");
    let c = utf16("c");
    assert_eq!(to_strings_utf16(&[StringHelperArgument::String(&a), StringHelperArgument::String(&b)]), vec![a.clone(), b.clone()]);
    let items = [StringHelperArgument::String(&b), StringHelperArgument::String(&c)];
    assert_eq!(
        to_strings_utf16(&[StringHelperArgument::String(&a), StringHelperArgument::Collection { items: &items, display: &|| utf16("[b, c]") }, StringHelperArgument::Other(&|| utf16("123"))]),
        vec![a, b, c, utf16("123")]
    );
    assert_eq!(to_strings_utf16(&[StringHelperArgument::Other(&|| utf16("custom"))]), vec![utf16("custom")]);
}

#[test]
fn string_helper_to_system_line_separator() {
    let system_separator = if cfg!(windows) { "\r\n" } else { "\n" };
    assert_eq!(to_system_line_separator("line1\nline2"), format!("line1{system_separator}line2"));
    assert_eq!(to_system_line_separator("line1\r\nline2"), format!("line1{system_separator}line2"));
}

#[test]
fn string_helper_simple_uppercase_and_one_unit_lowercase_preserve_java_semantics() {
    for cases in CASES {
        assert_eq!(us_locale_capitalize_utf16(&utf16("ßuffix"), cases), utf16("ßuffix"));
        assert_eq!(us_locale_capitalize_utf16(&utf16("ﬀoo"), cases), utf16("ﬀoo"));
        assert_eq!(us_locale_capitalize_utf16(&utf16("ırest"), cases), utf16("Irest"));
        assert_eq!(us_locale_capitalize_utf16(&[0xd801, 0xdc28, 0x61], cases), [0xd801, 0xdc00, 0x61]);
        assert_eq!(us_locale_decapitalize_utf16(&utf16("İsuffix"), cases), utf16("i\u{307}suffix"));
        assert_eq!(us_locale_decapitalize_utf16(&utf16("Σsuffix"), cases), utf16("σsuffix"));
        for value in [[0xd801, 0x61], [0xdc00, 0x61]] {
            assert_eq!(us_locale_capitalize_utf16(&value, cases), value);
            assert_eq!(us_locale_decapitalize_utf16(&value, cases), value);
        }
    }
}

#[test]
fn string_helper_case_tables_are_explicit_and_versioned() {
    assert_eq!(CaseMapping::for_jdk_major(17), Some(CaseMapping::Unicode13));
    assert_eq!(CaseMapping::for_jdk_major(25), Some(CaseMapping::Unicode16));
    assert_eq!(CaseMapping::for_jdk_major(26), None);
    assert_eq!(us_locale_capitalize_utf16(&[0xa7d1], CaseMapping::Unicode13), [0xa7d1]);
    assert_eq!(us_locale_capitalize_utf16(&[0xa7d1], CaseMapping::Unicode14), [0xa7d0]);
    assert_eq!(us_locale_capitalize_utf16(&[0xa7cd], CaseMapping::Unicode15_1), [0xa7cd]);
    assert_eq!(us_locale_capitalize_utf16(&[0xa7cd], CaseMapping::Unicode16), [0xa7cc]);
    assert_eq!(capitalize_ascii("freeDebug"), Some("FreeDebug".into()));
    assert_eq!(capitalize_ascii(""), Some(String::new()));
    assert_eq!(capitalize_ascii("êDebug"), None);
}

#[test]
fn string_helper_camel_case_tracks_first_element_independently_of_builder_emptiness() {
    for cases in CASES {
        assert_eq!(combine_as_camel_case_utf16([&[][..], &utf16("foo")[..]], cases), utf16("Foo"));
        let mut destination = Vec::new();
        append_camel_case_utf16(&mut destination, &[], cases);
        append_camel_case_utf16(&mut destination, &utf16("foo"), cases);
        assert_eq!(destination, utf16("foo"));
        let calls = Cell::new(0);
        let mut destination = utf16("prefix");
        combine_as_camel_case_mapped_into_utf16(&mut destination, ["foo", "", "bar"], |word| {
            calls.set(calls.get() + 1);
            utf16(word)
        }, cases);
        assert_eq!(destination, utf16("prefixfooBar"));
        assert_eq!(calls.get(), 3);
        assert_eq!(combine_as_camel_case_utf16([&[0xd800][..], &utf16("foo")[..]], cases), vec![0xd800, 0x46, 0x6f, 0x6f]);
    }
}

#[test]
fn string_helper_collection_non_strings_repeat_the_enclosing_display() {
    let calls = Cell::new(0);
    let display = || { calls.set(calls.get() + 1); utf16("[word, 123, null, []]") };
    let word = utf16("word");
    let items = [StringHelperArgument::String(&word), StringHelperArgument::Other(&|| utf16("123")), StringHelperArgument::Other(&|| utf16("null")), StringHelperArgument::Collection { items: &[], display: &|| utf16("[]") }];
    assert_eq!(
        to_strings_utf16(&[StringHelperArgument::Collection { items: &items, display: &display }]),
        vec![word, utf16("[word, 123, null, []]"), utf16("[word, 123, null, []]"), utf16("[word, 123, null, []]")]
    );
    assert_eq!(calls.get(), 3);
    assert_eq!(to_strings_utf16(&[StringHelperArgument::Collection { items: &[], display: &display }]), Vec::<Vec<u16>>::new());
    assert_eq!(calls.get(), 3);
    assert_eq!(to_strings_utf16(&[StringHelperArgument::String(&[0xd800])]), vec![vec![0xd800]]);
}

#[test]
fn string_helper_newlines_remove_all_cr_and_preserve_other_utf16_units() {
    let input = [0xd800, 0x0d, 0x0d, 0x61, 0x0a, 0x0d, 0x62, 0x0a];
    assert_eq!(to_line_separator_utf16(&input, SystemLineSeparator::Lf), [0xd800, 0x61, 0x0a, 0x62, 0x0a]);
    assert_eq!(to_line_separator_utf16(&input, SystemLineSeparator::CrLf), [0xd800, 0x61, 0x0d, 0x0a, 0x62, 0x0d, 0x0a]);
    assert_eq!(to_line_separator_utf16(&[], SystemLineSeparator::CrLf), Vec::<u16>::new());
    assert_eq!(to_line_separator_utf16(&[0x0d], SystemLineSeparator::Lf), Vec::<u16>::new());
}
