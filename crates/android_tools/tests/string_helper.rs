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
