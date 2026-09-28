#!/usr/bin/env python3
"""Isolate native Nautilus HALT order handling from the project Adapter."""

from __future__ import annotations

import importlib.metadata
import json
import platform
import subprocess
from datetime import datetime, timezone

PINNED_VERSION = "2.0.0rc5"
QUOTE_TS = 1_767_756_600_000_000_000
HALT_TS = QUOTE_TS + 1_000_000_000
SUBMIT_TS = HALT_TS + 1_000_000


def run_case(with_halt: bool) -> dict:
    from nautilus_trader.backtest import BacktestEngine
    from nautilus_trader.common import LogLevel
    from nautilus_trader.config import BacktestEngineConfig, LoggerConfig, StrategyConfig
    from nautilus_trader.model import (
        AccountType, Currency, Equity, InstrumentId, InstrumentStatus,
        MarketStatusAction, Money, OmsType, OrderSide, Price, Quantity,
        QuoteTick, Symbol, Venue,
    )
    from nautilus_trader.trading import Strategy

    class Probe(Strategy):
        def __init__(self) -> None:
            super().__init__(StrategyConfig())
            self.events: list[dict] = []
            self.order = None

        def on_start(self) -> None:
            self.subscribe_quotes(instrument_id)
            if with_halt:
                self.subscribe_instrument_status(instrument_id)

        def submit_probe(self) -> None:
            self.order = self.order_factory.market(
                instrument_id=instrument_id,
                order_side=OrderSide.BUY,
                quantity=Quantity.from_int(1),
            )
            self.events.append({"type": "submit_order", "ts_event": SUBMIT_TS})
            self.submit_order(self.order)

        def on_quote(self, tick) -> None:
            self.events.append({"type": "quote_observed", "ts_event": int(tick.ts_event)})
            self.clock.set_time_alert(
                "submit_probe", datetime.fromtimestamp(SUBMIT_TS / 1_000_000_000, tz=timezone.utc)
            )

        def on_instrument_status(self, status) -> None:
            self.events.append({"type": "status_observed", "action": status.action.name, "ts_event": int(status.ts_event)})

        def on_time_event(self, event) -> None:
            self.submit_probe()

        def on_order_submitted(self, event) -> None:
            self.events.append({"type": "submitted", "ts_event": int(event.ts_event)})

        def on_order_accepted(self, event) -> None:
            self.events.append({"type": "accepted", "ts_event": int(event.ts_event)})

        def on_order_rejected(self, event) -> None:
            self.events.append({"type": "rejected", "reason": str(event.reason), "ts_event": int(event.ts_event)})

        def on_order_filled(self, event) -> None:
            self.events.append({"type": "filled", "price": str(event.last_px), "ts_event": int(event.ts_event)})

    venue = Venue("SIM")
    currency = Currency.from_str("USD")
    instrument_id = InstrumentId.from_str("PROBE.SIM")
    instrument = Equity(
        instrument_id=instrument_id, raw_symbol=Symbol("PROBE"), currency=currency,
        price_precision=2, price_increment=Price.from_str("0.01"),
        lot_size=Quantity.from_int(1), ts_event=0, ts_init=0,
    )
    quote = QuoteTick(
        instrument_id, Price.from_str("99.00"), Price.from_str("100.00"),
        Quantity.from_int(100), Quantity.from_int(100), QUOTE_TS, QUOTE_TS,
    )
    data = [quote]
    if with_halt:
        data.append(InstrumentStatus(instrument_id, MarketStatusAction.HALT, HALT_TS, HALT_TS))
    engine = BacktestEngine(BacktestEngineConfig(logging=LoggerConfig(stdout_level=LogLevel.ERROR)))
    engine.add_venue(
        venue=venue, oms_type=OmsType.NETTING, account_type=AccountType.CASH,
        starting_balances=[Money.from_str("10000.00 USD")], base_currency=currency,
    )
    engine.add_instrument(instrument)
    engine.add_data(data)
    strategy = Probe()
    engine.add_strategy(strategy)
    engine_error = None
    try:
        engine.run(end=SUBMIT_TS + 1_000_000)
    except Exception as error:
        engine_error = f"{type(error).__name__}: {error}"
    event_types = [event["type"] for event in strategy.events]
    cached_order = None if strategy.order is None else strategy.cache.order(strategy.order.client_order_id)
    order_state = None if cached_order is None else cached_order.status.name
    if "rejected" in event_types:
        outcome = "on_order_rejected"
    elif "filled" in event_types:
        outcome = "filled"
    elif "accepted" in event_types:
        outcome = "accepted_unfilled"
    else:
        outcome = "other"
    if engine_error is not None:
        outcome = "other"
    return {"with_halt": with_halt, "events": strategy.events, "order_state": order_state, "outcome": outcome, "engine_error": engine_error}


def main() -> None:
    try:
        version = importlib.metadata.version("nautilus_trader")
    except importlib.metadata.PackageNotFoundError:
        version = None
    if version != PINNED_VERSION:
        print(json.dumps({"outcome": "Unknown", "reason": f"requires nautilus_trader=={PINNED_VERSION}; installed={version}"}))
        return
    try:
        import nautilus_trader  # noqa: F401
    except Exception as error:
        print(json.dumps({"outcome": "Unknown", "reason": f"pinned wheel unavailable: {type(error).__name__}: {error}"}))
        return
    cases = []
    for with_halt in (False, True):
        try:
            cases.append(run_case(with_halt))
        except Exception as error:
            cases.append({
                "with_halt": with_halt, "events": [], "order_state": None,
                "outcome": "other", "engine_error": f"{type(error).__name__}: {error}",
            })
    control, halted = cases
    if control["engine_error"] is None:
        assert control["outcome"] == "filled", control
        assert control["events"][-1]["price"] == "100.00", control
        assert control["order_state"] == "FILLED", control
    if halted["engine_error"] is None:
        assert [event["type"] for event in halted["events"][:2]] == ["quote_observed", "status_observed"], halted
        assert halted["events"][1]["action"] == "HALT", halted
        if halted["outcome"] == "on_order_rejected":
            assert halted["order_state"] == "REJECTED", halted
        elif halted["outcome"] == "filled":
            assert halted["order_state"] == "FILLED", halted
        elif halted["outcome"] == "accepted_unfilled":
            assert halted["order_state"] == "ACCEPTED", halted
    report = {
        "nautilus_version": version, "python_version": platform.python_version(),
        "code_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "event_sequence": {"quote_ts": QUOTE_TS, "halt_ts": HALT_TS, "submit_ts": SUBMIT_TS},
        "control": control, "halted": halted,
        "conclusion": (
            "Unknown" if control["engine_error"] or halted["engine_error"] or halted["outcome"] == "other"
            else "confirmed" if halted["outcome"] == "on_order_rejected"
            else "falsified"
        ),
    }
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()