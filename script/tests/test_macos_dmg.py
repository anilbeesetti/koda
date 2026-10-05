"""Exercise the actual DMG helper with fake hdiutil and a nonblocking sleep."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "create-macos-dmg"
BUSY = "hdiutil: create failed - Resource busy"


class DmgCreationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="macos-dmg-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.calls = self.root / "calls.jsonl"
        self.sleeps = self.root / "sleeps"
        hdiutil = self.root / "hdiutil"
        hdiutil.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
log = pathlib.Path(os.environ['DMG_TEST_CALLS'])
attempt = len(log.read_text().splitlines()) + 1 if log.exists() else 1
with log.open('a') as output: output.write(json.dumps(sys.argv[1:]) + '\\n')
if attempt <= int(os.environ['DMG_TEST_FAILURES']):
    print(os.environ['DMG_TEST_ERROR'], file=sys.stderr)
    sys.exit(23)
pathlib.Path(sys.argv[-1]).write_bytes(b'test disk image')
print('created: ' + sys.argv[-1])
""")
        hdiutil.chmod(0o755)
        sleep = self.root / "sleep"
        sleep.write_text("#!/usr/bin/env bash\nprintf '%s\\n' \"$1\" >> \"$DMG_TEST_SLEEPS\"\n")
        sleep.chmod(0o755)

    def run_helper(self, failures, error=BUSY):
        source = self.root / "source folder"
        source.mkdir()
        output = self.root / "output disk.dmg"
        result = subprocess.run(
            [str(SCRIPT), str(source), str(output)], capture_output=True, text=True,
            env=dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
                     DMG_TEST_CALLS=str(self.calls), DMG_TEST_SLEEPS=str(self.sleeps),
                     DMG_TEST_FAILURES=str(failures), DMG_TEST_ERROR=error),
        )
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        expected_args = ["create", "-volname", "Koda", "-srcfolder", str(source), "-ov", "-format", "UDZO", str(output)]
        self.assertEqual(calls, [expected_args] * len(calls))
        sleeps = self.sleeps.read_text().splitlines() if self.sleeps.exists() else []
        return result, calls, sleeps, output

    def test_first_attempt_success(self):
        result, calls, sleeps, output = self.run_helper(0)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertEqual(sleeps, [])
        self.assertEqual(result.stdout.strip(), f"created: {output}")
        self.assertEqual(result.stderr, "")
        self.assertTrue(output.exists())

    def test_resource_busy_then_success(self):
        result, calls, sleeps, output = self.run_helper(2)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(len(calls), 3)
        self.assertEqual(sleeps, ["3", "3"])
        self.assertEqual(result.stderr.count(BUSY), 2)
        self.assertTrue(output.exists())

    def test_permanent_error_fails_immediately(self):
        error = "hdiutil: create failed - No space left on device"
        result, calls, sleeps, output = self.run_helper(100, error)
        self.assertEqual(result.returncode, 23)
        self.assertEqual(len(calls), 1)
        self.assertEqual(sleeps, [])
        self.assertEqual(result.stderr.strip(), error)
        self.assertFalse(output.exists())

    def test_resource_busy_exhausts_bounded_retries(self):
        result, calls, sleeps, output = self.run_helper(100)
        self.assertEqual(result.returncode, 23)
        self.assertEqual(len(calls), 5)
        self.assertEqual(sleeps, ["3"] * 4)
        self.assertEqual(result.stderr.count(BUSY), 5)
        self.assertFalse(output.exists())

    def test_unrelated_busy_warning_does_not_retry(self):
        error = "disk scan warning: Resource busy\nhdiutil: create failed - Permission denied"
        result, calls, sleeps, _ = self.run_helper(100, error)
        self.assertEqual(result.returncode, 23)
        self.assertEqual(len(calls), 1)
        self.assertEqual(sleeps, [])


if __name__ == "__main__":
    unittest.main()
