#!/usr/bin/env python3
"""POC-0 Nautilus boundary probe; never substitutes synthetic timing values."""

from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ProcessPoolExecutor
from datetime import datetime, timedelta
from decimal import Decimal, ROUND_HALF_UP
import hashlib
import importlib.metadata
import json
import math
import multiprocessing
import os
import platform
import re
import resource
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

PINNED_NAUTILUS_VERSION = "2.0.0rc5"
_NAUTILUS_PREPARED_CACHE: dict[tuple[str, str], dict[str, Any]] = {}
_NAUTILUS_DATASET_CACHE_KEYS: dict[int, str] = {}
_NAUTILUS_RESET_ENGINES: dict[tuple[str, str], Any] = {}
ROOT = Path(__file__).resolve().parents[2]
INSTALL_EVIDENCE = ROOT / "poc/poc0-benchmark/results/nautilus-install-attempt-2026-09-27.json"
BAR_OPEN_DOC = (
    "https://nautilustrader.io/docs/latest/concepts/backtesting/bar-execution/"
)


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _version_probe() -> tuple[str | None, str | None]:
    try:
        version = importlib.metadata.version("nautilus_trader")
    except importlib.metadata.PackageNotFoundError as error:
        return None, f"{type(error).__name__}: {error}"
    if version != PINNED_NAUTILUS_VERSION:
        return version, f"installed version {version!r} does not match pinned {PINNED_NAUTILUS_VERSION!r}"
    try:
        import nautilus_trader  # noqa: F401
    except Exception as error:  # Package metadata alone does not prove a working native extension.
        return version, f"{type(error).__name__}: {error}"
    return version, None


def _rotation_signals(dataset: dict[str, Any]) -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
    parameters = dataset["strategy"]
    missing = {(item["symbol"], item["date"]) for item in dataset.get("missing_bars", [])}
    observed: dict[str, list[tuple[str, float]]] = {item["symbol"]: [] for item in dataset["instruments"]}
    signals: dict[str, dict[str, Any]] = {}
    rankings: list[dict[str, Any]] = []
    eligible_dates = 0
    short = parameters["momentum_short_days"]
    long = parameters["momentum_long_days"]
    window = parameters["volatility_window"]
    for index, date in enumerate(dataset["calendar"]):
        candidates = []
        for item in dataset["instruments"]:
            symbol = item["symbol"]
            if (symbol, date) in missing:
                continue
            close = float(item["closes"][index])
            history = observed[symbol]
            history.append((date, close))
            if len(history) <= max(short, long, window) or window <= 1:
                continue
            returns = [history[point][1] / history[point - 1][1] - 1 for point in range(len(history) - window, len(history))]
            mean = sum(returns) / window
            volatility = (sum((value - mean) ** 2 for value in returns) / (window - 1)) ** 0.5
            score = (
                parameters["short_momentum_weight"] * (close / history[-short - 1][1] - 1)
                + parameters["long_momentum_weight"] * (close / history[-long - 1][1] - 1)
                - parameters["volatility_weight"] * volatility
            )
            if math.isfinite(score):
                candidates.append((symbol, score))
        if candidates:
            candidates.sort(key=lambda item: (-item[1], item[0]))
            rankings.append({"date": date, "candidates": [
                {"symbol": symbol, "score": score} for symbol, score in candidates
            ]})
            if eligible_dates % max(1, parameters["rebalance_every"]) == 0:
                signals[date] = {
                    "date": date,
                    "target_symbols": [symbol for symbol, _ in candidates[:parameters["top_n"]]],
                }
            eligible_dates += 1
    return signals, rankings


def _ma_dataset(spec: dict[str, Any]) -> dict[str, Any]:
    day = datetime.fromisoformat(spec["start_date"]).date()
    calendar = []
    for _ in range(spec["flat_sessions"] + spec["high_sessions"] + spec["low_sessions"]):
        while day.weekday() >= 5:
            day += timedelta(days=1)
        calendar.append(day.isoformat())
        day += timedelta(days=1)
    closes = ([spec["flat_close"]] * spec["flat_sessions"]
              + [spec["high_close"]] * spec["high_sessions"]
              + [spec["low_close"]] * spec["low_sessions"])
    return {
        "dataset_version": spec["version"],
        "s3_rising_symbol": spec["rising_symbol"],
        "calendar": calendar,
        "timezone": "Asia/Shanghai",
        "bar_defaults": {"open": spec["open"]},
        "instruments": [
            {"symbol": symbol, "price_precision": 2,
             "closes": closes if symbol == spec["rising_symbol"] else [spec["flat_close"]] * len(calendar)}
            for symbol in spec["symbols"]
        ],
        "missing_bars": [],
        "execution_status_overrides": [],
        "account": {"initial_cash": spec["initial_cash"], "lot_size": spec["lot_size"]},
        "costs": {
            "minimum_commission": spec["minimum_commission"],
            "commission_rate": spec["commission_rate"],
            "buy_slippage_bps": spec["slippage_bps"],
            "sell_slippage_bps": spec["slippage_bps"],
        },
    }


def _ma_signals(dataset: dict[str, Any]) -> dict[str, dict[str, Any]]:
    signals = {}
    held = False
    previous_gap = None
    symbol = dataset["s3_rising_symbol"]
    closes = next(item["closes"] for item in dataset["instruments"] if item["symbol"] == symbol)
    for index, date in enumerate(dataset["calendar"]):
        if index < 59:
            continue
        fast = sum(closes[index - 19:index + 1]) / 20
        slow = sum(closes[index - 59:index + 1]) / 60
        gap = fast - slow
        if previous_gap is not None and not held and previous_gap <= 0 < gap:
            held = True
            signals[date] = {"date": date, "target_symbols": [symbol]}
        elif previous_gap is not None and held and previous_gap >= 0 > gap:
            held = False
            signals[date] = {"date": date, "target_symbols": []}
        previous_gap = gap
    return signals


