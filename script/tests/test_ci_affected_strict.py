import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/affected_strict_ci.yml"
CLIPPY = ROOT / "script/clippy"
CLIPPY_SHA256 = "0a2fa62c186e472ed7eb70f066d93d5b1e880d9d7e2c40b81f2a9e15544cfdb8"
SOURCE_COMMIT = "6f33b8897c68f1d06acf4baa0aa12faa4afa6980"
SOURCE_TREE = "0c39463519b99d87a5bb0fd19dfe5411874e1a45"
CONTEXT_BRANCH = "android-studio-task/1-context-reference-strict-ci"
ACP_COMMAND = "./script/clippy --locked -p agent_servers -p acp_thread -p project -p gpui -p fs"
GPUI_COMMAND = "./script/clippy --locked -p android_tools -p android_ui -p command_palette -p fs -p gpui -p project -p title_bar -p workspace -p zed"
CONTEXT_COMMAND = GPUI_COMMAND + " -p project_panel"
REFERENCE_COMMAND = "./script/clippy --locked -p xtask"


def step_script(workflow, name):
    match = re.search(
        rf"^      - name: {re.escape(name)}\n(.*?)(?=^      - |\Z)",
        workflow,
        re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise ValueError(f"Workflow step not found: {name}")
    lines = match.group(1).splitlines()
    start = lines.index("        run: |") + 1
    script = []
    for line in lines[start:]:
        if line and not line.startswith("          "):
            break
        script.append(line[10:] if line else "")
    return "\n".join(script) + "\n"


class AffectedStrictTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / "script").mkdir()
        (self.root / "bin").mkdir()
        (self.root / "evidence").mkdir()
        (self.root / ".github/workflows/affected_strict_ci.yml").write_text(self.workflow)
        (self.root / "script/clippy").write_bytes(CLIPPY.read_bytes())
        (self.root / "script/clippy").chmod(0o755)
        (self.root / "Cargo.lock").write_text("fixture lock\n")
        (self.root / "rust-toolchain.toml").write_text("fixture toolchain\n")
        self.output = self.root / "github-output"
        self.state = self.root / "git-state.json"
        self.write_state()
        self.environment = dict(os.environ)
        for variable in (
            "CARGO", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
        ):
            self.environment.pop(variable, None)
        self.environment.update({
            "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"],
            "PYTHONDONTWRITEBYTECODE": "1",
            "GITHUB_OUTPUT": str(self.output),
            "CI_EVENT_NAME": "pull_request",
            "CI_HEAD_REF": CONTEXT_BRANCH,
            "CI_REPOSITORY": "anilbeesetti/koda",
            "EXPECTED_PROFILE": "",
            "PROFILE_FAMILY": "context-reference",
            "STRICT_PROFILE": "android-context",
            "STRICT_SOURCE_SHA": SOURCE_COMMIT,
            "STRICT_SOURCE_TREE": SOURCE_TREE,
            "STRICT_EVIDENCE": str(self.root / "evidence"),
            "MOCK_GIT_STATE": str(self.state),
            "MOCK_COMMAND_EXIT": "0",
            "MOCK_CAPTURE_EXIT": "0",
            "MOCK_SOURCE_CHANGE": "",
        })
        self.executable("bin/git", """
            import json
            import os
            from pathlib import Path
            import sys
            state = json.loads(Path(os.environ["MOCK_GIT_STATE"]).read_text())
            arguments = sys.argv[1:]
            if arguments == ["rev-parse", "HEAD"]:
                print(state["commit"])
            elif arguments == ["rev-parse", "HEAD^{tree}"]:
                print(state["tree"])
            elif arguments == ["rev-parse", "HEAD", "HEAD^{tree}"]:
                print(state["commit"])
                print(state["tree"])
            elif arguments == ["status", "--porcelain", "--untracked-files=no"]:
                sys.stdout.write(state["status"])
            else:
                raise SystemExit(97)
        """)

    def executable(self, relative, source):
        path = self.root / relative
        path.write_text(f"#!{sys.executable}\n" + textwrap.dedent(source).lstrip())
        path.chmod(0o755)

    def write_state(self, commit=SOURCE_COMMIT, tree=SOURCE_TREE, status=""):
        self.state.write_text(json.dumps({"commit": commit, "tree": tree, "status": status}))

    def request(self, profile="android-context", **changes):
        request = {
            "schema_version": 1,
            "profile": profile,
            "source_commit": SOURCE_COMMIT,
            "source_tree": SOURCE_TREE,
        }
        request.update(changes)
        (self.root / ".github/affected-strict-ci-request.json").write_text(json.dumps(request))

    def run_script(self, script, **environment):
        return subprocess.run(
            ["bash", "--noprofile", "--norc", "-euo", "pipefail", "-c", script],
            cwd=self.root,
            env={**self.environment, **environment},
            capture_output=True,
            text=True,
            timeout=15,
        )

    def run_step(self, name, **environment):
        return self.run_script(step_script(self.workflow, name), **environment)

    def outputs(self):
        return dict(line.split("=", 1) for line in self.output.read_text().splitlines())

    def prepare_worker(self):
        self.executable("bin/timeout", """
            import json
            import os
            from pathlib import Path
            import subprocess
            import sys
            arguments = sys.argv[1:]
            Path(os.environ["STRICT_EVIDENCE"], "timeout-arguments.json").write_text(
                json.dumps(arguments[:3])
            )
            if arguments[:3] != ["--signal=TERM", "--kill-after=30s", "165m"]:
                raise SystemExit(96)
            raise SystemExit(subprocess.run(arguments[3:], check=False).returncode)
        """)
        self.executable("bin/tee", """
            import json
            import os
            from pathlib import Path
            import resource
            import sys
            limit = resource.getrlimit(resource.RLIMIT_FSIZE)
            Path(os.environ["STRICT_EVIDENCE"], "capture-limit.json").write_text(json.dumps(limit))
            data = sys.stdin.buffer.read()
            Path(sys.argv[1]).write_bytes(data)
            sys.stdout.buffer.write(data)
            raise SystemExit(int(os.environ["MOCK_CAPTURE_EXIT"]))
        """)
        self.executable("script/clippy", """
            import json
            import os
            from pathlib import Path
            import sys
            Path(os.environ["STRICT_EVIDENCE"], "observed-command.json").write_text(
                json.dumps(["./script/clippy", *sys.argv[1:]])
            )
            if os.environ["MOCK_SOURCE_CHANGE"]:
                path = Path(os.environ["MOCK_GIT_STATE"])
                state = json.loads(path.read_text())
                state[os.environ["MOCK_SOURCE_CHANGE"]] = "changed"
                path.write_text(json.dumps(state))
            print("fixture compiler output")
            raise SystemExit(int(os.environ["MOCK_COMMAND_EXIT"]))
        """)

    def test_exact_branch_and_repository_eligibility(self):
        branches = {
            "android-studio-task/1-acp-isolated-strict-ci": ("acp", ""),
            "android-studio-task/1-software-adapter-isolated-strict-ci": ("gpui", ""),
            CONTEXT_BRANCH: ("", "context-reference"),
        }
        for branch, (profile, family) in branches.items():
            with self.subTest(branch=branch):
                self.output.unlink(missing_ok=True)
                result = self.run_step("Select exact task branch", CI_HEAD_REF=branch)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.outputs(), {
                    "eligible": "true", "expected_profile": profile, "profile_family": family,
                })

    def test_unmatched_branch_event_or_repository_is_ineligible(self):
        invalid = [
            {"CI_HEAD_REF": CONTEXT_BRANCH.upper()},
            {"CI_HEAD_REF": CONTEXT_BRANCH + "-other"},
            {"CI_HEAD_REF": "android-studio"},
            {"CI_HEAD_REF": CONTEXT_BRANCH + "\nworkflow_dispatch:"},
            {"CI_EVENT_NAME": "push"},
            {"CI_REPOSITORY": "Anilbeesetti/koda"},
            {"CI_REPOSITORY": "unrelated/koda"},
        ]
        for environment in invalid:
            with self.subTest(environment=environment):
                self.output.unlink(missing_ok=True)
                result = self.run_step("Select exact task branch", **environment)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.outputs(), {"eligible": "false"})

    def test_manual_dispatch_remains_eligible_in_literal_repository(self):
        result = self.run_step("Select exact task branch", CI_EVENT_NAME="workflow_dispatch")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.outputs(), {
            "eligible": "true", "expected_profile": "", "profile_family": "",
        })

    def test_new_profiles_and_immutable_source_outputs(self):
        for profile in ("android-context", "reference-tools"):
            with self.subTest(profile=profile):
                self.output.unlink(missing_ok=True)
                self.request(profile)
                result = self.run_step("Validate fixed profile and immutable source")
                self.assertEqual(result.returncode, 0, result.stderr)
                outputs = self.outputs()
                self.assertEqual(outputs["profile"], profile)
                self.assertEqual(outputs["source_commit"], SOURCE_COMMIT)
                self.assertEqual(outputs["source_tree"], SOURCE_TREE)
                self.assertEqual(outputs["control_commit"], SOURCE_COMMIT)
                self.assertEqual(outputs["control_tree"], SOURCE_TREE)
                self.assertEqual(outputs["request_sha256"], hashlib.sha256(
                    (self.root / ".github/affected-strict-ci-request.json").read_bytes()
                ).hexdigest())
                self.assertEqual(outputs["workflow_sha256"], hashlib.sha256(
                    self.workflow.encode()
                ).hexdigest())

    def test_checked_in_request_is_valid_for_new_branch(self):
        request = (ROOT / ".github/affected-strict-ci-request.json").read_bytes()
        (self.root / ".github/affected-strict-ci-request.json").write_bytes(request)
        result = self.run_step("Validate fixed profile and immutable source")
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = json.loads(request)
        self.assertEqual(self.outputs()["profile"], expected["profile"])
        self.assertEqual(self.outputs()["source_commit"], expected["source_commit"])
        self.assertEqual(self.outputs()["source_tree"], expected["source_tree"])

    def test_old_branch_profiles_remain_exact(self):
        for profile in ("acp", "gpui"):
            with self.subTest(profile=profile):
                self.output.unlink(missing_ok=True)
                self.request(profile)
                result = self.run_step(
                    "Validate fixed profile and immutable source",
                    EXPECTED_PROFILE=profile, PROFILE_FAMILY="",
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.outputs()["profile"], profile)
                self.request("android-context")
                result = self.run_step(
                    "Validate fixed profile and immutable source",
                    EXPECTED_PROFILE=profile, PROFILE_FAMILY="",
                )
                self.assertNotEqual(result.returncode, 0)

    def test_new_branch_rejects_old_profiles(self):
        for profile in ("acp", "gpui"):
            with self.subTest(profile=profile):
                self.request(profile)
                result = self.run_step("Validate fixed profile and immutable source")
                self.assertNotEqual(result.returncode, 0)

    def test_request_rejects_profile_package_and_command_injection(self):
        for profile in (
            "ANDROID-CONTEXT", "reference-tools -p zed", "xtask",
            "android-context; exit 0", "reference-tools\nselected=true", None, [],
        ):
            with self.subTest(profile=profile):
                self.request(profile)
                result = self.run_step("Validate fixed profile and immutable source")
                self.assertNotEqual(result.returncode, 0)
        for extra in (
            {"packages": ["xtask", "zed"]},
            {"command": "exit 0"},
            {"flags": ["--allow", "warnings"]},
            {"command_timeout_seconds": 99999},
            {"maximum_log_bytes": 999999999},
        ):
            with self.subTest(extra=extra):
                self.request(**extra)
                result = self.run_step("Validate fixed profile and immutable source")
                self.assertNotEqual(result.returncode, 0)

    def test_request_rejects_malformed_source_pins_and_schema(self):
        for changes in (
            {"source_commit": SOURCE_COMMIT.upper()},
            {"source_tree": "0" * 39},
            {"source_commit": SOURCE_COMMIT + "; exit 0"},
            {"source_tree": SOURCE_TREE + "\nprofile=gpui"},
            {"source_commit": None},
            {"schema_version": 2},
            {"schema_version": "1"},
        ):
            with self.subTest(changes=changes):
                self.request(**changes)
                result = self.run_step("Validate fixed profile and immutable source")
                self.assertNotEqual(result.returncode, 0)

    def test_original_commands_remain_byte_exact_and_new_packages_are_fixed(self):
        script = step_script(self.workflow, "Run fixed affected strict profile")
        self.assertIn(f"  acp) command=({ACP_COMMAND}) ;;\n", script)
        self.assertIn(f"  gpui) command=({GPUI_COMMAND}) ;;\n", script)
        case = script.split('case "$STRICT_PROFILE" in\n', 1)[1].split("esac", 1)[0]
        case = 'case "$STRICT_PROFILE" in\n' + case + 'esac\nprintf "%s\\0" "${command[@]}"\n'
        for profile, command in (
            ("acp", ACP_COMMAND), ("gpui", GPUI_COMMAND),
            ("android-context", CONTEXT_COMMAND), ("reference-tools", REFERENCE_COMMAND),
        ):
            with self.subTest(profile=profile):
                result = self.run_script(case, STRICT_PROFILE=profile)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.split("\0")[:-1], command.split())
        result = self.run_script(case, STRICT_PROFILE="reference-tools -p zed")
        self.assertNotEqual(result.returncode, 0)

    def test_frozen_script_keeps_all_strict_flags(self):
        self.assertEqual(hashlib.sha256(CLIPPY.read_bytes()).hexdigest(), CLIPPY_SHA256)
        self.executable("bin/cargo-fixture", """
            import json
            import os
            from pathlib import Path
            import sys
            Path(os.environ["STRICT_EVIDENCE"], "cargo-arguments.json").write_text(
                json.dumps(sys.argv[1:])
            )
        """)
        result = self.run_script(
            "./script/clippy --locked -p xtask\n",
            CARGO=str(self.root / "bin/cargo-fixture"), GITHUB_ACTIONS="true",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.root / "evidence/cargo-arguments.json").read_text()), [
            "clippy", "--locked", "-p", "xtask", "--release", "--all-targets",
            "--all-features", "--", "--deny", "warnings",
        ])

    def test_source_checks_reject_wrong_commit_tree_and_dirty_checkout(self):
        for changes in (
            {"commit": "a" * 40}, {"tree": "b" * 40}, {"status": " M Cargo.lock\n"},
        ):
            with self.subTest(changes=changes):
                self.write_state(**changes)
                result = self.run_step("Verify source before bootstrap")
                self.assertNotEqual(result.returncode, 0)

    def test_source_checks_reject_compiler_overrides(self):
        for variable in (
            "CARGO", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
        ):
            with self.subTest(variable=variable):
                result = self.run_step("Verify source before bootstrap", **{variable: ""})
                self.assertNotEqual(result.returncode, 0)

    def test_source_check_freezes_script_and_lock_across_bootstrap(self):
        result = self.run_step("Verify source before bootstrap")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_step("Verify source before compiler")
        self.assertEqual(result.returncode, 0, result.stderr)
        (self.root / "Cargo.lock").write_text("changed lock\n")
        result = self.run_step("Verify source before compiler")
        self.assertNotEqual(result.returncode, 0)
        (self.root / "Cargo.lock").write_text("fixture lock\n")
        with (self.root / "script/clippy").open("a") as script:
            script.write("\nexit 0\n")
        result = self.run_step("Verify source before compiler")
        self.assertNotEqual(result.returncode, 0)

    def test_worker_preserves_deadline_capture_cap_command_and_exit_evidence(self):
        self.prepare_worker()
        for profile, command in (
            ("android-context", CONTEXT_COMMAND), ("reference-tools", REFERENCE_COMMAND),
        ):
            with self.subTest(profile=profile):
                result = self.run_step("Run fixed affected strict profile", STRICT_PROFILE=profile)
                self.assertEqual(result.returncode, 0, result.stderr)
                evidence = self.root / "evidence"
                self.assertEqual(json.loads((evidence / "command.json").read_text()), command.split())
                self.assertEqual(json.loads((evidence / "observed-command.json").read_text()), command.split())
                self.assertEqual(json.loads((evidence / "timeout-arguments.json").read_text()), [
                    "--signal=TERM", "--kill-after=30s", "165m",
                ])
                self.assertEqual(json.loads((evidence / "capture-limit.json").read_text()), [
                    67108864, 67108864,
                ])
                self.assertEqual(json.loads((evidence / "exit.json").read_text()), {
                    "strict_process_exit": 0, "log_capture_exit": 0,
                    "command_timeout_seconds": 9900, "maximum_log_bytes": 67108864,
                    "tests_executed": False,
                })
                self.assertEqual((evidence / "strict.log").read_text(), "fixture compiler output\n")

    def test_worker_propagates_compiler_and_capture_failures(self):
        self.prepare_worker()
        result = self.run_step("Run fixed affected strict profile", MOCK_COMMAND_EXIT="23")
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(json.loads((self.root / "evidence/exit.json").read_text())["strict_process_exit"], 23)
        result = self.run_step("Run fixed affected strict profile", MOCK_CAPTURE_EXIT="19")
        self.assertEqual(result.returncode, 19, result.stderr)
        self.assertEqual(json.loads((self.root / "evidence/exit.json").read_text())["log_capture_exit"], 19)

    def test_worker_rejects_source_mutation_after_compiler(self):
        self.prepare_worker()
        for field in ("commit", "tree", "status"):
            with self.subTest(field=field):
                self.write_state()
                result = self.run_step("Run fixed affected strict profile", MOCK_SOURCE_CHANGE=field)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue((self.root / "evidence/exit.json").is_file())


if __name__ == "__main__":
    unittest.main()
