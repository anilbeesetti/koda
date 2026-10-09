//! Rust source-set casing over the captured JDK's Unicode data.
//!
//! The tables are generated from attributed Unicode data, not the host Rust or
//! ICU version. Conditional mappings follow Unicode SpecialCasing. Java's sigma
//! rule additionally depends on locale word breaking; unsupported word-boundary
//! contexts remain explicit unavailable evidence rather than guessed membership.

#[path = "jdk_source_set_case_data.rs"]
mod data;

#[derive(Clone, Copy)]
pub(super) struct JdkCaseMap {
    column: Option<usize>,
}

impl JdkCaseMap {
    pub(super) fn for_version(version: &str) -> Self {
        let major = version
            .split(['.', '-', '+'])
            .next()
            .and_then(|value| value.parse::<u32>().ok());
        Self {
            column: match major {
                Some(17 | 18) => Some(0),
                Some(19) => Some(1),
                Some(20 | 21) => Some(2),
                Some(22 | 23) => Some(3),
                Some(24 | 25) => Some(4),
                _ => None,
            },
        }
    }

    pub(super) fn lowercase(self, input: &str, language: &str) -> Result<String, &'static str> {
        self.lowercase_with_context_probes(input, language, || {})
    }

    fn lowercase_with_context_probes(
        self,
        input: &str,
        language: &str,
        mut probe: impl FnMut(),
    ) -> Result<String, &'static str> {
        // ASCII mappings, including locale-sensitive I, are invariant across
        // the supported JDKs and require no Unicode-version assumption.
        if input.is_ascii() {
            return Ok(input
                .chars()
                .map(|value| {
                    if value == 'I' && matches!(language, "tr" | "az") {
                        '\u{131}'
                    } else {
                        value.to_ascii_lowercase()
                    }
                })
                .collect());
        }
        let column = self
            .column
            .ok_or("Captured JDK has no supported Unicode casing data")?;
        let chars: Vec<char> = input.chars().collect();
        let has_sigma = chars.contains(&'\u{3a3}');
        if has_sigma
            && !chars.iter().all(|value| {
                value.is_ascii_alphabetic()
                    || ('\u{300}'..='\u{36f}').contains(value)
                    || Self::greek_cased(*value, column)
            })
        {
            return Err("Captured Java sigma word-boundary context is not supported");
        }
        // Leading enclosing marks form standalone words in Java's default
        // word rules. Once a real letter starts this admitted Greek/ASCII word,
        // embedded U+0345 is cased. Record its first/last cased positions once;
        // rescanning a long mark prefix for every sigma would be quadratic.
        let mut first_cased = None;
        let mut last_cased = None;
        if has_sigma {
            for (index, &value) in chars.iter().enumerate() {
                probe();
                let letter = value.is_ascii_alphabetic() || Self::greek_cased(value, column);
                if first_cased.is_none() && letter {
                    first_cased = Some(index);
                }
                if first_cased.is_some() && (letter || value == '\u{345}') {
                    last_cased = Some(index);
                }
            }
        }
        let mut output = String::with_capacity(input.len());
        for (index, &value) in chars.iter().enumerate() {
            let following = &chars[index + 1..];
            let preceding = &chars[..index];
            let more_above = || {
                following
                    .iter()
                    .map(|value| Self::combining(*value, column))
                    .take_while(|class| *class != 0)
                    .any(|class| class == 230)
            };
            let before_dot = || {
                following
                    .iter()
                    .take_while(|value| {
                        let class = Self::combining(**value, column);
                        **value == '\u{307}' || (class != 0 && class != 230)
                    })
                    .any(|value| *value == '\u{307}')
            };
            let after_i = || {
                preceding
                    .iter()
                    .rev()
                    .find(|value| {
                        let class = Self::combining(**value, column);
                        class == 0 || class == 230
                    })
                    .is_some_and(|value| *value == 'I')
            };
            match (language, value) {
                ("tr" | "az", '\u{130}') => output.push('i'),
                ("tr" | "az", '\u{307}') if after_i() => {}
                ("tr" | "az", 'I') if !before_dot() => output.push('\u{131}'),
                ("lt", 'I' | 'J' | '\u{12e}') if more_above() => {
                    Self::append_simple(&mut output, value, column);
                    output.push('\u{307}');
                }
                ("lt", '\u{cc}') => output.push_str("i\u{307}\u{300}"),
                ("lt", '\u{cd}') => output.push_str("i\u{307}\u{301}"),
                ("lt", '\u{128}') => output.push_str("i\u{307}\u{303}"),
                (_, '\u{3a3}') => {
                    output.push(
                        if first_cased.is_some_and(|first| first < index)
                            && last_cased == Some(index)
                        {
                            '\u{3c2}'
                        } else {
                            '\u{3c3}'
                        },
                    );
                }
                _ => Self::append_simple(&mut output, value, column),
            }
        }
        Ok(output)
    }

    fn append_simple(output: &mut String, value: char, column: usize) {
        match data::LOWER.binary_search_by_key(&(value as u32), |row| row.0) {
            Ok(index) => output.push_str(data::LOWER[index].1[column]),
            Err(_) => output.push(value),
        }
    }

    fn combining(value: char, column: usize) -> u8 {
        data::COMBINING
            .binary_search_by_key(&(value as u32), |row| row.0)
            .ok()
            .map_or(0, |index| data::COMBINING[index].1[column])
    }

    fn greek_cased(value: char, column: usize) -> bool {
        data::GREEK_CASED
            .binary_search_by_key(&(value as u32), |row| row.0)
            .ok()
            .is_some_and(|index| data::GREEK_CASED[index].1 & (1 << column) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::JdkCaseMap;

    #[test]
    fn unicode_additions_follow_supported_jdk_release_data() {
        for (version, latin, vithkuqi) in [
            ("17.0.17", "\u{a7cc}", "\u{10570}"),
            ("18", "\u{a7cc}", "\u{10570}"),
            ("19", "\u{a7cc}", "\u{10597}"),
            ("20", "\u{a7cc}", "\u{10597}"),
            ("21.0.9+10", "\u{a7cc}", "\u{10597}"),
            ("22", "\u{a7cc}", "\u{10597}"),
            ("23", "\u{a7cc}", "\u{10597}"),
            ("24", "\u{a7cd}", "\u{10597}"),
            ("25-ea", "\u{a7cd}", "\u{10597}"),
        ] {
            let mapper = JdkCaseMap::for_version(version);
            assert_eq!(mapper.lowercase("\u{a7cc}", "en"), Ok(latin.into()));
            assert_eq!(mapper.lowercase("\u{10570}", "en"), Ok(vithkuqi.into()));
        }
    }

    #[test]
    fn versioned_conditional_casing_preserves_combining_context() {
        for version in ["17", "21", "25"] {
            let mapper = JdkCaseMap::for_version(version);
            for (language, input, expected) in [
                ("tr", "I\u{307}", "i"),
                ("az", "I\u{301}\u{307}", "\u{131}\u{301}\u{307}"),
                ("tr", "I\u{323}\u{307}", "i\u{323}"),
                ("lt", "I\u{301}", "i\u{307}\u{301}"),
                ("lt", "I\u{323}\u{301}", "i\u{307}\u{323}\u{301}"),
                ("lt", "\u{cc}", "i\u{307}\u{300}"),
                ("en", "\u{130}", "i\u{307}"),
                ("el", "ΟΣ", "ος"),
                ("el", "ΟΣΑ", "οσα"),
            ] {
                assert_eq!(mapper.lowercase(input, language), Ok(expected.into()));
            }
        }
    }

    #[test]
    fn absent_or_unsupported_unicode_provenance_never_guesses_non_ascii() {
        for version in ["", "unknown", "1.8.0_402", "26", "21garbage"] {
            let mapper = JdkCaseMap::for_version(version);
            assert_eq!(mapper.lowercase("MAIN", "en"), Ok("main".into()));
            assert_eq!(mapper.lowercase("MAIN", "tr"), Ok("ma\u{131}n".into()));
            assert_eq!(
                mapper.lowercase("\u{a7cc}", "en"),
                Err("Captured JDK has no supported Unicode casing data")
            );
        }
    }

    #[test]
    fn ambiguous_java_sigma_word_breaking_is_explicit_unavailable() {
        assert_eq!(
            JdkCaseMap::for_version("21").lowercase("AΣ.B", "en"),
            Err("Captured Java sigma word-boundary context is not supported")
        );
        assert_eq!(
            JdkCaseMap::for_version("21").lowercase("Σ", "el"),
            Ok("σ".into())
        );
    }
}

#[cfg(test)]
mod sigma_word_boundary_tests {
    use super::JdkCaseMap;

    #[test]
    fn leading_enclosing_cased_marks_do_not_supply_java_word_prefix() {
        let mapper = JdkCaseMap::for_version("21");
        for (input, expected) in [
            ("\u{345}Σ", "\u{345}σ"),
            ("\u{345}ΑΣ", "\u{345}ας"),
            ("AΣ\u{345}", "aσ\u{345}"),
            ("\u{300}ΣΣ", "\u{300}σς"),
        ] {
            assert_eq!(mapper.lowercase(input, "el"), Ok(expected.into()));
        }
    }

    #[test]
    fn long_enclosing_mark_and_sigma_sequences_have_linear_context_work() {
        let prefix = "\u{300}".repeat(65_536);
        let input = format!("{prefix}{}", "Σ".repeat(65_536));
        let mut inspected = 0;
        let actual = JdkCaseMap::for_version("21")
            .lowercase_with_context_probes(&input, "el", || {
                inspected += 1;
            })
            .expect("Supported standalone marks and Greek word");
        assert_eq!(inspected, 131_072);
        assert!(actual.starts_with(&prefix));
        let word = &actual[prefix.len()..];
        assert_eq!(word.chars().count(), 65_536);
        assert!(word.chars().take(65_535).all(|value| value == 'σ'));
        assert_eq!(word.chars().last(), Some('ς'));
    }
}
