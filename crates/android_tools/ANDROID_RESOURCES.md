# Android resource semantics

This feature consumes the selected project and component graph from the shared
Android project model (PR #50). The additive `resourceModels` metadata carries
AGP resource layers, transformed AAR resources/public lists, the selected SDK
resources and the merged-manifest output path. There is no second variant picker
or independently inferred Gradle graph. Older exports retain navigation, but
resource rename requires overlay metadata from a successful sync.

## Supported matrix

| Operation | Supported behavior | Boundary |
| --- | --- | --- |
| Resolution | Selected application/library/feature and component dependency roots; AGP outer layers in highest-priority order, equal-priority inner directories; custom resource directories; build types/flavors; one winner per qualifier, shadowed declarations and equal-priority conflicts | Qualifiers are separate candidates, not a simulated device configuration. Dependency namespace ambiguity is exposed as multiple candidates; library merge precedence is not inferred. |
| References | Kotlin/Java `R.type.name`, explicit namespace R, ordinary R imports and Kotlin R aliases; XML whole attribute/text `@type/name`, `@namespace:type/name`, `?attr/name`, and `@+id/name`; comments and ordinary strings are excluded | This is a conservative resource lexer, not JVM symbol binding. Static/member imports, interpolated strings, escaped syntax and data-binding expressions need language-server support. Uses across all catalogued variants are namespace-based candidates. |
| Definitions and hover | XML value resources, resource-file names (including `.9.png`, WebP and other non-XML files), layout-created IDs, selected project dependencies, transformed AARs and SDK resources; provenance/conflict hover | Text definition links cover XML. Ordinary Go to Definition opens PNG/JPEG/WebP/GIF through the workspace image viewer; split/hover image links are not implemented. Resource-file hover indexes other binary formats without opening them as text. Composite `R.styleable.Widget_attr` members and complete attr/style inheritance are not modeled. |
| Completions | Matching kind/prefix in XML and Kotlin/Java R expressions, including ordinary R imports/aliases; namespace/dependency candidates with provenance; project `<public>`/AAR public lists/framework public declarations filter foreign completion candidates | Incomplete results merge with existing server completions. Transitive-R alias completion and uncommon framework public-group forms remain incomplete. |
| Rename | Preview before applying; simple lowercase XML value names and layout/values IDs across catalogued source sets, qualifiers, Kotlin, Java, XML and `<public>` metadata; unsaved buffers; unsaved multi-buffer transactions using the editor's project-transaction/undo flow | Rejects file-resource moves, style/attr/styleable or normalized names, external/framework/generated declarations or references, collisions, equal-priority conflicts, shared namespaces, transitive R mode, ambiguous JVM bindings/imports, inheritance, anonymous subclasses, implicit/extension/context receivers and unrecognized callback scopes, templates/escapes, data binding and unqualified cross-namespace XML references. Refusal is intentional when complete ownership cannot be proved. |
| Manifest provenance | From an existing merged-manifest attribute, navigate to matching source declarations in selected manifest roots/project dependencies/AAR manifests; hover distinguishes equal and different source values | Does not run the merger or establish the merger winner. Placeholder expansion, tools directives, class-name normalization and merger-blame reports are not interpreted. The merged output must already exist. |
| Lifecycle | Model generation and trust checks; source/document snapshots; file membership, deletion and disk metadata; public-list metadata; failed-sync invalidation/recovery; edited-path canonical identity, symlink and writability checks; bounded scans/previews | Local trusted projects only. Scans are on demand, bounded to 20,000 files / 128 MiB text; SDK roots are scanned only for explicit `android` references. Rename review is bounded to 200 files / 2,000 edits. Unsaved external `public.txt` changes are not a supported visibility input. |

Repeated `@+id` declarations are legal and are not treated as duplicate-resource
conflicts. Dependency, SDK and generated resources are read-only for this rename
operation; that restriction does not globally lock ordinary editor buffers.

The resource model uses compile-classpath AAR transforms and the compile SDK's
`data/res`. It does not parse binary-only APK tables or a dependency R JAR as a
replacement for source resources. Generated/test R references resolve against
component namespaces and source scopes; generated files are never rewritten by
resource rename.

## Generic refactoring

Existing Kotlin/Java language-server rename and server-provided code actions
remain available for ordinary language symbols. No verified server contract in
this repository implements general move/package, change-signature or safe-delete
operations with Android resource ownership. This change adds none of those
operations and never substitutes project-wide text replacement for them. It is
not Android Studio refactoring parity or a visual layout editor.

## Verification

- `cargo test --locked -p android_tools`: parser, masking, aliases, metadata,
  legal repeated IDs, overlay conflicts and conservative rename guards.
- Project integration tests filtered by `test_android_resource`: selected model,
  custom/flavor/qualifier/dependency/framework/non-XML roots, completion and
  manifest candidates, preview/unsaved edits/undo, collisions/generated files,
  stale source/model/disk state, file addition/deletion, failed sync/recovery,
  metadata edits and safe refusals.
- Editor tests filtered by `resource_rename::tests`: read-only preview,
  apply/cancel/Escape/dismissal, keyboard focus and viewport bounds.
- `script/test-android-resources --gradle <path> --sdk <path> --agp 8.9.1|9.4.0`:
  real AGP custom/equal/variant/generated resource layers, transformed AndroidX
  AAR/public metadata, SDK paths, test resources and shared-model validation.

AGP probes need the matching Gradle version, a full JDK 21, API 35 and matching
build tools. They export resource metadata and do not run an emulator or assert
runtime device resource selection.
