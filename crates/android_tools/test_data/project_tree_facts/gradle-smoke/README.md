# Provider capture smoke

This fixture exercises evaluated two-dimension flavors, shared/nested asset
roots, an inactive DSL source set, late static assets, explicit generated
assets, custom resources/manifest and disabled release unit-test artifacts.
Its toolchain is public AGP 9.4.0 and the prepared Gradle/SDK/JDK environment.
It is a supplemental model-capture smoke, not any original Gradle tree test.

The asset task registration is adapted from the attributed original test setup;
the original complete test and Apache notice remain byte-identical in the
existing `project_tree` test data. No original assertion or fixture is replaced
by this smoke. Runtime model availability and actual names/order must be checked,
never inferred from this script or the configured directory names.
