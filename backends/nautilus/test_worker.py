"""Real worker protocol tests; dependencies are required, never skipped."""

import json
from pathlib import Path
import subprocess
import sys
import unittest
from datetime import date
from decimal import Decimal
import tempfile

import pyarrow as pa
import pyarrow.parquet as pq


def packet(directory: Path, *, halted=False, precise=False, unrepresentable=False):
    day = date(2026, 1, 5)
    start = 1767576600000000000
    increment = Decimal("0.000000000000000001" if precise else "0.01")
    opens = ["1.123456789012345678"] * 2 if precise else ["10.01" if unrepresentable else "10", "12"]
    close = ["1.123456789012345678"] * 2 if precise else ["11", "13"]
    instrument = "A.SYNTH"
    dsv = "dsv:sha256:" + "0" * 64
    (directory / "packet.json").write_text(json.dumps({"protocol": "prajna-adapter@1", "dsv": dsv}))
    tables = {
        "instruments": [{
            "instrument_id": instrument, "venue_id": "SYNTH", "kind": "Etf", "native_symbol": "A",
            "quote_currency": "CNY", "is_inverse": False, "size_increment": Decimal(1),
            "multiplier": Decimal(1), "lot_size": Decimal(1), "price_increment": increment,
        }],
        "sessions": [], "bars": [], "execution_status": [],
    }
    for index in range(2):
        session_day = date.fromordinal(day.toordinal() + index)
        ns = start + index * 86400000000000
        tables["sessions"].append({
            "venue_id": "SYNTH", "session_date": session_day, "ts_open": ns, "ts_close": ns + 1000000000,
        })
        tables["bars"].append({
            "instrument_id": instrument, "session_date": session_day, "ts_open": ns,
            "ts_close": ns + 1000000000, "open": Decimal(opens[index]),
            "high": Decimal(close[index]), "low": Decimal(opens[index]), "close": Decimal(close[index]),
        })
        tables["execution_status"].append({
            "instrument_id": instrument, "session_date": session_day,
            "is_tradable": not halted, "trade_status": "halted" if halted else "trading",
        })
    decimal_fields = {"size_increment", "multiplier", "lot_size", "price_increment", "open", "high", "low", "close"}
    for name, rows in tables.items():
        fields = []
        for key, value in rows[0].items():
            field_type = (pa.decimal128(38, 18) if key in decimal_fields else
                          pa.timestamp("ns", tz="UTC") if key in {"ts_open", "ts_close"} else
                          pa.date32() if isinstance(value, date) else
                          pa.bool_() if isinstance(value, bool) else pa.string())
            fields.append(pa.field(key, field_type))
        metadata = {f"prajna.{key}".encode(): value.encode() for key, value in {
            "table": name, "schema_version": "1", "dsv": dsv,
        }.items()}
        pq.write_table(pa.Table.from_pylist(rows, schema=pa.schema(fields, metadata=metadata)),
                       directory / f"{name}.parquet")
    costs = {
        "commission_rate": "0.01", "minimum_commission": "2",
        "buy_tax_rate": "0.001", "sell_tax_rate": "0.002",
        "buy_slippage_rate": "0.01", "sell_slippage_rate": "0.01",
    }
    if precise:
        costs = dict.fromkeys(costs, "0")
    if unrepresentable:
        costs["buy_slippage_rate"] = "0.001"
    (directory / "worker.json").write_text(json.dumps({"initial_cash": "1000", "costs": costs}))
    messages = [{"type": "begin_run", "protocol": "prajna-adapter@1", "packet": str(directory)}]
    for index, session in enumerate(tables["sessions"]):
        day_text = session["session_date"].isoformat()
        bid, ask = (opens[index], opens[index]) if precise else (
            "9.9" if index == 0 else "11.88", "10.1" if index == 0 else "12.12")
        messages.extend([
            {"type": "open", "session_date": day_text, "ts_open": str(session["ts_open"]),
             "quotes": [{"instrument_id": instrument, "raw_open": opens[index],
                         "bid": bid, "ask": ask, "tradable": not halted}]},
            {"type": "order", "order_id": str(index + 1), "portfolio_id": "7",
             "instrument_id": instrument, "side": "buy" if index == 0 else "sell",
             "quantity": "10", "submitted_at": str(session["ts_open"])},
            {"type": "close", "session_date": day_text, "ts_close": str(session["ts_close"])},
        ])
    messages.append({"type": "end_run"})
    return messages


def execute(messages):
    return subprocess.run(
        [sys.executable, str(Path(__file__).with_name("worker.py"))],
        input="".join(json.dumps(m) + "\n" for m in messages),
        capture_output=True, text=True, timeout=30,
    )


