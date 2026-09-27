#!/usr/bin/env python3
"""POC-0 Nautilus boundary probe; never substitutes synthetic timing values."""

from __future__ import annotations

import argparse
from datetime import datetime
from decimal import Decimal, ROUND_HALF_UP
import hashlib
import importlib.metadata
import json
import math
import os
import platform
import resource
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

PINNED_NAUTILUS_VERSION = "2.0.0rc5"
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


def _run_nautilus(dataset: dict[str, Any]) -> dict[str, Any]:
    """Run one isolated B2 scenario through public Nautilus 2.x Python APIs."""
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

    conversion_started = time.perf_counter_ns()
    currency = Currency.from_str("CNY")
    venue = Venue("SIM")
    ids = []
    instruments = []
    rows_by_symbol: dict[str, list[tuple[str, str, Any]]] = {}
    precision_quantum = Decimal("0.01")
    bars_by_date = {d: {} for d in dataset["calendar"]}
    missing = {(x["symbol"], x["date"]) for x in dataset.get("missing_bars", [])}
    blocked_statuses = {
        (x["symbol"], x["date"]): x.get("trade_status") or "missing_status"
        for x in dataset.get("execution_status_overrides", [])
        if not x["is_tradable"] or x.get("trade_status") != "TRADABLE" or not x.get("sources")
    }
    signal_date = dataset["calendar"][len(dataset["calendar"]) - 5]
    open_time = ZoneInfo(dataset.get("timezone", "Asia/Shanghai"))
    slippage_rate = Decimal(str(dataset["costs"]["buy_slippage_bps"])) / Decimal(10_000)
    targets: dict[str, int] = {}
    budget = Decimal(str(dataset["account"]["initial_cash"])) / Decimal(len(dataset["instruments"]))
    lot = int(dataset["account"]["lot_size"])
    for item in dataset["instruments"]:
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
            buy_ask = (open_price * (Decimal(1) + slippage_rate)).quantize(
                precision_quantum, rounding=ROUND_HALF_UP
            )
            buy_bid = (open_price * (Decimal(1) - slippage_rate)).quantize(
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
            bars_by_date[date][symbol] = float(close_price)
        rows_by_symbol[symbol] = rows
    signal_ts = int(datetime.fromisoformat(f"{signal_date}T15:00:00+08:00").timestamp() * 1_000_000_000)
    open_timestamps = {
        int(datetime.fromisoformat(f"{date}T09:30:00+08:00").timestamp() * 1_000_000_000)
        for date in dataset["calendar"]
        if date > signal_date
    }
    open_dates = {
        int(datetime.fromisoformat(f"{date}T09:30:00+08:00").timestamp() * 1_000_000_000): date
        for date in dataset["calendar"]
        if date > signal_date
    }
    close_dates = {
        int(datetime.fromisoformat(f"{date}T15:00:00+08:00").timestamp() * 1_000_000_000): date
        for date in dataset["calendar"]
    }
    conversion_ns = time.perf_counter_ns() - conversion_started

    init_started = time.perf_counter_ns()
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
    strategy = BuyHoldStrategy(
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
    engine.add_strategy(strategy)
    init_ns = time.perf_counter_ns() - init_started

    run_started = time.perf_counter_ns()
    engine.run()
    event_ns = time.perf_counter_ns() - run_started

    fills = sorted(strategy.fill_events, key=lambda x: (x["ts_event"], x["symbol"]))
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
            cash -= fill["gross_value"] + fill["commission"]
            holdings[fill["symbol"]] = holdings.get(fill["symbol"], 0) + fill["quantity"]
        marks.update(bars_by_date[date])
        marked_holdings = [
            {"symbol": symbol, "quantity": quantity, "mark_price": marks[symbol]}
            for symbol, quantity in sorted(holdings.items())
        ]
        nav = cash + sum(h["quantity"] * h["mark_price"] for h in marked_holdings)
        ledger.append({"date": date, "cash": cash, "holdings": marked_holdings, "nav": nav})
    engine.dispose()
    return {
        "projection": {
            "orders": sorted(strategy.order_events, key=lambda x: (x.get("attempt_date", ""), x["symbol"])),
            "fills": fills,
            "ledger": ledger,
            "nautilus_account_snapshots": strategy.account_snapshots,
            "targets": targets,
            "summary": {
                "initial_cash": float(dataset["account"]["initial_cash"]),
                "final_equity": ledger[-1]["nav"],
                "final_positions": holdings,
                "commission": sum(x["commission"] for x in fills),
                "tax": 0.0,
                "slippage_cost": sum(
                    (x["fill_price"] - float(dataset["bar_defaults"]["open"])) * x["quantity"]
                    for x in fills
                ),
                "total_cost": sum(x["commission"] for x in fills)
                + sum((x["fill_price"] - float(dataset["bar_defaults"]["open"])) * x["quantity"] for x in fills),
            },
        },
        "timings_ns": {
            "conversion": conversion_ns,
            "initialization": init_ns,
            "event_processing": event_ns,
            "end_to_end": time.perf_counter_ns() - started,
        },
    }


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
            "passed": actual_orders == expected_orders,
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
