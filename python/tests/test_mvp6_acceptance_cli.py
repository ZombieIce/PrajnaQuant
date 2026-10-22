"""The MVP-6 acceptance CLI validates synthetic artifacts without market data."""
import json
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest

from scripts.mvp6_acceptance import (
    build_pit_input,
    fixed_universe_diagnostics,
    raw_integrity_failure,
)


ROOT = Path(__file__).resolve().parents[2]
CLI = ROOT / "scripts" / "mvp6_acceptance.py"


def run_artifacts():
    levels = {
        "summary": {"run_ids": ["run"], "summary_logical_hash": "summary", "table_hashes": {}},
        "standard": {"run_ids": ["run"], "summary_logical_hash": "summary", "table_hashes": {"account_valuations": "standard"}},
        "full": {"run_ids": ["run"], "summary_logical_hash": "summary", "table_hashes": {"account_valuations": "full", "fills": "fills"}},
    }
    independent = {
        "experiment_id": "experiment", "execution_id": "execution", "summary_logical_hash": "summary",
        "levels": levels,
    }
    return {
        "schema": "mvp6-acceptance-artifacts@1",
        "main_run": {
            "sizing": "lot", "rebalance": "full_target", "return_basis": "total_return",
            "actions": [
                {"instrument_id": "159919.SZSE", "kind": "dividend_entitlement"},
                {"instrument_id": "159560.SZSE", "kind": "share_conversion", "quantity_before": "100", "quantity_after": "200", "theoretical_quantity": "200", "residual_quantity": "0", "corporate_action_rounding": "0"},
            ],
            "terminal_settlement_version": "mvp6-terminal-settlement@1",
            "terminal_settlements": [],
        },
        "parity": {"days": [{"session_date": "2026-01-05", "vector_open_nav": "1000", "fast_open_nav": "1000"}]},
        "independent_runs": [independent, json.loads(json.dumps(independent))],
        "python_cli": {
            "cli": {"experiment_id": "experiment", "execution_id": "execution", "summary_logical_hash": "summary", "runs": {"run": {"status": "completed", "table_hashes": {"account_valuations": "account", "fills": "fills"}}}},
            "python": {"experiment_id": "experiment", "execution_id": "execution", "summary_logical_hash": "summary", "runs": {"run": {"status": "completed", "table_hashes": {"account_valuations": "account", "fills": "fills"}}}},
            "api_action": "replayed",
            "as_of_view_matches": True,
        },
        "accounting": {
            "valuations": [{
                "session_date": "2026-01-05", "valuation_point": "close", "cash": "970",
                "dividend_receivable": "5", "liquidation_receivable": "0", "equity": "1000",
                "positions": {"159919.SZSE": {"quantity": "5", "price": "5", "market_value": "25"}},
            }],
            "fills": [{"total_fees": "5", "costs": {"commission": "3", "minimum_commission_top_up": "2", "tax": "0"}, "slippage": "0"}],
            "forbidden_cells": [{"instrument_id": "159612.SZSE", "session_date": "2026-01-05", "state": "halted"}],
            "trades": [{"instrument_id": "159919.SZSE", "session_date": "2026-01-05"}],
            "fees_present": True,
        },
        "unknown_coverage": {
            "engineering_acceptance": "passed", "unknown_cells": 1, "denominator_cells": 100,
            "thresholds": {"total_unknown_percent": 5, "per_instrument_unknown_percent": 20},
            "instruments": [{"symbol": "159919", "cells": 20, "unknown_cells": 1}],
        },
        "raw_immutable": True,
    }


