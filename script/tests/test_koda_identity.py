"""Check Koda installation identity and coexistence without touching real profiles."""

import hashlib
import json
import os
import plistlib
import shutil
import struct
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
import xml.etree.ElementTree as ET


REPOSITORY = Path(__file__).resolve().parents[2]


class KodaIdentityTests(unittest.TestCase):
    def test_bundle_and_runtime_identifiers_match(self):
        metadata = tomllib.loads((REPOSITORY / "crates/zed/Cargo.toml").read_text())
        runtime = (REPOSITORY / "crates/release_channel/src/lib.rs").read_text()
        self.assertEqual(metadata["package"]["default-run"], "koda")
        self.assertIn("koda", [target["name"] for target in metadata["bin"]])
        for channel in ("stable", "dev", "nightly", "preview"):
            suffix = "" if channel == "stable" else f"-{channel}"
            bundle = metadata["package"]["metadata"][f"bundle-{channel}"]
            identifier = f"dev.anilbeesetti.koda{suffix}"
            self.assertEqual(bundle["identifier"], identifier)
            self.assertEqual(bundle["osx_url_schemes"], ["koda"])
            self.assertIn(f'"{identifier}"', runtime)
            self.assertTrue(bundle["name"].startswith("Koda"))
        desktop = (REPOSITORY / "crates/zed/resources/zed.desktop.in").read_text()
        self.assertIn("x-scheme-handler/koda;", desktop)
        self.assertNotIn("x-scheme-handler/zed;", desktop)

    def test_explorer_classes_are_distinct_and_match_the_dll(self):
        source = (REPOSITORY / "crates/explorer_command_injector/src/explorer_command_injector.rs").read_text()
        identifiers = set()
        for channel in ("stable", "dev", "nightly", "preview"):
            suffix = "" if channel == "stable" else f"-{channel}"
            filename = "AppxManifest.xml" if channel == "stable" else f"AppxManifest-{channel.title()}.xml"
            tree = ET.parse(REPOSITORY / "crates/explorer_command_injector" / filename)
            identity = tree.find("{*}Identity")
            self.assertEqual(identity.attrib["Name"], f"dev.anilbeesetti.koda{suffix}")
            classes = tree.findall(".//{*}Class")
            self.assertEqual(len(classes), 1)
            identifier = classes[0].attrib["Id"]
            self.assertNotIn(identifier, identifiers)
            identifiers.add(identifier)
            self.assertIn(identifier.replace("-", "_"), source)
            for verb in tree.findall(".//{*}Verb"):
                self.assertEqual(verb.attrib["Clsid"], identifier)

    def test_android_launcher_has_koda_identity_and_existing_icon(self):
        with tempfile.TemporaryDirectory(prefix="koda-launcher-") as directory:
            root = Path(directory)
            (root / "script").mkdir()
            shutil.copyfile(REPOSITORY / "script/android-ide", root / "script/android-ide")
            resources = root / "crates/zed/resources"
            resources.mkdir(parents=True)
            shutil.copyfile(REPOSITORY / "crates/zed/resources/KodaNightly.icns", resources / "KodaNightly.icns")
            tools = root / "tools"
            tools.mkdir()
            (tools / "uname").write_text("#!/bin/sh\necho Darwin\n")
            (tools / "uname").chmod(0o755)
            server = tools / "kotlin-server"
            server.write_text("#!/bin/sh\nexit 0\n")
            server.chmod(0o755)
            target = root / "target/debug"
            target.mkdir(parents=True)
            binary = target / "koda"
            binary.write_text('#!/bin/sh\nprintf \'%s\\n\' "$@" > "$KODA_TEST_ARGUMENTS"\n')
            binary.chmod(0o755)
            arguments = root / "arguments"
            profile = root / "profile"
            environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                               CARGO_TARGET_DIR=str(root / "target"),
                               ANDROID_IDE_OFFICIAL_KOTLIN_SERVER=str(server),
                               KODA_TEST_ARGUMENTS=str(arguments))
            result = subprocess.run(["bash", str(root / "script/android-ide"), "--skip-build",
                                     "--profile", str(profile), str(root / "project")],
                                    env=environment, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            bundle = root / "target/android-ide/nightly/debug/Koda Nightly.app"
            metadata = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
            self.assertEqual(metadata["CFBundleIdentifier"], "dev.anilbeesetti.koda-nightly")
            self.assertEqual(metadata["CFBundleExecutable"], "koda")
            self.assertEqual(metadata["CFBundleIconFile"], "KodaNightly.icns")
            self.assertEqual((bundle / "Contents/Resources/KodaNightly.icns").read_bytes(),
                             (resources / "KodaNightly.icns").read_bytes())
            self.assertEqual(arguments.read_text().splitlines(),
                             ["--user-data-dir", str(profile), str(root / "project")])
            self.assertFalse(json.loads((profile / "config/settings.json").read_text())["auto_update"])

    def test_icons_share_koda_branding(self):
        resources = REPOSITORY / "crates/zed/resources"
        for suffix in ("-preview", "-dev"):
            for resolution in ("", "@2x"):
                self.assertEqual((resources / f"app-icon{suffix}{resolution}.png").read_bytes(),
                                 (resources / f"app-icon{resolution}.png").read_bytes())
            self.assertEqual((resources / f"windows/app-icon{suffix}.ico").read_bytes(),
                             (resources / "windows/app-icon.ico").read_bytes())
        for resolution in ("", "@2x"):
            self.assertNotEqual((resources / f"app-icon-nightly{resolution}.png").read_bytes(),
                                (resources / f"app-icon{resolution}.png").read_bytes())
        self.assertNotEqual((resources / "windows/app-icon-nightly.ico").read_bytes(),
                            (resources / "windows/app-icon.ico").read_bytes())
        self.check_icns(resources, "KodaNightly.icns", "-nightly")
        icon = (resources / "Koda.icns").read_bytes()
        self.assertEqual((resources / "Document.icns").read_bytes(), icon)
        self.check_icns(resources, "Koda.icns", "")

    def check_icns(self, resources, filename, suffix):
        icon = (resources / filename).read_bytes()
        self.assertEqual(icon[:4], b"icns")
        self.assertEqual(struct.unpack(">I", icon[4:8])[0], len(icon))
        offset = 8
        for code, filename in ((b"ic09", f"app-icon{suffix}.png"), (b"ic10", f"app-icon{suffix}@2x.png")):
            self.assertEqual(icon[offset:offset + 4], code)
            length = struct.unpack(">I", icon[offset + 4:offset + 8])[0]
            self.assertEqual(icon[offset + 8:offset + length], (resources / filename).read_bytes())
            offset += length
        self.assertEqual(offset, len(icon))

    @unittest.skipUnless(sys.platform == "linux", "requires Linux uninstall tools")
    def test_nightly_uninstall_preserves_stable_profile_and_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            stable_files = [".local/koda.app/bin/koda", ".config/koda/settings.json",
                            ".local/share/koda/db/0-stable/keep", ".koda_server/keep"]
            for filename in stable_files:
                path = profile / filename
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("stable data")
            for filename in [".local/koda-nightly.app/keep", ".config/koda-nightly/settings.json",
                             ".local/share/koda-nightly/db/0-nightly/keep"]:
                path = profile / filename
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("nightly data")
            command = profile / ".local/bin/koda"
            command.parent.mkdir(parents=True)
            command.symlink_to(profile / ".local/koda.app/bin/koda")
            source = (REPOSITORY / "script/uninstall.sh").read_text()
            script = root / "uninstall.sh"
            script.write_text(source.replace("$HOME", str(profile)))
            result = subprocess.run(["sh", str(script)], env=dict(os.environ, KODA_CHANNEL="nightly"),
                                    input="n\n", text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            for filename in stable_files:
                self.assertEqual((profile / filename).read_text(), "stable data")
            self.assertTrue(command.is_symlink())
            self.assertTrue(command.exists())
            self.assertFalse((profile / ".local/koda-nightly.app").exists())
            self.assertFalse((profile / ".config/koda-nightly").exists())
            self.assertFalse((profile / ".local/share/koda-nightly").exists())

    @unittest.skipUnless(sys.platform == "linux", "requires Linux installation tools")
    def test_linux_install_and_uninstall_preserve_zed(self):
        for channel in ("stable", "dev", "nightly", "preview"):
            with self.subTest(channel=channel), tempfile.TemporaryDirectory(prefix="koda-coexist-") as directory:
                root = Path(directory)
                profile = root / "profile"
                suffix = "" if channel == "stable" else f"-{channel}"
                upstream = [
                    ".local/zed.app/keep", ".local/bin/zed", ".config/zed/settings.json",
                    ".local/share/zed/db/0-stable/keep", ".zed_server/keep",
                    ".local/share/applications/dev.zed.Zed.desktop",
                ]
                for filename in upstream:
                    path = profile / filename
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(f"original Zed: {filename}")
                before = {filename: hashlib.sha256((profile / filename).read_bytes()).digest() for filename in upstream}
                tools = root / "tools"
                tools.mkdir()
                (tools / "uname").write_text('#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
                (tools / "uname").chmod(0o755)
                app = root / f"koda{suffix}.app"
                (app / "bin").mkdir(parents=True)
                (app / "bin/koda").write_text("#!/bin/sh\nexit 0\n")
                (app / "bin/koda").chmod(0o755)
                (app / "share/applications").mkdir(parents=True)
                (app / f"share/applications/dev.anilbeesetti.koda{suffix}.desktop").write_text(
                    "[Desktop Entry]\nName=Koda\nExec=koda %U\nIcon=koda\n"
                )
                archive = root / "bundle.tar.gz"
                with tarfile.open(archive, "w:gz") as output:
                    output.add(app, arcname=app.name)
                environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                                   KODA_BUNDLE_PATH=str(archive), KODA_CHANNEL=channel,
                                   TMPDIR=str(root), SHELL="/bin/sh")
                for action in ("install", "uninstall"):
                    source = (REPOSITORY / f"script/{action}.sh").read_text()
                    # Substitute only the fixture's profile paths; keep the real HOME intact.
                    script = root / f"{action}.sh"
                    script.write_text(source.replace("$HOME", str(profile)))
                    if action == "uninstall":
                        profile_directory = "koda-nightly" if channel == "nightly" else "koda"
                        for filename in (f".config/{profile_directory}/settings.json", f".local/share/{profile_directory}/db/0-{channel}/keep", ".koda_server/keep"):
                            path = profile / filename
                            path.parent.mkdir(parents=True, exist_ok=True)
                            path.write_text("Koda data")
                    result = subprocess.run(["sh", str(script)], env=environment, input="n\n", text=True,
                                            capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    if action == "install":
                        self.assertTrue((profile / ".local/bin/koda").is_symlink())
                        self.assertTrue((profile / f".local/koda{suffix}.app").is_dir())
                    after = {filename: hashlib.sha256((profile / filename).read_bytes()).digest() for filename in upstream}
                    self.assertEqual(before, after)
                self.assertFalse((profile / f".local/koda{suffix}.app").exists())
                self.assertFalse((profile / f".config/{profile_directory}").exists())
                self.assertFalse((profile / ".koda_server").exists())


if __name__ == "__main__":
    unittest.main()
