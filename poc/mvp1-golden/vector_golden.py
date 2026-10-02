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


def build_output(fixture_bytes, trend_window=None):
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

    return {
        "fixture_sha256": hashlib.sha256(fixture_bytes).hexdigest(),
        "parameters": parameters,
        "tolerance": {"factor_abs": 1e-12},
        "factors": factors,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--trend", type=int)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    fixture_bytes = args.fixture.read_bytes()
    output = build_output(fixture_bytes, args.trend)
    args.out.write_text(
        json.dumps(output, allow_nan=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
