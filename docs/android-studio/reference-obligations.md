# Reference obligation index

`cargo xtask android-reference-obligations` creates a deterministic, bounded JSONL
index of retained source discovery and conserves the checkout's exact canonical
manifest and parity ledger. It does not execute a JVM or reference runner, walk
an original reference archive, identify every effective test, or grant behavioral
parity credit. It always reports `census_complete: false`, unknown runtime case
counts, and `new_behavioral_parity_credit: 0`.

The existing canonical matrix remains the authority for reviewed reference IDs
and historical port/run statuses. This command never edits it. Source declaration
candidates are a separate inventory: helpers, providers, finalizers, abstract or
zero-method suites, inherited methods, non-JVM dispatch, configured build targets,
generated code, parameter tuples and custom runner descriptions remain unresolved.
Deferring a feature does not turn its tests into `not_applicable`: existing
`unported` rows survive verbatim, and undiscovered effective cases remain open
obligations. An existing `not_applicable` row must retain its own nonempty reason;
the exporter cannot decide that the reason makes a test inapplicable.

## Pinned inputs

The baseline is Android Studio Rabbit1 stable (`studio-2026.2.1`), with matching
IntelliJ build family `262.9437.185`. The three repository identities and complete
reference archive SHA-256 values are fixed in the command:

| Source | Revision | Archive SHA-256 |
| --- | --- | --- |
| AOSP tools/base | `4a5d2ec9571e2021fc9a4621e24a195f966ee6dd` | `0cbac492c1e2b47273fc9a1e2929caa6460660cf475cbd7773256f443cb30bb4` |
| AOSP tools/adt/idea | `a84efec3ba9542d9bfa1255103f0dc94833a3796` | `f15d0ee82d4719da82aa7d549dc4c3c14c180dac8112c4a1a73a65474419cf84` |
| JetBrains/android | `132bc7c3cf52598117590637d00e81b929444bde` | `d7bb6a155c077166a6aed54ddaaa7d1629cbee8c200ceced0bddc0bcedfd92e2` |

The mirror's `unverified_mirror` coverage is preserved. Equal source paths or
payload hashes across repositories do not establish mirror equivalence. Archive
hashes in the retained discovery summary are provenance; this exporter
independently authenticates the complete retained inventory files, not those
original archive bytes. An original archive traversal needs separate evidence.

An intake JSON file, independently bound by `--intake-sha256`, names exactly the
summary, retention receipt, three inventory roles for each source, and the exact
checkout manifest and ledger. Schema version 1 rejects unknown and duplicate
fields. A plain binding has `path`, `bytes`, and lowercase hexadecimal `sha256`.
A compressed binding has `file` (a plain binding), `decompressed_bytes`, and
`decompressed_sha256`.

```json
{
  "schema_version": 1,
  "summary": {
    "file": { "path": "summary.json.gz", "bytes": 1888, "sha256": "<64 lowercase hex characters>" },
    "decompressed_bytes": 5431,
    "decompressed_sha256": "<64 lowercase hex characters>"
  },
  "retention": { "path": "inventory-retention.json", "bytes": 7745, "sha256": "<64 lowercase hex characters>" },
  "sources": [
    {
      "id": "base",
      "members": { "file": { "path": "base-members.jsonl.gz", "bytes": 0, "sha256": "<exact gzip hash>" }, "decompressed_bytes": 0, "decompressed_sha256": "<exact JSONL hash>" },
      "physical_members": { "file": { "path": "base-physical-members.jsonl.gz", "bytes": 0, "sha256": "<exact gzip hash>" }, "decompressed_bytes": 0, "decompressed_sha256": "<exact JSONL hash>" },
      "candidates": { "file": { "path": "base-candidates.jsonl.gz", "bytes": 0, "sha256": "<exact gzip hash>" }, "decompressed_bytes": 0, "decompressed_sha256": "<exact JSONL hash>" }
    }
  ],
  "manifest": { "path": "docs/android-studio/reference-manifest.json", "bytes": 0, "sha256": "<exact checkout file hash>" },
  "ledger": { "path": "docs/android-studio/test-parity.json", "bytes": 0, "sha256": "<exact checkout file hash>" }
}
```

