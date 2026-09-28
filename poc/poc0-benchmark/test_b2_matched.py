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
import b2_robustness


def fixed_nautilus_available() -> bool:
    try:
        return importlib.metadata.version("nautilus_trader") == "2.0.0rc5"
    except importlib.metadata.PackageNotFoundError:
        return False


def valid_mode_evidence(reset_parity: str) -> dict:
    selected = "reset" if reset_parity == "passed" else "cached_conversion_new_engine"
    checksum = "a" * 64
    state = {"iteration_count": 10}
    failed = reset_parity == "failed"
    loads = []
    for name in ("s2_decision", "s3_decision", "s2_robustness", "s3_robustness"):
        loads.append({
            "name": name,
            "fresh_engine_checksum_sha256": checksum,
            "golden_checksum_sha256": checksum if name.endswith("decision") else None,
            "golden_match": name.endswith("decision"),
            "reset_checksums_sha256": [checksum] * 3,
            "reset_runs": 3,
            "fresh_engine_state": state,
            "reset_engine_states": [state] * 3,
            "reset_runs_engine_reused": [True] * 3,
            "parity": "failed" if failed else "passed",
            "reset_error_logs": ["native error"] * 3 if failed else [],
        })
    return {
        "schema_version": b2_matched.RESET_PARITY_SCHEMA,
        "nautilus_version": "2.0.0rc5",
        "revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=b2_matched.ROOT, text=True
        ).strip(),
        "dirty_worktree": False,
        "reset_parity": reset_parity,
        "selected_mode": selected,
        "loads": loads,
    }


