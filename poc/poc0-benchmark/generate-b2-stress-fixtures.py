#!/usr/bin/env python3
"""Recreate the B3 registered 64x252 stress inputs used by B2."""

from __future__ import annotations

import json
from datetime import date, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "poc/poc0-benchmark/fixtures"


def weekdays(start: date, count: int) -> list[str]:
    result = []
    current = start
    while len(result) < count:
        if current.weekday() < 5:
            result.append(current.isoformat())
        current += timedelta(days=1)
    return result


def s2_dataset(base: dict) -> dict:
    sessions, instruments = 252, 64
    calendar = weekdays(date(2025, 1, 6), sessions)
    symbols = [f"ETF{index:03}" for index in range(instruments)]
    result = dict(base)
    result.update(
        dataset_version="poc0.b3.s2-scale-64x252.v1",
        source_identity="poc0-b3-deterministic-scale-generator-v1",
        calendar_basis="fixed_synthetic_weekday_sessions",
        calendar=calendar,
        seed=20260928,
        seed_semantics="deterministic arithmetic price paths, not random sampling",
        missing_bars=[{
            "symbol": symbols[1], "date": calendar[sessions // 2],
            "reason": "deterministic_scale_fixture_missing_bar",
        }],
        execution_status_overrides=[{
            "symbol": symbols[2], "date": calendar[sessions // 2 + 1],
            "trade_status": "HALTED", "is_tradable": False,
            "sources": "poc0-b3-scale-fixture", "available_at": "08:50:00+08:00",
        }],
        instruments=[{
            "symbol": symbol, "currency": "CNY", "price_precision": 2,
            "quantity_precision": 0,
            "closes": [100.0 + (((index * 17 + day * (index % 13 + 1)) % 101) - 50) * 0.1
                       for day in range(sessions)],
        } for index, symbol in enumerate(symbols)],
    )
    result["strategy"] = dict(base["strategy"], momentum_short_days=20,
                              momentum_long_days=60, volatility_window=20,
                              volatility_weight=1.0, top_n=5, rebalance_every=5)
    return result


def s2_dataset_v2(base: dict) -> dict:
    result = s2_dataset(base)
    result["dataset_version"] = "poc0.b3.s2-scale-64x252.v2"
    result["seed_semantics"] = (
        "deterministic arithmetic close paths; open[0] = close[0]; "
        "open[d] = round(close[d-1] * "
        "(1 + (((instrument_index * 7 + d * 3) % 11 - 5) * 0.001)), 2) for d > 0"
    )
    for instrument_index, instrument in enumerate(result["instruments"]):
        closes = instrument["closes"]
        instrument["opens"] = [
            closes[0]
            if day == 0
            else round(
                closes[day - 1]
                * (1 + (((instrument_index * 7 + day * 3) % 11 - 5) * 0.001)),
                2,
            )
            for day in range(len(closes))
        ]
    return result


def s2_halted_dataset(base: dict) -> dict:
    result = s2_dataset_v2(base)
    calendar, symbols = result["calendar"], [i["symbol"] for i in result["instruments"]]
    result["dataset_version"] = "poc0.b3.s2-scale-64x252-halted.v1"
    result["execution_status_overrides"] += [
        {
            "symbol": symbols[symbol_index], "date": calendar[session_index],
            "trade_status": "HALTED", "is_tradable": False,
            "sources": "poc0-mvp1-halted-scale-fixture", "available_at": "08:50:00+08:00",
        }
        for symbol_index, session_index in ((29, 131), (41, 136))
    ]
    return result


def s3_dataset(base: dict, ma: dict) -> dict:
    sessions = 252
    calendar = weekdays(date.fromisoformat(ma["start_date"]), sessions)
    flat, high = ma["flat_sessions"], ma["high_sessions"]
    low_close, high_close, flat_close = ma["low_close"], ma["high_close"], ma["flat_close"]
    symbols = ["A"] + [f"ETF{index:03}" for index in range(1, 64)]
    result = dict(base)
    result.update(
        dataset_version="poc0.b3.s3-scale-64x252.v1",
        source_identity="poc0-b3-s3-deterministic-scale-generator-v1",
        calendar_basis="fixed_synthetic_weekday_sessions",
        calendar=calendar,
        seed=20260928,
        seed_semantics="fixed MA20/60 golden price path extended with flat symbols/sessions",
        instruments=[{
            "symbol": symbol, "currency": "CNY", "price_precision": 2,
            "quantity_precision": 0,
            "closes": ([flat_close] * flat + [high_close] * high + [low_close] * (sessions - flat - high)
                       if symbol == "A" else [flat_close] * sessions),
        } for symbol in symbols],
        missing_bars=[], execution_status_overrides=[],
        s3_rising_symbol="A",
    )
    result["bar_defaults"] = dict(base["bar_defaults"], open=ma["open"])
    result["account"] = dict(base["account"], initial_cash=ma["initial_cash"], lot_size=ma["lot_size"])
    result["costs"] = dict(
        base["costs"], commission_rate=ma["commission_rate"],
        minimum_commission=ma["minimum_commission"],
        buy_slippage_bps=ma["slippage_bps"], sell_slippage_bps=ma["slippage_bps"],
        buy_tax_rate=0.0, sell_tax_rate=0.0,
    )
    return result


def main() -> None:
    base = json.loads((FIXTURES / "dataset-v1.json").read_text())
    ma = json.loads((FIXTURES / "b2-ma20-60-v1.json").read_text())
    FIXTURES.joinpath("b2-s2-scale-64x252-v1.json").write_text(
        json.dumps(s2_dataset(base), indent=2) + "\n", encoding="utf-8")
    FIXTURES.joinpath("b2-s2-scale-64x252-v2.json").write_text(
        json.dumps(s2_dataset_v2(base), indent=2) + "\n", encoding="utf-8")
    FIXTURES.joinpath("b2-s2-scale-64x252-halted-v1.json").write_text(
        json.dumps(s2_halted_dataset(base), indent=2) + "\n", encoding="utf-8")
    FIXTURES.joinpath("b2-s3-scale-64x252-v1.json").write_text(
        json.dumps(s3_dataset(base, ma), indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
