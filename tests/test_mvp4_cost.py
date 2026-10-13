import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("mvp4_cost", ROOT / "poc/mvp4-cost/measure.py")
cost = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cost)


class MeasurementCliTests(unittest.TestCase):
    def test_disk_gate_records_missing_measurement_without_launching_build(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "report.json"
            with patch.object(cost.shutil, "disk_usage", return_value=(100, 95, 5)), \
                    patch.object(cost.subprocess, "run") as run:
                self.assertEqual(cost.main(["run", "--out", str(output)]), 2)
            run.assert_not_called()
            report = json.loads(output.read_text())
            self.assertEqual(report["status"], "missing")
            self.assertEqual(report["reason"], "10 GiB resource gate")
            self.assertIsNone(report["measurements"])
            self.assertEqual(report["thresholds"], None)

    def test_summary_retains_latency_series_and_worker_peak_upper_bound(self):
        sample = lambda ns, rss: {"wall_ns": ns, "checksum": "fixed", "usage": {"rss_bytes": rss}}
        cell = {
            "main": [sample(i, 100) for i in range(1, 21)],
            "end_to_end": [sample(i + 20, 100) for i in range(1, 21)],
            "baseline": {"rss_bytes": 30},
            "parallel": [
                {"wall_ns": 100, "workers": [
                    {"baseline": {"rss_bytes": 30}, "samples": [sample(10, 100)] * 6},
                    {"baseline": {"rss_bytes": 40}, "samples": [sample(10, 200)] * 6},
                ]} for _ in range(5)
            ],
        }
        summary = cost.summarize_cell(cell)
        self.assertEqual(summary["main_ns"]["median"], 10.5)
        self.assertEqual(summary["main_ns"]["p95"], 19)
        self.assertEqual(summary["rss_worker_peak_sum_upper_bound_bytes"], 300)
        self.assertEqual(summary["rss_baseline_sum_bytes"], 70)
        cell["parallel"][0]["wall_ns"] = 50
        with self.assertRaisesRegex(ValueError, "enclose"):
            cost.summarize_cell(cell)
        cell["parallel"][0]["wall_ns"] = 100
        cell["main"][0]["checksum"] = "different"
        with self.assertRaisesRegex(ValueError, "checksum"):
            cost.summarize_cell(cell)

    def test_blocked_mapping_is_missing_not_estimated(self):
        self.assertEqual(cost.summarize_cell({
            "status": "missing", "reason": "unsupported original Instrument increment", "metrics": None,
        }), {"status": "missing", "reason": "unsupported original Instrument increment", "metrics": None})

    def test_corrected_boundary_requires_per_run_research_evidence(self):
        sample = {
            "wall_ns": 10, "research_ns": 3, "research_invocations": 1,
            "decision_checksum": "independent", "checksum": "fixed",
            "usage": {"rss_bytes": 100},
        }
        cell = {
            "research": {"implementation": "Rust decide_panel"},
            "main": [dict(sample) for _ in range(20)],
            "end_to_end": [dict(sample) for _ in range(20)],
            "baseline": {"rss_bytes": 30},
            "parallel": [{"wall_ns": 100, "workers": [
                {"baseline": {"rss_bytes": 30}, "samples": [dict(sample) for _ in range(6)]}
                for _ in range(2)
            ]} for _ in range(5)],
        }
        summary = cost.summarize_cell(cell)
        self.assertEqual(summary["main_research_ns"]["median"], 3)
        self.assertEqual(summary["parallel_wall_ns"]["values"], [100] * 5)
        for field, value in [
            ("research_invocations", 0), ("research_ns", 0),
            ("research_ns", 11), ("decision_checksum", "changed"),
        ]:
            with self.subTest(field=field, value=value):
                cell["main"][0][field] = value
                with self.assertRaises(ValueError):
                    cost.summarize_cell(cell)
                cell["main"][0][field] = sample[field]
        del cell["parallel"][0]["workers"][0]["samples"][0]["research_ns"]
        with self.assertRaises(ValueError):
            cost.summarize_cell(cell)


if __name__ == "__main__":
    unittest.main()
