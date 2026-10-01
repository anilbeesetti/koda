"""Exercise release publication against local tags and a simulated GitHub CLI."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
VERSION = "2026.10.01.12.30"


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.log = self.root / "calls.jsonl"
        self.git("init", "-q")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.com",
                 "commit", "--allow-empty", "-qm", "Fixture")
        self.commit = self.git("rev-parse", "HEAD").strip()
        self.assets = self.root / "release-assets"
        self.assets.mkdir()
        command = self.root / "gh"
        command.write_text("""#!/usr/bin/env python3
import json, os, sys
arguments = sys.argv[1:]
with open(os.environ['CALL_LOG'], 'a') as output:
    output.write(json.dumps(arguments) + '\\n')
if arguments[0] == 'api':
    if '--paginate' in arguments:
        print(os.environ.get('EXISTING_TAG', ''))
    else:
        print(os.environ['MAIN_SHA'])
if arguments[:2] == ['release', 'upload'] and os.environ.get('FAIL_UPLOAD'):
    sys.exit(1)
""")
        command.chmod(0o755)

    def git(self, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=self.root, text=True)

    def publish(self, channel="stable", **overrides):
        tag = f"nightly-{VERSION}" if channel == "nightly" else VERSION
        self.git("tag", tag)
        name = "Koda-Nightly" if channel == "nightly" else "Koda"
        for filename in [f"{name}-{VERSION}-macos-aarch64.dmg",
                         f"koda-remote-server-{tag}-macos-aarch64.gz"]:
            (self.assets / filename).write_bytes(b"fixture download")
        checksums = "".join(
            f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n"
            for asset in sorted(self.assets.iterdir())
        )
        (self.assets / "SHA256SUMS-macos-aarch64.txt").write_text(checksums)
        environment = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
                           CALL_LOG=str(self.log), MAIN_SHA=self.commit,
                           GITHUB_SHA=self.commit, GITHUB_REPOSITORY="owner/repo",
                           RELEASE_VERSION=VERSION, RELEASE_TAG=tag,
                           RELEASE_CHANNEL=channel)
        environment.update(overrides)
        result = subprocess.run(["bash", str(ROOT / "script/publish-macos-release")],
                                cwd=self.root, env=environment, capture_output=True, text=True)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return result, calls

    def test_stable_publication_promotes_current_main(self):
        result, calls = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        create = next(call for call in calls if call[:2] == ["release", "create"])
        self.assertIn("--draft", create)
        self.assertIn("--prerelease=false", create)
        edit = calls[-1]
        self.assertEqual(edit[:3], ["release", "edit", VERSION])
        self.assertIn("--latest=true", edit)
        self.assertIn("--draft=false", edit)

    def test_nightly_publication_never_promotes_stable_latest(self):
        result, calls = self.publish("nightly")
        self.assertEqual(result.returncode, 0, result.stderr)
        create = next(call for call in calls if call[:2] == ["release", "create"])
        self.assertEqual(create[2], f"nightly-{VERSION}")
        self.assertIn("--prerelease=true", create)
        self.assertIn("--prerelease=true", calls[-1])
        self.assertIn("--latest=false", calls[-1])
        self.assertFalse(any("repos/owner/repo/git/ref/heads/main" in call for call in calls))

    def test_older_stable_commit_does_not_replace_latest(self):
        result, calls = self.publish(MAIN_SHA="b" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--latest=false", calls[-1])

    def test_upload_failure_keeps_release_draft(self):
        result, calls = self.publish("nightly", FAIL_UPLOAD="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(any(call[:2] == ["release", "create"] for call in calls))
        self.assertFalse(any(call[:2] == ["release", "edit"] for call in calls))

    def test_retry_uploads_existing_release_without_recreating_it(self):
        result, calls = self.publish("nightly", EXISTING_TAG=f"nightly-{VERSION}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(call[:2] == ["release", "create"] for call in calls))
        self.assertTrue(any(call[:2] == ["release", "upload"] for call in calls))

    def test_mismatched_channel_tag_fails_before_github_calls(self):
        result, calls = self.publish("nightly", RELEASE_TAG=VERSION)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_wrong_tag_commit_fails_before_github_calls(self):
        result, calls = self.publish(GITHUB_SHA="b" * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])


class ReleaseTriggerTests(unittest.TestCase):
    def test_release_entrypoints_accept_only_manual_dispatch(self):
        for filename in ["main_macos_release.yml",
                         "release.yml", "release_nightly.yml"]:
            with self.subTest(workflow=filename):
                text = (ROOT / ".github/workflows" / filename).read_text()
                trigger = text.split("\non:\n", 1)[1].split("\n\n", 1)[0]
                self.assertIn("workflow_dispatch:", trigger)
                self.assertNotIn("push:", trigger)
                self.assertNotIn("schedule:", trigger)
                self.assertNotIn("workflow_run:", trigger)

    def test_nightly_runs_at_midnight_in_calcutta_and_allows_manual_retries(self):
        workflow = (ROOT / ".github/workflows/nightly_macos_release.yml").read_text()
        trigger = workflow.split("\non:\n", 1)[1].split("\n\n", 1)[0]
        self.assertIn("cron: '30 18 * * *'", trigger)
        self.assertIn("workflow_dispatch:", trigger)
        self.assertNotIn("push:", trigger)
        self.assertIn("channel: nightly", workflow)
        self.assertIn("cancel-in-progress: false", workflow)


if __name__ == "__main__":
    unittest.main()
