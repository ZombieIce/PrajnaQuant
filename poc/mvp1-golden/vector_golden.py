#!/usr/bin/env python3
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path


STATUS_PRECEDENCE = (
    "insufficient_window",
    "missing_input",
    "unknown_availability",
)


def positive_integer(value, name, minimum=1):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")
    return value


def finite_number(value, name):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a finite number")
    value = float(value)
    if not math.isfinite(value):
        raise ValueError(f"{name} must be a finite number")
    return value


def available_at_for_session(session_date, close_available_at):
    if close_available_at is None:
        return None
    if not isinstance(close_available_at, str):
        raise ValueError("bar_defaults.close_available_at must be a string or null")
    try:
        timestamp = datetime.fromisoformat(
            f"{session_date}T{close_available_at}"
        )
    except ValueError as exc:
        raise ValueError(
            f"invalid close_available_at for session {session_date}"
        ) from exc
    if timestamp.utcoffset() is None:
        raise ValueError(
            f"close_available_at for session {session_date} needs an explicit offset"
        )
    return timestamp.astimezone(timezone.utc)


def timestamp_text(timestamp):
    if timestamp is None:
        return None
    return timestamp.isoformat().replace("+00:00", "Z")


def session_timestamp(session_date, local_time, field_name):
    if local_time is None:
        return None
    if not isinstance(local_time, str):
        raise ValueError(f"{field_name} must be a string or null")
    try:
        timestamp = datetime.fromisoformat(f"{session_date}T{local_time}")
    except ValueError as exc:
        raise ValueError(f"invalid {field_name} for session {session_date}") from exc
    if timestamp.utcoffset() is None:
        raise ValueError(f"{field_name} for session {session_date} needs an explicit offset")
    return timestamp.astimezone(timezone.utc)


def build_inputs(fixture):
    if not isinstance(fixture, dict):
        raise ValueError("fixture must be a JSON object")

    calendar = fixture.get("calendar")
    if not isinstance(calendar, list) or not calendar:
        raise ValueError("fixture.calendar must be a non-empty list")
    try:
        parsed_dates = [datetime.strptime(item, "%Y-%m-%d").date() for item in calendar]
    except (TypeError, ValueError) as exc:
        raise ValueError("fixture.calendar must contain ISO session dates") from exc
    if any(
        parsed_date.isoformat() != value
        for parsed_date, value in zip(parsed_dates, calendar)
    ):
        raise ValueError("fixture.calendar dates must use YYYY-MM-DD")
    if parsed_dates != sorted(set(parsed_dates)):
        raise ValueError("fixture.calendar must be strictly increasing")

    defaults = fixture.get("bar_defaults")
    if not isinstance(defaults, dict):
        raise ValueError("fixture.bar_defaults must be an object")
    if "close_available_at" not in defaults:
        raise ValueError("fixture.bar_defaults.close_available_at is required")
    close_available_at = defaults.get("close_available_at")
    session_availability = [
        available_at_for_session(session_date, close_available_at)
        for session_date in calendar
    ]

    instruments = fixture.get("instruments")
    if not isinstance(instruments, list) or not instruments:
        raise ValueError("fixture.instruments must be a non-empty list")
    symbols = set()
    bars_by_symbol = {}
    for instrument in instruments:
        if not isinstance(instrument, dict):
            raise ValueError("each fixture instrument must be an object")
        symbol = instrument.get("symbol")
        if not isinstance(symbol, str) or not symbol or symbol in symbols:
            raise ValueError("fixture instrument symbols must be unique non-empty strings")
        symbols.add(symbol)
        closes = instrument.get("closes")
        if not isinstance(closes, list) or len(closes) != len(calendar):
            raise ValueError(f"{symbol}.closes must match fixture.calendar")
        bars = []
        for index, close in enumerate(closes):
            if close is None:
                bars.append(None)
                continue
            close = finite_number(close, f"{symbol}.closes[{index}]")
            if close <= 0:
                raise ValueError(f"{symbol}.closes[{index}] must be positive")
            bars.append(
                {
                    "close": close,
                    "available_at": session_availability[index],
                }
            )
        bars_by_symbol[symbol] = bars

    missing_bars = fixture.get("missing_bars", [])
    if not isinstance(missing_bars, list):
        raise ValueError("fixture.missing_bars must be a list")
    seen_missing = set()
    calendar_indices = {session_date: index for index, session_date in enumerate(calendar)}
    for missing in missing_bars:
        if not isinstance(missing, dict):
            raise ValueError("each missing bar must be an object")
        symbol = missing.get("symbol")
        session_date = missing.get("date")
        key = (symbol, session_date)
        if symbol not in bars_by_symbol or session_date not in calendar_indices:
            raise ValueError(f"missing bar references an unknown symbol or session: {key}")
        if key in seen_missing:
            raise ValueError(f"duplicate missing bar: {key}")
        seen_missing.add(key)
        bars_by_symbol[symbol][calendar_indices[session_date]] = None

    strategy = fixture.get("strategy")
    if not isinstance(strategy, dict):
        raise ValueError("fixture.strategy must be an object")
    parameters = {
        "momentum_short_days": positive_integer(
            strategy.get("momentum_short_days"), "strategy.momentum_short_days"
        ),
        "momentum_long_days": positive_integer(
            strategy.get("momentum_long_days"), "strategy.momentum_long_days"
        ),
        "volatility_window": positive_integer(
            strategy.get("volatility_window"),
            "strategy.volatility_window",
            minimum=2,
        ),
        "short_momentum_weight": finite_number(
            strategy.get("short_momentum_weight"),
            "strategy.short_momentum_weight",
        ),
        "long_momentum_weight": finite_number(
            strategy.get("long_momentum_weight"),
            "strategy.long_momentum_weight",
        ),
        "volatility_weight": finite_number(
            strategy.get("volatility_weight"), "strategy.volatility_weight"
        ),
    }
    trend_filter_enabled = strategy.get("trend_filter")
    if not isinstance(trend_filter_enabled, bool):
        raise ValueError("strategy.trend_filter must be a boolean")

    return calendar, bars_by_symbol, parameters, trend_filter_enabled


