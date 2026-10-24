"""The MVP-6 acceptance CLI validates synthetic artifacts without market data."""
import contextlib
import io
import hashlib
import json
import subprocess
import sys
import tempfile
from decimal import Decimal
from pathlib import Path
import unittest
from unittest.mock import patch

from scripts import mvp6_acceptance as acceptance_runner

from scripts.mvp6_acceptance import (
    build_pit_input,
    fixed_universe_diagnostics,
    identity_change,
    ledger_notional,
    ledger_oracle,
    parity_cost_fraction,
    native_fees_by_date,
    native_trajectories,
    normalized_trajectory,
    manifest_result_level,
    manifest_results,
    prior_run_identity,
    raw_integrity_failure,
    raw_hashes,
    raw_snapshot_unchanged,
    reconstruct_vector_close_nav,
    vector_open_nav,
    verify_manifest_level,
    write_post_main_failure_report,
)


ROOT = Path(__file__).resolve().parents[2]
CLI = ROOT / "scripts" / "mvp6_acceptance.py"
FIXED_CODES = ["159919", "159560", "159522", "159612", "159952"]
DATES = ["2026-01-05", "2026-01-06", "2026-01-07"]


def unknown_coverage():
    return {
        "engineering_acceptance": "passed", "unknown_cells": 5, "denominator_cells": 300,
        "thresholds": {"total_unknown_percent": 5, "per_instrument_unknown_percent": 20},
        "instruments": [
            {"symbol": code, "cells": 60, "unknown_cells": 1} for code in FIXED_CODES
        ],
    }


def universe_diagnostics():
    codes_by_session = [{"session_index": index, "session_date": date,
                         "included_codes": list(FIXED_CODES), "excluded_reasons": {}}
                        for index, date in enumerate(DATES)]
    return {
        "configuration": {
            "version": "mvp6-fixed-etf@3", "codes": list(FIXED_CODES), "minimum_code_count": 3,
        },
        "catalog_versions": ["catalog@1"], "rule_versions": ["rules@1"],
        "included_codes": list(FIXED_CODES), "excluded_reasons": {}, "unresolved_codes": [],
        "session_count": len(DATES), "sessions": codes_by_session,
    }


def parity_days():
    rows = []
    for index, date in enumerate(DATES):
        vector_components = {
            "available_cash_weight": 0.97, "dividend_receivable_weight": 0.005,
            "liquidation_receivable_weight": 0.0,
        }
        fast_components = {
            "cash_weight": 0.97, "invested_weight": 0.025,
            "dividend_receivable_weight": 0.005, "liquidation_receivable_weight": 0.0,
        }
        rows.append({
            "session_date": date,
            "vector_open_nav": "1", "fast_open_nav": "1",
            "vector_close_nav": "1", "fast_close_nav": "1",
            "vector_open_components": vector_components,
            "fast_open_components": fast_components,
            "vector_close_components": vector_components,
            "fast_close_components": fast_components,
            "vector_cost": "0", "fast_cost": "0",
            "vector_trajectory": [], "fast_trajectory": [],
        })
    return rows


def accounting_valuations():
    return [{
        "session_date": date, "valuation_point": point, "cash": "970",
        "dividend_receivable": "5", "liquidation_receivable": "0", "equity": "1000",
        "unallocated_capital": "0", "accounting_verified": True,
        "positions": {"159919.SZSE": {"quantity": "5", "price": "5", "market_value": "25"}},
    } for date in DATES for point in ("open_after", "close")]


def dividend_action(kind, date):
    return {
        "session_date": date, "portfolio_id": 1, "announcement_id": "dividend-1",
        "action_index": 0, "instrument_id": "159919.SZSE", "kind": kind,
        "quantity_before": "5", "quantity_after": "5", "gross_amount": "5",
        "tax_amount": "0", "net_amount": "5",
    }


def posting_fixture():
    accounts = []
    for index, date in enumerate(DATES):
        for point in ("open_after", "close"):
            paid = index == 2 and point == "close"
            receivable = "5" if index > 0 and not paid else "0"
            cash = "975" if paid else "970"
            split = index > 0
            accounts.append({
                "session_date": date, "valuation_point": point, "cash": cash,
                "dividend_receivable": receivable, "liquidation_receivable": "0",
                "equity": str(Decimal(cash) + Decimal(receivable) + 625),
                "unallocated_capital": "0", "accounting_verified": True,
                "positions": {
                    "159919.SZSE": {"quantity": "5", "price": "5", "market_value": "25"},
                    "159560.SZSE": {"quantity": "200" if split else "100",
                                    "price": "3" if split else "6", "market_value": "600"},
                },
            })
    portfolios = [{**json.loads(json.dumps(row)), "portfolio_id": 1, "scope": "vp"} for row in accounts]
    fills = [{
        "order": {"portfolio_id": 1, "instrument_id": instrument, "side": "Buy", "quantity": quantity},
        "executed_at": DATES[0] + "T09:30:00Z", "raw_price": price,
        "total_fees": fees, "cash_delta": delta,
        "costs": {"commission": fees, "minimum_commission_top_up": "0", "tax": "0"}, "slippage": "0",
    } for instrument, quantity, price, fees, delta in (
        ("159919.SZSE", "5", "5", "5", "-30"), ("159560.SZSE", "100", "6", "0", "-600"),
    )]
    return {"expected_sessions": DATES, "initial_portfolios": [{"id": 1, "capital": "1600"}],
            "dividend_tax_rate": "0", "portfolio_valuations": portfolios,
            "account_valuations": accounts, "fills": fills, "fee_settlements": []}


