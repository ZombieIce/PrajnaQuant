import json
from datetime import datetime, timezone
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import prajna


ROOT = Path(__file__).resolve().parents[2]
MEMBERS = ("A.SYNTH", "B.SYNTH", "C.SYNTH")


class StrategyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        cls.lake = Path(cls.directory.name) / "lake"
        cls.fixture = json.loads(
            (ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json").read_text()
        )
        cls.dsv = cls.publish(cls.fixture)

    @classmethod
    def publish(cls, fixture):
        path = Path(cls.directory.name) / "fixture.json"
        path.write_text(json.dumps(fixture))
        result = subprocess.run(
            [
                "cargo", "run", "-q", "-p", "prajna-py", "--example",
                "strategy_fixture", "--locked", "--offline", "--",
                str(cls.lake), str(path),
            ],
            cwd=ROOT, check=False, capture_output=True, text=True,
            env=os.environ | {"PYO3_PYTHON": sys.executable},
        )
        if result.returncode:
            raise RuntimeError(f"Fixture publication failed:\n{result.stderr}")
        return result.stdout.strip()

    def run_strategy(self, decide, *, dsv=None, strategy=None, assumption="none", params=None):
        return prajna.generate_decisions(
            lake_root=str(self.lake), dsv=dsv or self.dsv, venue="SYNTH",
            members=MEMBERS, strategy=strategy or prajna.Strategy(
                "test", "1", bar_columns=("close",),
                lookback_sessions=3, rebalance_every=2,
            ),
            decide=decide, params=params or {}, availability_assumption=assumption,
        )

    def test_only_declared_sessions_invoke_the_strategy_with_a_full_window(self):
        seen = []

        def decide(view, params, prev_decision):
            seen.append((view.session_date, view.sessions.num_rows, prev_decision))
            return prajna.Decision({MEMBERS[0]: 0.5})

        decisions = self.run_strategy(decide)
        self.assertEqual([item[0] for item in seen], [
            "2026-01-07", "2026-01-09", "2026-01-13", "2026-01-15",
        ])
        self.assertEqual([item[1] for item in seen], [3, 3, 3, 3])
        self.assertIsNone(seen[0][2])
        self.assertEqual([item[2].targets for item in seen[1:]], [{MEMBERS[0]: 0.5}] * 3)
        self.assertEqual([d.session_date for d in decisions], [item[0] for item in seen])

    def test_invalid_decisions_fail_the_run_without_repair(self):
        for targets, message in [
            ({MEMBERS[0]: -0.1}, "nonnegative"),
            ({MEMBERS[0]: 0.6, MEMBERS[1]: 0.5}, "sum"),
            ({"OUTSIDE.SYNTH": 0.1}, "Universe"),
            ({MEMBERS[0]: float("nan")}, "finite"),
            ({MEMBERS[0]: float("inf")}, "finite"),
        ]:
            with self.subTest(targets=targets):
                def decide(view, params, prev_decision):
                    return prajna.Decision(targets)

                with self.assertRaisesRegex(ValueError, message):
                    self.run_strategy(decide)

    def test_rust_factor_dependencies_control_warmup_and_export_only_declared_values(self):
        seen = []

        def decide(view, params, prev_decision):
            self.assertEqual(set(view.factors), {"momentum", "volatility"})
            current = [
                row for row in view.factors["momentum"].to_pylist()
                if row["session_date"].isoformat() == view.session_date
                and row["instrument_id"] == MEMBERS[0]
            ]
            seen.append((view.session_date, current[0]["value"]))
            return prajna.Decision({})

        strategy = prajna.Strategy(
            "factors", "1", bar_columns=(),
            factors=(
                prajna.FactorDependency("momentum", "momentum", {"n": 1}),
                prajna.FactorDependency("volatility", "volatility", {"n": 2}),
            ),
            lookback_sessions=2, rebalance_every=3,
        )
        self.run_strategy(decide, strategy=strategy)
        self.assertEqual([date for date, _ in seen], [
            "2026-01-07", "2026-01-12", "2026-01-15",
        ])
        self.assertAlmostEqual(seen[0][1], 102 / 101 - 1)

    def test_view_excludes_future_unknown_and_undeclared_data_even_in_arrow_buffers(self):
        fixture = dict(self.fixture)
        late = int(datetime(2026, 1, 8, 7, tzinfo=timezone.utc).timestamp()) * 10**9
        overrides = [
            {"instrument_id": MEMBERS[0], "session_date": "2026-01-07", "available_at": late},
            {"instrument_id": MEMBERS[1], "session_date": "2026-01-06", "available_at": None},
        ]
        fixture["test_bars_availability"] = overrides
        fixture["test_execution_status_availability"] = overrides
        dsv = self.publish(fixture)
        views = []

        def decide(view, params, prev_decision):
            views.append(view)
            self.assertEqual(view.bars.column_names, [
                "instrument_id", "session_date", "available_at", "close",
            ])
            with self.assertRaises(KeyError):
                view.bars["open"]
            with self.assertRaises(IndexError):
                view.bars["close"][view.bars.num_rows]
            with self.assertRaises(KeyError):
                view.factors["not_declared"]
            for attr in ("account", "holdings", "fills"):
                self.assertFalse(hasattr(view, attr))
            dates = [row["session_date"] for row in view.sessions.to_pylist()]
            for table in (view.bars, view.execution_status, *view.factors.values()):
                for row in table.to_pylist():
                    self.assertIn(row["session_date"], dates)
                    self.assertIsNotNone(row["available_at"])
                    self.assertLessEqual(
                        int(row["available_at"].timestamp()) * 10**9, view.ts_close,
                    )
                for chunk in table["session_date"].chunks:
                    # A reconstructed array cannot extend to a hidden "next" row.
                    self.assertEqual(chunk.offset, 0)
                    self.assertEqual(chunk.buffers()[1].size, 4 * len(chunk))
            close = view.bars["close"].chunk(0)
            self.assertEqual(close.buffers()[1].size, 16 * len(close))
            self.assertFalse(close.buffers()[1].is_mutable)
            return prajna.Decision({})

        strategy = prajna.Strategy(
            "boundary", "1", lookback_sessions=3, rebalance_every=2,
            factors=(prajna.FactorDependency("momentum", "momentum", {"n": 1}),),
        )
        self.run_strategy(decide, dsv=dsv, strategy=strategy)
        first_keys = {
            (row["instrument_id"], row["session_date"].isoformat())
            for row in views[0].bars.to_pylist()
        }
        self.assertNotIn((MEMBERS[0], "2026-01-07"), first_keys)
        self.assertNotIn((MEMBERS[1], "2026-01-06"), first_keys)
        later_keys = {
            (row["instrument_id"], row["session_date"].isoformat())
            for row in views[1].bars.to_pylist()
        }
        self.assertIn((MEMBERS[0], "2026-01-07"), later_keys)
        first_factor_keys = {
            (row["instrument_id"], row["session_date"].isoformat())
            for row in views[0].factors["momentum"].to_pylist()
        }
        self.assertNotIn((MEMBERS[0], "2026-01-07"), first_factor_keys)
        self.assertNotIn((MEMBERS[1], "2026-01-07"), first_factor_keys)

    def test_unknown_values_require_an_explicit_availability_assumption(self):
        fixture = dict(self.fixture)
        fixture["test_bars_availability"] = [
            {"instrument_id": MEMBERS[0], "session_date": "2026-01-07", "available_at": None},
        ]
        dsv = self.publish(fixture)
        seen = []

        def decide(view, params, prev_decision):
            seen.append(view)
            return prajna.Decision({})

        self.run_strategy(decide, dsv=dsv, assumption="treat_unknown_as_ts_close")
        self.assertEqual(seen[0].availability_assumption, "treat_unknown_as_ts_close")
        self.assertIn(
            (MEMBERS[0], "2026-01-07"),
            {(row["instrument_id"], row["session_date"].isoformat())
             for row in seen[0].bars.to_pylist()},
        )

    def test_none_is_not_a_skip_and_python_errors_are_not_hidden(self):
        def skip(view, params, prev_decision):
            return None

        with self.assertRaisesRegex(TypeError, "Decision"):
            self.run_strategy(skip)

        def broken(view, params, prev_decision):
            raise RuntimeError("strategy failed")

        with self.assertRaisesRegex(RuntimeError, "strategy failed"):
            self.run_strategy(broken)

    def test_params_and_previous_decision_cannot_mutate_later_calls(self):
        seen = []

        def decide(view, params, prev_decision):
            seen.append(params["nested"]["value"])
            params["nested"]["value"] = 99
            if prev_decision:
                prev_decision.targets[MEMBERS[0]] = 0.9
                with self.assertRaises(AttributeError):
                    prev_decision.session_date = "future"
            return prajna.Decision({MEMBERS[0]: 0.4}, [(MEMBERS[0], 1.5, 1)])

        params = {"nested": {"value": 7}}
        decisions = self.run_strategy(decide, params=params)
        self.assertEqual(seen, [7, 7, 7, 7])
        self.assertEqual(params, {"nested": {"value": 7}})
        self.assertEqual([d.targets for d in decisions], [{MEMBERS[0]: 0.4}] * 4)
        self.assertEqual(decisions[0].ranked, [(MEMBERS[0], 1.5, 1)])

    def test_invalid_declarations_fail_before_calling_python(self):
        def decide(view, params, prev_decision):
            self.fail("invalid declarations must not call decide")

        for kwargs, message in [
            ({"lookback_sessions": 0}, "at least 1"),
            ({"rebalance_every": 0}, "at least 1"),
            ({"capability": "EventDriven"}, "Vectorizable"),
            ({"bar_columns": ("forward_return",)}, "Bar column"),
            ({"bar_columns": ("close", "close")}, "duplicate"),
            ({"strategy_id": ""}, "identity component"),
            ({"factors": (prajna.FactorDependency("bad", "python_factor", {}),)}, "factor kind"),
            ({"factors": (prajna.FactorDependency("bad", "momentum", {"n": 0}),)}, "at least 1"),
            ({"factors": (prajna.FactorDependency("bad", "momentum", {"n": 1, "extra": 1}),)}, "unknown field"),
        ]:
            with self.subTest(kwargs=kwargs):
                values = {"strategy_id": "test", "version": "1"} | kwargs
                with self.assertRaisesRegex(ValueError, message):
                    self.run_strategy(decide, strategy=prajna.Strategy(**values))

    def test_static_universe_is_projected_before_export_and_validation(self):
        def decide(view, params, prev_decision):
            self.assertEqual(view.universe, [MEMBERS[1]])
            expected = set() if view.session_date == "2026-01-16" else {MEMBERS[1]}
            self.assertEqual(set(view.bars["instrument_id"].to_pylist()), expected)
            self.assertEqual(
                set(view.execution_status["instrument_id"].to_pylist()), {MEMBERS[1]},
            )
            return prajna.Decision({MEMBERS[1]: 1.0})

        decisions = prajna.generate_decisions(
            lake_root=str(self.lake), dsv=self.dsv, venue="SYNTH",
            members=(MEMBERS[1],), strategy=prajna.Strategy("subset", "1"),
            decide=decide, params={},
        )
        self.assertEqual(len(decisions), 10)

    def test_no_eligible_sessions_produces_no_callbacks_or_partial_windows(self):
        def decide(view, params, prev_decision):
            self.fail("warmup must complete before any callback")

        for strategy in [
            prajna.Strategy("long_window", "1", lookback_sessions=11),
            prajna.Strategy(
                "long_factor", "1",
                factors=(prajna.FactorDependency("momentum", "momentum", {"n": 10}),),
            ),
        ]:
            with self.subTest(strategy=strategy):
                self.assertEqual(self.run_strategy(decide, strategy=strategy), [])

    def test_rank_validation_and_weight_boundaries(self):
        for ranked in [
            [(MEMBERS[0], float("nan"), 1)],
            [(MEMBERS[0], 1.0, 0)],
            [(MEMBERS[0], 1.0, 1), (MEMBERS[1], 2.0, 1)],
            [(MEMBERS[0], 1.0, 1), (MEMBERS[0], 2.0, 2)],
            [("OUTSIDE.SYNTH", 1.0, 1)],
        ]:
            with self.subTest(ranked=ranked):
                def decide(view, params, prev_decision):
                    return prajna.Decision({}, ranked)

                with self.assertRaises(ValueError):
                    self.run_strategy(decide)
        for targets in ({}, {MEMBERS[0]: 0.0}, {MEMBERS[0]: 1.0}):
            with self.subTest(targets=targets):
                def decide(view, params, prev_decision):
                    return prajna.Decision(targets)

                self.assertEqual(self.run_strategy(decide)[0].targets, targets)
        with self.assertRaisesRegex(ValueError, "sum"):
            self.run_strategy(
                lambda view, params, previous: prajna.Decision({MEMBERS[0]: 1.0000000000000002}),
            )

    def test_rust_canonicalizes_json_params_and_rejects_non_json_values(self):
        def decide(view, params, prev_decision):
            self.assertEqual(list(params), ["a", "z"])
            return prajna.Decision({})

        self.run_strategy(decide, params={"z": 1, "a": 2})
        for value in (float("nan"), float("inf"), object()):
            with self.subTest(value=value):
                with self.assertRaises((ValueError, TypeError)):
                    self.run_strategy(decide, params={"value": value})


if __name__ == "__main__":
    unittest.main()
