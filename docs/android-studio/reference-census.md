# Discover reference source candidates

`cargo xtask android-reference-census` discovers review candidates from the
archives pinned in `reference-manifest.json`. It does not populate the canonical
test matrix, identify effective runtime case counts, or award parity credit.

Use an existing parent directory outside the checkout. The output directory must
be new; the command preserves existing output and never extracts archive files.

```sh
cargo xtask android-reference-census \
  --repository /workspace/android-studio-reference-census \
  --reference-root /workspace/android-studio-references \
  --output /workspace/android-studio-artifacts/reference-census

cargo xtask android-reference-census \
  --repository /workspace/android-studio-reference-census \
  --reference-root /workspace/android-studio-references \
  --output /workspace/android-studio-artifacts/reference-census --check
```

The first command verifies the pinned archives and stages all generated files
before reserving the output directory. Each destination file is created without
replacement. `summary.json` is published last and marks completed generation;
never accept a directory without this marker. A publication failure preserves
existing files and reports that a fresh output directory is needed. `--check`
regenerates in isolated staging and requires an identical artifact set and bytes.
Changed archives, unsafe/duplicate paths, malformed framing, gzip corruption or
unparsed trailing archive content fail before successful publication.

Each source has three newline-delimited JSON inventories:

| Artifact                          | What it records                                                                                                                            |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `<source>-physical-members.jsonl` | Every raw tar header and payload, including GNU/PAX extension metadata; ordinal, raw path bytes, type byte and hashes.                     |
| `<source>-members.jsonl`          | Resolved logical paths, member kinds, sizes, payload hashes, links and discovery classifications.                                          |
| `<source>-candidates.jsonl`       | Source/build/config/script candidates, fixture path hints, copyright lines, JVM lexical declarations and explicit unresolved requirements. |

`summary.json` pins revisions and archive hashes, hashes every generated inventory
and separates physical member totals, logical members and declaration candidates.
These counts describe source discovery. They are never test-case or port counts.
Both archive traversals independently verify the exact compressed stream consumed
against the pinned hash and size before publication.

The mirror's exact pinned root directory is retained with an empty logical
`path`; its original `archive_path` and physical header remain recorded. This
empty root applies only to a directory, whether its wrapper name has a trailing
slash. Regular files cannot have an empty logical path, and every mirror child
still requires the exact pinned wrapper prefix and normal safe-path validation.

The verified Git archive's global PAX record is retained as logical kind
`global_pax_metadata`, classified `archive_metadata`, with its original metadata
name `pax_global_header` and payload hash. Its physical header/payload provenance
is also retained. This metadata name is not a filesystem path. Only the exact
`52 comment=<pinned revision>\n` payload is supported; semantic overrides such as
`path`, `linkpath` or `size`, other keys, malformed records and other metadata
names fail before publication. Regular entries receive no metadata exception,
and duplicate normalized filesystem names remain rejected. Metadata names have
their own duplicate guard, so a valid anchored regular file whose relative name
is `pax_global_header` remains distinct from that metadata record. Duplicate global
metadata headers are still rejected.

The scanner preserves JUnit3 test-prefixed declarations, suite factories, Kotlin
backtick names, declared nested classes and annotations. It retains abstract
classes and inheritance text, but does not resolve their effective runner suites.
Imports, aliases, custom annotations, disabled tests, parameter providers, repeated
or dynamic cases and generated test dispatch remain unresolved. Java Unicode
escape preprocessing is flagged rather than silently treated as resolved. A
declaration's `effective_runtime_cases` stays `null`.

Python, C/C++, Rust, shell and other runner/configuration sources retain hashes and
unresolved discovery. Executable and shebang scripts are retained even without a
recognized suffix. Fixture-like source paths are candidates for review; they are
not automatically included as tests or excluded. Unclassified payloads remain in
the complete member inventory and require review for additional runner formats.
Sources larger than the four-MiB lexical buffer retain their full member hash and
an explicit blocker. Lines longer than 16 KiB defer lexical discovery; token and
declaration budgets flag partial discovery explicitly. Cached lexical boundaries
avoid repeated source scans. Declaration prefixes and class headers have explicit
limits; incomplete inheritance text is flagged. An eight-MiB retained metadata
budget includes nested suite names. Oversized extension metadata and GNU sparse
framing fail instead of making an unsupported complete-traversal claim.

The global census remains incomplete until these candidates are reviewed against
upstream build targets and runner descriptions, all inherited/parameterized/
generated instances are resolved, fixtures are identified, and the IntelliJ
mirror omissions are reconciled with canonical pinned sources. Product deferrals
remain unported; discovery cannot justify `not_applicable` classifications.
