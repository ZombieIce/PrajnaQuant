import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
SCRIPT = ROOT / "poc/mvp1-golden/vector_golden.py"


class VectorGoldenTests(unittest.TestCase):
    def run_cli(self, output_path, fixture_path=FIXTURE, trend=None):
        command = [
            sys.executable,
            str(SCRIPT),
            "--fixture",
            str(fixture_path),
            "--out",
            str(output_path),
        ]
        if trend is not None:
            command.extend(["--trend", str(trend)])
        subprocess.run(command, check=True, capture_output=True, text=True)
        return output_path.read_bytes()

    def test_cli_emits_hand_checkable_factor_values_and_statuses(self):
        with tempfile.TemporaryDirectory() as directory:
            output_path = Path(directory) / "factors.json"
            output_bytes = self.run_cli(output_path)
            output = json.loads(output_bytes)

            self.assertEqual(output["tolerance"], {"factor_abs": 1e-12})
            self.assertEqual(
                set(output["factors"]),
                {"momentum(1)", "volatility(2)", "rotation_score"},
            )
            self.assertEqual(len(output["factors"]["momentum(1)"]), 30)

            def factor_row(name, instrument_id, session_date):
                return next(
                    row
                    for row in output["factors"][name]
                    if row["instrument_id"] == instrument_id
                    and row["session_date"] == session_date
                )

            first_session = factor_row(
                "momentum(1)", "A.SYNTH", "2026-01-05"
            )
            self.assertIsNone(first_session["value"])
            self.assertIsNone(first_session["available_at"])
            self.assertEqual(first_session["status"], "insufficient_window")

            known_momentum = factor_row(
                "momentum(1)", "A.SYNTH", "2026-01-06"
            )
            self.assertAlmostEqual(known_momentum["value"], 0.01, delta=1e-12)
            self.assertEqual(known_momentum["status"], "ok")
            self.assertEqual(
                known_momentum["available_at"], "2026-01-06T07:00:00Z"
            )

            sample_volatility = factor_row(
                "volatility(2)", "C.SYNTH", "2026-01-07"
            )
            self.assertAlmostEqual(
                sample_volatility["value"],
                0.01 / math.sqrt(2),
                delta=1e-15,
            )

            missing_bar = factor_row(
                "momentum(1)", "B.SYNTH", "2026-01-16"
            )
            self.assertIsNone(missing_bar["value"])
            self.assertIsNone(missing_bar["available_at"])
            self.assertEqual(missing_bar["status"], "missing_input")
            self.assertEqual(
                factor_row("rotation_score", "B.SYNTH", "2026-01-16")[
                    "status"
                ],
                "missing_input",
            )

    def test_cli_output_is_byte_for_byte_deterministic(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.run_cli(Path(directory) / "first.json")
            second = self.run_cli(Path(directory) / "second.json")
            self.assertEqual(first, second)

    def test_committed_expected_output_matches_the_cli(self):
        expected_path = (
            ROOT / "poc/mvp1-golden/expected/dataset-v1.factors.json"
        )
        with tempfile.TemporaryDirectory() as directory:
            generated = self.run_cli(Path(directory) / "factors.json")
        self.assertEqual(generated, expected_path.read_bytes())

    def test_cli_applies_trend_filter_and_preserves_its_availability(self):
        with tempfile.TemporaryDirectory() as directory:
            output_path = Path(directory) / "factors.json"
            self.run_cli(output_path, trend=2)
            output = json.loads(output_path.read_bytes())
            trend_row = next(
                row
                for row in output["factors"]["trend_filter(2)"]
                if row["instrument_id"] == "C.SYNTH"
                and row["session_date"] == "2026-01-08"
            )
            self.assertEqual(trend_row["value"], 0)
            self.assertEqual(trend_row["status"], "ok")

            filtered_score = next(
                row
                for row in output["factors"]["rotation_score"]
                if row["instrument_id"] == "C.SYNTH"
                and row["session_date"] == "2026-01-08"
            )
            self.assertIsNone(filtered_score["value"])
            self.assertEqual(filtered_score["status"], "filtered")
            self.assertEqual(
                filtered_score["available_at"], "2026-01-08T07:00:00Z"
            )

    def test_cli_propagates_status_precedence_and_unknown_availability(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = json.loads(FIXTURE.read_text(encoding="utf-8"))
            fixture["bar_defaults"]["close_available_at"] = None
            fixture["strategy"]["momentum_long_days"] = 4
            fixture["missing_bars"].extend(
                [
                    {"symbol": "A", "date": "2026-01-06"},
                    {"symbol": "A", "date": "2026-01-08"},
                ]
            )
            fixture_path = Path(directory) / "fixture.json"
            fixture_path.write_text(json.dumps(fixture), encoding="utf-8")
            output_path = Path(directory) / "factors.json"
            self.run_cli(output_path, fixture_path=fixture_path)
            output = json.loads(output_path.read_bytes())

            def score_row(instrument_id, session_date):
                return next(
                    row
                    for row in output["factors"]["rotation_score"]
                    if row["instrument_id"] == instrument_id
                    and row["session_date"] == session_date
                )

            insufficient_over_missing = score_row("A.SYNTH", "2026-01-06")
            self.assertEqual(
                insufficient_over_missing["status"], "insufficient_window"
            )
            self.assertIsNone(insufficient_over_missing["value"])

            missing_over_unknown = score_row("A.SYNTH", "2026-01-12")
            self.assertEqual(missing_over_unknown["status"], "missing_input")
            self.assertIsNone(missing_over_unknown["value"])

            unknown_availability = score_row("C.SYNTH", "2026-01-12")
            self.assertEqual(
                unknown_availability["status"], "unknown_availability"
            )
            self.assertIsNone(unknown_availability["value"])


if __name__ == "__main__":
    unittest.main()