def evaluate_window(bars, session_index, kind, window):
    if kind == "momentum":
        if session_index < window:
            return None, None, "insufficient_window"
        indices = (session_index - window, session_index)
    elif kind == "volatility":
        if session_index < window:
            return None, None, "insufficient_window"
        indices = range(session_index - window, session_index + 1)
    elif kind == "trend_filter":
        if session_index + 1 < window:
            return None, None, "insufficient_window"
        indices = range(session_index - window + 1, session_index + 1)
    else:
        raise ValueError(f"unknown factor kind: {kind}")

    inputs = [bars[index] for index in indices]
    if any(item is None for item in inputs):
        return None, None, "missing_input"
    available_at = [item["available_at"] for item in inputs]
    if any(timestamp is None for timestamp in available_at):
        return None, None, "unknown_availability"

    closes = [item["close"] for item in inputs]
    if kind == "momentum":
        value = closes[-1] / closes[0] - 1.0
    elif kind == "volatility":
        returns = [
            closes[index] / closes[index - 1] - 1.0
            for index in range(1, len(closes))
        ]
        mean_return = math.fsum(returns) / window
        variance = math.fsum(
            (daily_return - mean_return) ** 2 for daily_return in returns
        ) / (window - 1)
        value = math.sqrt(variance)
    else:
        value = int(closes[-1] >= math.fsum(closes) / window)

    return value, max(available_at), "ok"


def factor_result(value, available_at, status):
    return {
        "value": value,
        "available_at": timestamp_text(available_at),
        "status": status,
    }


def execution_record(value, session_date):
    if not isinstance(value, dict):
        raise ValueError("execution status must be an object")
    is_tradable = value.get("is_tradable")
    if not isinstance(is_tradable, bool):
        raise ValueError("execution status is_tradable must be a boolean")
    if "available_at" not in value:
        raise ValueError("execution status available_at is required")
    return {
        "is_tradable": is_tradable,
        "available_at": session_timestamp(
            session_date, value["available_at"], "execution_status.available_at"
        ),
    }


