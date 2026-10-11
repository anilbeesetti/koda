"""Exercise conservative selection using actual Git histories and worktrees."""

import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "ci-test-selection"
loader = importlib.machinery.SourceFileLoader("ci_test_selection", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
selection = importlib.util.module_from_spec(spec)
loader.exec_module(selection)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="CI selection ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "repository with spaces"
        self.root.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "CI Test")
        self.git("config", "user.email", "ci-test@example.invalid")
        packages = [
            self.package("leaf-package", "crates/leaf directory"),
            self.package("optional-consumer", "crates/optional", [
                self.dependency("leaf-package", "crates/leaf directory", optional=True)]),
            self.package("dev-consumer", "crates/dev", [
                self.dependency("optional-consumer", "crates/optional", kind="dev")]),
            self.package("target-consumer", "crates/target", [
                self.dependency("dev-consumer", "crates/dev", target="cfg(target_os = \"macos\")")]),
            self.package("unrelated", "crates/unrelated"),
            self.package("nested", "crates/optional/nested"),
        ]
        self.metadata = {"workspace_members": [p["id"] for p in packages], "packages": packages}
        self.write("Cargo.toml", "[workspace]\n")
        self.write("README.md", "Initial documentation\n")
        self.write("assets/themes/theme.json", "{}")
        for package in packages:
            directory = Path(package["manifest_path"]).parent.relative_to(self.root)
            self.write(str(directory / "Cargo.toml"), f'[package]\nname = "{package["name"]}"\nversion = "0.1.0"\n')
            self.write(str(directory / "src/lib.rs"), "pub fn initial() {}\n")
        self.base = self.commit()

    def package(self, name, directory, dependencies=()):
        return {"id": name + "@0.1.0", "name": name,
                "manifest_path": str(self.root / directory / "Cargo.toml"),
                "dependencies": list(dependencies)}

    def dependency(self, name, directory, **fields):
        return {"name": name, "path": str(self.root / directory), "source": None, **fields}

    def git(self, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=self.root, stderr=subprocess.PIPE).decode().strip()

    def write(self, path, contents="Changed\n"):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)

    def commit(self):
        self.git("add", "--all")
        self.git("commit", "-qm", "Test revision")
        return self.git("rev-parse", "HEAD")

    def result(self, event="pull_request"):
        head = self.commit()
        return selection.create_plan(self.root, event, self.base, head, self.metadata)

    def test_real_git_diff_selects_transitive_dev_optional_and_target_dependents(self):
        self.write("crates/leaf directory/src/lib.rs")
        result = self.result()
        self.assertEqual(result["mode"], "selected")
        self.assertEqual(result["selected_packages"], ["dev-consumer", "leaf-package", "optional-consumer", "target-consumer"])
        self.assertNotIn("unrelated", result["test_filter"])
        self.assertIn("rdeps(leaf-package)", result["test_filter"])

    def test_registry_dependency_possibly_patched_to_workspace_is_included(self):
        consumer = next(p for p in self.metadata["packages"] if p["name"] == "unrelated")
        consumer["dependencies"] = [{"name": "leaf-package", "source": "registry+https://example.invalid", "path": None}]
        self.write("crates/leaf directory/src/lib.rs")
        self.assertIn("unrelated", self.result()["selected_packages"])

    def test_longest_package_directory_wins(self):
        self.write("crates/optional/nested/src/lib.rs")
        self.assertEqual(self.result()["selected_packages"], ["nested"])

    def test_added_source_and_newlines_in_filename_are_nul_safe(self):
        self.write("crates/unrelated/src/name with\nnewline.rs")
        self.assertEqual(self.result()["selected_packages"], ["unrelated"])

    def test_deleted_file_requires_full_suite(self):
        (self.root / "crates/unrelated/src/lib.rs").unlink()
        self.assertEqual(self.result()["mode"], "full")

    def test_rename_detected_as_deletion_requires_full_suite(self):
        (self.root / "crates/unrelated/src/lib.rs").rename(self.root / "crates/unrelated/src/renamed.rs")
        self.assertEqual(self.result()["mode"], "full")

    def test_root_workspace_toolchain_and_unknown_paths_require_full_suite(self):
        for path in ["Cargo.toml", "Cargo.lock", "crates/unrelated/Cargo.toml", "crates/unrelated/Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml", ".github/workflows/other.yml", "script/linux", "assets/themes/theme.json", "tooling/unknown/main.rs"]:
            with self.subTest(path=path):
                result = selection.select([("M", path)], self.metadata, self.root)
                self.assertEqual(result["mode"], "full")
                self.assertEqual(result["test_filter"], "")
                self.assertTrue(result["native_checks"])

    def test_extension_fixtures_and_non_rust_package_resources_require_full_suite(self):
        for path in ["extensions/test-extension/src/lib.rs", "extensions/example/extension.toml",
                     "crates/unrelated/src/fixture.json", "crates/unrelated/assets/font.ttf",
                     "crates/unrelated/src/injections.scm", "crates/unrelated/README.md"]:
            with self.subTest(path=path):
                self.assertEqual(selection.select([("M", path)], self.metadata, self.root)["mode"], "full")

    def test_synthetic_merge_tests_pr_changes_integrated_with_advanced_base(self):
        self.git("checkout", "-qb", "pr-feature", self.base)
        self.write("crates/optional/src/lib.rs", "pub fn feature() { main_new_api(); }\n")
        self.commit()
        self.git("checkout", "-qb", "advanced-base", self.base)
        self.write("crates/unrelated/src/lib.rs", "pub fn main_new_api() {}\n")
        advanced_base = self.commit()
        self.git("merge", "--no-ff", "pr-feature", "-m", "Synthetic integration merge")
        merge = self.git("rev-parse", "HEAD")
        changes = selection.changed_files(self.root, advanced_base, merge)
        self.assertEqual(changes, [("M", "crates/optional/src/lib.rs")])
        result = selection.create_plan(self.root, "pull_request", advanced_base, merge, self.metadata)
        self.assertEqual(result["selected_packages"], ["dev-consumer", "optional-consumer", "target-consumer"])
        self.assertNotIn("unrelated", result["selected_packages"])
        self.assertIn("main_new_api", (self.root / "crates/unrelated/src/lib.rs").read_text())

    def test_documentation_and_exact_ci_helpers_only_skip_native(self):
        for path in ["README.md", "docs/guide.md", ".github/workflows/fork_ci.yml", "script/ci-test-selection", "script/ci-run", "script/tests/test_ci_run.py", "script/tests/test_ci_test_selection.py"]:
            self.write(path)
        result = self.result()
        self.assertEqual(result["mode"], "checks-only")
        self.assertFalse(result["native_checks"])

    def test_documentation_like_unknown_code_does_not_skip_native(self):
        self.write("docs/example.rs")
        self.assertEqual(self.result()["mode"], "full")

    def test_exact_ci_only_mixed_with_source_still_tests_source(self):
        self.write("script/ci-run")
        self.write("crates/unrelated/src/lib.rs")
        self.assertEqual(self.result()["selected_packages"], ["unrelated"])

    def test_main_and_manual_always_full_even_documentation(self):
        self.write("README.md")
        head = self.commit()
        for event in ["push", "workflow_dispatch", "merge_group"]:
            with self.subTest(event=event):
                self.assertEqual(selection.create_plan(self.root, event, self.base, head, self.metadata)["mode"], "full")

    def test_checkout_or_git_failure_falls_back_full(self):
        self.write("crates/unrelated/src/lib.rs")
        self.commit()
        with patch("sys.stderr"):
            result = selection.create_plan(self.root, "pull_request", self.base, self.base, self.metadata)
            self.assertEqual(result["mode"], "full")
            result = selection.create_plan(self.root, "pull_request", "f" * 40, self.git("rev-parse", "HEAD"), self.metadata)
            self.assertEqual(result["mode"], "full")

    def test_metadata_failure_falls_back_full(self):
        self.write("crates/unrelated/src/lib.rs")
        head = self.commit()
        original = selection.command
        def unavailable(arguments, root):
            if arguments[0] == "cargo":
                raise FileNotFoundError("Cargo unavailable")
            return original(arguments, root)
        with patch.object(selection, "command", unavailable), patch("sys.stderr"):
            self.assertEqual(selection.create_plan(self.root, "pull_request", self.base, head)["mode"], "full")
        with patch("sys.stderr"):
            self.assertEqual(selection.create_plan(self.root, "pull_request", self.base, head, {})["mode"], "full")

    def test_checks_only_does_not_require_cargo(self):
        self.write("docs/guide.md")
        head = self.commit()
        original = selection.command
        def unavailable(arguments, root):
            self.assertNotEqual(arguments[0], "cargo")
            return original(arguments, root)
        with patch.object(selection, "command", unavailable):
            self.assertEqual(selection.create_plan(self.root, "pull_request", self.base, head)["mode"], "checks-only")

    def test_package_name_cannot_inject_filter_expression(self):
        self.metadata["packages"][0]["name"] = "leaf) | all("
        self.write("crates/leaf directory/src/lib.rs")
        with patch("sys.stderr"):
            self.assertEqual(self.result()["mode"], "full")

    def test_worktree_maps_metadata_directories_and_exact_head(self):
        other = Path(self.temporary.name) / "second worktree"
        self.git("worktree", "add", "--detach", str(other), self.base)
        metadata = json.loads(json.dumps(self.metadata).replace(str(self.root), str(other)))
        (other / "crates/unrelated/src/lib.rs").write_text("changed")
        subprocess.check_call(["git", "add", "--all"], cwd=other)
        subprocess.check_call(["git", "commit", "-qm", "Worktree change"], cwd=other)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=other).decode().strip()
        self.assertEqual(selection.create_plan(other, "pull_request", self.base, head, metadata)["selected_packages"], ["unrelated"])

    def test_cli_outputs_json_and_safe_github_outputs(self):
        self.write("docs/guide.md")
        head = self.commit()
        output = Path(self.temporary.name) / "github output"
        result = subprocess.check_output([str(SCRIPT), "--base", self.base, "--head", head, "--github-output", str(output)], cwd=self.root)
        self.assertEqual(json.loads(result)["mode"], "checks-only")
        self.assertIn("native_checks=false\n", output.read_text())
        self.assertIn("selected_packages=[]\n", output.read_text())
        self.assertEqual(len(output.read_text().splitlines()), 5)


if __name__ == "__main__":
    unittest.main()