class WorkerTests(unittest.TestCase):
    def test_scale_18_instrument_is_rejected_before_native_position_panics(self):
        with tempfile.TemporaryDirectory() as directory:
            result = execute(packet(Path(directory), precise=True))
        self.assertEqual(result.returncode, 1, result.stderr + result.stdout)
        self.assertIn("precision 18 exceeds rc6 Position maximum 16", json.loads(result.stdout)["reason"])
        self.assertNotIn("panicked", result.stderr)

    def test_unrepresentable_synthetic_quote_is_reported_instead_of_rounded(self):
        with tempfile.TemporaryDirectory() as directory:
            result = execute(packet(Path(directory), unrepresentable=True))
        self.assertNotEqual(result.returncode, 0)
        message = json.loads(result.stdout)
        self.assertEqual(message["type"], "error")
        self.assertIn("unrepresentable price", message["reason"])
        self.assertIn("increment 0.01", message["reason"])
        self.assertNotIn('"type":"ready"', result.stdout)

    def test_native_halt_rejects_without_cash_or_position_mutations(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            result = execute(packet(path, halted=True))
            diagnostics = [json.loads(line) for line in (path / "backend-diagnostics.jsonl").read_text().splitlines()]
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        rejects = [e for e in events if e["type"] == "reject"]
        self.assertEqual(len(rejects), 2)
        self.assertTrue(all("Market A.PRAJNA is CLOSED" in e["reason"] for e in rejects))
        self.assertFalse(any(e["type"] == "fill" for e in events))
        self.assertEqual([e["cash"] for e in diagnostics if e["event"] == "account_balance"], ["1000.00"] * 2)
        self.assertEqual(len([e for e in diagnostics if e["category"] == "adapter_construction"
                              and e["event"] == "instrument_status" and e["action"] == "HALT"]), 2)
        self.assertEqual(len([e for e in diagnostics if e["category"] == "backend_native"
                              and e["event"] == "instrument_status" and e["action"] == "HALT"]), 2)

    def test_numeric_quantity_is_fatal_and_does_not_emit_done(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = packet(Path(directory))
            messages[2]["quantity"] = 10.0
            result = execute(messages)
        self.assertNotEqual(result.returncode, 0)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(events[-1]["type"], "error")
        self.assertIn("decimal string", events[-1]["reason"])
        self.assertFalse(any(e["type"] in {"fill", "done"} for e in events))

    def test_scale_18_slippage_overflow_is_reported_not_silently_quantized(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            messages = packet(path)
            config = json.loads((path / "worker.json").read_text())
            config["costs"]["buy_slippage_rate"] = "0.000000000000000001"
            (path / "worker.json").write_text(json.dumps(config))
            bars = pq.read_table(path / "bars.parquet")
            index = bars.schema.get_field_index("open")
            bars = bars.set_column(index, bars.schema.field(index),
                                   pa.array([Decimal("10.000000000000000001"), Decimal("12")],
                                            type=pa.decimal128(38, 18)))
            low_index = bars.schema.get_field_index("low")
            bars = bars.set_column(low_index, bars.schema.field(low_index), bars.column("open"))
            pq.write_table(bars, path / "bars.parquet")
            result = execute(messages)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("synthetic price", json.loads(result.stdout)["reason"])

    def test_costed_native_fills_and_cash_are_separate_from_adapter_constructions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            result = execute(packet(path))
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            events = [json.loads(line) for line in result.stdout.splitlines()]
            diagnostics = [json.loads(line) for line in (path / "backend-diagnostics.jsonl").read_text().splitlines()]
        fills = [e for e in events if e["type"] == "fill"]
        self.assertEqual([(e["price"], e["fee"], e["portfolio_id"]) for e in fills],
                         [("10.10", "2.10", "7"), ("11.88", "2.24", "7")])
        self.assertEqual([e["cash"] for e in diagnostics if e["event"] == "account_balance"],
                         ["896.90", "1013.46"])
        fees = [e for e in diagnostics if e["event"] == "fee_model"]
        self.assertEqual([Decimal(e["expected_fee"]) for e in fees], [Decimal("2.101"), Decimal("2.2376")])
        self.assertTrue(all(e["difference_class"] == "backend_native" for e in fees))
        self.assertEqual(len([e for e in diagnostics if e["event"] == "synthetic_bid_ask"
                              and e["category"] == "adapter_construction"]), 2)
        self.assertEqual(len([e for e in diagnostics if e["event"] == "fill"
                              and e["category"] == "backend_native"]), 2)

    def test_incompatible_protocol_is_reported_and_exits_unsuccessfully(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("worker.py"))],
            input=json.dumps({"type": "begin_run", "protocol": "unknown", "packet": "."}) + "\n",
            capture_output=True, text=True, timeout=30,
        )
        self.assertNotEqual(result.returncode, 0)
        message = json.loads(result.stdout)
        self.assertEqual(message["type"], "error")
        self.assertIn("protocol", message["reason"])


if __name__ == "__main__":
    unittest.main()