def _run_nautilus(
    dataset: dict[str, Any],
    strategy: str = "s1",
    *,
    engine_mode: str = "cached_conversion_new_engine",
    capture_engine_state: bool = False,
) -> dict[str, Any]:
    """Run one isolated B2 scenario through public Nautilus 2.x Python APIs."""
    if engine_mode not in {"cached_conversion_new_engine", "reset"}:
        raise ValueError(f"unsupported Nautilus engine mode: {engine_mode}")
    started = time.perf_counter_ns()
    from nautilus_trader.backtest import BacktestEngine
    from nautilus_trader.config import BacktestEngineConfig, LoggerConfig
    from nautilus_trader.common import LogLevel
    from nautilus_trader.execution import FixedFeeModel
    from nautilus_trader.model import (
        AccountType,
        Currency,
        Equity,
        InstrumentId,
        Money,
        OmsType,
        OrderSide,
        Price,
        Quantity,
        QuoteTick,
        Symbol,
        Venue,
    )
    from nautilus_trader.config import StrategyConfig
    from nautilus_trader.trading import Strategy

    class AdapterConfig(StrategyConfig):
        def __init__(
            self,
            *,
            ids: list[Any],
            quantities: dict[str, int],
            signal_ts: int,
            decision_date: str,
            open_timestamps: set[int],
            open_dates: dict[int, str],
            close_dates: dict[int, str],
            blocked_statuses: dict[tuple[str, str], str],
            venue: Any,
            currency: Any,
        ) -> None:
            super().__init__()
            self.ids = ids
            self.quantities = quantities
            self.signal_ts = signal_ts
            self.decision_date = decision_date
            self.open_timestamps = open_timestamps
            self.open_dates = open_dates
            self.close_dates = close_dates
            self.blocked_statuses = blocked_statuses
            self.venue = venue
            self.currency = currency

    class BuyHoldStrategy(Strategy):
        def __init__(self, config: AdapterConfig) -> None:
            super().__init__(config)
            self.armed: set[str] = set()
            self.submitted: set[str] = set()
            self.order_events: list[dict[str, Any]] = []
            self.fill_events: list[dict[str, Any]] = []
            self.account_snapshots: dict[str, dict[str, Any]] = {}

        def on_start(self) -> None:
            for instrument_id in self.config.ids:
                self.subscribe_quotes(instrument_id)

        def on_quote(self, tick: Any) -> None:
            symbol = tick.instrument_id.symbol.value
            if tick.ts_event == self.config.signal_ts:
                self.armed.add(symbol)
            elif (
                symbol in self.armed
                and tick.ts_event in self.config.open_timestamps
                and symbol not in self.submitted
            ):
                attempt_date = self.config.open_dates[int(tick.ts_event)]
                reason = self.config.blocked_statuses.get((symbol, attempt_date))
                if reason is None:
                    self.submitted.add(symbol)
                    order = self.order_factory.market(
                        instrument_id=tick.instrument_id,
                        order_side=OrderSide.BUY,
                        quantity=Quantity.from_int(self.config.quantities[symbol]),
                    )
                    self.submit_order(order)
                self.order_events.append(
                    {
                        "decision_date": self.config.decision_date,
                        "attempt_date": attempt_date,
                        "symbol": symbol,
                        "side": "BUY",
                        "quantity": 0 if reason else self.config.quantities[symbol],
                        "reason": reason,
                        "decision_ts": self.config.signal_ts,
                        "submission_ts": None if reason else int(tick.ts_event),
                        "origin": "adapter_status_gate" if reason else "nautilus_order",
                    }
                )
            self._snapshot(tick)

        def _snapshot(self, tick: Any) -> None:
            date = self.config.close_dates.get(int(tick.ts_event))
            if date is not None and date not in self.account_snapshots:
                account = self.portfolio.account(self.config.venue)
                positions = {}
                for instrument_id in self.config.ids:
                    quantity = self.portfolio.net_position(instrument_id)
                    quantity_value = 0 if quantity is None else int(str(quantity))
                    if quantity_value:
                        positions[instrument_id.symbol.value] = quantity_value
                self.account_snapshots[date] = {
                    "cash": float(str(account.balance_total(self.config.currency)).split()[0]),
                    "positions": positions,
                }

        def on_order_filled(self, event: Any) -> None:
            self.fill_events.append(
                {
                    "symbol": event.instrument_id.symbol.value,
                    "side": event.order_side.name,
                    "quantity": int(str(event.last_qty)),
                    "fill_price": float(str(event.last_px)),
                    "commission": float(str(event.commission).split()[0]),
                    "ts_event": int(event.ts_event),
                }
            )

        def on_order_rejected(self, event: Any) -> None:
            self.order_events.append(
                {
                    "symbol": event.instrument_id.symbol.value,
                    "side": "BUY",
                    "reason": str(event.reason),
                    "ts_event": int(event.ts_event),
                    "origin": "nautilus_rejection",
                }
            )

    class RotationStrategy(BuyHoldStrategy):
        def __init__(self, config: AdapterConfig) -> None:
            super().__init__(config)
            self.pending: dict[str, Any] | None = None
            self.seen_open: dict[str, set[str]] = {}
            self.blocked_buys: set[tuple[str, str]] = set()
            self.positions: dict[str, int] = {}
            self.cash = float(dataset["account"]["initial_cash"])
            self.blocked_exits: set[str] = set()
            self.pending_budget: float | None = None

        def on_quote(self, tick: Any) -> None:
            date = self.config.close_dates.get(int(tick.ts_event))
            if date is not None:
                self._snapshot(tick)
                if date in rotation_signals:
                    self.pending = rotation_signals[date]
                    self.pending_budget = None
                return
            date = self.config.open_dates.get(int(tick.ts_event))
            if date is None or self.pending is None:
                return
            symbol_at_open = tick.instrument_id.symbol.value
            self.seen_open.setdefault(date, set()).add(symbol_at_open)
            signal = self.pending
            targets = signal["target_symbols"]
            exits = sorted(symbol for symbol in self.positions if symbol not in targets)
            if len(self.seen_open[date]) == 1 and any(
                self._blocked(symbol, date, signal["date"], "SELL") for symbol in exits
            ):
                self.blocked_exits.add(date)
            if date in self.blocked_exits:
                return
            if symbol_at_open in exits:
                self._submit(symbol_at_open, date, signal["date"], "SELL", self.positions[symbol_at_open])
            if symbol_at_open in targets and symbol_at_open not in self.positions and not any(
                symbol not in targets for symbol in self.positions
            ):
                if (date, symbol_at_open) in self.blocked_buys:
                    return
                if self._blocked(symbol_at_open, date, signal["date"], "BUY"):
                    self.blocked_buys.add((date, symbol_at_open))
                    return
                buy_ask = float((Decimal(str(dataset["bar_defaults"]["open"])) * (
                    Decimal(1) + Decimal(str(dataset["costs"]["buy_slippage_bps"])) / 10_000
                )).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP))
                lot = int(dataset["account"]["lot_size"])
                if self.pending_budget is None:
                    remaining_targets = max(1, len(targets) - len(self.positions))
                    self.pending_budget = self.cash / remaining_targets
                quantity = int(self.pending_budget / buy_ask / lot) * lot
                while quantity > 0 and quantity * buy_ask + float(dataset["costs"]["minimum_commission"]) > self.cash:
                    quantity -= lot
                if quantity > 0:
                    self._submit(symbol_at_open, date, signal["date"], "BUY", quantity)
            if all(symbol in self.positions for symbol in targets) and all(
                symbol in targets for symbol in self.positions
            ):
                self.pending = None

        def _blocked(self, symbol: str, date: str, decision: str, side: str) -> bool:
            reason = "missing_open" if symbol not in bars_by_date[date] else self.config.blocked_statuses.get((symbol, date))
            if reason is None:
                return False
            self.order_events.append({
                "decision_date": decision, "attempt_date": date, "symbol": symbol,
                "side": side, "quantity": 0, "reason": reason,
                "origin": "adapter_status_gate",
            })
            return True

        def _submit(self, symbol: str, date: str, decision: str, side: str, quantity: int) -> None:
            instrument_id = next(item for item in self.config.ids if item.symbol.value == symbol)
            order = self.order_factory.market(
                instrument_id=instrument_id,
                order_side=OrderSide.BUY if side == "BUY" else OrderSide.SELL,
                quantity=Quantity.from_int(quantity),
            )
            self.submit_order(order)
            self.order_events.append({
                "decision_date": decision, "attempt_date": date, "symbol": symbol,
                "side": side, "quantity": quantity, "reason": None,
                "origin": "nautilus_order",
            })

        def on_order_filled(self, event: Any) -> None:
            super().on_order_filled(event)
            symbol = event.instrument_id.symbol.value
            quantity = int(str(event.last_qty))
            gross = quantity * float(str(event.last_px))
            fee = float(str(event.commission).split()[0])
            if event.order_side == OrderSide.BUY:
                self.cash -= gross + fee
                self.positions[symbol] = self.positions.get(symbol, 0) + quantity
            else:
                self.cash += gross - fee
                remaining = self.positions[symbol] - quantity
                if remaining:
                    self.positions[symbol] = remaining
                else:
                    del self.positions[symbol]

    dataset_cache_key = _NAUTILUS_DATASET_CACHE_KEYS.get(id(dataset))
    if dataset_cache_key is None:
        dataset_cache_key = hashlib.sha256(json.dumps(dataset, sort_keys=True).encode()).hexdigest()
    cache_key = (dataset_cache_key, strategy)
    cached = _NAUTILUS_PREPARED_CACHE.get(cache_key)
    if cached is not None:
        currency = cached['currency']
        venue = cached['venue']
        ids = cached['ids']
        instruments = cached['instruments']
        rows_by_symbol = cached['rows_by_symbol']
        bars_by_date = cached['bars_by_date']
        rotation_signals = cached['rotation_signals']
        rotation_rankings = cached['rotation_rankings']
        missing = cached['missing']
        blocked_statuses = cached['blocked_statuses']
        signal_date = cached['signal_date']
        signal_ts = cached['signal_ts']
        open_time = cached['open_time']
        slippage_rate = cached['slippage_rate']
        sell_slippage_rate = cached['sell_slippage_rate']
        targets = cached['targets']
        open_timestamps = cached['open_timestamps']
        open_dates = cached['open_dates']
        close_dates = cached['close_dates']
        conversion_ns = 0
    else:
        conversion_started = time.perf_counter_ns()
        currency = Currency.from_str("CNY")
        venue = Venue("SIM")
        ids = []
        instruments = []
        rows_by_symbol: dict[str, list[tuple[str, str, Any]]] = {}
        precision_quantum = Decimal("0.01")
        bars_by_date = {d: {} for d in dataset["calendar"]}
        rotation_signals, rotation_rankings = {}, []
        missing = {(x["symbol"], x["date"]) for x in dataset.get("missing_bars", [])}
        blocked_statuses = {
            (x["symbol"], x["date"]): x.get("trade_status") or "missing_status"
            for x in dataset.get("execution_status_overrides", [])
            if not x["is_tradable"] or x.get("trade_status") != "TRADABLE" or not x.get("sources")
        }
        signal_date = dataset["calendar"][len(dataset["calendar"]) - 5]
        open_time = ZoneInfo(dataset.get("timezone", "Asia/Shanghai"))
        slippage_rate = Decimal(str(dataset["costs"]["buy_slippage_bps"])) / Decimal(10_000)
        sell_slippage_rate = Decimal(str(dataset["costs"].get("sell_slippage_bps", dataset["costs"]["buy_slippage_bps"]))) / Decimal(10_000)
        targets: dict[str, int] = {}
        budget = Decimal(str(dataset["account"]["initial_cash"])) / Decimal(len(dataset["instruments"]))
        lot = int(dataset["account"]["lot_size"])
        for instrument_index, item in enumerate(dataset["instruments"]):
            symbol = item["symbol"]
            instrument_id = InstrumentId.from_str(f"{symbol}.SIM")
            ids.append(instrument_id)
            instruments.append(
                Equity(
                    instrument_id=instrument_id,
                    raw_symbol=Symbol(symbol),
                    currency=currency,
                    price_precision=int(item["price_precision"]),
                    price_increment=Price.from_str("0.01"),
                    lot_size=Quantity.from_int(lot),
                    ts_event=0,
                    ts_init=0,
                )
            )
            open_price = Decimal(str(dataset["bar_defaults"]["open"]))
            slipped_open = (open_price * (Decimal(1) + slippage_rate)).quantize(
                precision_quantum, rounding=ROUND_HALF_UP
            )
            quantity = int((budget / slipped_open / lot).to_integral_value(rounding="ROUND_DOWN")) * lot
            targets[symbol] = quantity
            rows = []
            for date, close in zip(dataset["calendar"], item["closes"], strict=True):
                if (symbol, date) in missing:
                    continue
                close_price = Decimal(str(close)).quantize(precision_quantum, rounding=ROUND_HALF_UP)
                close_stamp = int(datetime.fromisoformat(f"{date}T15:00:00+08:00").timestamp() * 1_000_000_000)
                open_stamp = int(datetime.fromisoformat(f"{date}T09:30:00+08:00").timestamp() * 1_000_000_000)
                if strategy != "s1":
                    open_stamp += len(dataset["instruments"]) - instrument_index
                buy_ask = (open_price * (Decimal(1) + slippage_rate)).quantize(
                    precision_quantum, rounding=ROUND_HALF_UP
                )
                buy_bid = (open_price * (Decimal(1) - sell_slippage_rate)).quantize(
                    precision_quantum, rounding=ROUND_HALF_UP
                )
                open_tick = QuoteTick(
                    instrument_id, Price.from_str(str(buy_bid)), Price.from_str(str(buy_ask)),
                    Quantity.from_int(1_000_000), Quantity.from_int(1_000_000), open_stamp, open_stamp
                )
                close_tick = QuoteTick(
                    instrument_id, Price.from_str(str(close_price)), Price.from_str(str(close_price)),
                    Quantity.from_int(1_000_000), Quantity.from_int(1_000_000), close_stamp, close_stamp
                )
                rows.extend([(date, "open", open_tick), (date, "close", close_tick)])
                if strategy == "s2":
                    retry_stamp = open_stamp + len(dataset["instruments"])
                    rows.append((date, "open", QuoteTick(
                        instrument_id, Price.from_str(str(buy_bid)), Price.from_str(str(buy_ask)),
                        Quantity.from_int(1_000_000), Quantity.from_int(1_000_000), retry_stamp, retry_stamp
                    )))
                bars_by_date[date][symbol] = float(close_price)
            rows_by_symbol[symbol] = rows
        signal_ts = int(datetime.fromisoformat(f"{signal_date}T15:00:00+08:00").timestamp() * 1_000_000_000)
        open_timestamps = {
            int(datetime.fromisoformat(f"{date}T09:30:00+08:00").timestamp() * 1_000_000_000) + offset
            for date in dataset["calendar"]
            if strategy != "s1" or date > signal_date
            for offset in (range(1, 2 * len(ids) + 1) if strategy == "s2" else
                           range(1, len(ids) + 1) if strategy == "s3" else (0,))
        }
        open_dates = {
            int(datetime.fromisoformat(f"{date}T09:30:00+08:00").timestamp() * 1_000_000_000) + offset: date
            for date in dataset["calendar"]
            if strategy != "s1" or date > signal_date
            for offset in (range(1, 2 * len(ids) + 1) if strategy == "s2" else
                           range(1, len(ids) + 1) if strategy == "s3" else (0,))
        }
        close_dates = {
            int(datetime.fromisoformat(f"{date}T15:00:00+08:00").timestamp() * 1_000_000_000): date
            for date in dataset["calendar"]
        }
        conversion_ns = time.perf_counter_ns() - conversion_started
        _NAUTILUS_PREPARED_CACHE[cache_key] = {
            'currency': currency,
            'venue': venue,
            'ids': ids,
            'instruments': instruments,
            'rows_by_symbol': rows_by_symbol,
            'bars_by_date': bars_by_date,
            'rotation_signals': rotation_signals,
            'rotation_rankings': rotation_rankings,
            'missing': missing,
            'blocked_statuses': blocked_statuses,
            'signal_date': signal_date,
            'signal_ts': signal_ts,
            'open_time': open_time,
            'slippage_rate': slippage_rate,
            'sell_slippage_rate': sell_slippage_rate,
            'targets': targets,
            'open_timestamps': open_timestamps,
            'open_dates': open_dates,
            'close_dates': close_dates,
        }

    factor_started = time.perf_counter_ns()
    rotation_signals, rotation_rankings = _rotation_signals(dataset) if strategy == "s2" else (
        _ma_signals(dataset) if strategy == "s3" else {}, []
    )
    factor_ns = time.perf_counter_ns() - factor_started

    init_started = time.perf_counter_ns()
    dataset_digest = _NAUTILUS_DATASET_CACHE_KEYS.get(id(dataset))
    if dataset_digest is None:
        dataset_digest = hashlib.sha256(json.dumps(dataset, sort_keys=True).encode()).hexdigest()
    reset_engine_key = (dataset_digest, strategy)
    engine = _NAUTILUS_RESET_ENGINES.get(reset_engine_key) if engine_mode == "reset" else None
    reused_engine = engine is not None
    if engine is None:
        engine = BacktestEngine(
            BacktestEngineConfig(logging=LoggerConfig(stdout_level=LogLevel.ERROR))
        )
        engine.add_venue(
            venue=venue,
            oms_type=OmsType.NETTING,
            account_type=AccountType.CASH,
            starting_balances=[Money.from_str(f"{dataset['account']['initial_cash']:.2f} CNY")],
            base_currency=currency,
            fee_model=FixedFeeModel(Money.from_str(f"{dataset['costs']['minimum_commission']:.2f} CNY")),
        )
        for instrument, item in zip(instruments, dataset["instruments"], strict=True):
            engine.add_instrument(instrument)
            engine.add_data([row[2] for row in rows_by_symbol[item["symbol"]]])
        if engine_mode == "reset":
            _NAUTILUS_RESET_ENGINES[reset_engine_key] = engine
    else:
        # BacktestEngine.reset() preserves venue, instruments and data. Clear the old
        # Strategy explicitly before registering a fresh instance for this Run.
        engine.reset()
        engine.clear_strategies()
    strategy_instance = (RotationStrategy if strategy != "s1" else BuyHoldStrategy)(
        AdapterConfig(
            ids=ids,
            quantities=targets,
            signal_ts=signal_ts,
            decision_date=signal_date,
            open_timestamps=open_timestamps,
            open_dates=open_dates,
            close_dates=close_dates,
            blocked_statuses=blocked_statuses,
            venue=venue,
            currency=currency,
        )
    )
    engine.add_strategy(strategy_instance)
    init_ns = time.perf_counter_ns() - init_started

    run_started = time.perf_counter_ns()
    engine.run()
    event_ns = time.perf_counter_ns() - run_started

    fills = sorted(strategy_instance.fill_events, key=lambda x: (x["ts_event"], x["symbol"]))
    cash = float(dataset["account"]["initial_cash"])
    holdings: dict[str, int] = {}
    fill_by_date: dict[str, list[dict[str, Any]]] = {}
    for fill in fills:
        date = datetime.fromtimestamp(fill["ts_event"] / 1_000_000_000, tz=ZoneInfo("UTC")).astimezone(open_time).date().isoformat()
        fill["date"] = date
        gross = fill["quantity"] * fill["fill_price"]
        fill["gross_value"] = gross
        fill_by_date.setdefault(date, []).append(fill)
    ledger = []
    marks: dict[str, float] = {}
    for date in dataset["calendar"]:
        for fill in fill_by_date.get(date, []):
            if fill["side"] == "BUY":
                cash -= fill["gross_value"] + fill["commission"]
                holdings[fill["symbol"]] = holdings.get(fill["symbol"], 0) + fill["quantity"]
            else:
                cash += fill["gross_value"] - fill["commission"]
                holdings[fill["symbol"]] -= fill["quantity"]
                if holdings[fill["symbol"]] == 0:
                    del holdings[fill["symbol"]]
        marks.update(bars_by_date[date])
        marked_holdings = [
            {"symbol": symbol, "quantity": quantity, "mark_price": marks[symbol]}
            for symbol, quantity in sorted(holdings.items())
        ]
        nav = cash + sum(h["quantity"] * h["mark_price"] for h in marked_holdings)
        ledger.append({"date": date, "cash": cash, "holdings": marked_holdings, "nav": nav})
    engine_state = None
    if capture_engine_state:
        account = engine.cache.account_for_venue(venue)
        engine_state = {
            "account_balances": {
                currency_code.code: str(balance.total)
                for currency_code, balance in sorted(account.balances().items(), key=lambda item: item[0].code)
            },
            "order_count": engine.cache.orders_total_count(),
            "open_order_count": engine.cache.orders_open_count(),
            "position_count": engine.cache.positions_total_count(),
            "open_position_count": engine.cache.positions_open_count(),
            "iteration_count": engine.iteration,
            "backtest_start": engine.backtest_start,
            "backtest_end": engine.backtest_end,
            "strategy_ids": sorted(str(value) for value in engine.cache.strategy_ids()),
            "instrument_ids": sorted(str(value.id) for value in engine.cache.instruments()),
            "quote_counts_by_instrument": {
                str(instrument_id): engine.cache.quote_count(instrument_id)
                for instrument_id in sorted(engine.cache.instrument_ids(), key=str)
            },
        }
    if engine_mode != "reset":
        engine.dispose()
    return {
        "engine_mode": engine_mode,
        "engine_reused": reused_engine,
        "projection": {
            "orders": sorted(strategy_instance.order_events, key=lambda x: (x.get("attempt_date", ""), x.get("side") != "SELL", x["symbol"])),
            "fills": fills,
            "ledger": ledger,
            "nautilus_account_snapshots": strategy_instance.account_snapshots,
            **({"signals": list(rotation_signals.values())} if strategy != "s1" else {}),
            **({"rankings": rotation_rankings} if strategy == "s2" else {}),
            "targets": targets,
            "summary": {
                "initial_cash": float(dataset["account"]["initial_cash"]),
                "final_equity": ledger[-1]["nav"],
                "final_positions": holdings,
                "commission": sum(x["commission"] for x in fills),
                "tax": 0.0,
                "slippage_cost": sum(
                    abs(x["fill_price"] - float(dataset["bar_defaults"]["open"])) * x["quantity"]
                    for x in fills
                ),
                "total_cost": sum(x["commission"] for x in fills)
                + sum(abs(x["fill_price"] - float(dataset["bar_defaults"]["open"])) * x["quantity"] for x in fills),
            },
        },
        "timings_ns": {
            "conversion": conversion_ns,
            "factor_signal": factor_ns,
            "initialization": init_ns,
            "event_processing": event_ns,
            "end_to_end": time.perf_counter_ns() - started,
        },
        **({"engine_state": engine_state} if capture_engine_state else {}),
    }


