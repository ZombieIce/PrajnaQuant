"""Public benchmark report CLI checks; no Rust build is needed."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "poc/mvp2-benchmark/benchmark.py"


def measurements():
    samples = []
    for threads in (1, 2, 4, 8):
        for cache in ("cold", "hot"):
            for level in ("summary", "full"):
                for repeat, seconds in enumerate((3.0, 1.0, 2.0), 1):
                    samples.append({
                        "threads": threads, "cache_state": cache,
                        "result_level": level, "repeat": repeat,
                        "wall_seconds": seconds,
                        "time_output": "  123456 maximum resident set size\n",
                        "summary_no_runs": True if level == "summary" else None,
                        "summary_logical_hash": "same-result",
                        "execution_provenance": {
                            "git_revision": "a" * 40 + "\n", "reproducible": True,
                        },
                        "cli": {
                            "run_count": 288, "failed_count": 0, "action": "created",
                            "reproducible": True, "threads": threads,
                            "timings_ms": {"total": 2000, "factors": 100, "runs": 900},
                            "cache": {"compute_count": 0 if cache == "hot" else 20,
                                      "hit_count": 20 if cache == "hot" else 1},
                            "written_bytes": 5000, "written_files": 2,
                        },
                    })
    return samples


class BenchmarkReportCliTests(unittest.TestCase):
    def summarize(self, samples, status="measured"):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "raw.json"
            source.write_text(json.dumps({
                "status": status, "repeats": 3, "measurements": samples,
                "provenance": {"git_revision": "a" * 40, "reproducible": True},
            }))
            return subprocess.run(
                [sys.executable, str(SCRIPT), "summarize", str(source)],
                capture_output=True, text=True, check=False,
            )

    def test_checked_in_workload_is_the_registered_288_run_fixture_grid(self):
        definition = json.loads((SCRIPT.parent / "experiment.json").read_text())
        self.assertEqual(definition["parameter_space"]["grid"], {
            "short": [5, 10, 20], "long": [40, 60], "vol": [10, 20],
            "trend": [None, 20], "top_k": [1, 3, 5],
            "rebalance_every": [1, 5, 10, 20],
            "w_s": [1.0], "w_l": [1.0], "w_v": [1.0],
        })
        self.assertEqual(definition["costs"], {
            "commission_rate": 0.001, "buy_slippage_bps": 10.0,
            "sell_slippage_bps": 10.0, "buy_tax_rate": 0.0, "sell_tax_rate": 0.0,
        })
        self.assertEqual(definition["sessions_per_year"], 252)
        self.assertEqual(definition["availability_assumption"], "none")
        self.assertNotIn("seed", definition)

    def test_complete_matrix_reports_medians_without_dropping_raw_values(self):
        result = self.summarize(measurements())
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(len(report["cells"]), 16)
        first = report["cells"][0]
        self.assertEqual(first["sample_count"], 3)
        self.assertEqual(first["median"]["wall_seconds"], 2.0)
        self.assertEqual(first["median"]["runs_per_second"], 144.0)
        self.assertEqual(first["median"]["peak_rss_bytes"], 123456)
        self.assertEqual(first["raw"]["wall_seconds"], [3.0, 1.0, 2.0])

    def test_incomplete_or_invalid_measurements_are_not_success_reports(self):
        for field, value, diagnostic in (
            ("time_output", "no RSS here", "RSS"),
            ("summary_no_runs", False, "runs/"),
            ("repeat", 2, "repeat"),
            ("summary_logical_hash", "different-result", "logical hash"),
        ):
            with self.subTest(field=field):
                samples = measurements()
                samples[0][field] = value
                result = self.summarize(samples)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertIn(diagnostic, result.stderr)
        result = self.summarize(measurements()[1:])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("repeat", result.stderr)

    def test_incomplete_or_changed_provenance_cannot_be_summarized(self):
        result = self.summarize(measurements(), status="incomplete")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("incomplete", result.stderr)
        for field, value in (("git_revision", "different"), ("reproducible", False)):
            with self.subTest(field=field):
                samples = measurements()
                samples[-1]["execution_provenance"][field] = value
                result = self.summarize(samples)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertIn("provenance", result.stderr)


if __name__ == "__main__":
    unittest.main()
