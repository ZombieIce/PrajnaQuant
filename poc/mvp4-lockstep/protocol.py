"""Spike-only, Backend-neutral Arrow packet and JSON-lines primitives."""

from decimal import Decimal, localcontext
import json
from pathlib import Path
import re
from typing import TextIO

import pyarrow as pa
import pyarrow.ipc as ipc

VERSION = "mvp4-lockstep-spike@1"
PINNED_VERSION = "2.0.0rc6"
WIRE_PROBE = {
    "price": "1.123456789012345678",
    "quantity": "0.000000000000000001",
    "fee": "99999999999999999999.999999999999999999",
}


def decimal_value(text: str) -> Decimal:
    if not isinstance(text, str) or not re.fullmatch(r"-?[0-9]+(?:\.[0-9]{1,18})?", text):
        raise ValueError(f"expected a scale <= 18 decimal string, got {text!r}")
    value = Decimal(text)
    if value.copy_abs() >= Decimal("1e20"):
        raise ValueError(f"decimal exceeds Domain scale-18 range: {text}")
    return value


def encode(value: Decimal) -> str:
    text = format(value, "f")
    decimal_value(text)
    return text


def send(stream: TextIO, message: dict) -> None:
    stream.write(json.dumps(message, separators=(",", ":")) + "\n")
    stream.flush()


def receive(stream: TextIO, expected: str) -> dict:
    line = stream.readline()
    if not line:
        raise EOFError(f"controller disconnected while waiting for {expected}")
    message = json.loads(line)
    if message["type"] != expected:
        raise ValueError(f"expected {expected}, got {message['type']}")
    return message


def write_packet(path: Path, *, halt: bool = False) -> None:
    instruments = [
        {"instrument_id": f"{symbol}.SIM", "price_precision": "2",
         "price_increment": "0.01", "lot_size": "1", "currency": "USD"}
        for symbol in ("A", "B", "C")
    ]
    rows = []
    opens = [("10", "20", "30"), ("12", "20", "30"), ("13", "18", "32")]
    closes = [("11", "21", "30"), ("12", "19", "31"), ("14", "18", "32")]
    for index in range(3):
        open_ns = 1_767_756_600_000_000_000 + index * 86_400_000_000_000
        for column, spec in enumerate(instruments):
            rows.append({
                "session_id": f"s{index + 1}", "instrument_id": spec["instrument_id"],
                "open_ns": open_ns, "close_ns": open_ns + 3_600_000_000_000,
                "open": opens[index][column], "close": closes[index][column],
                "execution_status": "HALT" if halt and column == 2 else "TRADING",
            })
    schema = pa.schema([
        ("session_id", pa.string()), ("instrument_id", pa.string()),
        ("open_ns", pa.int64()), ("close_ns", pa.int64()),
        ("open", pa.string()), ("close", pa.string()), ("execution_status", pa.string()),
    ], metadata={
        b"protocol": VERSION.encode(),
        b"instruments": json.dumps(instruments).encode(),
        b"buy_slippage": b"0.01", b"sell_slippage": b"0.01",
        b"decimal_probe": json.dumps(WIRE_PROBE).encode(),
    })
    with ipc.new_file(path, schema) as writer:
        writer.write_table(pa.Table.from_pylist(rows, schema=schema))


def read_packet(path: Path) -> tuple[list[dict], list[dict], Decimal, Decimal, dict[str, str]]:
    with ipc.open_file(path) as reader:
        table = reader.read_all()
    metadata = table.schema.metadata
    if metadata[b"protocol"].decode() != VERSION:
        raise ValueError("unsupported packet protocol")
    specs = json.loads(metadata[b"instruments"])
    rows = table.to_pylist()
    ids = {s["instrument_id"] for s in specs}
    if len(ids) != len(specs) or not ids or not rows:
        raise ValueError("packet requires unique instruments and nonempty bars")
    sessions: dict[str, list[dict]] = {}
    for row in rows:
        if row["instrument_id"] not in ids or row["open_ns"] >= row["close_ns"]:
            raise ValueError(f"invalid bar identity or time: {row}")
        if row["execution_status"] not in {"TRADING", "HALT"}:
            raise ValueError(f"unmapped execution status: {row['execution_status']}")
        if decimal_value(row["open"]) <= 0 or decimal_value(row["close"]) <= 0:
            raise ValueError("prices must be positive")
        sessions.setdefault(row["session_id"], []).append(row)
    previous_close = -1
    for bars in sessions.values():
        if len(bars) != len(ids) or {r["instrument_id"] for r in bars} != ids:
            raise ValueError("each Session must contain exactly one bar per instrument")
        if len({(r["open_ns"], r["close_ns"]) for r in bars}) != 1:
            raise ValueError("inconsistent Session boundaries")
        if bars[0]["open_ns"] <= previous_close:
            raise ValueError("Sessions must be ordered and nonoverlapping")
        previous_close = bars[0]["close_ns"]
    buy = decimal_value(metadata[b"buy_slippage"].decode())
    sell = decimal_value(metadata[b"sell_slippage"].decode())
    if not 0 <= buy < 1 or not 0 <= sell < 1:
        raise ValueError("slippage must be in [0, 1)")
    probe = {
        key: encode(decimal_value(value))
        for key, value in json.loads(metadata[b"decimal_probe"]).items()
    }
    return specs, rows, buy, sell, probe


def synthetic_prices(raw: str, buy: Decimal, sell: Decimal) -> tuple[str, str]:
    with localcontext() as context:
        context.prec = 80
        price = decimal_value(raw)
        return encode(price * (1 - sell)), encode(price * (1 + buy))
