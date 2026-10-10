# Manifest Class-Path behavior ports

The complete originals in `original/` come from AOSP `tools/base`, revision
`4a5d2ec9571e2021fc9a4621e24a195f966ee6dd`, under
[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).
Original copyright/license headers are retained. `provenance.json` binds every
original byte count and SHA-256, reference path and archive member ordinal.
The Rust helper and the two named behavior tests are adapted from these sources.

`TestGroupTest.testValidAbsolutePath` contributes two assertions, at lines 53
and 59. `TestGroupTest.testValidRelativePath` contributes three, at lines 68,
71 and 87. The Rust integration target preserves them all, including explicit
successful target creation and absence of the target basename in the actual cwd.
Complete queue equality preserves the original `containsExactly` checks.

The originals create real ZIP/JAR fixtures at runtime. The Rust tests do the
same with `META-INF/MANIFEST.MF`, `Manifest-Version: 1.0`, `Class-Path`, and
`dummy.txt` containing `dummy`. Empty target files match the original fixture;
this helper checks file existence, not target JAR validity. Stored and deflated
members exercise the production reader. No test substitutes a raw CP string for
the JAR reader. No process-wide cwd mutation is needed for concurrent tests.

The helper resolves only the direct manifest of one wrapper. It preserves queue
seeds, token order, duplicates and already appended prefixes on later URI/limit
errors. Relative URI tokens use the wrapper's parent. Missing targets or
folders receive diagnostics; I/O errors propagate. Java's ASCII regex separators
are preserved instead of Rust's broader Unicode whitespace. Main attribute
names are case-insensitive, continuation bytes fold before UTF-8 replacement
decoding, and duplicate main attributes retain the last value with a diagnostic.
Named sections are parsed for syntax and never contribute their Class-Path.

This bounded ZIP profile supports stored/deflated manifests. It rejects ZIP64,
multidisk, encrypted entries, duplicate manifest members and encoded dot path
segments with explicit errors. WHATWG URL parsing would otherwise normalize the
encoded dot segments differently from Java URI resolution. Non-file URI schemes,
authorities (including localhost), queries, fragments, malformed escapes and
non-UTF-8 URI paths are errors. These explicit limitations are unresolved
compatibility obligations; no complete Java ZIP/URI equivalence or N/A verdict
is claimed. Permission/I/O failures are propagated, rather than Java File's
boolean suppression. Default limits are 16 MiB per archive, 2 MiB per central
directory, 4,096 entries, 256 KiB expanded manifest, 512 bytes per physical
manifest line, 4,096 tokens/queue entries, and 8 KiB per URI/resolved path.

Only the two individually named original cases can earn parity credit after
actual exact Rust runs and reviewed evidence. All additional tests are
supplemental. The provenance file records not-run status; canonical parity
rows/log evidence are integrated by the lead after validation. Global census
completeness and effective runtime case counts remain unknown.

Run the complete integration target with:

```sh
cargo test --locked -p xtask --test android_manifest_classpath
```

Run either original independently by adding its exact test name:

```sh
cargo test --locked -p xtask --test android_manifest_classpath test_valid_absolute_path -- --exact --show-output
cargo test --locked -p xtask --test android_manifest_classpath test_valid_relative_path -- --exact --show-output
```

Inspect a wrapper with the developer command:

```sh
cargo xtask android-manifest-class-path --jar /absolute/path/to/wrapper.jar
```

The JSON reports direct entries/diagnostics and makes no suite discovery claim.
