import importlib.util
import sys
import unittest
from datetime import date
from pathlib import Path


MODULE = Path(__file__).resolve().parents[1] / "scripts" / "audit_execution_status.py"
SPEC = importlib.util.spec_from_file_location("status_audit", MODULE)
status_audit = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = status_audit
SPEC.loader.exec_module(status_audit)


def fact(**overrides):
    value = {
        "instrument_id": "SH:510320", "symbol": "sh510320",
        "effective_from": "2026-01-05", "effective_to": "2026-01-06",
        "trade_status": "TRADABLE", "source": "daily-source",
        "source_ref": "https://source.invalid/file.csv", "raw_sha256": "a" * 64,
        "published_at": None, "available_at": None, "observed_at": "2026-01-06T00:00:00+08:00",
        "verification_status": "verified", "coverage_ref": "coverage:day",
        "kind": "FULL_DAILY", "full_daily_coverage": True,
    }
    value.update(overrides)
    return status_audit.Fact.parse(value)


class ExecutionStatusAuditTests(unittest.TestCase):
    def test_absent_state_and_event_list_silence_are_unknown(self):
        row = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5), [])
        self.assertEqual(row["trade_status"], "UNKNOWN")
        self.assertEqual(row["reason"], "no_complete_daily_state_evidence")
        event_only = fact(kind="EVENT", trade_status="HALTED", full_daily_coverage=False,
                          effective_from="2026-01-06", effective_to="2026-01-07")
        no_event_day = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5), [event_only])
        self.assertEqual(no_event_day["trade_status"], "UNKNOWN")

    def test_halt_interval_is_half_open_and_resume_needs_positive_evidence(self):
        halt = fact(kind="EVENT", trade_status="HALTED", full_daily_coverage=False,
                    effective_from="2026-01-05", effective_to="2026-01-07")
        resumed = fact(effective_from="2026-01-07", effective_to="2026-01-08",
                       source="full-daily-after-resume", raw_sha256="b" * 64)
        at_start = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5), [halt, resumed])
        at_last_halt_day = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 6), [halt, resumed])
        at_resume_boundary = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 7), [halt, resumed])
        self.assertEqual(at_start["trade_status"], "HALTED")
        self.assertEqual(at_last_halt_day["trade_status"], "HALTED")
        self.assertEqual(at_resume_boundary["trade_status"], "TRADABLE")

    def test_intraday_halt_is_preserved_but_not_promoted_to_daily_status(self):
        intraday = fact(kind="EVENT", scope="intraday", trade_status="HALTED",
                        full_daily_coverage=False, intraday_start="09:30", intraday_end="10:30",
                        available_at=None)
        row = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5), [intraday])
        self.assertEqual(row["trade_status"], "UNKNOWN")
        self.assertEqual(row["reason"], "intraday_restriction_not_expressible_in_daily_gate")
        self.assertEqual(row["intraday_restrictions"][0]["to"], "10:30")

    def test_unknown_publication_and_availability_are_not_filled_from_observed_at(self):
        row = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5), [fact()])
        self.assertIsNone(row["published_at"])
        self.assertIsNone(row["available_at"])
        self.assertEqual(row["observed_at"], "2026-01-06T00:00:00+08:00")

    def test_conflicting_sources_and_same_source_conflicts_block(self):
        tradable = fact()
        halted_other = fact(source="event-source", kind="EVENT", trade_status="HALTED",
                            full_daily_coverage=False, raw_sha256="b" * 64)
        conflict = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5),
                                            [tradable, halted_other])
        self.assertEqual(conflict["trade_status"], "CONFLICT")
        same_source_conflict = fact(trade_status="HALTED", raw_sha256="c" * 64)
        row = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5),
                                       [tradable, same_source_conflict])
        self.assertEqual(row["trade_status"], "CONFLICT")

    def test_explicit_unknown_source_does_not_get_overridden_by_tradable_source(self):
        row = status_audit.resolve_day(
            "SH:510320", "sh510320", date(2026, 1, 5),
            [fact(), fact(source="unknown-source", trade_status="UNKNOWN", raw_sha256="d" * 64)],
        )
        self.assertEqual(row["trade_status"], "UNKNOWN")

    def test_exact_duplicate_collapses_but_incomplete_coverage_blocks_execution(self):
        one = fact()
        row = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 5), [one, one])
        self.assertEqual(row["fact_count"], 1)
        unknown = status_audit.resolve_day("SH:510320", "sh510320", date(2026, 1, 6), [one])
        self.assertEqual(unknown["trade_status"], "UNKNOWN")
        with self.assertRaisesRegex(ValueError, "full_daily_coverage"):
            fact(full_daily_coverage=False)

    def test_late_or_unknown_available_time_blocks_execution_readiness(self):
        on_time = status_audit.resolve_day(
            "SH:510320", "sh510320", date(2026, 1, 5),
            [fact(published_at="2026-01-05T09:10:00+08:00",
                  available_at="2026-01-05T09:20:00+08:00")],
        )
        late = status_audit.resolve_day(
            "SH:510320", "sh510320", date(2026, 1, 5),
            [fact(published_at="2026-01-05T09:10:00+08:00",
                  available_at="2026-01-05T09:31:00+08:00")],
        )
        unknown_time = status_audit.resolve_day(
            "SH:510320", "sh510320", date(2026, 1, 5), [fact()],
        )
        self.assertTrue(status_audit.execution_ready([on_time]))
        self.assertFalse(status_audit.execution_ready([late]))
        self.assertFalse(status_audit.execution_ready([unknown_time]))

    def test_missing_original_hash_blocks_execution_readiness(self):
        with self.assertRaisesRegex(ValueError, "source_ref and raw_sha256"):
            status_audit.Fact.parse({
                "instrument_id": "SH:510320", "symbol": "sh510320",
                "effective_from": "2026-01-05", "effective_to": "2026-01-06",
                "trade_status": "TRADABLE", "source": "daily-source",
                "source_ref": "https://source.invalid/file.csv",
                "raw_sha256": None, "kind": "FULL_DAILY", "full_daily_coverage": True,
            })


if __name__ == "__main__":
    unittest.main()
