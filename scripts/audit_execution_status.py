#!/usr/bin/env python3
"""Fail-closed normalizer for versioned daily execution-status evidence.

Input is JSONL: one source fact per line.  Event-list sources can prove a
halt interval when an event exists, but their silence never proves TRADABLE.
Only a full-daily fact with an explicit per-security/date coverage assertion
can prove either daily state.  Source publication/availability timestamps are
preserved verbatim; missing values remain null.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from dataclasses import dataclass
from datetime import date, datetime, time, timezone, timedelta
from pathlib import Path
from typing import Any


STATUSES = {"TRADABLE", "HALTED", "UNKNOWN"}
SHANGHAI = timezone(timedelta(hours=8))


@dataclass(frozen=True)
class Fact:
    instrument_id: str
    symbol: str
    effective_from: date
    effective_to: date
    trade_status: str
    source: str
    source_ref: str
    raw_sha256: str
    published_at: str | None
    available_at: str | None
    observed_at: str | None
    verification_status: str
    coverage_ref: str | None
    kind: str
    full_daily_coverage: bool
    scope: str
    intraday_start: str | None
    intraday_end: str | None

    @classmethod
    def parse(cls, obj: dict[str, Any]) -> "Fact":
        start = date.fromisoformat(obj["effective_from"])
        end = date.fromisoformat(obj["effective_to"])
        if end <= start:
            raise ValueError("effective_to must be after effective_from (half-open)")
        status = obj["trade_status"]
        if status not in STATUSES:
            raise ValueError(f"invalid trade_status: {status}")
        kind = obj["kind"]
        if kind not in {"FULL_DAILY", "EVENT"}:
            raise ValueError(f"invalid kind: {kind}")
        digest = obj.get("raw_sha256")
        if digest is not None and (len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest)):
            raise ValueError("raw_sha256 must be null or lowercase SHA-256 hex")
        if kind == "EVENT" and status == "TRADABLE":
            raise ValueError("event-list silence/positive event cannot assert TRADABLE")
        if kind == "FULL_DAILY" and obj.get("full_daily_coverage") is not True:
            raise ValueError("FULL_DAILY facts require an explicit full_daily_coverage assertion")
        if kind == "FULL_DAILY" and (not obj.get("source_ref") or digest is None):
            raise ValueError("FULL_DAILY facts require a source_ref and raw_sha256")
        scope = obj.get("scope", "full_day")
        if scope not in {"full_day", "intraday"}:
            raise ValueError(f"invalid scope: {scope}")
        if kind == "FULL_DAILY" and scope != "full_day":
            raise ValueError("FULL_DAILY facts must have full_day scope")
        return cls(
            instrument_id=obj["instrument_id"], symbol=obj["symbol"],
            effective_from=start, effective_to=end, trade_status=status,
            source=obj["source"], source_ref=obj["source_ref"], raw_sha256=digest,
            published_at=obj.get("published_at"), available_at=obj.get("available_at"),
            observed_at=obj.get("observed_at"),
            verification_status=obj.get("verification_status", "unverified"),
            coverage_ref=obj.get("coverage_ref"), kind=kind,
            full_daily_coverage=obj.get("full_daily_coverage") is True,
            scope=scope, intraday_start=obj.get("intraday_start"),
            intraday_end=obj.get("intraday_end"),
        )

    def identity(self) -> tuple[Any, ...]:
        return (
            self.instrument_id, self.symbol, self.effective_from, self.effective_to,
            self.trade_status, self.source, self.source_ref, self.raw_sha256,
            self.published_at, self.available_at, self.kind, self.scope,
            self.intraday_start, self.intraday_end,
        )


def resolve_day(instrument_id: str, symbol: str, trade_day: date,
                facts: list[Fact]) -> dict[str, Any]:
    relevant = [f for f in facts if f.instrument_id == instrument_id
                and f.symbol == symbol and f.effective_from <= trade_day < f.effective_to]
    unique = {f.identity(): f for f in relevant}
    relevant = list(unique.values())
    daily = [f for f in relevant if f.kind == "FULL_DAILY" and f.full_daily_coverage]
    events = [f for f in relevant if f.kind == "EVENT"]
    daily_states = {f.trade_status for f in daily}
    statuses = {f.trade_status for f in daily + events if f.trade_status != "UNKNOWN"}
    has_unknown_fact = any(f.trade_status == "UNKNOWN" for f in relevant)
    conflict = len(statuses) > 1
    if conflict:
        status, reason = "CONFLICT", "source_conflict"
    elif has_unknown_fact:
        status, reason = "UNKNOWN", "source_reports_unknown"
    elif daily:
        status = next(iter(daily_states)) if len(daily_states) == 1 else "CONFLICT"
        reason = "full_daily_source" if status != "CONFLICT" else "same_source_conflict"
    elif any(f.scope == "intraday" for f in events):
        status, reason = "UNKNOWN", "intraday_restriction_not_expressible_in_daily_gate"
    elif events and statuses == {"HALTED"}:
        status, reason = "HALTED", "halt_event_interval"
    else:
        status, reason = "UNKNOWN", "no_complete_daily_state_evidence"
    return {
        "instrument_id": instrument_id, "symbol": symbol,
        "trade_date": trade_day.isoformat(), "trade_status": status,
        "is_tradable": status == "TRADABLE", "reason": reason,
        "sources": ";".join(sorted({f.source for f in relevant})) or None,
        "source_refs": ";".join(sorted({f.source_ref for f in relevant})) or None,
        "raw_sha256": ";".join(sorted({f.raw_sha256 for f in relevant if f.raw_sha256})) or None,
        "published_at": ";".join(sorted({f.published_at or "" for f in relevant})) or None,
        "available_at": ";".join(sorted({f.available_at or "" for f in relevant})) or None,
        "observed_at": ";".join(sorted({f.observed_at or "" for f in relevant})) or None,
        "verification_status": "unverified" if not relevant or status in {"UNKNOWN", "CONFLICT"}
            else ";".join(sorted({f.verification_status for f in relevant})),
        "intraday_restrictions": [
            {"from": f.intraday_start, "to": f.intraday_end,
             "source_ref": f.source_ref, "verification_status": f.verification_status}
            for f in relevant if f.scope == "intraday"
        ],
        "fact_count": len(relevant),
    }


def audit(dates: list[date], securities: list[tuple[str, str]], facts: list[Fact]
          ) -> list[dict[str, Any]]:
    return [resolve_day(iid, symbol, day, facts)
            for iid, symbol in securities for day in dates]


def execution_ready(rows: list[dict[str, Any]]) -> bool:
    """Require a verified daily state available by that session's 09:30 CST."""
    if not rows:
        return False
    for row in rows:
        verification = (row["verification_status"] or "").split(";")
        if row["trade_status"] not in {"TRADABLE", "HALTED"} or not verification or any(
            value != "verified" for value in verification
        ):
            return False
        if not row["source_refs"] or not row["raw_sha256"]:
            return False
        try:
            cutoff = datetime.combine(date.fromisoformat(row["trade_date"]), time(9, 30), SHANGHAI)
            if not row["published_at"] or not row["available_at"]:
                return False
            available_values = [datetime.fromisoformat(value) for value in row["available_at"].split(";")]
            published_values = [datetime.fromisoformat(value) for value in row["published_at"].split(";")]
            timestamps = available_values + published_values
            if not timestamps or any(value.tzinfo is None for value in timestamps):
                return False
            if any(value.astimezone(timezone.utc) > cutoff.astimezone(timezone.utc) for value in timestamps):
                return False
            hashes = row["raw_sha256"].split(";")
            if any(len(value) != 64 or any(c not in "0123456789abcdef" for c in value) for value in hashes):
                return False
        except (TypeError, ValueError):
            return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dates", required=True, type=Path,
                        help="CSV with trade_date column from the frozen official calendar")
    parser.add_argument("--facts", required=True, type=Path,
                        help="source facts JSONL; use an empty file when no facts were obtained")
    parser.add_argument("--securities", required=True, type=Path,
                        help="CSV with instrument_id,symbol columns")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    with args.dates.open(newline="", encoding="utf-8") as stream:
        dates = [date.fromisoformat(row["trade_date"]) for row in csv.DictReader(stream)]
    with args.securities.open(newline="", encoding="utf-8") as stream:
        securities = [(r["instrument_id"], r["symbol"]) for r in csv.DictReader(stream)]
    facts = []
    for line_no, line in enumerate(args.facts.read_text(encoding="utf-8").splitlines(), 1):
        if line.strip():
            try:
                facts.append(Fact.parse(json.loads(line)))
            except Exception as exc:
                raise SystemExit(f"invalid fact on line {line_no}: {exc}") from exc
    rows = audit(dates, securities, facts)
    args.out.mkdir(parents=True, exist_ok=True)
    output = args.out / "daily_status.csv"
    with output.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]) if rows else ["trade_date"])
        writer.writeheader()
        writer.writerows(rows)
    raw_hash = hashlib.sha256(args.facts.read_bytes()).hexdigest()
    ready = execution_ready(rows)
    summary = {
        "schema_version": 1, "row_count": len(rows),
        "dates": len(dates), "securities": len(securities),
        "fact_count_after_duplicate_collapse": len({f.identity() for f in facts}),
        "coverage_status": "complete" if ready else "gaps",
        "source_integrity": "unverified",
        "status_counts": {s: sum(r["trade_status"] == s for r in rows)
                          for s in ("TRADABLE", "HALTED", "UNKNOWN", "CONFLICT")},
        "source_facts_sha256": raw_hash,
        "daily_status_sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
        "published_at": None, "available_at": None,
        "history_availability": "unknown unless source evidence explicitly supplies it",
        "execution_ready": ready,
    }
    (args.out / "manifest.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n",
                                              encoding="utf-8")
    print(json.dumps(summary, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