class EnvironmentTests(unittest.TestCase):
    def test_hardware_identity_falls_back_without_leaking_system_profiler_output(self):
        with patch.object(b2_matched.platform, "system", return_value="Darwin"), patch.object(
            b2_matched,
            "_system_value",
            side_effect=["Hardware Overview:\n    Chip: Apple M1\n    Serial Number: private", None],
        ), patch.object(b2_matched.os, "sysconf", side_effect=[4, 4096]):
            cpu, memory = b2_matched._hardware_identity()
        self.assertEqual(cpu, "Apple M1")
        self.assertEqual(memory, 16_384)


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

    def test_non_excluded_correctness_failures_require_attribution_not_reject(self):
        decision = self.decide(correctness_checks={"fills": False})
        self.assertEqual(decision["status"], "unresolved")
        self.assertTrue(decision["attribution_required"])
        self.assertEqual(self.decide(correctness="failed")["status"], "unresolved")

    def test_missing_measurement_or_unmatched_protocol_is_unresolved(self):
        self.assertEqual(self.decide(rust_peak_rss_bytes=None)["status"], "unresolved")
        self.assertEqual(self.decide(protocol="unmatched")["status"], "unresolved")

    def test_reset_unverified_fallback_is_exploratory_not_registered(self):
        exploratory = self.decide(protocol="fallback_reset_unverified")
        self.assertEqual(exploratory["status"], "adopt")
        self.assertEqual(exploratory["evidence_level"], "exploratory")
        with_evidence = self.decide(
            protocol="fallback_reset_unverified",
            mode_selection=valid_mode_evidence("failed"),
        )
        self.assertEqual(with_evidence["evidence_level"], "exploratory")

    def test_legacy_protocol_labels_without_mode_evidence_stay_exploratory(self):
        for protocol in ("matched", "matched_fallback"):
            decision = self.decide(protocol=protocol)
            self.assertEqual(decision["status"], "adopt")
            self.assertEqual(decision["evidence_level"], "exploratory")

    def test_registered_only_when_mode_matches_adr_0013_selection(self):
        reset = valid_mode_evidence("passed")
        fallback = valid_mode_evidence("failed")
        self.assertEqual(self.decide(protocol="matched", mode_selection=reset)["evidence_level"], "registered")
        self.assertEqual(self.decide(protocol="matched_fallback", mode_selection=fallback)["evidence_level"], "registered")
        self.assertEqual(self.decide(protocol="matched_fallback", mode_selection=reset)["status"], "unresolved")
        self.assertEqual(self.decide(protocol="matched", mode_selection=fallback)["status"], "unresolved")
        inconsistent = {"reset_parity": "passed", "selected_mode": "cached_conversion_new_engine"}
        self.assertEqual(self.decide(protocol="matched_fallback", mode_selection=inconsistent)["status"], "unresolved")
        self.assertEqual(self.decide(protocol="matched", mode_selection={})["status"], "unresolved")

    def test_mode_selection_requires_complete_clean_pinned_evidence(self):
        incomplete = valid_mode_evidence("failed")
        incomplete["loads"].pop()
        decision = self.decide(protocol="matched_fallback", mode_selection=incomplete)
        self.assertEqual(decision["status"], "unresolved")
        self.assertIn("four required loads", decision["reason"])

        malformed = valid_mode_evidence("passed")
        malformed["loads"][0].pop("fresh_engine_state")
        malformed["loads"][0]["reset_engine_states"] = [None] * 3
        decision = self.decide(protocol="matched", mode_selection=malformed)
        self.assertEqual(decision["status"], "unresolved")
        self.assertIn("engine-state snapshots", decision["reason"])

        malformed = valid_mode_evidence("passed")
        malformed["loads"][0]["name"] = []
        decision = self.decide(protocol="matched", mode_selection=malformed)
        self.assertEqual(decision["status"], "unresolved")
        self.assertIn("invalid load name", decision["reason"])

        malformed = valid_mode_evidence("passed")
        malformed["revision"] = "f" * 40
        decision = self.decide(protocol="matched", mode_selection=malformed)
        self.assertEqual(decision["status"], "unresolved")
        self.assertIn("not an available Git commit", decision["reason"])

        contradictory = valid_mode_evidence("failed")
        for load in contradictory["loads"]:
            load["parity"] = "failed"
            load["reset_error_logs"] = []
        decision = self.decide(protocol="matched_fallback", mode_selection=contradictory)
        self.assertEqual(decision["status"], "unresolved")
        self.assertIn("contradicts observed parity", decision["reason"])

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

    def test_inactive_optional_extras_are_excluded_from_installed_closure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            dist_info = root / "nautilus_trader-2.0.0rc5.dist-info"
            dist_info.mkdir()
            (dist_info / "METADATA").write_text(
                "Metadata-Version: 2.1\nName: nautilus_trader\nVersion: 2.0.0rc5\n"
                "Requires-Dist: optional-viz-lib>=1; extra == 'visualization'\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"PYTHONPATH": str(root)}):
                snapshot = b2_matched._resolved_python_transitives(Path(sys.executable))
        self.assertEqual(snapshot["status"], "known")
        self.assertEqual(snapshot["count"], 0)
        self.assertEqual(snapshot["packages"], [])


class RobustnessDecisionTests(unittest.TestCase):
    def test_robustness_measurement_uses_evidence_selected_engine_mode(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "robustness.json"

            def create_output(command: list[str]) -> subprocess.CompletedProcess[str]:
                artifact = Path(command[command.index("--output") + 1])
                artifact.write_text(json.dumps({"status": "passed"}), encoding="utf-8")
                return subprocess.CompletedProcess(command, 0, "", "")

            with patch.object(b2_robustness, "_run", side_effect=create_output) as mocked_run:
                b2_robustness._measurement(
                    python=Path(sys.executable), binary=Path("quant-research"),
                    dataset=Path("fixture.json"), strategy="s2", checksum="rust",
                    nautilus_checksum="nautilus", version="fixture-v1", backend="nautilus",
                    mode="serial", group=0, output=output,
                    engine_mode="cached_conversion_new_engine",
                )

            command = mocked_run.call_args.args[0]
            self.assertEqual(command[command.index("--engine-mode") + 1], "cached_conversion_new_engine")

    def test_repeatability_requires_two_identical_passes_or_failures(self):
        passed = [
            {"status": "passed", "nautilus_checksum_sha256": "same"},
            {"status": "passed", "nautilus_checksum_sha256": "same"},
        ]
        failed = [
            {"status": "failed", "nautilus_checksum_sha256": "same", "failure_fields": ["fills"]},
            {"status": "failed", "nautilus_checksum_sha256": "same", "failure_fields": ["fills"]},
        ]
        self.assertEqual(b2_robustness._repeatability_status(passed), "passed")
        self.assertEqual(b2_robustness._repeatability_status(failed), "failed")
        self.assertEqual(b2_robustness._repeatability_status(failed[:1]), "unresolved")
        self.assertEqual(
            b2_robustness._repeatability_status([
                failed[0], {**failed[1], "nautilus_checksum_sha256": "different"}
            ]),
            "unresolved",
        )
        self.assertEqual(
            b2_robustness._repeatability_status([
                failed[0], {**failed[1], "failure_fields": ["daily.cash_nav"]}
            ]),
            "unresolved",
        )

    def test_mismatch_diagnostics_keep_date_shift_ledger_and_halt_boundary_visible(self):
        expected = {
            "orders": [{"decision_date": "2025-04-07", "attempt_date": "2025-04-08",
                        "symbol": "ETF038", "side": "BUY", "quantity": 100, "reason": None}],
            "fills": [{"date": "2025-04-08", "symbol": "ETF038", "side": "BUY",
                       "quantity": 100, "fill_price": 100.1, "commission": 100.0}],
            "ledger": [{"date": "2025-04-08", "cash": 48_350.0, "nav": 99_560.0,
                        "holdings": [{"symbol": "ETF038", "quantity": 100}]}],
            "summary": {"commission": 100.0, "tax": 0.0, "slippage_cost": 10.0, "total_cost": 110.0},
        }
        actual = {
            "orders": [{"decision_date": "2025-04-07", "attempt_date": "2025-04-09",
                        "symbol": "ETF038", "side": "BUY", "quantity": 100, "reason": None}],
            "fills": [{"date": "2025-04-09", "symbol": "ETF038", "side": "BUY",
                       "quantity": 100, "fill_price": 100.1, "commission": 100.0}],
            "ledger": [{"date": "2025-04-08", "cash": 98_900.0, "nav": 98_900.0,
                        "holdings": []}],
            "summary": {"commission": 100.0, "tax": 0.0, "slippage_cost": 10.0, "total_cost": 110.0},
        }
        dataset = {"execution_status_overrides": [{
            "date": "2025-07-02", "symbol": "ETF002", "trade_status": "HALTED"
        }]}
        diagnostics = b2_robustness._mismatch_diagnostics(actual, expected, dataset)
        self.assertEqual(diagnostics["orders"]["expected_only_examples"][0]["attempt_date"], "2025-04-08")
        self.assertEqual(diagnostics["orders"]["actual_only_examples"][0]["attempt_date"], "2025-04-09")
        self.assertEqual(diagnostics["fills"]["expected_only_examples"][0]["date"], "2025-04-08")
        self.assertEqual(diagnostics["fills"]["actual_only_examples"][0]["date"], "2025-04-09")
        self.assertEqual(diagnostics["daily_ledger_first_mismatches"][0]["cash_delta_actual_minus_expected"], 50_550.0)
        self.assertEqual(diagnostics["halt_attribution"][0]["rust_attempt_count"], 0)
        self.assertEqual(diagnostics["halt_attribution"][0]["nautilus_fill_count"], 0)
        self.assertIn("ADR 0012", diagnostics["halt_attribution"][0]["adr_0012_scope"])

    def test_robustness_miss_defers_an_adopted_decision_load(self):
        result = b2_robustness.combine_robustness(
            {"status": "adopt", "reason": "decision loads passed"},
            "passed",
            {"status": "defer", "reason": "latency gate missed"},
        )
        self.assertEqual(result["status"], "defer")
        self.assertEqual(result["robustness_status"], "passed")

    def test_unverified_robustness_retains_the_decision_load_conclusion(self):
        result = b2_robustness.combine_robustness(
            {"status": "adopt", "reason": "decision loads passed"}, "unverified"
        )
        self.assertEqual(result["status"], "adopt")
        self.assertEqual(result["robustness_status"], "unverified")

    def test_robustness_correctness_failure_requires_attribution(self):
        result = b2_robustness.combine_robustness(
            {"status": "adopt"}, "correctness_failed"
        )
        self.assertEqual(result["status"], "unresolved")
        self.assertTrue(result["attribution_required"])

    def test_unresolved_decision_load_stays_unresolved_when_robustness_passes(self):
        result = b2_robustness.combine_robustness(
            {"status": "unresolved"}, "passed", {"status": "adopt"}
        )
        self.assertEqual(result["status"], "unresolved")

    def test_registered_fixture_versions_and_dimensions(self):
        for fixture in b2_robustness.REGISTERED.values():
            dataset = json.loads(fixture["dataset"].read_text(encoding="utf-8"))
            self.assertEqual(dataset["dataset_version"], fixture["version"])
            self.assertEqual(len(dataset["instruments"]), 64)
            self.assertEqual(len(dataset["calendar"]), 252)


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
            self.assertIn("FileNotFoundError", report["preflight_error"])
            self.assertEqual(set(report["decision_loads"]), {"s2", "s3"})
            self.assertIn("primary", report["timing_boundaries"])
            self.assertIn("secondary", report["timing_boundaries"])
            self.assertEqual(report["sampling_protocol"]["serial_runs"], 20)
            self.assertIn("rss_baselines", report)
            self.assertIn("python_transitive_dependency_snapshot", report["environment"])
            self.assertIn("maintenance_cost_proxies", report)
            self.assertFalse(report["maintenance_cost_proxies"]["decision_inputs"])

    def test_rss_baselines_report_scope_and_platform_units(self):
        baselines = b2_matched._rss_baselines(Path(b2_matched.ROOT / "target/release/quant-research"), Path(sys.executable))
        if baselines["rust_empty_binary"]["status"] == "measured":
            self.assertIn("scope", baselines["rust_empty_binary"])
        self.assertIn("status", baselines["python_import_nautilus"])
        if baselines["python_import_nautilus"]["status"] == "measured":
            self.assertIn("raw_unit", baselines["python_import_nautilus"])

    @unittest.skipUnless(
        fixed_nautilus_available(), "fixed Nautilus 2.0.0rc5 runtime is unavailable"
    )
    def test_coordinator_report_exposes_both_decision_loads_and_required_sections(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "decision-loads.json"
            mode_evidence = Path(temporary) / "reset-parity.json"
            mode_evidence.write_text(json.dumps(valid_mode_evidence("failed")))
            binary = b2_matched.ROOT / "target/debug/quant-research"
            result = subprocess.run(
                [
                    sys.executable,
                    str(b2_matched.ROOT / "poc/poc0-benchmark/b2_matched.py"),
                    "--binary",
                    str(binary),
                    "--python",
                    sys.executable,
                    "--reset-parity-evidence",
                    str(mode_evidence),
                    "--output",
                    str(output),
                ],
                cwd=b2_matched.ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(set(report["decision_loads"]), {"s2", "s3"})
            self.assertIn("primary", report["timing_boundaries"])
            self.assertIn("secondary", report["timing_boundaries"])
            self.assertEqual(report["sampling_protocol"]["parallel_groups"], 5)
            self.assertIn("rss_baselines", report)
            self.assertIn("rustc_vv", report["environment"])
            self.assertIn("python_transitive_dependency_snapshot", report["environment"])
            self.assertEqual(report["nautilus_mode_selection"]["reset_parity"], "failed")
            self.assertEqual(report["nautilus_mode_selection"]["selected_mode"], "cached_conversion_new_engine")
            self.assertEqual(
                report["nautilus_mode_selection"]["revision"],
                subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=b2_matched.ROOT, text=True).strip(),
            )


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
        self.assertEqual(measurement["engine_mode"], "cached_conversion_new_engine")
        self.assertEqual(len(measurement["peak_rss_by_worker_bytes"]), 2)
        self.assertGreater(measurement["peak_rss_sum_upper_bound_bytes"], 0)
        self.assertEqual(len(measurement["secondary_boundary_samples_ns"]), 2)


if __name__ == "__main__":
    unittest.main()
