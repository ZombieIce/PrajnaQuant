import json
import os
import shutil
import subprocess
import sys
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import eternal_baseline as baseline
import eternal_compat as compat
from test_eternal_baseline import make_replay_snapshot


ROOT = Path(__file__).resolve().parents[1]
RUNNER = ROOT / "scripts/eternal_compat.py"


def seal_snapshot(snapshot):
    manifest_path = snapshot / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["files"] = {
        path.relative_to(snapshot).as_posix(): {
            "sha256": baseline.sha256_file(path),
            "bytes": path.stat().st_size,
        }
        for path in sorted(path for path in snapshot.rglob("*") if path.is_file() and path != manifest_path)
    }
    manifest_path.write_text(
        json.dumps(baseline.seal_manifest(manifest), ensure_ascii=False, indent=2),
        encoding="utf-8",
    )


class EternalCompatibilityTests(unittest.TestCase):
    def setUp(self):
        artifact_root = os.environ.get("PRAJNA_TEST_ARTIFACTS")
        if not artifact_root:
            self.fail("PRAJNA_TEST_ARTIFACTS must point to the session validation folder")
        self.directory = Path(artifact_root) / f"eternal-compat-{uuid.uuid4()}"
        self.directory.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.directory)
        self.snapshot = self.directory / "snapshot"
        make_replay_snapshot(self.snapshot)

    def run_compat(self, name, config=None, snapshot=None):
        request = {
            "task_id": f"synthetic-{name}",
            "snapshot_path": str(snapshot or self.snapshot),
            "config": config or {},
        }
        request_path = self.directory / f"{name}-request.json"
        output_path = self.directory / f"{name}-result.json"
        request_path.write_text(json.dumps(request), encoding="utf-8")
        completed = subprocess.run(
            [
                sys.executable, str(RUNNER), "run",
                "--request", str(request_path), "--output", str(output_path),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        return completed, output_path

    def test_default_and_variant_config_are_persisted_with_model_nav_units(self):
        default, default_path = self.run_compat("default")
        self.assertEqual(default.returncode, 0, default.stderr)
        default_result = json.loads(default_path.read_text(encoding="utf-8"))
        self.assertEqual(default_result["effective_config"]["initial_nav"], 1.0)
        self.assertIsNone(default_result["effective_config"]["initial_capital_currency_amount"])
        self.assertEqual(default_result["effective_config"]["initial_capital_unit"], "model_nav")
        self.assertEqual(default_result["effective_config"]["weights"], baseline.WEIGHTS)
        self.assertEqual(
            default_result["frozen_reference_comparison"]["overall_status"],
            "match",
        )

        variant, variant_path = self.run_compat("variant", {
            "initial_nav": 10.0,
            "weights": {
                "sh511260": .25,
                "sh518880": .20,
                "sh515080": .20,
                "sh513300": .25,
                "sz159612": .10,
            },
            "rebalance_threshold": .05,
            "fee_rate_one_way": .001,
        })
        self.assertEqual(variant.returncode, 0, variant.stderr)
        variant_result = json.loads(variant_path.read_text(encoding="utf-8"))
        self.assertEqual(variant_result["effective_config"]["initial_nav"], 10.0)
        self.assertEqual(variant_result["summary"]["fee_bps"], 10.0)
        self.assertAlmostEqual(variant_result["daily"][0]["nav"], 10 / 1.001)
        self.assertNotEqual(
            default_result["identity"]["compat_run_id"],
            variant_result["identity"]["compat_run_id"],
        )
        for row in variant_result["daily"]:
            self.assertAlmostEqual(row["nav"], row["cash"] + sum(row["holdings"].values()))

    def test_threshold_change_controls_synthetic_close_signal_and_next_open_trade(self):
        bars_path = self.snapshot / "bars/sh511260.csv.gz"
        bars, fields = baseline.csv_rows(bars_path)
        for row in bars:
            if row["date"] == "2026-11-09":
                row["close"] = "1.06"
        baseline.write_csv(bars_path, bars, fields)
        seal_snapshot(self.snapshot)

        under, under_path = self.run_compat("threshold-10", {"rebalance_threshold": .10})
        over, over_path = self.run_compat(
            "threshold-5",
            {
                "initial_nav": 10.0,
                "rebalance_threshold": .05,
                "fee_rate_one_way": .001,
            },
        )
        reference, reference_path = self.run_compat(
            "threshold-5-reference",
            {"rebalance_threshold": .05, "fee_rate_one_way": .001},
        )
        self.assertEqual(under.returncode, 0, under.stderr)
        self.assertEqual(over.returncode, 0, over.stderr)
        self.assertEqual(reference.returncode, 0, reference.stderr)
        under_result = json.loads(under_path.read_text(encoding="utf-8"))
        over_result = json.loads(over_path.read_text(encoding="utf-8"))
        reference_result = json.loads(reference_path.read_text(encoding="utf-8"))
        self.assertEqual(under_result["signals"], [])
        self.assertEqual(len(over_result["signals"]), 1)
        self.assertEqual(over_result["signals"][0]["signal_date"], "2026-11-09")
        self.assertEqual(over_result["trades"][0]["intended_date"], "2026-11-10")
        self.assertEqual(over_result["trades"][0]["trade_date"], "2026-11-10")
        self.assertAlmostEqual(
            over_result["trades"][0]["fees_in_initial_nav"],
            reference_result["trades"][0]["fees_in_initial_nav"] * 10,
        )

    def test_invalid_or_missing_input_fails_without_publishing_a_result(self):
        with self.assertRaisesRegex(compat.RunnerError, "exactly the five fixed assets"):
            compat.validate_config({"weights": {"sh511260": 1.0}})
        with self.assertRaisesRegex(compat.RunnerError, "greater than zero"):
            compat.validate_config({"initial_nav": 0.0})

        completed, output_path = self.run_compat(
            "missing-input",
            snapshot=self.directory / "missing-snapshot",
        )
        self.assertEqual(completed.returncode, 2)
        self.assertEqual(json.loads(completed.stderr)["error"]["code"], "compat_input_unavailable")
        self.assertFalse(output_path.exists())

    def test_rejects_non_normalized_weights_and_overwrite(self):
        with self.assertRaisesRegex(compat.RunnerError, "sum to 1.0"):
            compat.validate_config({"weights": {code: .1 for code in baseline.ETF_CODES}})

        first, output_path = self.run_compat("immutable-output")
        self.assertEqual(first.returncode, 0, first.stderr)
        second, _ = self.run_compat("immutable-output")
        self.assertEqual(second.returncode, 2)
        self.assertEqual(json.loads(second.stderr)["error"]["code"], "compat_output_exists")


if __name__ == "__main__":
    unittest.main()
