import json
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

import nautilus_adapter


class NautilusAdapterReportTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
