import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "manage-android-tools"
MANAGER = runpy.run_path(str(SCRIPT))
INSTALLER = runpy.run_path(str(SCRIPT.with_name("install-android-kotlin")))


class ManagedToolTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def build(self, staging):
        distribution = staging / "distribution"
        distribution.mkdir()
        (distribution / "PreviewBridge.class").write_bytes(b"bridge")
        (distribution / "renderer.jar").write_bytes(b"renderer")
        return distribution

    def install(self, recipe="recipe", build=None):
        return MANAGER["provision"](self.root, "preview", recipe, build or self.build, "PreviewBridge.class")

    def test_clean_install_restart_validation_and_compatible_rollback(self):
        first = self.install()
        second = self.install()
        active = MANAGER["read_manifest"](self.root / "preview.json")
        self.assertEqual(active["slot"], second["slot"])
        self.assertEqual(active["previous"]["slot"], first["slot"])
        MANAGER["validate"](self.root, active, "recipe")
        MANAGER["validate"](self.root, active["previous"], "recipe")
        MANAGER["publish"](self.root / "preview.json", active["previous"])
        self.assertEqual(MANAGER["read_manifest"](self.root / "preview.json")["slot"], first["slot"])

    def test_failed_download_or_build_preserves_active_runtime(self):
        first = self.install()
        def broken(staging):
            self.build(staging)
            raise RuntimeError("network unavailable")
        with self.assertRaisesRegex(RuntimeError, "network unavailable"):
            self.install(build=broken)
        self.assertEqual(MANAGER["read_manifest"](self.root / "preview.json")["slot"], first["slot"])
        self.assertEqual(list(self.root.glob(".stage-*")), [])

    def test_corrupt_runtime_and_removed_files_are_detected_and_repaired(self):
        first = self.install()
        runtime = self.root / first["slot"]
        (runtime / "renderer.jar").write_bytes(b"corrupt")
        with self.assertRaisesRegex(RuntimeError, "integrity"):
            MANAGER["validate"](self.root, first, "recipe")
        repaired = self.install()
        MANAGER["validate"](self.root, repaired, "recipe")
        (self.root / repaired["slot"] / "PreviewBridge.class").unlink()
        with self.assertRaisesRegex(RuntimeError, "integrity"):
            MANAGER["validate"](self.root, repaired, "recipe")

    def test_upgrade_requires_current_recipe_and_preserves_previous(self):
        first = self.install()
        with self.assertRaisesRegex(RuntimeError, "changed"):
            MANAGER["validate"](self.root, first, "new recipe")
        second = self.install("new recipe")
        MANAGER["validate"](self.root, second, "new recipe")
        self.assertEqual(second["previous"]["slot"], first["slot"])
        with self.assertRaisesRegex(RuntimeError, "changed"):
            MANAGER["validate"](self.root, second["previous"], "new recipe")

    def test_debugger_revision_upgrade_builds_a_new_slot_without_relabeling_old_runtime(self):
        def build_revision(revision):
            def build(staging):
                distribution = staging / "adapter"
                (distribution / "bin").mkdir(parents=True)
                binary = distribution / "bin/kotlin-debug-adapter"
                binary.write_text("launcher")
                binary.chmod(0o755)
                (distribution / ".revision").write_text(revision)
                return distribution
            return build
        provision = MANAGER["provision"]
        old = provision(self.root, "debugger", "recipe-1", build_revision("upstream+android-1"), "bin/kotlin-debug-adapter")
        new = provision(self.root, "debugger", "recipe-2", build_revision("upstream+android-2"), "bin/kotlin-debug-adapter")
        self.assertEqual((self.root / old["slot"] / ".revision").read_text(), "upstream+android-1")
        self.assertEqual((self.root / new["slot"] / ".revision").read_text(), "upstream+android-2")
        self.assertEqual(new["previous"]["slot"], old["slot"])
        MANAGER["validate"](self.root, new, "recipe-2", "debugger")
        with self.assertRaisesRegex(RuntimeError, "changed"):
            MANAGER["validate"](self.root, old, "recipe-2", "debugger")

    def test_concurrent_windows_are_rejected_without_mutation(self):
        first = self.install()
        with (self.root / ".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaisesRegex(RuntimeError, "Another Koda window"):
                self.install()
        self.assertEqual(MANAGER["read_manifest"](self.root / "preview.json")["slot"], first["slot"])

    def test_interrupted_process_recovers_staging_on_next_install(self):
        first = self.install()
        code = """import runpy, sys, time
from pathlib import Path
manager = runpy.run_path(sys.argv[1])
def build(staging):
    (staging / 'entered').touch()
    print('ready', flush=True)
    time.sleep(60)
manager['provision'](Path(sys.argv[2]), 'preview', 'recipe', build, 'PreviewBridge.class')
"""
        process = subprocess.Popen([sys.executable, "-c", code, str(SCRIPT), str(self.root)], stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(process.stdout.readline().strip(), "ready")
            process.kill()
            process.wait(timeout=5)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            process.stdout.close()
        self.assertEqual(MANAGER["read_manifest"](self.root / "preview.json")["slot"], first["slot"])
        self.assertTrue(list(self.root.glob(".stage-*")))
        self.install()
        self.assertFalse(list(self.root.glob(".stage-*")))

    def test_setup_failure_stops_descendants_holding_output_open(self):
        copied = self.root / "manager"
        copied.write_bytes(SCRIPT.read_bytes())
        installer = self.root / "install-android-preview"
        installer.write_text("""import subprocess, sys
def main():
    subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
    raise RuntimeError('simulated build failure')
""")
        wrapper = """import pathlib, platform, runpy, sys
from unittest.mock import patch
platform.system = lambda: 'Darwin'
platform.machine = lambda: 'arm64'
sys.version_info = (3, 12, 0)
original = pathlib.Path.is_file
with patch.object(pathlib.Path, 'is_file', lambda path: True if str(path) == '/usr/bin/ditto' else original(path)):
    sys.argv = [sys.argv[1], 'install', 'preview', sys.argv[2], 'recipe']
    runpy.run_path(sys.argv[0], run_name='__main__')
"""
        process = subprocess.Popen([sys.executable, "-c", wrapper, str(copied), str(self.root / "profile")],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
        try:
            _, error = process.communicate(timeout=5)
            self.assertEqual(process.returncode, -9)
            self.assertIn("simulated build failure", error)
        finally:
            if process.poll() is None:
                import signal
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=5)

    def test_manifest_and_runtime_links_cannot_escape(self):
        outside = self.root / "outside"
        outside.write_text("user file")
        def escaped(staging):
            directory = self.build(staging)
            (directory / "link").symlink_to(outside)
            return directory
        with self.assertRaisesRegex(RuntimeError, "escapes"):
            self.install(build=escaped)
        (self.root / "preview.json").symlink_to(outside)
        with self.assertRaisesRegex(RuntimeError, "Unsafe runtime manifest"):
            self.install()
        self.assertEqual(outside.read_text(), "user file")

    def test_inventory_is_bounded(self):
        globals_ = MANAGER["inventory"].__globals__
        with patch.dict(globals_, MAX_FILES=1):
            with self.assertRaisesRegex(RuntimeError, "file count"):
                self.install()

        with patch.dict(globals_, MAX_BYTES=1):
            with self.assertRaisesRegex(RuntimeError, "GiB"):
                self.install()

    def test_corrupt_manifest_shapes_are_preserved_and_repaired(self):
        for content in ("{broken", "[]", "null", '{"schema":1}'):
            (self.root / "preview.json").write_text(content)
            manifest = self.install()
            MANAGER["validate"](self.root, manifest, "recipe", "preview")
        self.assertEqual(len(list(self.root.glob("preview.corrupt-*.json"))), 4)
        (self.root / "preview.json").write_text('{"schema":99}')
        with self.assertRaisesRegex(RuntimeError, "Unsupported"):
            self.install()

    def test_expected_tool_entrypoint_and_slot_are_enforced(self):
        manifest = self.install()
        for key, value in (("tool", "kotlin"), ("entrypoint", "./PreviewBridge.class"), ("slot", "install-../escape")):
            modified = dict(manifest, **{key: value})
            with self.assertRaises(RuntimeError):
                MANAGER["validate"](self.root, modified, "recipe", "preview")

    def test_interrupted_download_recovery_and_storage_budget(self):
        downloads = self.root / "downloads"
        downloads.mkdir()
        (downloads / ".download-interrupted").write_bytes(b"partial")
        (downloads / "verified-cache").write_bytes(b"cache")
        self.install()
        self.assertFalse((downloads / ".download-interrupted").exists())
        self.assertTrue((downloads / "verified-cache").exists())
        with self.assertRaisesRegex(RuntimeError, "storage exceeds"):
            MANAGER["enforce_budget"](self.root, maximum=1)
        (downloads / ".download-unsafe").symlink_to(downloads / "verified-cache")
        with self.assertRaisesRegex(RuntimeError, "unexpected download recovery"):
            self.install()


class DownloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.payload = b"verified payload"
        self.checksum = hashlib.sha256(self.payload).hexdigest()
        env = patch.dict(os.environ, {"KODA_TOOL_DOWNLOADS": str(self.root), "KODA_TOOL_OFFLINE": "0"})
        env.start()
        self.addCleanup(env.stop)

    def response(self, *args, **kwargs):
        response = io.BytesIO(self.payload)
        response.url = "https://example.test/artifact"
        return response

    def download(self):
        return INSTALLER["download"]("artifact", "https://example.test/artifact", self.checksum)

    def test_verified_cache_and_offline_repair(self):
        with patch("urllib.request.urlopen", side_effect=self.response):
            self.download()
        with patch.dict(os.environ, KODA_TOOL_OFFLINE="1"), patch("urllib.request.urlopen", side_effect=AssertionError("network")):
            self.download()
            (self.root / "artifact").write_bytes(b"broken")
            with self.assertRaisesRegex(RuntimeError, "Offline"):
                self.download()
        with patch("urllib.request.urlopen", side_effect=self.response):
            self.download()

    def test_failed_transfer_checksum_and_limits_leave_no_partial_archive(self):
        with patch("urllib.request.urlopen", side_effect=OSError("download failed")):
            with self.assertRaisesRegex(OSError, "download failed"):
                self.download()
        with patch("urllib.request.urlopen", side_effect=self.response):
            with patch.dict(INSTALLER["download"].__globals__, MAX_DOWNLOAD_BYTES=1):
                with self.assertRaisesRegex(RuntimeError, "limit"):
                    self.download()
            with patch.dict(INSTALLER["download"].__globals__, DOWNLOAD_SECONDS=-1):
                with self.assertRaisesRegex(RuntimeError, "limit"):
                    self.download()
            self.checksum = "0" * 64
            with self.assertRaisesRegex(RuntimeError, "Checksum mismatch"):
                self.download()
        self.assertEqual(list(self.root.iterdir()), [])

    def test_reject_insecure_sources_and_redirects(self):
        with self.assertRaisesRegex(RuntimeError, "HTTPS"):
            INSTALLER["download"]("artifact", "http://example.test/artifact", self.checksum)
        def insecure(*args, **kwargs):
            response = self.response()
            response.url = "http://example.test/artifact"
            return response
        with patch("urllib.request.urlopen", side_effect=insecure):
            with self.assertRaisesRegex(RuntimeError, "insecure"):
                self.download()


class DebuggerVerificationTests(unittest.TestCase):
    def test_transitive_artifacts_use_pinned_sha256_without_trust_exceptions(self):
        debugger = runpy.run_path(str(SCRIPT.with_name("install-android-debugger")))
        metadata = SCRIPT.with_name("android-debugger-verification.xml").read_bytes()
        self.assertEqual(hashlib.sha256(metadata).hexdigest(), debugger["VERIFICATION_SHA256"])
        root = ET.fromstring(metadata)
        namespace = {"v": "https://schema.gradle.org/dependency-verification"}
        self.assertEqual(root.find("v:configuration/v:verify-metadata", namespace).text, "true")
        self.assertIsNone(root.find("v:configuration/v:trusted-artifacts", namespace))
        artifacts = root.findall("v:components/v:component/v:artifact", namespace)
        self.assertGreater(len(artifacts), 300)
        for artifact in artifacts:
            checksums = artifact.findall("v:sha256", namespace)
            self.assertTrue(checksums, artifact.attrib)
            self.assertTrue(all(len(checksum.attrib["value"]) == 64 for checksum in checksums))


if __name__ == "__main__":
    unittest.main()