def _parallel_worker(dataset: dict[str, Any], strategy: str) -> dict[str, Any]:
    started = time.perf_counter_ns()
    result = _run_nautilus(dataset, strategy=strategy)
    result["timings_ns"]["worker_run"] = time.perf_counter_ns() - started
    result["peak_worker_rss_bytes"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (
        1024 if sys.platform.startswith("linux") else 1
    )
    return result


_B2_WORKER_DATASET: dict[str, Any] | None = None
_B2_WORKER_STRATEGY: str | None = None
_B2_WORKER_EXPECTED_CHECKSUM: str | None = None
_B2_WORKER_WARMUP_CHECKSUM: str | None = None
_B2_WORKER_ENGINE_MODE = "cached_conversion_new_engine"
_B2_WORKER_READY_BARRIER: Any = None
_B2_WORKER_MEASUREMENT_BARRIER: Any = None


def _projection_checksum(projection: dict[str, Any]) -> str:
    serialized = json.dumps(projection, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(serialized).hexdigest()


def _first_projection_difference(expected: Any, actual: Any, path: str = "projection") -> str | None:
    if type(expected) is not type(actual):
        return path
    if isinstance(expected, dict):
        if expected.keys() != actual.keys():
            keys = sorted(set(expected) | set(actual))
            return next(f"{path}.{key}" for key in keys if key not in expected or key not in actual)
        for key in sorted(expected):
            difference = _first_projection_difference(expected[key], actual[key], f"{path}.{key}")
            if difference is not None:
                return difference
        return None
    if isinstance(expected, list):
        if len(expected) != len(actual):
            return f"{path}.length"
        for index, (left, right) in enumerate(zip(expected, actual, strict=True)):
            difference = _first_projection_difference(left, right, f"{path}[{index}]")
            if difference is not None:
                return difference
        return None
    return None if expected == actual else path


def _run_with_native_log_capture(*args: Any, **kwargs: Any) -> tuple[dict[str, Any], list[str]]:
    """Capture Rust/C logger output so a matching projection cannot hide engine errors."""
    sys.stdout.flush()
    sys.stderr.flush()
    with tempfile.TemporaryFile() as captured:
        saved_stdout, saved_stderr = os.dup(1), os.dup(2)
        try:
            os.dup2(captured.fileno(), 1)
            os.dup2(captured.fileno(), 2)
            result = _run_nautilus(*args, **kwargs)
            sys.stdout.flush()
            sys.stderr.flush()
        finally:
            os.dup2(saved_stdout, 1)
            os.dup2(saved_stderr, 2)
            os.close(saved_stdout)
            os.close(saved_stderr)
        captured.seek(0)
        log_lines = captured.read().decode("utf-8", errors="replace").splitlines()
    return result, [
        re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", line)
        for line in log_lines
        if "[ERROR]" in line or "\tERROR\t" in line
    ]


def verify_reset_parity(
    workloads: list[dict[str, Any]],
    *,
    minimum_reset_runs: int = 3,
) -> dict[str, Any]:
    """Compare repeated reset Runs with fresh-engine projections on all four B2 loads.

    Each workload has ``name``, ``strategy``, ``dataset`` and, for decision loads,
    ``golden_checksum_sha256``. The first reset-mode call primes venue/instrument/data;
    the following ``minimum_reset_runs`` calls each reset that same engine first.
    """
    version, version_error = _version_probe()
    evidence: dict[str, Any] = {
        "schema_version": "poc0.b2-nautilus-reset-parity.v1",
        "nautilus_version": version,
        "revision": None,
        "reset_parity": "unresolved",
        "selected_mode": None,
        "loads": [],
    }
    try:
        evidence["revision"] = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True,
            capture_output=True, check=True,
        ).stdout.strip()
        dirty = subprocess.run(
            ["git", "status", "--porcelain"], cwd=ROOT, text=True,
            capture_output=True, check=True,
        ).stdout.splitlines()
        evidence["dirty_worktree"] = bool(dirty)
        evidence["dirty_paths"] = [line[3:] for line in dirty]
    except (OSError, subprocess.CalledProcessError) as error:
        evidence["revision_error"] = f"{type(error).__name__}: {error}"
    if version_error:
        evidence["reason"] = f"pinned Nautilus runtime unavailable: {version_error}"
        return evidence
    if minimum_reset_runs < 3:
        raise ValueError("ticket 07 requires at least three reset Runs per workload")
    if len(workloads) != 4 or len({item.get("name") for item in workloads}) != 4:
        evidence["reason"] = "reset parity requires four uniquely named decision and robustness workloads"
        return evidence

    parity_failures = []
    golden_failures = []
    for workload in workloads:
        name = workload["name"]
        strategy = workload["strategy"]
        dataset = workload["dataset"]
        dataset_digest = hashlib.sha256(json.dumps(dataset, sort_keys=True).encode()).hexdigest()
        _NAUTILUS_DATASET_CACHE_KEYS[id(dataset)] = dataset_digest
        reset_key = (dataset_digest, strategy)
        previous_engine = _NAUTILUS_RESET_ENGINES.pop(reset_key, None)
        if previous_engine is not None:
            previous_engine.dispose()

        fresh = _run_nautilus(
            dataset, strategy=strategy, engine_mode="cached_conversion_new_engine", capture_engine_state=True
        )
        fresh_checksum = _projection_checksum(fresh["projection"])
        golden_checksum = workload.get("golden_checksum_sha256")
        golden_required = workload.get("golden_required", name in {"s2_decision", "s3_decision"})
        golden_match = None if golden_checksum is None else fresh_checksum == golden_checksum
        first_difference = None
        if golden_required and golden_checksum is None:
            first_difference = "golden_checksum_sha256 (required but missing)"
        elif golden_match is False:
            first_difference = "projection (fresh Nautilus checksum differs from independent golden)"

        # The first reset-mode call loads the reusable engine; all subsequent calls
        # exercise reset(), clear_strategies(), and a newly constructed Strategy.
        prime = _run_nautilus(dataset, strategy=strategy, engine_mode="reset", capture_engine_state=True)
        reset_results = [
            _run_with_native_log_capture(
                dataset,
                strategy=strategy,
                engine_mode="reset",
                capture_engine_state=True,
            )
            for _ in range(minimum_reset_runs)
        ]
        reset_runs = [item[0] for item in reset_results]
        reset_error_logs = [line for _, lines in reset_results for line in lines]
        reset_checksums = [_projection_checksum(item["projection"]) for item in reset_runs]
        reset_differences = [
            _first_projection_difference(fresh["projection"], item["projection"])
            for item in reset_runs
        ]
        state_differences = [
            _first_projection_difference(fresh["engine_state"], item["engine_state"], "engine_state")
            for item in reset_runs
        ]
        parity_ok = (
            all(value == fresh_checksum for value in reset_checksums)
            and all(difference is None for difference in state_differences)
            and not reset_error_logs
        )
        if not parity_ok and first_difference is None:
            first_difference = next(
                (
                    value for value in reset_differences + state_differences if value is not None
                ),
                "engine_log.error" if reset_error_logs else None,
            )
        if not parity_ok:
            parity_failures.append(name)
        if golden_required and golden_match is not True:
            golden_failures.append(name)
        evidence["loads"].append({
            "name": name,
            "strategy": strategy,
            "dataset_sha256": dataset_digest,
            "fresh_engine_checksum_sha256": fresh_checksum,
            "golden_checksum_sha256": golden_checksum,
            "golden_match": golden_match,
            "reset_checksums_sha256": reset_checksums,
            "fresh_engine_state": fresh["engine_state"],
            "reset_engine_states": [item["engine_state"] for item in reset_runs],
            "reset_runs": len(reset_runs),
            "reset_runs_engine_reused": [item["engine_reused"] for item in reset_runs],
            "reset_error_logs": reset_error_logs,
            "parity": "failed" if not parity_ok else "unresolved" if golden_required and golden_match is not True else "passed",
            "first_difference_field": first_difference,
            "prime_checksum_sha256": _projection_checksum(prime["projection"]),
        })
        cached_engine = _NAUTILUS_RESET_ENGINES.pop(reset_key, None)
        if cached_engine is not None:
            cached_engine.dispose()

    if parity_failures:
        evidence["reset_parity"] = "failed"
        evidence["selected_mode"] = "cached_conversion_new_engine"
        evidence["reason"] = "one or more reset Runs differed from a fresh engine or emitted native engine error logs"
        evidence["failed_loads"] = parity_failures
        if golden_failures:
            evidence["golden_unresolved_loads"] = golden_failures
    elif golden_failures:
        evidence["reset_parity"] = "unresolved"
        evidence["reason"] = "reset projections matched fresh engines, but a required decision-load golden was missing or mismatched"
        evidence["golden_unresolved_loads"] = golden_failures
    else:
        evidence["reset_parity"] = "passed"
        evidence["selected_mode"] = "reset"
        evidence["reason"] = "all four loads matched fresh-engine projections; decision loads also matched independent goldens"
    return evidence


def _initialize_b2_digest_worker(
    dataset: dict[str, Any], strategy: str, expected_checksum: str,
    ready_barrier: Any = None, measurement_barrier: Any = None,
    engine_mode: str = "cached_conversion_new_engine",
) -> None:
    global _B2_WORKER_DATASET, _B2_WORKER_STRATEGY, _B2_WORKER_EXPECTED_CHECKSUM
    global _B2_WORKER_WARMUP_CHECKSUM, _B2_WORKER_READY_BARRIER, _B2_WORKER_MEASUREMENT_BARRIER
    global _B2_WORKER_ENGINE_MODE
    if engine_mode not in {"reset", "cached_conversion_new_engine"}:
        raise ValueError(f"unsupported Nautilus engine mode: {engine_mode}")
    _NAUTILUS_DATASET_CACHE_KEYS[id(dataset)] = hashlib.sha256(
        json.dumps(dataset, sort_keys=True).encode()
    ).hexdigest()
    _B2_WORKER_DATASET = dataset
    _B2_WORKER_STRATEGY = strategy
    _B2_WORKER_EXPECTED_CHECKSUM = expected_checksum
    _B2_WORKER_READY_BARRIER = ready_barrier
    _B2_WORKER_MEASUREMENT_BARRIER = measurement_barrier
    _B2_WORKER_ENGINE_MODE = engine_mode
    for _ in range(2):
        warmup = _run_nautilus(dataset, strategy=strategy, engine_mode=engine_mode)
        checksum = _projection_checksum(warmup["projection"])
        if checksum != expected_checksum:
            raise RuntimeError("Nautilus warmup differs from the independently checked S2 projection")
        if _B2_WORKER_WARMUP_CHECKSUM not in (None, checksum):
            raise RuntimeError("Nautilus warmup projection changed within worker")
        _B2_WORKER_WARMUP_CHECKSUM = checksum
    if ready_barrier is not None:
        ready_barrier.wait(timeout=60)


def _parallel_worker_digest(run_index: int) -> dict[str, Any]:
    """Return only the run identity and measurements across the worker boundary."""
    if (
        _B2_WORKER_DATASET is None
        or _B2_WORKER_STRATEGY is None
        or _B2_WORKER_EXPECTED_CHECKSUM is None
        or _B2_WORKER_WARMUP_CHECKSUM is None
    ):
        raise RuntimeError("B2 worker was not initialized")
    result = _run_nautilus(
        _B2_WORKER_DATASET,
        strategy=_B2_WORKER_STRATEGY,
        engine_mode=_B2_WORKER_ENGINE_MODE,
    )
    checksum = _projection_checksum(result["projection"])
    return {
        "run_index": run_index,
        "run_ns": result["timings_ns"]["end_to_end"],
        "checksum_sha256": checksum,
        "worker_pid": os.getpid(),
        "peak_worker_rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (
            1024 if sys.platform.startswith("linux") else 1
        ),
    }


