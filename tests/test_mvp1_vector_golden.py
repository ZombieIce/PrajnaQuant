import importlib.util
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
SCALE_FIXTURE = ROOT / "poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json"
SCALE_EXPECTED = {
    None: ROOT / "poc/mvp1-golden/expected/b2-s2-scale-64x252-v2.json",
    20: ROOT / "poc/mvp1-golden/expected/b2-s2-scale-64x252-v2.trend20.json",
}
HALTED_FIXTURE = (
    ROOT / "poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-halted-v1.json"
)
HALTED_EXPECTED = (
    ROOT / "poc/mvp1-golden/expected/b2-s2-scale-64x252-halted-v1.json"
)


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

            self.assertEqual(
                output["tolerance"],
                {"factor_abs": 1e-12, "nav_abs": 1e-10},
            )
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

    def test_3x10_summary_matches_hand_calculated_metrics(self):
        with tempfile.TemporaryDirectory() as directory:
            output = json.loads(self.run_cli(Path(directory) / "golden.json"))

        summary = output["summary"]
        # Ten opens span nine returns. Two proportional costs of 0.002 and
        # 0.004 leave NAV=0.998*0.996=0.994008; the final peak/trough is Jan 5/14.
        self.assertEqual(summary["n_returns"], 9)
        self.assertAlmostEqual(summary["total_return"], -0.005992, delta=1e-12)
        self.assertAlmostEqual(
            summary["annualized_return"], 0.994008 ** (252 / 9) - 1, delta=1e-12
        )
        self.assertAlmostEqual(summary["mean_return"], -0.006 / 9, delta=1e-12)
        self.assertAlmostEqual(
            summary["std_return"], math.sqrt(2e-6), delta=1e-12
        )
        self.assertAlmostEqual(summary["max_drawdown"], -0.005992, delta=1e-12)
        self.assertEqual(summary["drawdown_peak_session"], "2026-01-05")
        self.assertEqual(summary["drawdown_trough_session"], "2026-01-14")
        self.assertEqual(summary["total_turnover"], 3.0)
        self.assertAlmostEqual(summary["total_cost"], 0.006, delta=1e-12)
        self.assertEqual(summary["executed_count"], 6)
        self.assertEqual(summary["deferred_count"], 1)
        self.assertEqual(summary["skipped_buy_count"], 1)
        self.assertEqual(summary["session_count"], 10)
        self.assertTrue(summary["pending_at_end"])
        self.assertEqual(summary["assumptions"], output["vector"]["assumptions"])
        self.assertEqual(
            summary["availability_assumption"],
            output["vector"]["availability_assumption"],
        )

    def test_constant_returns_have_exact_zero_volatility_and_null_sharpe(self):
        spec = importlib.util.spec_from_file_location("vector_golden", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for count in (3, 10):
            sessions = [{"net_return": 0.0, "nav": 1.0, "session_date": "d0", "turnover": 0.0, "cost": 0.0}]
            nav = 1.0
            for index in range(count):
                nav *= 1.1
                sessions.append(
                    {"net_return": 0.1, "nav": nav, "session_date": f"d{index + 1}", "turnover": 0.0, "cost": 0.0}
                )
            summary = module.build_summary(
                {"sessions": sessions, "executions": [], "pending_at_end": None,
                 "assumptions": [], "availability_assumption": "none"},
                252,
            )
            self.assertEqual(summary["std_return"], 0.0)
            self.assertIsNone(summary["sharpe"])
            self.assertTrue(summary["zero_volatility"])

    def test_cli_output_is_byte_for_byte_deterministic(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.run_cli(Path(directory) / "first.json")
            second = self.run_cli(Path(directory) / "second.json")
            self.assertEqual(first, second)

    def test_scale_v2_expected_outputs_cover_structure_and_semantics(self):
        fixture = json.loads(SCALE_FIXTURE.read_text(encoding="utf-8"))
        self.assertEqual(len(fixture["calendar"]), 252)
        self.assertEqual(len(fixture["instruments"]), 64)
        expected_rows = len(fixture["calendar"]) * len(fixture["instruments"])
        expected_factor_rows = {
            (f"{instrument['symbol']}.SYNTH", session_date)
            for instrument in fixture["instruments"]
            for session_date in fixture["calendar"]
        }
        expected_factor_names = {
            "momentum(20)",
            "momentum(60)",
            "volatility(20)",
            "rotation_score",
        }
        missing_bar = fixture["missing_bars"][0]
        halted = fixture["execution_status_overrides"][0]
        self.assertEqual(halted["trade_status"], "HALTED")
        self.assertFalse(halted["is_tradable"])

        outputs = {}
        with tempfile.TemporaryDirectory() as directory:
            for trend in (None, 20):
                output_path = Path(directory) / f"trend-{trend}.json"
                generated = self.run_cli(
                    output_path,
                    fixture_path=SCALE_FIXTURE,
                    trend=trend,
                )
                repeated = self.run_cli(
                    Path(directory) / f"repeat-trend-{trend}.json",
                    fixture_path=SCALE_FIXTURE,
                    trend=trend,
                )
                self.assertEqual(generated, repeated)
                self.assertEqual(generated, SCALE_EXPECTED[trend].read_bytes())
                outputs[trend] = json.loads(generated)

        for trend, output in outputs.items():
            factor_names = expected_factor_names | (
                {"trend_filter(20)"} if trend is not None else set()
            )
            self.assertEqual(set(output["factors"]), factor_names)
            for rows in output["factors"].values():
                self.assertEqual(len(rows), expected_rows)
                self.assertEqual(
                    {
                        (row["instrument_id"], row["session_date"])
                        for row in rows
                    },
                    expected_factor_rows,
                )

            vector = output["vector"]
            self.assertEqual(
                [row["session_date"] for row in vector["sessions"]],
                fixture["calendar"],
            )
            self.assertEqual(len(vector["sessions"]), 252)

            missing_instrument = f"{missing_bar['symbol']}.SYNTH"
            for factor_name in factor_names - {"trend_filter(20)"}:
                missing_row = next(
                    row
                    for row in output["factors"][factor_name]
                    if row["instrument_id"] == missing_instrument
                    and row["session_date"] == missing_bar["date"]
                )
                self.assertEqual(missing_row["status"], "missing_input")
                self.assertIsNone(missing_row["value"])
                self.assertIsNone(missing_row["available_at"])
            if trend is not None:
                missing_trend = next(
                    row
                    for row in output["factors"]["trend_filter(20)"]
                    if row["instrument_id"] == missing_instrument
                    and row["session_date"] == missing_bar["date"]
                )
                self.assertEqual(missing_trend["status"], "missing_input")
                self.assertIsNone(missing_trend["value"])
                self.assertIsNone(missing_trend["available_at"])

            execution_dates = {
                row["session_date"] for row in vector["executions"]
            }
            previous_session = fixture["calendar"][
                fixture["calendar"].index(halted["date"]) - 1
            ]
            previous_execution = next(
                row
                for row in vector["executions"]
                if row["session_date"] == previous_session
            )
            self.assertEqual(previous_execution["kind"], "executed")
            self.assertNotIn(halted["date"], execution_dates)
            carried = {
                row["session_date"]: row["valuation_carried"]
                for row in vector["sessions"]
            }
            self.assertIn(missing_instrument, carried[missing_bar["date"]])
            self.assertTrue(
                any(row["gross_return"] != 0 for row in vector["sessions"])
            )

        trend_output = outputs[20]
        self.assertTrue(
            any(
                row["status"] == "filtered"
                for row in trend_output["factors"]["rotation_score"]
            )
        )
        plain_decisions = {
            row["decision_session"]: row
            for row in outputs[None]["vector"]["decisions"]
        }
        trend_decisions = {
            row["decision_session"]: row
            for row in trend_output["vector"]["decisions"]
        }
        self.assertEqual(set(plain_decisions), set(trend_decisions))
        self.assertTrue(
            any(
                plain_decisions[session]["ranked"]
                != trend_decisions[session]["ranked"]
                or plain_decisions[session]["targets"]
                != trend_decisions[session]["targets"]
                for session in plain_decisions
            )
        )

    def test_scale_halted_golden_blocks_sells_and_skips_buys(self):
        fixture = json.loads(HALTED_FIXTURE.read_text(encoding="utf-8"))
        overrides = {
            (row["symbol"], row["date"]): row
            for row in fixture["execution_status_overrides"]
        }
        self.assertEqual(len(fixture["calendar"]), 252)
        self.assertEqual(len(fixture["instruments"]), 64)
        for key in (("ETF029", "2025-07-08"), ("ETF041", "2025-07-15")):
            self.assertEqual(overrides[key]["trade_status"], "HALTED")
            self.assertFalse(overrides[key]["is_tradable"])

        ignored_fixture = dict(fixture, execution_status_overrides=[])
        with tempfile.TemporaryDirectory() as directory:
            generated = self.run_cli(
                Path(directory) / "halted.json", fixture_path=HALTED_FIXTURE
            )
            repeated = self.run_cli(
                Path(directory) / "repeat.json", fixture_path=HALTED_FIXTURE
            )
            ignored_path = Path(directory) / "ignored-fixture.json"
            ignored_path.write_text(json.dumps(ignored_fixture), encoding="utf-8")
            ignored = self.run_cli(
                Path(directory) / "ignored.json", fixture_path=ignored_path
            )
        self.assertEqual(generated, repeated)
        self.assertEqual(generated, HALTED_EXPECTED.read_bytes())
        self.assertNotEqual(generated, ignored)

        def execution_map(output_bytes):
            return {
                row["session_date"]: row
                for row in json.loads(output_bytes)["vector"]["executions"]
            }

        def sessions_map(output_bytes):
            return {
                row["session_date"]: row
                for row in json.loads(output_bytes)["vector"]["sessions"]
            }

        executions = execution_map(generated)
        sessions = sessions_map(generated)
        decisions = {
            row["decision_session"]: row
            for row in json.loads(generated)["vector"]["decisions"]
        }

        # The 07-07 target replaces held ETF029 and the 07-08 open halts it, so
        # the sell is blocked and the whole rebalance waits for 07-09.
        self.assertNotIn("ETF029.SYNTH", decisions["2025-07-07"]["targets"])
        self.assertIn(
            "ETF029.SYNTH", sessions["2025-07-07"]["weights_after_execution"]
        )
        self.assertEqual(
            executions["2025-07-08"],
            {
                "session_date": "2025-07-08",
                "kind": "deferred",
                "blocked": ["ETF029.SYNTH"],
                "skipped_buys": [],
            },
        )
        self.assertEqual(sessions["2025-07-08"]["turnover"], 0.0)
        self.assertEqual(sessions["2025-07-08"]["cost"], 0.0)
        self.assertEqual(
            executions["2025-07-09"],
            {
                "session_date": "2025-07-09",
                "kind": "executed",
                "blocked": [],
                "skipped_buys": [],
            },
        )
        self.assertEqual(
            sessions["2025-07-09"]["weights_after_execution"],
            {
                f"ETF{number:03}.SYNTH": 0.2
                for number in (12, 16, 25, 33, 54)
            },
        )

        # The 07-14 target buys ETF041, which is HALTED at the 07-15 open; its
        # weight stays in cash. Four 0.2 buys plus selling the whole prior
        # book gives turnover 1.8 and cost 1.8 * (0.001 + 10 / 1e4).
        self.assertIn("ETF041.SYNTH", decisions["2025-07-14"]["targets"])
        self.assertEqual(
            executions["2025-07-15"],
            {
                "session_date": "2025-07-15",
                "kind": "executed",
                "blocked": [],
                "skipped_buys": ["ETF041.SYNTH"],
            },
        )
        self.assertEqual(
            sessions["2025-07-15"]["weights_after_execution"],
            {
                f"ETF{number:03}.SYNTH": 0.2
                for number in (3, 7, 38, 51)
            },
        )
        self.assertAlmostEqual(sessions["2025-07-15"]["turnover"], 1.8, delta=1e-12)
        self.assertAlmostEqual(sessions["2025-07-15"]["cost"], 0.0036, delta=1e-12)

        # Ignoring HALTED would execute both sessions without blocking or skips.
        ignored_executions = execution_map(ignored)
        self.assertEqual(ignored_executions["2025-07-08"]["kind"], "executed")
        self.assertEqual(ignored_executions["2025-07-08"]["blocked"], [])
        self.assertEqual(ignored_executions["2025-07-15"]["skipped_buys"], [])
        self.assertIn(
            "ETF041.SYNTH",
            sessions_map(ignored)["2025-07-15"]["weights_after_execution"],
        )

        # Only these two executions are blocked or skipped.
        self.assertEqual(
            [
                session
                for session, row in executions.items()
                if row["blocked"] or row["skipped_buys"]
            ],
            ["2025-07-08", "2025-07-15"],
        )
        self.assertIn(
            "ETF001.SYNTH", sessions["2025-07-01"]["valuation_carried"]
        )

    def test_committed_expected_output_matches_the_cli(self):
        expected_path = ROOT / "poc/mvp1-golden/expected/dataset-v1.json"
        with tempfile.TemporaryDirectory() as directory:
            generated = self.run_cli(Path(directory) / "dataset-v1.json")
        self.assertEqual(generated, expected_path.read_bytes())

    def test_vector_matches_hand_derived_execution_costs_and_nav(self):
        with tempfile.TemporaryDirectory() as directory:
            output_path = Path(directory) / "dataset-v1.json"
            self.run_cli(output_path)
            output = json.loads(output_path.read_bytes())

        vector = output["vector"]
        sessions = {
            row["session_date"]: row for row in vector["sessions"]
        }
        decisions = {
            row["decision_session"]: row for row in vector["decisions"]
        }
        executions = {
            row["session_date"]: row for row in vector["executions"]
        }
        self.assertEqual(len(vector["sessions"]), 10)
        self.assertEqual(len(vector["decisions"]), 8)

        # Jan 7 selects C; Jan 8 skips its UNKNOWN buy; Jan 13 defers the
        # C rebalance because held B is HALTED; Jan 14 executes the new A target.
        self.assertEqual(
            decisions["2026-01-07"]["targets"], {"C.SYNTH": 1.0}
        )
        self.assertEqual(
            executions["2026-01-08"],
            {
                "session_date": "2026-01-08",
                "kind": "executed",
                "blocked": [],
                "skipped_buys": ["C.SYNTH"],
            },
        )
        self.assertEqual(
            executions["2026-01-13"],
            {
                "session_date": "2026-01-13",
                "kind": "deferred",
                "blocked": ["B.SYNTH"],
                "skipped_buys": [],
            },
        )
        self.assertEqual(sessions["2026-01-09"]["turnover"], 1.0)
        self.assertAlmostEqual(
            sessions["2026-01-09"]["cost"], 0.002, delta=1e-15
        )
        self.assertEqual(sessions["2026-01-14"]["turnover"], 2.0)
        self.assertAlmostEqual(
            sessions["2026-01-14"]["cost"], 0.004, delta=1e-15
        )
        self.assertTrue(
            all(row["gross_return"] == 0 for row in vector["sessions"])
        )
        expected_nav = [
            1.0,
            1.0,
            1.0,
            1.0,
            0.998,
            0.998,
            0.998,
            0.994008,
            0.994008,
            0.994008,
        ]
        for row, expected in zip(vector["sessions"], expected_nav):
            self.assertAlmostEqual(row["nav"], expected, delta=1e-10)
        self.assertEqual(
            sessions["2026-01-16"]["valuation_carried"], ["B.SYNTH"]
        )
        self.assertEqual(
            vector["pending_at_end"],
            {
                "decision_session": "2026-01-16",
                "targets": {"C.SYNTH": 1.0},
            },
        )
        self.assertEqual(output["tolerance"]["nav_abs"], 1e-10)

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
