#!/usr/bin/env python3
"""Independent stdlib oracle for spec #112 full_target (not a production engine)."""

import argparse
from datetime import date, datetime, timedelta
import hashlib
import json
import math
from pathlib import Path
import statistics


def number(value, name, positive=False):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be numeric")
    value = float(value)
    if not math.isfinite(value) or value < 0 or (positive and value == 0):
        raise ValueError(f"{name} must be finite and {'positive' if positive else 'nonnegative'}")
    return value


def timestamp(day, time):
    if time is None:
        return None
    result = datetime.fromisoformat(f"{day}T{time}")
    if result.utcoffset() is None:
        raise ValueError("availability timestamps need an explicit offset")
    return result


def expand_ma(fixture):
    if "calendar" in fixture:
        return fixture
    counts = [integer(fixture[key], key) for key in ("flat_sessions", "high_sessions", "low_sessions")]
    calendar = []
    day = date.fromisoformat(fixture["start_date"])
    while len(calendar) < sum(counts):
        if day.weekday() < 5:
            calendar.append(day.isoformat())
        day += timedelta(days=1)
    flat, high, low = counts
    return {
        "calendar": calendar,
        "bar_defaults": {
            "open": fixture["open"], "open_available_at": "09:30:00+08:00",
            "close_available_at": "15:00:00+08:00",
        },
        "instruments": [
            {"symbol": symbol, "lot_size": fixture["lot_size"], "closes":
             [fixture["flat_close"]] * flat + [fixture["high_close"]] * high + [fixture["low_close"]] * low
             if symbol == fixture["rising_symbol"] else [fixture["flat_close"]] * len(calendar)}
            for symbol in fixture["symbols"]
        ],
        "execution_status_default": {
            "trade_status": "TRADABLE", "is_tradable": True, "available_at": "08:50:00+08:00",
        },
        "account": {"initial_cash": fixture["initial_cash"]},
        "costs": {
            "commission_rate": fixture["commission_rate"],
            "minimum_commission": fixture["minimum_commission"],
            "buy_tax_rate": 0.0, "sell_tax_rate": 0.0,
            "buy_slippage_bps": fixture["slippage_bps"],
            "sell_slippage_bps": fixture["slippage_bps"],
        },
    }


def inputs(fixture):
    calendar = fixture["calendar"]
    dates = [date.fromisoformat(day) for day in calendar]
    if not dates or dates != sorted(set(dates)):
        raise ValueError("calendar must contain strictly increasing ISO Session dates")
    defaults = fixture["bar_defaults"]
    opens_at = [timestamp(day, defaults["open_available_at"]) for day in calendar]
    if any(value is None for value in opens_at):
        raise ValueError("open availability cannot be unknown")
    closes_at = [timestamp(day, "15:00:00+08:00") for day in calendar]
    missing = {(row["symbol"], row["date"]) for row in fixture.get("missing_bars", [])}
    bars = {}
    lots = {}
    for item in fixture["instruments"]:
        instrument = f"{item['symbol']}.SYNTH"
        if instrument in bars:
            raise ValueError(f"duplicate Instrument: {instrument}")
        lots[instrument] = number(
            item.get("lot_size", fixture.get("account", {}).get("lot_size")),
            f"{instrument}.lot_size", positive=True,
        )
        opens = item.get("opens", [defaults["open"]] * len(calendar))
        closes = item["closes"]
        if len(opens) != len(calendar) or len(closes) != len(calendar):
            raise ValueError("open/close arrays must match calendar")
        bars[instrument] = []
        for index, day in enumerate(calendar):
            absent = (item["symbol"], day) in missing or closes[index] is None
            bars[instrument].append({
                "open": None if absent or opens[index] is None else number(opens[index], "open", True),
                "close": None if absent else number(closes[index], "close", True),
                "available_at": timestamp(day, defaults["close_available_at"]),
            })
    if not bars:
        raise ValueError("Universe cannot be empty")
    ids = sorted(bars)
    valid_keys = {(instrument.removesuffix(".SYNTH"), day) for instrument in ids for day in calendar}
    if not missing <= valid_keys:
        raise ValueError("missing bar references unknown Instrument or Session")
    statuses = {}
    default_status = fixture.get("execution_status_default")
    if default_status is not None:
        statuses = {key: default_status for key in valid_keys}
    seen = set()
    for row in fixture.get("execution_status_overrides", []):
        key = (row["symbol"], row["date"])
        if key not in valid_keys or key in seen:
            raise ValueError("unknown or duplicate execution status override")
        seen.add(key)
        statuses[key] = row
    gates = []
    for index, day in enumerate(calendar):
        gate = {}
        for instrument in ids:
            status = statuses.get((instrument.removesuffix(".SYNTH"), day))
            if bars[instrument][index]["open"] is None:
                reason = "missing_open"
            elif status is None:
                reason = "unknown_execution_status"
            elif status["trade_status"] != "TRADABLE" or status["is_tradable"] is not True:
                reason = status["trade_status"].lower()
            else:
                available = timestamp(day, status["available_at"])
                reason = (
                    "unknown_status_availability" if available is None else
                    "late_status" if available > opens_at[index] else None
                )
            gate[instrument] = reason
        gates.append(gate)
    costs = {
        key: number(fixture["costs"][key], f"costs.{key}")
        for key in (
            "commission_rate", "minimum_commission", "buy_tax_rate", "sell_tax_rate",
            "buy_slippage_bps", "sell_slippage_bps",
        )
    }
    if costs["sell_slippage_bps"] >= 10000:
        raise ValueError("sell slippage must leave a positive Fill price")
    initial = number(fixture["account"]["initial_cash"], "initial_cash", True)
    return calendar, bars, lots, gates, costs, initial, closes_at