def build_vector_inputs(fixture, calendar, bars_by_symbol):
    defaults = fixture["bar_defaults"]
    if "open_available_at" not in defaults:
        raise ValueError("bar_defaults.open_available_at is required")
    open_times = [
        session_timestamp(
            session_date,
            defaults["open_available_at"],
            "bar_defaults.open_available_at",
        )
        for session_date in calendar
    ]
    if any(timestamp is None for timestamp in open_times):
        raise ValueError("bar_defaults.open_available_at cannot be null")
    default_open = finite_number(defaults.get("open"), "bar_defaults.open")
    if default_open <= 0:
        raise ValueError("bar_defaults.open must be positive")

    instruments = {}
    for instrument in fixture.get("instruments", []):
        symbol = instrument["symbol"]
        opens = instrument.get("opens")
        if opens is None:
            prices = [default_open] * len(calendar)
        else:
            if not isinstance(opens, list) or len(opens) != len(calendar):
                raise ValueError(f"{symbol}.opens must match fixture.calendar")
            prices = []
            for index, price in enumerate(opens):
                if price is None:
                    prices.append(None)
                    continue
                price = finite_number(price, f"{symbol}.opens[{index}]")
                if price <= 0:
                    raise ValueError(f"{symbol}.opens[{index}] must be positive")
                prices.append(price)

        instrument_id = f"{symbol}.SYNTH"
        instruments[instrument_id] = [
            price if bars_by_symbol[symbol][index] is not None else None
            for index, price in enumerate(prices)
        ]

    default_status = fixture.get("execution_status_default")
    if default_status is not None and not isinstance(default_status, dict):
        raise ValueError("fixture.execution_status_default must be an object")
    statuses = {}
    if default_status is not None:
        for instrument in fixture["instruments"]:
            for session_date in calendar:
                statuses[(instrument["symbol"], session_date)] = execution_record(
                    default_status, session_date
                )

    overrides = fixture.get("execution_status_overrides", [])
    if not isinstance(overrides, list):
        raise ValueError("fixture.execution_status_overrides must be a list")
    symbols = {instrument["symbol"] for instrument in fixture["instruments"]}
    seen_overrides = set()
    calendar_dates = set(calendar)
    for override in overrides:
        if not isinstance(override, dict):
            raise ValueError("each execution status override must be an object")
        symbol = override.get("symbol")
        session_date = override.get("date")
        key = (symbol, session_date)
        if symbol not in symbols or session_date not in calendar_dates:
            raise ValueError(
                f"execution status override references an unknown symbol or session: {key}"
            )
        if key in seen_overrides:
            raise ValueError(f"duplicate execution status override: {key}")
        seen_overrides.add(key)
        statuses[key] = execution_record(override, session_date)

    return instruments, open_times, statuses


