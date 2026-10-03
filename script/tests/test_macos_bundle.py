"""Check release metadata passed to cargo-bundle and the macOS CLI version check."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest


REPOSITORY = Path(__file__).resolve().parents[2]


class BundleMetadataTests(unittest.TestCase):
    def test_selected_binary_receives_channel_metadata(self):
        manifest = REPOSITORY / "crates/zed/Cargo.toml"
        metadata = tomllib.loads(manifest.read_text())["package"]["metadata"]
        for channel in ("stable", "nightly", "dev", "preview"):
            for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
                with self.subTest(channel=channel, target=target), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    scripts = root / "script"
                    scripts.mkdir()
                    shutil.copyfile(REPOSITORY / "script/bundle-mac", scripts / "bundle-mac")
                    (scripts / "lib").mkdir()
                    (scripts / "lib/blob-store.sh").write_text("")
                    licenses = scripts / "generate-licenses"
                    licenses.write_text("#!/bin/sh\nexit 0\n")
                    licenses.chmod(0o755)
                    package = root / "crates/zed"
                    package.mkdir(parents=True)
                    shutil.copyfile(manifest, package / "Cargo.toml")
                    (package / "RELEASE_CHANNEL").write_text(channel)
                    tools = root / "tools"
                    tools.mkdir()
                    cargo = tools / "cargo"
                    cargo.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys, tomllib
if '--help' in sys.argv:
    print('cargo-bundle v0.6.1-zed')
elif sys.argv[1] == 'bundle':
    metadata = tomllib.loads(pathlib.Path('Cargo.toml').read_text())['package']['metadata']
    binary = sys.argv[sys.argv.index('--bin') + 1]
    # The pinned cargo-bundle uses only bundle.bin[--bin], with empty defaults if absent.
    settings = metadata.get('bundle', {}).get('bin', {}).get(binary, {})
    pathlib.Path(os.environ['BUNDLE_TEST_SNAPSHOT']).write_text(json.dumps({
        'settings': settings, 'arguments': sys.argv[1:],
        'skip_build': os.environ.get('CARGO_BUNDLE_SKIP_BUILD'),
    }))
    sys.exit(23)  # Stop before invoking native macOS packaging commands.
""")
                    cargo.chmod(0o755)
                    for name, body in (("rustc", "echo 'host: aarch64-apple-darwin'"),
                                       ("rustup", "exit 0")):
                        tool = tools / name
                        tool.write_text(f"#!/bin/sh\n{body}\n")
                        tool.chmod(0o755)
                    snapshot = root / "bundle.json"
                    environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                                       BUNDLE_TEST_SNAPSHOT=str(snapshot), ZED_BUNDLE_TIMINGS="0",
                                       ZED_RELEASE_INPUTS_MANIFEST="")
                    result = subprocess.run(["bash", str(scripts / "bundle-mac"), target],
                                            cwd=root, env=environment, capture_output=True,
                                            text=True, timeout=10)
                    self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
                    bundled = json.loads(snapshot.read_text())
                    self.assertEqual(bundled["settings"], metadata[f"bundle-{channel}"])
                    self.assertEqual(bundled["skip_build"], "true")
                    self.assertEqual(bundled["arguments"],
                                     ["bundle", "--release", "--bin", "koda", "--target", target,
                                      "--select-workspace-root"])


class CliVersionVerificationTests(unittest.TestCase):
    def test_verifies_numeric_bundle_version_for_stable_and_nightly(self):
        workflow = (REPOSITORY / ".github/workflows/macos_release.yml").read_text()
        command, = [line.strip() for line in workflow.splitlines()
                    if line.strip().startswith('"$app/Contents/MacOS/cli" --version')]
        version = "2026.10.03.03.55"
        for channel in ("stable", "nightly"):
            for cli_version in (version, "2026.10.02.03.55"):
                with self.subTest(channel=channel, cli_version=cli_version), tempfile.TemporaryDirectory() as directory:
                    app = Path(directory) / ("Koda.app" if channel == "stable" else "Koda Nightly.app")
                    cli = app / "Contents/MacOS/cli"
                    cli.parent.mkdir(parents=True)
                    cli.write_text('#!/bin/sh\nprintf "Koda %s – %s\\n" "$CLI_TEST_VERSION" "$app"\n')
                    cli.chmod(0o755)
                    tag = version if channel == "stable" else f"nightly-{version}"
                    result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", command],
                                            env=dict(os.environ, app=str(app), ZED_BUNDLE_VERSION=version,
                                                     ZED_RELEASE_VERSION=tag, CLI_TEST_VERSION=cli_version),
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0 if cli_version == version else 1,
                                     result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