def lot_fill(instrument, side, quantity, raw, costs):
    slippage_rate = costs[f"{side}_slippage_bps"] / 10000
    price = raw * (1 + slippage_rate if side == "buy" else 1 - slippage_rate)
    notional = quantity * price
    proportional = notional * costs["commission_rate"]
    commission = max(proportional, costs["minimum_commission"])
    tax = notional * costs[f"{side}_tax_rate"]
    cash_change = (-notional if side == "buy" else notional) - commission - tax
    return {
        "instrument_id": instrument, "side": side, "quantity": quantity,
        "raw_open": raw, "price": price, "notional": notional,
        "fee_basis": notional, "commission": commission,
        "minimum_commission_top_up": commission - proportional,
        "tax": tax, "slippage": abs(price - raw) * quantity,
        "cash_change": cash_change,
    }


NO_TRADE_WEIGHT = 1e-12


def parity_retry_fills(quantities, targets, prices, equity, cash, costs):
    # ADR 0019 #116: pre-fee budget, fees paid from remaining cash, cut by ID order.
    rate = costs["commission_rate"] + costs["buy_slippage_bps"] / 10000 + costs["buy_tax_rate"]
    held = math.fsum(quantity * prices[i] for i, quantity in quantities.items() if quantity > 0)
    cash_weight = max(1 - held / equity, 0.0) if equity > 0 else 0.0
    cash_weight = min(cash_weight, max(cash, 0.0) / equity)
    fills = []
    for instrument in sorted(targets):
        buy = min(targets[instrument], cash_weight / (1 + rate))
        cash_weight = max(cash_weight - buy * (1 + rate), 0.0)
        if buy <= 0:
            continue
        basis = equity * buy
        fees = {
            "fee_basis": basis, "fee_side": "buy",
            "commission": basis * costs["commission_rate"],
            "slippage": basis * costs["buy_slippage_bps"] / 10000,
            "tax": basis * costs["buy_tax_rate"],
        }
        raw = prices[instrument]
        fills.append({
            "instrument_id": instrument, "side": "buy", "quantity": basis / raw,
            "raw_open": raw, "price": raw, "notional": basis,
            "minimum_commission_top_up": 0.0, **fees,
            "cash_change": -basis - fees["commission"] - fees["slippage"] - fees["tax"],
        })
    return fills


