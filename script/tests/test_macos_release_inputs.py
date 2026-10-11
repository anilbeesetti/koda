import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


loader = importlib.machinery.SourceFileLoader(
    "macos_release_inputs", str(Path(__file__).resolve().parents[1] / "macos-release-inputs")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
inputs = importlib.util.module_from_spec(spec)
loader.exec_module(inputs)

OLD_TIME = 1_700_000_000_000_000_123
NEW_TIME = OLD_TIME + 1_000_000_000


class ReleaseInputTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="release inputs ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.source = self.root / "src" / "lib.rs"
        self.source.parent.mkdir()
        self.source.write_text("pub fn value() -> usize { 1 }\n")
        (self.root / ".gitignore").write_text("/assets/licenses.md\n/target/\n")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        self.licenses = self.root / "assets" / "licenses.md"
        self.licenses.parent.mkdir()
        self.licenses.write_text("Strictly generated licenses\n")
        self.manifest = self.root / "target" / "inputs.json"
        self.pending = self.manifest.with_suffix(".pending")
        self.set_time(self.source, OLD_TIME)
        self.set_time(self.licenses, OLD_TIME)

    @staticmethod
    def set_time(path, value):
        os.utime(path, ns=(value, value))

    def test_only_identical_bytes_restore_and_metadata_is_consumed(self):
        inputs.capture(self.root, self.manifest)
        self.set_time(self.source, NEW_TIME)
        self.licenses.write_text("Changed licenses\n")
        self.set_time(self.licenses, NEW_TIME)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(self.source.stat().st_mtime_ns, OLD_TIME)
        self.assertEqual(self.licenses.stat().st_mtime_ns, NEW_TIME)
        self.assertFalse(self.manifest.exists())

    def test_generated_licenses_are_explicitly_included(self):
        inputs.capture(self.root, self.manifest)
        self.assertIn("assets/licenses.md", inputs.load_manifest(self.manifest))
        self.set_time(self.licenses, NEW_TIME)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(self.licenses.stat().st_mtime_ns, OLD_TIME)

    def test_changed_source_is_never_backdated(self):
        inputs.capture(self.root, self.manifest)
        self.source.write_text("pub fn value() -> usize { 2 }\n")
        self.set_time(self.source, NEW_TIME)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(self.source.stat().st_mtime_ns, NEW_TIME)

    def test_changed_file_mode_is_never_backdated_or_promoted(self):
        inputs.capture(self.root, self.manifest)
        inputs.capture(self.root, self.pending)
        self.source.chmod(0o755)
        self.set_time(self.source, NEW_TIME)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(self.source.stat().st_mtime_ns, NEW_TIME)
        inputs.validate(self.root, self.pending, self.manifest)
        self.assertNotIn("src/lib.rs", inputs.load_manifest(self.manifest))

    def test_successful_promotion_prunes_postbuild_changes(self):
        inputs.capture(self.root, self.pending)
        self.source.write_text("pub fn value() -> usize { 2 }\n")
        # A same-byte rewrite, like bundle-mac's Cargo.toml restore, retains
        # the original precompiler timestamp rather than the postbuild one.
        self.set_time(self.licenses, NEW_TIME)
        inputs.validate(self.root, self.pending, self.manifest)
        records = inputs.load_manifest(self.manifest)
        self.assertNotIn("src/lib.rs", records)
        self.assertEqual(records["assets/licenses.md"]["mtime_ns"], OLD_TIME)
        self.assertFalse(self.pending.exists())

    def test_failed_build_cannot_leave_previous_final_metadata(self):
        inputs.capture(self.root, self.manifest)
        inputs.restore(self.root, self.manifest)
        self.source.write_text("pub fn value() -> usize { 2 }\n")
        inputs.capture(self.root, self.pending)
        # The production workflow never promotes or saves a failed bundle.
        self.assertTrue(self.pending.exists())
        self.assertFalse(self.manifest.exists())

    def test_malformed_and_missing_manifests_degrade_to_cold(self):
        self.manifest.parent.mkdir()
        for content in ("broken json", '{"version":1,"files":[]}', '{"version":true,"files":{}}'):
            with self.subTest(content=content):
                self.manifest.write_text(content)
                self.set_time(self.source, NEW_TIME)
                inputs.restore(self.root, self.manifest)
                self.assertEqual(self.source.stat().st_mtime_ns, NEW_TIME)
                self.assertFalse(self.manifest.exists())
        inputs.restore(self.root, self.manifest)

    def test_invalid_timestamp_is_rejected_without_partial_restore(self):
        inputs.capture(self.root, self.manifest)
        data = json.loads(self.manifest.read_text())
        data["files"]["src/lib.rs"]["mtime_ns"] = 2**100
        self.manifest.write_text(json.dumps(data))
        self.set_time(self.licenses, NEW_TIME)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(self.licenses.stat().st_mtime_ns, NEW_TIME)
        self.assertFalse(self.manifest.exists())

    def test_malformed_permission_mode_degrades_to_cold(self):
        for mode in (None, True, -1, 0o10000):
            with self.subTest(mode=mode):
                inputs.capture(self.root, self.manifest)
                data = json.loads(self.manifest.read_text())
                data["files"]["src/lib.rs"]["mode"] = mode
                self.manifest.write_text(json.dumps(data))
                self.set_time(self.source, NEW_TIME)
                inputs.restore(self.root, self.manifest)
                self.assertEqual(self.source.stat().st_mtime_ns, NEW_TIME)
                self.assertFalse(self.manifest.exists())

    def test_source_symlink_is_never_followed(self):
        inputs.capture(self.root, self.manifest)
        external = self.root / "untracked.rs"
        external.write_bytes(self.source.read_bytes())
        self.set_time(external, NEW_TIME)
        self.source.unlink()
        self.source.symlink_to(external)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(external.stat().st_mtime_ns, NEW_TIME)

    def test_parent_symlink_is_never_followed(self):
        inputs.capture(self.root, self.manifest)
        external = self.root / "untracked-directory"
        external.mkdir()
        external_file = external / "lib.rs"
        external_file.write_bytes(self.source.read_bytes())
        self.set_time(external_file, NEW_TIME)
        self.source.unlink()
        self.source.parent.rmdir()
        self.source.parent.symlink_to(external, target_is_directory=True)
        inputs.restore(self.root, self.manifest)
        self.assertEqual(external_file.stat().st_mtime_ns, NEW_TIME)

    def test_untracked_git_and_escaping_paths_are_rejected(self):
        for name in ("../outside", "/tmp/outside", ".git/config", "untracked.rs", "src/../src/lib.rs"):
            with self.subTest(name=name):
                allowed = inputs.tracked_inputs(self.root)
                # Even an allowlist entry cannot escape or access Git metadata.
                if name != "untracked.rs":
                    allowed.add(name)
                self.assertIsNone(inputs.safe_file(self.root, name, allowed))


if __name__ == "__main__":
    unittest.main()
