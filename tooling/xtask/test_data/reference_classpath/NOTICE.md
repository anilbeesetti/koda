The complete original TestGroup.java and TestGroupTest.kt files are retained from
AOSP platform/tools/base at 4a5d2ec9571e2021fc9a4621e24a195f966ee6dd.
Their copyright notices and Apache License 2.0 headers are unchanged. The full
license is in LICENSE-APACHE-2.0.txt; byte identities are in provenance.json.

The Rust manifest Class-Path reader adapts addManifestClassPath and both original
test behaviors. Tests construct actual wrapper ZIP files, manifest attributes,
and dummy.txt entries; referenced JARs remain empty regular files as upstream.
The two original cases retain all five assertions. Supplemental Rust tests cover
bounds and malformed inputs without claiming additional upstream case parity.

This helper reports ordered existing paths and missing-reference diagnostics.
It does not discover configured classes, runner descriptions, parameters, or
suite membership. No runtime success is claimed until actual Rust checks exist.