class Mvp6AcceptanceCliTests(unittest.TestCase):
    def invoke(self, artifacts):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        path = Path(temp.name) / "synthetic-artifacts.json"
        path.write_text(json.dumps(artifacts))
        return subprocess.run(
            [sys.executable, str(CLI), "verify", str(path)],
            cwd=ROOT, capture_output=True, text=True,
        )

    def test_cli_accepts_matching_synthetic_temp_artifacts(self):
        result = self.invoke(run_artifacts())
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertTrue(all(item["passed"] for item in report["checks"].values()))

    def test_cli_rejects_nav_mismatch_and_unknown_floor_overrun(self):
        artifacts = run_artifacts()
        artifacts["accounting"]["valuations"][0]["equity"] = "1001"
        artifacts["unknown_coverage"]["unknown_cells"] = 6
        result = self.invoke(artifacts)
        self.assertEqual(result.returncode, 1, result.stderr)
        report = json.loads(result.stdout)
        self.assertFalse(report["checks"]["daily_accounting"]["passed"])
        self.assertFalse(report["checks"]["unknown_floor"]["passed"])

    def test_runner_supplies_explicit_mvp6_fixed_universe(self):
        base = {"catalog": {"coverage": {"start": "2023-01-01", "end": "2026-09-30"}}}
        opening = {"semantic_version": "szse-derived-open@3"}

        pit = build_pit_input(base, opening)

        self.assertEqual(
            pit["fixed_universe"],
            {
                "version": "mvp6-fixed-etf@3",
                "codes": ["159919", "159560", "159522", "159612", "159952"],
                "minimum_code_count": 3,
            },
        )
        self.assertEqual(pit["market"], opening)
        self.assertNotIn("fixed_universe", base)

    def test_fixed_universe_diagnostics_keep_catalog_rule_versions_and_reasons(self):
        run_spec = {
            "pit": {"fixed_universe": {"codes": ["159919", "159560"], "minimum_code_count": 3}},
            "session_inputs": [{"eligible": {"universe": {"run_spec": {
                "catalog_version": "catalog@1",
                "rule_version": "rules@1",
                "fixed_list": ["159919"],
                "fixed_list_exclusions": {"159560": "no lifecycle evidence"},
            }}}}],
        }

        report = fixed_universe_diagnostics(run_spec)

        self.assertEqual(report["included_codes"], ["159919"])
        self.assertEqual(report["excluded_reasons"], {"159560": ["no lifecycle evidence"]})
        self.assertEqual(report["catalog_versions"], ["catalog@1"])
        self.assertEqual(report["rule_versions"], ["rules@1"])
        self.assertEqual(report["unresolved_codes"], [])

    def test_raw_integrity_failure_stops_before_engine_runs(self):
        self.assertIsNone(raw_integrity_failure({"raw-a": True, "raw-b": True}))
        self.assertEqual(
            raw_integrity_failure({"raw-a": True, "raw-b": False}),
            "frozen Raw integrity failed: 1 object missing or mismatched",
        )

    def test_cli_reports_early_failed_run_and_marks_dependent_checks_unverified(self):
        artifacts = {
            "schema": "mvp6-early-failure@1",
            "failure": {
                "stage": "main_run_summary",
                "status": "failed",
                "error": "synthetic P27-19 minimum-count failure",
                "fixed_universe": {
                    "version": "mvp6-fixed-etf@3",
                    "codes": ["159919", "159560", "159522", "159612", "159952"],
                    "minimum_code_count": 3,
                    "included_codes": ["159919", "159560"],
                    "excluded_reasons": {"159522": "synthetic catalog exclusion"},
                },
            },
            "unknown_coverage": {
                "engineering_acceptance": "passed",
                "unknown_cells": 0,
                "denominator_cells": 100,
                "thresholds": {"total_unknown_percent": 5, "per_instrument_unknown_percent": 20},
                "instruments": [{"symbol": "159919", "cells": 100, "unknown_cells": 0}],
            },
            "raw_immutable": True,
        }
        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        report = json.loads(result.stdout)
        self.assertFalse(report["passed"])
        self.assertEqual(set(report["checks"]), {
            "main_run", "daily_parity", "reproducibility", "python_as_of_and_cli",
            "daily_accounting", "unknown_floor", "frozen_raw_immutable",
        })
        self.assertEqual(report["checks"]["main_run"]["status"], "failed")
        self.assertEqual(report["checks"]["main_run"]["error"], artifacts["failure"]["error"])
        for name in ("daily_parity", "reproducibility", "python_as_of_and_cli", "daily_accounting"):
            self.assertEqual(report["checks"][name]["status"], "not_run")
            self.assertFalse(report["checks"][name]["passed"])
        self.assertEqual(report["checks"]["unknown_floor"]["status"], "passed")
        self.assertEqual(report["checks"]["frozen_raw_immutable"]["status"], "passed")


if __name__ == "__main__":
    unittest.main()
