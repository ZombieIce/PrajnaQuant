"""Real rc6 subprocess tests; missing dependencies are failures, never skips."""

from decimal import Decimal
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from controller import run_fixture
from protocol import VERSION, WIRE_PROBE, write_packet


class LockstepTests(unittest.TestCase):
    def test_external_orders_use_previous_fills_and_keep_state_across_sessions(self):
        result = run_fixture()
        self.assertEqual(
            [(s["cash"], s["positions"], s["nav"]) for s in result["ledger"]],
            [
                ("798.00", {"A.SIM": "10", "B.SIM": "5"}, "1013.00"),
                ("432.00", {"B.SIM": "5", "C.SIM": "16"}, "1023.00"),
                ("896.68", {"A.SIM": "10"}, "1036.68"),
            ],
        )
        self.assertEqual(
            [(e["order_id"], e["virtual_portfolio_id"], e["quantity"], e["price"])
             for e in result["events"] if e["type"] == "fill"],
            [
                ("s1-a", "alpha", "10", "10.10"),
                ("s1-b", "beta", "5", "20.20"),
                ("s2-a", "alpha", "10", "11.88"),
                ("s2-c", "alpha", "16", "30.30"),
                ("s3-b", "beta", "5", "17.82"),
                ("s3-c", "alpha", "16", "31.68"),
                ("s3-a", "alpha", "10", "13.13"),
            ],
        )
        for snapshot in result["ledger"]:
            self.assertEqual(Decimal(snapshot["cash"]), Decimal(snapshot["backend_cash"]))

    def test_native_halt_rejects_external_orders_without_changing_cash(self):
        result = run_fixture(halt=True)
        rejects = [e for e in result["events"] if e["type"] == "reject"]
        self.assertEqual(len(rejects), 3)
        self.assertTrue(all("Market C.SIM is CLOSED" in e["reason"] for e in rejects))
        self.assertEqual(
            len([e for e in result["events"] if e["type"] == "backend_status"
                 and e["instrument_id"] == "C.SIM" and e["action"] == "HALT"]),
            3,
        )
        self.assertTrue(all(e["virtual_portfolio_id"] == "alpha" for e in rejects))
        self.assertFalse(any(e["type"] == "fill" for e in result["events"]))
        self.assertEqual([s["cash"] for s in result["ledger"]], ["1000.00"] * 3)
        self.assertTrue(all(s["positions"] == {} for s in result["ledger"]))

    def test_reset_replays_same_engine_without_cash_positions_or_order_id_leakage(self):
        result = run_fixture(reset=True)
        self.assertEqual(len(result["ledger"]), 6)
        self.assertEqual(result["ledger"][:3], result["ledger"][3:])
        fills = [e for e in result["events"] if e["type"] == "fill"]
        self.assertEqual(fills[:7], fills[7:])
        self.assertEqual(result["native_stderr"], "")

    def test_scale_18_prices_quantities_and_fees_survive_arrow_and_stdio(self):
        result = run_fixture()
        ready = next(e for e in result["events"] if e["type"] == "ready")
        expected = {
            "price": "1.123456789012345678",
            "quantity": "0.000000000000000001",
            "fee": "99999999999999999999.999999999999999999",
        }
        self.assertEqual(ready["decimal_probe"], {"packet": expected, "stdio": expected})

    def test_invalid_numeric_order_is_an_explicit_worker_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            packet = Path(directory) / "run.arrow"
            write_packet(packet)
            messages = [{
                "type": "start", "protocol": VERSION, "packet": str(packet),
                "initial_cash": "1000.00", "reset_probe": False, "decimal_probe": WIRE_PROBE,
            }] + [{
                "type": "orders", "session_id": f"s{index}", "orders": [{
                    "order_id": f"invalid-{index}", "virtual_portfolio_id": "alpha",
                    "instrument_id": "A.SIM", "side": "BUY", "quantity": 1.0,
                }],
            } for index in range(1, 4)]
            result = subprocess.run(
                [sys.executable, str(Path(__file__).with_name("worker.py"))],
                input="".join(json.dumps(m) + "\n" for m in messages),
                capture_output=True, text=True, timeout=30,
            )
        self.assertNotEqual(result.returncode, 0)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertTrue(any(e["type"] == "error" and "decimal string" in e["reason"] for e in events))
        self.assertFalse(any(e["type"] == "done" for e in events))

    def test_backend_precision_distinguishes_exact_scale_18_from_unmappable_prices(self):
        from capture import price_observations

        result = price_observations()
        self.assertEqual(result["scale18"]["price"], "1.123456789012345678")
        self.assertEqual(result["scale18"]["raw"], "1123456789012345678")
        self.assertEqual(result["scale19"]["status"], "unsupported")
        self.assertEqual(result["synthetic_scale20"]["status"], "unsupported")
        self.assertEqual(result["instrument_tick_mismatch"]["status"], "unsupported")


if __name__ == "__main__":
    unittest.main()
