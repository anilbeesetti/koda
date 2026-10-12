# StringHelper parity: existing PR69

Base: `6d8c5358e73e080d72fe9eafb3e52963a4ddb470`, existing branch
`android-studio-task/1-kotlin-string-policy`. The pinned reference is AOSP
`platform/tools/base@4a5d2ec9571e2021fc9a4621e24a195f966ee6dd`, suite
`com.android.utils.StringHelperTest`, file
`common/src/test/java/com/android/utils/StringHelperTest.kt` (SHA-256
`64c78dd2d8717c43b555760b578b9eaa5b5136eac2434d0a3f2a3d6246fa3950`).
Complete original source, fixtures and Apache-2.0 attribution remain unchanged.

All 19 methods are applicable. Each original operand and ordered predicate has
an adapted Rust assertion; none is skipped, weakened, deleted or declared
inapplicable. This adds the remaining 18 methods/31 original predicates to the
existing six-predicate separated-list method. Canonical catalog and ledger now
track these 19 individually, preserving all 95 previous obligations. New runs
are `not_run`, with no run receipt: source implementation is not acceptance.

| Original method | Predicates | Rust integration test (`string_helper` unless noted) | Source status / run |
| --- | ---: | --- | --- |
| testBasicCapitalize | 1 | string_helper_basic_capitalize | adapted / not_run |
| testNonLetterCapitalize | 1 | string_helper_non_letter_capitalize | adapted / not_run |
| testUnicodeCapitalize | 1 | string_helper_unicode_capitalize | adapted / not_run |
| testSurrogateValuesCapitalize | 1 | string_helper_surrogate_values_capitalize | adapted / not_run |
| testAppendCapitalized | 1 | string_helper_append_capitalized | adapted / not_run |
| testAppendCapitalizedVarArgs | 1 | string_helper_append_capitalized_var_args | adapted / not_run |
| testDecapitalizeEmpty | 1 | string_helper_decapitalize_empty | adapted / not_run |
| testDecapitalizeNonLetter | 1 | string_helper_decapitalize_non_letter | adapted / not_run |
| testDecapitalizeUnicode | 1 | string_helper_decapitalize_unicode | adapted / not_run |
| testSurrogateValuesDecapitalize | 1 | string_helper_surrogate_values_decapitalize | adapted / not_run |
| testAsSeparatedListContains | 6 | string_helper_as_separated_list_contains | adapted / named recording pending |
| testStringBuilderAppendCapitalized | 3 | string_helper_string_builder_append_capitalized | adapted / not_run |
| testAppendCapitalizedOverloads | 2 | string_helper_append_capitalized_overloads | adapted / not_run |
| testCapitalizeAndAppend | 2 | string_helper_capitalize_and_append | adapted / not_run |
| testAppendCamelCase | 3 | string_helper_append_camel_case | adapted / not_run |
| testCombineAsCamelCase | 4 | string_helper_combine_as_camel_case | adapted / not_run |
| testToStrings | 3 | string_helper_to_strings | adapted / not_run |
| testTokenizeCommandLine | 2 | string_helper_tokenize::test_tokenize_command_line | adapted / not_run |
| testToSystemLineSeparator | 2 | string_helper_to_system_line_separator | adapted / not_run |

The StringHelper fixture manifest retains exact original method/assertion
hashes, full extracted predicates, literal UTF-16 fixtures and Rust targets.
Preflight `unported` statuses nested in those immutable extraction records are
historical declarations; current source/run status is stated separately.

## Independent tasks and acceptance

| Task | Files | Dependencies | Required acceptance |
| --- | --- | --- | --- |
| newline | string_helper/text.rs; tests/string_helper.rs | existing UTF-16 helper | Both original predicates, LF/CRLF cores, remove every CR, isolated surrogate preservation |
| object display | string_helper/text.rs; tests/string_helper.rs | none | Three original ordered results; each non-string collection item repeats enclosing collection display; callback count and empty/nested/nullable display fixtures |
| uppercase | string_helper/case.rs and case_data.rs; tests/string_helper.rs | attributed versioned Unicode inputs | Five original methods/seven predicates; Java simple BMP mapping, leading supplementary uppercase and isolated surrogates |
| lowercase | same casing files | explicit case-data version | Four original methods/four predicates; first UTF-16 unit only, U+0130 expansion, standalone sigma, supplementary unchanged |
| append overloads | same casing files | uppercase | Four original methods/six predicates; preserve prefixes/suffixes and independently uppercase supplied words |
| camel-case | same casing files | uppercase | Two original methods/seven predicates; distinguish first-element flag from empty-builder state and call mapper once per input |
| tokenizer | string_helper/tokenize.rs; tests/string_helper_tokenize.rs | pinned TokenizedCommandLine rules | Two original ordered predicates; both platforms, Java whitespace, exact quote/backslash/caret/newline rules and UTF-16 preservation |
| named recording | xtask AndroidStringHelper; focused workflow | mapped tests and clean tested revision | Exact named Cargo output contains one execution, zero ignored cases, verified exit/source hashes; preserve failures; never auto-credit the ledger |
| IDE consumer | AndroidTarget::gradle_task | model's existing ASCII variant validation | Existing assemble/test task-name assertions remain; use invariant ASCII casing; preserve public API's prior Unicode fallback |

Shared-file integration is sequential. Each task requires compilation, its
focused tests, the full workspace suite, formatting/Clippy, five independent
reviews and lead review before merge. This packet does not claim those gates
have run.

## Unicode and workflow limits

Casing is explicitly parameterized by JDK17–25's documented Unicode versions
(13, 14, 15, 15.1, 16); there is no guessed host default. Original fixtures are
checked for every supported version. Tables use the exact Unicode inputs already
attributed by PR77 at `074ef8782244b18240d199c887595247b9ac131d`.
The pinned Android Studio Rabbit1 bundled JBR version remains unbound, and its
JVM differential remains pending. These are acceptance blockers, not exclusions.

Only `AndroidTarget::gradle_task` has a production consumer added. Its evaluated
project model accepts ASCII variant names, whose case mapping is invariant.
Text normalization never changes raw Gradle model transport. Object conversion
and command tokenization currently expose tested compatibility APIs without a
new IDE workflow caller; no screenshot or end-to-end consumer credit is claimed.
TokenizedCommandLine removal, index-generation and hashing APIs and their
separate suites remain outside this wrapper port and are tracked unported.

The focused workflow runs all StringHelper originals/supplements, the strict
Rust recorder tests, and the existing separated-list original with
`--exact --show-output`. It uploads a hash-bound Rust receipt and genuine log.
The recorder validates a workflow-supplied completed log; it does not itself
execute Cargo or establish the workflow's external provenance. Root must review
the actual workflow/run/commit before reconciling canonical run evidence. The
full Fork CI workflow and all existing checks remain unchanged.

All implementation, regeneration, recording and assertions are Rust. Bash/YAML
only invoke tools and transport a completed process status/log; the inherited
Linux dependency installer and official GitHub actions supply the existing
build environment. No JVM is used as a production StringHelper implementation.

The string token wrappers return Result<Vec<String>, FromUtf16Error> and propagate
conversion errors. UTF-16 core behavior is unchanged. Both wrapper tests return
Result and retain every original ordered equality; a conversion error fails the
test instead of being discarded or replaced. This resolves the initial draft
quality finding about a production expect() without weakening any predicate.
