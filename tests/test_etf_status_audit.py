import datetime as dt
import importlib.util
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("audit_etf_status", ROOT / "scripts/audit_etf_status.py")
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)


def fact(**overrides):
    result = {
        "code": "513300",
        "source": "exchange_daily_status",
        "source_ref": "fixture://state",
        "raw_sha256": "a" * 64,
        "published_at": "2026-09-18T08:00:00+08:00",
        "available_at": "2026-09-18T08:00:00+08:00",
        "effective_from": "2026-09-18",
        "effective_to": "2026-09-19",
        "trade_status": "TRADABLE",
        "scope": "full_day",
    }
    result.update(overrides)
    return result


class StatusAuditTests(unittest.TestCase):
    def setUp(self):
        self.dates = ["2025-09-23", "2025-09-24", "2025-09-25", "2026-09-18"]

    def row(self, rows, code, day):
        return next(r for r in rows if r["code"] == code and r["trade_date"] == day)

    def test_missing_source_cells_remain_unknown_and_fail_closed(self):
        snapshot, report = audit.build_snapshot(self.dates, [], "2026-09-24T00:00:00+00:00")
        self.assertEqual(len(snapshot["status_rows"]), 20)
        self.assertEqual(report["coverage_status"], "gaps")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2025-09-23")["trade_status"], "UNKNOWN")
        self.assertFalse(self.row(snapshot["status_rows"], "513300", "2025-09-23")["is_tradable"])

    def test_half_open_halt_and_resume_boundary(self):
        facts = [
            fact(effective_from="2025-09-23", effective_to="2025-09-25", trade_status="HALTED", published_at="2025-09-22T18:00:00+08:00", available_at="2025-09-22T18:00:00+08:00"),
            fact(effective_from="2025-09-25", effective_to="2025-09-26", trade_status="TRADABLE", published_at="2025-09-24T18:00:00+08:00", available_at="2025-09-24T18:00:00+08:00"),
        ]
        snapshot, _ = audit.build_snapshot(self.dates, facts, "now")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2025-09-23")["trade_status"], "HALTED")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2025-09-24")["trade_status"], "HALTED")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2025-09-25")["trade_status"], "TRADABLE")

    def test_intraday_halt_is_preserved_but_daily_status_stays_unknown(self):
        event = fact(
            published_at="2026-09-17", available_at=None,
            effective_from="2026-09-18", effective_to="2026-09-19",
            trade_status="HALTED", scope="intraday", intraday_start="09:30", intraday_end="10:30",
        )
        snapshot, report = audit.build_snapshot(self.dates, [event], "2026-09-24T00:00:00+00:00")
        row = self.row(snapshot["status_rows"], "513300", "2026-09-18")
        self.assertEqual(row["trade_status"], "UNKNOWN")
        self.assertEqual(row["intraday_restrictions"][0]["to"], "10:30")
        self.assertEqual(row["available_at"], [])
        self.assertEqual(report["coverage_status"], "gaps")

    def test_conflict_preserves_all_facts_and_marks_conflict(self):
        snapshot, report = audit.build_snapshot(self.dates, [fact(), fact(source="other", trade_status="HALTED")], "now")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2026-09-18")["trade_status"], "CONFLICT")
        self.assertEqual(len(report["conflicts"]), 1)

    def test_exact_duplicate_is_deduped_but_audited(self):
        item = fact()
        snapshot, report = audit.build_snapshot(self.dates, [item, dict(item)], "now")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2026-09-18")["trade_status"], "TRADABLE")
        self.assertEqual(len(report["duplicate_facts"]), 1)

    def test_late_announcement_does_not_backfill_available_at(self):
        late = fact(published_at="2026-09-19T09:00:00+08:00", available_at="2026-09-19T09:00:00+08:00")
        snapshot, _ = audit.build_snapshot(self.dates, [late], "now")
        row = self.row(snapshot["status_rows"], "513300", "2026-09-18")
        self.assertEqual(row["available_at"], ["2026-09-19T09:00:00+08:00"])
        self.assertEqual(row["trade_status"], "UNKNOWN")
        self.assertGreater(dt.datetime.fromisoformat(row["available_at"][0]), dt.datetime.fromisoformat("2026-09-18T09:30:00+08:00"))

    def test_full_day_state_without_historical_available_at_stays_unknown(self):
        un_timed = fact(available_at=None)
        snapshot, _ = audit.build_snapshot(self.dates, [un_timed], "now")
        row = self.row(snapshot["status_rows"], "513300", "2026-09-18")
        self.assertEqual(row["trade_status"], "UNKNOWN")
        self.assertEqual(row["reason"], "full-day fact lacks proven availability by 09:30 Shanghai execution cutoff")

    def test_current_only_query_never_fills_historical_dates(self):
        current = fact(historical_scope="current_only", query_date="2026-09-18")
        snapshot, _ = audit.build_snapshot(self.dates, [current], "now")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2025-09-23")["trade_status"], "UNKNOWN")
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2026-09-18")["trade_status"], "UNKNOWN")

    def test_no_unknown_rows_still_needs_coverage_attestation(self):
        facts = []
        for code in audit.SECURITIES:
            for day in self.dates:
                available = day + "T08:00:00+08:00"
                facts.append(fact(code=code, effective_from=day, effective_to=(dt.date.fromisoformat(day) + dt.timedelta(days=1)).isoformat(), published_at=available, available_at=available))
        _, unverified = audit.build_snapshot(self.dates, facts, "now")
        _, verified_fixture = audit.build_snapshot(self.dates, facts, "now", coverage_verified=True)
        self.assertEqual(unverified["coverage_status"], "unverified")
        self.assertEqual(verified_fixture["coverage_status"], "complete")

    def test_invalid_scope_is_reported_and_cannot_become_tradable(self):
        snapshot, report = audit.build_snapshot(self.dates, [fact(scope="event_list")], "now")
        self.assertEqual(report["normalization_errors"], ["fact[0]: scope must be full_day or intraday"])
        self.assertEqual(self.row(snapshot["status_rows"], "513300", "2026-09-18")["trade_status"], "UNKNOWN")


if __name__ == "__main__":
    unittest.main()
