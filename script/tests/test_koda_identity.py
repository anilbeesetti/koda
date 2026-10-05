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
            shutil.copyfile(REPOSITORY / "crates/zed/resources/KodaDev.icns", resources / "KodaDev.icns")
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
            binary.write_text('''#!/bin/sh
case "$*" in
  *--system-specs*) printf 'Koda: v0.1 (%s)\\n' "${KODA_TEST_BINARY_CHANNEL:-Koda Dev}"; exit 0 ;;
esac
printf '%s\\n' "$@" > "$KODA_TEST_ARGUMENTS"
''')
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
            bundle = root / "target/android-ide/dev/debug/Koda Dev.app"
            metadata = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
            self.assertEqual(metadata["CFBundleIdentifier"], "dev.anilbeesetti.koda-dev")
            self.assertEqual(metadata["CFBundleExecutable"], "koda")
            self.assertEqual(metadata["CFBundleIconFile"], "KodaDev.icns")
            self.assertEqual((bundle / "Contents/Resources/KodaDev.icns").read_bytes(),
                             (resources / "KodaDev.icns").read_bytes())
            self.assertEqual(arguments.read_text().splitlines(),
                             ["--user-data-dir", str(profile), str(root / "project")])
            self.assertFalse(json.loads((profile / "config/settings.json").read_text())["auto_update"])
            release_binary = root / "target/release/koda"
            release_binary.parent.mkdir()
            shutil.copyfile(binary, release_binary)
            release_binary.chmod(0o755)
            environment["KODA_TEST_BINARY_CHANNEL"] = "Koda Nightly"
            result = subprocess.run(["bash", str(root / "script/android-ide"), "--release",
                                     "--skip-build", "--profile", str(profile)],
                                    env=environment, text=True, capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Existing binary is not Koda Dev", result.stderr)
            self.assertFalse((root / "target/android-ide/dev/release/Koda Dev.app").exists())

    def test_icons_share_koda_branding(self):
        resources = REPOSITORY / "crates/zed/resources"
        for suffix in ("-preview",):
            for resolution in ("", "@2x"):
                self.assertEqual((resources / f"app-icon{suffix}{resolution}.png").read_bytes(),
                                 (resources / f"app-icon{resolution}.png").read_bytes())
            self.assertEqual((resources / f"windows/app-icon{suffix}.ico").read_bytes(),
                             (resources / "windows/app-icon.ico").read_bytes())
        for suffix, filename in (("-nightly", "KodaNightly.icns"), ("-dev", "KodaDev.icns")):
            for resolution in ("", "@2x"):
                self.assertNotEqual((resources / f"app-icon{suffix}{resolution}.png").read_bytes(),
                                    (resources / f"app-icon{resolution}.png").read_bytes())
            self.assertNotEqual((resources / f"windows/app-icon{suffix}.ico").read_bytes(),
                                (resources / "windows/app-icon.ico").read_bytes())
            self.check_icns(resources, filename, suffix)
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

    def test_app_icon_artwork_fills_an_opaque_canvas(self):
        resources = REPOSITORY / "crates/zed/resources"
        for channel in ("", "-nightly", "-dev", "-preview"):
            for resolution, size in (("", 512), ("@2x", 1024)):
                with self.subTest(channel=channel, resolution=resolution):
                    icon = (resources / f"app-icon{channel}{resolution}.png").read_bytes()
                    self.assertEqual(icon[:8], b"\x89PNG\r\n\x1a\n")
                    self.assertEqual(icon[12:16], b"IHDR")
                    width, height, depth, color_type, _, _, _ = struct.unpack(
                        ">IIBBBBB", icon[16:29]
                    )
                    self.assertEqual((width, height), (size, size))
                    self.assertEqual(depth, 8)
                    # Keep margins and corners opaque so the artwork reaches
                    # the system's icon mask without transparent padding.
                    self.assertEqual(color_type, 2)
                    offset = 8
                    while offset < len(icon):
                        length = struct.unpack(">I", icon[offset:offset + 4])[0]
                        self.assertNotEqual(icon[offset + 4:offset + 8], b"tRNS")
                        offset += length + 12

    @unittest.skipUnless(shutil.which("rustc"), "requires the Rust toolchain")
    def test_profile_defaults_and_explicit_channels_are_isolated(self):
        with tempfile.TemporaryDirectory(prefix="koda-channel-") as directory:
            binary = Path(directory) / "profile-build"
            subprocess.run(["rustc", "--edition=2024", str(REPOSITORY / "crates/paths/build.rs"),
                            "-o", str(binary)], check=True, capture_output=True)
            release_channel = (REPOSITORY / "crates/zed/RELEASE_CHANNEL").read_text().strip()
            profiles = {"dev": ("Koda Dev", "koda-dev"), "nightly": ("Koda Nightly", "koda-nightly"),
                        "stable": ("Koda", "koda"), "preview": ("Koda", "koda")}
            for debug, override in ((True, None), (False, None), (True, "nightly"),
                                    (True, "stable"), (False, "dev"), (False, "preview")):
                with self.subTest(debug=debug, override=override):
                    environment = dict(os.environ)
                    environment.pop("ZED_RELEASE_CHANNEL", None)
                    environment.pop("CARGO_CFG_DEBUG_ASSERTIONS", None)
                    if debug:
                        environment["CARGO_CFG_DEBUG_ASSERTIONS"] = ""
                    if override is not None:
                        environment["ZED_RELEASE_CHANNEL"] = override
                    expected_channel = override or ("dev" if debug else release_channel)
                    name, profile = profiles[expected_channel]
                    output = subprocess.check_output([str(binary)], cwd=REPOSITORY / "crates/paths",
                                                     env=environment, text=True)
                    self.assertIn(f"cargo::rustc-env=KODA_PROFILE_NAME={name}\n", output)
                    self.assertIn(f"cargo::rustc-env=KODA_PROFILE_DIRECTORY={profile}\n", output)

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
    def test_dev_uninstall_preserves_nightly_and_stable(self):
        with tempfile.TemporaryDirectory(prefix="koda-dev-uninstall-") as directory:
            root = Path(directory)
            profile = root / "profile"
            preserved = [".local/koda.app/bin/koda", ".local/koda-nightly.app/keep",
                         ".config/koda/settings.json", ".config/koda-nightly/settings.json",
                         ".local/share/koda/db/0-stable/keep",
                         ".local/share/koda-nightly/db/0-nightly/keep", ".koda_server/keep"]
            removed = [".local/koda-dev.app/keep", ".config/koda-dev/settings.json",
                       ".local/share/koda-dev/db/0-dev/keep", ".local/share/koda-dev/logs/keep"]
            for filename in preserved + removed:
                path = profile / filename
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(filename)
            command = profile / ".local/bin/koda"
            command.parent.mkdir(parents=True)
            command.symlink_to(profile / ".local/koda.app/bin/koda")
            script = root / "uninstall.sh"
            script.write_text((REPOSITORY / "script/uninstall.sh").read_text().replace("$HOME", str(profile)))
            result = subprocess.run(["sh", str(script)], env=dict(os.environ, KODA_CHANNEL="dev"),
                                    input="n\n", text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("Koda Dev preferences", result.stdout)
            for filename in preserved:
                self.assertEqual((profile / filename).read_text(), filename)
            for filename in removed:
                self.assertFalse((profile / filename).exists())
            self.assertTrue(command.is_symlink())

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
                        profile_directory = {"nightly": "koda-nightly", "dev": "koda-dev"}.get(channel, "koda")
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