def _parallel_worker_digest_batch(run_indices: list[int]) -> dict[str, Any]:
    """Run a worker's assigned samples, then checksum projections after the timed boundary."""
    if _B2_WORKER_DATASET is None or _B2_WORKER_STRATEGY is None:
        raise RuntimeError("B2 worker was not initialized")
    if _B2_WORKER_MEASUREMENT_BARRIER is not None:
        _B2_WORKER_MEASUREMENT_BARRIER.wait(timeout=60)
    measured = []
    for run_index in run_indices:
        result = _run_nautilus(
            _B2_WORKER_DATASET,
            strategy=_B2_WORKER_STRATEGY,
            engine_mode=_B2_WORKER_ENGINE_MODE,
        )
        measured.append((run_index, result["timings_ns"]["end_to_end"], result["projection"]))
    completed_ns = time.perf_counter_ns()
    # Wait for every worker's measured batch before any projection hashing can contend
    # with another worker's engine runs.
    if _B2_WORKER_MEASUREMENT_BARRIER is not None:
        _B2_WORKER_MEASUREMENT_BARRIER.wait(timeout=60)
    checksummed = [
        {"run_index": run_index, "run_ns": run_ns, "checksum_sha256": _projection_checksum(projection)}
        for run_index, run_ns, projection in measured
    ]
    return {
        "worker_pid": os.getpid(),
        "completed_ns": completed_ns,
        "runs": checksummed,
        "peak_worker_rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (
            1024 if sys.platform.startswith("linux") else 1
        ),
    }


