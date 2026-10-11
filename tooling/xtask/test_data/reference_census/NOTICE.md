# Reference declaration fixtures

The three complete files under `idea/` retain their original bytes and Apache
2.0 copyright/license headers. They originate from AOSP `tools/adt/idea` revision
`a84efec3ba9542d9bfa1255103f0dc94833a3796` in the pinned stable release.
`provenance.json` records each source path, file hash and archive hash.

These files test source discovery only. Retaining them and identifying declared
methods does not port their behavior, establish runner expansion, or change any
canonical parity status. The original upstream tests and assertions are intact.

`jetbrains-android/archive-prefix.tar` retains the first 1,536 decompressed bytes
of the pinned Apache 2.0 JetBrains Android source archive at revision
`132bc7c3cf52598117590637d00e81b929444bde`. It contains the original Git global PAX
comment header/payload/padding and wrapper directory header, with no source file
payload. Its separate provenance file records the archive hash, byte offset and
fixture hash. Tests append synthetic entries without changing the original
prefix. This archive-format regression provides no Android behavior parity credit.
