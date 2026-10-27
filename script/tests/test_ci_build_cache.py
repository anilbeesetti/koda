import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import call, patch


SCRIPT = Path(__file__).resolve().parents[1] / "ci-build-cache"
loader = importlib.machinery.SourceFileLoader("ci_build_cache", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
cache = importlib.util.module_from_spec(spec)
loader.exec_module(cache)


class BuildCacheTests(unittest.TestCase):
    def test_missing_cache_does_not_invoke_cargo(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(cache.subprocess, "run") as run:
                cache.clean_workspace(Path(temporary) / "missing")
            run.assert_not_called()

    def test_cleanup_is_locked_workspace_only_and_failure_is_fatal(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary)
            with patch.dict(os.environ, {"CARGO": "fixture-cargo"}):
                with patch.object(cache.subprocess, "run") as run:
                    cache.clean_workspace(target, "wasm32-wasip2")
                    run.assert_called_once_with([
                        "fixture-cargo", "clean", "--locked", "--workspace",
                        "--profile", "dev", "--target-dir", str(target),
                        "--target", "wasm32-wasip2",
                    ], check=True)
                with patch.object(cache.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "cargo")):
                    with self.assertRaises(subprocess.CalledProcessError):
                        cache.clean_workspace(target)

    def test_extension_cleanup_includes_host_and_wasi_artifacts(self):
        with patch.object(sys, "argv", [str(SCRIPT), "--target-dir", "cache", "--include-test-extension"]):
            with patch.object(cache, "clean_workspace") as clean:
                cache.main()
        extension = Path("extensions/test-extension/target")
        self.assertEqual(clean.call_args_list, [call(Path("cache")), call(extension), call(extension, "wasm32-wasip2")])

    def test_workflow_invalidates_after_restore_before_compilation(self):
        workflow = (SCRIPT.parents[1] / ".github/workflows/fork_ci.yml").read_text()
        linux, macos = workflow.split("  linux:\n", 1)[1].split("  macos:\n", 1)
        for section, restore, compile_step in (
            (linux, "Restore Linux build cache", "Build test extension"),
            (macos, "Restore macOS build cache", "Check macOS Zed application"),
        ):
            guard = section.index("name: Invalidate restored workspace artifacts")
            self.assertLess(section.index(f"name: {restore}"), guard)
            self.assertLess(guard, section.index(f"name: {compile_step}"))
            guard_step = section[guard:].split("\n      - ", 1)[0]
            self.assertNotIn("continue-on-error", guard_step)
            self.assertNotIn("if:", guard_step)

    def test_real_cargo_rebuilds_stale_local_api_and_preserves_external_artifacts(self):
        cargo = subprocess.check_output(["rustup", "which", "cargo"], text=True).strip()
        rustc = subprocess.check_output(["rustup", "which", "rustc"], text=True).strip()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            dependency, workspace = root / "dependency", root / "workspace"
            (dependency / "src").mkdir(parents=True)
            (dependency / "Cargo.toml").write_text('[package]\nname="ci_cached_dependency"\nversion="0.1.0"\nedition="2021"\n')
            (dependency / "src/lib.rs").write_text("pub fn value() -> u32 { 7 }")

            def git(*arguments):
                return subprocess.check_output(["git", "-C", str(dependency), *arguments], stderr=subprocess.DEVNULL, text=True).strip()

            git("init", "-q")
            git("add", ".")
            git("-c", "user.name=CI cache test", "-c", "user.email=cache@example.invalid", "commit", "-qm", "fixture")
            revision = git("rev-parse", "HEAD")
            workspace.mkdir()
            (workspace / "Cargo.toml").write_text('[workspace]\nmembers=["api","app"]\nresolver="2"\n')
            for name in ("api", "app"):
                package = workspace / name
                (package / "src").mkdir(parents=True)
                (package / "Cargo.toml").write_text(f'[package]\nname="ci_cache_{name}"\nversion="0.1.0"\nedition="2021"\n')
            with (workspace / "api/Cargo.toml").open("a") as manifest:
                manifest.write(f'\n[dependencies]\nci_cached_dependency={{git="{dependency.as_uri()}",rev="{revision}"}}\n')
            with (workspace / "app/Cargo.toml").open("a") as manifest:
                manifest.write('\n[dependencies]\nci_cache_api={path="../api"}\n')
            api_source = workspace / "api/src/lib.rs"
            app_source = workspace / "app/src/main.rs"
            api_source.write_text("pub fn old_api() -> u32 { ci_cached_dependency::value() }")
            app_source.write_text('fn main() { println!("{}", ci_cache_api::old_api()); }')
            original_mtime = api_source.stat().st_mtime_ns
            target = workspace / "target"
            extension_target = workspace / "extensions/test-extension/target"
            env = {**os.environ, "CARGO": cargo, "RUSTC": rustc,
                   "CARGO_HOME": str(root / "cargo"), "CARGO_TARGET_DIR": str(target),
                   "CARGO_NET_OFFLINE": "false"}

            def run(*arguments, check=True):
                result = subprocess.run([cargo, *arguments], cwd=workspace, env=env, check=False, capture_output=True, text=True)
                if check:
                    self.assertEqual(result.returncode, 0, result.stderr)
                return result

            run("generate-lockfile")
            env["CARGO_NET_OFFLINE"] = "true"
            run("build", "--offline", "--locked", "-p", "ci_cache_app")
            run("build", "--offline", "--locked", "-p", "ci_cache_api", "--target-dir", str(extension_target))
            run("build", "--offline", "--locked", "-p", "ci_cache_api", "--target-dir", str(extension_target), "--target", "wasm32-wasip2")
            local_artifacts = list(target.glob("debug/deps/libci_cache_api-*.rlib"))
            local_artifacts += list(extension_target.glob("debug/deps/libci_cache_api-*.rlib"))
            local_artifacts += list(extension_target.glob("wasm32-wasip2/debug/deps/libci_cache_api-*.rlib"))
            self.assertEqual(len(local_artifacts), 3)
            external_artifacts = list(target.glob("debug/deps/libci_cached_dependency-*.rlib"))
            external_artifacts += list(extension_target.glob("debug/deps/libci_cached_dependency-*.rlib"))
            external_artifacts += list(extension_target.glob("wasm32-wasip2/debug/deps/libci_cached_dependency-*.rlib"))
            self.assertEqual(len(external_artifacts), 3)
            external_mtimes = {path: path.stat().st_mtime_ns for path in external_artifacts}

            api_source.write_text("pub fn new_api() -> u32 { ci_cached_dependency::value() }")
            os.utime(api_source, ns=(original_mtime, original_mtime))
            app_source.write_text('fn main() { println!("{}", ci_cache_api::new_api()); }')
            stale = run("build", "--offline", "--locked", "-p", "ci_cache_app", check=False)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("cannot find function `new_api`", stale.stderr)

            subprocess.run([sys.executable, str(SCRIPT), "--target-dir", str(target), "--include-test-extension"],
                           cwd=workspace, env=env, check=True, capture_output=True, text=True)
            self.assertTrue(all(not path.exists() for path in local_artifacts))
            run("build", "--offline", "--locked", "-p", "ci_cache_app")
            for path, mtime in external_mtimes.items():
                self.assertEqual(path.stat().st_mtime_ns, mtime)


if __name__ == "__main__":
    unittest.main()
