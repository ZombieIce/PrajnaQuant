#!/usr/bin/env python3
"""Run or verify the local-only MVP-6 engineering acceptance artifacts."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation, ROUND_FLOOR
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Any


SCHEMA = "mvp6-acceptance-artifacts@1"
EARLY_FAILURE_SCHEMA = "mvp6-early-failure@1"
PHASE_FAILURE_SCHEMA = "mvp6-post-main-failure@1"
FIXED_UNIVERSE = {
    "version": "mvp6-fixed-etf@3",
    "codes": ["159919", "159560", "159522", "159612", "159952"],
    "minimum_code_count": 3,
}
PARAMETERS = {"lookback": 60, "top_k": 3, "rebalance_every": 5}
LOT = {
    "currency": "CNY",
    "initial_cash": "1000000",
    "sizing": "lot",
    "rebalance": "full_target",
    "minimum_commission": "5",
    "portfolios": [{"id": 1, "capital": "1000000"}],
    "lot_size_source": {"kind": "instrument"},
}
COSTS = {
    "commission_rate": 0.0003,
    "buy_slippage_bps": 2.0,
    "sell_slippage_bps": 2.0,
    "buy_tax_rate": 0.0,
    "sell_tax_rate": 0.0,
}
TERMINAL = {"semantic_version": "mvp6-terminal-settlement@1"}
TOLERANCE = 1e-12
WEIGHT_TOLERANCE = Decimal("1e-12")
UNKNOWN_LIMITS = {
    "total_unknown_percent": Decimal("5"),
    "per_instrument_unknown_percent": Decimal("20"),
}
ACTIVE_POST_MAIN_RUN: dict[str, Any] | None = None


def dec(value: Any) -> Decimal:
    return Decimal(str(value))


def result(passed: bool, **details: Any) -> dict[str, Any]:
    return {"passed": bool(passed), "status": "passed" if passed else "failed", **details}


def mark_run_phase(criterion: str) -> None:
    if ACTIVE_POST_MAIN_RUN is not None:
        ACTIVE_POST_MAIN_RUN["criterion"] = criterion


def build_pit_input(pit_base: dict[str, Any], frozen_opening: dict[str, Any]) -> dict[str, Any]:
    """Build the identity-bearing MVP-6 inputs from the frozen local evidence."""
    return {
        **pit_base,
        "market": frozen_opening,
        "terminal_settlement": dict(TERMINAL),
        "fixed_universe": {**FIXED_UNIVERSE, "codes": list(FIXED_UNIVERSE["codes"])},
    }


def normalize_unknown_coverage(coverage: dict[str, Any]) -> dict[str, Any]:
    instruments: dict[str, dict[str, Any]] = {}
    for item in coverage.get("instruments", []):
        symbol = item.get("symbol", item.get("native_symbol"))
        if symbol is None:
            symbol = ""
        totals = instruments.setdefault(symbol, {"symbol": symbol, "cells": 0, "unknown_cells": 0})
        if type(item.get("cells")) is int:
            totals["cells"] += item["cells"]
        if type(item.get("unknown_cells")) is int:
            totals["unknown_cells"] += item["unknown_cells"]
    return {
        "engineering_acceptance": coverage.get("engineering_acceptance"),
        "unknown_cells": coverage.get("unknown_cells"),
        "denominator_cells": coverage.get("denominator_cells"),
        "thresholds": coverage.get("thresholds", {}),
        "instruments": list(instruments.values()),
    }


@dataclass
class RawHashSnapshot:
    checked: dict[str, bool]
    request_records: int
    inventory_error: str | None = None
    inventory_sha256: str | None = None


def raw_snapshot_valid(snapshot: RawHashSnapshot | dict[str, bool]) -> bool:
    if isinstance(snapshot, RawHashSnapshot):
        return (
            snapshot.inventory_error is None
            and snapshot.request_records > 0
            and bool(snapshot.checked)
            and all(snapshot.checked.values())
        )
    return bool(snapshot) and all(snapshot.values())


def raw_integrity_failure(checked: RawHashSnapshot | dict[str, bool]) -> str | None:
    if isinstance(checked, RawHashSnapshot):
        if checked.inventory_error or checked.request_records <= 0 or not checked.checked:
            return "frozen Raw integrity failed: empty or malformed inventory"
        values = checked.checked
    else:
        if not checked:
            return "frozen Raw integrity failed: empty or malformed inventory"
        values = checked
    invalid = sum(not valid for valid in values.values())
    return f"frozen Raw integrity failed: {invalid} object missing or mismatched" if invalid else None


def raw_snapshot_unchanged(before: RawHashSnapshot, after: RawHashSnapshot) -> bool:
    return before == after and raw_snapshot_valid(before) and raw_snapshot_valid(after)


def verify_unknown_coverage(coverage: dict[str, Any]) -> dict[str, Any]:
    errors: list[dict[str, Any]] = []
    aggregate_unknown = coverage.get("unknown_cells")
    aggregate_cells = coverage.get("denominator_cells")
    thresholds = coverage.get("thresholds", {})
    total_limit = thresholds.get("total_unknown_percent")
    instrument_limit = thresholds.get("per_instrument_unknown_percent")
    counts: dict[str, list[int]] = {}
    for item in coverage.get("instruments", []):
        symbol = item.get("symbol")
        cells, unknown = item.get("cells"), item.get("unknown_cells")
        if not isinstance(symbol, str) or not symbol or type(cells) is not int or type(unknown) is not int:
            errors.append({"kind": "invalid_instrument_coverage"})
            continue
        if cells <= 0 or unknown < 0 or unknown > cells:
            errors.append({"kind": "invalid_instrument_denominator", "symbol": symbol})
            continue
        totals = counts.setdefault(symbol, [0, 0])
        totals[0] += cells
        totals[1] += unknown

    valid_aggregate = (
        type(aggregate_unknown) is int and type(aggregate_cells) is int
        and aggregate_cells > 0 and aggregate_unknown >= 0 and aggregate_unknown <= aggregate_cells
    )
    if not valid_aggregate:
        errors.append({"kind": "invalid_aggregate_denominator"})
    if set(counts) != set(FIXED_UNIVERSE["codes"]):
        errors.append({"kind": "fixed_universe_coverage_mismatch"})
    per_instrument = []
    for symbol in FIXED_UNIVERSE["codes"]:
        cells, unknown = counts.get(symbol, [0, 0])
        percent = Decimal(100) * unknown / cells if cells > 0 else None
        per_instrument.append({
            "symbol": symbol, "cells": cells, "unknown_cells": unknown,
            "percent": float(percent) if percent is not None else None,
        })
    if valid_aggregate and (
        sum(item[0] for item in counts.values()) != aggregate_cells
        or sum(item[1] for item in counts.values()) != aggregate_unknown
    ):
        errors.append({"kind": "aggregate_instrument_reconciliation"})
    try:
        total_limit_dec = dec(total_limit)
        instrument_limit_dec = dec(instrument_limit)
        if not total_limit_dec.is_finite() or not instrument_limit_dec.is_finite():
            raise InvalidOperation
    except (InvalidOperation, TypeError, ValueError):
        total_limit_dec = instrument_limit_dec = Decimal(-1)
        errors.append({"kind": "invalid_threshold"})
    if (total_limit_dec < 0 or instrument_limit_dec < 0
            or total_limit_dec > UNKNOWN_LIMITS["total_unknown_percent"]
            or instrument_limit_dec > UNKNOWN_LIMITS["per_instrument_unknown_percent"]):
        errors.append({"kind": "invalid_threshold"})
    total_limit_dec = min(total_limit_dec, UNKNOWN_LIMITS["total_unknown_percent"])
    instrument_limit_dec = min(
        instrument_limit_dec, UNKNOWN_LIMITS["per_instrument_unknown_percent"],
    )
    total_percent = (
        Decimal(100) * aggregate_unknown / aggregate_cells if valid_aggregate else None
    )
    if total_percent is not None and total_percent > total_limit_dec:
        errors.append({"kind": "total_unknown_percent_over_limit"})
    if any(item["percent"] is None or Decimal(str(item["percent"])) > instrument_limit_dec
           for item in per_instrument):
        errors.append({"kind": "per_instrument_unknown_percent_over_limit"})
    if coverage.get("engineering_acceptance") != "passed":
        errors.append({"kind": "coverage_report_not_accepted"})
    return result(
        not errors,
        total_percent=float(total_percent) if total_percent is not None else None,
        per_instrument=per_instrument,
        errors=errors,
    )


def verify_early_failure(data: dict[str, Any]) -> dict[str, Any]:
    failure = data["failure"]
    status = failure.get("status")
    if status not in {"failed", "not_run"}:
        raise ValueError("early failure status must be failed or not_run")
    main = result(
        False,
        status=status,
        stage=failure.get("stage"),
        error=failure.get("error", "early run did not complete"),
        fixed_universe=failure.get("fixed_universe"),
        identity_change=failure.get("identity_change"),
    )
    reason = "The main Run did not complete, so this criterion was not executed."
    checks = {
        "main_run": main,
        "daily_parity": result(False, status="not_run", reason=reason),
        "reproducibility": result(False, status="not_run", reason=reason),
        "python_as_of_and_cli": result(False, status="not_run", reason=reason),
        "daily_accounting": result(False, status="not_run", reason=reason),
        "unknown_floor": verify_unknown_coverage(normalize_unknown_coverage(data["unknown_coverage"])),
        "frozen_raw_immutable": result(data.get("raw_immutable") is True),
    }
    return {
        "schema": "mvp6-acceptance-verification@1",
        "passed": all(item["passed"] for item in checks.values()),
        "checks": checks,
    }


def verify_post_main_failure(data: dict[str, Any]) -> dict[str, Any]:
    failure = data["failure"]
    reason = "This criterion was not completed because post-main evidence assembly stopped."
    checks = {
        name: result(False, status="not_run", reason=reason)
        for name in (
            "main_run", "daily_parity", "reproducibility", "python_as_of_and_cli", "daily_accounting",
        )
    }
    checks["main_run"] = result(True, status="passed", reason="The main Run completed before the post-main phase failed.")
    failed_check = failure.get("criterion")
    if failed_check not in checks:
        failed_check = "daily_accounting"
    checks[failed_check] = result(False, error=failure.get("error", "post-main phase failed"))
    checks["unknown_floor"] = verify_unknown_coverage(data["unknown_coverage"])
    checks["frozen_raw_immutable"] = result(data.get("raw_immutable") is True)
    non_blocking = data.get("non_blocking", {})
    non_blocking_errors = []
    expected_non_blocking = {"small_grid", "accurate_smoke", "legacy_comparison"}
    if set(non_blocking) != expected_non_blocking:
        non_blocking_errors.append("non-blocking outcomes are incomplete")
    for name in expected_non_blocking:
        item = non_blocking.get(name, {})
        if item.get("status") not in {"passed", "failed", "not_run"}:
            non_blocking_errors.append(f"{name} has an invalid status")
        elif item["status"] == "not_run" and not str(item.get("reason", "")).strip():
            non_blocking_errors.append(f"{name} is not_run without a reason")
    return {
        "schema": "mvp6-acceptance-verification@1",
        "passed": False,
        "checks": checks,
        "non_blocking": non_blocking,
        "non_blocking_errors": non_blocking_errors,
    }


def verify_fixed_universe(diagnostics: dict[str, Any], expected_session_count: int) -> dict[str, Any]:
    errors = []
    configuration = diagnostics.get("configuration", {})
    configured_codes = configuration.get("codes", [])
    if (configuration.get("version") != FIXED_UNIVERSE["version"]
            or not isinstance(configured_codes, list)
            or not all(isinstance(code, str) for code in configured_codes)
            or len(configured_codes) != len(FIXED_UNIVERSE["codes"])
            or set(configured_codes) != set(FIXED_UNIVERSE["codes"])
            or configuration.get("minimum_code_count") != FIXED_UNIVERSE["minimum_code_count"]
            or configuration.get("excluded_codes", {})):
        errors.append({"kind": "fixed_universe_configuration_mismatch"})
    if not diagnostics.get("catalog_versions") or not diagnostics.get("rule_versions"):
        errors.append({"kind": "missing_catalog_or_rule_version"})
    sessions = diagnostics.get("sessions", [])
    if (not sessions or len(sessions) != diagnostics.get("session_count")
            or len(sessions) != expected_session_count):
        errors.append({"kind": "fixed_universe_session_coverage"})
    included_union: set[str] = set()
    excluded_union: dict[str, set[str]] = {}
    for index, session in enumerate(sessions):
        if session.get("session_index") != index:
            errors.append({"kind": "fixed_universe_session_order", "session_index": index})
        included = session.get("included_codes", [])
        exclusions = session.get("excluded_reasons", {})
        if (not isinstance(included, list) or not all(isinstance(code, str) for code in included)
                or not isinstance(exclusions, dict)
                or not all(isinstance(code, str) for code in exclusions)):
            errors.append({"kind": "invalid_session_universe", "session_index": index})
            continue
        included_set = set(included)
        if len(included_set) != len(included):
            errors.append({"kind": "duplicate_session_universe_code", "session_index": index})
        included_union.update(included_set)
        if (not included_set.issubset(FIXED_UNIVERSE["codes"])
                or not set(exclusions).issubset(FIXED_UNIVERSE["codes"])
                or included_set.intersection(exclusions)):
            errors.append({"kind": "invalid_session_universe_membership", "session_index": index})
        if len(included_set) < FIXED_UNIVERSE["minimum_code_count"]:
            errors.append({"kind": "minimum_code_count", "session_index": index})
        for code, reason in exclusions.items():
            if not isinstance(reason, str) or not reason.strip():
                errors.append({"kind": "invalid_exclusion_reason", "session_index": index})
            else:
                excluded_union.setdefault(code, set()).add(reason)
        for code in FIXED_UNIVERSE["codes"]:
            reason = exclusions.get(code)
            if code not in included_set and (not isinstance(reason, str) or not reason.strip()):
                errors.append({"kind": "unaccounted_fixed_code", "session_index": index})
                break
    if set(diagnostics.get("included_codes", [])) != included_union:
        errors.append({"kind": "included_code_summary_mismatch"})
    normalized_exclusions = {
        code: sorted(reasons) for code, reasons in sorted(excluded_union.items())
    }
    if diagnostics.get("excluded_reasons") != normalized_exclusions:
        errors.append({"kind": "excluded_code_summary_mismatch"})
    if diagnostics.get("unresolved_codes") != []:
        errors.append({"kind": "unresolved_fixed_codes"})
    return result(not errors, sessions=len(sessions), errors=errors)


def verify_dividend_ledger(actions: list[dict[str, Any]]) -> tuple[bool, list[dict[str, Any]]]:
    errors = []
    groups: dict[tuple[Any, Any, Any], dict[str, list[dict[str, Any]]]] = {}
    for event in actions:
        if not event.get("instrument_id", "").startswith("159919"):
            continue
        kind = event.get("kind")
        if kind not in {"dividend_entitlement", "dividend_receivable", "dividend_paid"}:
            continue
        if (event.get("portfolio_id") is None or not event.get("announcement_id")
                or type(event.get("action_index")) is not int):
            errors.append({"kind": "missing_dividend_identity"})
        key = (event.get("portfolio_id"), event.get("announcement_id"), event.get("action_index"))
        groups.setdefault(key, {}).setdefault(kind, []).append(event)
    if not groups:
        errors.append({"kind": "missing_dividend_ledger"})
    if not any(transitions.get("dividend_paid") for transitions in groups.values()):
        errors.append({"kind": "missing_dividend_payment"})
    for key, transitions in groups.items():
        entitlement = transitions.get("dividend_entitlement", [])
        receivable = transitions.get("dividend_receivable", [])
        paid = transitions.get("dividend_paid", [])
        if len(entitlement) != 1 or len(receivable) != 1 or len(paid) > 1:
            errors.append({"kind": "incomplete_dividend_transition_chain"})
            continue
        ordered = [entitlement[0], receivable[0], *paid]
        if any(not isinstance(row.get("session_date"), str) for row in ordered):
            errors.append({"kind": "invalid_dividend_session"})
            continue
        dates = [row["session_date"] for row in ordered]
        if dates != sorted(dates) or dates[0] == dates[1] or (len(dates) == 3 and dates[1] == dates[2]):
            errors.append({"kind": "dividend_transition_date_order"})
        fields = ("quantity_before", "quantity_after", "gross_amount", "tax_amount", "net_amount")
        reference = entitlement[0]
        try:
            gross, tax, net = (dec(reference[field]) for field in ("gross_amount", "tax_amount", "net_amount"))
            if gross - tax != net or gross < 0 or tax < 0 or net < 0:
                errors.append({"kind": "dividend_amount_identity"})
            for row in ordered[1:]:
                if any(dec(row[field]) != dec(reference[field]) for field in fields):
                    errors.append({"kind": "dividend_transition_reconciliation"})
                    break
        except (KeyError, InvalidOperation, TypeError):
            errors.append({"kind": "invalid_dividend_amounts"})
    return not errors, errors


def verify_split_ledger(actions: list[dict[str, Any]]) -> tuple[bool, list[dict[str, Any]]]:
    rows = [event for event in actions if event.get("instrument_id", "").startswith("159560")
            and event.get("kind") == "share_conversion"]
    errors = []
    if not rows:
        return False, [{"kind": "missing_split_ledger"}]
    for row in rows:
        try:
            if (row.get("portfolio_id") is None or not row.get("announcement_id")
                    or type(row.get("action_index")) is not int):
                errors.append({"kind": "missing_split_identity"})
            before = dec(row["quantity_before"])
            ratio = dec(row["share_ratio"])
            after = dec(row["quantity_after"])
            theoretical = dec(row["theoretical_quantity"])
            residual = dec(row["residual_quantity"])
            rounding = dec(row["corporate_action_rounding"])
            calculated = before * ratio
            if (before < 0 or ratio <= 0 or calculated != theoretical
                    or theoretical < 0 or theoretical.to_integral_value(rounding=ROUND_FLOOR) != after
                    or theoretical - after != residual or residual < 0):
                errors.append({"kind": "split_quantity_reconciliation"})
            if residual == 0:
                expected_rounding = Decimal(0)
            else:
                price = dec(row["valuation_price"])
                if not price.is_finite() or price <= 0:
                    raise InvalidOperation
                expected_rounding = -(residual * price)
            if rounding != expected_rounding:
                errors.append({"kind": "split_rounding_reconciliation"})
        except (KeyError, InvalidOperation, TypeError):
            errors.append({"kind": "invalid_split_ledger"})
    return not errors, errors


def _close(left: Any, right: Any) -> bool:
    try:
        a, b = dec(left), dec(right)
        return a.is_finite() and b.is_finite() and abs(a - b) <= Decimal(str(TOLERANCE))
    except (InvalidOperation, TypeError, ValueError):
        return False


def ledger_oracle(
    valuation: dict[str, Any],
) -> tuple[Decimal, Decimal, Decimal, Decimal, Decimal]:
    cash = dec(valuation["cash"])
    dividend = dec(valuation["dividend_receivable"])
    liquidation = dec(valuation.get("liquidation_receivable", "0"))
    if cash < 0 or dividend < 0 or liquidation < 0:
        raise ValueError("Fast Event cash and receivables must be nonnegative")
    invested = Decimal(0)
    for position in valuation["positions"].values():
        quantity = dec(position["quantity"])
        price = dec(position["price"])
        market_value = dec(position["market_value"])
        calculated_value = quantity * price
        if quantity < 0 or price <= 0 or calculated_value != market_value:
            raise ValueError("position quantity, raw valuation price, and market value do not reconcile")
        invested += calculated_value
    return cash + dividend + liquidation + invested, cash, invested, dividend, liquidation


def normalized_trajectory(rows: list[dict[str, Any]]) -> list[tuple[str, tuple[str, ...], tuple[str, ...]]]:
    kinds = {"entries_retried": "retryable_execution", "retryable_execution": "retryable_execution"}
    normalized = []
    for row in rows:
        kind = kinds.get(row.get("kind"), row.get("kind"))
        normalized.append((kind, tuple(sorted(row.get("blocked", []))),
                           tuple(sorted(row.get("skipped_buys", [])))))
    return sorted(normalized)


def verify_parity(parity: dict[str, Any]) -> dict[str, Any]:
    expected = parity.get("expected_sessions", [])
    days = parity.get("days", [])
    errors = []
    try:
        initial_cash = dec(parity["initial_cash"])
        if not initial_cash.is_finite() or initial_cash <= 0:
            raise InvalidOperation
    except (KeyError, InvalidOperation, TypeError, ValueError):
        initial_cash = Decimal(0)
        errors.append({"kind": "oracle", "reason": "invalid initial capital"})
    if not expected or len(set(expected)) != len(expected):
        errors.append({"kind": "invalid_expected_sessions"})
    by_date = {day.get("session_date"): day for day in days}
    if len(by_date) != len(days) or set(by_date) != set(expected):
        errors.append({"kind": "parity_session_coverage"})
    valuation_rows = parity.get("fast_valuations", [])
    valuation_map = {
        (row.get("session_date"), row.get("valuation_point")): row
        for row in valuation_rows
    }
    expected_points = {(date, point) for date in expected for point in ("open_after", "close")}
    observed_points = [(row.get("session_date"), row.get("valuation_point"))
                       for row in valuation_rows]
    if len(observed_points) != len(set(observed_points)) or set(observed_points) != expected_points:
        errors.append({"kind": "oracle", "reason": "Fast Event valuation coverage is incomplete"})
    oracle_valuation_count = 0
    for date in expected:
        day = by_date.get(date)
        if day is None:
            continue
        for point, vector_point, fast_point in (
            ("open_after", "open", "open"), ("close", "close", "close"),
        ):
            valuation = valuation_map.get((date, point))
            if valuation is None:
                continue
            try:
                equity, cash, invested, dividend, liquidation = ledger_oracle(valuation)
                oracle_valuation_count += 1
                reported_equity = dec(valuation["equity"])
                if (valuation.get("accounting_verified") is not True
                        or dec(valuation.get("unallocated_capital", "0")) != 0
                        or equity != reported_equity):
                    raise ValueError("Fast Event NAV does not match independently recomputed account ledger")
                if initial_cash > 0:
                    oracle_nav = equity / initial_cash
                    if not _close(day.get(f"vector_{vector_point}_nav"), oracle_nav):
                        errors.append({"kind": f"{fast_point}_nav_mismatch", "session_date": date})
                    if not _close(day.get(f"fast_{fast_point}_nav"), oracle_nav):
                        errors.append({"kind": "oracle", "reason": "Fast Event NAV differs from the ledger oracle",
                                       "session_date": date, "valuation_point": point})
                vector = day.get(f"vector_{vector_point}_components") or {}
                fast = day.get(f"fast_{fast_point}_components") or {}
                if equity <= 0:
                    raise ValueError("nonpositive Fast Event ledger equity")
                oracle_components = {
                    "cash_weight": cash / equity,
                    "invested_weight": invested / equity,
                    "dividend_receivable_weight": dividend / equity,
                    "liquidation_receivable_weight": liquidation / equity,
                }
                if not _close(vector.get("available_cash_weight"), oracle_components["cash_weight"]):
                    errors.append({"kind": f"{vector_point}_oracle_cash_mismatch", "session_date": date})
                if not _close(vector.get("dividend_receivable_weight"), oracle_components["dividend_receivable_weight"]):
                    errors.append({"kind": f"{vector_point}_oracle_receivable_mismatch", "session_date": date})
                if not _close(vector.get("liquidation_receivable_weight"), oracle_components["liquidation_receivable_weight"]):
                    errors.append({"kind": f"{vector_point}_oracle_liquidation_mismatch", "session_date": date})
                if not _close(fast.get("cash_weight"), oracle_components["cash_weight"]):
                    errors.append({"kind": f"{fast_point}_oracle_cash_mismatch", "session_date": date})
                if not _close(fast.get("invested_weight"), oracle_components["invested_weight"]):
                    errors.append({"kind": f"{fast_point}_oracle_invested_mismatch", "session_date": date})
                if not _close(fast.get("dividend_receivable_weight"), oracle_components["dividend_receivable_weight"]):
                    errors.append({"kind": f"{fast_point}_oracle_receivable_mismatch", "session_date": date})
                if not _close(fast.get("liquidation_receivable_weight"), oracle_components["liquidation_receivable_weight"]):
                    errors.append({"kind": f"{fast_point}_oracle_liquidation_mismatch", "session_date": date})
            except (KeyError, InvalidOperation, TypeError, ValueError):
                errors.append({"kind": "oracle", "session_date": date, "valuation_point": point})
        if not _close(day.get("vector_cost"), day.get("fast_cost")):
            errors.append({"kind": "cost_mismatch", "session_date": date})
        for point in ("open", "close"):
            vector = day.get(f"vector_{point}_components") or {}
            fast = day.get(f"fast_{point}_components") or {}
            try:
                vector_cash = dec(vector["available_cash_weight"])
                vector_dividend = dec(vector["dividend_receivable_weight"])
                vector_liquidation = dec(vector["liquidation_receivable_weight"])
                vector_invested = Decimal(1) - vector_cash - vector_dividend - vector_liquidation
                components = (
                    (vector_cash, dec(fast["cash_weight"])),
                    (vector_invested, dec(fast["invested_weight"])),
                    (vector_dividend, dec(fast["dividend_receivable_weight"])),
                    (vector_liquidation, dec(fast["liquidation_receivable_weight"])),
                )
                if any(not left.is_finite() or not right.is_finite() or abs(left - right) > Decimal(str(TOLERANCE))
                       for left, right in components):
                    errors.append({"kind": f"{point}_component_mismatch", "session_date": date})
            except (KeyError, InvalidOperation, TypeError):
                errors.append({"kind": f"missing_{point}_components", "session_date": date})
        try:
            if normalized_trajectory(day["vector_trajectory"]) != normalized_trajectory(day["fast_trajectory"]):
                errors.append({"kind": "status_defer_trajectory_mismatch", "session_date": date})
        except (KeyError, TypeError):
            errors.append({"kind": "missing_status_defer_trajectory", "session_date": date})
    return result(
        not errors,
        sessions=len(days),
        oracle={
            "basis": "Decimal(cash + dividend receivable + liquidation receivable + sum(quantity * raw price))",
            "valuation_points": oracle_valuation_count,
        },
        errors=errors,
    )


def verify_artifacts(data: dict[str, Any]) -> dict[str, Any]:
    """Evaluate the six blocking criteria from a report or synthetic artifact set."""
    if data.get("schema") == EARLY_FAILURE_SCHEMA:
        return verify_early_failure(data)
    if data.get("schema") == PHASE_FAILURE_SCHEMA:
        return verify_post_main_failure(data)
    if data.get("schema") != SCHEMA:
        raise ValueError(f"schema must be {SCHEMA}")

    main = data["main_run"]
    actions = main.get("actions", [])
    dividend, dividend_errors = verify_dividend_ledger(actions)
    split_traceable, split_errors = verify_split_ledger(actions)
    terminal_rows = main.get("terminal_settlements", [])
    terminal_errors = []
    for row in terminal_rows:
        try:
            quantity = dec(row["quantity"])
            price = dec(row["price"])
            proceeds = dec(row["proceeds"])
            if (not row.get("session_date") or not row.get("instrument_id")
                    or quantity <= 0 or price <= 0 or proceeds != quantity * price):
                terminal_errors.append({"kind": "invalid_terminal_settlement"})
        except (KeyError, InvalidOperation, TypeError):
            terminal_errors.append({"kind": "invalid_terminal_settlement"})
    terminal_traceable = (
        main.get("terminal_settlement_version") == TERMINAL["semantic_version"]
        and not terminal_errors
    )
    universe_check = verify_fixed_universe(
        data.get("fixed_universe", {}), data.get("accounting", {}).get("session_input_count", 0),
    )
    main_ok = (
        main.get("sizing") == "lot" and main.get("rebalance") == "full_target"
        and main.get("return_basis") == "total_return" and dividend and split_traceable
        and terminal_traceable and universe_check["passed"]
    )

    parity_check = verify_parity(data.get("parity", {}))

    independent = data["independent_runs"]
    deterministic = len(independent) == 2
    if deterministic:
        deterministic = (
            bool(independent[0].get("experiment_id"))
            and bool(independent[0].get("execution_id"))
            and bool(independent[0].get("summary_logical_hash"))
            and independent[0]["experiment_id"] == independent[1]["experiment_id"]
            and independent[0]["execution_id"] == independent[1]["execution_id"]
            and independent[0]["summary_logical_hash"] == independent[1]["summary_logical_hash"]
            and set(independent[0]["levels"]) == {"summary", "standard", "full"}
            and independent[0]["levels"] == independent[1]["levels"]
        )
    if deterministic:
        deterministic = all(
            bool(independent[0]["levels"][level].get("runs"))
            and all(run.get("status") == "completed"
                    for run in independent[0]["levels"][level].get("runs", []))
            for level in ("summary", "standard", "full")
        )

    python = data["python_cli"]
    python_cli_ok = (
        python.get("cli") == python.get("python")
        and python.get("api_action") == "replayed"
        and python.get("as_of_view_matches") is True
    )

    accounting = data["accounting"]
    accounting_errors = []
    expected_sessions = accounting.get("expected_sessions", [])
    session_input_count = accounting.get("session_input_count")
    expected_points = {(date, point) for date in expected_sessions for point in ("open_after", "close")}
    observed_points = [(row.get("session_date"), row.get("valuation_point"))
                       for row in accounting.get("valuations", [])]
    if (not expected_sessions or len(set(expected_sessions)) != len(expected_sessions)
            or type(session_input_count) is not int or session_input_count != len(expected_sessions)
            or len(observed_points) != len(set(observed_points))
            or set(observed_points) != expected_points):
        accounting_errors.append({"kind": "valuation_coverage"})
    weight_errors = []
    weight_summaries = []
    for row in accounting.get("valuations", []):
        try:
            cash = dec(row["cash"])
            dividend_receivable = dec(row["dividend_receivable"])
            liquidation_receivable = dec(row.get("liquidation_receivable", "0"))
            reported_equity = dec(row["equity"])
            if row.get("accounting_verified") is not True or dec(row.get("unallocated_capital", "0")) != 0:
                accounting_errors.append({"session_date": row.get("session_date"), "kind": "unverified_account"})
            equity = cash + dividend_receivable + liquidation_receivable
            position_weight_total = Decimal(0)
            for instrument, position in row["positions"].items():
                quantity, price = dec(position["quantity"]), dec(position["price"])
                market_value = dec(position["market_value"])
                if quantity < 0 or price <= 0 or quantity * price != market_value:
                    accounting_errors.append({"session_date": row["session_date"], "instrument_id": instrument})
                if reported_equity > 0:
                    weight = market_value / reported_equity
                    position_weight_total += weight
                    if weight < 0:
                        weight_errors.append({
                            "session_date": row["session_date"],
                            "instrument_id": instrument,
                            "kind": "negative_position_weight",
                        })
                equity += market_value
            if equity != reported_equity:
                accounting_errors.append({"session_date": row["session_date"], "kind": "nav_identity"})
            if reported_equity <= 0:
                weight_errors.append({"session_date": row["session_date"], "kind": "nonpositive_equity"})
            else:
                cash_weight = cash / reported_equity
                receivable_weight = (dividend_receivable + liquidation_receivable) / reported_equity
                cash_and_position_weight = cash_weight + position_weight_total
                total_weight = cash_and_position_weight + receivable_weight
                if cash_weight < 0:
                    weight_errors.append({"session_date": row["session_date"], "kind": "negative_cash_weight"})
                if dividend_receivable < 0 or liquidation_receivable < 0:
                    weight_errors.append({"session_date": row["session_date"], "kind": "negative_receivable_weight"})
                if abs(total_weight - Decimal(1)) > WEIGHT_TOLERANCE:
                    weight_errors.append({"session_date": row["session_date"], "kind": "weight_identity"})
                weight_summaries.append({
                    "session_date": row["session_date"], "valuation_point": row["valuation_point"],
                    "cash_remainder": {"amount": str(cash), "weight": str(cash_weight)},
                    "position_weight_total": str(position_weight_total),
                    "cash_and_position_weight": str(cash_and_position_weight),
                    "receivable_weight": str(receivable_weight),
                    "total_weight": str(total_weight),
                })
        except (KeyError, InvalidOperation, TypeError):
            accounting_errors.append({"session_date": row.get("session_date"), "kind": "invalid_valuation"})

    fee_errors = []
    for fill in accounting.get("fills", []):
        try:
            costs = fill["costs"]
            values = [dec(costs[key]) for key in ("commission", "minimum_commission_top_up", "tax")]
            slippage = dec(fill["slippage"])
            total = sum(values, Decimal(0)) + slippage
            if any(value < 0 for value in [*values, slippage]) or total != dec(fill["total_fees"]):
                fee_errors.append({"kind": "fill_fee_reconciliation"})
        except (KeyError, InvalidOperation, TypeError):
            fee_errors.append({"kind": "invalid_fill_fee"})

    blocked = {
        (cell["instrument_id"], cell["session_date"])
        for cell in accounting.get("forbidden_cells", [])
        if cell.get("state") in {"unknown", "halted"}
    }
    blocked_trades = [
        trade for trade in accounting.get("trades", [])
        if (trade.get("instrument_id"), trade.get("session_date")) in blocked
    ]
    accounting_ok = (
        bool(expected_sessions) and not accounting_errors and not weight_errors
        and bool(accounting.get("fills")) and not fee_errors
        and accounting.get("fees_present") is True and not blocked_trades
        and split_traceable and terminal_traceable
    )

    checks = {
        "main_run": result(main_ok, dividend_ledger=dividend, dividend_errors=dividend_errors,
                            split_ledger=split_traceable, split_errors=split_errors,
                            terminal_settlement=terminal_traceable, terminal_errors=terminal_errors,
                            fixed_universe=universe_check),
        "daily_parity": parity_check,
        "reproducibility": result(deterministic),
        "python_as_of_and_cli": result(python_cli_ok),
        "daily_accounting": result(accounting_ok, valuation_errors=accounting_errors,
                                   weight_errors=weight_errors, weight_summaries=weight_summaries,
                                   fee_errors=fee_errors, blocked_trades=len(blocked_trades)),
        "unknown_floor": verify_unknown_coverage(data["unknown_coverage"]),
        "frozen_raw_immutable": result(data.get("raw_immutable") is True),
    }
    return {
        "schema": "mvp6-acceptance-verification@1",
        "passed": all(item["passed"] for item in checks.values()),
        "checks": checks,
    }


def parse_payload_rows(frame: Any) -> list[dict[str, Any]]:
    rows = []
    for row in frame.to_pylist():
        payload = row.get("payload")
        if not isinstance(payload, str):
            raise ValueError("result table row has no JSON payload")
        rows.append(json.loads(payload))
    return rows


def experiment_from_definition(definition: dict[str, Any]) -> Any:
    import prajna

    engine_kind = prajna.EngineKind(definition["engine"])
    if engine_kind == prajna.EngineKind.FAST_EVENT:
        raw = definition["fast_event"]
        settings = prajna.FastEventSettings(
            currency=raw["currency"], initial_cash=raw["initial_cash"],
            sizing=raw["sizing"], rebalance=raw["rebalance"],
            minimum_commission=raw["minimum_commission"],
            portfolios=[prajna.Capital(item["id"], item["capital"]) for item in raw["portfolios"]],
            lot_size_source=prajna.LotSizeSource(),
        )
        engine = prajna.Engine(engine_kind, settings=settings)
    else:
        engine = prajna.Engine(
            engine_kind, initial_capital=definition["vector_pit"]["initial_capital"],
        )
    strategy = prajna.RustStrategy(definition["strategy"])
    return prajna.Experiment(
        dataset_version=prajna.DatasetVersion(definition["dsv"]),
        universe=prajna.Universe(definition["universe"]),
        venue=prajna.Venue(definition["venue"]),
        strategy=strategy,
        engine=engine,
        costs=prajna.Costs(**definition["costs"]),
        parameter_space=prajna.List(definition["parameter_space"]["list"]),
        availability_assumption=prajna.AvailabilityAssumption(definition["availability_assumption"]),
        sessions_per_year=definition["sessions_per_year"],
        pit=definition["pit"],
    )


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def invoke(argv: list[str], log_path: Path, *, cwd: Path) -> dict[str, Any]:
    completed = subprocess.run(argv, cwd=cwd, capture_output=True, text=True)
    write_json(log_path.with_suffix(".json"), {
        "argv": argv,
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    })
    if completed.returncode:
        raise RuntimeError(f"command failed; diagnostics are local at {log_path.with_suffix('.json')}")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"command did not emit JSON; diagnostics are local at {log_path.with_suffix('.json')}") from error


class RunPhaseError(RuntimeError):
    def __init__(
        self, stage: str, status: str, error: str, *,
        cli_result: dict[str, Any] | None = None, manifest: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(error)
        self.stage = stage
        self.status = status
        self.cli_result = cli_result
        self.manifest = manifest


def fixed_universe_diagnostics(run_spec: dict[str, Any]) -> dict[str, Any]:
    fixed = run_spec.get("pit", {}).get("fixed_universe", {})
    included: set[str] = set()
    excluded: dict[str, set[str]] = {}
    catalog_versions: set[str] = set()
    rule_versions: set[str] = set()
    session_reports = []
    for index, session in enumerate(run_spec.get("session_inputs", [])):
        universe = session.get("eligible", {}).get("universe", {}).get("run_spec", {})
        session_included = set(universe.get("fixed_list", []))
        session_excluded = universe.get("fixed_list_exclusions", {}) or {}
        included.update(session_included)
        if universe.get("catalog_version"):
            catalog_versions.add(universe["catalog_version"])
        if universe.get("rule_version"):
            rule_versions.add(universe["rule_version"])
        for code, reason in session_excluded.items():
            excluded.setdefault(code, set()).add(reason)
        session_reports.append({
            "session_index": index,
            "included_codes": sorted(session_included),
            "excluded_reasons": dict(sorted(session_excluded.items())),
        })
    candidates = set(fixed.get("codes", [])) | set(fixed.get("excluded_codes", {}))
    accounted = included | set(excluded)
    return {
        "configuration": fixed,
        "catalog_versions": sorted(catalog_versions),
        "rule_versions": sorted(rule_versions),
        "included_codes": sorted(included),
        "excluded_reasons": {code: sorted(reasons) for code, reasons in sorted(excluded.items())},
        "unresolved_codes": sorted(candidates - accounted),
        "session_count": len(session_reports),
        "sessions": session_reports,
    }


def unevaluated_fixed_universe(fixed: dict[str, Any]) -> dict[str, Any]:
    return {
        "configuration": fixed,
        "catalog_versions": [],
        "rule_versions": [],
        "included_codes": [],
        "excluded_reasons": {},
        "unresolved_codes": list(fixed["codes"]),
        "status": "not_evaluated_due_to_raw_preflight",
    }


def prior_run_identity(acceptance: Path, current_output: Path) -> dict[str, Any] | None:
    for previous in sorted(acceptance.glob("issue-234-*"), reverse=True):
        if previous == current_output:
            continue
        input_path = previous / "independent-run-1" / "pit-input.json"
        log_path = previous / "independent-run-1" / "main-summary-cli.json"
        if not input_path.is_file() or not log_path.is_file():
            continue
        try:
            prior_input = json.loads(input_path.read_text())
            cli_log = json.loads(log_path.read_text())
            cli_result = json.loads(cli_log["stdout"])
        except (OSError, KeyError, json.JSONDecodeError):
            continue
        # The identity baseline must be the legacy input where the universe
        # was omitted. A newer explicit-universe attempt cannot replace it.
        if "fixed_universe" in prior_input:
            continue
        if cli_result.get("exp") and cli_result.get("exe"):
            return {
                "experiment_id": cli_result["exp"],
                "execution_id": cli_result["exe"],
                "fixed_universe": prior_input.get("fixed_universe", "omitted; legacy default applied"),
            }
    return None


def identity_change(
    acceptance: Path, current_output: Path, cli_result: dict[str, Any],
) -> dict[str, Any]:
    current = {"experiment_id": cli_result.get("exp"), "execution_id": cli_result.get("exe")}
    previous = prior_run_identity(acceptance, current_output)
    return {
        "previous": previous,
        "current": current,
        "experiment_changed": (
            previous["experiment_id"] != current["experiment_id"] if previous is not None else None
        ),
        "execution_changed": (
            previous["execution_id"] != current["execution_id"] if previous is not None else None
        ),
    }


def write_early_failure_report(
    *, output: Path, acceptance: Path, raw_inventory: Path, coverage: dict[str, Any],
    unknown: dict[str, Any], pit_base: dict[str, Any], failure: dict[str, Any],
) -> dict[str, Any]:
    normalized_unknown = normalize_unknown_coverage(unknown)
    raw_before = failure["raw_hashes_before"]
    raw_after = raw_hashes(acceptance.parent, raw_inventory)
    raw_immutable = raw_snapshot_unchanged(raw_before, raw_after)
    artifacts = {
        "schema": EARLY_FAILURE_SCHEMA,
        "failure": {key: failure.get(key) for key in (
            "stage", "status", "error", "fixed_universe", "identity_change",
        )},
        "unknown_coverage": normalized_unknown,
        "raw_immutable": raw_immutable,
    }
    verification = verify_early_failure(artifacts)
    report = {
        "schema": "mvp6-local-engineering-report@1",
        "issue": 234,
        "classification": "platform engineering acceptance; not strategy historical performance",
        "verification": verification,
        "failure": artifacts["failure"],
        "raw_immutability": {
            "passed": raw_immutable,
            "inventory_entries": raw_before.request_records,
            "verified_before_run": sum(raw_before.checked.values()),
            "verified_after_run": sum(raw_after.checked.values()),
            "failed_before_run": sum(not valid for valid in raw_before.checked.values()),
        },
        "non_blocking": {
            name: {"status": "not_run", "reason": "The main Run failed before acceptance could continue."}
            for name in ("small_grid", "accurate_smoke", "legacy_comparison")
        },
        "source_evidence": {
            "decision_window": {"start": "2023-01-01", "end": "2026-09-30", "preheat": "60 Sessions"},
            "return_basis": "total_return (planned; no Run result was produced)",
            "universe_claim": "retrospective_static",
            "survivorship_bias": True,
            "source_status": coverage.get("source_status"),
            "p21_status": coverage.get("p21", {}).get("status"),
            "unknown_semantics": "P27-23.5 narrowed UNKNOWN to data defects; no-Bar and unknown rules are excluded.",
            "risk_notice_count": len(pit_base.get("risk_notices", [])),
            "cninfo_index_supplied": "cninfo_index" in pit_base,
        },
        "local_artifacts": {
            "directory": str(output),
            "rerun_command": "python scripts/mvp6_acceptance.py run --root ~/PrajnaData/mvp-6",
            "frozen_raw_inventory": str(raw_inventory),
        },
        "unknown_coverage": normalized_unknown,
    }
    write_json(output / "mvp6-acceptance-report.json", report)
    safe_summary = {
        "passed": False,
        "checks": {name: check["status"] for name, check in verification["checks"].items()},
        "per_etf_unknown_coverage": [
            {
                "code": item["symbol"],
                "unknown_cells": item["unknown_cells"],
                "cells": item["cells"],
                "unknown_percent": 100 * item["unknown_cells"] / item["cells"] if item["cells"] else None,
            }
            for item in normalized_unknown["instruments"]
        ],
        "conclusion": "Platform engineering acceptance only; this is not strategy historical performance.",
        "report": str(output / "mvp6-acceptance-report.json"),
    }
    return safe_summary


def write_post_main_failure_report(
    *, output: Path, acceptance: Path, raw_inventory: Path, unknown: dict[str, Any],
    criterion: str, error: str, raw_before: RawHashSnapshot,
) -> dict[str, Any]:
    normalized_unknown = normalize_unknown_coverage(unknown)
    raw_after = raw_hashes(acceptance.parent, raw_inventory)
    raw_immutable = raw_snapshot_unchanged(raw_before, raw_after)
    failure = {"criterion": criterion, "error": error}
    non_blocking = {
        "small_grid": {
            "status": "not_run",
            "reason": "P16 non-blocking grid is not needed for the engineering ledger gate.",
        },
        "accurate_smoke": {
            "status": "not_run",
            "reason": "No Accurate Event worker was configured for this local acceptance.",
        },
        "legacy_comparison": {
            "status": "not_run",
            "reason": "The old-system comparator for the legacy window is not available in this workspace.",
        },
    }
    artifacts = {
        "schema": PHASE_FAILURE_SCHEMA,
        "failure": failure,
        "unknown_coverage": normalized_unknown,
        "raw_immutable": raw_immutable,
        "non_blocking": non_blocking,
    }
    verification = verify_post_main_failure(artifacts)
    report = {
        "schema": "mvp6-local-engineering-report@1",
        "issue": 234,
        "classification": "platform engineering acceptance; not strategy historical performance",
        "verification": verification,
        "failure": failure,
        "non_blocking": non_blocking,
        "raw_immutability": {
            "passed": raw_immutable,
            "inventory_entries": raw_before.request_records,
            "verified_before_run": sum(raw_before.checked.values()),
            "verified_after_run": sum(raw_after.checked.values()),
        },
        "unknown_coverage": normalized_unknown,
        "local_artifacts": {
            "directory": str(output),
            "rerun_command": "python scripts/mvp6_acceptance.py run --root ~/PrajnaData/mvp-6",
        },
    }
    write_json(output / "mvp6-acceptance-report.json", report)
    return {
        "passed": False,
        "checks": {name: check["status"] for name, check in verification["checks"].items()},
        "per_etf_unknown_coverage": [
            {
                "code": item["symbol"], "unknown_cells": item["unknown_cells"],
                "cells": item["cells"],
                "unknown_percent": 100 * item["unknown_cells"] / item["cells"] if item["cells"] else None,
            }
            for item in normalized_unknown["instruments"]
            if item["symbol"] in FIXED_UNIVERSE["codes"]
        ],
        "conclusion": "Platform engineering acceptance only; this is not strategy historical performance.",
        "report": str(output / "mvp6-acceptance-report.json"),
    }


def raw_hashes(root: Path, inventory: Path) -> RawHashSnapshot:
    try:
        inventory_bytes = inventory.read_bytes()
    except (OSError, json.JSONDecodeError):
        return RawHashSnapshot({}, 0, "inventory cannot be read")
    inventory_sha256 = hashlib.sha256(inventory_bytes).hexdigest()
    try:
        data = json.loads(inventory_bytes)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return RawHashSnapshot({}, 0, "inventory cannot be read", inventory_sha256)
    records = data.get("request_records") if isinstance(data, dict) else None
    if not isinstance(records, list) or not records:
        return RawHashSnapshot({}, 0, "inventory has no request records")
    hashes: set[str] = set()
    malformed = False
    for item in records:
        value = item.get("sha256") if isinstance(item, dict) else None
        if not isinstance(value, str):
            malformed = True
            continue
        digest = value.removeprefix("sha256:")
        if not re.fullmatch(r"[0-9a-fA-F]{64}", digest):
            malformed = True
            continue
        hashes.add(digest.lower())
    checked: dict[str, bool] = {}
    for digest in sorted(hashes):
        path = root / "raw" / "sha256" / digest[:2] / digest
        if not path.is_file():
            checked[digest] = False
            continue
        try:
            checked[digest] = hashlib.sha256(path.read_bytes()).hexdigest() == digest
        except OSError:
            checked[digest] = False
    return RawHashSnapshot(
        checked, len(records),
        "inventory contains a missing or invalid SHA-256" if malformed else None,
        inventory_sha256,
    )


def local_lake(source: Path, destination: Path) -> Path:
    lake = destination / "lake"
    lake.mkdir(parents=True)
    for name in ("raw", "raw-sources", "manifests", "normalized"):
        target = source / name
        if not target.is_dir():
            raise ValueError(f"frozen input directory is missing: {name}")
        (lake / name).symlink_to(target, target_is_directory=True)
    return lake


def experiment_definition(
    dsv: str, catalog: str, pit: dict[str, Any], engine: str, *, sizing: str = "lot",
) -> dict[str, Any]:
    definition: dict[str, Any] = {
        "experiment_version": 1,
        "dsv": dsv,
        "universe": catalog,
        "venue": "SZSE",
        "strategy": "s2_pit_rotation",
        "engine": engine,
        "availability_assumption": "none",
        "sessions_per_year": 252,
        "costs": COSTS,
        "parameter_space": {"list": [PARAMETERS]},
        "pit": pit,
    }
    if engine == "fast_event":
        definition["fast_event"] = {**LOT, "sizing": sizing}
    else:
        definition["vector_pit"] = {"initial_capital": "1000000"}
    return definition


def run_levels(
    definition: dict[str, Any], lake: Path, cli: Path, repository: Path,
    output: Path, prefix: str, *, levels: tuple[str, ...],
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    import prajna

    typed = experiment_from_definition(definition)
    if json.loads(typed.to_json()) != definition:
        raise ValueError("Python typed definition differs from the CLI definition")
    run_ids: list[str] = []
    cli_results: dict[str, Any] = {}
    api_results: dict[str, Any] = {}
    manifests: dict[str, Any] = {}
    for level in levels:
        argv = [str(cli), "run", str(output / f"{prefix}-experiment.json"), "--lake", str(lake),
                "--level", level, "--threads", "1"]
        if level != "summary" and run_ids:
            argv.extend(["--promote", *run_ids])
        try:
            cli_result = invoke(argv, output / f"{prefix}-{level}-cli", cwd=repository)
        except RuntimeError as error:
            raise RunPhaseError(f"{prefix}_{level}", "failed", str(error)) from error
        experiment_hex = cli_result["exp"].split(":sha256:", 1)[1]
        execution_hex = cli_result["exe"].split(":sha256:", 1)[1]
        cli_manifest_path = lake / "experiments" / experiment_hex / "executions" / execution_hex / "execution.json"
        cli_manifest = json.loads(cli_manifest_path.read_text())
        if cli_result.get("failed_count") != 0:
            failures = [run for run in cli_manifest.get("runs", []) if run.get("status") != "completed"]
            original_error = next((run.get("error") for run in failures if run.get("error")), None)
            raise RunPhaseError(
                f"{prefix}_{level}", "failed",
                original_error or f"CLI recorded {cli_result.get('failed_count')} failed Run(s)",
                cli_result=cli_result, manifest=cli_manifest,
            )
        run_ids = [run["run_id"] for run in cli_manifest["runs"]]
        api_result = typed.run(
            lake, repository=repository, level=prajna.ResultLevel(level), threads=1,
        )
        manifest = json.loads((api_result.path / "execution.json").read_text())
        if api_result.action != "replayed" or cli_manifest != manifest:
            raise RunPhaseError(
                f"{prefix}_{level}_api_replay", "not_run",
                "Python API did not replay CLI Execution: "
                f"action={api_result.action}, "
                f"experiment_match={cli_result.get('exp') == manifest.get('experiment_id')}, "
                f"execution_match={cli_result.get('exe') == manifest.get('execution', {}).get('id')}, "
                f"manifest_match={cli_manifest == manifest}",
                cli_result=cli_result,
                manifest=cli_manifest,
            )
        if cli_result.get("exp") != manifest["experiment_id"] or cli_result.get("exe") != manifest["execution"]["id"]:
            raise RuntimeError("Python API and CLI identities differ")
        api_results[level] = {
            "experiment_id": manifest["experiment_id"],
            "execution_id": manifest["execution"]["id"],
            "action": api_result.action,
            **manifest_results(manifest),
        }
        cli_results[level] = {**cli_result, **manifest_results(cli_manifest)}
        manifests[level] = manifest
    return cli_results, api_results, manifests


def read_table(lake: Path, manifest: dict[str, Any], run_id: str, table: str) -> list[dict[str, Any]]:
    import prajna

    return parse_payload_rows(prajna.read_run(
        str(lake), manifest["experiment_id"], manifest["execution"]["id"], run_id,
        table=table, result_level="full",
    ))


def compare_independent_levels(manifests_by_run: list[dict[str, Any]]) -> list[dict[str, Any]]:
    left, right = manifests_by_run
    reports = []
    for levels in (left, right):
        if set(levels) != {"summary", "standard", "full"}:
            raise RuntimeError("each independent run must contain Summary, Standard and Full")
        manifests = list(levels.values())
        if len({item["experiment_id"] for item in manifests}) != 1 \
                or len({item["execution"]["id"] for item in manifests}) != 1:
            raise RuntimeError("Run/Execution identity changed across result levels")
        reports.append({
            "experiment_id": manifests[0]["experiment_id"],
            "execution_id": manifests[0]["execution"]["id"],
            "summary_logical_hash": manifests[0]["summary_logical_hash"],
            "levels": {
                level: {
                    "run_ids": [run["run_id"] for run in manifest["runs"]],
                    "summary_logical_hash": manifest["summary_logical_hash"],
                    "table_hashes": {run["run_id"]: run["table_hashes"] for run in manifest["runs"]},
                    "runs": [{"status": run["status"]} for run in manifest["runs"]],
                }
                for level, manifest in levels.items()
            },
        })
    return reports


def manifest_results(manifest: dict[str, Any]) -> dict[str, Any]:
    return {
        "summary_logical_hash": manifest["summary_logical_hash"],
        "runs": {run["run_id"]: {"status": run["status"], "table_hashes": run["table_hashes"]}
                 for run in manifest["runs"]},
    }


def as_of_decisions_match(lake: Path, dsv: str, pit: dict[str, Any], vector_manifest: dict[str, Any]) -> bool:
    import prajna

    params = PARAMETERS
    strategy = prajna.Strategy(
        "mvp6_asof_check", "1", bar_columns=(), lookback_sessions=params["lookback"],
        rebalance_every=params["rebalance_every"],
        factors=(prajna.FactorDependency(
            "momentum", "etf_tri_momentum", {"lookback": params["lookback"]},
        ),),
    )

    def decide(view: Any, parameters: dict[str, Any], previous: Any) -> Any:
        rows = [row for row in view.factors["momentum"].to_pylist()
                if str(row["session_date"]) == view.session_date and row["value"] is not None
                and row["instrument_id"] in view.eligible
                and row["instrument_id"] not in view.risk_excluded]
        rows.sort(key=lambda row: (-row["value"], row["instrument_id"]))
        selected = rows[:parameters["top_k"]]
        return prajna.Decision(
            {row["instrument_id"]: 1 / len(selected) for row in selected},
            [(row["instrument_id"], row["value"], index + 1)
             for index, row in enumerate(rows)],
        )

    decisions = prajna.generate_decisions(
        lake_root=str(lake), dsv=dsv, venue="SZSE", members=[], strategy=strategy,
        decide=decide, params=params, pit=pit,
    )
    vector_run = vector_manifest["runs"][0]
    stored = read_table(lake, vector_manifest, vector_run["run_id"], "decisions")
    expected = sorted(
        (row["decision_session"], row["instrument_id"], row["rank"], row["score"], row["target_weight"])
        for row in stored
    )
    actual = sorted(
        (decision.session_date, instrument, rank, score,
         decision.targets.get(instrument))
        for decision in decisions
        for instrument, score, rank in decision.ranked
    )
    return len(expected) == len(actual) and all(
        left[:3] == right[:3]
        and abs(float(left[3]) - float(right[3])) <= TOLERANCE
        and ((left[4] is None and right[4] is None)
             or (left[4] is not None and right[4] is not None
                 and abs(float(left[4]) - float(right[4])) <= TOLERANCE))
        for left, right in zip(expected, actual)
    )


def run_acceptance(args: argparse.Namespace) -> tuple[dict[str, Any], Path]:
    global ACTIVE_POST_MAIN_RUN
    ACTIVE_POST_MAIN_RUN = None
    root = Path(args.root).expanduser().resolve()
    expected_root = (Path.home() / "PrajnaData" / "mvp-6").resolve()
    if root != expected_root:
        raise ValueError("real-data execution is restricted to ~/PrajnaData/mvp-6")
    if sys.version_info[:2] != (3, 12):
        raise ValueError("run with the project Python 3.12 environment")
    cli = Path(args.cli).resolve()
    repository = Path(args.repository).resolve()
    if not cli.is_file():
        raise ValueError(f"prajna-experiment binary not found: {cli}")

    acceptance = root / "acceptance"
    input_path = Path(args.pit_input).expanduser().resolve() if args.pit_input else (
        acceptance / "issue-224-rerun-20261022T100126" / "pit-input.json"
    )
    if root not in input_path.parents:
        raise ValueError("real-data inputs must be inside ~/PrajnaData/mvp-6")
    coverage_path = acceptance / "mvp6-coverage.json"
    unknown_path = acceptance / "mvp6-unknown-coverage.json"
    raw_inventory = acceptance / "mvp6-raw-inventory.json"
    coverage = json.loads(coverage_path.read_text())
    unknown = json.loads(unknown_path.read_text())
    pit_base = json.loads(input_path.read_text())
    frozen_opening_path = acceptance / "mvp6-opening-evidence-v3.json"
    frozen_opening = json.loads(frozen_opening_path.read_text())
    if (frozen_opening.get("semantic_version") != "szse-derived-open@3"
            or frozen_opening.get("start") != pit_base["catalog"]["coverage"]["start"]
            or frozen_opening.get("end") != pit_base["catalog"]["coverage"]["end"]):
        raise ValueError("frozen opening evidence does not match the PIT coverage")
    dsv = coverage["corporate_action_dataset"]["dsv"]
    catalog_version = unknown["catalog_version"]

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = acceptance / f"issue-234-{stamp}"
    output.mkdir(parents=True, exist_ok=False)
    before = raw_hashes(root, raw_inventory)
    write_json(output / "frozen-input-evidence.json", {
        "data_read_only": True,
        "raw_inventory_entries": before.request_records,
        "all_inventory_hashes_verified_before": raw_snapshot_valid(before),
        "raw_integrity_failure": raw_integrity_failure(before),
        "fixed_universe": FIXED_UNIVERSE,
        "source_status": coverage.get("source_status"),
        "p21_status": coverage.get("p21", {}).get("status"),
        "historical_capture_status": coverage.get("historical_capture_status"),
        "terms_conclusion": coverage.get("terms_conclusion"),
        "risk_notice_count": len(pit_base.get("risk_notices", [])),
        "cninfo_index_supplied": "cninfo_index" in pit_base,
    })

    if reason := raw_integrity_failure(before):
        summary = write_early_failure_report(
            output=output,
            acceptance=acceptance,
            raw_inventory=raw_inventory,
            coverage=coverage,
            unknown=unknown,
            pit_base=pit_base,
            failure={
                "stage": "raw_preflight",
                "status": "failed",
                "error": reason,
                "fixed_universe": unevaluated_fixed_universe(FIXED_UNIVERSE),
                "identity_change": None,
                "raw_hashes_before": before,
            },
        )
        return summary, output

    # Two independent result stores use symlinks to the same immutable inputs.
    lake_dirs = [local_lake(root, output / f"independent-run-{index}") for index in (1, 2)]
    pit_inputs = []
    evidence_bytes = frozen_opening_path.read_bytes()
    for index, lake in enumerate(lake_dirs, start=1):
        evidence_path = lake / "acceptance" / frozen_opening_path.name
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        evidence_path.write_bytes(evidence_bytes)
        pit = build_pit_input(pit_base, frozen_opening)
        pit_inputs.append(pit)
        write_json(output / f"independent-run-{index}" / "pit-input.json", pit)

    main_manifests = []
    main_results = []
    for index, (lake, pit) in enumerate(zip(lake_dirs, pit_inputs), start=1):
        definition = experiment_definition(dsv, catalog_version, pit, "fast_event")
        definition_path = output / f"independent-run-{index}" / "main-experiment.json"
        write_json(definition_path, definition)
        try:
            cli_results, api_results, manifests = run_levels(
                definition, lake, cli, repository, output / f"independent-run-{index}", "main",
                levels=("summary", "standard", "full"),
            )
        except Exception as error:
            manifest = getattr(error, "manifest", None) or {}
            runs = manifest.get("runs", [])
            failed_run = next((
                run for run in runs
                if run.get("status") != "completed"
            ), next(iter(runs), {}))
            run_spec = failed_run.get("run_spec", {})
            configured = fixed_universe_diagnostics(run_spec) if run_spec else {
                **unevaluated_fixed_universe(pit["fixed_universe"]),
                "status": "not_evaluated_due_to_run_failure",
            }
            current_cli = getattr(error, "cli_result", None) or {}
            summary = write_early_failure_report(
                output=output,
                acceptance=acceptance,
                raw_inventory=raw_inventory,
                coverage=coverage,
                unknown=unknown,
                pit_base=pit_base,
                failure={
                    "stage": getattr(error, "stage", f"main_run_{index}_setup_or_replay"),
                    "status": getattr(error, "status", "not_run"),
                    "error": str(error),
                    "fixed_universe": configured,
                    "identity_change": identity_change(acceptance, output, current_cli)
                    if current_cli.get("exp") and current_cli.get("exe") else None,
                    "raw_hashes_before": before,
                },
            )
            return summary, output
        main_manifests.append(manifests)
        main_results.append({"cli": cli_results, "api": api_results})

    ACTIVE_POST_MAIN_RUN = {
        "output": output, "acceptance": acceptance, "raw_inventory": raw_inventory,
        "unknown": unknown, "raw_before": before, "criterion": "daily_parity",
    }
    mark_run_phase("daily_parity")
    vector_definition = experiment_definition(dsv, catalog_version, pit_inputs[0], "vector")
    parity_fast_definition = experiment_definition(
        dsv, catalog_version, pit_inputs[0], "fast_event", sizing="vector_parity",
    )
    parity_paths = output / "independent-run-1"
    write_json(parity_paths / "parity-vector-experiment.json", vector_definition)
    write_json(parity_paths / "parity-fast-experiment.json", parity_fast_definition)
    vector_cli, vector_api, vector_manifests = run_levels(
        vector_definition, lake_dirs[0], cli, repository, parity_paths, "parity-vector", levels=("full",),
    )
    fast_cli, fast_api, fast_manifests = run_levels(
        parity_fast_definition, lake_dirs[0], cli, repository, parity_paths, "parity-fast", levels=("full",),
    )
    vector_manifest = vector_manifests["full"]
    fast_manifest = fast_manifests["full"]

    mark_run_phase("daily_accounting")
    main_full_manifest = main_manifests[0]["full"]
    main_run = main_full_manifest["runs"][0]
    main_lake = lake_dirs[0]
    main_full_tables = {
        name: read_table(main_lake, main_full_manifest, main_run["run_id"], name)
        for name in ("account_valuations", "fills", "fee_settlements", "corporate_actions", "terminal_settlements")
    }
    fast_run = fast_manifest["runs"][0]
    mark_run_phase("daily_parity")
    fast_accounts = read_table(lake_dirs[0], fast_manifest, fast_run["run_id"], "account_valuations")
    fast_fills = read_table(lake_dirs[0], fast_manifest, fast_run["run_id"], "fills")
    fast_attempts = read_table(lake_dirs[0], fast_manifest, fast_run["run_id"], "orders")
    vector_run = vector_manifest["runs"][0]
    vector_sessions = read_table(lake_dirs[0], vector_manifest, vector_run["run_id"], "sessions")
    vector_executions = read_table(lake_dirs[0], vector_manifest, vector_run["run_id"], "executions")

    main_account_rows = [row for row in main_full_tables["account_valuations"]
                         if row.get("scope") == "account"]
    expected_sessions = sorted({row["session_date"] for row in main_account_rows})
    fast_account_by_point = {
        (row["session_date"], row["valuation_point"]): row
        for row in fast_accounts
        if row.get("scope") == "account"
    }
    vector_executions_by_date: dict[str, list[dict[str, Any]]] = {}
    for event in vector_executions:
        if event.get("session_date"):
            vector_executions_by_date.setdefault(event["session_date"], []).append({
                "kind": event.get("kind"), "blocked": event.get("blocked", []),
                "skipped_buys": event.get("skipped_buys", []),
            })
    fast_attempts_by_date: dict[str, list[dict[str, Any]]] = {}
    for attempt in fast_attempts:
        if attempt.get("record_type") == "attempt" and attempt.get("session_date"):
            fast_attempts_by_date.setdefault(attempt["session_date"], []).append({
                "kind": attempt.get("kind"), "blocked": attempt.get("blocked", []),
                "skipped_buys": attempt.get("skipped_buys", []),
            })
    fast_fees_by_date: dict[str, Decimal] = {}
    for row in fast_fills:
        fill = row["fill"]
        date = fill["executed_at"][:10]
        fast_fees_by_date[date] = fast_fees_by_date.get(date, Decimal(0)) + dec(fill["total_fees"])

    def fast_components(row: dict[str, Any]) -> dict[str, float]:
        equity, cash, invested, dividend, liquidation = ledger_oracle(row)
        return {
            "cash_weight": float(cash / equity),
            "invested_weight": float(invested / equity),
            "dividend_receivable_weight": float(dividend / equity),
            "liquidation_receivable_weight": float(liquidation / equity),
        }

    parity_days = []
    previous_close_equity = dec(parity_fast_definition["fast_event"]["initial_cash"])
    parity_initial_cash = dec(parity_fast_definition["fast_event"]["initial_cash"])
    vector_by_date = {row["session_date"]: row for row in vector_sessions}
    for date in expected_sessions:
        vector = vector_by_date[date]
        fast_open_row = fast_account_by_point[(date, "open_after")]
        fast_close_row = fast_account_by_point[(date, "close")]
        fast_cost = fast_fees_by_date.get(date, Decimal(0)) / previous_close_equity
        vector_components_open = vector.get("portfolio_components")
        vector_components_close = vector.get("portfolio_components_close")
        parity_days.append({
            "session_date": date,
            "vector_open_nav": float(vector["nav"]) / (1 + float(vector["gross_return"])),
            "fast_open_nav": float(ledger_oracle(fast_open_row)[0] / parity_initial_cash),
            "vector_close_nav": vector["nav"],
            "fast_close_nav": float(ledger_oracle(fast_close_row)[0] / parity_initial_cash),
            "vector_open_components": vector_components_open,
            "fast_open_components": fast_components(fast_open_row),
            "vector_close_components": vector_components_close,
            "fast_close_components": fast_components(fast_close_row),
            "vector_cost": vector["cost"],
            "fast_cost": float(fast_cost),
            "vector_trajectory": vector_executions_by_date.get(date, []),
            "fast_trajectory": fast_attempts_by_date.get(date, []),
        })
        previous_close_equity = dec(fast_close_row["equity"])

    mark_run_phase("daily_accounting")
    excluded = main_run["run_spec"].get("execution_exclusions", [])
    forbidden_cells = [cell for cell in excluded if cell.get("state") in {"unknown", "halted"}]
    trades = []
    for row in main_full_tables["fills"]:
        fill = row["fill"]
        order = fill["order"]
        timestamp = fill["executed_at"]
        trades.append({
            "instrument_id": order["instrument_id"],
            "session_date": timestamp[:10],
        })

    valuations = []
    for row in main_account_rows:
        valuations.append({
            "session_date": row["session_date"],
            "valuation_point": row["valuation_point"],
            "cash": row["cash"],
            "dividend_receivable": row["dividend_receivable"],
            "liquidation_receivable": row["liquidation_receivable"],
            "equity": row["equity"],
            "unallocated_capital": row["unallocated_capital"],
            "accounting_verified": row["accounting_verified"],
            "positions": row["positions"],
        })

    all_run_specs = [run["run_spec"] for run in main_full_manifest["runs"]]
    run_spec = all_run_specs[0]
    engine = run_spec["engine"]
    open_prices_by_date = {
        row["session_date"]: {
            instrument: position["price"]
            for instrument, position in row["positions"].items()
        }
        for row in main_account_rows
        if row.get("valuation_point") == "open_after"
    }
    action_rows = []
    for action in main_full_tables["corporate_actions"]:
        enriched = dict(action)
        if action.get("kind") == "share_conversion":
            enriched["valuation_price"] = open_prices_by_date.get(
                action.get("session_date"), {},
            ).get(action.get("instrument_id"))
        action_rows.append(enriched)
    mark_run_phase("reproducibility")
    independent_reports = compare_independent_levels(main_manifests)
    mark_run_phase("python_as_of_and_cli")
    as_of_matches = as_of_decisions_match(
        lake_dirs[0], dsv, pit_inputs[0], vector_manifest,
    )
    mark_run_phase("daily_accounting")
    evidence = {
        "schema": SCHEMA,
        "fixed_universe": fixed_universe_diagnostics(run_spec),
        "identity_change": identity_change(
            acceptance, output, main_results[0]["cli"]["full"],
        ),
        "main_run": {
            "sizing": engine.get("sizing"),
            "rebalance": engine.get("rebalance"),
            "return_basis": run_spec.get("corporate_action_schedule", {}).get("return_basis"),
            "actions": action_rows,
            "terminal_settlement_version": run_spec.get("terminal_settlement", {}).get("semantic_version"),
            "terminal_settlements": main_full_tables["terminal_settlements"],
        },
        "parity": {
            "initial_cash": str(parity_initial_cash),
            "expected_sessions": expected_sessions,
            "days": parity_days,
            "fast_valuations": [
                row for row in fast_accounts if row.get("scope") == "account"
            ],
        },
        "independent_runs": independent_reports,
        "python_cli": {
            "cli": {
                "experiment_id": main_results[0]["cli"]["full"]["exp"],
                "execution_id": main_results[0]["cli"]["full"]["exe"],
                "summary_logical_hash": main_results[0]["cli"]["full"]["summary_logical_hash"],
                "runs": main_results[0]["cli"]["full"]["runs"],
            },
            "python": {
                "experiment_id": main_results[0]["api"]["full"]["experiment_id"],
                "execution_id": main_results[0]["api"]["full"]["execution_id"],
                "summary_logical_hash": main_results[0]["api"]["full"]["summary_logical_hash"],
                "runs": main_results[0]["api"]["full"]["runs"],
            },
            "api_action": main_results[0]["api"]["full"]["action"],
            "as_of_view_matches": as_of_matches,
        },
    "accounting": {
        "expected_sessions": expected_sessions,
        "session_input_count": len(run_spec.get("session_inputs", [])),
            "valuations": valuations,
            "fills": [{
                "session_date": row["fill"]["executed_at"][:10],
                "instrument_id": row["fill"]["order"]["instrument_id"],
                "total_fees": row["fill"]["total_fees"],
                "costs": row["fill"]["costs"],
                "slippage": row["fill"]["slippage"],
            } for row in main_full_tables["fills"]],
            "forbidden_cells": forbidden_cells,
            "trades": trades,
            "fees_present": bool(main_full_tables["fills"]),
        },
        "unknown_coverage": run_spec["unknown_coverage"],
        "raw_immutable": (
            raw_snapshot_unchanged(before, (after := raw_hashes(root, raw_inventory)))
        ),
        "non_blocking": {
            "small_grid": {"status": "not_run", "reason": "P16 non-blocking grid is not needed for the engineering ledger gate."},
            "accurate_smoke": {"status": "not_run", "reason": "No Accurate Event worker was configured for this local acceptance."},
            "legacy_comparison": {"status": "not_run", "reason": "The old-system comparator for the legacy window is not available in this workspace."},
        },
        "source_evidence": {
            "market_normalizer": coverage.get("corporate_action_dataset", {}).get("normalizer"),
            "p21_status": coverage.get("p21", {}).get("status"),
            "source_status": coverage.get("source_status"),
            "historical_capture_status": coverage.get("historical_capture_status"),
            "terms_conclusion": coverage.get("terms_conclusion"),
            "decision_window": {"start": "2023-01-01", "end": "2026-09-30", "preheat": "60 Sessions"},
            "universe_claim": "retrospective_static",
            "survivorship_bias": True,
            "unknown_semantics": "P27-23.5 narrowed UNKNOWN to data defects; no-Bar and unknown rules are excluded.",
            "execution_limitations": [
                "The Session close is the market-data availability time; next Session execution uses the first-trade/open price.",
                "Same-Session sells and buys may not share the real intraday funding order.",
                "Orders submitted before open are assumed to participate after an opening halt resumes; this is unverified.",
                "Termination settlement uses the last in-lifecycle Bar close, not actual liquidation amount or date.",
                "No-Bar Sessions are treated as halted and non-tradable, not UNKNOWN.",
            ],
            "risk_notice_count": len(pit_base.get("risk_notices", [])),
            "cninfo_index_supplied": "cninfo_index" in pit_base,
        },
    }
    verification = verify_artifacts(evidence)
    report = {
        "schema": "mvp6-local-engineering-report@1",
        "issue": 234,
        "classification": "platform engineering acceptance; not strategy historical performance",
        "benchmark": None,
        "verification": verification,
        "local_artifacts": {
            "directory": str(output),
            "rerun_command": "python scripts/mvp6_acceptance.py run --root ~/PrajnaData/mvp-6",
            "frozen_raw_inventory": str(raw_inventory),
        },
        "details": evidence,
    }
    write_json(output / "mvp6-acceptance-report.json", report)
    public_unknown = evidence["unknown_coverage"]["instruments"]
    safe_summary = {
        "passed": verification["passed"],
        "checks": {name: item["passed"] for name, item in verification["checks"].items()},
        "per_etf_unknown_coverage": [
            {"code": item["symbol"], "unknown_cells": item["unknown_cells"],
             "cells": item["cells"],
             "unknown_percent": 100 * item["unknown_cells"] / item["cells"] if item["cells"] else None}
            for item in public_unknown
            if item.get("symbol") in FIXED_UNIVERSE["codes"]
        ],
        "conclusion": "Platform engineering acceptance only; this is not strategy historical performance.",
        "report": str(output / "mvp6-acceptance-report.json"),
    }
    return safe_summary, output


def main() -> int:
    global ACTIVE_POST_MAIN_RUN
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    verify = subparsers.add_parser("verify", help="verify a local or synthetic artifact JSON")
    verify.add_argument("artifacts", type=Path)
    run = subparsers.add_parser("run", help="run the acceptance against the frozen local dataset")
    run.add_argument("--root", default="~/PrajnaData/mvp-6")
    run.add_argument("--pit-input", type=Path)
    run.add_argument("--cli", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/prajna-experiment")
    run.add_argument("--repository", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        if args.command == "verify":
            verified = verify_artifacts(json.loads(args.artifacts.read_text()))
            print(json.dumps(verified, indent=2, sort_keys=True))
            return 0 if verified["passed"] else 1
        summary, _ = run_acceptance(args)
        print(json.dumps(summary, indent=2, sort_keys=True))
        return 0 if summary["passed"] else 1
    except Exception as error:
        if ACTIVE_POST_MAIN_RUN is not None:
            context = ACTIVE_POST_MAIN_RUN
            try:
                summary = write_post_main_failure_report(
                    output=context["output"], acceptance=context["acceptance"],
                    raw_inventory=context["raw_inventory"], unknown=context["unknown"],
                    criterion=context["criterion"], error=str(error), raw_before=context["raw_before"],
                )
                ACTIVE_POST_MAIN_RUN = None
                print(json.dumps(summary, indent=2, sort_keys=True))
                return 1
            except Exception as report_error:
                print(f"MVP-6 acceptance failed while writing phase report: {report_error}", file=sys.stderr)
                return 2
        print(f"MVP-6 acceptance failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
