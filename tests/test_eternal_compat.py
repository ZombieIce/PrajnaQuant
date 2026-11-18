import json
import io
import os
import shutil
import subprocess
import sys
import unittest
import uuid
from contextlib import redirect_stderr
from datetime import date
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import eternal_baseline as baseline
import eternal_compat as compat
from test_eternal_baseline import make_replay_snapshot, temporary_directory_outside_repository


ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "scripts/eternal_compat.py"


def seal_snapshot(snapshot):
    manifest_path = snapshot / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["files"] = {
        path.relative_to(snapshot).as_posix(): {
            "sha256": baseline.sha256_file(path),
            "bytes": path.stat().st_size,
        }
        for path in sorted(path for path in snapshot.rglob("*") if path.is_file() and path != manifest_path)
    }
    manifest_path.write_text(
        json.dumps(baseline.seal_manifest(manifest), ensure_ascii=False, indent=2),
        encoding="utf-8",
    )


class EternalCompatibilityTests(unittest.TestCase):
    def setUp(self):
        artifact_root = os.environ.get("PRAJNA_TEST_ARTIFACTS")
        self._temporary_artifacts = None
        if artifact_root:
            root = Path(artifact_root)
        else:
            self._temporary_artifacts = temporary_directory_outside_repository()
            self.addCleanup(self._temporary_artifacts.cleanup)
            root = Path(self._temporary_artifacts.name)
        self.directory = root / f"eternal-compat-{uuid.uuid4()}"
        self.directory.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.directory)
        self.snapshot = self.directory / "snapshot"
        make_replay_snapshot(self.snapshot)

    def run_compat(self, name, config=None, snapshot=None, timeout=30):
        request = {
            "task_id": f"synthetic-{name}",
            "snapshot_path": str(snapshot or self.snapshot),
            "config": config or {},
        }
        request_path = self.directory / f"{name}-request.json"
        output_path = self.directory / f"{name}-result.json"
        request_path.write_text(json.dumps(request), encoding="utf-8")
        completed = subprocess.run(
            [
                sys.executable, str(RUNNER), "run",
                "--request", str(request_path), "--output", str(output_path),
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
        return completed, output_path

    def test_test_artifacts_and_temp_root_fallback_stay_outside_repository(self):
        repository = ROOT.resolve()
        self.assertNotIn(repository, self.directory.resolve().parents)

        with temporary_directory_outside_repository((ROOT, self.directory)) as fallback:
            fallback_path = Path(fallback).resolve()
            self.assertNotIn(repository, fallback_path.parents)
            self.assertNotEqual(repository, fallback_path)

    def test_default_and_variant_config_are_persisted_with_model_nav_units(self):
        default, default_path = self.run_compat("default")
        self.assertEqual(default.returncode, 0, default.stderr)
        default_result = json.loads(default_path.read_text(encoding="utf-8"))
        self.assertEqual(default_result["effective_config"]["initial_nav"], 1.0)
        self.assertIsNone(default_result["effective_config"]["initial_capital_currency_amount"])
        self.assertEqual(default_result["effective_config"]["initial_capital_unit"], "model_nav")
        self.assertEqual(default_result["effective_config"]["weights"], baseline.WEIGHTS)
        self.assertEqual(
            default_result["frozen_reference_comparison"]["overall_status"],
            "match",
        )
        self.assertEqual(
            default_result["frozen_reference_comparison"]["evaluation_mode"],
            "required_default_parity",
        )

        variant, variant_path = self.run_compat("variant", {
            "initial_nav": 10.0,
            "weights": {
                "sh511260": .25,
                "sh518880": .20,
                "sh515080": .20,
                "sh513300": .25,
                "sz159612": .10,
            },
            "rebalance_threshold": .05,
            "fee_rate_one_way": .001,
        })
        self.assertEqual(variant.returncode, 0, variant.stderr)
        variant_result = json.loads(variant_path.read_text(encoding="utf-8"))
        self.assertEqual(variant_result["effective_config"]["initial_nav"], 10.0)
        self.assertEqual(variant_result["summary"]["fee_bps"], 10.0)
        self.assertAlmostEqual(variant_result["daily"][0]["nav"], 10 / 1.001)
        self.assertEqual(
            variant_result["frozen_reference_comparison"]["overall_status"],
            "mismatch",
        )
        self.assertEqual(
            variant_result["frozen_reference_comparison"]["evaluation_mode"],
            "informational_variant",
        )
        self.assertNotEqual(
            default_result["identity"]["compat_run_id"],
            variant_result["identity"]["compat_run_id"],
        )
        for row in variant_result["daily"]:
            self.assertAlmostEqual(row["nav"], row["cash"] + sum(row["holdings"].values()))

    def test_capital_only_variant_compares_normalized_nav_and_scales_ledger(self):
        default, default_path = self.run_compat("capital-default")
        capital, capital_path = self.run_compat(
            "capital-only-variant",
            {"initial_nav": 10.0},
        )
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertEqual(capital.returncode, 0, capital.stderr)
        default_result = json.loads(default_path.read_text(encoding="utf-8"))
        capital_result = json.loads(capital_path.read_text(encoding="utf-8"))
        comparison = capital_result["frozen_reference_comparison"]
        self.assertEqual(comparison["overall_status"], "match")
        self.assertEqual(comparison["daily_nav"]["status"], "match")
        self.assertEqual(comparison["evaluation_mode"], "informational_variant")
        self.assertAlmostEqual(
            capital_result["summary"]["total_return"],
            default_result["summary"]["total_return"],
        )
        for default_day, capital_day in zip(
            default_result["daily"],
            capital_result["daily"],
            strict=True,
        ):
            for field in ("nav", "cash", "fees"):
                self.assertAlmostEqual(capital_day[field], default_day[field] * 10)
            for field in ("units", "holdings"):
                for asset in baseline.ETF_CODES:
                    self.assertAlmostEqual(
                        capital_day[field][asset],
                        default_day[field][asset] * 10,
                    )

    def test_positive_allocation_that_underflows_fails_without_result(self):
        weights = dict(baseline.WEIGHTS)
        tiny_asset = "sz159612"
        tiny_weight = 1e-16
        weights["sh511260"] += weights[tiny_asset] - tiny_weight
        weights[tiny_asset] = tiny_weight
        config = {
            "initial_nav": sys.float_info.min,
            "weights": weights,
        }

        completed, output_path = self.run_compat("scaled-allocation-underflow", config)

        self.assertEqual(completed.returncode, 2)
        summary = json.loads(completed.stderr)["error"]
        self.assertEqual(summary["code"], "compat_scaled_value_underflow")
        failure = json.loads(
            output_path.with_name("compat-failure.json").read_text(encoding="utf-8")
        )["error"]
        self.assertEqual(failure["code"], "compat_scaled_value_underflow")
        self.assertEqual(failure["details"]["field"], "daily.units")
        self.assertEqual(failure["details"]["asset"], tiny_asset)
        self.assertFalse(output_path.exists())

    def test_nonzero_subnormal_allocation_with_insufficient_precision_fails(self):
        weights = dict(baseline.WEIGHTS)
        weights["sh511260"] += weights["sz159612"] - 3e-16
        weights["sz159612"] = 3e-16
        completed, output = self.run_compat("scaled-imprecise", {
            "initial_nav": sys.float_info.min, "weights": weights,
        })
        self.assertEqual(completed.returncode, 2, completed.stderr)
        failure = json.loads(output.with_name("compat-failure.json").read_text())["error"]
        self.assertEqual(failure["code"], "compat_scaled_value_precision_loss")
        self.assertEqual(failure["details"]["asset"], "sz159612")
        self.assertFalse(output.exists())

    def test_normalized_positive_holding_underflow_fails_before_scaling(self):
        bars_path = self.snapshot / "bars/sh511260.csv.gz"
        bars, fields = baseline.csv_rows(bars_path)
        for row in bars:
            if row["date"] == "2026-11-09":
                row.update(close="0.5", low="0.5")
        baseline.write_csv(bars_path, bars, fields)
        seal_snapshot(self.snapshot)

        weights = dict(baseline.WEIGHTS)
        weights["sh518880"] += weights["sh511260"]
        weights["sh511260"] = 5e-324
        completed, output = self.run_compat(
            "normalized-holding-underflow",
            {"initial_nav": 1e100, "weights": weights},
        )
        self.assertEqual(completed.returncode, 2, completed.stderr)
        failure = json.loads(output.with_name("compat-failure.json").read_text())["error"]
        self.assertEqual(failure["code"], "compat_normalized_value_underflow")
        self.assertEqual(failure["details"], {
            "field": "daily.holdings", "asset": "sh511260", "date": "2026-11-09",
        })
        self.assertFalse(output.exists())

        weights["sh511260"] = 0.0
        zero_weight, result = self.run_compat(
            "zero-weight", {"initial_nav": 10.0, "weights": weights},
        )
        self.assertEqual(zero_weight.returncode, 0, zero_weight.stderr)
        self.assertEqual(json.loads(result.read_text())["daily"][1]["holdings"]["sh511260"], 0)

    def test_normalized_units_cannot_disappear_for_positive_weight(self):
        row = {
            "date": date(2026, 11, 9),
            "units": {"sh511260": 0.0},
            "holdings": {"sh511260": 0.0},
        }
        with self.assertRaises(compat.RunnerError) as caught:
            compat._validate_normalized_holdings([row], {"sh511260": 5e-324})
        self.assertEqual(caught.exception.code, "compat_normalized_value_underflow")
        self.assertEqual(caught.exception.details["field"], "daily.units")

    def test_nonzero_normalized_valuation_loss_is_not_a_successful_ledger(self):
        for label, weight, close, error_code in (
            ("zero-underflow", 5e-324, "0.5", "compat_normalized_value_underflow"),
            ("nonzero-loss", 5e-324, "0.75", "compat_normalized_value_precision_loss"),
            ("another-loss", 5e-324, "0.625", "compat_normalized_value_precision_loss"),
            ("zero-weight", 0.0, "0.75", None),
            ("ordinary-weight", 0.3, "0.75", None),
            ("small-normal-weight", 1e-300, "0.75", None),
            ("exact-subnormal", 1e-323, "0.5", None),
        ):
            with self.subTest(label=label):
                snapshot = self.directory / label
                shutil.copytree(self.snapshot, snapshot)
                path = snapshot / "bars/sh511260.csv.gz"
                bars, fields = baseline.csv_rows(path)
                for row in bars:
                    if row["date"] == "2026-11-09":
                        row.update(close=close, low=close)
                baseline.write_csv(path, bars, fields)
                seal_snapshot(snapshot)
                weights = dict(baseline.WEIGHTS)
                weights["sh518880"] += weights["sh511260"] - weight
                weights["sh511260"] = weight
                completed, output = self.run_compat(label, {
                    "initial_nav": 1e100, "fee_rate_one_way": 0.0,
                    "rebalance_threshold": 1.0, "weights": weights,
                }, snapshot=snapshot)
                if error_code is None:
                    self.assertEqual(completed.returncode, 0, completed.stderr)
                    result = json.loads(output.read_text())
                    for row in result["daily"]:
                        for asset in baseline.ETF_CODES:
                            price = (
                                float(close)
                                if asset == "sh511260" and row["date"] == "2026-11-09" else 1.0
                            )
                            expected = row["units"][asset] * price
                            actual = row["holdings"][asset]
                            if expected == 0:
                                self.assertEqual(actual, 0)
                            else:
                                self.assertLessEqual(abs(actual - expected) / expected, 1e-12)
                    continue
                if completed.returncode == 0:
                    result = json.loads(output.read_text())
                    row = result["daily"][1]
                    expected = row["units"]["sh511260"] * float(close)
                    relative_error = abs(row["holdings"]["sh511260"] - expected) / expected
                    self.fail(f"published a ledger with {relative_error:.6f} relative valuation error")
                self.assertEqual(completed.returncode, 2, completed.stderr)
                failure = json.loads(output.with_name("compat-failure.json").read_text())
                self.assertTrue(failure["structured_details_complete"])
                error = failure["error"]
                self.assertEqual(error["code"], error_code)
                self.assertEqual(error["details"]["asset"], "sh511260")
                self.assertEqual(error["details"]["date"], "2026-11-09")
                self.assertFalse(output.exists())

    def test_allocation_units_follow_principal_weights_fee_and_execution_price(self):
        def write_bars(snapshot, rebalance):
            path = snapshot / "bars/sh511260.csv.gz"
            bars, fields = baseline.csv_rows(path)
            for row in bars:
                if rebalance and row["date"] == "2026-11-09":
                    row.update(close="1.06", high="1.06")
                if rebalance and row["date"] == "2026-11-10":
                    row.update(open="1.06", high="1.06")
            baseline.write_csv(path, bars, fields)
            seal_snapshot(snapshot)

        # label, tiny asset, its weight, fee, threshold, error date / basis or None.
        cases = (
            ("initial-fee-half", "sh511260", 5e-324, 0.5, 1.0, ("2026-11-06", "initial")),
            ("initial-default-fee", "sh511260", 5e-324, 0.0005, 1.0, ("2026-11-06", "initial")),
            ("rebalance-capital-change", "sz159612", 5e-324, 0.0, 0.05,
             ("2026-11-10", "rebalance")),
            ("exact-subnormal-no-fee", "sh511260", 5e-324, 0.0, 1.0, None),
            ("zero-weight-fee-half", "sh511260", 0.0, 0.5, 1.0, None),
            ("ordinary-fee-half", "sh511260", 0.3, 0.5, 1.0, None),
            ("ordinary-rebalance-fee", "sz159612", 0.05, 0.001, 0.05, None),
        )
        initial_nav = 1e100
        for label, asset, weight, fee, threshold, failure in cases:
            with self.subTest(label=label):
                snapshot = self.directory / label
                shutil.copytree(self.snapshot, snapshot)
                write_bars(snapshot, threshold < 1.0)
                weights = dict(baseline.WEIGHTS)
                weights["sh518880"] += weights[asset] - weight
                weights[asset] = weight
                completed, output = self.run_compat(label, {
                    "initial_nav": initial_nav, "fee_rate_one_way": fee,
                    "rebalance_threshold": threshold, "weights": weights,
                }, snapshot=snapshot)
                if failure is not None:
                    self.assertEqual(completed.returncode, 2, completed.stderr)
                    self.assertFalse(output.exists())
                    error = json.loads(
                        output.with_name("compat-failure.json").read_text()
                    )["error"]
                    self.assertEqual(error["code"], "compat_normalized_allocation_precision_loss")
                    self.assertEqual(error["details"]["asset"], asset)
                    self.assertEqual(error["details"]["date"], failure[0])
                    self.assertTrue(error["details"]["allocation_basis"].startswith(failure[1]))
                    continue
                self.assertEqual(completed.returncode, 0, completed.stderr)
                result = json.loads(output.read_text())
                first = result["daily"][0]
                for code, configured in weights.items():
                    expected = initial_nav / (1 + fee) * configured
                    self.assertLessEqual(
                        abs(first["units"][code] - expected), abs(expected) * 1e-12, code,
                    )
                if threshold < 1.0:
                    trade = result["trades"][0]
                    before, after = result["daily"][1], result["daily"][2]
                    self.assertEqual(after["date"], trade["trade_date"])
                    opens = {code: 1.0 for code in baseline.ETF_CODES}
                    opens["sh511260"] = 1.06
                    capital = sum(before["units"][code] * opens[code] for code in opens)
                    net = sum(after["units"][code] * opens[code] for code in opens)
                    traded = sum(
                        abs(net * weights[code] - before["units"][code] * opens[code])
                        for code in opens
                    )
                    self.assertLessEqual(abs(net + fee * traded - capital), capital * 1e-12)
                    for code in opens:
                        self.assertLessEqual(
                            abs(after["units"][code] * opens[code] - net * weights[code]),
                            net * weights[code] * 1e-12, code,
                        )

    def test_fee_solved_net_capital_matches_hand_calculation(self):
        F = compat.Fraction
        # 0.5N < 0.6 and 0.5N > 0.4, so turnover is 0.2 and N = 1 - 0.01 * 0.2.
        self.assertEqual(
            compat._fee_solved_net_capital([F("0.6"), F("0.4")], F(0), [F("0.5"), F("0.5")], F("0.01")),
            F("0.998"),
        )
        # A zero-weight holding is sold entirely: N + 0.1 * ((N - 0.5) + 0.5) = 1.
        self.assertEqual(
            compat._fee_solved_net_capital([F("0.5"), F("0.5")], F(0), [F(1), F(0)], F("0.1")),
            F(10, 11),
        )

    def test_units_cannot_change_without_an_executed_trade(self):
        rows = [
            {"date": date(2026, 11, day), "cash": 0.0,
             "units": {code: weight for code, weight in baseline.WEIGHTS.items()}}
            for day in (6, 9)
        ]
        rows[1]["units"] = {**rows[1]["units"], "sz159612": 0.06}
        opens = [[1.0] * len(baseline.ETF_CODES)] * 2
        config = {"weights": baseline.WEIGHTS, "fee_rate_one_way": 0.0}
        with self.assertRaises(compat.RunnerError) as caught:
            compat._validate_allocation_units(rows, opens, [], config, 1, "normalized")
        self.assertEqual(caught.exception.code, "compat_normalized_allocation_precision_loss")
        self.assertEqual(caught.exception.details["allocation_basis"], "carried_forward")
        self.assertEqual(caught.exception.details["asset"], "sz159612")

    def test_validation_time_input_gaps_preserve_known_details(self):
        first_two = ("sh511260", "sh518880")
        for scenario, asset, field, expectations in (
            ("early-bar", "sh511260", "volume", {
                "bar_loading": ("partial", ("sh511260",), None),
                "factor_loading": ("partial", ("sh511260",), None),
                "bar_validation": ("partial", (), "sh511260"),
                "total_return_price_and_factor_validation": ("not_run", (), None),
                "opening_limit_construction": ("not_run", (), None),
                "historical_overlap_and_splice_validation": ("not_run", (), None),
            }),
            ("middle-factor", "sh515080", "factor", {
                "bar_loading": ("partial", (*first_two, "sh515080"), None),
                "factor_loading": ("partial", (*first_two, "sh515080"), None),
                "bar_validation": ("partial", (*first_two, "sh515080"), None),
                "total_return_price_and_factor_validation": ("partial", first_two, "sh515080"),
                "opening_limit_construction": ("partial", first_two, None),
                "historical_overlap_and_splice_validation": ("not_run", (), None),
            }),
            ("second-splice", "sh513500/sz159612", "overlap", {
                "bar_loading": ("completed", baseline.HISTORY_CODES, None),
                "factor_loading": ("completed", baseline.HISTORY_CODES, None),
                "bar_validation": ("completed", baseline.HISTORY_CODES, None),
                "total_return_price_and_factor_validation": ("completed", baseline.HISTORY_CODES, None),
                "opening_limit_construction": ("completed", baseline.HISTORY_CODES, None),
                "historical_overlap_and_splice_validation": (
                    "partial", ("sh513100/sh513300",), "sh513500/sz159612",
                ),
            }),
        ):
            with self.subTest(scenario=scenario):
                snapshot = self.directory / scenario
                shutil.copytree(self.snapshot, snapshot)
                if scenario == "middle-factor":
                    path = snapshot / f"factors/{asset}.csv.gz"
                    rows, fields = baseline.csv_rows(path)
                    rows[0]["date"] = "2026-11-09"
                else:
                    code = "sh513500" if scenario == "second-splice" else asset
                    path = snapshot / f"bars/{code}.csv.gz"
                    rows, fields = baseline.csv_rows(path)
                    if scenario == "early-bar":
                        rows[0]["volume"] = "0"
                    else:
                        rows = rows[:1]
                baseline.write_csv(path, rows, fields)
                if scenario == "early-bar":
                    later = snapshot / "bars/sh513500.csv.gz"
                    later_rows, later_fields = baseline.csv_rows(later)
                    later_rows[0]["volume"] = "0"
                    baseline.write_csv(later, later_rows, later_fields)
                seal_snapshot(snapshot)
                completed, output = self.run_compat(scenario, snapshot=snapshot)
                self.assertEqual(completed.returncode, 2, completed.stderr)
                failure = json.loads(output.with_name("compat-failure.json").read_text())
                self.assertTrue(failure["structured_details_complete"])
                self.assertEqual(failure["error"]["code"], "compat_input_invalid")
                details = failure["error"]["details"]
                self.assertEqual(len(details["input_gaps"]), 1)
                gap = details["input_gaps"][0]
                self.assertEqual((gap["asset"], gap["field"]), (asset, field))
                self.assertTrue(gap["date"])
                self.assertTrue(gap["impact"])
                self.assertTrue(gap["candidate_measures"])
                if scenario == "early-bar":
                    self.assertEqual(gap["source"], "synthetic")
                else:
                    self.assertNotIn("source", gap)
                progress = details["check_progress"]
                for name in (
                    "snapshot_manifest_and_inventory_validation",
                    "snapshot_configuration_validation", "frozen_source_validation",
                ):
                    self.assertEqual(progress[name], {"status": "completed"})
                for name, (status, completed_scopes, partial_scope) in expectations.items():
                    scopes = (
                        ("sh513100/sh513300", "sh513500/sz159612")
                        if name == "historical_overlap_and_splice_validation"
                        else baseline.HISTORY_CODES
                    )
                    expected = dict.fromkeys(scopes, "not_run")
                    expected.update(dict.fromkeys(completed_scopes, "completed"))
                    if partial_scope is not None:
                        expected[partial_scope] = "partial"
                    self.assertEqual(progress[name], {"status": status, "scopes": expected}, name)
                for name in ("common_session_validation", "simulation", "frozen_reference_comparison"):
                    self.assertEqual(progress[name], {"status": "not_run"})
                unfinished = {item["name"]: item for item in details["checks_not_run"]}
                expected_unfinished = {
                    name for name, (status, _, _) in expectations.items() if status != "completed"
                } | {"common_session_validation", "simulation", "frozen_reference_comparison"}
                self.assertEqual(set(unfinished), expected_unfinished)
                for name, item in unfinished.items():
                    self.assertEqual(item["status"], progress[name]["status"])
                    self.assertEqual(item.get("scopes"), progress[name].get("scopes"))
                    self.assertTrue(item["reason"])
                self.assertFalse(output.exists())

    def test_factor_input_gap_does_not_invent_a_bar_source(self):
        factors_path = self.snapshot / "factors/sh511260.csv.gz"
        factors, fields = baseline.csv_rows(factors_path)
        factors[0]["date"] = "2026-11-09"
        baseline.write_csv(factors_path, factors, fields)
        seal_snapshot(self.snapshot)

        completed, output = self.run_compat("factor-validation-gap")
        self.assertEqual(completed.returncode, 2, completed.stderr)
        failure = json.loads(output.with_name("compat-failure.json").read_text())
        gap = failure["error"]["details"]["input_gaps"][0]
        self.assertEqual((gap["asset"], gap["date"], gap["field"]),
                         ("sh511260", "2026-11-06", "factor"))
        self.assertNotIn("source", gap)
        self.assertTrue(failure["structured_details_complete"])
        self.assertFalse(output.exists())

    def test_default_reference_mismatch_fails_without_publishing_a_result(self):
        reference_path = self.snapshot / "reference.json"
        reference = json.loads(reference_path.read_text(encoding="utf-8"))
        reference["history_etf"][0][1] += 0.1
        reference_path.write_text(json.dumps(reference), encoding="utf-8")
        seal_snapshot(self.snapshot)

        completed, output_path = self.run_compat("default-reference-mismatch")

        self.assertEqual(completed.returncode, 2)
        error = json.loads(completed.stderr)["error"]
        self.assertEqual(error["code"], "compat_default_reference_mismatch")
        self.assertIn("default", error["message"])
        self.assertFalse(output_path.exists())

    def test_pending_gap_report_is_structured_before_manifest_validation(self):
        pending_snapshot = self.directory / "pending-only-snapshot"
        pending_snapshot.mkdir()
        report = {
            "status": "pending_decision",
            "gaps": [
                {
                    "asset": "sh511260",
                    "date": "2026-11-09",
                    "field": "volume",
                    "impact": "synthetic first gap",
                    "candidate_measures": ["synthetic first option"],
                    "source": "synthetic-source-a",
                    "extra_field": {"nested": ["retained-a", "😀", 0.1]},
                },
                {
                    "asset": "sh518880",
                    "date": "2026-11-10",
                    "field": "daily_bar",
                    "impact": "synthetic second gap",
                    "candidate_measures": ["synthetic second option"],
                    "source": "synthetic-source-b",
                    "extra_field": "retained-b",
                },
            ],
        }
        (pending_snapshot / "pending-gaps.json").write_text(
            json.dumps(report), encoding="utf-8",
        )

        completed, output_path = self.run_compat(
            "pending-only-snapshot",
            snapshot=pending_snapshot,
        )

        self.assertEqual(completed.returncode, 2)
        summary = json.loads(completed.stderr)["error"]
        self.assertTrue(summary["details"]["structured_details_persisted"])
        persisted = json.loads(
            output_path.with_name("compat-failure.json").read_text(encoding="utf-8")
        )
        error = persisted["error"]
        self.assertEqual(error["code"], "compat_input_gaps_pending")
        self.assertEqual(error["details"]["pending_gap_report"], report)
        self.assertEqual(
            [check["name"] for check in error["details"]["checks_not_run"]],
            [
                "snapshot_manifest_validation",
                "snapshot_file_inventory_validation",
                "snapshot_configuration_validation",
                "frozen_source_validation",
                "bar_and_factor_validation",
                "total_return_price_and_limit_construction",
                "historical_overlap_and_splice_validation",
                "common_session_validation",
                "simulation",
                "frozen_reference_comparison",
            ],
        )
        self.assertFalse(output_path.exists())

    def test_deep_pending_metadata_keeps_queryable_gaps_with_raw_report(self):
        nested = None
        for _ in range(130):
            nested = [nested]
        report = {
            "status": "pending_decision",
            "gaps": [
                {
                    "asset": "sh511260",
                    "date": "2026-11-09",
                    "field": "volume",
                    "source": "synthetic-source-a",
                    "impact": "first gap remains queryable",
                    "candidate_measures": ["synthetic first option"],
                    "extra_deep": nested,
                },
                {
                    "asset": "sh518880",
                    "date": "2026-11-10",
                    "field": "daily_bar",
                    "source": "synthetic-source-b",
                    "impact": "second gap remains queryable",
                    "candidate_measures": ["synthetic second option"],
                },
            ],
        }
        payload = json.dumps(report, separators=(",", ":"))
        pending_snapshot = self.directory / "deep-pending-snapshot"
        pending_snapshot.mkdir()
        (pending_snapshot / "pending-gaps.json").write_text(payload, encoding="utf-8")

        completed, output_path = self.run_compat(
            "deep-pending-metadata",
            snapshot=pending_snapshot,
        )

        self.assertEqual(completed.returncode, 2)
        summary = json.loads(completed.stderr)["error"]
        self.assertEqual(summary["code"], "compat_pending_gaps_invalid")
        self.assertFalse(summary["details"]["structured_details_complete"])
        failure = json.loads(
            output_path.with_name("compat-failure.json").read_text(encoding="utf-8")
        )
        error = failure["error"]
        self.assertEqual(error["details"]["report_status"], "depth_limit_exceeded")
        self.assertFalse(error["details"]["report_contents_persisted"])
        self.assertEqual(error["details"]["pending_gap_report_raw_json"], payload)
        recovered = error["details"]["recoverable_pending_gap_report"]
        self.assertEqual(
            recovered["gaps"][0]["asset"],
            "sh511260",
        )
        self.assertEqual(
            recovered["gaps"][0]["extra_deep"],
            {"unsupported_json_depth": "see pending_gap_report_raw_json"},
        )
        self.assertEqual(
            recovered["gaps"][1],
            {
                "asset": "sh518880",
                "date": "2026-11-10",
                "field": "daily_bar",
                "source": "synthetic-source-b",
                "impact": "second gap remains queryable",
                "candidate_measures": ["synthetic second option"],
            },
        )
        self.assertEqual(len(error["details"]["checks_not_run"]), 10)
        self.assertFalse(output_path.exists())

    def test_nonrepresentable_pending_report_numbers_fail_with_structured_diagnostics(self):
        for label, number, expected_status in (
            ("positive-overflow", "1e1000", "non_representable_number"),
            ("negative-underflow", "1e-1000", "non_representable_number"),
            ("integer-overflow", "18446744073709551616", "non_representable_number"),
            ("rounded-decimal", "9007199254740993.0", "non_representable_number"),
            ("rounded-fraction", "0.1234567890123456789", "non_representable_number"),
            ("nonstandard-constant", "NaN", "invalid"),
        ):
            with self.subTest(number=number):
                snapshot = self.directory / f"pending-{label}"
                snapshot.mkdir()
                known = {
                    "asset": "sh518880", "date": "2026-11-10", "field": "daily_bar",
                    "source": "synthetic-b", "impact": "second gap",
                    "candidate_measures": ["option"],
                }
                first = {**known, "asset": "sh511260", "extra_numeric": "NUMBER_TOKEN"}
                report = {"status": "pending_decision", "gaps": [first, known]}
                (snapshot / "pending-gaps.json").write_text(
                    json.dumps(report).replace('"NUMBER_TOKEN"', number), encoding="utf-8",
                )

                completed, output_path = self.run_compat(
                    f"pending-{label}",
                    snapshot=snapshot,
                )

                self.assertEqual(completed.returncode, 2, completed.stderr)
                summary = json.loads(completed.stderr)["error"]
                self.assertEqual(summary["code"], "compat_pending_gaps_invalid")
                self.assertTrue(summary["details"]["structured_details_persisted"])
                self.assertFalse(summary["details"]["structured_details_complete"])
                failure = json.loads(
                    output_path.with_name("compat-failure.json").read_text(
                        encoding="utf-8"
                    )
                )["error"]
                self.assertEqual(failure["details"]["report_status"], expected_status)
                self.assertFalse(failure["details"]["report_contents_persisted"])
                self.assertNotIn("pending_gap_report", failure["details"])
                recovered = failure["details"]["recoverable_pending_gap_report"]
                self.assertEqual(
                    {key: value for key, value in recovered["gaps"][0].items()
                     if key != "extra_numeric"}, {**known, "asset": "sh511260"},
                )
                self.assertEqual(recovered["gaps"][0]["extra_numeric"],
                                 {"unsupported_json_number": number})
                self.assertEqual(recovered["gaps"][1], {
                    "asset": "sh518880", "date": "2026-11-10", "field": "daily_bar",
                    "source": "synthetic-b", "impact": "second gap",
                    "candidate_measures": ["option"],
                })
                self.assertIn(number, failure["details"]["pending_gap_report_raw_json"])
                self.assertFalse(output_path.exists())

    def test_pending_report_rejects_lone_surrogates_and_duplicate_keys(self):
        for label, fragment in (
            ("surrogate-value", '"extra":"\\ud800"'),
            ("surrogate-key", '"\\udfff":"extra"'),
            ("duplicate-key", '"source":"first","source":"second"'),
        ):
            with self.subTest(label=label):
                snapshot = self.directory / label
                snapshot.mkdir()
                (snapshot / "pending-gaps.json").write_text(
                    '{"status":"pending_decision","gaps":[{' + fragment + ' }]}',
                    encoding="utf-8",
                )
                completed, output_path = self.run_compat(label, snapshot=snapshot)
                self.assertEqual(completed.returncode, 2, completed.stderr)
                failure = json.loads(output_path.with_name("compat-failure.json").read_text())
                self.assertEqual(failure["error"]["code"], "compat_pending_gaps_invalid")
                self.assertFalse(failure["error"]["details"]["report_contents_persisted"])
                self.assertNotIn("pending_gap_report", failure["error"]["details"])
                self.assertFalse(output_path.exists())

    def test_failure_serialization_falls_back_to_partial_durable_diagnostics(self):
        output_path = self.directory / "serialization-failure-result.json"
        stderr = io.StringIO()
        error = compat.RunnerError(
            "compat_synthetic_failure",
            "synthetic failure with unsupported diagnostic",
            {"unsupported_numeric_detail": float("inf")},
        )

        with redirect_stderr(stderr):
            compat._emit_failure(output_path, error)

        summary = json.loads(stderr.getvalue())["error"]
        self.assertEqual(summary["code"], error.code)
        self.assertTrue(summary["details"]["structured_details_persisted"])
        self.assertFalse(summary["details"]["structured_details_complete"])
        failure = json.loads(
            output_path.with_name("compat-failure.json").read_text(encoding="utf-8")
        )
        self.assertEqual(failure["error"]["code"], error.code)
        self.assertFalse(failure["structured_details_complete"])
        self.assertIn(
            "original failure details were not JSON serializable",
            failure["error"]["details"]["failure_details_omitted_reason"],
        )
        self.assertFalse(output_path.exists())

    def test_failure_sidecar_store_failure_is_explicitly_incomplete(self):
        from unittest.mock import patch

        output = self.directory / "store-failure-result.json"
        stderr = io.StringIO()
        with patch.object(compat.os, "replace", side_effect=PermissionError("synthetic")):
            with redirect_stderr(stderr):
                compat._emit_failure(output, compat.RunnerError("compat_synthetic", "synthetic"))
        details = json.loads(stderr.getvalue())["error"]["details"]
        self.assertFalse(details["structured_details_persisted"])
        self.assertFalse(details["structured_details_complete"])
        self.assertTrue(details["structured_details_persistence_failed"])
        self.assertFalse(output.with_name("compat-failure.json").exists())
        self.assertFalse(list(self.directory.glob(".compat-failure.json.*.tmp")))

    def test_dangling_pending_report_symlink_fails_without_result(self):
        dangling_snapshot = self.directory / "dangling-pending-snapshot"
        shutil.copytree(self.snapshot, dangling_snapshot)
        (dangling_snapshot / "pending-gaps.json").symlink_to("missing-report.json")

        completed, output_path = self.run_compat(
            "dangling-pending-symlink",
            snapshot=dangling_snapshot,
        )

        self.assertEqual(completed.returncode, 2)
        stderr_error = json.loads(completed.stderr)["error"]
        self.assertEqual(
            stderr_error["code"],
            "compat_pending_gaps_unsupported",
        )
        error = json.loads(
            output_path.with_name("compat-failure.json").read_text(encoding="utf-8")
        )["error"]
        self.assertEqual(error["code"], "compat_pending_gaps_unsupported")
        self.assertIn("regular file", error["message"])
        self.assertEqual(
            error["details"]["report_status"],
            "unsupported_file_type",
        )
        self.assertFalse(output_path.exists())

    def test_malformed_nonregular_and_unreadable_pending_gap_reports_fail_explicitly(self):
        malformed = self.directory / "malformed-pending-snapshot"
        malformed.mkdir()
        (malformed / "pending-gaps.json").write_text("{not-json", encoding="utf-8")
        malformed_result, malformed_output = self.run_compat(
            "malformed-pending",
            snapshot=malformed,
        )
        self.assertEqual(malformed_result.returncode, 2)
        self.assertEqual(
            json.loads(malformed_result.stderr)["error"]["code"],
            "compat_pending_gaps_invalid",
        )
        self.assertFalse(malformed_output.exists())

        unreadable = self.directory / "unreadable-pending-snapshot"
        unreadable.mkdir()
        (unreadable / "pending-gaps.json").mkdir()
        unreadable_result, unreadable_output = self.run_compat(
            "unreadable-pending",
            snapshot=unreadable,
        )
        self.assertEqual(unreadable_result.returncode, 2)
        self.assertEqual(
            json.loads(unreadable_result.stderr)["error"]["code"],
            "compat_pending_gaps_unsupported",
        )
        self.assertIn(
            "regular file",
            json.loads(unreadable_result.stderr)["error"]["message"],
        )
        self.assertFalse(unreadable_output.exists())

        if hasattr(os, "geteuid") and os.geteuid() != 0:
            unreadable_file_snapshot = self.directory / "unreadable-file-pending-snapshot"
            unreadable_file_snapshot.mkdir()
            report_path = unreadable_file_snapshot / "pending-gaps.json"
            report_path.write_text("{}", encoding="utf-8")
            report_path.chmod(0)
            try:
                file_result, file_output = self.run_compat(
                    "unreadable-pending-file",
                    snapshot=unreadable_file_snapshot,
                )
            finally:
                report_path.chmod(0o600)
            self.assertEqual(file_result.returncode, 2)
            self.assertEqual(
                json.loads(file_result.stderr)["error"]["code"],
                "compat_pending_gaps_unreadable",
            )
            self.assertFalse(file_output.exists())

    def test_pending_gap_fifo_is_rejected_without_blocking(self):
        if not hasattr(os, "mkfifo"):
            self.skipTest("FIFO reports are unavailable on this platform")
        fifo_snapshot = self.directory / "fifo-pending-snapshot"
        fifo_snapshot.mkdir()
        os.mkfifo(fifo_snapshot / "pending-gaps.json")

        completed, output_path = self.run_compat(
            "fifo-pending",
            snapshot=fifo_snapshot,
            timeout=5,
        )

        self.assertEqual(completed.returncode, 2)
        error = json.loads(completed.stderr)["error"]
        self.assertEqual(error["code"], "compat_pending_gaps_unsupported")
        self.assertIn("regular file", error["message"])
        self.assertFalse(output_path.exists())

    def test_config_boundaries_match_documented_positive_nav_and_subunit_fee(self):
        accepted = compat.validate_config({
            "initial_nav": sys.float_info.min,
            "fee_rate_one_way": 0.9999999995,
        })
        self.assertEqual(accepted["initial_nav"], sys.float_info.min)
        self.assertEqual(accepted["fee_rate_one_way"], 0.9999999995)
        for value in (0.0, -1.0):
            with self.subTest(initial_nav=value):
                with self.assertRaisesRegex(compat.RunnerError, "greater than zero"):
                    compat.validate_config({"initial_nav": value})
        for value in (1e-310, sys.float_info.min / 2):
            with self.subTest(initial_nav=value):
                with self.assertRaisesRegex(compat.RunnerError, "minimum normal positive"):
                    compat.validate_config({"initial_nav": value})
        for value in (1.0, 1.1):
            with self.subTest(fee_rate_one_way=value):
                with self.assertRaisesRegex(compat.RunnerError, "less than 1.0"):
                    compat.validate_config({"fee_rate_one_way": value})

    def test_threshold_change_controls_synthetic_close_signal_and_next_open_trade(self):
        bars_path = self.snapshot / "bars/sh511260.csv.gz"
        bars, fields = baseline.csv_rows(bars_path)
        for row in bars:
            if row["date"] == "2026-11-09":
                row["close"] = "1.06"
        baseline.write_csv(bars_path, bars, fields)
        seal_snapshot(self.snapshot)

        under, under_path = self.run_compat(
            "threshold-10",
            {"initial_nav": 10.0, "rebalance_threshold": .10},
        )
        over, over_path = self.run_compat(
            "threshold-5",
            {
                "initial_nav": 10.0,
                "rebalance_threshold": .05,
                "fee_rate_one_way": .001,
            },
        )
        reference, reference_path = self.run_compat(
            "threshold-5-reference",
            {"rebalance_threshold": .05, "fee_rate_one_way": .001},
        )
        self.assertEqual(under.returncode, 0, under.stderr)
        self.assertEqual(over.returncode, 0, over.stderr)
        self.assertEqual(reference.returncode, 0, reference.stderr)
        under_result = json.loads(under_path.read_text(encoding="utf-8"))
        over_result = json.loads(over_path.read_text(encoding="utf-8"))
        reference_result = json.loads(reference_path.read_text(encoding="utf-8"))
        self.assertEqual(under_result["signals"], [])
        self.assertEqual(len(over_result["signals"]), 1)
        self.assertEqual(over_result["signals"][0]["signal_date"], "2026-11-09")
        self.assertEqual(over_result["trades"][0]["intended_date"], "2026-11-10")
        self.assertEqual(over_result["trades"][0]["trade_date"], "2026-11-10")
        self.assertAlmostEqual(
            over_result["trades"][0]["fees_in_initial_nav"],
            reference_result["trades"][0]["fees_in_initial_nav"] * 10,
        )

    def test_invalid_or_missing_input_fails_without_publishing_a_result(self):
        with self.assertRaisesRegex(compat.RunnerError, "exactly the five fixed assets"):
            compat.validate_config({"weights": {"sh511260": 1.0}})
        with self.assertRaisesRegex(compat.RunnerError, "greater than zero"):
            compat.validate_config({"initial_nav": 0.0})

        completed, output_path = self.run_compat(
            "missing-input",
            snapshot=self.directory / "missing-snapshot",
        )
        self.assertEqual(completed.returncode, 2)
        self.assertEqual(json.loads(completed.stderr)["error"]["code"], "compat_input_unavailable")
        self.assertFalse(output_path.exists())

    def test_rejects_non_normalized_weights_and_overwrite(self):
        with self.assertRaisesRegex(compat.RunnerError, "sum to 1.0"):
            compat.validate_config({"weights": {code: .1 for code in baseline.ETF_CODES}})

        first, output_path = self.run_compat("immutable-output")
        self.assertEqual(first.returncode, 0, first.stderr)
        second, _ = self.run_compat("immutable-output")
        self.assertEqual(second.returncode, 2)
        self.assertEqual(json.loads(second.stderr)["error"]["code"], "compat_output_exists")


if __name__ == "__main__":
    unittest.main()
