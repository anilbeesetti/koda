"""Release tag reservation checks; no remote repository mutations."""

import datetime
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from zoneinfo import ZoneInfo


SCRIPT = Path(__file__).resolve().parents[1] / "reserve-macos-release-version"
LOADER = importlib.machinery.SourceFileLoader("release_version", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
release_version = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(release_version)
COMMIT = "a" * 40


class ReservationTests(unittest.TestCase):
    def setUp(self):
        self.tags = patch.object(release_version.subprocess, "check_output", return_value="")
        self.tags.start()
        self.addCleanup(self.tags.stop)
        self.api = patch.object(release_version, "api", side_effect=[{"sha": "tag-object"}, {"ref": "reserved"}])
        self.remote = self.api.start()
        self.addCleanup(self.api.stop)

    def reserve(self, clock, channel="stable"):
        return release_version.reserve("owner/repo", "123", COMMIT, clock=clock, wait=lambda _: None, channel=channel)

    def test_timestamp_timezone_and_exact_commit(self):
        # 18:35 UTC is 00:05 next day in Asia/Calcutta.
        timestamp = datetime.datetime(2026, 9, 30, 18, 35, tzinfo=datetime.timezone.utc).astimezone(ZoneInfo("Asia/Calcutta"))
        self.assertEqual(self.reserve(lambda: timestamp), "2026.10.01.00.05")
        self.assertEqual(self.remote.call_args_list[0].args[1]["object"], COMMIT)
        self.assertEqual(self.remote.call_args_list[1].args[1]["sha"], "tag-object")

    def test_collision_waits_for_real_next_minute(self):
        self.remote.side_effect = [{"sha": "first"}, None, {"sha": "second"}, {"ref": "reserved"}]
        timestamps = iter([datetime.datetime(2026, 9, 30, 23, 59), datetime.datetime(2026, 10, 1, 0, 0)])
        self.assertEqual(self.reserve(lambda: next(timestamps)), "2026.10.01.00.00")

    def test_rerun_reuses_annotated_tag_without_writes(self):
        release_version.subprocess.check_output.return_value = f"2026.09.30.12.00\tMain macOS release for workflow run 123\t{COMMIT}\n"
        self.assertEqual(self.reserve(lambda: self.fail("Reruns must not select a new timestamp")), "2026.09.30.12.00")
        self.remote.assert_not_called()

    def test_rerun_rejects_changed_commit(self):
        release_version.subprocess.check_output.return_value = f"2026.09.30.12.00\tMain macOS release for workflow run 123\t{'b' * 40}\n"
        with self.assertRaisesRegex(RuntimeError, "does not match"):
            self.reserve(lambda: None)
        self.remote.assert_not_called()

    def test_nightly_reservation_has_separate_tag_and_run_marker(self):
        timestamp = datetime.datetime(2026, 10, 1, 12, 30)
        self.assertEqual(self.reserve(lambda: timestamp, "nightly"), "nightly-2026.10.01.12.30")
        payload = self.remote.call_args_list[0].args[1]
        self.assertEqual(payload["tag"], "nightly-2026.10.01.12.30")
        self.assertEqual(payload["message"], "Nightly macOS release for workflow run 123")

    def test_nightly_rerun_reuses_only_its_channel_tag(self):
        release_version.subprocess.check_output.return_value = (
            f"2026.09.30.12.00\tMain macOS release for workflow run 123\t{COMMIT}\n"
            f"nightly-2026.09.30.12.01\tNightly macOS release for workflow run 123\t{COMMIT}\n"
        )
        self.assertEqual(self.reserve(lambda: self.fail("Rerun selected a new tag"), "nightly"), "nightly-2026.09.30.12.01")
        self.remote.assert_not_called()

    def test_unsupported_channel_fails_before_reservation(self):
        with self.assertRaisesRegex(ValueError, "Unsupported release channel"):
            self.reserve(lambda: None, "preview")
        self.remote.assert_not_called()


class ApiTests(unittest.TestCase):
    def test_only_existing_reference_is_a_collision(self):
        response = subprocess.CompletedProcess([], 1, json.dumps({"message": "Reference already exists"}), "HTTP 422")
        with patch.object(release_version.subprocess, "run", return_value=response):
            self.assertIsNone(release_version.api("repos/owner/repo/git/refs", {}))

    def test_permissions_validation_and_network_errors_fail(self):
        for message in ["Resource not accessible by integration", "Validation Failed", "Not Found", "network failed"]:
            response = subprocess.CompletedProcess([], 1, json.dumps({"message": message}), message)
            with self.subTest(message=message), patch.object(release_version.subprocess, "run", return_value=response):
                with self.assertRaisesRegex(RuntimeError, message):
                    release_version.api("repos/owner/repo/git/refs", {})


class NightlyChangesTests(unittest.TestCase):
    def release(self, tag="nightly-2026.10.01.00.00", **overrides):
        return dict(tag_name=tag, draft=False, prerelease=True,
                    published_at="2026-09-30T18:30:00Z", **overrides)

    def check(self, pages, previous=COMMIT):
        with patch.object(release_version.subprocess, "check_output",
                          side_effect=[json.dumps(pages), previous + "\n"]) as command:
            result = release_version.nightly_has_changes("owner/repo", COMMIT)
            return result, command.call_args_list

    def test_first_nightly_builds(self):
        result, calls = self.check([[]])
        self.assertTrue(result)
        self.assertEqual(len(calls), 1)

    def test_unchanged_commit_skips_and_changed_commit_builds(self):
        self.assertFalse(self.check([[self.release()]])[0])
        self.assertTrue(self.check([[self.release()]], previous="b" * 40)[0])

    def test_ignores_drafts_stable_dev_unpublished_and_invalid_timestamps(self):
        draft = self.release()
        draft["draft"] = True
        unpublished = self.release()
        unpublished["published_at"] = None
        stable = self.release("2026.10.01.00.00")
        stable["prerelease"] = False
        releases = [draft, unpublished, stable, self.release("dev-2026.10.01.00.00"),
                    self.release("nightly-2026.99.01.00.00")]
        result, calls = self.check([releases])
        self.assertTrue(result)
        self.assertEqual(len(calls), 1)

    def test_newest_timestamp_across_pages_resolves_actual_tag_commit(self):
        result, calls = self.check([
            [self.release("nightly-2026.09.29.00.00")],
            [dict(self.release(), target_commitish="main")],
        ])
        self.assertFalse(result)
        self.assertIn("--paginate", calls[0].args[0])
        self.assertEqual(calls[1].args[0],
                         ["git", "rev-parse", "refs/tags/nightly-2026.10.01.00.00^{commit}"])

    def test_api_and_missing_tag_errors_stop_the_release(self):
        with patch.object(release_version.subprocess, "check_output",
                          side_effect=subprocess.CalledProcessError(1, "gh")):
            with self.assertRaises(subprocess.CalledProcessError):
                release_version.nightly_has_changes("owner/repo", COMMIT)
        with patch.object(release_version.subprocess, "check_output", side_effect=[
            json.dumps([[self.release()]]), subprocess.CalledProcessError(1, "git"),
        ]):
            with self.assertRaises(subprocess.CalledProcessError):
                release_version.nightly_has_changes("owner/repo", COMMIT)

    def test_skip_does_not_reserve_a_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            with patch.dict(os.environ, GITHUB_REPOSITORY="owner/repo", GITHUB_SHA=COMMIT,
                            RELEASE_CHANNEL="nightly", GITHUB_OUTPUT=str(output)), \
                    patch.object(release_version, "nightly_has_changes", return_value=False), \
                    patch.object(release_version, "reserve") as reserve:
                release_version.main()
            self.assertEqual(output.read_text(), "release=false\n")
            reserve.assert_not_called()


if __name__ == "__main__":
    unittest.main()