This abbreviated example is intentionally unusable: all lengths/hashes must be
actual values, and `idea` and `jetbrains-android` must also appear exactly once.
The retention receipt must agree with all ten gzip/decompressed bindings and
complete byte totals. Its retirement information is preserved as provenance.
Original raw files need not remain present after their documented retirement.
A new checkout matrix revision requires an explicitly regenerated intake binding;
a matrix from a different branch cannot silently replace it.

## Create and check

All paths must be absolute. Input files and input/output directory components must
be ordinary files/directories without symlinks. Output must be outside both the
checkout and retained input directory. Linux input reads use `O_NOATIME` and
`O_NOFOLLOW`; the account needs permission to use `O_NOATIME`. Unix physical
metadata comparisons include device, inode, mode, links, owner, group, rdev,
length, atime, mtime, ctime, block size and allocated blocks. No timestamp is
restored. On other Unix platforms this command has no no-atime open flag, so a
read that updates atime fails the immutable-metadata check; a portable no-atime
backend remains an open tooling task. Non-Unix comparisons are limited to length
and modification time and are not evidence of Unix physical-metadata parity.

```sh
cargo xtask android-reference-obligations \
  --repository /absolute/task-checkout \
  --input-root /absolute/retained-inventories \
  --intake /absolute/reviewed-intake.json \
  --intake-sha256 EXACT_INTAKE_SHA256 \
  --output /absolute/new-output
```

Add `--check` to regenerate into a fresh staging directory and require exactly
the existing artifact set and bytes. Create refuses any existing output. Check
never replaces or edits existing artifact files. Both modes use a fresh owned
staging directory; incomplete staging is retained with a failure diagnostic.
Create reserves its destination without replacement, copies authenticated staged
artifacts, and publishes `summary.json` last. A partial destination without that
summary is not a completed index. Successful operations remove only their own
staging directory. A failed invocation neither records a pass nor grants credit.

Each immutable input is checked when opened and closed. Before publishing the
summary the command again reads all fourteen complete bound input files, checks
their exact lengths and hashes, and compares their original physical metadata.
Descriptors are closed within the invocation. The command launches no subprocess
or background actor and uses no network, SDK, Gradle, Cargo or JVM internally.
The CLI must still be built and its tests run by the normal authorized build
workflow; source inspection alone supplies no passing-test evidence.

## Output and conservation

Each source has seven artifact streams: members, physical members, candidates,
classes, declarations, build obligations and unresolved obligations. Candidate
records preserve their original typed fields, annotations, reasons, raw JSONL
record SHA-256 and original inventory ordinal. All physical records retain their
ordinal, raw path bytes, header hash, payload hash, type and length. Whole member
records retain archive paths and unresolved link targets.

Identity hashes bind source, revision, original member path/hash, inventory
ordinal, role and class/method child ordinal. Same-line overloads and identical
mirror source paths remain separate records. A top-level helper with no owner
remains a declaration. Zero-method suite classes remain class obligations.

Two inventories per source are streamed into 64 bounded conservation shards.
Every candidate must match exactly one ordinary original member with the same
path, payload hash and length. Duplicate paths fail; they are not dropped.
Global PAX metadata uses a separate bounded namespace so an ordinary payload
named `pax_global_header` is not lost. Unclassified payloads and unresolved links
remain visible rather than acquiring automatic exclusion reasons.

`canonical.jsonl` preserves each complete reference test and ledger entry,
including fixtures, targets, historical run evidence, reasons and exact ID.
`canonical-matches.jsonl` preserves every candidate/ID match, including one-to-many
explicit case IDs and ambiguous same-line candidates. A join uses source, member
path/hash, suite candidate, method name and declaration line. Exactly one match
binds a source candidate; it does not prove runner membership or runtime case
expansion. Zero or multiple matches remain explicitly unresolved. Existing port
or run statuses are historical evidence only and are never promoted by a join.

