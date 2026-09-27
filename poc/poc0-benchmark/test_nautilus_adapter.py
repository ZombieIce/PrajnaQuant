import json
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

import nautilus_adapter


class NautilusAdapterReportTests(unittest.TestCase):
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
        self.assertIsNone(report["timings"]["conversion_samples_ns"])
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
        self.assertTrue(all(check["passed"] for check in checks[1:]))


if __name__ == "__main__":
    unittest.main()
