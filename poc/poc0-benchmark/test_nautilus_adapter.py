import json
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

import nautilus_adapter


class NautilusAdapterReportTests(unittest.TestCase):
    @unittest.skipUnless(nautilus_adapter._version_probe()[1] is None, "requires pinned Nautilus")
    def test_rotation_reinvests_exit_proceeds_at_next_open(self):
        dataset_path = nautilus_adapter.ROOT / "poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v1.json"
        dataset = json.loads(dataset_path.read_text(encoding="utf-8"))
        symbols = {"ETF003", "ETF007", "ETF012", "ETF025", "ETF054",
                   "ETF038", "ETF041", "ETF046", "ETF051", "ETF055"}
        last_session = dataset["calendar"].index("2025-04-09") + 1
        dataset["calendar"] = dataset["calendar"][:last_session]
        dataset["instruments"] = [
            {**item, "closes": item["closes"][:last_session]}
            for item in dataset["instruments"] if item["symbol"] in symbols
        ]
        dataset["missing_bars"] = [
            row for row in dataset["missing_bars"]
            if row["symbol"] in symbols and row["date"] <= "2025-04-09"
        ]
        dataset["execution_status_overrides"] = [
            row for row in dataset["execution_status_overrides"]
            if row["symbol"] in symbols and row["date"] <= "2025-04-09"
        ]
        projection = nautilus_adapter._run_nautilus(dataset, strategy="s2")["projection"]

        self.assertEqual(len(dataset["instruments"]), 10)
        self.assertEqual(projection["signals"][-1]["date"], "2025-04-07")
        self.assertEqual(set(projection["signals"][-1]["target_symbols"]),
                         {"ETF038", "ETF041", "ETF046", "ETF051", "ETF055"})
        self.assertIn(
            ("2025-04-07", "2025-04-08", "ETF038", "BUY", 100),
            [(row["decision_date"], row["attempt_date"], row["symbol"], row["side"], row["quantity"])
             for row in projection["orders"]],
        )
        self.assertEqual(
            next(row for row in projection["ledger"] if row["date"] == "2025-04-08")["cash"],
            48350.0,
        )
        self.assertEqual(
            next(row for row in projection["ledger"] if row["date"] == "2025-04-08")["nav"],
            99560.0,
        )
        self.assertEqual(len(projection["fills"]), 15)
        entry = next(row for row in projection["fills"] if row["date"] == "2025-04-08"
                 and row["symbol"] == "ETF038")
        self.assertEqual((entry["side"], entry["quantity"], entry["fill_price"], entry["commission"]),
                 ("BUY", 100, 100.1, 100.0))
        holdings = next(row for row in projection["ledger"] if row["date"] == "2025-04-08")["holdings"]
        self.assertEqual({row["symbol"]: row["quantity"] for row in holdings},
                 {symbol: 100 for symbol in {"ETF038", "ETF041", "ETF046", "ETF051", "ETF055"}})

    @unittest.skipUnless(
        nautilus_adapter._version_probe()[1] is None,
        "requires the pinned Nautilus runtime",
    )
    def test_rotation_orders_and_account_match_worked_fixture(self):
        dataset_path = nautilus_adapter.ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
        dataset = json.loads(dataset_path.read_text(encoding="utf-8"))
        projection = nautilus_adapter._run_nautilus(dataset, strategy="s2")["projection"]

        self.assertEqual(projection["signals"][0], {"date": "2026-01-07", "target_symbols": ["C"]})
        self.assertEqual(
            [(order["attempt_date"], order["symbol"], order["side"], order["quantity"], order["reason"]) for order in projection["orders"]],
            [
                ("2026-01-08", "C", "BUY", 0, "UNKNOWN"),
                ("2026-01-09", "B", "BUY", 900, None),
                ("2026-01-13", "B", "SELL", 0, "HALTED"),
                ("2026-01-14", "B", "SELL", 900, None),
                ("2026-01-14", "A", "BUY", 900, None),
            ],
        )
        self.assertEqual([row["cash"] for row in projection["ledger"]], [100000.0] * 4 + [9810.0] * 3 + [9430.0] * 3)
        self.assertEqual([row["nav"] for row in projection["ledger"]], [100000.0] * 4 + [101610.0, 102510.0, 100710.0, 100330.0, 101230.0, 102130.0])
        self.assertAlmostEqual(projection["summary"]["total_cost"], 570.0)

        reference_path = nautilus_adapter.ROOT / "target/poc-0/benchmark-report.json"
        with tempfile.TemporaryDirectory() as directory:
            reference_path = Path(directory) / "rust.json"
            from subprocess import run
            run([
                str(nautilus_adapter.ROOT / "target/debug/quant-research"),
                "benchmark-poc0", "--candidate", "soa", "--output", str(reference_path),
            ], cwd=nautilus_adapter.ROOT, check=True, capture_output=True)
            report = nautilus_adapter.build_report(dataset_path, reference_path)
        rotation = report["strategy_comparisons"]["s2"]
        self.assertEqual(rotation["status"], "passed_common_subset")
        self.assertTrue(all(check["passed"] for check in rotation["checks"] if check["field"] != "orders.nautilus_native_lifecycle"))
        self.assertTrue(next(check["passed"] for check in rotation["checks"] if check["field"] == "signals.factor_rankings"))
        self.assertFalse(next(check["passed"] for check in rotation["checks"] if check["field"] == "orders.nautilus_native_lifecycle"))
        self.assertEqual(len(rotation["timings"]["event_processing"]["raw_samples_ns"]), 5)

    @unittest.skipUnless(nautilus_adapter._version_probe()[1] is None, "requires pinned Nautilus")
    def test_ma20_60_trades_only_after_completed_window_and_cross(self):
        spec_path = nautilus_adapter.ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json"
        dataset = nautilus_adapter._ma_dataset(json.loads(spec_path.read_text(encoding="utf-8")))
        projection = nautilus_adapter._run_nautilus(dataset, strategy="s3")["projection"]
        calendar = dataset["calendar"]
        self.assertEqual(projection["signals"], [
            {"date": calendar[60], "target_symbols": ["A"]},
            {"date": calendar[85], "target_symbols": []},
        ])
        self.assertEqual(
            [(item["attempt_date"], item["side"], item["quantity"]) for item in projection["orders"]],
            [(calendar[61], "BUY", 900), (calendar[86], "SELL", 900)],
        )
        self.assertAlmostEqual(projection["ledger"][60]["nav"], 100000.0)
        self.assertAlmostEqual(projection["ledger"][61]["cash"], 9810.0)
        self.assertAlmostEqual(projection["ledger"][86]["cash"], 99620.0)
        self.assertAlmostEqual(projection["summary"]["total_cost"], 380.0)
        with tempfile.TemporaryDirectory() as directory:
            from subprocess import run
            reference_path = Path(directory) / "rust.json"
            run([
                str(nautilus_adapter.ROOT / "target/debug/quant-research"),
                "benchmark-poc0", "--candidate", "soa", "--output", str(reference_path),
            ], cwd=nautilus_adapter.ROOT, check=True, capture_output=True)
            report = nautilus_adapter.build_report(
                nautilus_adapter.ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json", reference_path
            )
        comparison = report["strategy_comparisons"]["s3"]
        self.assertEqual(comparison["status"], "passed_common_subset")
        self.assertTrue(all(check["passed"] for check in comparison["checks"]))
        self.assertEqual(len(comparison["timings"]["event_processing"]["raw_samples_ns"]), 5)
        self.assertEqual(report["ticket09_decision"]["status"], "unresolved")
        self.assertEqual(report["ticket09_decision"]["minimum_fast_event_gain"], 2.0)

    @unittest.skipUnless(nautilus_adapter._version_probe()[1] is None, "requires pinned Nautilus")
    def test_parallel_runs_keep_full_projection_and_raw_samples(self):
        dataset = json.loads((nautilus_adapter.ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json").read_text(encoding="utf-8"))
        report = nautilus_adapter.parallel_runs(dataset, "s2", workers=2, runs=4)
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["workers"], 2)
        self.assertEqual(report["warmup_runs"], 2)
        self.assertEqual(len(report["raw_samples_ns"]), 4)
        self.assertGreater(report["parallel_runs_per_second"], 0)
        self.assertGreater(report["peak_worker_rss_bytes"], 0)

    @unittest.skipUnless(
        nautilus_adapter._version_probe()[1] is None,
        "requires the pinned Nautilus runtime",
    )
    def test_halted_order_is_rejected_at_open_then_retried(self):
        dataset_path = nautilus_adapter.ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
        dataset = json.loads(
            dataset_path.read_text(
                encoding="utf-8"
            )
        )
        projection = nautilus_adapter._run_nautilus(dataset)["projection"]

        self.assertEqual(
            [
                (order["attempt_date"], order["symbol"], order["quantity"], order["reason"])
                for order in projection["orders"]
            ],
            [
                ("2026-01-13", "A", 300, None),
                ("2026-01-13", "B", 0, "HALTED"),
                ("2026-01-13", "C", 300, None),
                ("2026-01-14", "B", 300, None),
            ],
        )
        self.assertEqual(
            [(fill["date"], fill["symbol"]) for fill in projection["fills"]],
            [
                ("2026-01-13", "A"),
                ("2026-01-13", "C"),
                ("2026-01-14", "B"),
            ],
        )
        rejected = projection["orders"][1]
        retried = projection["orders"][3]
        self.assertEqual(rejected["origin"], "adapter_status_gate")
        self.assertIsNone(rejected["submission_ts"])
        self.assertEqual(retried["origin"], "nautilus_order")
        self.assertGreater(retried["submission_ts"], rejected["decision_ts"])
        self.assertEqual(
            sum(order["origin"] == "nautilus_order" for order in projection["orders"]), 3
        )

        reference = (
            nautilus_adapter.ROOT
            / "poc/poc0-benchmark/results/nautilus-adapter-comparison-status-gated-2026-09-27.json"
        )
        report = nautilus_adapter.build_report(dataset_path, reference)
        checks = {check["field"]: check for check in report["semantic_comparison"]["checks"]}
        self.assertEqual(report["status"], "unresolved")
        self.assertTrue(checks["orders.adapter_project_contract"]["passed"])
        self.assertFalse(checks["orders.nautilus_native_lifecycle"]["passed"])
        self.assertEqual(report["timings"]["completed_runs"], 5)
        self.assertEqual(len(report["timings"]["event_processing"]["raw_samples_ns"]), 5)
        self.assertTrue(report["timings"]["repeated_projection_equal"])

    def test_unavailable_or_unverified_runtime_keeps_measurements_unknown(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            dataset = root / "dataset.json"
            reference = root / "reference.json"
            dataset.write_text(
                json.dumps(
                    {
                        "dataset_version": "fixture-v1",
                        "calendar": ["2026-01-01"],
                        "instruments": [{"symbol": "A"}],
                    }
                ),
                encoding="utf-8",
            )
            reference.write_text(
                json.dumps(
                    {
                        "b2_fast_event_buy_hold": {
                            "checksum_sha256": "abc",
                            "projection": {"fills": [], "ledger": []},
                        }
                    }
                ),
                encoding="utf-8",
            )

            with (
                patch.object(
                    nautilus_adapter,
                    "_version_probe",
                    return_value=(None, "PackageNotFoundError: forced missing dependency"),
                ),
                patch.object(nautilus_adapter, "INSTALL_EVIDENCE", root / "missing.json"),
            ):
                report = nautilus_adapter.build_report(dataset, reference)

        self.assertEqual(report["status"], "unresolved")
        self.assertEqual(report["project_contract"]["reference_checksum_sha256"], "abc")
        self.assertEqual(report["semantic_comparison"]["status"], "not_run")
        self.assertIsNone(report["semantic_comparison"]["orders"])
        self.assertIsNone(report["timings"]["conversion"])
        self.assertIsNone(report["build_resources"]["release_build_ns"])
        self.assertTrue(report["unresolved_reasons"])

    def test_comparison_keeps_account_parity_separate_from_order_lifecycle(self):
        fills = [
            {"date": "2026-01-13", "symbol": "A", "side": "BUY", "quantity": 300, "fill_price": 100.1, "commission": 100.0}
        ]
        ledger = [
            {"date": "2026-01-13", "cash": 69870.0, "holdings": [{"symbol": "A", "quantity": 300}], "nav": 99870.0}
        ]
        actual = {
            "orders": [{"symbol": "A", "side": "BUY"}],
            "fills": fills,
            "ledger": ledger,
            "nautilus_account_snapshots": {
                "2026-01-13": {"cash": 69870.0, "positions": {"A": 300}}
            },
            "summary": {"commission": 100.0, "tax": 0.0, "slippage_cost": 30.0, "total_cost": 130.0},
        }
        expected = {
            "orders": [{"symbol": "A", "side": "BUY"}, {"symbol": "A", "side": "BUY"}],
            "fills": fills,
            "ledger": ledger,
            "nautilus_account_snapshots": {
                "2026-01-13": {"cash": 69870.0, "positions": {"A": 300}}
            },
            "summary": {"commission": 100.0, "tax": 0.0, "slippage_cost": 30.0, "total_cost": 130.0},
        }

        checks = nautilus_adapter._compare(actual, expected)

        self.assertFalse(checks[0]["passed"])
        self.assertFalse(checks[1]["passed"])
        self.assertTrue(all(check["passed"] for check in checks[2:]))

    def test_order_comparison_checks_rejection_reason_and_attempt_date(self):
        expected = {
            "orders": [
                {
                    "decision_date": "2026-01-12",
                    "attempt_date": "2026-01-13",
                    "symbol": "B",
                    "side": "BUY",
                    "quantity": 0,
                    "reason": "HALTED",
                }
            ]
        }
        actual = {"orders": [dict(expected["orders"][0])]}
        self.assertTrue(nautilus_adapter._compare(actual, expected)[0]["passed"])

        actual["orders"][0]["attempt_date"] = "2026-01-14"
        self.assertFalse(nautilus_adapter._compare(actual, expected)[0]["passed"])

    def test_order_comparison_ignores_cross_instrument_arrival_order(self):
        orders = [
            {"decision_date": "2025-04-07", "attempt_date": "2025-04-08", "symbol": symbol,
             "side": "BUY", "quantity": 100, "reason": None}
            for symbol in ("ETF038", "ETF041")
        ]
        self.assertTrue(nautilus_adapter._compare({"orders": orders[::-1]}, {"orders": orders})[0]["passed"])


if __name__ == "__main__":
    unittest.main()
