import errno
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "ci-source-cache"
loader = importlib.machinery.SourceFileLoader("ci_source_cache", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
cache = importlib.util.module_from_spec(spec)
loader.exec_module(cache)


class SourceCacheTests(unittest.TestCase):
    def test_roundtrip_preserves_mtimes_modes_symlinks_and_existing_home(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, staged, target = (root / p for p in ("home-a", "portable", "home-b"))
            original = source / "registry/src/crate/source.rs"
            original.parent.mkdir(parents=True)
            original.write_text("source")
            original.chmod(0o644)
            os.utime(original, ns=(1_500_000_000_000_000_000,) * 2)
            (original.parent / "relative-link").symlink_to("source.rs")
            target.mkdir()
            (target / "config.toml").write_text("existing config")
            cache.move_sources(source, staged)
            # Repeat staging and merging as on a run with a previous snapshot.
            cache.move_sources(source, staged)
            cache.move_sources(staged, target)
            restored = target / "registry/src/crate/source.rs"
            self.assertEqual(restored.stat().st_mtime_ns, original.stat().st_mtime_ns)
            self.assertEqual(restored.stat().st_mode, original.stat().st_mode)
            self.assertEqual(os.readlink(restored.parent / "relative-link"), "source.rs")
            self.assertEqual((target / "config.toml").read_text(), "existing config")
            self.assertTrue(os.path.samefile(original, restored))

    def test_cross_volume_copy_preserves_timestamp_without_link(self):
        with tempfile.TemporaryDirectory() as temp:
            source, target = (Path(temp) / p for p in ("source", "target"))
            source.write_text("data")
            os.utime(source, ns=(1_500_000_000_000_000_000,) * 2)
            with patch.object(cache.os, "link", side_effect=OSError(errno.EXDEV, "cross volume")):
                cache.merge_tree(source, target)
            self.assertEqual(target.stat().st_mtime_ns, source.stat().st_mtime_ns)
            self.assertEqual(target.read_text(), "data")
            self.assertFalse(os.path.samefile(source, target))

    def test_prefetch_locks_both_manifests_for_all_required_targets(self):
        with patch.object(cache.subprocess, "run") as run:
            cache.prefetch()
        self.assertEqual(run.call_count, 2)
        for call, manifest in zip(run.call_args_list, ("Cargo.toml", "extensions/test-extension/Cargo.toml")):
            self.assertEqual(call.args[0][:5], ["cargo", "fetch", "--locked", "--manifest-path", manifest])
            self.assertEqual(call.args[0][5:], [arg for target in cache.TARGETS for arg in ("--target", target)])
            self.assertTrue(call.kwargs["check"])

    def test_portable_archive_supports_locked_offline_git_dependency(self):
        # Exercise actual Cargo's cached Git layout after archive relocation.
        cargo = subprocess.check_output(["rustup", "which", "cargo"], text=True).strip()
        rustc = subprocess.check_output(["rustup", "which", "rustc"], text=True).strip()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            dependency, project = root / "dependency", root / "project"
            for directory, name in ((dependency, "cached_dependency"), (project, "cache_consumer")):
                (directory / "src").mkdir(parents=True)
                (directory / "src/lib.rs").write_text("")
                (directory / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n')
            def git(*args):
                return subprocess.check_output(["git", "-C", str(dependency), *args], stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q")
            git("add", ".")
            git("-c", "user.name=CI cache test", "-c", "user.email=cache@example.invalid", "commit", "-qm", "fixture")
            revision = git("rev-parse", "HEAD")
            with (project / "Cargo.toml").open("a") as output:
                output.write(f'\n[dependencies]\ncached_dependency = {{ git = "{dependency.as_uri()}", rev = "{revision}" }}\n')
            first_home, restored_home = root / "cargo-a", root / "cargo-b"
            env = {**os.environ, "CARGO_HOME": str(first_home), "RUSTC": rustc}
            subprocess.run([cargo, "generate-lockfile", "--manifest-path", str(project / "Cargo.toml")], env=env, check=True, capture_output=True)
            staging = root / "archive-root/.ci-cache/cargo-sources"
            cache.move_sources(first_home, staging)
            archive = root / "sources.tar"
            with tarfile.open(archive, "w") as output:
                output.add(staging, arcname=".ci-cache/cargo-sources")
            moved_root = root / "other-os-workspace"
            moved_root.mkdir()
            with tarfile.open(archive) as source:
                self.assertTrue(all(not Path(member.name).is_absolute() for member in source))
                source.extractall(moved_root, filter="data")
            cache.move_sources(moved_root / ".ci-cache/cargo-sources", restored_home)
            shutil.rmtree(dependency)
            shutil.rmtree(first_home)
            subprocess.run([cargo, "metadata", "--offline", "--locked", "--format-version", "1", "--manifest-path", str(project / "Cargo.toml")],
                           env={**env, "CARGO_HOME": str(restored_home)}, check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
