# Android reference declaration fixtures

The files under `sources` are complete, unmodified source files from the pinned
Android Open Source Project and JetBrains Android archives. Their original
copyright and license headers are retained where present. The upstream module
license is Apache 2.0; see `LICENSE-APACHE-2.0.txt`. `selection.json` records each
source repository identity, revision, original path, byte count and SHA-256.
The pinned repository/archive identities are in the project reference manifest.

These fixtures exercise declaration discovery. Their upstream IDE/build test
behaviors are not ported here, and the discovery tests earn no behavioral parity
credit. A source declaration is not a runner-expanded test instance. AOSP and
JetBrains identities remain separate even when their file bytes are identical.