def parallel_runs(dataset: dict[str, Any], strategy: str, *, workers: int = 2, runs: int = 6) -> dict[str, Any]:
    if workers <= 0 or runs < workers:
        raise ValueError("runs must be >= positive workers")
    with ProcessPoolExecutor(max_workers=workers) as pool:
        warmups = [pool.submit(_parallel_worker, dataset, strategy) for _ in range(workers)]
        expected = [json.dumps(warmup.result()["projection"], sort_keys=True) for warmup in warmups]
        started = time.perf_counter_ns()
        futures = [pool.submit(_parallel_worker, dataset, strategy) for _ in range(runs)]
        measured = [future.result() for future in futures]
        elapsed = time.perf_counter_ns() - started
    projections = [json.dumps(item["projection"], sort_keys=True) for item in measured]
    if len(set(expected + projections)) != 1:
        return {"status": "unresolved", "reason": "parallel projection mismatch"}
    samples = [item["timings_ns"]["worker_run"] for item in measured]
    return {
        "status": "passed",
        "workers": workers,
        "warmup_runs": workers,
        "runs": runs,
        "raw_samples_ns": samples,
        "median_ns": statistics.median(samples),
        "p95_ns": sorted(samples)[math.ceil(0.95 * len(samples)) - 1],
        "parallel_wall_ns": elapsed,
        "parallel_runs_per_second": runs * 1_000_000_000 / elapsed,
        "peak_worker_rss_bytes": max(item["peak_worker_rss_bytes"] for item in measured),
        "run_checksum_sha256": hashlib.sha256(projections[0].encode()).hexdigest(),
        "measurement_scope": "warm Python process pool; independent conversion, initialization and Nautilus replay per Run; excludes pool startup",
    }


