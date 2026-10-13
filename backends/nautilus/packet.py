"""Read the version-1 Rust RunPacket without float or datetime conversions."""

from decimal import Decimal
import json
from pathlib import Path
import re

import pyarrow as pa
import pyarrow.parquet as pq

VERSION = "prajna-adapter@1"
SCALE = Decimal("0.000000000000000001")


def decimal_value(text: str) -> Decimal:
    if not isinstance(text, str) or not re.fullmatch(r"-?[0-9]+(?:\.[0-9]{1,18})?", text):
        raise ValueError("expected a plain scale <= 18 decimal string")
    value = Decimal(text)
    if value.copy_abs() >= Decimal("1e20"):
        raise ValueError("decimal outside Domain scale-18 range")
    return value


def encode(value: Decimal) -> str:
    return format(value, "f")


def read_packet(path: Path) -> tuple[list[dict], list[dict], dict, dict]:
    header = json.loads((path / "packet.json").read_text())
    if header["protocol"] != VERSION:
        raise ValueError("unsupported Run packet protocol")
    if not re.fullmatch(r"dsv:sha256:[0-9a-f]{64}", header["dsv"]):
        raise ValueError("invalid Dataset Version")
    tables = {}
    for name in ("instruments", "sessions", "bars", "execution_status"):
        table = pq.read_table(path / f"{name}.parquet")
        metadata = table.schema.metadata or {}
        for key, expected in {
            "prajna.table": name, "prajna.schema_version": "1", "prajna.dsv": header["dsv"],
        }.items():
            if metadata.get(key.encode()) != expected.encode():
                raise ValueError(f"{name} {key} mismatch")
        for field in table.schema:
            if pa.types.is_floating(field.type):
                raise ValueError(f"{name}.{field.name}: floating-point packet column")
            if pa.types.is_decimal(field.type) and field.type != pa.decimal128(38, 18):
                raise ValueError(f"{name}.{field.name}: expected Decimal128(38,18)")
            if pa.types.is_timestamp(field.type):
                if field.type != pa.timestamp("ns", tz="UTC"):
                    raise ValueError(f"{name}.{field.name}: expected UTC nanoseconds")
                index = table.schema.get_field_index(field.name)
                table = table.set_column(index, field.name, table.column(index).cast(pa.int64()))
        tables[name] = table.to_pylist()
    specs = tables["instruments"]
    sessions = sorted(tables["sessions"], key=lambda row: row["ts_open"])
    if not specs or not sessions or not tables["bars"]:
        raise ValueError("empty Run packet")
    ids = {s["instrument_id"] for s in specs}
    dates = {s["session_date"].isoformat() for s in sessions}
    venue = sessions[0]["venue_id"]
    if len(ids) != len(specs) or len(dates) != len(sessions):
        raise ValueError("duplicate Instrument or Session")
    if any(s["venue_id"] != venue for s in specs + sessions):
        raise ValueError("worker requires one Venue")
    currency = specs[0]["quote_currency"]
    for spec in specs:
        if (spec["kind"] not in {"Equity", "Etf"} or spec["quote_currency"] != currency
                or not currency or spec["is_inverse"] or spec["size_increment"] != 1
                or spec["multiplier"] not in (None, Decimal(1))):
            raise ValueError("worker supports single-currency non-inverse Equity/ETF with integral size")
        lot = spec["lot_size"]
        if lot is None or lot <= 0 or lot != lot.to_integral_value():
            raise ValueError("Equity/ETF requires an explicit positive integral lot_size")
        if spec["price_increment"] <= 0:
            raise ValueError("price_increment must be positive")
    previous = None
    session_by_date = {}
    for session in sessions:
        day = session["session_date"].isoformat()
        if session["ts_open"] < 0 or session["ts_open"] + 2_000_000 >= session["ts_close"]:
            raise ValueError("invalid Session times")
        if previous is not None and session["ts_open"] <= previous + 2_000_000:
            raise ValueError("Sessions overlap the registered shim barrier")
        previous = session["ts_close"]
        session_by_date[day] = session
    bars = {}
    statuses = {}
    for name, target in (("bars", bars), ("execution_status", statuses)):
        for row in tables[name]:
            key = (row["instrument_id"], row["session_date"].isoformat())
            if key[0] not in ids or key[1] not in dates or key in target:
                raise ValueError(f"{name}: unknown or duplicate key")
            if name == "bars":
                session = session_by_date[key[1]]
                if (row["ts_open"], row["ts_close"]) != (session["ts_open"], session["ts_close"]):
                    raise ValueError("bar/Session time mismatch")
                if not (0 < row["low"] <= min(row["open"], row["close"])
                        <= max(row["open"], row["close"]) <= row["high"]):
                    raise ValueError("invalid OHLC")
            elif not isinstance(row["is_tradable"], bool):
                raise ValueError("is_tradable must be explicit")
            target[key] = row
    if len(statuses) != len(ids) * len(dates):
        raise ValueError("every Instrument/Session requires explicit status")
    return specs, sessions, bars, statuses
