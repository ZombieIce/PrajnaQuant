import json
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from zipfile import ZipFile

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

    def test_internal_price_gaps_split_warmup_and_run_without_flagging_prior_listing(self):
        days = ["2024-01-02", "2024-01-03", "2024-01-04", "2024-01-05", "2024-01-08"]
        inputs = {CODE: (dict.fromkeys((days[0], days[2], days[-1]), (10, 10)),),
                  "sh600941": (dict.fromkeys(days[2:], (10, 10)),)}
        gaps = baseline.session_price_gaps(days, inputs, days[2])
        self.assertEqual([(gap["asset"], gap["date"], gap["field"]) for gap in gaps],
                         [(CODE, days[1], "daily_bar"), (CODE, days[3], "daily_bar")])
        self.assertIn("预热", gaps[0]["impact"])
        self.assertIn("估值", gaps[1]["impact"])

    def make_suspension_evidence(self, directory):
        evidence = {"method": "a-stock-data 3.10.4 §6.5", "rows": [{
            "code": CODE, "date": DAYS[1], "baostock": {
                "code": "sh.601398", "date": DAYS[1], "tradestatus": "0", "volume": "0", "amount": "",
            }, "tencent": None, "tdx": None,
        }]}
        raw = directory / "batch" / f"baostock-{CODE}.json"
        raw.parent.mkdir(parents=True)
        record = evidence["rows"][0]["baostock"]
        raw.write_text(json.dumps({"error_code": "0", "fields": list(record), "rows": [list(record.values())]}))
        evidence["sources"] = {f"batch/{raw.name}": baseline.sha256_file(raw)}
        file = directory / "evidence.json"
        file.write_text(json.dumps(evidence))
        return file, evidence

    def test_suspension_evidence_requires_explicit_matched_status_without_trades(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence_file, evidence = self.make_suspension_evidence(root / "original")
            snapshot = root / "snapshot"
            baseline.archive_suspension_evidence(evidence_file, snapshot)
            file = snapshot / "suspension-evidence.json"
            self.assertEqual(baseline.suspension_days(snapshot), {(CODE, DAYS[1])})
            evidence["rows"][0]["tencent"] = [DAYS[1], 10, 10, 10, 10, 1]
            file.write_text(json.dumps(evidence))
            with self.assertRaisesRegex(ValueError, "does not match archived"):
                baseline.suspension_days(snapshot)
            evidence["rows"][0]["tencent"] = None
            evidence["rows"][0]["baostock"]["tradestatus"] = ""
            file.write_text(json.dumps(evidence))
            with self.assertRaisesRegex(ValueError, "does not match archived"):
                baseline.suspension_days(snapshot)

    def test_derived_suspension_cannot_override_a_successful_baostock_response(self):
        for discrepancy in ("traded", "absent", "unsuccessful"):
            with self.subTest(discrepancy=discrepancy), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                evidence_file, evidence = self.make_suspension_evidence(root / "original")
                relative = next(iter(evidence["sources"]))
                raw = evidence_file.parent / relative
                payload = json.loads(raw.read_bytes())
                if discrepancy == "traded":
                    payload["rows"][0][payload["fields"].index("tradestatus")] = "1"
                    payload["rows"][0][payload["fields"].index("volume")] = "100"
                elif discrepancy == "absent":
                    payload["rows"] = []
                else:
                    payload["error_code"] = "error"
                raw.write_text(json.dumps(payload))
                evidence["sources"][relative] = baseline.sha256_file(raw)
                evidence_file.write_text(json.dumps(evidence))
                snapshot = root / "snapshot"
                baseline.archive_suspension_evidence(evidence_file, snapshot)
                with self.assertRaisesRegex(ValueError, "archived source|BaoStock source"):
                    baseline.suspension_days(snapshot)

    def test_derived_suspension_cannot_hide_archived_cross_source_trades(self):
        day = DAYS[1]
        for provider in ("tencent", "tdx"):
            with self.subTest(provider=provider), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                evidence_file, evidence = self.make_suspension_evidence(root / "original")
                if provider == "tencent":
                    raw = evidence_file.parent / f"tencent-{CODE}-{day}-0.json"
                    raw.write_text(json.dumps({"code": 0, "data": {CODE: {"day": [[day, 10, 10, 10, 10, 5]]}}}))
                else:
                    ymd = day.replace("-", "")
                    raw = evidence_file.parent / f"tdx-{ymd}.zip"
                    code_table = bytearray(150)
                    code_table[:6] = CODE[2:].encode("ascii")
                    code_table[40:44] = b"Test"
                    bars = bytearray(512)
                    struct.pack_into("<5d", bars, 4, 10, 10, 10, 10, 10)
                    struct.pack_into("<Q", bars, 56, 100)
                    struct.pack_into("<d", bars, 72, 1000)
                    with ZipFile(raw, "w") as archive:
                        archive.writestr(f"sh{ymd[2:]}.cod", code_table)
                        archive.writestr(f"sh{ymd[2:]}.md1", bars)
                evidence["sources"][raw.name] = baseline.sha256_file(raw)
                evidence_file.write_text(json.dumps(evidence))
                snapshot = root / "snapshot"
                baseline.archive_suspension_evidence(evidence_file, snapshot)
                with self.assertRaisesRegex(ValueError, "does not match archived"):
                    baseline.suspension_days(snapshot)
                # Even a faithfully derived row may not classify a day with trades as suspended.
                parser = baseline.raw_tencent(raw, CODE) if provider == "tencent" else baseline.raw_tdx(
                    raw, ymd, {(CODE, day)},
                )
                evidence["rows"][0][provider] = next(parser)[1]
                (snapshot / "suspension-evidence.json").write_text(json.dumps(evidence))
                with self.assertRaisesRegex(baseline.InputGaps, "turnover conflicts"):
                    baseline.suspension_days(snapshot)

    def test_replay_binds_original_suspension_sources_after_snapshot_move(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "original"
            evidence_file, _ = self.make_suspension_evidence(original)
            snapshot = root / "snapshot"
            baseline.archive_suspension_evidence(evidence_file, snapshot)
            for script in ("income_baseline.py", "eternal_baseline.py"):
                target = snapshot / "source" / script
                target.parent.mkdir(exist_ok=True)
                shutil.copyfile(ROOT / "scripts" / script, target)
            (snapshot / "reference.json").write_text("{}")
            manifest = {"schema_version": baseline.SCHEMA_VERSION, "strategy": "income",
                        "balance_basis": baseline.BALANCE_BASIS, "environment": baseline.environment(),
                        "comparison_tolerances": baseline.TOLERANCES, "configuration": {}, "coverage": {},
                        "files": baseline.digest_files(snapshot)}
            (snapshot / "manifest.json").write_text(json.dumps(baseline.seal_manifest(manifest)))
            relocated = snapshot.rename(root / "relocated")
            shutil.rmtree(original)
            args = SimpleNamespace(snapshot=relocated, output=root / "result.json")

            def execute(path, _reference):
                self.assertEqual(baseline.suspension_days(path), {(CODE, DAYS[1])})
                return {}, {}, None, {}, []

            with (patch.object(baseline, "execute", side_effect=execute),
                  patch.object(baseline, "compare", return_value={"match": True}),
                  patch.object(baseline, "result_fields", return_value={})):
                self.assertEqual(baseline.replay(args), 0)
                self.assertTrue(args.output.is_file())
                args.output.unlink()
                raw = relocated / "suspension-sources" / "batch" / f"baostock-{CODE}.json"
                raw.write_text("tampered")
                with self.assertRaisesRegex(ValueError, "snapshot integrity"):
                    baseline.replay(args)
                raw.unlink()
                with self.assertRaisesRegex(ValueError, "snapshot integrity"):
                    baseline.replay(args)
                self.assertFalse(args.output.exists())
                # The derived evidence still requires its original bytes even if the inventory is resealed.
                manifest_file = relocated / "manifest.json"
                manifest["files"] = baseline.digest_files(relocated)
                del manifest["files"]["manifest.json"]
                manifest_file.write_text(json.dumps(baseline.seal_manifest(manifest)))
                with self.assertRaisesRegex(ValueError, "missing or linked suspension source"):
                    baseline.replay(args)

    def test_result_labels_nested_legacy_balances_and_flows(self):
        details = {
            "summary": {"free_cash_used": 1},
            "rebalance_events": [{"income_used": 2}],
            "rebalance_transfers": [{"cash": 3}],
            "fills": [{"cash_trade": -4}],
            "stock_ledgers": {CODE: ([], {"dividend_cash": 5}, [{"cash_trade": -4}])},
        }
        result = baseline.result_fields({}, {}, {}, details)
        self.assertIsNone(result["settled_cash_available"])
        self.assertEqual(result["summary"], {"legacy_free_cash_used": 1})
        self.assertEqual(result["rebalance_events"], [{"legacy_income_used": 2}])
        self.assertEqual(result["rebalance_transfers"], [{"modeled_transfer_amount": 3}])
        self.assertEqual(result["fills"], [{"modeled_trade_cash_flow": -4}])
        self.assertEqual(result["stock_ledgers"][CODE],
                         ([], {"legacy_income_balance": 5}, [{"modeled_trade_cash_flow": -4}]))
        self.assertEqual(details["summary"], {"free_cash_used": 1})

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

            evidence_file, evidence = self.make_suspension_evidence(root / "original")
            row = evidence["rows"][0]
            row["baostock"]["tradestatus"] = "1"
            row["tencent"] = [DAYS[1], 10, 10, 10, 10, 1]
            evidence["rows"].append(dict(row, date=DAYS[0], tencent=None))
            evidence_file.write_text(json.dumps(evidence))
            conflict = root / "conflict"
            result = subprocess.run([sys.executable, str(ROOT / "scripts/income_baseline.py"), "freeze",
                                     "--legacy-root", dashboard.parent, "--snapshot", conflict,
                                     "--suspension-evidence", evidence_file], capture_output=True)
            self.assertEqual(result.returncode, 2, result.stderr)
            pending = json.loads((conflict / "pending-gaps.json").read_bytes())
            self.assertEqual(pending["status"], "pending_decision")
            self.assertEqual({(item["asset"], item["date"], item["field"]) for item in pending["gaps"]}, {
                (CODE, DAYS[1], "baostock.tradestatus"), (CODE, DAYS[1], "tencent.record"),
                (CODE, DAYS[0], "baostock.record"),
            })
            self.assertTrue(all(item["impact"] and item["candidate_measures"] for item in pending["gaps"]))
            self.assertFalse((conflict / "manifest.json").exists())

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