def parallel_digest_runs(
    dataset: dict[str, Any],
    strategy: str,
    expected_checksum: str,
    *,
    workers: int = 2,
    runs: int = 6,
    engine_mode: str = "cached_conversion_new_engine",
) -> dict[str, Any]:
    """Run correctness-gated workers while keeping projection payloads out of IPC."""
    if workers <= 0 or runs < workers:
        raise ValueError("runs must be >= positive workers")
    process_context = multiprocessing.get_context()
    ready_barrier = process_context.Barrier(workers)
    measurement_barrier = process_context.Barrier(workers)
    with ProcessPoolExecutor(
        max_workers=workers,
        mp_context=process_context,
        initializer=_initialize_b2_digest_worker,
        initargs=(dataset, strategy, expected_checksum, ready_barrier, measurement_barrier, engine_mode),
    ) as pool:
        ready_futures = [pool.submit(_parallel_worker_ready) for _ in range(workers)]
        ready_pids = {probe.result() for probe in ready_futures}
        if len(ready_pids) != workers:
            raise RuntimeError("worker startup did not exercise every Nautilus worker")
        started = time.perf_counter_ns()
        assignments = [list(range(worker, runs, workers)) for worker in range(workers)]
        batches = [pool.submit(_parallel_worker_digest_batch, indices) for indices in assignments]
        measured_batches = [future.result() for future in batches]
        elapsed = max(item["completed_ns"] for item in measured_batches) - started
        secondary_futures = [
            pool.submit(_cold_end_to_end_sample, dataset, strategy, index)
            for index in range(runs)
        ]
        secondary_samples = [future.result() for future in secondary_futures]
    measured_pids = {batch["worker_pid"] for batch in measured_batches}
    if measured_pids != ready_pids:
        raise RuntimeError("each initialized worker must contribute a measured Run")
    measured = [run for batch in measured_batches for run in batch["runs"]]
    checksums = {item["checksum_sha256"] for item in measured}
    if checksums != {expected_checksum}:
        return {
            "status": "unresolved",
            "reason": "parallel projection checksum did not match independently checked Nautilus S2 golden",
            "mismatched_checksums_sha256": sorted(checksums - {expected_checksum}),
        }
    if {sample["checksum_sha256"] for sample in secondary_samples} != {expected_checksum}:
        return {
            "status": "unresolved",
            "reason": "secondary-boundary projection checksum mismatch",
        }
    per_worker: dict[int, int] = {}
    for batch in measured_batches:
        per_worker[batch["worker_pid"]] = batch["peak_worker_rss_bytes"]
    samples = [item["run_ns"] for item in measured]
    return {
        "status": "passed",
        "workers": workers,
        "runs": runs,
        "warmup_runs": 2 * workers,
        "warmup_runs_per_worker": 2,
        "raw_samples_ns": samples,
        "secondary_boundary_samples_ns": secondary_samples,
        "secondary_boundary_median_ns": statistics.median(item["end_to_end_ns"] for item in secondary_samples),
        "secondary_boundary_p95_ns": sorted(item["end_to_end_ns"] for item in secondary_samples)[math.ceil(0.95 * len(secondary_samples)) - 1],
        "checksums_sha256": [item["checksum_sha256"] for item in measured],
        "median_ns": statistics.median(samples),
        "p95_ns": sorted(samples)[math.ceil(0.95 * len(samples)) - 1],
        "parallel_wall_ns": elapsed,
        "parallel_runs_per_second": runs * 1_000_000_000 / elapsed,
        "peak_rss_by_worker_bytes": per_worker,
        "peak_rss_sum_upper_bound_bytes": sum(per_worker.values()),
        "measurement_scope": "warm Python process pool; worker completion timestamps precede projection checksumming and IPC result collection",
        "engine_mode": engine_mode,
        "engine_mode_reason": "selected by ticket 07 reset parity evidence" if engine_mode == "reset" else "fallback selected by ticket 07 reset parity evidence or used without evidence",
        "conversion_cached_per_worker": True,
    }


def serial_digest_runs(
    dataset: dict[str, Any],
    strategy: str,
    expected_checksum: str,
    *,
    runs: int = 20,
    engine_mode: str = "cached_conversion_new_engine",
) -> dict[str, Any]:
    """Measure serial Runs after worker-local conversion and two correctness warmups."""
    if runs <= 0:
        raise ValueError("runs must be positive")
    _initialize_b2_digest_worker(dataset, strategy, expected_checksum, engine_mode=engine_mode)
    measured = [_parallel_worker_digest(index) for index in range(runs)]
    checksums = {item["checksum_sha256"] for item in measured}
    if checksums != {expected_checksum}:
        return {
            "status": "unresolved",
            "reason": "serial projection checksum did not match independently checked Nautilus S2 golden",
            "mismatched_checksums_sha256": sorted(checksums - {expected_checksum}),
        }
    samples = [item["run_ns"] for item in measured]
    ordered = sorted(samples)
    secondary_samples = [_cold_end_to_end_sample(dataset, strategy, index) for index in range(runs)]
    if {sample["checksum_sha256"] for sample in secondary_samples} != {expected_checksum}:
        return {
            "status": "unresolved",
            "reason": "secondary-boundary projection checksum mismatch",
        }
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (1024 if sys.platform.startswith("linux") else 1)
    return {
        "status": "passed",
        "workers": 1,
        "warmup_runs": 2,
        "warmup_runs_per_worker": 2,
        "runs": runs,
        "raw_samples_ns": samples,
        "secondary_boundary_samples_ns": secondary_samples,
        "secondary_boundary_median_ns": statistics.median(item["end_to_end_ns"] for item in secondary_samples),
        "secondary_boundary_p95_ns": sorted(item["end_to_end_ns"] for item in secondary_samples)[math.ceil(0.95 * len(secondary_samples)) - 1],
        "checksums_sha256": [item["checksum_sha256"] for item in measured],
        "median_ns": statistics.median(samples),
        "p95_ns": ordered[math.ceil(0.95 * len(ordered)) - 1],
        "peak_rss_bytes": peak,
        "engine_mode": engine_mode,
        "engine_mode_reason": "selected by ticket 07 reset parity evidence" if engine_mode == "reset" else "fallback selected by ticket 07 reset parity evidence or used without evidence",
        "conversion_cached_per_worker": True,
        "measurement_scope": "single worker; conversion and two warmups excluded; factor/signal, new engine initialization, replay and projection included",
    }


def _cold_end_to_end_sample(
    dataset: dict[str, Any], strategy: str, run_index: int
) -> dict[str, int]:
    """Measure a fresh canonical-bars conversion, engine setup and one complete Run."""
    _NAUTILUS_PREPARED_CACHE.clear()
    result = _run_nautilus(dataset, strategy=strategy)
    return {
        "run_index": run_index,
        "end_to_end_ns": result["timings_ns"]["end_to_end"],
        "checksum_sha256": _projection_checksum(result["projection"]),
    }


def _parallel_worker_ready() -> int:
    if _B2_WORKER_WARMUP_CHECKSUM is None or _B2_WORKER_READY_BARRIER is None:
        raise RuntimeError("B2 worker warmups did not finish")
    _B2_WORKER_READY_BARRIER.wait(timeout=60)
    return os.getpid()