def run_artifacts():
    postings = posting_fixture()
    levels = {
        "summary": {"run_ids": ["run"], "summary_logical_hash": "summary", "table_hashes": {},
                    "runs": [{"status": "completed"}]},
        "standard": {"run_ids": ["run"], "summary_logical_hash": "summary",
                     "table_hashes": {"account_valuations": "standard"},
                     "runs": [{"status": "completed"}]},
        "full": {"run_ids": ["run"], "summary_logical_hash": "summary",
                 "table_hashes": {"account_valuations": "full", "fills": "fills"},
                 "runs": [{"status": "completed"}]},
    }
    independent = {
        "experiment_id": "experiment", "execution_id": "execution", "summary_logical_hash": "summary",
        "levels": levels,
    }
    python_cli_snapshot = {
        "experiment_id": "experiment",
        "execution_id": "execution",
        "default_result_level": "summary",
        "result_level": "full",
        "summary_logical_hash": "summary",
        "runs": {
            "run": {
                "status": "completed",
                "result_level": "full",
                "table_hashes": {"account_valuations": "account", "fills": "fills"},
                "not_saved": [],
                "not_produced_by_engine": [],
                "accounting_verified": True,
            },
        },
    }
    return {
        "schema": "mvp6-acceptance-artifacts@1",
        "fixed_universe": universe_diagnostics(),
        "main_run": {
            "sizing": "lot", "rebalance": "full_target", "return_basis": "total_return",
            "actions": [
                dividend_action("dividend_entitlement", DATES[0]),
                dividend_action("dividend_receivable", DATES[1]),
                dividend_action("dividend_paid", DATES[2]),
                {"session_date": DATES[1], "portfolio_id": 1, "announcement_id": "split-1",
                 "action_index": 1, "instrument_id": "159560.SZSE", "kind": "share_conversion",
                 "quantity_before": "100", "share_ratio": "2", "quantity_after": "200",
                 "theoretical_quantity": "200", "residual_quantity": "0",
                 "corporate_action_rounding": "0", "valuation_price": "6"},
            ],
            "postings": postings,
            "terminal_settlement_version": "mvp6-terminal-settlement@1",
            "terminal_settlements": [],
        },
        "parity": {
            "initial_cash": "1000", "expected_sessions": DATES, "days": parity_days(),
            "fast_valuations": accounting_valuations(),
        },
        "independent_runs": [independent, json.loads(json.dumps(independent))],
        "python_cli": {
            "cli": json.loads(json.dumps(python_cli_snapshot)),
            "python": json.loads(json.dumps(python_cli_snapshot)),
            "api_action": "replayed",
            "api_entry_level": "summary",
            "actual_result_level": "full",
            "table_hashes_match": True,
            "as_of_view_matches": True,
        },
        "accounting": {
            "expected_sessions": DATES,
            "session_input_count": len(DATES),
            "valuations": postings["account_valuations"],
            "fills": [{"session_date": fill["executed_at"][:10],
                       "instrument_id": fill["order"]["instrument_id"],
                       "total_fees": fill["total_fees"], "costs": fill["costs"], "slippage": fill["slippage"]}
                      for fill in postings["fills"]],
            "forbidden_cells": [{"instrument_id": "159612.SZSE", "session_date": "2026-01-05", "state": "halted"}],
            "trades": [{"instrument_id": "159919.SZSE", "session_date": "2026-01-05"}],
            "fees_present": True,
        },
        "unknown_coverage": {
            **unknown_coverage(),
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
        weights = report["checks"]["daily_accounting"]["weight_summaries"][0]
        self.assertEqual(weights["cash_remainder"]["amount"], "970")
        self.assertAlmostEqual(float(weights["position_weight_total"]), 625 / 1595)
        self.assertAlmostEqual(float(weights["cash_and_position_weight"]), 1)
        self.assertEqual(Decimal(weights["receivable_weight"]), Decimal(0))
        self.assertAlmostEqual(float(weights["total_weight"]), 1)

    def test_dividend_postings_reject_consistently_inflated_action_chain(self):
        artifacts = run_artifacts()
        for action in artifacts["main_run"]["actions"][:3]:
            action.update(gross_amount="5000000", net_amount="5000000")
        result = self.invoke(artifacts)
        checks = json.loads(result.stdout)["checks"]
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(checks["main_run"]["passed"])
        self.assertFalse(checks["daily_accounting"]["passed"])
        self.assertTrue(checks["main_run"]["action_postings"]["errors"])

    def test_postings_reject_registration_holdings_split_and_account_contradictions(self):
        mutations = (
            lambda a: [event.update(quantity_before="6", quantity_after="6")
                       for event in a["main_run"]["actions"][:3]],
            lambda a: a["main_run"]["actions"][3].update(quantity_before="50", quantity_after="100",
                theoretical_quantity="100", residual_quantity="0"),
            lambda a: a["main_run"]["postings"]["fills"][0].update(cash_delta="0"),
            lambda a: a["main_run"]["postings"]["portfolio_valuations"][0].update(portfolio_id=2),
            lambda a: a["main_run"]["postings"].pop("portfolio_valuations"),
        )
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                artifacts = run_artifacts()
                mutate(artifacts)
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(json.loads(result.stdout)["checks"]["main_run"]["action_postings"]["passed"])

    def test_postings_account_for_same_session_trades_fees_and_multiple_payments(self):
        artifacts = run_artifacts()
        main = artifacts["main_run"]
        postings = main["postings"]
        for action in list(main["actions"][:3]):
            main["actions"].append({**action, "announcement_id": "second-dividend", "action_index": 2,
                                    "gross_amount": "2", "net_amount": "2"})
        postings["fills"].append({
            "order": {"portfolio_id": 1, "instrument_id": "159919.SZSE", "side": "Sell", "quantity": "1"},
            "executed_at": DATES[2] + "T09:30:00Z", "raw_price": "5", "total_fees": "1", "cash_delta": "4",
            "costs": {"commission": "1", "minimum_commission_top_up": "0", "tax": "0"}, "slippage": "0",
        })
        postings["fee_settlements"].append({
            "portfolio_id": 1, "instrument_id": "159919.SZSE", "executed_at": DATES[2] + "T09:30:00Z",
            "total_fees": "2", "cash_delta": "-2", "slippage": "0",
            "costs": {"commission": "2", "minimum_commission_top_up": "0", "tax": "0"},
        })
        for rows in (postings["account_valuations"], postings["portfolio_valuations"]):
            for row in rows:
                if row["session_date"] == DATES[1]:
                    row.update(dividend_receivable="7", equity="1602")
                elif row["session_date"] == DATES[2]:
                    row["positions"]["159919.SZSE"].update(quantity="4", market_value="20")
                    row.update(cash="972" if row["valuation_point"] == "open_after" else "979",
                               dividend_receivable="7" if row["valuation_point"] == "open_after" else "0", equity="1599")
        result = self.invoke(artifacts)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        # A payment contradiction cannot hide behind an otherwise valid static NAV.
        for rows in (postings["account_valuations"], postings["portfolio_valuations"]):
            rows[-1].update(cash="980", equity="1600")
        result = self.invoke(artifacts)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(json.loads(result.stdout)["checks"]["main_run"]["action_postings"]["passed"])

    def test_initialization_failures_persist_all_outcomes_and_original_error(self):
        cases = ("missing_coverage", "malformed_coverage", "malformed_unknown", "malformed_pit",
                 "malformed_opening", "opening_mismatch", "missing_raw", "missing_cli")
        for case in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                root = home / "PrajnaData/mvp-6"
                inputs = root / "acceptance"
                inputs.mkdir(parents=True)
                cli = home / "dummy-cli"
                if case != "missing_cli":
                    cli.touch()
                if case != "missing_coverage":
                    (inputs / "mvp6-coverage.json").write_text("{" if case == "malformed_coverage" else json.dumps({"corporate_action_dataset": {"dsv": "synthetic"}}))
                (inputs / "mvp6-unknown-coverage.json").write_text("[" if case == "malformed_unknown" else json.dumps({"catalog_version": "synthetic"}))
                pit = inputs / "pit.json"
                pit.write_text("{" if case == "malformed_pit" else json.dumps({"catalog": {"coverage": {"start": "2022-10-10", "end": "2026-09-30"}}}))
                (inputs / "mvp6-opening-evidence-v3.json").write_text("[" if case == "malformed_opening" else json.dumps({
                    "semantic_version": "szse-derived-open@3",
                    "start": "2023-01-01" if case == "opening_mismatch" else "2022-10-10", "end": "2026-09-30"}))
                stdout = io.StringIO()
                with patch.object(Path, "home", return_value=home), patch.object(sys, "argv", [
                    str(CLI), "run", "--root", str(root), "--cli", str(cli), "--pit-input", str(pit),
                ]), contextlib.redirect_stdout(stdout):
                    code = acceptance_runner.main()
                self.assertEqual(code, 1)
                summary = json.loads(stdout.getvalue())
                report = json.loads(Path(summary["report"]).read_text())
                checks = report["verification"]["checks"]
                self.assertEqual(len(checks), 7)
                self.assertEqual(checks["main_run"]["status"], "failed")
                self.assertTrue(report["failure"]["error"])
                for name, check in checks.items():
                    self.assertFalse(check["passed"])
                    if name != "main_run":
                        expected = "failed" if case == "missing_raw" and name in {"unknown_floor", "frozen_raw_immutable"} else "not_run"
                        self.assertEqual(check["status"], expected)
                self.assertEqual(checks["main_run"]["error"], report["failure"]["error"])

    def test_cli_rejects_negative_cash_weight_even_when_nav_identity_holds(self):
        artifacts = run_artifacts()
        valuation = artifacts["accounting"]["valuations"][0]
        valuation["cash"] = "-10"
        valuation["equity"] = "20"

        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        accounting = json.loads(result.stdout)["checks"]["daily_accounting"]
        self.assertFalse(accounting["passed"])
        self.assertEqual(accounting["weight_errors"][0]["kind"], "negative_cash_weight")

    def test_cli_rejects_negative_position_and_receivable_weights(self):
        cases = (
            ("negative_position_weight", lambda row: (
                row["positions"]["159919.SZSE"].update(price="-5", market_value="-25"),
                row.update(equity="950"),
            )),
            ("negative_receivable_weight", lambda row: (
                row.update(dividend_receivable="-5", equity="990"),
            )),
        )
        for expected_kind, mutate in cases:
            with self.subTest(expected_kind=expected_kind):
                artifacts = run_artifacts()
                mutate(artifacts["accounting"]["valuations"][0])
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                accounting = json.loads(result.stdout)["checks"]["daily_accounting"]
                self.assertFalse(accounting["passed"])
                self.assertIn(expected_kind, [error["kind"] for error in accounting["weight_errors"]])

    def test_cli_rejects_nav_mismatch_and_unknown_floor_overrun(self):
        artifacts = run_artifacts()
        artifacts["accounting"]["valuations"][0]["equity"] = "1001"
        artifacts["unknown_coverage"]["unknown_cells"] = 16
        result = self.invoke(artifacts)
        self.assertEqual(result.returncode, 1, result.stderr)
        report = json.loads(result.stdout)
        self.assertFalse(report["checks"]["daily_accounting"]["passed"])
        self.assertFalse(report["checks"]["unknown_floor"]["passed"])

    def test_unknown_coverage_requires_positive_reconciled_fixed_universe_denominators(self):
        cases = (
            lambda coverage: coverage.update(denominator_cells=0),
            lambda coverage: coverage.update(instruments=[]),
            lambda coverage: coverage["instruments"].pop(),
            lambda coverage: coverage.update(unknown_cells=4),
            lambda coverage: coverage["instruments"][0].update(cells=0),
        )
        for mutate in cases:
            with self.subTest(mutation=mutate):
                artifacts = run_artifacts()
                mutate(artifacts["unknown_coverage"])
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(json.loads(result.stdout)["checks"]["unknown_floor"]["passed"])

    def test_unknown_coverage_cannot_raise_ticket_limits(self):
        artifacts = run_artifacts()
        coverage = artifacts["unknown_coverage"]
        coverage["unknown_cells"] = 100
        coverage["denominator_cells"] = 300
        coverage["thresholds"] = {
            "total_unknown_percent": 100,
            "per_instrument_unknown_percent": 100,
        }
        coverage["instruments"] = [
            {"symbol": code, "cells": 60, "unknown_cells": 20} for code in FIXED_CODES
        ]

        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        check = json.loads(result.stdout)["checks"]["unknown_floor"]
        self.assertFalse(check["passed"])
        self.assertIn("total_unknown_percent_over_limit", [e["kind"] for e in check["errors"]])
        self.assertIn("per_instrument_unknown_percent_over_limit", [e["kind"] for e in check["errors"]])

    def test_main_run_requires_dividend_transitions_and_consistent_split_arithmetic(self):
        cases = (
            lambda actions: actions.__delitem__(1),
            lambda actions: actions.__delitem__(2),
            lambda actions: actions[0].update(kind="dividend_paid"),
            lambda actions: actions[1].update(net_amount="4"),
            lambda actions: actions[2].update(announcement_id="orphan-payment"),
            lambda actions: actions[1].update(instrument_id="159919.WRONG"),
            lambda actions: actions[2].update(instrument_id="159919.WRONG"),
            lambda actions: actions[2].update(portfolio_id=2),
            lambda actions: actions[2].update(action_index=99),
            lambda actions: actions[3].update(quantity_after="199"),
            lambda actions: actions[3].update(residual_quantity="1"),
            lambda actions: actions[3].update(corporate_action_rounding=None),
            lambda actions: actions[3].update(
                quantity_before="101", share_ratio="1.5", theoretical_quantity="151.5", quantity_after="151",
                residual_quantity="0.5", corporate_action_rounding="-999",
            ),
        )
        for mutate in cases:
            with self.subTest(mutation=mutate):
                artifacts = run_artifacts()
                mutate(artifacts["main_run"]["actions"])
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                checks = json.loads(result.stdout)["checks"]
                self.assertFalse(checks["main_run"]["passed"])

    def test_daily_parity_requires_full_sessions_nav_components_cost_and_trajectory(self):
        mutations = (
            lambda days: days.pop(),
            lambda days: days[0].update(fast_close_nav="0.999"),
            lambda days: days[0]["fast_open_components"].update(invested_weight=0.0),
            lambda days: days[1].update(vector_cost="0.001"),
            lambda days: days[1].update(vector_trajectory=[{"kind": "executed", "blocked": [], "skipped_buys": []}]),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                artifacts = run_artifacts()
                mutate(artifacts["parity"]["days"])
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(json.loads(result.stdout)["checks"]["daily_parity"]["passed"])

    def test_ledger_oracle_uses_scale_18_ties_even_without_tolerance(self):
        for quantity, expected in (("0.000000000000000001", "0"),
                                   ("0.000000000000000003", "0.000000000000000002"),
                                   ("123456.123456123456123456", "15234.485634485634485634")):
            price = "0.1234" if quantity.startswith("123") else "0.5"
            with self.subTest(quantity=quantity):
                value = ledger_notional(Decimal(quantity), Decimal(price))
                self.assertEqual(value, Decimal(expected))
                row = {"cash": "1", "dividend_receivable": "0", "positions": {
                    "synthetic": {"quantity": quantity, "price": price, "market_value": expected},
                }}
                self.assertEqual(ledger_oracle(row)[2], value)
                row["positions"]["synthetic"]["market_value"] = str(value + Decimal("1e-18"))
                with self.assertRaises(ValueError):
                    ledger_oracle(row)

    def test_daily_parity_recomputes_the_fast_ledger_as_an_independent_oracle(self):
        artifacts = run_artifacts()
        position = artifacts["parity"]["fast_valuations"][0]["positions"]["159919.SZSE"]
        position["price"] = "6"

        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        check = json.loads(result.stdout)["checks"]["daily_parity"]
        self.assertFalse(check["passed"])
        self.assertIn("oracle", [error.get("kind") for error in check["errors"]])

    def test_daily_parity_matches_the_documented_dividend_hand_calculation(self):
        artifacts = run_artifacts()
        def valuation(date, point, price, market_value, cash, receivable, equity):
            return {
                "session_date": date, "valuation_point": point, "cash": str(cash),
                "dividend_receivable": str(receivable), "liquidation_receivable": "0",
                "equity": str(equity), "unallocated_capital": "0", "accounting_verified": True,
                "positions": {"159919.SZSE": {
                    "quantity": "100", "price": str(price), "market_value": market_value,
                }},
            }

        artifacts["parity"]["fast_valuations"] = [
            valuation(DATES[0], "open_after", "10", "1000", "0", "0", "1000"),
            valuation(DATES[0], "close", "10", "1000", "0", "0", "1000"),
            valuation(DATES[1], "open_after", "9.9", "990", "0", "10", "1000"),
            valuation(DATES[1], "close", "9.9", "990", "0", "10", "1000"),
            valuation(DATES[2], "open_after", "9.9", "990", "10", "0", "1000"),
            valuation(DATES[2], "close", "10.89", "1089", "10", "0", "1099"),
        ]
        navs = (("1", "1"), ("1", "1"), ("1", "1.099"))
        components = (
            ({"available_cash_weight": 0, "dividend_receivable_weight": 0, "liquidation_receivable_weight": 0},
             {"cash_weight": 0, "invested_weight": 1, "dividend_receivable_weight": 0, "liquidation_receivable_weight": 0}),
            ({"available_cash_weight": 0, "dividend_receivable_weight": 0.01, "liquidation_receivable_weight": 0},
             {"cash_weight": 0, "invested_weight": 0.99, "dividend_receivable_weight": 0.01, "liquidation_receivable_weight": 0}),
            ({"available_cash_weight": 0.01, "dividend_receivable_weight": 0, "liquidation_receivable_weight": 0},
             {"cash_weight": 0.01, "invested_weight": 0.99, "dividend_receivable_weight": 0, "liquidation_receivable_weight": 0}),
        )
        for index, day in enumerate(artifacts["parity"]["days"]):
            day["vector_open_nav"], day["fast_open_nav"] = navs[index][0], navs[index][0]
            day["vector_close_nav"], day["fast_close_nav"] = navs[index][1], navs[index][1]
            vector_components, fast_components = components[index]
            day["vector_open_components"] = vector_components
            day["vector_close_components"] = vector_components
            day["fast_open_components"] = fast_components
            day["fast_close_components"] = fast_components
        final_day = artifacts["parity"]["days"][-1]
        final_cash_weight = 10 / 1099
        final_invested_weight = 1089 / 1099
        final_day["vector_close_components"] = {
            "available_cash_weight": final_cash_weight,
            "dividend_receivable_weight": 0,
            "liquidation_receivable_weight": 0,
        }
        final_day["fast_close_components"] = {
            "cash_weight": final_cash_weight,
            "invested_weight": final_invested_weight,
            "dividend_receivable_weight": 0,
            "liquidation_receivable_weight": 0,
        }

        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 0, result.stderr)
        parity = json.loads(result.stdout)["checks"]["daily_parity"]
        self.assertTrue(parity["passed"])
        self.assertEqual(parity["oracle"]["valuation_points"], 6)

    def test_vector_close_nav_revalues_the_same_session_not_the_next_open(self):
        session = {
            "nav": "1.1", "gross_return": "0.1",
            "portfolio_components": {
                "available_cash_weight": "0", "dividend_receivable_weight": "0",
                "liquidation_receivable_weight": "0",
            },
        }
        instrument = "159919.SZSE"

        self.assertEqual(vector_open_nav(session), Decimal(1))
        close_nav = reconstruct_vector_close_nav(
            session, {instrument: "1"}, {instrument: "10"}, {instrument: "10.5"},
            Decimal("1000"),
        )
        self.assertEqual(close_nav, Decimal("1.05"))
        self.assertNotEqual(close_nav, Decimal(str(session["nav"])))

        dividend_session = {
            "nav": "1.0", "gross_return": "0",
            "portfolio_components": {
                "available_cash_weight": "0", "dividend_receivable_weight": "0.01",
                "liquidation_receivable_weight": "0",
            },
        }
        dividend_close = reconstruct_vector_close_nav(
            dividend_session, {instrument: "0.99"}, {instrument: "9.9"}, {instrument: "10.89"},
            Decimal("1000"),
        )
        self.assertEqual(dividend_close, Decimal("1.099"))

    def test_native_fee_mapping_includes_fee_only_settlements_once(self):
        fills = [{"fill": {"executed_at": "2026-01-05T09:30:00Z", "total_fees": "2"}}]
        settlements = [{"executed_at": "2026-01-05T09:30:00Z", "total_fees": "3"}]
        self.assertEqual(native_fees_by_date(fills, settlements), {DATES[0]: Decimal(5)})
        self.assertEqual(native_fees_by_date([], settlements), {DATES[0]: Decimal(3)})

    def test_native_risk_exit_mapping_preserves_status_instrument_and_pending(self):
        rows = [
            {"record_type": "risk_exit", "attempt": {
                "session_date": DATES[0], "instrument_id": "159919.SZSE", "executed": False}},
            {"record_type": "risk_exit_pending_at_end", "instrument_id": "159919.SZSE"},
        ]
        vector = native_trajectories(rows, DATES[-1])
        fast = native_trajectories([{**row, "portfolio_id": 1} for row in rows], DATES[-1])
        self.assertEqual(vector, fast)
        self.assertEqual(vector[DATES[-1]][0]["kind"], "risk_exit_pending_at_end")
        self.assertEqual(vector[DATES[0]][0]["blocked"], ["159919.SZSE"])
        rows[0]["attempt"]["executed"] = True
        changed = native_trajectories(rows, DATES[-1])
        self.assertNotEqual(normalized_trajectory(vector[DATES[0]]),
                            normalized_trajectory(changed[DATES[0]]))

    def test_parity_cost_uses_current_open_before_fees_not_previous_close(self):
        row = accounting_valuations()[0]
        # Current open-before equity is 1005; previous close may differ.
        self.assertEqual(parity_cost_fraction(row, Decimal("5")), Decimal(5) / Decimal(1005))
        self.assertNotEqual(parity_cost_fraction(row, Decimal("5")), Decimal(5) / Decimal(900))
        with self.assertRaises(ValueError):
            parity_cost_fraction(row, Decimal("-1"))

    def test_python_cli_requires_summary_replay_promoted_level_and_table_hashes(self):
        mutations = (
            lambda python_cli: python_cli.update(actual_result_level="standard"),
            lambda python_cli: python_cli.update(api_entry_level="standard"),
            lambda python_cli: python_cli.update(api_action="created"),
            lambda python_cli: python_cli.update(table_hashes_match=False),
            lambda python_cli: python_cli["python"]["runs"]["run"]["table_hashes"].update(
                account_valuations="tampered",
            ),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                artifacts = run_artifacts()
                mutate(artifacts["python_cli"])
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                check = json.loads(result.stdout)["checks"]["python_as_of_and_cli"]
                self.assertFalse(check["passed"])

    def test_manifest_levels_use_promoted_runs_not_the_creation_default(self):
        run = {
            "run_id": "run", "status": "completed", "result_level": "full",
            "table_hashes": {"account_valuations": "hash"}, "not_saved": [],
            "not_produced_by_engine": [], "accounting_verified": True,
        }
        manifest = {
            "default_result_level": "summary", "summary_logical_hash": "summary",
            "runs": [run],
        }

        self.assertEqual(manifest_result_level(manifest), "full")
        verify_manifest_level(manifest, "full")
        with self.assertRaises(ValueError):
            verify_manifest_level(manifest, "standard")
        report = manifest_results(manifest)
        self.assertEqual(report["default_result_level"], "summary")
        self.assertEqual(report["result_level"], "full")
        self.assertEqual(report["runs"]["run"]["table_hashes"], {"account_valuations": "hash"})

        mixed = json.loads(json.dumps(manifest))
        mixed["runs"].append({**run, "run_id": "other", "result_level": "standard"})
        with self.assertRaises(ValueError):
            manifest_result_level(mixed)

    def test_daily_accounting_requires_every_session_at_both_valuation_points(self):
        artifacts = run_artifacts()
        artifacts["accounting"]["valuations"].pop()
        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        accounting = json.loads(result.stdout)["checks"]["daily_accounting"]
        self.assertFalse(accounting["passed"])
        self.assertIn("valuation_coverage", [error["kind"] for error in accounting["valuation_errors"]])

    def test_fixed_universe_diagnostics_enforce_five_codes_and_minimum_per_session(self):
        cases = (
            lambda diagnostics: diagnostics["sessions"][0]["included_codes"].pop(),
            lambda diagnostics: diagnostics["configuration"].update(codes=FIXED_CODES[:4]),
            lambda diagnostics: diagnostics.update(catalog_versions=[]),
        )
        for mutate in cases:
            with self.subTest(mutation=mutate):
                artifacts = run_artifacts()
                mutate(artifacts["fixed_universe"])
                result = self.invoke(artifacts)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(json.loads(result.stdout)["checks"]["main_run"]["passed"])

    def test_empty_full_run_cannot_pass_reproducibility_vacuously(self):
        artifacts = run_artifacts()
        artifacts["independent_runs"][0]["levels"]["full"]["runs"] = []
        artifacts["independent_runs"][1]["levels"]["full"]["runs"] = []
        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(json.loads(result.stdout)["checks"]["reproducibility"]["passed"])

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
        self.assertEqual(
            raw_integrity_failure({}),
            "frozen Raw integrity failed: empty or malformed inventory",
        )

    def test_raw_inventory_rejects_empty_records_and_missing_sha256(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            inventory = root / "inventory.json"
            for records in ([], [{"url": "local-only-no-hash"}]):
                inventory.write_text(json.dumps({"request_records": records}))
                snapshot = raw_hashes(root, inventory)
                self.assertEqual(
                    raw_integrity_failure(snapshot),
                    "frozen Raw integrity failed: empty or malformed inventory",
                )
            inventory.write_bytes(b"\xff\xfe\xfa")
            self.assertEqual(
                raw_integrity_failure(raw_hashes(root, inventory)),
                "frozen Raw integrity failed: empty or malformed inventory",
            )

    def test_raw_inventory_checks_content_at_the_sha256_address(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            body = b"synthetic raw body"
            digest = hashlib.sha256(body).hexdigest()
            raw = root / "raw" / "sha256" / digest[:2] / digest
            raw.parent.mkdir(parents=True)
            raw.write_bytes(body)
            inventory = root / "inventory.json"
            content = {"request_records": [{"url": "first", "sha256": f"sha256:{digest}"}]}
            inventory.write_text(json.dumps(content))
            before = raw_hashes(root, inventory)
            self.assertIsNone(raw_integrity_failure(before))

            content["request_records"][0]["url"] = "changed-metadata"
            inventory.write_text(json.dumps(content))
            after = raw_hashes(root, inventory)
            self.assertFalse(raw_snapshot_unchanged(before, after))

            raw.write_bytes(b"tampered synthetic raw body")
            self.assertEqual(
                raw_integrity_failure(raw_hashes(root, inventory)),
                "frozen Raw integrity failed: 1 object missing or mismatched",
            )

    def test_raw_inventory_separates_failed_attempt_provenance_from_frozen_objects(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            body = b"synthetic frozen raw body"
            digest = hashlib.sha256(body).hexdigest()
            raw = root / "raw" / "sha256" / digest[:2] / digest
            raw.parent.mkdir(parents=True)
            raw.write_bytes(body)
            inventory = root / "inventory.json"
            inventory.write_text(json.dumps({"request_records": [
                {"url": "successful", "sha256": f"sha256:{digest}"},
                {"url": "failed", "sha256": "", "error": "timeout; body hash unavailable"},
            ]}))
            snapshot = raw_hashes(root, inventory)

            self.assertIsNone(raw_integrity_failure(snapshot))
            self.assertEqual(snapshot.unavailable_records, 1)
            self.assertEqual(len(snapshot.checked), 1)
            self.assertTrue(all(snapshot.checked.values()))

            inventory.write_text(json.dumps({"request_records": [
                {"url": "failed-only", "sha256": "unknown", "error": "capture interrupted"},
            ]}))
            self.assertEqual(
                raw_integrity_failure(raw_hashes(root, inventory)),
                "frozen Raw integrity failed: empty or malformed inventory",
            )

            inventory.write_text(json.dumps({"request_records": [
                {"url": "unexplained", "sha256": "unknown"},
            ]}))
            self.assertEqual(
                raw_integrity_failure(raw_hashes(root, inventory)),
                "frozen Raw integrity failed: empty or malformed inventory",
            )

    def test_identity_baseline_skips_newer_prior_run_with_explicit_universe(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            acceptance = Path(temp_dir)
            legacy = acceptance / "issue-234-20261021-legacy"
            explicit = acceptance / "issue-234-20261022-explicit"
            current = acceptance / "issue-234-20261023-current"
            for run_dir, run_id, pit_input in (
                (legacy, "legacy-run", {"catalog": {"coverage": {}}}),
                (explicit, "explicit-run", {"fixed_universe": {"codes": ["159919"]}}),
            ):
                run = run_dir / "independent-run-1"
                run.mkdir(parents=True)
                (run / "pit-input.json").write_text(json.dumps(pit_input))
                (run / "main-summary-cli.json").write_text(json.dumps({
                    "stdout": json.dumps({"exp": f"exp-{run_id}", "exe": f"exe-{run_id}"}),
                }))

            previous = prior_run_identity(acceptance, current)

        self.assertEqual(previous["experiment_id"], "exp-legacy-run")
        self.assertEqual(previous["execution_id"], "exe-legacy-run")
        self.assertEqual(previous["fixed_universe"], "omitted; legacy default applied")

    def test_identity_change_is_unknown_without_a_legacy_baseline(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            current_output = Path(temp_dir) / "current"
            comparison = identity_change(
                Path(temp_dir), current_output, {"exp": "new-exp", "exe": "new-exe"},
            )

        self.assertIsNone(comparison["previous"])
        self.assertIsNone(comparison["experiment_changed"])
        self.assertIsNone(comparison["execution_changed"])

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
                **unknown_coverage(),
                "unknown_cells": 0,
                "instruments": [{"symbol": code, "cells": 60, "unknown_cells": 0}
                                for code in FIXED_CODES],
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

    def test_post_main_phase_failure_still_emits_all_blocking_checks(self):
        standard = run_artifacts()
        non_blocking = {
            "small_grid": {"status": "not_run", "reason": "Not required for the engineering ledger gate."},
            "accurate_smoke": {"status": "not_run", "reason": "No Accurate Event worker was configured."},
            "legacy_comparison": {"status": "not_run", "reason": "Legacy comparator is unavailable."},
        }
        artifacts = {
            "schema": "mvp6-post-main-failure@1",
            "failure": {"criterion": "daily_parity", "error": "synthetic parity read failure"},
            "engine_completed": True,
            "main_run": standard["main_run"],
            "fixed_universe": standard["fixed_universe"],
            "session_input_count": len(DATES),
            "unknown_coverage": unknown_coverage(),
            "raw_immutable": False,
            "non_blocking": non_blocking,
        }
        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(set(report["checks"]), {
            "main_run", "daily_parity", "reproducibility", "python_as_of_and_cli",
            "daily_accounting", "unknown_floor", "frozen_raw_immutable",
        })
        self.assertEqual(report["checks"]["daily_parity"]["status"], "failed")
        self.assertEqual(report["checks"]["main_run"]["status"], "passed")
        self.assertTrue(report["checks"]["main_run"]["engine_completed"])
        self.assertEqual(report["checks"]["frozen_raw_immutable"]["status"], "failed")
        self.assertEqual(report["non_blocking"], non_blocking)

        artifacts["non_blocking"]["accurate_smoke"].pop("reason")
        invalid_result = self.invoke(artifacts)
        invalid_report = json.loads(invalid_result.stdout)
        self.assertIn("accurate_smoke is not_run without a reason", invalid_report["non_blocking_errors"])

    def test_post_main_failure_never_passes_unverified_main_run(self):
        artifacts = {
            "schema": "mvp6-post-main-failure@1",
            "failure": {"criterion": "daily_parity", "error": "synthetic parity read failure"},
            "engine_completed": True,
            "unknown_coverage": unknown_coverage(),
            "raw_immutable": True,
            "non_blocking": {
                "small_grid": {"status": "not_run", "reason": "Not required."},
                "accurate_smoke": {"status": "not_run", "reason": "No worker."},
                "legacy_comparison": {"status": "not_run", "reason": "Unavailable."},
            },
        }
        result = self.invoke(artifacts)

        self.assertEqual(result.returncode, 1, result.stderr)
        main = json.loads(result.stdout)["checks"]["main_run"]
        self.assertFalse(main["passed"])
        self.assertEqual(main["status"], "not_run")
        self.assertTrue(main["engine_completed"])
        self.assertIn("not verified", main["reason"])

        standard = run_artifacts()
        artifacts["main_run"] = json.loads(json.dumps(standard["main_run"]))
        artifacts["fixed_universe"] = standard["fixed_universe"]
        artifacts["session_input_count"] = len(DATES)
        artifacts["main_run"]["actions"] = []
        invalid_result = self.invoke(artifacts)
        invalid_main = json.loads(invalid_result.stdout)["checks"]["main_run"]
        self.assertFalse(invalid_main["passed"])
        self.assertEqual(invalid_main["status"], "failed")

        artifacts.pop("engine_completed")
        artifacts["main_run"] = json.loads(json.dumps(standard["main_run"]))
        missing_engine = self.invoke(artifacts)
        missing_engine_main = json.loads(missing_engine.stdout)["checks"]["main_run"]
        self.assertFalse(missing_engine_main["passed"])
        self.assertEqual(missing_engine_main["status"], "not_run")

    def test_post_main_report_retains_source_context_and_raw_inventory_location(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            acceptance = root / "acceptance"
            output = acceptance / "issue-234-test"
            output.mkdir(parents=True)
            body = b"synthetic frozen raw body"
            digest = hashlib.sha256(body).hexdigest()
            raw = root / "raw" / "sha256" / digest[:2] / digest
            raw.parent.mkdir(parents=True)
            raw.write_bytes(body)
            inventory = acceptance / "inventory.json"
            inventory.write_text(json.dumps({"request_records": [
                {"url": "successful", "sha256": digest},
            ]}))
            before = raw_hashes(root, inventory)

            write_post_main_failure_report(
                output=output, acceptance=acceptance, raw_inventory=inventory,
                unknown=unknown_coverage(), criterion="daily_parity",
                error="synthetic post-main failure", raw_before=before,
                coverage={"source_status": {"tdx": "captured"}, "p21": {"status": "source_gap"}},
                pit_base={"risk_notices": [{"announcement_id": "risk-1"}]},
                engine_completed=True,
            )
            report = json.loads((output / "mvp6-acceptance-report.json").read_text())

        source = report["source_evidence"]
        self.assertEqual(source["decision_window"]["start"], "2023-01-01")
        self.assertEqual(source["universe_claim"], "retrospective_static")
        self.assertTrue(source["survivorship_bias"])
        self.assertEqual(source["return_basis"], "unknown (post-main failure before Run Spec verification)")
        self.assertEqual(source["p21_status"], "source_gap")
        self.assertEqual(source["risk_notice_count"], 1)
        self.assertEqual(report["local_artifacts"]["frozen_raw_inventory"], str(inventory))
        self.assertTrue(report["raw_immutability"]["passed"])
        self.assertEqual(report["verification"]["checks"]["main_run"]["status"], "not_run")


if __name__ == "__main__":
    unittest.main()
