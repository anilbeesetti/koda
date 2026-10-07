# Java declaration facts for the Android project tree

This task adds a bounded Rust declaration producer in
`android_tools::java_class_facts`. It accepts actual source bytes, without a
filename, and returns the package and top-level class, interface, enum, record,
and annotation declarations in source encounter order. Nested and local types
are excluded. Declaration starts and name tokens retain their original UTF-8
byte offsets. `discover_top_level_java_classes` converts successful results to
the existing `project_tree::JavaClassFact` with a known name-token offset.

The parser recognizes declarations and balanced lexical structure. It does not
validate Java statements, resolve symbols, type-check source, or provide a Java
language server. Facts describe declarations, not compilation success. Callers
must bind facts to the exact buffer revision and validate offsets before opening
or publishing a navigation target. An unavailable result keeps the file
presentation available without inventing a class name or providing parity
credit.

Comments, escaped character/string literals, text blocks, annotations, generic
headers and nested scopes are handled by a streaming lexer. Unicode contents in
comments and literals preserve byte offsets. Every backslash followed by `u` is
conservatively unavailable, including sequences that Java might consider
ineligible, because this producer does not implement Java's pre-tokenization
Unicode escape eligibility rules. Non-ASCII code tokens are unavailable because
Java identifier classification requires its own versioned Unicode tables.
Invalid UTF-8, malformed or unterminated lexical structures, unsupported
top-level shapes, and mismatched delimiters return a typed error with a byte
offset. Results are all-or-nothing; no partial declaration list escapes after an
error. Source size is bounded to 8 MiB, delimiter nesting to 512, and declarations
to 16,384 to bound background work and output memory.

The exact original source literal from pinned AOSP
`AndroidProjectViewTest.testGeneratedSourceFiles_lightClasses` is retained in
`crates/android_tools/test_data/java_class_facts/BuildConfig.java`. Its provenance
records the original revision, source-suite SHA-256, byte count and fixture hash.
The Apache 2.0 license and unchanged original source remain in the existing
project-tree fixture collection. Supplemental Rust tests exercise that literal
and the unchanged original `MyActivity.java`, alongside lexical, malformed,
Unicode-offset, scope and bounded-input regressions.

No canonical parity row changes in this task. The complete original generated
class case still requires real Gradle/model-generated root discovery, exact
source write, real file refresh and the integrated Android tree assertion. The
other four original tree cases also remain unported. Product tree wiring,
full-app validation and combined full-workspace gates are required before this
component can be treated as an integrated feature.

## Acceptance and validation

| Item | Required result |
| --- | --- |
| Actual declaration names | Exact original BuildConfig and MyActivity inputs produce their declared names and package facts without any filename input. |
| Navigation offsets | Actual name tokens point to unchanged UTF-8 bytes, including multibyte comments and literals preceding them. |
| Scope and lexical behavior | Multi-type encounter order, all five type kinds, annotations, modifiers, generic headers, comments, literals, text blocks and nested types have meaningful Rust regressions. |
| Unavailable facts | Malformed/unsupported input returns the specific typed reason; successful prefixes never become partial success after a later failure. |
| Resource bounds | Near-limit source and many-declaration inputs retain correct facts; size, declaration-count and depth overflow produce typed unavailable results. |
| Review and build | Five scoped reviewer verdicts, actual focused tests, affected crate tests, formatting and repository Clippy must pass before lead integration. |
| Product and parity | The lead must run the full app and full workspace gates. This component supplies no original integration-test parity credit by itself. |

The task touches `android_tools/src/java_class_facts.rs`, one module export, its
attributed fixture/provenance, and this task report. It depends on the reviewed
project-tree component's unchanged `JavaClassFact` interface. The active
source/provider producer and background/UI adapter are separate dependent tasks.
