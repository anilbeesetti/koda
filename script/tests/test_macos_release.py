"""Release tag reservation checks; no remote repository mutations."""

import datetime
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
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

    def test_dev_reservation_has_separate_tag_and_run_marker(self):
        timestamp = datetime.datetime(2026, 10, 1, 12, 30)
        self.assertEqual(self.reserve(lambda: timestamp, "dev"), "dev-2026.10.01.12.30")
        payload = self.remote.call_args_list[0].args[1]
        self.assertEqual(payload["tag"], "dev-2026.10.01.12.30")
        self.assertEqual(payload["message"], "Dev macOS release for workflow run 123")

    def test_dev_rerun_reuses_only_its_channel_tag(self):
        release_version.subprocess.check_output.return_value = (
            f"2026.09.30.12.00\tMain macOS release for workflow run 123\t{COMMIT}\n"
            f"dev-2026.09.30.12.01\tDev macOS release for workflow run 123\t{COMMIT}\n"
        )
        self.assertEqual(self.reserve(lambda: self.fail("Rerun selected a new tag"), "dev"), "dev-2026.09.30.12.01")
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


if __name__ == "__main__":
    unittest.main()
