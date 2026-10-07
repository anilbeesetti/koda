The AGP version parser and preview ordering in `generated_artifacts.rs` follow
`sdk-common/src/main/java/com/android/ide/common/repository/AgpVersion.kt` from
the Android Open Source Project `tools/base` repository, revision
`4a5d2ec9571e2021fc9a4621e24a195f966ee6dd`.

The complete original source is retained verbatim as `AgpVersion.kt`, with its
Apache License 2.0 copyright and license header. Its SHA-256 is
`e689e8d93c9b78f77bcfb94c41b97c5a0bb825537e576e84a30f5ffbbed0edfb`.

The new Rust regression cases exercise the pinned numeric bounds, preview
ordering, and historical preview padding. They are supplemental source-derived
checks and do not claim to port an original upstream test suite.
