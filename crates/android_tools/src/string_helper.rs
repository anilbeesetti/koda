/*
 * Copyright (C) 2013 The Android Open Source Project
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

/// Checks for an exact element in a list separated by individual UTF-16 code units.
///
/// Adapted from AOSP `com.android.utils.StringHelper.asSeparatedListContains`.
pub fn separated_list_contains_utf16(input: &[u16], element: &[u16], separators: &[u16]) -> bool {
    let mut offset = 0;
    while let Some(remainder) = input.get(offset..) {
        if remainder.is_empty() {
            break;
        }
        if let Some(after_element) = remainder.strip_prefix(element)
            && (after_element.is_empty()
                || after_element
                    .first()
                    .is_some_and(|character| separators.contains(character)))
        {
            return true;
        }

        // The reference tests token starts while offset < length, so a trailing
        // separator does not introduce another empty element to examine.
        let advance = remainder
            .iter()
            .position(|character| separators.contains(character))
            .map_or(remainder.len(), |separator_offset| separator_offset + 1);
        offset += advance;
    }
    false
}

/// Checks separated-list membership with the reference's UTF-16 semantics.
pub fn separated_list_contains(input: &str, element: &str, separators: &str) -> bool {
    // Kotlin indexes CharSequence and matches separator Char values as UTF-16
    // units; Unicode scalar separators would change supplementary characters.
    let input: Vec<_> = input.encode_utf16().collect();
    let element: Vec<_> = element.encode_utf16().collect();
    let separators: Vec<_> = separators.encode_utf16().collect();
    separated_list_contains_utf16(&input, &element, &separators)
}