`summary.json` includes full intake and retention provenance, original discovery
summary, exact canonical file bindings, conservation counts, unresolved join
counts and every non-summary artifact's full length/hash. Source counters are
checked against the complete retained discovery summary. The summary itself is
also authenticated by byte-for-byte create/check comparison.

## Bounds and failures

The parser consumes one candidate record at a time; it never accumulates every
candidate or method in a global vector. Defaults are fixed for this intake schema:

| Item | Limit |
| --- | --- |
| Intake, retention, decompressed summary | 1 MiB each |
| Canonical manifest or ledger | 16 MiB each; 16,384 rows |
| Compressed or decompressed inventory stream | 512 MiB each |
| Inventory records per stream | 2,000,000 |
| Input or emitted JSONL record, including newline | 16 MiB |
| Conservation shard | 8 MiB; 16,384 rows |
| Generated or checked individual artifact | 2 GiB |
| Global PAX metadata names per source | 8 |

Count arithmetic is checked. Canonical rows are a deliberately bounded reviewed
subset; a larger canonical matrix will require another explicit sharded task.
Large inventories, malformed JSON, unknown fields/statuses, unsafe paths,
duplicate IDs, mismatched source pins, incomplete retention, CRC failure,
concatenated/trailing gzip content, changed metadata or hashes, oversized or
unterminated records, ambiguous canonical joins and unsupported declaration
expansion cannot silently become complete tests. Integrity failures abort without
a complete summary; ambiguous joins and semantic expansion remain indexed open
obligations. Inputs are not truncated to satisfy a bound.

## Regression evidence and remaining work

`tooling/xtask/test_data/reference_obligations/` retains seventeen complete,
unmodified Apache-2.0 originals with their copyright/license notices: the
base TestGroup tests, helper, runner and build definition; IDEA runner/finalizer,
parameterized and Gradle module import sources; six complete Gradle sync test
sources; a zero-method suite; and its build definition. `provenance.json` binds
each complete original and four exact retained scanner-record slices to pinned
revisions, original paths, full hashes and original record ordinals.

The Rust regressions exercise discovery and integrity only. They distinguish the
nine DefaultVariants lexical candidates from its eight named `@Test` declarations
and preserve TestGroup's helper alongside its two named tests. They cover
zero-method suites, unresolved inheritance/providers/finalizers, overload/mirror
identity, explicit ambiguous cases, unchanged historical statuses, duplicate and
missing paths/IDs, unsupported encodings/non-JVM inputs, PAX metadata names,
corrupt hashes/pins/CRC, trailing gzip, schema/record/shard limits, symlinks,
deterministic create/check and changed/extra/missing/promoted output rejection.
Their transport gzip/physical headers are explicitly synthetic; they are not
upstream ZIP fixtures or original runner execution. Passing these regressions
would supply zero original behavioral parity credit.

The retained corpus authenticated for this task has 193,381 member records,
305,254 physical records, 108,311 candidate files and 342,071 method candidates
across the independently pinned repositories. These quantities describe source
discovery, not applicable tests or effective runtime cases. The dispatch checkout
has 96 canonical rows: 20 ported, 16 adapted and 60 unported, with 36 historical
passing statuses and 60 not-run statuses. Those statuses are unchanged; fresh
execution evidence must be collected separately. Other branches can legitimately
have different canonical row counts. Earlier aggregate module/runner counts do
not override these exact intake bindings.

The next expansion work must reconcile configured build targets/modules,
classpath-loaded suite classes, inheritance and runner selection, disabled or
conditional dispatch, parameter factories, generated inputs and non-JVM runners.
A regex or aggregate CI pass cannot resolve those obligations or substitute for
individual behavioral ports. The full applicable test matrix remains incomplete.
