This supplemental Gradle adapter regression exercises the complete production
`project_model.gradle` init script. The fixture plugin registers the real
`com.android.library` plugin ID, supplies a physical library/variant and direct
Java/manifest roots, and throws from `getPluginVersion` or `getVersion`. Each
getter is tested with both `IllegalStateException` and `NoClassDefFoundError`.
It has no Android SDK, external dependency, or original-reference parity claim.

The Rust example prepares a new attempt directory, preserving all previous
attempts, and verifies the actual complete Gradle logs and exit files:

```sh
cargo run --locked -p android_tools --example gradle_optional_sdk_getters -- prepare /absolute/new-attempt
```

Run each case from `new-attempt/project` with the admitted Gradle installation,
the prepared `production-project_model.gradle` init script,
`--no-daemon --offline --max-workers=1 --console=plain --no-configuration-cache`,
`-PfixtureGetter=getPluginVersion|getVersion`,
`-PfixtureFailure=exception|linkage`, and `kodaAndroidProjectModel`.
Capture complete combined stdout/stderr to `<getter>-<failure>.log` and write
the actual process exit code plus newline to `<getter>-<failure>.exit`.
The runtime owner must enforce 420 seconds and 16 MiB per case, retain failures,
and verify cleanup of its own Gradle processes. Run the four cases serially.
The Rust harness starts no processes and changes no shared SDK or Gradle cache.

```sh
cargo run --locked -p android_tools --example gradle_optional_sdk_getters -- verify /absolute/new-attempt
```

Verification requires every case to have actually succeeded and checks the core
model, both physical source roots, null unavailable AGP version, original SDK
getter class/message/provenance, capability failure, and source-provenance
diagnostic. A missing case, nonzero exit, oversized log, malformed wire, or
unexpected root fails verification. No case is skipped.