def parity_fills(quantities, targets, prices, equity, costs):
    weights = {
        instrument: quantity * prices[instrument] / equity
        for instrument, quantity in quantities.items() if quantity > 0
    }
    bases = {}
    for instrument in sorted(set(weights) | set(targets)):
        delta = targets.get(instrument, 0) - weights.get(instrument, 0)
        side = "buy" if delta >= 0 else "sell"
        basis = 0.0 if abs(delta) <= NO_TRADE_WEIGHT else equity * abs(delta)
        bases[instrument] = {
            "fee_basis": basis, "fee_side": side,
            "commission": basis * costs["commission_rate"],
            "slippage": basis * costs[f"{side}_slippage_bps"] / 10000,
            "tax": basis * costs[f"{side}_tax_rate"],
        }
    cost = math.fsum(row["commission"] + row["slippage"] + row["tax"] for row in bases.values())
    after = equity - cost
    if after <= 0:
        raise ValueError("costs exhaust pre-open equity")
    fills = []
    for instrument, fees in bases.items():
        target = targets.get(instrument, 0)
        desired = target * after / prices[instrument] if target else 0.0
        delta = desired - quantities[instrument]
        side = "buy" if delta >= 0 else "sell"
        quantity = abs(delta)
        raw = prices[instrument]
        # Zero fee basis still requires resizing to E1 when other legs incur costs.
        if fees["fee_basis"] == 0 and quantity * raw / equity <= NO_TRADE_WEIGHT:
            continue
        fills.append({
            "instrument_id": instrument, "side": side, "quantity": quantity,
            "raw_open": raw, "price": raw, "notional": quantity * raw,
            "minimum_commission_top_up": 0.0, **fees,
            "cash_change": -delta * raw - fees["commission"] - fees["slippage"] - fees["tax"],
        })
    return sorted(fills, key=lambda row: (row["side"] != "sell", row["instrument_id"]))


def integer(value, name, minimum=1):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")
    return value


def s2_score(rows, index, config, cutoff):
    short = integer(config["momentum_short_days"], "momentum_short_days")
    long = integer(config["momentum_long_days"], "momentum_long_days")
    window = integer(config["volatility_window"], "volatility_window", 2)
    if index < max(short, long, window):
        return None
    required = {index, index - short, index - long} | set(range(index - window, index + 1))
    if config["trend_filter"]:
        integer(config["trend_window"], "trend_window")
        if index + 1 < config["trend_window"]:
            return None
        required |= set(range(index - config["trend_window"] + 1, index + 1))
    if any(rows[i]["close"] is None or rows[i]["available_at"] is None
           or rows[i]["available_at"] > cutoff for i in required):
        return None
    close = rows[index]["close"]
    if config["trend_filter"]:
        mean = statistics.mean(rows[i]["close"] for i in range(index - config["trend_window"] + 1, index + 1))
        if close < mean:
            return None
    returns = [rows[i]["close"] / rows[i - 1]["close"] - 1 for i in range(index - window + 1, index + 1)]
    return (
        (close / rows[index - short]["close"] - 1) * config["short_momentum_weight"]
        + (close / rows[index - long]["close"] - 1) * config["long_momentum_weight"]
        - statistics.stdev(returns) * config["volatility_weight"]
    )


