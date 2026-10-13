"""Annotate synthetic oracle inputs for the platform normalizer, without execution logic."""

import argparse
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "independent_golden", ROOT / "poc/mvp3-golden/fast_event_golden.py"
)
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)


def prepare(source):
    fixture = oracle.expand_ma(json.loads(source.read_text()))
    defaults = json.loads((ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json").read_text())
    fixture.setdefault("timezone", defaults["timezone"])
    fixture.setdefault("missing_bars", [])
    fixture.setdefault("execution_status_overrides", [])
    for key in ("volume", "amount", "high_rule", "low_rule"):
        fixture["bar_defaults"][key] = defaults["bar_defaults"][key]
    lots = {row.get("lot_size", fixture["account"].get("lot_size")) for row in fixture["instruments"]}
    if len(lots) != 1 or None in lots:
        raise ValueError("synthetic-etf-daily@3 requires one explicit common fixture lot size")
    fixture["account"]["lot_size"] = lots.pop()
    fixture["execution_status_default"]["sources"] = defaults["execution_status_default"]["sources"]
    for row in fixture["execution_status_overrides"]:
        row["sources"] = defaults["execution_status_default"]["sources"]
    for row in fixture["instruments"]:
        for key in ("currency", "price_precision", "quantity_precision"):
            row.setdefault(key, defaults["instruments"][0][key])
    return fixture


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(json.dumps(prepare(args.source), allow_nan=False) + "\n")