def build_vector_result(fixture, calendar, bars_by_symbol, factors, strategy_kind="top_k_rank"):
    retry = strategy_kind == "buy_and_hold"
    instruments, open_times, statuses = build_vector_inputs(
        fixture, calendar, bars_by_symbol
    )
    strategy = fixture["strategy"]
    top_k = positive_integer(strategy.get("top_n"), "strategy.top_n")
    rebalance_every = positive_integer(
        strategy.get("rebalance_every"), "strategy.rebalance_every"
    )
    fixture_costs = fixture.get("costs")
    if not isinstance(fixture_costs, dict):
        raise ValueError("fixture.costs must be an object")
    cost_parameters = {
        name: finite_number(fixture_costs.get(name), f"costs.{name}")
        for name in (
            "commission_rate",
            "buy_tax_rate",
            "sell_tax_rate",
            "buy_slippage_bps",
            "sell_slippage_bps",
        )
    }
    if any(value < 0 for value in cost_parameters.values()):
        raise ValueError("vector transaction costs must be non-negative")
    buy_rate = (
        cost_parameters["commission_rate"]
        + cost_parameters["buy_slippage_bps"] / 10000
        + cost_parameters["buy_tax_rate"]
    )
    sell_rate = (
        cost_parameters["commission_rate"]
        + cost_parameters["sell_slippage_bps"] / 10000
        + cost_parameters["sell_tax_rate"]
    )
    instrument_ids = sorted(instruments)
    symbols = {
        f"{instrument['symbol']}.SYNTH": instrument["symbol"]
        for instrument in fixture["instruments"]
    }

    scores_by_date = {}
    for row in factors.get("rotation_score", []):
        if row["status"] == "ok":
            scores_by_date.setdefault(row["session_date"], []).append(
                (row["instrument_id"], finite_number(row["value"], "rotation_score"))
            )

    first_decision_index = next(
        (
            index
            for index, session_date in enumerate(calendar)
            if scores_by_date.get(session_date)
        ),
        None,
    )
    decision_indices = (
        set(
            range(
                first_decision_index,
                len(calendar),
                rebalance_every,
            )
        )
        if first_decision_index is not None
        else set()
    )
    decision_rows = []
    decisions_by_index = {}
    for index in sorted(decision_indices):
        session_date = calendar[index]
        ranked_scores = sorted(
            scores_by_date.get(session_date, []),
            key=lambda item: (-item[1], item[0]),
        )
        ranked = [
            {
                "instrument_id": instrument_id,
                "score": score,
                "rank": rank,
            }
            for rank, (instrument_id, score) in enumerate(ranked_scores, start=1)
        ]
        selected = ranked[:top_k]
        weight = 1.0 / len(selected) if selected else 0.0
        targets = {
            item["instrument_id"]: weight for item in selected
        }
        decision = {
            "decision_session": session_date,
            "ranked": ranked,
            "targets": targets,
        }
        decision_rows.append(decision)
        decisions_by_index[index] = decision

    if strategy_kind != "top_k_rank":
        decision_rows = []
        if strategy_kind == "buy_and_hold":
            decision_rows = [{"decision_session": calendar[0], "ranked": [],
                              "targets": {id: 1 / len(instrument_ids) for id in instrument_ids}}]
        else:
            held, previous = set(), {}
            gaps_by_date = {}
            for row in factors["ma_gap(20,60)"]:
                gaps_by_date.setdefault(row["session_date"], []).append(row)
            for index, date in enumerate(calendar):
                old = held.copy()
                for row in gaps_by_date[date]:
                    id = row["instrument_id"]
                    usable = row["status"] == "ok" and row["available_at"] <= timestamp_text(
                        available_at_for_session(date, "15:00:00+08:00"))
                    if not usable:
                        previous.pop(id, None)
                        continue
                    gap = row["value"]
                    prior = previous.get(id)
                    if prior is not None:
                        if prior <= 0 and gap > 0:
                            held.add(id)
                        if prior >= 0 and gap < 0:
                            held.discard(id)
                    previous[id] = gap
                if old != held:
                    decision_rows.append({"decision_session": date, "ranked": [],
                                          "targets": {id: 1 / len(instrument_ids) for id in sorted(held)}})
        decisions_by_index = {calendar.index(row["decision_session"]): row for row in decision_rows}

    marks_by_index = []
    carried_by_index = []
    last_prices = {}
    for index in range(len(calendar)):
        marks = {}
        valuation_carried = []
        for instrument_id in instrument_ids:
            price = instruments[instrument_id][index]
            if price is not None:
                marks[instrument_id] = price
            elif instrument_id in last_prices:
                marks[instrument_id] = last_prices[instrument_id]
                valuation_carried.append(instrument_id)
        marks_by_index.append(marks)
        carried_by_index.append(valuation_carried)
        last_prices = marks

    sessions = []
    executions = []
    weights = {}
    target_weights = {}
    pending = None
    nav = 1.0
    retry_entries = False

    for index, session_date in enumerate(calendar):
        marks = marks_by_index[index]
        valuation_carried = carried_by_index[index]

        session_cost = 0.0
        turnover = 0.0
        if pending is not None:
            executable = {}
            for instrument_id in instrument_ids:
                symbol = symbols[instrument_id]
                status = statuses.get((symbol, session_date))
                executable[instrument_id] = (
                    instruments[instrument_id][index] is not None
                    and status is not None
                    and status["is_tradable"]
                    and status["available_at"] is not None
                    and status["available_at"] <= open_times[index]
                )
            blocked = sorted(
                instrument_id
                for instrument_id, weight in weights.items()
                if weight > 0 and not executable[instrument_id]
            )
            if blocked:
                executions.append(
                    {
                        "session_date": session_date,
                        "kind": "retry_deferred" if retry_entries else "deferred",
                        "blocked": blocked,
                        "skipped_buys": [],
                    }
                )
            else:
                skipped_buys = sorted(
                    instrument_id
                    for instrument_id, weight in pending["targets"].items()
                    if weight > 0 and not executable[instrument_id]
                )
                next_weights = {
                    instrument_id: weight
                    for instrument_id, weight in pending["targets"].items()
                    if executable[instrument_id]
                }
                if retry_entries:
                    cash = max(0.0, 1 - math.fsum(weights.values()))
                    buys = {}
                    for id, target in sorted(next_weights.items()):
                        amount = min(target, cash / (1 + buy_rate))
                        cash = max(0.0, cash - amount * (1 + buy_rate))
                        if amount > 0:
                            buys[id] = amount
                    turnover = math.fsum(buys.values())
                    session_cost = turnover * buy_rate
                    next_weights = {id: weight / (1 - session_cost) for id, weight in weights.items()}
                    next_weights.update({id: amount / (1 - session_cost) for id, amount in buys.items()})
                deltas = {
                    instrument_id: next_weights.get(instrument_id, 0.0)
                    - weights.get(instrument_id, 0.0)
                    for instrument_id in instrument_ids
                }
                if not retry_entries:
                    buys = math.fsum(max(delta, 0.0) for delta in deltas.values())
                    sells = math.fsum(max(-delta, 0.0) for delta in deltas.values())
                    turnover = buys + sells
                    session_cost = buys * buy_rate + sells * sell_rate
                if session_cost >= 1:
                    raise ValueError("transaction costs must be less than 100% of NAV")
                weights = next_weights
                executions.append(
                    {
                        "session_date": session_date,
                        "kind": "entries_retried" if retry_entries else "executed",
                        "blocked": [],
                        "skipped_buys": skipped_buys,
                    }
                )
                remaining = {id: weight for id, weight in pending["targets"].items() if id in skipped_buys}
                if retry and remaining:
                    pending = {"decision_session": pending["decision_session"], "targets": remaining,
                               "retry_entries": True}
                    retry_entries = True
                else:
                    pending = None
                    retry_entries = False

        weights_after_execution = weights.copy()
        gross_return = 0.0
        if index + 1 < len(calendar):
            next_marks = marks_by_index[index + 1]
            contributions = []
            for instrument_id, weight in weights.items():
                if instrument_id not in marks or instrument_id not in next_marks:
                    raise ValueError(
                        f"held instrument has no valuation price: {instrument_id}"
                    )
                contributions.append(
                    weight * (next_marks[instrument_id] / marks[instrument_id] - 1)
                )
            gross_return = math.fsum(contributions)
            divisor = 1 + gross_return
            if divisor <= 0:
                raise ValueError("portfolio return cannot reduce NAV to zero")
            weights = {
                instrument_id: weight
                * (next_marks[instrument_id] / marks[instrument_id])
                / divisor
                for instrument_id, weight in weights.items()
            }

        nav *= (1 - session_cost) * (1 + gross_return)
        net_return = (1 - session_cost) * (1 + gross_return) - 1
        decision = decisions_by_index.get(index)
        if decision is not None:
            pending = {
                "decision_session": session_date,
                "targets": decision["targets"],
            }
            target_weights = decision["targets"]
            retry_entries = False

        sessions.append(
            {
                "session_date": session_date,
                "target_weights": target_weights,
                "weights_after_execution": weights_after_execution,
                "turnover": turnover,
                "cost": session_cost,
                "gross_return": gross_return,
                "net_return": net_return,
                "nav": nav,
                "valuation_carried": valuation_carried,
            }
        )

    assumptions = [
        "proportional transaction costs",
        "no minimum commission or lot size",
        "raw open-to-open returns",
        "Static Universe is not point-in-time",
        "conservative deferral when a held instrument is unavailable",
        "weight-based Vector NAV is not a cash-and-quantity account ledger",
        "long-only unlevered weights; residual cash earns zero return",
        "availability assumption: none",
    ]
    return {
        "sessions": sessions,
        "decisions": decision_rows,
        "executions": executions,
        "pending_at_end": pending,
        "assumptions": assumptions,
        "costs": cost_parameters,
        "availability_assumption": "none",
    }


