# Android project-view policy reference sources

These files retain the original bytes and notices from the pinned Android Studio
Rabbit 1 source baseline (`studio-2026.2.1`). They are reference fixtures, not
JVM components used by the Rust IDE.

The AOSP Android source revision is
`a84efec3ba9542d9bfa1255103f0dc94833a3796`. `AndroidProjectViewTest.java`
contains fourteen declared tests. The Rust integration target
`project_view_preferences_reference` adapts nine policy cases and every one of
their forty-six assertions. The five tree cases remain unported.

`AndroidProjectViewPane.java`, `AndroidProjectViewSettings.kt`, and
`AndroidProjectViewSettingsImpl.kt` define the policy, migration notification,
flag fallback, and change-event behavior. The matching platform revision is
`1141b43139d42a022d3b9f6d15e9d0b445bd302f`; its `VMOptions.java` establishes
last-value precedence and removal of all matching custom options.

All five sources are Apache-2.0. Their copyright notices remain unchanged, and
`LICENSE-APACHE` contains the complete license. Exact upstream paths, URLs,
revision hashes, retained-file checksums, assertion mappings, and adaptations
are recorded in `docs/android-studio/ports/project-view-policy.json`.

The Rust backend returns preferences and local notifications/events; the product
settings adapter, view selector, node rendering, and GPUI notification delivery
remain separate work. Preserving unrelated configuration bytes adds a guarantee
to the original migration behavior without changing any original assertion.
