This supplemental fixture runs the full production `project_context.gradle`
adapter against Gradle-owned extension objects. It distinguishes the public
`AndroidComponentsExtension.getPluginVersion().getVersion()` value from the
version object's descriptive `toString()`. Stable and preview cases require
the exact getter result; four failure cases require the original exception or
linkage error from either getter to remain unavailable. No Android SDK or
external dependency is required.

The fixture and Rust harness are authored regression coverage, not copied
Android Studio tests, an AGP implementation, or reference parity credit.
The separate official AGP positive-import probe must also pass.

Prepare a new directory with the Rust harness; existing attempts are retained:

```sh
cargo run --locked -p android_tools --example gradle_context_sdk_getters -- prepare /absolute/new-attempt
```

For every case listed in `cases-PREPARED.json`, run the admitted Gradle from
`project` with `--no-daemon --offline --max-workers=1 --console=plain
--no-configuration-cache`, `--init-script production-project_context.gradle`,
that case's exact properties, and `kodaProjectContext`. Capture the complete
combined output in the named `.log` and the actual exit code plus newline in
the named `.exit`. Run all six cases serially, enforcing 420 seconds and
16 MiB per case, and verify cleanup of the owned Gradle processes. The Rust
harness launches no processes and changes no shared SDK or Gradle cache.

```sh
cargo run --locked -p android_tools --example gradle_context_sdk_getters -- verify /absolute/new-attempt
```

Verification requires every actual Gradle exit to be zero, one complete context
record, both exact project owners, the full applied-plugin catalogue, exact
positive versions or original negative failure details, and Rust capabilities
that preserve explicit Sync while denying unavailable device/run/preview work.
Missing cases, malformed records, oversized logs, and failed assertions fail
verification. Nothing is skipped.