def build_report(dataset_path: Path, reference_path: Path) -> dict[str, Any]:
    dataset = json.loads(dataset_path.read_text(encoding="utf-8"))
    reference = json.loads(reference_path.read_text(encoding="utf-8"))
    version, probe_error = _version_probe()
    install_evidence = None
    if INSTALL_EVIDENCE.exists():
        install_evidence = json.loads(INSTALL_EVIDENCE.read_text(encoding="utf-8"))

    rust_b2 = reference.get("b2_fast_event_buy_hold", {})
    adapter_result = None
    measured_runs: list[dict[str, Any]] = []
    adapter_error = None
    if probe_error is None:
        try:
            _run_nautilus(dataset)  # Warm the pinned runtime; do not mix this run into samples.
            measured_runs = [_run_nautilus(dataset) for _ in range(5)]
            adapter_result = measured_runs[0]
        except Exception as error:
            adapter_error = f"{type(error).__name__}: {error}"

    unresolved_reasons = []
    if probe_error:
        unresolved_reasons.append(
            {
                "code": "pinned_runtime_unavailable",
                "detail": probe_error,
                "replay_condition": "Install the exact pinned wheel and rerun this command.",
            }
        )
    elif adapter_error:
        unresolved_reasons.append(
            {
                "code": "adapter_runtime_error",
                "detail": adapter_error,
                "replay_condition": "Keep the pinned wheel and rerun after correcting the API/runtime error.",
            }
        )

    projection_bytes = [json.dumps(run["projection"], sort_keys=True).encode() for run in measured_runs]
    repeated_projections_equal = len(set(projection_bytes)) == 1 if measured_runs else None
    if repeated_projections_equal is False:
        unresolved_reasons.append({
            "code": "repeated_projection_mismatch",
            "detail": "The five measured Nautilus runs produced different project projections.",
            "replay_condition": "Investigate nondeterminism before comparing timing samples.",
        })

    adapter_builds = (install_evidence or {}).get("adapter_builds", {})
    dev_build = adapter_builds.get("dev", {})
    release_build = adapter_builds.get("release", {})
    requirements = ROOT / "poc/poc0-benchmark/requirements-nautilus.txt"
    pip_freeze = subprocess.run(
        [sys.executable, "-m", "pip", "freeze"], text=True, capture_output=True, check=False
    )
    resolved_packages = sorted(pip_freeze.stdout.splitlines()) if pip_freeze.returncode == 0 else None
    preflight_path = os.environ.get("NAUTILUS_POC_PREFLIGHT")
    preflight = json.loads(Path(preflight_path).read_text(encoding="utf-8")) if preflight_path else None

    def timing_summary(stage: str) -> dict[str, Any] | None:
        if not measured_runs:
            return None
        samples = [run["timings_ns"][stage] for run in measured_runs]
        ordered = sorted(samples)
        return {
            "raw_samples_ns": samples,
            "median_ns": statistics.median(samples),
            "p95_ns": ordered[math.ceil(0.95 * len(ordered)) - 1],
            "min_ns": ordered[0],
            "max_ns": ordered[-1],
        }

    native_orders = None if adapter_result is None else [
        order for order in adapter_result["projection"]["orders"]
        if order.get("origin") == "nautilus_order"
    ]
    report = {
        "schema_version": "poc0.nautilus-adapter.v1",
        "status": "unresolved" if adapter_result is None else "executed_with_semantic_differences",
        "backend": "nautilus",
        "strategy": "S1 Buy & Hold",
        "dataset": {
            "version": dataset.get("dataset_version"),
            "content_sha256": _sha256(dataset_path),
            "instrument_count": len(dataset.get("instruments", [])),
            "calendar_sessions": len(dataset.get("calendar", [])),
        },
        "versions": {
            "nautilus_required": PINNED_NAUTILUS_VERSION,
            "nautilus_observed": version,
            "python": platform.python_version(),
            "platform": platform.platform(),
        },
        "project_contract": {
            "reference_engine": "rust-b2-fast-event-buy-hold",
            "reference_checksum_sha256": rust_b2.get("checksum_sha256"),
            "reference_projection": rust_b2.get("projection"),
            "adapter_boundary": (
                "Instrument, Order, Fill, account, costs, holdings and NAV are serialized "
                "using the project report contract. Nautilus types remain inside this "
                "Python process boundary."
            ),
        },
        "semantic_comparison": {
            "status": "not_run" if adapter_result is None else "compared",
            "checks": [] if adapter_result is None else _compare(adapter_result["projection"], rust_b2.get("projection", {})),
            "differences": unresolved_reasons,
            "orders": None if adapter_result is None else adapter_result["projection"]["orders"],
            "nautilus_native_submissions": native_orders,
            "fills": None if adapter_result is None else adapter_result["projection"]["fills"],
            "cash": None if adapter_result is None else [x["cash"] for x in adapter_result["projection"]["ledger"]],
            "holdings": None if adapter_result is None else [x["holdings"] for x in adapter_result["projection"]["ledger"]],
            "costs": None if adapter_result is None else adapter_result["projection"]["summary"],
            "daily_nav": None if adapter_result is None else [x["nav"] for x in adapter_result["projection"]["ledger"]],
            "adapter_projection": None if adapter_result is None else adapter_result["projection"],
            "known_semantic_differences": [] if adapter_result is None else [
                "The adapter applies the project's 08:50 execution status gate at the open: HALTED creates a project rejection without submitting a Nautilus order. Nautilus has three native submissions while Rust has four project attempts; native HALTED rejection was not exercised or verified.",
                "Daily bars are converted to synthetic open and close L1 QuoteTicks; the strategy arms at close and submits at the next available open quote because Nautilus documents no native next-bar-open mode for bar-only data.",
                "The fixture's 10 bp buy slippage is represented by a synthetic bid/ask around the open; Rust stores it as fill price plus explicit slippage cost.",
                "The fixture's 100 CNY minimum commission is represented by FixedFeeModel(100 CNY) per fill; this fixture's one fill per instrument makes totals numerically comparable, while the general fee schedules are not equivalent.",
            ],
        },
        "timings": {
            "status": "not_measured" if adapter_result is None else "five_warm_runs_internal_only",
            "warmup_runs": 1 if adapter_result is not None else 0,
            "requested_runs": 5,
            "completed_runs": len(measured_runs),
            "conversion": timing_summary("conversion"),
            "initialization": timing_summary("initialization"),
            "event_processing": timing_summary("event_processing"),
            "end_to_end": timing_summary("end_to_end"),
            "repeated_projection_equal": repeated_projections_equal,
            "projection_sha256": hashlib.sha256(projection_bytes[0]).hexdigest() if projection_bytes else None,
            "peak_process_rss_bytes": (
                resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
                * (1024 if sys.platform.startswith("linux") else 1)
            ) if adapter_result is not None else None,
            "cold_start_end_to_end_ns": None,
            "throughput_comparison": "unresolved_native_order_lifecycle",
        },
        "measurement_protocol": {
            "reference_provenance": reference.get("provenance"),
            "cargo_lock_sha256": _sha256(ROOT / "Cargo.lock"),
            "python_requirements_sha256": _sha256(requirements),
            "resolved_python_packages": resolved_packages,
            "resolved_python_packages_sha256": (
                hashlib.sha256("\n".join(resolved_packages).encode()).hexdigest()
                if resolved_packages is not None else None
            ),
            "python_dependencies_fully_locked": False,
            "pip_freeze_error": pip_freeze.stderr if pip_freeze.returncode else None,
            "rust_build_profile": "release (--no-default-features --locked --offline)",
            "rustflags": os.environ.get("RUSTFLAGS"),
            "rustc_version": (reference.get("provenance") or {}).get("rustc"),
            "python_executable": sys.executable,
            "preflight": preflight,
        },
        "build_resources": {
            "status": "prebuilt_wheel_installed_no_native_build" if adapter_result is not None else "installation_or_runtime_blocked",
            "install_evidence": install_evidence,
            "target_path": str(ROOT / "target"),
            "target_delta_bytes": {
                "dev": dev_build.get("target_delta_bytes"),
                "release": release_build.get("target_delta_bytes"),
            },
            "incremental_build_ns": None if dev_build.get("wall_seconds") is None else int(dev_build["wall_seconds"] * 1_000_000_000),
            "release_build_ns": None if release_build.get("wall_seconds") is None else int(release_build["wall_seconds"] * 1_000_000_000),
            "cache_identity": dev_build.get("dependency_graph_sha256"),
            "cold_build_status": "unknown; no cache was cleared or isolated cold target built",
            "cold_build_ns": None,
        },
        "unresolved_reasons": unresolved_reasons,
    }
    if adapter_result is not None:
        all_checks_passed = all(
            check["passed"] for check in report["semantic_comparison"]["checks"]
        )
        report["status"] = "unresolved" if not all_checks_passed or unresolved_reasons else "passed"
        report["semantic_comparison"]["status"] = "mismatch" if not all_checks_passed else "passed"
        failed_fields = [check["field"] for check in report["semantic_comparison"]["checks"] if not check["passed"]]
        if "orders.nautilus_native_lifecycle" in failed_fields:
            report["unresolved_reasons"].append({
                "code": "nautilus_native_order_lifecycle_unverified",
                "detail": "Adapter status gating matches four Rust project attempts, but Nautilus received only three orders and did not reject B on the halted day.",
                "replay_condition": "Exercise native Nautilus halt rejection or retain this dimension as an explicit cross-engine gap.",
            })
        for field in failed_fields:
            if field != "orders.nautilus_native_lifecycle":
                report["unresolved_reasons"].append({
                    "code": "semantic_comparison_mismatch",
                    "detail": f"Project comparison failed for {field}.",
                    "replay_condition": "Inspect the raw field values and rerun the fixed fixture.",
                })
    report["strategy_comparisons"] = {}
    for strategy_name, rust_key in (
        ("s2", "b2_fast_event_momentum_rotation"),
        ("s3", "b2_fast_event_ma20_60"),
    ):
        rust_rotation = reference.get(rust_key)
        if rust_rotation is None:
            continue
        strategy_dataset = dataset
        input_hash = _sha256(dataset_path)
        if strategy_name == "s3":
            fixture_path = ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json"
            input_hash = _sha256(fixture_path)
            strategy_dataset = _ma_dataset(json.loads(fixture_path.read_text(encoding="utf-8")))
            expected_path = ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-expected-v1.json"
            if rust_rotation.get("expected_sha256") != _sha256(expected_path):
                report["strategy_comparisons"][strategy_name] = {
                    "status": "unresolved", "error": "S3 independent golden hash differs from Rust reference",
                }
                continue
        rotation_runs = []
        rotation_error = None
        if probe_error is None and rust_rotation.get("status") == "correctness_passed_and_measured":
            try:
                _run_nautilus(strategy_dataset, strategy=strategy_name)
                rotation_runs = [_run_nautilus(strategy_dataset, strategy=strategy_name) for _ in range(5)]
            except Exception as error:
                rotation_error = f"{type(error).__name__}: {error}"
        rotation = rotation_runs[0]["projection"] if rotation_runs else None
        expected_rotation = rust_rotation["projection"]
        checks = _compare(rotation, expected_rotation) if rotation is not None else []
        if rotation is not None:
            checks.append({
                "field": "signals.rank_topk_targets",
                "passed": rotation["signals"] == rust_rotation["signals"],
            })
            if strategy_name == "s2":
                actual_rankings = rotation["rankings"]
                expected_rankings = rust_rotation["rankings"]
                rank_equal = len(actual_rankings) == len(expected_rankings) and all(
                    actual["date"] == expected["date"]
                    and len(actual["candidates"]) == len(expected["candidates"])
                    and all(
                        left["symbol"] == right["symbol"]
                        and abs(left["score"] - right["score"]) <= 1e-8
                        for left, right in zip(actual["candidates"], expected["candidates"], strict=True)
                    ) for actual, expected in zip(actual_rankings, expected_rankings, strict=True)
                )
                checks.append({"field": "signals.factor_rankings", "passed": rank_equal})
        eligible = [check for check in checks if check["field"] != "orders.nautilus_native_lifecycle"]
        passed = bool(eligible) and all(check["passed"] for check in eligible)
        parallel = None
        if passed:
            try:
                parallel = parallel_runs(strategy_dataset, strategy_name)
            except Exception as error:
                rotation_error = f"parallel {type(error).__name__}: {error}"
        samples = [item["timings_ns"]["event_processing"] for item in rotation_runs]
        report["strategy_comparisons"][strategy_name] = {
            "status": "passed_common_subset" if passed else "unresolved",
            "checks": checks,
            "rust_reference_sha256": rust_rotation["checksum_sha256"],
            "input_sha256": input_hash,
            "input_version": strategy_dataset["dataset_version"],
            "adapter_projection": rotation,
            "nautilus_native_submissions": None if rotation is None else [
                order for order in rotation["orders"] if order.get("origin") == "nautilus_order"
            ],
            "adapter_project_events": None if rotation is None else rotation["orders"],
            "timings": {
                "warmup_runs": 1 if rotation_runs else 0,
                "event_processing": None if not samples else {
                    "raw_samples_ns": samples,
                    "median_ns": statistics.median(samples),
                    "p95_ns": sorted(samples)[math.ceil(0.95 * len(samples)) - 1],
                },
                "conversion_samples_ns": [item["timings_ns"]["conversion"] for item in rotation_runs],
                "initialization_samples_ns": [item["timings_ns"]["initialization"] for item in rotation_runs],
                "end_to_end_samples_ns": [item["timings_ns"]["end_to_end"] for item in rotation_runs],
            },
            "peak_process_rss_bytes": None if not rotation_runs else resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (1024 if sys.platform.startswith("linux") else 1),
            "parallel": parallel,
            "error": rotation_error or probe_error,
            "decision_scope": "Fill, cash, holdings, daily NAV, costs and common-subset throughput only; halted native order lifecycle excluded under ADR 0012.",
        }
    report["ticket09_decision"] = {
        "status": "unresolved",
        "minimum_fast_event_gain": 2.0,
        "required_conditions": [
            "Both S2 and S3 must pass Fill, cash, positions, NAV and cost parity.",
            "Fast Event must reach 2x lower median latency and 2x higher parallel Runs/s for both strategies on a matched release workload.",
            "Fast Event must have no higher peak RSS on a matched measurement scope.",
        ],
        "common_subset_status": {
            strategy: comparison["status"]
            for strategy, comparison in report["strategy_comparisons"].items()
        },
        "excluded_dimension": "HALTED native order submission and lifecycle; project adapter events and native submissions are reported separately under ADR 0012.",
        "reason": "Rust workers share prebuilt bars and use threads; Nautilus workers convert quotes, initialize an engine for each Run and use processes. The measured latency, RSS and throughput scopes are not aligned, so no engine-speed ratio or adopt/defer/reject selection is supported.",
        "rust_build_record": "poc/poc0-benchmark/results/b2-09-release-build-2026-09-27.json",
        "rust_parallel_records": [
            "poc/poc0-benchmark/results/b2-09-s2-throughput.json",
            "poc/poc0-benchmark/results/b2-09-s3-throughput.json",
        ],
        "next_step": "Run both engines under matching conversion and account-initialization boundaries with RSS measured on the same worker/process scope.",
    }
    return report


