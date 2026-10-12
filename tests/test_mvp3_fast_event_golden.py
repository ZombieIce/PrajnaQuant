import json
import hashlib
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "poc/mvp3-golden"
SCRIPT = GOLDEN / "fast_event_golden.py"
HAND = GOLDEN / "fixtures/hand-v1.json"
RETRY = GOLDEN / "fixtures/hand-retry-v1.json"
REBALANCE = GOLDEN / "fixtures/hand-rebalance-v1.json"
SMALL = ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
MA = ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json"
POC_PREFLIGHT = ROOT / "poc/poc0-benchmark/results/b2-matched-decision-loads-reset-selected-2026-09-28.preflight.json"


class FastEventGoldenTests(unittest.TestCase):
    def run_cli(
        self, fixture=HAND, strategy="s1", sizing="lot", retry=None,
        rebalance="full_target", expect_error=False, poc0_s1_timing=False,
    ):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            if isinstance(fixture, dict):
                path = directory / "fixture.json"
                path.write_text(json.dumps(fixture), encoding="utf-8")
            else:
                path = fixture
            out = directory / "out.json"
            command = [
                sys.executable, str(SCRIPT), "--fixture", str(path),
                "--strategy", strategy, "--sizing", sizing, "--out", str(out),
            ]
            command.extend(["--rebalance", rebalance])
            if retry is not None:
                command.extend(["--unfilled-entry", retry])
            if poc0_s1_timing:
                command.append("--poc0-s1-timing")
            completed = subprocess.run(command, check=False, capture_output=True, text=True)
            if expect_error:
                self.assertNotEqual(completed.returncode, 0)
                self.assertFalse(out.exists())
                return completed.stderr
            self.assertEqual(completed.returncode, 0, completed.stderr)
            return json.loads(out.read_text(encoding="utf-8"))

    def test_hand_buy_and_hold_lot_cash_and_fees(self):
        result = self.run_cli()
        sessions = result["sessions"]
        # D1 open and close: no order; close creates the sole S1 decision.
        self.assertEqual(sessions[0]["open"]["cash"], 1000)
        self.assertEqual(sessions[0]["close"]["equity"], 1000)
        self.assertEqual(result["decisions"][0]["session_date"], "2026-01-05")
        # D2: floor(1000/10/10)*10=100 is unaffordable at 10.1 plus costs.
        # Reduce to 90: 909 notional + 9.09 commission + 18.18 tax = 936.27.
        fill = result["fills"][0]
        self.assertEqual(fill["quantity"], 90)
        self.assertAlmostEqual(fill["price"], 10.1)
        self.assertAlmostEqual(fill["notional"], 909)
        self.assertAlmostEqual(fill["commission"], 9.09)
        self.assertAlmostEqual(fill["tax"], 18.18)
        self.assertAlmostEqual(fill["slippage"], 9)
        self.assertAlmostEqual(fill["cash_change"], -936.27)
        self.assertAlmostEqual(sessions[1]["open"]["cash"], 63.73)
        self.assertAlmostEqual(sessions[1]["open"]["equity"], 963.73)
        # D2 close marks 90 at 12: 63.73 + 1080 = 1143.73.
        self.assertAlmostEqual(sessions[1]["close"]["equity"], 1143.73)
        # D3 open re-marks at 10; S1 does not rebalance or incur new fees.
        self.assertAlmostEqual(sessions[2]["open"]["equity"], 963.73)
        self.assertAlmostEqual(sessions[2]["close"]["equity"], 1143.73)
        self.assertEqual(len(result["fills"]), 1)

    def test_entry_only_retries_halted_s1_target_with_fixed_initial_budget(self):
        fixture = json.loads(HAND.read_text())
        fixture["calendar"] += ["2026-01-08"]
        fixture["instruments"] = [
            {"symbol": "A", "lot_size": 1, "opens": [10, 10, 20, 20],
             "closes": [10, 20, 20, 20]},
            {"symbol": "B", "lot_size": 1, "opens": [10, 10, 10, 10],
             "closes": [10, 10, 10, 10]},
        ]
        fixture["costs"] = {key: 0 for key in fixture["costs"]}
        fixture["execution_status_overrides"] = [
            {"symbol": "B", "date": "2026-01-06", "trade_status": "HALTED",
             "is_tradable": False, "available_at": "08:50:00+08:00"},
        ]
        result = self.run_cli(
            fixture, strategy="s1", sizing="lot", rebalance="entry_only",
        )
        self.assertEqual(
            [(fill["session_date"], fill["instrument_id"], fill["quantity"])
             for fill in result["fills"]],
            [("2026-01-06", "A.SYNTH", 50), ("2026-01-07", "B.SYNTH", 50)],
        )
        self.assertEqual(result["parameters"]["rebalance"], "entry_only")
        self.assertEqual(
            [(row["session_date"], row["instrument_id"], row["status"], row["reason"])
             for row in result["orders"]],
            [("2026-01-06", "B.SYNTH", "blocked", "halted"),
             ("2026-01-06", "A.SYNTH", "filled", None),
             ("2026-01-07", "B.SYNTH", "filled", None)],
        )
        self.assertTrue(result["executions"][1]["retry_only"])
        self.assertEqual(result["sessions"][2]["open"]["holdings"]["A.SYNTH"]["quantity"], 50)
        self.assertEqual(result["sessions"][2]["open"]["cash"], 0)
        self.assertEqual(result["sessions"][2]["open"]["equity"], 1500)
        self.assertEqual(result["pending_at_end"], None)

    def test_entry_only_retries_cash_limited_s1_target_without_resizing_held_leg(self):
        fixture = json.loads(HAND.read_text())
        fixture["instruments"] = [
            {"symbol": "A", "lot_size": 50, "closes": [10, 10, 10]},
            {"symbol": "B", "lot_size": 50, "opens": [10, 10, 5],
             "closes": [10, 10, 5]},
        ]
        fixture["costs"] = {key: 0 for key in fixture["costs"]}
        fixture["costs"]["minimum_commission"] = 100
        result = self.run_cli(fixture, rebalance="entry_only")
        self.assertEqual(
            [(row["session_date"], row["instrument_id"], row["quantity"],
              row["status"], row["reason"]) for row in result["orders"]],
            [("2026-01-06", "A.SYNTH", 50, "filled", None),
             ("2026-01-06", "B.SYNTH", 0, "unfilled", "insufficient_cash"),
             ("2026-01-07", "B.SYNTH", 50, "filled", None)],
        )
        self.assertEqual([row["commission"] for row in result["fills"]], [100, 100])
        self.assertTrue(result["executions"][1]["retry_only"])
        self.assertEqual(result["sessions"][1]["open"]["cash"], 400)
        self.assertEqual(result["sessions"][2]["open"]["cash"], 50)
        self.assertEqual(result["sessions"][2]["open"]["equity"], 800)
        self.assertEqual(result["sessions"][2]["open"]["holdings"]["A.SYNTH"]["quantity"], 50)
        self.assertIsNone(result["pending_at_end"])

    def test_entry_only_rejects_skip_instead_of_recording_an_ignored_policy(self):
        for strategy, fixture in (("s1", HAND), ("s2", SMALL), ("s3", MA)):
            stderr = self.run_cli(
                fixture, strategy=strategy, retry="skip", rebalance="entry_only",
                expect_error=True,
            )
            self.assertIn("entry_only requires unfilled_entry=retry", stderr)

    def test_entry_only_divides_budget_by_slipped_fill_price_like_poc(self):
        fixture = json.loads(HAND.read_text())
        fixture["instruments"] = [
            {"symbol": symbol, "lot_size": 1, "closes": [10, 10, 10]}
            for symbol in ("A", "B")
        ]
        fixture["costs"] = {key: 0 for key in fixture["costs"]}
        fixture["costs"]["buy_slippage_bps"] = 1000
        result = self.run_cli(fixture, rebalance="entry_only")
        # Each fixed 500 budget buys floor(500/11)=45, not floor(500/10)=50.
        self.assertEqual([row["quantity"] for row in result["fills"]], [45, 45])
        self.assertEqual([row["price"] for row in result["fills"]], [11, 11])
        self.assertEqual(result["sessions"][1]["open"]["cash"], 10)
        self.assertEqual(result["sessions"][1]["open"]["equity"], 910)

    def test_entry_only_s2_preserves_existing_quantity_and_retries_missing_target(self):
        fixture = json.loads(REBALANCE.read_text())
        fixture["costs"] = {key: 0 for key in fixture["costs"]}
        fixture["strategy"]["rebalance_every"] = 10
        fixture["execution_status_overrides"] = [
            {"symbol": "B", "date": "2026-01-08", "trade_status": "HALTED",
             "is_tradable": False, "available_at": "08:50:00+08:00"},
        ]
        result = self.run_cli(
            fixture, strategy="s2", sizing="lot", rebalance="entry_only",
        )
        # D4 buys A with 1000/2=500; D5 buys B with remaining 500/1=500.
        self.assertEqual(
            [(row["session_date"], row["instrument_id"], row["quantity"])
             for row in result["fills"]],
            [("2026-01-08", "A.SYNTH", 50), ("2026-01-09", "B.SYNTH", 50)],
        )
        self.assertEqual(
            [(row["session_date"], row["instrument_id"], row["status"], row["reason"])
             for row in result["orders"]],
            [("2026-01-08", "B.SYNTH", "blocked", "halted"),
             ("2026-01-08", "A.SYNTH", "filled", None),
             ("2026-01-09", "B.SYNTH", "filled", None)],
        )
        self.assertEqual(len(result["decisions"]), 1)
        self.assertTrue(result["executions"][1]["retry_only"])
        self.assertEqual(result["sessions"][4]["open"]["holdings"]["A.SYNTH"]["quantity"], 50)
        self.assertEqual(result["sessions"][4]["open"]["cash"], 0)
        self.assertEqual(result["sessions"][4]["open"]["equity"], 1250)
        self.assertIsNone(result["pending_at_end"])

    def test_entry_only_rejects_vector_parity(self):
        for strategy in ("s1", "s2", "s3"):
            stderr = self.run_cli(
                {"s1": HAND, "s2": SMALL, "s3": MA}[strategy],
                strategy=strategy, sizing="vector_parity", rebalance="entry_only",
                expect_error=True,
            )
            self.assertIn("entry_only", stderr)

    def test_entry_only_s3_matches_independent_poc_orders_and_ledger_checkpoints(self):
        result = self.run_cli(MA, strategy="s3", rebalance="entry_only")
        expected = json.loads(
            (ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-expected-v1.json").read_text()
        )
        dates = [row["session_date"] for row in result["sessions"]]
        self.assertEqual(len(dates), 130)
        self.assertEqual(result["parameters"]["strategy_parameters"]["universe"], ["A.SYNTH"])
        self.assertEqual(result["parameters"]["unfilled_entry"], "retry")
        self.assertEqual(
            [{"session_index": dates.index(row["session_date"]),
              "target_symbols": [key.removesuffix(".SYNTH") for key in row["targets"]]}
             for row in result["decisions"]],
            expected["signals"],
        )
        self.assertEqual(
            [{"session_index": dates.index(row["session_date"]),
              "symbol": row["instrument_id"].removesuffix(".SYNTH"),
              "side": row["side"].upper(), "quantity": row["quantity"],
              "fill_price": row["price"], "commission": row["commission"]}
             for row in result["fills"]],
            expected["orders"],
        )
        self.assertEqual(len(result["orders"]), len(expected["orders"]))
        self.assertTrue(all(row["status"] == "filled" and row["reason"] is None
                            for row in result["orders"]))
        for checkpoint in expected["ledger_checkpoints"]:
            close = result["sessions"][checkpoint["session_index"]]["close"]
            self.assertAlmostEqual(close["cash"], checkpoint["cash"], delta=1e-6)
            self.assertAlmostEqual(close["equity"], checkpoint["nav"], delta=1e-6)
            self.assertEqual(
                close["holdings"].get("A.SYNTH", {}).get("quantity", 0),
                checkpoint["quantity"],
            )
        for field, total in (("commission", "total_commission"), ("slippage", "total_slippage"),
                             ("total_cost", "total_cost")):
            self.assertAlmostEqual(sum(row[field] for row in result["fills"]),
                                   expected[total], delta=1e-6)
        self.assertAlmostEqual(result["sessions"][-1]["close"]["equity"],
                               expected["final_equity"], delta=1e-6)
        # The expanded A fixture has no missing close in any complete MA window.
        self.assertTrue(all(row["ma20_60"]["A.SYNTH"]["status"] == "ok"
                            for row in result["sessions"][59:]))
        self.assertIsNone(result["pending_at_end"])

    def test_entry_only_s1_s2_s3_match_committed_poc_fills_and_daily_close_ledger(self):
        preflight = json.loads(POC_PREFLIGHT.read_text())
        for strategy, key in (("s1", "buy_hold"), ("s2", "momentum_rotation"), ("s3", "ma20_60")):
            with self.subTest(strategy=strategy):
                result = self.run_cli(
                    MA if strategy == "s3" else SMALL, strategy=strategy, rebalance="entry_only",
                    poc0_s1_timing=strategy == "s1",
                )
                expected = preflight[f"b2_fast_event_{key}"]["projection"]
                self.assertEqual(len(result["fills"]), len(expected["fills"]))
                self.assertEqual(len(result["orders"]), len(expected["orders"]))
                for actual, reference in zip(result["fills"], expected["fills"]):
                    self.assertEqual(actual["session_date"], reference["date"])
                    self.assertEqual(actual["instrument_id"], reference["symbol"] + ".SYNTH")
                    self.assertEqual(actual["side"], reference["side"].lower())
                    self.assertEqual(actual["quantity"], reference["quantity"])
                    for actual_key, reference_key in (
                        ("raw_open", "reference_price"), ("price", "fill_price"),
                        ("notional", "gross_value"), ("commission", "commission"),
                        ("tax", "tax"), ("slippage", "slippage_cost"),
                    ):
                        self.assertAlmostEqual(actual[actual_key], reference[reference_key], delta=1e-6)
                self.assertEqual(len(result["sessions"]), len(expected["ledger"]))
                for session, reference in zip(result["sessions"], expected["ledger"]):
                    self.assertEqual(session["session_date"], reference["date"])
                    close = session["close"]
                    self.assertAlmostEqual(close["cash"], reference["cash"], delta=1e-6)
                    self.assertAlmostEqual(close["equity"], reference["nav"], delta=1e-6)
                    self.assertEqual(
                        {key: row["quantity"] for key, row in close["holdings"].items()},
                        {row["symbol"] + ".SYNTH": row["quantity"] for row in reference["holdings"]},
                    )
                    for holding in reference["holdings"]:
                        self.assertEqual(close["holdings"][holding["symbol"] + ".SYNTH"]["price"],
                                         holding["mark_price"])
                for field, total in (("commission", "commission"), ("tax", "tax"),
                                     ("slippage", "slippage_cost"), ("total_cost", "total_cost")):
                    self.assertAlmostEqual(sum(row[field] for row in result["fills"]),
                                           expected["summary"][total], delta=1e-6)
                # Blocked diagnostics use the MVP-3 schema, not the legacy side/quantity encoding.
                self.assertEqual(
                    sorted((row["session_date"], row["instrument_id"], row["reason"])
                           for row in result["orders"] if row["status"] == "blocked"),
                    sorted((row["attempt_date"], row["symbol"] + ".SYNTH", row["reason"].lower())
                           for row in expected["orders"] if row["reason"]),
                )

    def test_ma20_60_crosses_only_after_warmup_and_executes_next_open(self):
        for sizing in ("lot", "vector_parity"):
            result = self.run_cli(MA, strategy="s3", sizing=sizing)
            self.assertEqual(len(result["sessions"]), 130)
            entries = [(row["session_date"], row["targets"]) for row in result["decisions"] if row["targets"]]
            # 60 flat closes give gap=0. First 110 close at index 60 makes gap>0.
            self.assertEqual(entries[0], (result["sessions"][60]["session_date"], {"A.SYNTH": 1 / 3}))
            self.assertEqual(result["fills"][0]["session_date"], result["sessions"][61]["session_date"])
            self.assertEqual(result["fills"][0]["instrument_id"], "A.SYNTH")
            # At index 84 gap>0; index 85 gap<0. Sell on index 86, not index 85.
            self.assertEqual(result["decisions"][-1]["session_date"], result["sessions"][85]["session_date"])
            self.assertEqual(result["decisions"][-1]["targets"], {})
            self.assertEqual(result["fills"][-1]["session_date"], result["sessions"][86]["session_date"])
            self.assertEqual(result["fills"][-1]["side"], "sell")
            self.assertFalse(result["sessions"][-1]["close"]["holdings"])

    def test_s2_matches_existing_independent_vector_decisions_and_opens(self):
        result = self.run_cli(SMALL, strategy="s2", sizing="vector_parity")
        previous = json.loads(
            (ROOT / "poc/mvp1-golden/expected/dataset-v1.json").read_text()
        )["vector"]
        self.assertEqual(
            [(row["session_date"], row["targets"]) for row in result["decisions"]],
            [(row["decision_session"], row["targets"]) for row in previous["decisions"]],
        )
        self.assertEqual(
            [(row["session_date"], row["kind"], row["blocked"], row["skipped_buys"])
             for row in result["executions"]],
            [(row["session_date"], row["kind"], row["blocked"], row["skipped_buys"])
             for row in previous["executions"]],
        )
        # Existing Vector session NAV includes the NEXT open's mark return;
        # constant raw opens here make it exactly this Session's post-fill NAV.
        for current, old in zip(result["sessions"], previous["sessions"]):
            self.assertTrue(
                math.isclose(current["open"]["nav"], old["nav"], rel_tol=1e-9, abs_tol=0),
                (current["session_date"], current["open"]["nav"], old["nav"]),
            )

    def test_hand_vector_parity_charges_weight_cost_not_fill_notional(self):
        result = self.run_cli(sizing="vector_parity")
        # D1 has only a close decision. D2 E0=1000, buy weight delta=1.
        # Fees: commission=10, slippage=10, tax=20; E1=960; q=960/10=96.
        fill = result["fills"][0]
        self.assertEqual(fill["quantity"], 96)
        self.assertEqual(fill["price"], 10)
        self.assertEqual(fill["notional"], 960)
        self.assertEqual(fill["fee_basis"], 1000)
        self.assertEqual(fill["commission"], 10)
        self.assertEqual(fill["minimum_commission_top_up"], 0)
        self.assertEqual(fill["slippage"], 10)
        self.assertEqual(fill["tax"], 20)
        self.assertEqual(fill["cash_change"], -1000)
        # D2 open cash=0, equity=960; close equity=96*12=1152.
        self.assertEqual(result["sessions"][1]["open"]["cash"], 0)
        self.assertEqual(result["sessions"][1]["open"]["equity"], 960)
        self.assertEqual(result["sessions"][1]["close"]["equity"], 1152)
        # D3 re-marks without another decision or Fill.
        self.assertEqual(result["sessions"][2]["open"]["equity"], 960)
        self.assertEqual(result["sessions"][2]["close"]["equity"], 1152)
        self.assertEqual(len(result["fills"]), 1)

    def test_retry_buys_only_missing_leg_at_current_equity(self):
        fixture = json.loads(HAND.read_text())
        fixture["calendar"] += ["2026-01-08"]
        fixture["instruments"] = [
            {"symbol": "A", "lot_size": 1, "opens": [10, 10, 20, 20], "closes": [10, 20, 20, 20]},
            {"symbol": "B", "lot_size": 1, "opens": [10, 10, 10, 10], "closes": [10, 10, 10, 10]},
        ]
        fixture["costs"] = {key: 0 for key in fixture["costs"]}
        fixture["execution_status_overrides"] = [
            {"symbol": "B", "date": "2026-01-06", "trade_status": "HALTED",
             "is_tradable": False, "available_at": "08:50:00+08:00"},
        ]
        result = self.run_cli(fixture)
        # D2 A=50, B blocked, cash=500. D3 E0=1500 so B budget=750.
        # Existing A must remain 50, not be reduced to 37.5.
        self.assertEqual(
            [(fill["instrument_id"], fill["quantity"]) for fill in result["fills"]],
            [("A.SYNTH", 50), ("B.SYNTH", 50)],
        )
        # Only 500 cash is available: B's requested 750 budget is reduced.
        self.assertEqual(result["executions"][1]["retry_only"], True)
        self.assertIsNone(result["pending_at_end"])
        self.assertEqual(result["sessions"][2]["open"]["holdings"]["A.SYNTH"]["quantity"], 50)
        # ADR 0019: parity retry also cuts the budget to cash instead of failing.
        cut = self.run_cli(fixture, sizing="vector_parity")
        self.assertEqual(
            [(fill["instrument_id"], fill["quantity"]) for fill in cut["fills"]],
            [("A.SYNTH", 50), ("B.SYNTH", 50)],
        )
        self.assertAlmostEqual(cut["sessions"][-1]["close"]["cash"], 0, delta=1e-9)
        for sizing in ("lot", "vector_parity"):
            skipped = self.run_cli(fixture, sizing=sizing, retry="skip")
            self.assertEqual(len(skipped["fills"]), 1)
        fixture["instruments"][0]["opens"][2:] = [5, 5]
        sufficient = self.run_cli(fixture, sizing="vector_parity")
        self.assertEqual(
            [(fill["instrument_id"], fill["quantity"]) for fill in sufficient["fills"]],
            [("A.SYNTH", 50), ("B.SYNTH", 37.5)],
        )

    def test_halted_and_missing_price_do_not_mean_zero_valuation(self):
        fixture = json.loads(HAND.read_text())
        fixture["instruments"][0]["opens"] = [10, 11, 99]
        fixture["instruments"][0]["closes"] = [10, 12, None]
        fixture["execution_status_overrides"] = [
            {"symbol": "A", "date": "2026-01-07", "trade_status": "HALTED",
             "is_tradable": False, "available_at": "08:50:00+08:00"}
        ]
        result = self.run_cli(fixture)
        open_mark = result["sessions"][2]["open"]["holdings"]["A.SYNTH"]
        close_mark = result["sessions"][2]["close"]["holdings"]["A.SYNTH"]
        self.assertEqual((open_mark["price"], close_mark["price"]), (11, 12))
        self.assertEqual(open_mark["price_source"]["kind"], "carried")
        self.assertEqual(close_mark["price_source"]["session_date"], "2026-01-06")
        fixture["instruments"][0]["closes"][2] = 13
        result = self.run_cli(fixture)
        self.assertEqual(result["sessions"][2]["open"]["holdings"]["A.SYNTH"]["price"], 99)
        self.assertEqual(result["sessions"][2]["close"]["holdings"]["A.SYNTH"]["price"], 13)

    def test_unknown_or_late_execution_status_fails_closed(self):
        for status in (None, {"trade_status": "UNKNOWN", "is_tradable": False, "available_at": None},
                       {"trade_status": "TRADABLE", "is_tradable": True, "available_at": "10:00:00+08:00"}):
            fixture = json.loads(HAND.read_text())
            fixture["execution_status_default"] = status
            result = self.run_cli(fixture)
            self.assertFalse(result["fills"])
            self.assertTrue(result["pending_at_end"])
            self.assertEqual(result["sessions"][-1]["close"]["cash"], 1000)
            self.assertTrue(all(order["status"] == "blocked" for order in result["orders"]))

    def test_committed_goldens_recompute_every_value_and_preserve_ledger(self):
        cases = [(SMALL, strategy) for strategy in ("s1", "s2", "s3")] + [
            (MA, "s3"), (HAND, "s1"), (RETRY, "s1"), (REBALANCE, "s2"),
        ]
        for fixture, strategy in cases:
            for sizing in ("lot", "vector_parity"):
                with self.subTest(fixture=fixture.name, strategy=strategy, sizing=sizing):
                    expected = json.loads(
                        (GOLDEN / "expected" / f"{fixture.stem}.{strategy}.{sizing}.json").read_text()
                    )
                    actual = self.run_cli(fixture, strategy=strategy, sizing=sizing)
                    self.assertEqual(actual, expected)
                    self.assertEqual(actual["fixture_sha256"], hashlib.sha256(fixture.read_bytes()).hexdigest())
                    previous_cash = actual["parameters"]["initial_cash"]
                    previous_quantities = {}
                    for session in actual["sessions"]:
                        fills = [fill for fill in actual["fills"] if fill["session_date"] == session["session_date"]]
                        for fill in fills:
                            sign = 1 if fill["side"] == "buy" else -1
                            previous_cash += fill["cash_change"]
                            instrument = fill["instrument_id"]
                            previous_quantities[instrument] = previous_quantities.get(instrument, 0) + sign * fill["quantity"]
                            self.assertAlmostEqual(
                                fill["cash_change"], -sign * fill["notional"] - fill["cash_fees"], delta=1e-6,
                            )
                            self.assertAlmostEqual(
                                fill["total_cost"], fill["commission"] + fill["tax"] + fill["slippage"], delta=1e-6,
                            )
                            self.assertGreaterEqual(fill["minimum_commission_top_up"], 0)
                        self.assertAlmostEqual(session["open"]["cash"], previous_cash, delta=1e-6)

                        for point in ("open", "close"):
                            value = session[point]
                            self.assertGreaterEqual(value["cash"], -1e-6)
                            self.assertAlmostEqual(
                                value["equity"], value["cash"] + sum(
                                    row["quantity"] * row["price"] for row in value["holdings"].values()
                                ), delta=1e-6,
                            )
                            for instrument, quantity in previous_quantities.items():
                                self.assertAlmostEqual(
                                    value["holdings"].get(instrument, {}).get("quantity", 0), quantity, delta=1e-12,
                                )
                        self.assertEqual(session["open"]["cash"], session["close"]["cash"])
                        filled_orders = [order for order in actual["orders"]
                                         if order["session_date"] == session["session_date"] and order["status"] == "filled"]
                        self.assertEqual([order["order_id"] for order in filled_orders], [fill["order_id"] for fill in fills])
                        self.assertEqual(
                            [(order["side"], order["instrument_id"]) for order in filled_orders],
                            sorted([(order["side"], order["instrument_id"]) for order in filled_orders],
                                   key=lambda item: (item[0] != "sell", item[1])),
                        )

    def test_entry_only_goldens_recompute_every_value(self):
        cases = [
            (SMALL, "s1", "FixedInitial", False),
            (SMALL, "s1", "FixedInitial", True),
            (SMALL, "s2", "RemainingCash", False),
            (MA, "s3", "RemainingCash", False),
        ]
        for fixture, strategy, budget, poc0_s1_timing in cases:
            with self.subTest(fixture=fixture.name, strategy=strategy, poc0=poc0_s1_timing):
                variant = ".poc0" if poc0_s1_timing else ""
                expected = json.loads(
                    (GOLDEN / "expected" /
                     f"{fixture.stem}.{strategy}{variant}.entry_only.lot.json").read_text()
                )
                actual = self.run_cli(
                    fixture, strategy=strategy, sizing="lot", rebalance="entry_only",
                    poc0_s1_timing=poc0_s1_timing,
                )
                self.assertEqual(actual, expected)
                self.assertEqual(actual["parameters"]["rebalance"], "entry_only")
                self.assertEqual(actual["parameters"]["sizing"], "lot")
                self.assertEqual(actual["parameters"]["entry_budget"], budget)
                self.assertEqual(actual["parameters"]["unfilled_entry"], "retry")
                cash = actual["parameters"]["initial_cash"]
                quantities = {}
                for session in actual["sessions"]:
                    for fill in actual["fills"]:
                        if fill["session_date"] == session["session_date"]:
                            cash += fill["cash_change"]
                            instrument = fill["instrument_id"]
                            sign = 1 if fill["side"] == "buy" else -1
                            quantities[instrument] = quantities.get(instrument, 0) + sign * fill["quantity"]
                            self.assertAlmostEqual(fill["cash_change"],
                                                   -sign * fill["notional"] - fill["cash_fees"], delta=1e-6)
                    for point in ("open", "close"):
                        value = session[point]
                        self.assertAlmostEqual(value["cash"], cash, delta=1e-6)
                        self.assertGreaterEqual(value["cash"], 0)
                        self.assertAlmostEqual(
                            value["equity"], cash + sum(row["quantity"] * row["price"]
                                                       for row in value["holdings"].values()), delta=1e-6,
                        )
                        for instrument, quantity in quantities.items():
                            self.assertEqual(value["holdings"].get(instrument, {}).get("quantity", 0),
                                             quantity)

    def test_poc_s1_timing_is_rejected_outside_s1_entry_only(self):
        for strategy, rebalance in (("s1", "full_target"), ("s2", "entry_only"), ("s3", "entry_only")):
            stderr = self.run_cli(
                MA if strategy == "s3" else SMALL, strategy=strategy, rebalance=rebalance,
                poc0_s1_timing=True, expect_error=True,
            )
            self.assertIn("poc0_s1_timing requires S1 lot + entry_only", stderr)

    def test_minimum_commission_top_up_and_both_directional_taxes(self):
        fixture = json.loads(SMALL.read_text())
        fixture["costs"]["buy_tax_rate"] = 0.002
        fixture["costs"]["sell_tax_rate"] = 0.003
        fixture["costs"]["sell_slippage_bps"] = 20
        for sizing in ("lot", "vector_parity"):
            result = self.run_cli(fixture, strategy="s2", sizing=sizing)
            self.assertTrue(any(fill["side"] == "sell" for fill in result["fills"]))
            for fill in result["fills"]:
                basis = fill["fee_basis"]
                side = fill.get("fee_side", fill["side"])
                self.assertAlmostEqual(fill["tax"], basis * fixture["costs"][f"{side}_tax_rate"])
                if sizing == "lot":
                    self.assertAlmostEqual(fill["commission"], max(basis * 0.001, 100))
                else:
                    self.assertAlmostEqual(fill["commission"], basis * 0.001)
            if sizing == "lot":
                self.assertTrue(any(fill["minimum_commission_top_up"] > 0 for fill in result["fills"]))

    def test_pending_retry_can_defer_then_be_replaced_by_a_new_decision(self):
        fixture = json.loads(SMALL.read_text())
        fixture["strategy"]["top_n"] = 2
        fixture["strategy"]["rebalance_every"] = 3
        fixture["execution_status_overrides"] += [
            {"symbol": "A", "date": "2026-01-09", "trade_status": "HALTED",
             "is_tradable": False, "available_at": "08:50:00+08:00"},
            {"symbol": "C", "date": "2026-01-12", "trade_status": "UNKNOWN",
             "is_tradable": False, "available_at": "08:50:00+08:00"},
        ]
        result = self.run_cli(fixture, strategy="s2", sizing="lot", retry="retry")
        # D3 targets A/C; D4 fills A but skips C. D5 retry is deferred by held A.
        attempts = result["executions"]
        self.assertEqual((attempts[0]["session_date"], attempts[0]["skipped_buys"]), ("2026-01-08", ["C.SYNTH"]))
        self.assertEqual((attempts[1]["kind"], attempts[1]["retry_only"], attempts[1]["blocked"]),
                         ("deferred", True, ["A.SYNTH"]))
        self.assertEqual(attempts[2]["retry_only"], True)
        # D6 close scheduled targets B/C replace pending; D7 uses that new decision.
        self.assertEqual(attempts[3]["decision_session"], "2026-01-12")
        self.assertFalse(attempts[3]["retry_only"])

    def test_future_closes_do_not_change_earlier_signals_or_fills(self):
        fixture = json.loads(SMALL.read_text())
        original = self.run_cli(fixture, strategy="s2", sizing="vector_parity")
        for instrument in fixture["instruments"]:
            instrument["closes"][7:] = [250, 10, 500]
        changed = self.run_cli(fixture, strategy="s2", sizing="vector_parity")
        cutoff = fixture["calendar"][7]
        for key in ("sessions", "decisions", "executions", "orders", "fills"):
            self.assertEqual(
                [row for row in original[key] if row["session_date"] < cutoff],
                [row for row in changed[key] if row["session_date"] < cutoff],
            )

    def test_ma_window_does_not_bridge_missing_close(self):
        fixture = json.loads(MA.read_text())
        # Expanded 3x130 version for a gap immediately before the first cross.
        baseline = self.run_cli(fixture, strategy="s3")
        fixture = {
            "calendar": [row["session_date"] for row in baseline["sessions"]],
            "bar_defaults": {"open": 100, "open_available_at": "09:30:00+08:00", "close_available_at": "15:00:00+08:00"},
            "instruments": [
                {"symbol": "A", "lot_size": 100, "closes": [100] * 60 + [110] * 20 + [80] * 50},
                {"symbol": "B", "lot_size": 100, "closes": [100] * 130},
                {"symbol": "C", "lot_size": 100, "closes": [100] * 130},
            ],
            "account": {"initial_cash": 100000},
            "costs": baseline["parameters"]["costs"],
            "execution_status_default": {"trade_status": "TRADABLE", "is_tradable": True, "available_at": "08:50:00+08:00"},
        }
        fixture["instruments"][0]["closes"][59] = None
        for rebalance in ("full_target", "entry_only"):
            result = self.run_cli(fixture, strategy="s3", rebalance=rebalance)
            self.assertEqual(result["sessions"][60]["ma20_60"]["A.SYNTH"]["status"], "missing_input")
            self.assertEqual(result["sessions"][118]["ma20_60"]["A.SYNTH"]["status"], "missing_input")
            self.assertEqual(result["sessions"][119]["ma20_60"]["A.SYNTH"]["status"], "ok")
            self.assertFalse(result["fills"])

    def test_unpriced_unheld_instrument_is_skipped_then_retried(self):
        fixture = json.loads(HAND.read_text())
        fixture["instruments"].append(
            {"symbol": "B", "lot_size": 10, "closes": [None, None, 10]}
        )
        fixture["costs"] = {key: 0 for key in fixture["costs"]}
        for sizing in ("lot", "vector_parity"):
            result = self.run_cli(fixture, sizing=sizing)
            self.assertEqual(
                [(fill["session_date"], fill["instrument_id"], fill["quantity"])
                 for fill in result["fills"]],
                [("2026-01-06", "A.SYNTH", 50), ("2026-01-07", "B.SYNTH", 50)],
            )
            self.assertEqual(result["executions"][0]["skipped_buys"], ["B.SYNTH"])

    def test_s2_equal_close_and_trend_mean_is_not_filtered(self):
        fixture = json.loads(SMALL.read_text())
        fixture["instruments"] = [
            {"symbol": "A", "lot_size": 100, "closes": [100] * 10},
        ]
        fixture["missing_bars"] = []
        fixture["execution_status_overrides"] = []
        fixture["strategy"]["trend_filter"] = True
        fixture["strategy"]["trend_window"] = 2
        result = self.run_cli(fixture, strategy="s2")
        self.assertEqual(result["decisions"][0]["session_date"], fixture["calendar"][2])
        self.assertEqual(result["decisions"][0]["targets"], {"A.SYNTH": 1})

    def test_costed_parity_retry_budget_uses_pre_fee_equity_and_cuts_to_cash(self):
        result = self.run_cli(RETRY, sizing="vector_parity")
        first, retry = result["fills"]
        # D2: E0=1000, cost=5, A=0.5*995/10. D3: E0=497.5+49.75*10=995.
        self.assertEqual(first["quantity"], 49.75)
        self.assertEqual(result["executions"][1]["retry_only"], True)
        # Budget 0.5*995 does not fit with 1% fee: buy = cash_weight/1.01.
        budget = 995 * (497.5 / 995) / 1.01
        self.assertAlmostEqual(retry["notional"], budget, delta=1e-9)
        self.assertAlmostEqual(retry["commission"], budget * 0.01, delta=1e-9)
        self.assertAlmostEqual(retry["quantity"], budget / 10, delta=1e-9)
        self.assertAlmostEqual(result["sessions"][-1]["close"]["cash"], 0, delta=1e-9)
        self.assertEqual(first["quantity"], result["sessions"][2]["open"]["holdings"]["A.SYNTH"]["quantity"])

    def test_retry_assumptions_describe_supported_cash_cut(self):
        result = self.run_cli(RETRY, sizing="vector_parity")
        self.assertIn(
            "vector_parity retry uses pre-fee equity budgets, fees paid from cash; "
            "cash-insufficient budgets cut in ascending instrument_id order",
            result["assumptions"],
        )
        self.assertFalse(any("unresolved" in row or "rejected" in row for row in result["assumptions"]))

    def test_continuing_holdings_are_trimmed_and_topped_up_by_full_target(self):
        result = self.run_cli(REBALANCE, strategy="s2", sizing="vector_parity")
        trim, top_up = [fill for fill in result["fills"] if fill["session_date"] == "2026-01-09"]
        # D5 open: A=49.5*15, B=49.5*10, E0=1237.5; each target 0.5 after 2.475 cost.
        self.assertEqual((trim["instrument_id"], trim["side"]), ("A.SYNTH", "sell"))
        self.assertAlmostEqual(trim["quantity"], 49.5 - 0.5 * 1235.025 / 15, delta=1e-9)
        self.assertEqual((top_up["instrument_id"], top_up["side"]), ("B.SYNTH", "buy"))
        self.assertAlmostEqual(top_up["quantity"], 0.5 * 1235.025 / 10 - 49.5, delta=1e-9)

    def test_float_zero_weight_delta_is_no_trade_not_a_dust_fill(self):
        result = self.run_cli(SMALL, strategy="s2", sizing="vector_parity")
        self.assertTrue(all(fill["quantity"] > 1e-6 for fill in result["fills"]))
        self.assertTrue(all(order["quantity"] is None or order["quantity"] > 1e-6
                            for order in result["orders"]))

    def test_flat_open_rotation_resizes_zero_delta_holding_after_costs(self):
        fixture = json.loads(REBALANCE.read_text())
        fixture["instruments"] = [
            {"symbol": "A", "lot_size": 1, "opens": [10] * 6,
             "closes": [10, 10, 12, 13, 14, 15]},
            {"symbol": "B", "lot_size": 1, "opens": [10] * 6,
             "closes": [10, 10, 11, 10, 10, 10]},
            {"symbol": "C", "lot_size": 1, "opens": [10] * 6,
             "closes": [10, 10, 10, 12, 12, 12]},
        ]
        result = self.run_cli(fixture, strategy="s2", sizing="vector_parity")
        self.assertEqual(
            [row["targets"] for row in result["decisions"][:3]],
            [{"A.SYNTH": 0.5, "B.SYNTH": 0.5},
             {"A.SYNTH": 0.5, "C.SYNTH": 0.5},
             {"A.SYNTH": 0.5, "B.SYNTH": 0.5}],
        )
        # D4 buys 49.5 each of A/B. D5 E0=990; rotating B to C costs 9.9.
        # E1=980.1 requires A to sell 0.495 despite its zero weight-delta basis.
        rotation = [fill for fill in result["fills"] if fill["session_date"] == "2026-01-09"]
        self.assertEqual(
            [(fill["instrument_id"], fill["side"]) for fill in rotation],
            [("A.SYNTH", "sell"), ("B.SYNTH", "sell"), ("C.SYNTH", "buy")],
        )
        for fill, quantity, basis, commission in zip(
            rotation, (0.495, 49.5, 49.005), (0, 495, 495), (0, 4.95, 4.95),
        ):
            self.assertAlmostEqual(fill["quantity"], quantity, delta=1e-12)
            self.assertEqual(fill["fee_basis"], basis)
            self.assertEqual(fill["commission"], commission)
            self.assertEqual(fill["tax"], 0)
            self.assertEqual(fill["slippage"], 0)
        for index, equity, holdings in (
            (4, 980.1, {"A.SYNTH": 49.005, "C.SYNTH": 49.005}),
            (5, 970.299, {"A.SYNTH": 48.51495, "B.SYNTH": 48.51495}),
        ):
            value = result["sessions"][index]["open"]
            self.assertAlmostEqual(value["cash"], 0, delta=1e-9)
            self.assertAlmostEqual(value["equity"], equity, delta=1e-9)
            self.assertEqual(set(value["holdings"]), set(holdings))
            for instrument, quantity in holdings.items():
                self.assertAlmostEqual(value["holdings"][instrument]["quantity"], quantity, delta=1e-12)
        cash = 1000
        for session in result["sessions"]:
            cash += math.fsum(fill["cash_change"] for fill in result["fills"]
                              if fill["session_date"] == session["session_date"])
            for point in ("open", "close"):
                value = session[point]
                self.assertAlmostEqual(value["cash"], cash, delta=1e-9)
                self.assertGreaterEqual(value["cash"], -1e-9)
                self.assertAlmostEqual(
                    value["equity"], cash + math.fsum(
                        row["quantity"] * row["price"] for row in value["holdings"].values()
                    ), delta=1e-9,
                )