def build_summary(vector, sessions_per_year):
    sessions = vector["sessions"]
    session_count = len(sessions)
    returns = [row["net_return"] for row in sessions[1:]]
    n_returns = len(returns)
    insufficient_sessions = n_returns < 2
    mean_return = math.fsum(returns) / n_returns if n_returns else None
    if insufficient_sessions:
        std_return = None
    elif len(set(returns)) == 1:
        # A rounded mean would turn identical returns into a tiny nonzero deviation.
        std_return = 0.0
    else:
        std_return = math.sqrt(
            math.fsum((value - mean_return) ** 2 for value in returns)
            / (n_returns - 1)
        )
    zero_volatility = std_return == 0
    annualized_volatility = (
        std_return * math.sqrt(sessions_per_year)
        if std_return is not None
        else None
    )
    sharpe = (
        mean_return / std_return * math.sqrt(sessions_per_year)
        if mean_return is not None and std_return not in (None, 0)
        else None
    )

    if sessions:
        nav_end = sessions[-1]["nav"]
        total_return = nav_end - 1
        annualized_return = (
            nav_end ** (sessions_per_year / n_returns) - 1
            if n_returns
            else None
        )
        peak_nav = 1.0
        peak_session = sessions[0]["session_date"]
        max_drawdown = 0.0
        drawdown_peak_session = None
        drawdown_trough_session = None
        for row in sessions:
            if row["nav"] > peak_nav:
                peak_nav = row["nav"]
                peak_session = row["session_date"]
            current_drawdown = row["nav"] / peak_nav - 1
            if current_drawdown < max_drawdown:
                max_drawdown = current_drawdown
                drawdown_peak_session = peak_session
                drawdown_trough_session = row["session_date"]
    else:
        total_return = None
        annualized_return = None
        max_drawdown = None
        drawdown_peak_session = None
        drawdown_trough_session = None

    return {
        "total_return": total_return,
        "annualized_return": annualized_return,
        "n_returns": n_returns,
        "mean_return": mean_return,
        "std_return": std_return,
        "annualized_volatility": annualized_volatility,
        "sharpe": sharpe,
        "max_drawdown": max_drawdown,
        "drawdown_peak_session": drawdown_peak_session,
        "drawdown_trough_session": drawdown_trough_session,
        "total_turnover": math.fsum(row["turnover"] for row in sessions),
        "total_cost": math.fsum(row["cost"] for row in sessions),
        "executed_count": sum(
            event["kind"] == "executed" for event in vector["executions"]
        ),
        "deferred_count": sum(
            event["kind"] == "deferred" for event in vector["executions"]
        ),
        "skipped_buy_count": sum(
            len(event["skipped_buys"]) for event in vector["executions"]
        ),
        "session_count": session_count,
        "pending_at_end": vector["pending_at_end"] is not None,
        "insufficient_sessions": insufficient_sessions,
        "zero_volatility": zero_volatility,
        "assumptions": vector["assumptions"],
        "availability_assumption": vector["availability_assumption"],
    }


