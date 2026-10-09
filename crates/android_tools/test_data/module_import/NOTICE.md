Module import presentation and transactional Kotlin settings

Copyright 2000-2026 JetBrains s.r.o. and contributors.
Copyright (C) 2024 The Android Open Source Project.
Licensed under the Apache License, Version 2.0. The full license is retained at
`../import_facts/LICENSE-APACHE`; original source and fixture headers are unchanged.

The source manifest records exact original paths, revisions, sizes and SHA-256.
IntelliJ Community is pinned to `b75ab523e6adbe1d26112219729eacbcfd24daa0`.
AOSP tools/adt/idea is pinned to `a84efec3ba9542d9bfa1255103f0dc94833a3796`.
Naming follows the retained `../import_facts/references` Gradle resolver and
ExternalProject model builder. Legacy filename escaping follows the complete
PathUtilRt source retained here, including Java whitespace distinctions.

Seven KotlinFacetBridgeTest settings/state methods are adapted to immutable Rust
publication and explicit transactional settings edits. IntelliJ WorkspaceModel,
FacetManager, object identity and Swing facet-tab assertions are classified
individually in reference-cases.json. No method receives passing parity credit
until its actual Rust test runs successfully at the reviewed source.

All 39 original KOTLIN_KAPT fixture files are copied byte-for-byte. They have not
been prepared, run, or replaced with synthetic captures. Its exact reference
membership assertion remains unported until the original fixture preparation,
version runner and actual official getter execution can be exercised.

strict-projection-template.json is a supplemental synthetic protocol fixture.
It proves no JVM getter availability, fixture import, runtime artifact identity,
KAPT behavior, model version compatibility, native UI or performance result.
The strict kotlin_import_facts module, tests and fixtures are copied unchanged
from task commit c1bd317ca39b923ed19dad51937369443cb66c62. Historical results for
that task do not count as validation of this candidate.

This task provides Rust naming, publication/settings, a strict snapshot/request
projection adapter and the imported empty-label boundary. Production discovery,
official getter execution, real version probes, complete Kotlin/MPP settings
import and visible tree publication are dependent work. The reduced transport
decoder cannot establish automatic Enabled or Disabled membership. Missing,
unsupported and MPP facts remain unknown; no IntelliJ JVM importer is hosted.
