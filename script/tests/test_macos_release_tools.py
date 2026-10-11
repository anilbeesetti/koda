import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


loader = importlib.machinery.SourceFileLoader(
    "macos_release_tools", str(Path(__file__).resolve().parents[1] / "macos-release-tools")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
tools = importlib.util.module_from_spec(spec)
loader.exec_module(tools)


class ReleaseToolTests(unittest.TestCase):
    def test_real_cached_executables_in_path_with_spaces(self):
        with tempfile.TemporaryDirectory(prefix="release tools ") as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            for name, _, expected, _ in tools.TOOLS:
                binary = root / "bin" / name
                binary.write_text(f"#!/bin/sh\nprintf '%s\\n' '{expected}'\n")
                binary.chmod(0o755)
            # Run actual executable validation while preventing any Cargo builds.
            original_run = tools.subprocess.run

            def checked_run(command, **kwargs):
                self.assertNotEqual(command[0], "cargo")
                return original_run(command, **kwargs)

            with patch.object(tools.subprocess, "run", side_effect=checked_run):
                tools.install_tools(root)

    def test_cached_tools_are_verified_without_installation(self):
        with patch.object(tools, "valid_tool", return_value=True), patch.object(
            tools.subprocess, "run"
        ) as run:
            tools.install_tools(Path("cache"))
        run.assert_not_called()

    def test_missing_tools_install_exact_revision_and_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(tools, "valid_tool", side_effect=[False, True, False, True]), patch.object(
                tools.subprocess, "run"
            ) as run:
                tools.install_tools(root)
            commands = [call.args[0] for call in run.call_args_list]
            self.assertEqual(commands[0][-2:], ["--rev", tools.CARGO_BUNDLE_REVISION])
            self.assertNotIn("--branch", commands[0])
            self.assertEqual(commands[1][-1], "cargo-about@0.8.2")
            self.assertEqual(commands[0][2:5], ["--force", "--root", str(root.resolve())])

    def test_install_through_symlink_root_uses_canonical_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            root = temporary / "canonical tools"
            root.mkdir()
            alias = temporary / "tool alias"
            alias.symlink_to(root, target_is_directory=True)
            calls = temporary / "cargo-calls.jsonl"
            cargo = temporary / "cargo"
            cargo.write_text(f"#!{sys.executable}\n" + '''
import json, os, pathlib, sys
arguments = sys.argv[1:]
with open(os.environ['TOOL_TEST_CALLS'], 'a') as output:
    output.write(json.dumps(arguments) + '\\n')
root = pathlib.Path(arguments[arguments.index('--root') + 1])
name, version = ('cargo-about', 'cargo-about 0.8.2') if 'cargo-about@0.8.2' in arguments else ('cargo-bundle', 'cargo-bundle v0.6.1-zed')
(root / 'bin').mkdir(exist_ok=True)
binary = root / 'bin' / name
binary.write_text("#!/bin/sh\\nprintf '%s\\\\n' '" + version + "'\\n")
binary.chmod(0o755)
''')
            cargo.chmod(0o755)
            with patch.dict(os.environ, PATH=f"{temporary}:{os.environ['PATH']}", TOOL_TEST_CALLS=str(calls)):
                tools.install_tools(alias)
            commands = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertEqual(len(commands), 2)
            for command in commands:
                self.assertEqual(command[command.index("--root") + 1], str(root.resolve()))
            self.assertTrue(tools.valid_tool(alias / "bin" / "cargo-bundle", "--help", "cargo-bundle v0.6.1-zed"))
            self.assertTrue(tools.valid_tool(alias / "bin" / "cargo-about", "--version", "cargo-about 0.8.2"))

    def test_wrong_version_cannot_be_published_to_cache(self):
        with patch.object(tools, "valid_tool", return_value=False), patch.object(
            tools.subprocess, "run"
        ):
            with self.assertRaisesRegex(RuntimeError, "did not report"):
                tools.install_tools(Path("cache"))

    def test_failed_install_is_not_silently_accepted(self):
        with patch.object(tools, "valid_tool", return_value=False), patch.object(
            tools.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "cargo")
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                tools.install_tools(Path("cache"))

    def test_validation_rejects_version_prefix_and_failed_process(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "tool"
            binary.touch()
            for stdout, returncode in [("cargo-about 0.8.20\n", 0), ("cargo-about 0.8.2\n", 1)]:
                with self.subTest(stdout=stdout, returncode=returncode), patch.object(
                    tools.subprocess, "run", return_value=subprocess.CompletedProcess([], returncode, stdout)
                ):
                    self.assertFalse(tools.valid_tool(binary, "--version", "cargo-about 0.8.2"))

    def test_nonexecutable_cache_entry_requires_reinstallation(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "tool"
            binary.write_text("incomplete cached binary")
            self.assertFalse(tools.valid_tool(binary, "--version", "cargo-about 0.8.2"))


if __name__ == "__main__":
    unittest.main()
