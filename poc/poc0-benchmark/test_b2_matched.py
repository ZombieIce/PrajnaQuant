from __future__ import annotations

import importlib.metadata
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import b2_matched


def fixed_nautilus_available() -> bool:
    try:
        return importlib.metadata.version("nautilus_trader") == "2.0.0rc5"
    except importlib.metadata.PackageNotFoundError:
        return False


class DecisionTests(unittest.TestCase):
    def decide(self, **overrides):
        inputs = {
            "correctness": "passed",
            "protocol": "matched",
            "rust_serial_median_ns": 50.0,
            "nautilus_serial_median_ns": 100.0,
            "rust_parallel_runs_per_second": 200.0,
            "nautilus_parallel_runs_per_second": 100.0,
            "rust_peak_rss_bytes": 1000,
            "nautilus_peak_rss_sum_upper_bound_bytes": 1000,
        }
        inputs.update(overrides)
        return b2_matched.decide(**inputs)

    def test_adopts_at_exact_speed_and_rss_boundaries_and_ignores_adr_exclusion(self):
        self.assertEqual(
            self.decide(correctness_checks={b2_matched.ADR_EXCLUDED: False})["status"],
            "adopt",
        )

    def test_defers_when_speed_or_rss_gate_misses(self):
        self.assertEqual(self.decide(rust_serial_median_ns=51.0)["status"], "defer")
        self.assertEqual(self.decide(nautilus_peak_rss_sum_upper_bound_bytes=999)["status"], "defer")

    def test_rejects_only_non_excluded_correctness_failures(self):
        self.assertEqual(self.decide(correctness_checks={"fills": False})["status"], "reject")
        self.assertEqual(self.decide(correctness="failed")["status"], "unresolved")

    def test_missing_measurement_or_unmatched_protocol_is_unresolved(self):
        self.assertEqual(self.decide(rust_peak_rss_bytes=None)["status"], "unresolved")
        self.assertEqual(self.decide(protocol="unmatched")["status"], "unresolved")

    def test_maintenance_proxy_changes_never_change_decision(self):
        metrics = b2_matched.maintenance_cost_proxies(Path(sys.executable))
        original = self.decide()
        metrics["semantic_difference_count"] = 999
        metrics["code_lines"]["fast_event_poc"]["count"] = 0
        metrics["new_direct_dependencies"]["fast_event_poc"]["count"] = 500
        self.assertEqual(self.decide(), original)
        self.assertFalse(metrics["decision_inputs"])
        self.assertEqual(metrics["semantic_difference_count"], 999)

    def test_maintenance_proxies_report_scope_and_unknown_dependency_state(self):
        metrics = b2_matched.maintenance_cost_proxies(Path("/missing/python"))
        self.assertGreater(metrics["code_lines"]["fast_event_poc"]["count"], 0)
        self.assertGreater(metrics["code_lines"]["nautilus_adapter"]["count"], 0)
        self.assertEqual(metrics["tests"]["fast_event_poc"]["count"], 4)
        self.assertEqual(metrics["new_direct_dependencies"]["fast_event_poc"]["count"], 0)
        self.assertEqual(metrics["new_direct_dependencies"]["nautilus_adapter"]["count"], 1)
        self.assertEqual(metrics["semantic_difference_count"], 1)
        self.assertEqual(metrics["python_transitive_dependencies"]["status"], "unknown")

    def test_transitive_dependency_snapshot_preserves_versions_and_detects_missing_packages(self):
        known = subprocess.CompletedProcess(
            [], 0,
            json.dumps({"status": "known", "count": 1, "packages": ["example-lib==1.2.3"], "missing": []}),
            "",
        )
        with patch.object(b2_matched.subprocess, "run", return_value=known):
            snapshot = b2_matched._resolved_python_transitives(Path("/fake/python"))
        self.assertEqual(snapshot["count"], 1)
        self.assertEqual(snapshot["packages"], ["example-lib==1.2.3"])

        incomplete = subprocess.CompletedProcess(
            [], 0,
            json.dumps({"status": "unknown", "count": 1, "packages": ["example-lib==1.2.3"], "missing": ["missing-lib"]}),
            "",
        )
        with patch.object(b2_matched.subprocess, "run", return_value=incomplete):
            snapshot = b2_matched._resolved_python_transitives(Path("/fake/python"))
        self.assertIsNone(snapshot["count"])
        self.assertEqual(snapshot["missing_packages"], ["missing-lib"])

        incompatible = subprocess.CompletedProcess(
            [], 0,
            json.dumps({
                "status": "unknown",
                "count": 1,
                "packages": ["example-lib==9.0.0"],
                "missing": [],
                "version_mismatches": ["example-lib==9.0.0 does not satisfy example-lib<2"],
                "unparseable_requirements": ["malformed requirement"],
            }),
            "",
        )
        with patch.object(b2_matched.subprocess, "run", return_value=incompatible):
            snapshot = b2_matched._resolved_python_transitives(Path("/fake/python"))
        self.assertIsNone(snapshot["count"])
        self.assertEqual(len(snapshot["version_mismatches"]), 1)
        self.assertEqual(snapshot["unparseable_requirements"], ["malformed requirement"])

    def test_conflicting_transitive_constraints_make_dependency_count_unknown(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            distributions = {
                "nautilus_trader-2.0.0rc5.dist-info": (
                    "Name: nautilus_trader\nVersion: 2.0.0rc5\n"
                    "Requires-Dist: dep-a\nRequires-Dist: dep-b\n"
                ),
                "dep_a-1.0.dist-info": (
                    "Name: dep-a\nVersion: 1.0\nRequires-Dist: shared-lib<2\n"
                ),
                "dep_b-1.0.dist-info": (
                    "Name: dep-b\nVersion: 1.0\nRequires-Dist: shared.lib>=2\n"
                ),
                "shared_lib-1.5.dist-info": "Name: shared_lib\nVersion: 1.5\n",
            }
            for directory, metadata in distributions.items():
                dist_info = root / directory
                dist_info.mkdir()
                (dist_info / "METADATA").write_text(
                    "Metadata-Version: 2.1\n" + metadata,
                    encoding="utf-8",
                )
            with patch.dict(os.environ, {"PYTHONPATH": str(root)}):
                snapshot = b2_matched._resolved_python_transitives(Path(sys.executable))
        self.assertEqual(snapshot["status"], "unknown")
        self.assertIsNone(snapshot["count"])
        self.assertTrue(any("shared-lib==1.5" in item for item in snapshot["version_mismatches"]))


class CoordinatorTests(unittest.TestCase):
    def test_missing_preflight_binary_writes_unresolved_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "missing-binary-report.json"
            missing_binary = Path(temporary) / "does-not-exist"
            result = subprocess.run(
                [
                    sys.executable,
                    str(b2_matched.ROOT / "poc/poc0-benchmark/b2_matched.py"),
                    "--binary",
                    str(missing_binary),
                    "--output",
                    str(output),
                ],
                cwd=b2_matched.ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(report["status"], "unresolved")
            self.assertIn("FileNotFoundError", report["correctness"]["stderr"])
            self.assertIn("maintenance_cost_proxies", report)
            self.assertFalse(report["maintenance_cost_proxies"]["decision_inputs"])


class NautilusIntegrationTests(unittest.TestCase):
    @unittest.skipUnless(
        fixed_nautilus_available(),
        "fixed Nautilus 2.0.0rc5 runtime is unavailable",
    )
    def test_s2_netcash_and_projection_agree_with_rust_golden_outside_lifecycle_exclusion(self):
        import json
        import subprocess
        from nautilus_adapter import _compare, _projection_checksum, _run_nautilus, parallel_digest_runs

        root = b2_matched.ROOT
        binary = root / "target/debug/quant-research"
        if not binary.exists():
            self.skipTest("Rust quant-research test binary is not built")
        output = root / "target/poc-0/b2-s2-integration-golden.json"
        result = subprocess.run(
            [str(binary), "benchmark-poc0", "--candidate", "soa", "--output", str(output)],
            cwd=root,
            text=True,
            capture_output=True,
            check=False,
        )
        if result.returncode:
            self.skipTest("Rust golden preflight failed: " + result.stderr[-1000:])
        golden = json.loads(output.read_text())["b2_fast_event_momentum_rotation"]["projection"]
        dataset = json.loads((root / "poc/poc0-benchmark/fixtures/dataset-v1.json").read_text())
        actual = _run_nautilus(dataset, strategy="s2")["projection"]
        checks = _compare(actual, golden)
        failures = [item["field"] for item in checks if not item["passed"] and item["field"] != b2_matched.ADR_EXCLUDED]
        self.assertEqual(failures, [])
        measurement = parallel_digest_runs(
            dataset,
            "s2",
            _projection_checksum(actual),
            workers=2,
            runs=2,
        )
        self.assertEqual(measurement["status"], "passed", measurement)
        self.assertEqual(measurement["warmup_runs_per_worker"], 2)
        self.assertEqual(set(measurement["checksums_sha256"]), {_projection_checksum(actual)})


if __name__ == "__main__":
    unittest.main()
