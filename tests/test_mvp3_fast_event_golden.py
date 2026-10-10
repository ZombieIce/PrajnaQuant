import json
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "poc/mvp3-golden"
SCRIPT = GOLDEN / "fast_event_golden.py"
HAND = GOLDEN / "fixtures/hand-v1.json"
SMALL = ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
MA = ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json"


class FastEventGoldenTests(unittest.TestCase):
    def run_cli(self, fixture=HAND, strategy="s1", sizing="lot", retry=None, expect_error=False):
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
            if retry is not None:
                command.extend(["--unfilled-entry", retry])
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
            self.assertAlmostEqual(current["open"]["nav"], old["nav"], delta=1e-9)

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
        self.assertIn("vector_parity retry cash shortfall", self.run_cli(
            fixture, sizing="vector_parity", expect_error=True,
        ))
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
            cases = [(SMALL, strategy) for strategy in ("s1", "s2", "s3")] + [(MA, "s3"), (HAND, "s1")]
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
        result = self.run_cli(fixture, strategy="s3")
        self.assertEqual(result["sessions"][60]["ma20_60"]["A.SYNTH"]["status"], "missing_input")
        self.assertEqual(result["sessions"][118]["ma20_60"]["A.SYNTH"]["status"], "missing_input")
        self.assertEqual(result["sessions"][119]["ma20_60"]["A.SYNTH"]["status"], "ok")
        self.assertFalse(result["fills"])
