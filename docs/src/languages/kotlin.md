---
title: Kotlin
description: "Configure Kotlin language support in Koda, including language servers, formatting, and debugging."
---

# Kotlin

Kotlin syntax and language integration in Koda are provided by the community-maintained [Kotlin extension](https://github.com/zed-extensions/kotlin).
Report issues to: [https://github.com/zed-extensions/kotlin/issues](https://github.com/zed-extensions/kotlin/issues)

- Tree-sitter: [fwcd/tree-sitter-kotlin](https://github.com/fwcd/tree-sitter-kotlin)
- Language Server: [kotlin/kotlin-lsp](https://github.com/kotlin/kotlin-lsp)
- Alternate Language Server: [fwcd/kotlin-language-server](https://github.com/fwcd/kotlin-language-server)

## Kotlin LSP

[Kotlin LSP](https://github.com/kotlin/kotlin-lsp) is the official language server for Kotlin, built by JetBrains.

On Apple Silicon macOS, Koda selects its pinned Kotlin server with Android patches
(`263.4702.0+android-8`) instead of the extension's server download. At startup,
Koda checks this profile's installation and installs a missing or outdated runtime
automatically after Java setup finishes. No project needs to be open. The
installer currently requires a full JDK 21, Python 3.12+ and Apple's Command Line
Tools. Open {#action android::Setup} to cancel installation, inspect an error or
retry. Cancelled and failed installations preserve the previous runtime.
After a successful Android project sync, Koda configures the default Kotlin
backend for the selected build variant automatically. Explicit custom server
settings are preserved.

The Kotlin server uses its private Java 25 runtime. Android builds and Compose
previews use the Java 21 selected in setup. Server caches are isolated by Koda
profile and project.

The patched Android importer includes the Android target of Kotlin Multiplatform
modules using `com.android.kotlin.multiplatform.library` or `com.android.library`
with `androidTarget()`. It imports the selected Android compilation and its
common sources; this does not add IDE support for the project's other targets.
Common and Android `expect`/`actual` declarations can still produce overload
ambiguity diagnostics. After updating the runtime, Koda refreshes eligible open
projects and their Kotlin language servers. You can also use Kotlin **Install /
repair** in **Customize**, **Advanced tools**.

On other platforms, the Kotlin extension still downloads and updates its default
server. Koda's patched runtime installer currently supports Apple Silicon macOS
only.

If you want to use a manually installed version, set the path to its launcher in
your `settings.json`:

```json [settings]
{
  "lsp": {
    "kotlin-lsp": {
      "binary": {
        "path": "path/to/kotlin-lsp.sh",
        "arguments": ["--stdio"]
      }
    }
  }
}
```

Note that the `kotlin-lsp.sh` script expects to be run from within the unzipped release zip file, and should not be moved elsewhere.

## Kotlin Language Server

The community-maintained [Kotlin Language Server](https://github.com/fwcd/kotlin-language-server) can be used instead of Kotlin LSP by explicitly enabling it in your `settings.json`:

```json [settings]
{
  "languages": {
    "Kotlin": {
      "language_servers": ["kotlin-language-server", "!kotlin-lsp", "..."]
    }
  }
}
```

### Configuration

Workspace configuration options can be passed to the language server via lsp
settings in `settings.json`.

The full list of lsp `settings` can be found
[here](https://github.com/fwcd/kotlin-language-server/blob/main/server/src/main/kotlin/org/javacs/kt/Configuration.kt)
under `class Configuration` and initialization_options under `class InitializationOptions`.

#### JVM Target

The following example changes the JVM target from `default` (which is 1.8) to
`17`:

```json [settings]
{
  "lsp": {
    "kotlin-language-server": {
      "settings": {
        "compiler": {
          "jvm": {
            "target": "17"
          }
        }
      }
    }
  }
}
```

#### JAVA_HOME

To use a specific java installation, just specify the `JAVA_HOME` environment variable with:

```json [settings]
{
  "lsp": {
    "kotlin-language-server": {
      "binary": {
        "env": {
          "JAVA_HOME": "/Users/whatever/Applications/Work/Android Studio.app/Contents/jbr/Contents/Home"
        }
      }
    }
  }
}
```
