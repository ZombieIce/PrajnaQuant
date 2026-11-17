import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
with patch.object(sys, "path", [str(ROOT / "scripts"), *sys.path]):
    import income_baseline as baseline

CODE = "sh601398"
DAYS = ["2024-01-02", "2024-01-03"]


class IncomeBaselineTest(unittest.TestCase):
    def event(self, **values):
        return dict({
            "SECURITY_CODE": CODE[2:], "ASSIGN_PROGRESS": "实施分配",
            "EX_DIVIDEND_DATE": DAYS[1], "EQUITY_RECORD_DATE": DAYS[0],
            "PRETAX_BONUS_RMB": "5", "BONUS_RATIO": "0", "IT_RATIO": "0",
        }, **values)

    def test_dividend_conflicts_and_wrong_record_date(self):
        event = self.event()
        self.assertEqual(baseline.normalize_events(CODE, [event, event], DAYS, DAYS[-1]), [])
        issues = baseline.normalize_events(
            CODE, [event, self.event(PRETAX_BONUS_RMB="6", EQUITY_RECORD_DATE=DAYS[1])], DAYS, DAYS[-1],
        )
        self.assertEqual({issue["field"] for issue in issues}, {"EQUITY_RECORD_DATE", "dividend_event"})
        self.assertEqual(baseline.normalize_events(
            CODE, [self.event(IT_RATIO="2")], DAYS, DAYS[-1], cutoff="2024-01-04",
        ), [])
        self.assertEqual(len(baseline.normalize_events(CODE, [self.event(IT_RATIO="2")], DAYS, DAYS[-1])), 1)

    def test_candidate_keeps_raw_fields_and_requires_implemented_status(self):
        event = self.event(IMPL_PLAN_PROFILE="每10股派5元")
        payload = {"code": 0, "result": {"data": [event, self.event(ASSIGN_PROGRESS="预案"),
                                                self.event(SECURITY_CODE="000001")]}}
        self.assertEqual(baseline.candidate_events(CODE, payload, {DAYS[1]}), [event])

    def test_short_warmup_fails_even_if_window_can_be_simulated(self):
        raw = dict.fromkeys(DAYS, (10, 10))
        with self.assertRaises(baseline.InputGaps) as failure:
            baseline.check_warmup(DAYS, {CODE: (raw, {}, {}, {}, {})}, DAYS[0], DAYS[1], {}, {CODE: {}})
        self.assertEqual(failure.exception.gaps[0]["field"], "warmup")

    def test_hand_calculated_round_trip_with_dividend_fee_and_tax(self):
        # Buy 100 × 10 with 5 fee. Before selling, receive 100 × .5.
        # Sell 100 × 11 with 6 fee and 10 dividend tax. End cash 2129.
        daily = [{"day": DAYS[0], "value": 1995}, {"day": DAYS[1], "value": 2129}]
        raw = {DAYS[0]: (10, 10), DAYS[1]: (11, 11)}
        inputs = {CODE: (raw, {DAYS[1]: .5}, {}, {}, raw)}
        path = [{"day": DAYS[0], "equity": 1000, "gross_dividend": 0, "dividend_tax": 0},
                {"day": DAYS[1], "equity": 0, "gross_dividend": 50, "dividend_tax": 10}]
        trades = [{"day": DAYS[0], "side": "core_buy", "shares": 100, "price": 10, "cash_trade": -1005, "tax": 0},
                  {"day": DAYS[1], "side": "sell", "shares": 100, "price": 11, "cash_trade": 1094, "tax": 10}]
        accounts = {CODE: (path, {"fees": 11}, trades)}
        fills, audited = baseline.ledger(DAYS, daily, inputs, accounts, [], 2000)
        self.assertEqual([fill["fee"] for fill in fills], [5, 6])
        self.assertEqual([row["legacy_book_balance"] for row in audited], [995, 2129])
        self.assertEqual([row["holdings"][CODE] for row in audited], [100, 0])
        path[1]["dividend_tax"] = 0
        with self.assertRaisesRegex(ValueError, "dividend tax"):
            baseline.ledger(DAYS, daily, inputs, accounts, [], 2000)

    def test_ex_date_income_funding_is_reported_as_legacy_book_balance(self):
        daily = [{"day": DAYS[0], "value": 1000}, {"day": DAYS[1], "value": 2000,
                 "cash": 0, "stock_values": {CODE: 2000}}]
        raw = dict.fromkeys(DAYS, (10, 10))
        inputs = {CODE: (raw, {DAYS[1]: 10}, {}, {}, raw)}
        path = [{"day": DAYS[0], "equity": 1000, "gross_dividend": 0, "dividend_tax": 0},
                {"day": DAYS[1], "equity": 2000, "gross_dividend": 1000, "dividend_tax": 0}]
        trades = [{"day": DAYS[0], "side": "core_buy", "shares": 100, "price": 10, "cash_trade": -1000, "tax": 0},
                  {"day": DAYS[1], "side": "core_buy", "shares": 100, "price": 10, "cash_trade": -1000, "tax": 0}]
        _, audited = baseline.ledger(DAYS, daily, inputs, {CODE: (path, {"fees": 0}, trades)}, [], 1000)
        self.assertEqual(audited[1]["legacy_book_balance"], 0)
        self.assertNotIn("cash", audited[1])
        self.assertNotIn("available_cash", audited[1])
        self.assertEqual(daily[1]["cash"], 0)

    def test_archives_relative_imports_and_package_initializers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "source"
            files = {
                "book/__init__.py": "from .registry import marker",
                "book/registry.py": "marker = 0",
                "book/ledger/__init__.py": "from .. import registry",
                "book/ledger/check.py": "from .fees import fee",
                "book/ledger/fees.py": "fee = 1",
                "book/unused.py": "",
            }
            for name, source in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            dest = Path(directory) / "archive"
            with patch.object(baseline, "read_revision", return_value=b"from book.ledger import check"):
                baseline.archive_source(root, dest)
            expected = (set(files) - {"book/unused.py"}) | {"strategy-dashboard/daily_update.py"}
            self.assertEqual(set(baseline.digest_files(dest)), expected)
            result = subprocess.run(
                [sys.executable, "-B", "-I", "-c",
                 "import sys; sys.path.insert(0, sys.argv[1]); from book.ledger import check; assert check.fee == 1",
                 dest], capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_reference_comparison_checks_date_axes_and_lists_unavailable_fields(self):
        stats = {"start": DAYS[0], "end": DAYS[1], "cagr": .1}
        reference = {"history": {"income": [[DAYS[0], 1], [DAYS[1], 1.1]]}, "stats": {"income": stats}}
        matching = (reference["history"]["income"], stats, [], [], {})
        comparison = baseline.compare(reference, matching)
        self.assertTrue(comparison["match"])
        self.assertIn("legacy_book_balance", comparison["reference_unavailable"])
        self.assertFalse(baseline.compare(reference, ([[DAYS[1], 1]], stats, [], [], {}))["match"])

    def test_cli_freeze_persists_pending_missing_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dashboard = root / "legacy" / "strategy-dashboard"
            dashboard.mkdir(parents=True)
            (dashboard / "daily_update.py").write_text("# synthetic legacy entry")
            (dashboard / "data.json").write_text(json.dumps({
                "history": {"income": [[DAYS[1], 1.0]]},
                "stats": {"income": {"start": DAYS[1], "end": DAYS[1]}},
            }))
            for command in (
                ["init", "-q"], ["add", "daily_update.py", "data.json"],
                ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "-qm", "synthetic dashboard"],
                ["branch", baseline.REVISION],
            ):
                subprocess.run(["git", "-C", dashboard, *command], check=True, capture_output=True)
            snapshot = root / "snapshot"
            result = subprocess.run([
                sys.executable, str(ROOT / "scripts/income_baseline.py"), "freeze",
                "--legacy-root", dashboard.parent, "--snapshot", snapshot,
            ], capture_output=True)
            self.assertEqual(result.returncode, 2, result.stderr)
            pending = json.loads((snapshot / "pending-gaps.json").read_bytes())
            self.assertEqual(pending["status"], "pending_decision")
            self.assertIn(baseline.CALENDAR, {gap["asset"] for gap in pending["gaps"]})
            self.assertFalse((snapshot / "manifest.json").exists())

    def test_cli_rejects_pending_or_unsealed_snapshot_without_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            snapshot = root / "snapshot"
            snapshot.mkdir()
            (snapshot / "pending-gaps.json").write_text('{"status":"pending_decision"}')
            output = root / "result.json"
            result = subprocess.run([sys.executable, str(ROOT / "scripts/income_baseline.py"), "replay",
                                     "--snapshot", snapshot, "--output", output], capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertFalse(output.exists())
