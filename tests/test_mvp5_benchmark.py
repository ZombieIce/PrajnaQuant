"""Public record-only report CLI; no extension or Rust build required."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "poc/mvp5-benchmark/benchmark.py"


def measurements():
    return [
        {
            "strategy": strategy, "implementation": implementation,
            "threads": threads, "repeat": repeat, "run_count": 2 if strategy == "s2" else 1,
            "action": "created", "statuses": ["completed"] * (2 if strategy == "s2" else 1),
            "wall_seconds": seconds, "peak_rss_bytes": 123456,
            "summary_logical_hash": f"{strategy}-{implementation}",
            "statistics": {
                "run_time_ms": 100.0,
                **({"python": {"stage1_ms": 400.0, "stage2_ms": 200.0,
                               "asof_view_ms": [1.0, 3.0] * (43 if strategy == "s2" else 96)}}
                   if implementation == "python" else {}),
            },
        }
        for strategy in ("s2", "s3")
        for implementation in ("rust", "python")
        for threads in (1, 2)
        for repeat, seconds in enumerate((3.0, 1.0, 2.0), 1)
    ]


class BenchmarkReportTests(unittest.TestCase):
    def summarize(self, samples):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "raw.json"
            source.write_text(json.dumps({
                "status": "measured", "correctness": {"returncode": 0},
                "repeats": 3, "measurements": samples,
            }))
            return subprocess.run(
                [sys.executable, SCRIPT, "summarize", source],
                capture_output=True, text=True,
            )

    def test_complete_matrix_preserves_raw_samples_without_performance_gate(self):
        result = self.summarize(measurements())
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["conclusion"], "record_only")
        self.assertEqual(len(report["cells"]), 8)
        cell = report["cells"][2]
        self.assertEqual(cell["median"]["runs_per_second"], 1.0)
        self.assertEqual(cell["median"]["stage1_ms"], 400.0)
        self.assertEqual(cell["asof_view_ms"]["median"], 2.0)
        self.assertEqual(cell["asof_view_ms"]["p95_nearest_rank"], 3.0)
        self.assertEqual(cell["raw"]["wall_seconds"], [3.0, 1.0, 2.0])

    def test_incomplete_failed_or_inconsistent_samples_are_rejected(self):
        for field, value in (
            ("action", "replayed"), ("statuses", ["failed", "completed"]),
            ("repeat", 2), ("summary_logical_hash", "changed"),
            ("peak_rss_bytes", 0), ("wall_seconds", float("nan")),
        ):
            with self.subTest(field=field):
                samples = measurements()
                samples[0][field] = value
                result = self.summarize(samples)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
        samples = measurements()
        samples[6]["statistics"]["python"]["asof_view_ms"].pop()
        self.assertNotEqual(self.summarize(samples).returncode, 0)
        self.assertNotEqual(self.summarize(measurements()[1:]).returncode, 0)

    def test_skipped_correctness_cannot_be_summarized_as_measured(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "raw.json"
            source.write_text(json.dumps({
                "status": "measured", "repeats": 3, "measurements": measurements(),
                "correctness": {"returncode": 0, "stderr": "OK (skipped=1)"},
            }))
            result = subprocess.run(
                [sys.executable, SCRIPT, "summarize", source], capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