def build_output(fixture_bytes, trend_window=None, strategy_kind="top_k_rank"):
    if strategy_kind not in ("top_k_rank", "buy_and_hold", "ma_crossover"):
        raise ValueError("unsupported strategy")
    fixture = json.loads(fixture_bytes)
    calendar, bars_by_symbol, parameters, configured_trend = build_inputs(fixture)
    if trend_window is not None:
        trend_window = positive_integer(trend_window, "--trend")
    elif configured_trend:
        raise ValueError("strategy.trend_filter requires an explicit --trend window")

    parameters["trend_filter"] = trend_window is not None
    parameters["trend_window"] = trend_window

    factor_names = {
        f"momentum({parameters['momentum_short_days']})",
        f"momentum({parameters['momentum_long_days']})",
        f"volatility({parameters['volatility_window']})",
        "rotation_score",
    }
    if trend_window is not None:
        factor_names.add(f"trend_filter({trend_window})")

    factors = {name: [] for name in sorted(factor_names)}
    for symbol in sorted(bars_by_symbol):
        instrument_id = f"{symbol}.SYNTH"
        bars = bars_by_symbol[symbol]
        for session_index, session_date in enumerate(calendar):
            momentum_values = {}
            for window in sorted(
                {
                    parameters["momentum_short_days"],
                    parameters["momentum_long_days"],
                }
            ):
                value, available_at, status = evaluate_window(
                    bars, session_index, "momentum", window
                )
                factor_name = f"momentum({window})"
                momentum_values[window] = (value, available_at, status)
                factors[factor_name].append(
                    {
                        "instrument_id": instrument_id,
                        "session_date": session_date,
                        **factor_result(value, available_at, status),
                    }
                )

            volatility_window = parameters["volatility_window"]
            volatility_value = evaluate_window(
                bars, session_index, "volatility", volatility_window
            )
            factors[f"volatility({volatility_window})"].append(
                {
                    "instrument_id": instrument_id,
                    "session_date": session_date,
                    **factor_result(*volatility_value),
                }
            )

            trend_value = None
            if trend_window is not None:
                trend_value = evaluate_window(
                    bars, session_index, "trend_filter", trend_window
                )
                factors[f"trend_filter({trend_window})"].append(
                    {
                        "instrument_id": instrument_id,
                        "session_date": session_date,
                        **factor_result(*trend_value),
                    }
                )

            short_value = momentum_values[parameters["momentum_short_days"]]
            long_value = momentum_values[parameters["momentum_long_days"]]
            dependencies = [short_value, long_value, volatility_value]
            if trend_value is not None:
                dependencies.append(trend_value)
            dependency_statuses = {dependency[2] for dependency in dependencies}
            rotation_status = next(
                (
                    status
                    for status in STATUS_PRECEDENCE
                    if status in dependency_statuses
                ),
                "ok",
            )
            if rotation_status != "ok":
                rotation_value = None
                rotation_available_at = None
            else:
                rotation_available_at = max(
                    dependency[1] for dependency in dependencies
                )
                if trend_value is not None and trend_value[0] == 0:
                    rotation_status = "filtered"
                    rotation_value = None
                else:
                    rotation_value = (
                        parameters["short_momentum_weight"] * short_value[0]
                        + parameters["long_momentum_weight"] * long_value[0]
                        - parameters["volatility_weight"] * volatility_value[0]
                    )

            factors["rotation_score"].append(
                {
                    "instrument_id": instrument_id,
                    "session_date": session_date,
                    **factor_result(
                        rotation_value, rotation_available_at, rotation_status
                    ),
                }
            )

    if strategy_kind == "ma_crossover":
        rows = []
        for symbol, bars in sorted(bars_by_symbol.items()):
            for index, date in enumerate(calendar):
                value, time, status = None, None, "insufficient_window"
                if index >= 59:
                    window = bars[index - 59:index + 1]
                    if any(bar is None for bar in window):
                        status = "missing_input"
                    elif any(bar["available_at"] is None for bar in window):
                        status = "unknown_availability"
                    else:
                        status = "ok"
                        value = math.fsum(bar["close"] for bar in window[-20:]) / 20 - math.fsum(bar["close"] for bar in window) / 60
                        time = max(bar["available_at"] for bar in window)
                rows.append({"instrument_id": f"{symbol}.SYNTH", "session_date": date,
                             **factor_result(value, time, status)})
        factors = {"ma_gap(20,60)": rows}
        parameters = {"strategy": strategy_kind, "params_version": 1, "short_window": 20, "long_window": 60}
    elif strategy_kind == "buy_and_hold":
        factors = {}
        parameters = {"strategy": strategy_kind, "params_version": 1, "unfilled_entry": "retry"}
    vector = build_vector_result(fixture, calendar, bars_by_symbol, factors, strategy_kind)
    sessions_per_year = 252
    return {
        "fixture_sha256": hashlib.sha256(fixture_bytes).hexdigest(),
        "parameters": parameters,
        "tolerance": {"factor_abs": 1e-12, "nav_abs": 1e-10},
        "factors": factors,
        "vector": vector,
        "summary": build_summary(vector, sessions_per_year),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--trend", type=int)
    parser.add_argument("--strategy", choices=("top_k_rank", "buy_and_hold", "ma_crossover"), default="top_k_rank")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    fixture_bytes = args.fixture.read_bytes()
    output = build_output(fixture_bytes, args.trend, args.strategy)
    args.out.write_text(
        json.dumps(output, allow_nan=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
