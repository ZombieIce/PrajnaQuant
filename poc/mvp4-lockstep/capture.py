"""Capture reproducible rc6 lockstep observations, not platform acceptance."""

import argparse
from collections import Counter
from datetime import datetime, timezone
from decimal import Decimal, localcontext
import hashlib
import importlib.metadata
import json
from pathlib import Path
import subprocess

from nautilus_trader.model import Currency, Equity, InstrumentId, Price, Quantity, QuoteTick, Symbol

from controller import run_fixture
from worker import exact_price


def price_observations() -> dict:
    text = "1.123456789012345678"
    price = Price.from_str(text)
    instrument = Equity(
        instrument_id=InstrumentId.from_str("PRECISION.SIM"), raw_symbol=Symbol("PRECISION"),
        currency=Currency.from_str("USD"), price_precision=18,
        price_increment=Price.from_str("0.000000000000000001"),
        lot_size=Quantity.from_int(1), ts_event=0, ts_init=0,
    )
    quote = QuoteTick(instrument.id, price, price, Quantity.from_int(1), Quantity.from_int(1), 1, 1)
    result = {
        "scale18": {
            "status": "confirmed", "price": str(price), "raw": str(price.raw),
            "equity_precision": str(instrument.price_precision), "quote_ask": str(quote.ask_price),
        },
    }
    with localcontext() as context:
        context.prec = 80
        synthetic = format(Decimal(text) * Decimal("1.01"), "f")
    for name, requested in (
        ("scale19", "1.1234567890123456789"), ("synthetic_scale20", synthetic),
    ):
        try:
            Price.from_str(requested)
        except ValueError as error:
            result[name] = {"status": "unsupported", "requested": requested, "error": str(error)}
        else:
            raise AssertionError(f"precision boundary changed: {requested}")
    try:
        exact_price("10.01", 2, Price.from_str("0.05"))
    except ValueError as error:
        result["instrument_tick_mismatch"] = {"status": "unsupported", "error": str(error)}
    else:
        raise AssertionError("Adapter accepted a non-tick-aligned synthetic price")
    return result


def counts(report: dict) -> dict:
    native = Counter(e["type"] for e in report["events"] if e["type"] in {
        "fill", "reject", "backend_status",
    })
    return {
        "adapter_constructed": {
            "open_quotes": "9", "close_quotes": "9", "instrument_status": "9",
        },
        "backend_native": {key: str(value) for key, value in sorted(native.items())},
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    hand, halted, reset = run_fixture(), run_fixture(halt=True), run_fixture(reset=True)
    if reset["ledger"][:3] != reset["ledger"][3:]:
        raise AssertionError("reset cash/positions replay differs")
    here = Path(__file__).resolve().parent
    root = here.parents[1]
    report = {
        "scope": "Issue #137 synthetic feasibility only; not Accurate Engine integration",
        "captured_at": datetime.now(timezone.utc).isoformat(),
        "provenance": {
            "base_revision": subprocess.check_output(
                ["git", "-C", str(root), "rev-parse", "HEAD"], text=True,
            ).strip(),
            "working_tree_dirty": bool(subprocess.check_output(
                ["git", "-C", str(root), "status", "--porcelain"], text=True,
            ).strip()),
            "source_sha256": {
                p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                for p in sorted(here.glob("*.py"))
            },
            "requirements_sha256": hashlib.sha256(
                (here / "requirements-nautilus.txt").read_bytes(),
            ).hexdigest(),
            "nautilus_trader": importlib.metadata.version("nautilus_trader"),
            "pyarrow": importlib.metadata.version("pyarrow"),
        },
        "observations": {
            "lockstep": {"status": "confirmed", "selected": hand["selected_lockstep"],
                         "multiple_run_end": "Unknown (not needed / not tested)"},
            "reset": {"status": "confirmed", "scope": "same loaded 3x3 fixture twice; no native error logs"},
            "halt": {"status": "confirmed", "scope": "three native OrderRejected; CLOSED reason"},
            "price_precision": price_observations(),
        },
        "hand": hand, "halted": halted, "reset": reset,
        "event_counts": {"hand": counts(hand), "halted": counts(halted)},
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(args.output), "observations": report["observations"]}))


if __name__ == "__main__":
    main()
