import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "ci-run"
loader = importlib.machinery.SourceFileLoader("ci_run", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
ci_run = importlib.util.module_from_spec(spec)
loader.exec_module(ci_run)
GIB = 1024**3


class CiRunTests(unittest.TestCase):
    def test_hosted_linux_uses_available_cpus(self):
        self.assertEqual(ci_run.concurrency(4, 16 * GIB, "Linux"), (4, 4))

    def test_small_macos_preserves_memory_headroom(self):
        self.assertEqual(ci_run.concurrency(3, 7 * GIB, "Darwin"), (2, 3))
        self.assertEqual(ci_run.concurrency(4, 14 * GIB, "Darwin"), (4, 4))

    def test_large_linux_bounds_compile_but_allows_parallel_tests(self):
        self.assertEqual(ci_run.concurrency(16, 32 * GIB, "Linux"), (4, 15))

    def test_low_memory_and_cpu_have_at_least_one_worker(self):
        self.assertEqual(ci_run.concurrency(0, GIB, "Linux"), (1, 1))
        self.assertEqual(ci_run.concurrency(8, 6 * GIB, "Linux"), (2, 2))

    def test_stage_preserves_arguments_exit_status_and_reports_duration(self):
        with tempfile.TemporaryDirectory() as temp:
            summary = Path(temp) / "summary"
            literal = "$(exit 99); spaces and 'quotes'"
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "stage", "test stage", "--", sys.executable,
                 "-c", "import sys; print(sys.argv[1]); sys.exit(23)", literal],
                capture_output=True, text=True,
                env={**os.environ, "GITHUB_STEP_SUMMARY": str(summary)},
            )
            self.assertEqual(result.returncode, 23)
            self.assertIn(literal, result.stdout)
            self.assertIn("test stage:", result.stdout)
            self.assertIn("(exit 23)", summary.read_text())


if __name__ == "__main__":
    unittest.main()