def _compare(actual: dict[str, Any], expected: dict[str, Any]) -> list[dict[str, Any]]:
    tolerance = 1e-8
    order_fields = ("decision_date", "attempt_date", "symbol", "side", "quantity", "reason")
    expected_orders = [tuple(order.get(field) for field in order_fields) for order in expected.get("orders", [])]
    actual_orders = [tuple(order.get(field) for field in order_fields) for order in actual.get("orders", [])]
    native_orders = [
        tuple(order.get(field) for field in order_fields)
        for order in actual.get("orders", [])
        if order.get("origin") == "nautilus_order"
    ]
    expected_fills = expected.get("fills", [])
    actual_fills = actual.get("fills", [])
    expected_by_key = {(x["date"], x["symbol"], x["side"]): x for x in expected_fills}
    actual_by_key = {(x["date"], x["symbol"], x["side"]): x for x in actual_fills}
    keys_equal = set(expected_by_key) == set(actual_by_key)
    fill_fields_equal = keys_equal and all(
        int(actual_by_key[key]["quantity"]) == int(expected_by_key[key]["quantity"])
        and abs(float(actual_by_key[key]["fill_price"]) - float(expected_by_key[key]["fill_price"])) <= tolerance
        and abs(float(actual_by_key[key]["commission"]) - float(expected_by_key[key]["commission"])) <= tolerance
        for key in expected_by_key
    )
    expected_ledger = {x["date"]: x for x in expected.get("ledger", [])}
    actual_ledger = {x["date"]: x for x in actual.get("ledger", [])}
    ledger_keys_equal = set(expected_ledger) == set(actual_ledger)
    cash_nav_equal = ledger_keys_equal and all(
        abs(float(expected_ledger[d]["cash"]) - float(actual_ledger[d]["cash"])) <= tolerance
        and abs(float(expected_ledger[d]["nav"]) - float(actual_ledger[d]["nav"])) <= tolerance
        for d in expected_ledger
    )
    holdings_equal = ledger_keys_equal and all(
        {h["symbol"]: h["quantity"] for h in expected_ledger[d]["holdings"]}
        == {h["symbol"]: h["quantity"] for h in actual_ledger[d]["holdings"]}
        for d in expected_ledger
    )
    account_snapshots = actual.get("nautilus_account_snapshots", {})
    account_keys_equal = set(account_snapshots) == set(expected_ledger)
    direct_account_equal = account_keys_equal and all(
        abs(float(expected_ledger[d]["cash"]) - float(account_snapshots[d]["cash"])) <= tolerance
        and {h["symbol"]: h["quantity"] for h in expected_ledger[d]["holdings"]}
        == account_snapshots[d]["positions"]
        for d in expected_ledger
    )
    expected_summary = expected.get("summary", {})
    actual_summary = actual.get("summary", {})
    cost_equal = all(
        abs(float(expected_summary.get(k, 0.0)) - float(actual_summary.get(k, 0.0))) <= tolerance
        for k in ("commission", "tax", "slippage_cost", "total_cost")
    )
    return [
        {
            "field": "orders.adapter_project_contract",
            "passed": Counter(actual_orders) == Counter(expected_orders),
            "expected_order_events": len(expected.get("orders", [])),
            "actual_adapter_order_events": len(actual.get("orders", [])),
            "project_order_fields": list(order_fields),
        },
        {
            "field": "orders.nautilus_native_lifecycle",
            "passed": native_orders == expected_orders,
            "expected_rust_attempts": len(expected_orders),
            "actual_nautilus_submissions": len(native_orders),
            "adapter_rejections_excluded": sum(
                order.get("origin") == "adapter_status_gate" for order in actual.get("orders", [])
            ),
        },
        {"field": "fills.quantity_price_commission", "passed": fill_fields_equal, "expected_fill_count": len(expected_fills), "actual_fill_count": len(actual_fills)},
        {"field": "daily.cash_nav", "passed": cash_nav_equal, "session_count": len(expected_ledger)},
        {"field": "daily.holdings", "passed": holdings_equal, "session_count": len(expected_ledger)},
        {"field": "nautilus.account_cash_positions", "passed": direct_account_equal, "snapshot_count": len(account_snapshots)},
        {"field": "summary.costs", "passed": cost_equal},
    ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--reference-report", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    reference = json.loads(args.reference_report.read_text(encoding="utf-8"))
    adapter_report = build_report(args.dataset, args.reference_report)
    reference["nautilus_adapter"] = adapter_report
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(reference, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"status": adapter_report["status"], "output": str(args.output)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