def run(fixture, strategy, sizing, unfilled_entry):
    fixture = expand_ma(fixture)
    calendar, bars, lots, gates, costs, initial, closes_at = inputs(fixture)
    if strategy == "s2":
        config = fixture["strategy"]
        for key in ("momentum_short_days", "momentum_long_days", "rebalance_every", "top_n"):
            integer(config[key], key)
        integer(config["volatility_window"], "volatility_window", 2)
        if not isinstance(config["trend_filter"], bool):
            raise ValueError("trend_filter must be boolean")
        if config["trend_filter"]:
            integer(config["trend_window"], "trend_window")
        for key in ("short_momentum_weight", "long_momentum_weight", "volatility_weight"):
            value = config[key]
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
                raise ValueError(f"{key} must be finite")
    ids = sorted(bars)
    cash = initial
    quantities = {instrument: 0.0 for instrument in ids}
    last_marks = {"open": {}, "close": {}}
    pending = None
    first_s2 = None
    previous_gaps = {instrument: None for instrument in ids}
    ma_held = set()
    result = {"sessions": [], "decisions": [], "executions": [], "orders": [], "fills": []}

    def marks(index, point):
        prices = {}
        sources = {}
        for instrument in ids:
            raw = bars[instrument][index][point]
            if raw is not None:
                last_marks[point][instrument] = (raw, calendar[index])
            previous = last_marks[point].get(instrument)
            if previous is not None:
                prices[instrument] = previous[0]
                sources[instrument] = {
                    "kind": "raw" if raw is not None else "carried",
                    "session_date": previous[1], "field": point,
                }
            elif quantities[instrument] > 0:
                raise ValueError(f"held Instrument lacks a {point} valuation: {instrument}")
        return prices, sources

    def valuation(prices, sources):
        holdings = {
            instrument: {
                "quantity": quantities[instrument], "price": prices[instrument],
                "value": quantities[instrument] * prices[instrument],
                "price_source": sources[instrument],
            }
            for instrument in ids if quantities[instrument] > 0
        }
        equity = cash + math.fsum(row["value"] for row in holdings.values())
        if cash < -1e-6 or any(value < 0 for value in quantities.values()):
            raise ValueError("long-only unlevered ledger violated")
        return {"cash": cash, "holdings": holdings, "equity": equity, "nav": equity / initial}

    def order(index, instrument, side, quantity, status, reason=None):
        row = {
            "order_id": f"order-{len(result['orders']) + 1}",
            "session_date": calendar[index], "decision_session": pending["decision_session"],
            "instrument_id": instrument, "side": side, "quantity": quantity,
            "status": status, "reason": reason, "vp_id": "vp-1",
        }
        result["orders"].append(row)
        return row

    def apply(index, fill):
        nonlocal cash
        instrument = fill["instrument_id"]
        row = order(index, instrument, fill["side"], fill["quantity"], "filled")
        fill.update({
            "order_id": row["order_id"], "session_date": calendar[index], "vp_id": "vp-1",
            "cash_fees": fill["commission"] + fill["tax"] + (
                fill["slippage"] if sizing == "vector_parity" else 0
            ),
            "total_cost": fill["commission"] + fill["tax"] + fill["slippage"],
        })
        quantities[instrument] += fill["quantity"] * (1 if fill["side"] == "buy" else -1)
        cash += fill["cash_change"]
        result["fills"].append(fill)

    for index, day in enumerate(calendar):
        prices, sources = marks(index, "open")
        before = valuation(prices, sources)["equity"]
        if pending is not None:
            targets = pending["targets"]
            blocked = [instrument for instrument in ids if quantities[instrument] > 0 and gates[index][instrument]]
            attempt = {
                "session_date": day, "decision_session": pending["decision_session"],
                "kind": "deferred" if blocked else "executed", "blocked": blocked,
                "skipped_buys": [], "retry_only": pending.get("retry_only", False),
            }
            result["executions"].append(attempt)
            if blocked:
                for instrument in blocked:
                    order(index, instrument, "rebalance", None, "blocked", gates[index][instrument])
            else:
                skipped = [instrument for instrument in targets if gates[index][instrument]]
                attempt["skipped_buys"] = sorted(skipped)
                for instrument in sorted(skipped):
                    order(index, instrument, "buy", None, "blocked", gates[index][instrument])
                effective = {instrument: weight for instrument, weight in targets.items() if instrument not in skipped}
                if sizing == "vector_parity":
                    if pending.get("retry_only"):
                        fills = parity_retry_fills(quantities, effective, prices, before, cash, costs)
                    else:
                        fills = parity_fills(quantities, effective, prices, before, costs)
                    for fill in fills:
                        if cash + fill["cash_change"] < -1e-6:
                            raise ValueError("vector_parity buy exceeds available cash")
                        apply(index, fill)
                else:
                    desired = {
                        instrument: math.floor(
                            effective.get(instrument, 0) * before / prices[instrument] / lots[instrument]
                        ) * lots[instrument]
                        for instrument in ids if instrument in prices
                    }
                    if pending.get("retry_only"):
                        desired = {instrument: quantities[instrument] for instrument in ids} | {
                            instrument: desired[instrument] for instrument in effective
                        }
                    for side in ("sell", "buy"):
                        for instrument in ids:
                            delta = desired.get(instrument, 0) - quantities[instrument]
                            if (side == "buy" and delta <= 0) or (side == "sell" and delta >= 0):
                                continue
                            quantity = abs(delta)
                            fill = lot_fill(instrument, side, quantity, prices[instrument], costs)
                            if side == "buy":
                                while quantity > 0 and cash + fill["cash_change"] < 0:
                                    quantity -= lots[instrument]
                                    if quantity > 0:
                                        fill = lot_fill(instrument, side, quantity, prices[instrument], costs)
                                if quantity == 0:
                                    order(index, instrument, side, 0, "unfilled", "insufficient_cash")
                                    continue
                            apply(index, fill)
                pending = (
                    {"decision_session": pending["decision_session"],
                     "targets": {instrument: targets[instrument] for instrument in skipped},
                     "retry_only": True}
                    if skipped and unfilled_entry == "retry" else None
                )
        open_value = valuation(prices, sources)
        close_prices, close_sources = marks(index, "close")
        close_value = valuation(close_prices, close_sources)
        result["sessions"].append({"session_date": day, "open": open_value, "close": close_value})
        if strategy == "s1" and index == 0:
            decision = {"session_date": day, "targets": {instrument: 1 / len(ids) for instrument in ids}}
            result["decisions"].append(decision)
            pending = {"decision_session": day, "targets": decision["targets"], "retry_only": False}
        elif strategy == "s2":
            config = fixture["strategy"]
            ranked = []
            for instrument in ids:
                score = s2_score(bars[instrument], index, config, closes_at[index])
                if score is not None:
                    ranked.append((instrument, score))
            ranked.sort(key=lambda row: (-row[1], row[0]))
            if ranked and first_s2 is None:
                first_s2 = index
            interval = integer(config["rebalance_every"], "rebalance_every")
            if first_s2 is not None and (index - first_s2) % interval == 0:
                selected = ranked[:integer(config["top_n"], "top_n")]
                targets = {instrument: 1 / len(selected) for instrument, _ in selected}
                decision = {
                    "session_date": day, "targets": targets,
                    "ranked": [{"instrument_id": instrument, "score": score, "rank": rank}
                               for rank, (instrument, score) in enumerate(ranked, 1)],
                }
                result["decisions"].append(decision)
                pending = {"decision_session": day, "targets": targets, "retry_only": False}
        elif strategy == "s3":
            changed = False
            diagnostics = {}
            for instrument in ids:
                rows = bars[instrument]
                window = rows[max(0, index - 59):index + 1]
                status = (
                    "insufficient_window" if index < 59 else
                    "missing_input" if any(row["close"] is None for row in window) else
                    "unknown_availability" if any(row["available_at"] is None for row in window) else
                    "late_availability" if any(row["available_at"] > closes_at[index] for row in window) else "ok"
                )
                gap = None
                if status == "ok":
                    gap = statistics.mean(row["close"] for row in window[-20:]) - statistics.mean(row["close"] for row in window)
                    previous = previous_gaps[instrument]
                    if previous is not None:
                        if previous <= 0 and gap > 0 and instrument not in ma_held:
                            ma_held.add(instrument)
                            changed = True
                        elif previous >= 0 and gap < 0 and instrument in ma_held:
                            ma_held.remove(instrument)
                            changed = True
                previous_gaps[instrument] = gap
                diagnostics[instrument] = {"status": status, "gap": gap}
            result["sessions"][-1]["ma20_60"] = diagnostics
            if changed:
                targets = {instrument: 1 / len(ids) for instrument in sorted(ma_held)}
                result["decisions"].append({"session_date": day, "targets": targets})
                pending = {"decision_session": day, "targets": targets, "retry_only": False}
    result["pending_at_end"] = pending
    result["parameters"] = {
        "strategy": strategy, "sizing": sizing, "rebalance": "full_target",
        "unfilled_entry": unfilled_entry, "costs": costs, "lot_sizes": lots,
        "initial_cash": initial,
        "strategy_parameters": (
            fixture["strategy"] if strategy == "s2" else
            {"short_window": 20, "long_window": 60, "decision_rule": "target_set_changes"}
            if strategy == "s3" else {"decision_rule": "first_session_only"}
        ),
        "capital_allocation": {"vp-1": initial}, "unallocated_capital": 0.0,
    }
    result["tolerance"] = {"cash_equity_fees_abs_cny": 1e-6, "normalized_nav_rel": 1e-9}
    result["assumptions"] = [
        "independent Python float64 oracle; Rust scale-18 ledger remains canonical",
        "single Venue, single Trading Account, one VP; no leverage or partial fills",
        "session close signals, next Session open execution; new decisions replace pending",
        "conservative deferral if any held Instrument is not executable",
        "raw open/close valuation even for HALTED/UNKNOWN valid bars; missing prices carry each field separately",
        "Static Universe, synthetic availability; not PIT or real-market evidence",
        "no T+1, price limits, currency-unit rounding, dividends, interest or funding",
        "vector_parity retry uses pre-fee equity budgets, fees paid from cash; "
        "cash-insufficient budgets cut in ascending instrument_id order",
    ]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--strategy", choices=("s1", "s2", "s3"), required=True)
    parser.add_argument("--sizing", choices=("lot", "vector_parity"), required=True)
    parser.add_argument("--unfilled-entry", choices=("skip", "retry"), default=None)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    raw = args.fixture.read_bytes()
    result = run(
        json.loads(raw), args.strategy, args.sizing,
        args.unfilled_entry or ("retry" if args.strategy == "s1" else "skip"),
    )
    result["schema_version"] = "mvp3.full_target.golden@1"
    result["fixture_sha256"] = hashlib.sha256(raw).hexdigest()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
