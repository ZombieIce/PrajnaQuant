#!/usr/bin/env python3
"""Build a conservative, versioned daily execution-status audit from evidence facts.

Input facts are normalized source observations, not inferred from bars. A fact can
only assert TRADABLE/HALTED when its scope is explicitly full_day. Intraday events
are retained as restrictions but map to UNKNOWN in this daily model.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
from collections import defaultdict
from pathlib import Path
from typing import Any

VERSION = "prajna-etf-status-evidence-v1"
WINDOW_START = dt.date(2025, 9, 23)
WINDOW_END = dt.date(2026, 9, 21)
SECURITIES = {
    "513300": ("SH:513300", "sh513300", "SSE"),
    "518880": ("SH:518880", "sh518880", "SSE"),
    "510320": ("SH:510320", "sh510320", "SSE"),
    "159612": ("SZ:159612", "sz159612", "SZSE"),
    "159952": ("SZ:159952", "sz159952", "SZSE"),
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _date(value: str) -> dt.date:
    return dt.date.fromisoformat(value)


def build_snapshot(
    expected_dates: list[str], facts: list[dict[str, Any]], observed_at: str,
    coverage_verified: bool = False,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Return (daily status snapshot, coverage report); never infer status from absence."""
    parsed_expected = [_date(d) for d in expected_dates]
    if len(parsed_expected) != len(set(parsed_expected)):
        raise ValueError("expected trading calendar contains duplicate dates")
    dates = sorted(d for d in parsed_expected if WINDOW_START <= d <= WINDOW_END)
    if not dates:
        raise ValueError("no expected trading dates in the fixed validation window")

    applicable: dict[tuple[str, dt.date], list[dict[str, Any]]] = defaultdict(list)
    errors: list[str] = []
    for index, fact in enumerate(facts):
        code = str(fact.get("code", ""))
        if code not in SECURITIES:
            errors.append(f"fact[{index}]: out-of-scope security {code!r}")
            continue
        try:
            start = _date(fact["effective_from"])
            end = _date(fact["effective_to"]) if fact.get("effective_to") else start + dt.timedelta(days=1)
        except (KeyError, TypeError, ValueError):
            errors.append(f"fact[{index}]: invalid effective interval")
            continue
        if end <= start:
            errors.append(f"fact[{index}]: effective_to must be exclusive and after effective_from")
            continue
        if fact.get("trade_status") not in ("TRADABLE", "HALTED"):
            errors.append(f"fact[{index}]: event fact status must be TRADABLE or HALTED")
            continue
        if fact.get("scope") not in ("full_day", "intraday"):
            errors.append(f"fact[{index}]: scope must be full_day or intraday")
            continue
        if fact.get("available_at") and not fact.get("published_at"):
            errors.append(f"fact[{index}]: available_at cannot be known without published_at evidence")
            continue
        for day in dates:
            if start <= day < end:
                applicable[(code, day)].append(fact)

    rows: list[dict[str, Any]] = []
    cell_counts: dict[str, dict[str, int]] = {code: defaultdict(int) for code in SECURITIES}
    conflicts: list[dict[str, Any]] = []
    duplicates: list[dict[str, Any]] = []
    for day in dates:
        for code, (instrument_id, symbol, market) in SECURITIES.items():
            source_facts = applicable[(code, day)]
            keyed = defaultdict(list)
            for fact in source_facts:
                key = (
                    fact.get("source", ""), fact.get("source_ref", ""),
                    fact.get("effective_from", ""), fact.get("effective_to", ""),
                    fact.get("trade_status", ""), fact.get("scope", ""),
                    fact.get("intraday_start", ""), fact.get("intraday_end", ""),
                )
                keyed[key].append(fact)
            unique = [copies[0] for copies in keyed.values()]
            if any(len(copies) > 1 for copies in keyed.values()):
                duplicates.append({"code": code, "trade_date": day.isoformat(), "duplicate_fact_groups": sum(len(c) - 1 for c in keyed.values())})
            open_cutoff = dt.datetime.combine(
                day, dt.time(9, 30), tzinfo=dt.timezone(dt.timedelta(hours=8))
            )
            full_day_facts = [fact for fact in unique if fact.get("scope") == "full_day"]
            available_facts = []
            unavailable_count = 0
            for fact in full_day_facts:
                available_at = fact.get("available_at")
                try:
                    available = dt.datetime.fromisoformat(available_at) if available_at else None
                    if available is not None and available.tzinfo is not None and available <= open_cutoff:
                        available_facts.append(fact)
                    else:
                        unavailable_count += 1
                except (TypeError, ValueError):
                    unavailable_count += 1
            statuses = {fact["trade_status"] for fact in available_facts}
            if len(statuses) > 1:
                status = "CONFLICT"
                conflicts.append({"code": code, "trade_date": day.isoformat(), "statuses": sorted(statuses)})
                reason = "conflicting full-day facts available by execution open"
            elif statuses:
                status = next(iter(statuses))
                reason = "full-day source fact"
            elif any(f.get("scope") == "intraday" for f in unique):
                status = "UNKNOWN"
                reason = "intraday restriction cannot be represented by daily status"
            elif unavailable_count:
                status = "UNKNOWN"
                reason = "full-day fact lacks proven availability by 09:30 Shanghai execution cutoff"
            else:
                status = "UNKNOWN"
                reason = "no complete full-day status evidence"
            # A current-only source can inform only its query date, never historical dates.
            current_only = [f for f in unique if f.get("historical_scope") == "current_only"]
            if current_only:
                status = "UNKNOWN"
                reason = "current-only query is not historical execution-state evidence"
            cell_counts[code][status] += 1
            rows.append({
                "instrument_id": instrument_id,
                "symbol": symbol,
                "code": code,
                "market": market,
                "trade_date": day.isoformat(),
                "trade_status": status,
                "is_tradable": status == "TRADABLE",
                "reason": reason,
                "sources": sorted({f.get("source", "") for f in unique if f.get("source")}),
                "source_refs": sorted({f.get("source_ref", "") for f in unique if f.get("source_ref")}),
                "raw_sha256": sorted({f.get("raw_sha256", "") for f in unique if f.get("raw_sha256")}),
                "published_at": sorted({f.get("published_at", "") for f in unique if f.get("published_at")}),
                "available_at": sorted({f.get("available_at", "") for f in unique if f.get("available_at")}),
                "observed_at": observed_at,
                "execution_status_covered": status in ("TRADABLE", "HALTED"),
                "intraday_restrictions": [
                    {"from": f.get("intraday_start"), "to": f.get("intraday_end"), "trade_status": f.get("trade_status"), "source_ref": f.get("source_ref")}
                    for f in unique if f.get("scope") == "intraday"
                ],
            })

    all_cells_known = all(
        cell_counts[code]["UNKNOWN"] == 0 and cell_counts[code]["CONFLICT"] == 0
        for code in SECURITIES
    )
    coverage_status = (
        "complete" if all_cells_known and coverage_verified
        else "unverified" if all_cells_known
        else "gaps"
    )
    coverage = {
        "schema_version": VERSION,
        "window": {"start": WINDOW_START.isoformat(), "end": WINDOW_END.isoformat()},
        "expected_dates_source": "caller-supplied exchange calendar; bars are not used as a calendar or status source",
        "expected_trading_dates": len(dates),
        "source_integrity": "verified" if coverage_verified else "unverified",
        "coverage_status": coverage_status,
        "securities": {
            code: {
                "instrument_id": identity[0], "symbol": identity[1], "market": identity[2],
                "expected_dates": len(dates), "counts_by_status": dict(cell_counts[code]),
                "known_full_day_dates": sum(cell_counts[code][s] for s in ("TRADABLE", "HALTED")),
                "unknown_dates": cell_counts[code]["UNKNOWN"],
                "conflict_dates": cell_counts[code]["CONFLICT"],
            }
            for code, identity in SECURITIES.items()
        },
        "conflicts": conflicts,
        "duplicate_facts": duplicates,
        "normalization_errors": errors,
        "limitations": [
            "No TRADABLE value is inferred from a bar, event-list silence, or missing record.",
            "Intraday restrictions are preserved, but daily open execution cannot model the restriction interval.",
            "published_at and available_at stay empty when their historical publication/availability instant is not evidenced.",
        ],
    }
    snapshot = {
        "schema_version": VERSION,
        "created_at": observed_at,
        "window": coverage["window"],
        "calendar_dates": [d.isoformat() for d in dates],
        "status_rows": rows,
        "source_facts": facts,
        "coverage_ref": "status-coverage-report.json",
    }
    return snapshot, coverage


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--calendar", required=True, type=Path, help="JSON array of expected YYYY-MM-DD trading dates")
    parser.add_argument("--facts", required=True, type=Path, help="JSON array of normalized source facts")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    calendar = json.loads(args.calendar.read_text())
    facts = json.loads(args.facts.read_text())
    if not isinstance(calendar, list) or not isinstance(facts, list):
        raise SystemExit("calendar and facts JSON roots must be arrays")
    observed_at = dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()
    snapshot, coverage = build_snapshot(calendar, facts, observed_at)
    args.output.mkdir(parents=True, exist_ok=True)
    snapshot_path = args.output / "status-snapshot-v1.json"
    coverage_path = args.output / "status-coverage-report.json"
    snapshot_path.write_text(json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n")
    coverage_path.write_text(json.dumps(coverage, ensure_ascii=False, indent=2) + "\n")
    metadata = {
        "schema_version": VERSION,
        "snapshot": snapshot_path.name,
        "snapshot_sha256": sha256_file(snapshot_path),
        "coverage_report": coverage_path.name,
        "coverage_report_sha256": sha256_file(coverage_path),
        "observed_at": observed_at,
        "coverage_status": coverage["coverage_status"],
        "expected_status_rows": len(snapshot["status_rows"]),
    }
    (args.output / "manifest.json").write_text(json.dumps(metadata, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(metadata, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
