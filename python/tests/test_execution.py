"""Python-hosted two-stage execution; synthetic evidence only."""
from dataclasses import replace
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest

import prajna
from acceptance_strategies import s2_declaration, s2_decide
from test_experiment import ExperimentTests, ROOT


def decide(view, params, previous):
    return prajna.Decision({view.universe[0]: params["weight"]})


class ExecutionTests(unittest.TestCase):
    setUpClass = ExperimentTests.__dict__["setUpClass"]
    setUp = ExperimentTests.setUp
    git = ExperimentTests.git
    experiment = ExperimentTests.experiment

    def python_experiment(self, engine=None, space=None):
        return self.experiment(
            prajna.PythonStrategy("test.execution", "1"),
            space or prajna.Grid({"weight": [0.25, 0.5]}), engine,
        )

    def run_python(self, experiment, callback=decide, **kwargs):
        return experiment.run(
            self.lake, repository=self.repo,
            strategy=prajna.Strategy("test.execution", "1", rebalance_every=20),
            decide=callback, threads=2, **kwargs,
        )

    def test_grid_results_retention_and_run_by_run_replay(self):
        settings = prajna.FastEventSettings(
            currency="CNY", initial_cash="100000", sizing="lot",
            rebalance="full_target", minimum_commission="0",
            portfolios=[prajna.Capital(1, "100000")],
            lot_size_source=prajna.LotSizeSource(),
        )
        for engine in (prajna.Engine(prajna.EngineKind.VECTOR),
                       prajna.Engine(prajna.EngineKind.FAST_EVENT, settings)):
            for level in prajna.ResultLevel:
                with self.subTest(engine=engine.kind, level=level):
                    # Same Run specs across retention levels, new git revision per Execution.
                    self.git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                             "-c", "commit.gpgsign=false", "commit", "--allow-empty",
                             "--quiet", "-m", f"{engine.kind}-{level}")
                    experiment = self.python_experiment(engine)
                    result = self.run_python(experiment, level=level)
                    runs = result.manifest["runs"]
                    self.assertEqual(len(runs), 2)
                    for run in runs:
                        self.assertEqual(run["status"], "completed")
                        self.assertEqual(run["result_level"], level.value)
                        self.assertEqual(bool(run["table_hashes"]), level != prajna.ResultLevel.SUMMARY)
                        if engine.kind == prajna.EngineKind.FAST_EVENT:
                            self.assertTrue(run["accounting_verified"])
                    replay = self.run_python(experiment, level=level)
                    self.assertEqual(replay.action, "replayed")
                    self.assertEqual(result.manifest["summary_logical_hash"], replay.manifest["summary_logical_hash"])
                    self.assertEqual([run["table_hashes"] for run in runs],
                                     [run["table_hashes"] for run in replay.manifest["runs"]])
                    self.assertEqual(replay.statistics["written_bytes"], 0)
                    metadata = result.manifest["execution"]["python"]
                    self.assertEqual(len(metadata["source_sha256"]), 64)
                    self.assertEqual(len(metadata["callable_sha256"]), 64)
                    self.assertEqual(metadata["python_version"], sys.version)
                    self.assertIn("pyarrow==", metadata["dependency_lock"].lower())
                    self.assertEqual(metadata["dependency_lock_source"], "sys.executable -m pip freeze --all")
                    self.assertTrue(result.manifest["execution"]["reproducible"])

    def test_notebook_function_is_not_fully_reproducible(self):
        namespace = {"prajna": prajna}
        exec(compile("def notebook(view, params, previous):\n return prajna.Decision({})", "<ipython-input-1>", "exec"), namespace)
        result = self.run_python(self.python_experiment(), namespace["notebook"])
        self.assertFalse(result.manifest["execution"]["reproducible"])
        self.assertIsNone(result.manifest["execution"]["python"]["source_file"])

    def test_changed_decisions_in_same_execution_are_rejected(self):
        # Trusted pure functions are required; this deliberate violation proves replay protection.
        weights = [0.25]
        def impure(view, params, previous):
            return prajna.Decision({view.universe[0]: weights[0]})
        experiment = self.python_experiment()
        self.run_python(experiment, impure, level=prajna.ResultLevel.FULL)
        weights[0] = 0.75
        with self.assertRaisesRegex(RuntimeError, "mismatch|differ"):
            self.run_python(experiment, impure, level=prajna.ResultLevel.FULL)

    def test_callbacks_are_serial_before_gil_free_execution(self):
        calls = []
        last_callback = []
        progress = []
        ready = threading.Event()
        execute_stage = threading.Event()
        def callback(view, params, previous):
            calls.append((threading.get_ident(), params["weight"]))
            last_callback[:] = [time.monotonic()]
            # Fixture: 252 Sessions, rebalance every 20 => 13 calls per Run.
            if len(calls) == 26:
                execute_stage.set()
            return decide(view, params, previous)
        def other_thread():
            ready.set()
            if execute_stage.wait(30):
                progress.append(time.monotonic())
        worker = threading.Thread(target=other_thread)
        interval = sys.getswitchinterval()
        try:
            sys.setswitchinterval(5.0)
            worker.start()
            self.assertTrue(ready.wait(5))
            result = self.run_python(self.python_experiment(), callback)
            finished = time.monotonic()
        finally:
            sys.setswitchinterval(interval)
            worker.join(5)
        self.assertEqual(result.action, "created")
        self.assertEqual(set(tid for tid, _ in calls), {threading.get_ident()})
        # All calls for each combination form one block; Rayon never calls Python.
        transitions = sum(left[1] != right[1] for left, right in zip(calls, calls[1:]))
        self.assertEqual(transitions, 1)
        self.assertEqual(len(progress), 1)
        self.assertGreater(progress[0], last_callback[0])
        self.assertLess(progress[0], finished)

    def test_reference_seed_and_callback_errors_do_not_publish_results(self):
        experiment = self.python_experiment(space=prajna.List([{"seed": 1, "weight": 0.5}]))
        with self.assertRaisesRegex(ValueError, "seed"):
            self.run_python(experiment)
        with self.assertRaisesRegex(ValueError, "must match"):
            self.python_experiment().run(self.lake, strategy=prajna.Strategy("wrong", "1"), decide=decide)
        with self.assertRaisesRegex(ValueError, "at least 1"):
            self.python_experiment().run(
                self.lake, repository=self.repo,
                strategy=prajna.Strategy("test.execution", "1", lookback_sessions=0),
                decide=decide,
            )
        self.assertFalse((self.lake / "experiments").exists())

    def test_invalid_decision_fails_only_its_run_and_replays(self):
        experiment = self.python_experiment(space=prajna.Grid({"weight": [-0.5, 0.5]}))
        result = self.run_python(experiment, level=prajna.ResultLevel.FULL)
        runs = result.manifest["runs"]
        self.assertEqual(sorted(run["status"] for run in runs), ["completed", "failed"])
        for run in runs:
            summary = prajna.read_run(str(self.lake), result.manifest["experiment_id"],
                                      result.manifest["execution"]["id"], run["run_id"],
                                      table="summary").to_pylist()[0]
            if run["status"] == "failed":
                self.assertIn("nonnegative", summary["error"])
                self.assertFalse(run["table_hashes"])
            else:
                self.assertTrue(run["table_hashes"])
        self.assertEqual(self.run_python(experiment, level=prajna.ResultLevel.FULL).action, "replayed")

    @unittest.skipUnless(os.environ.get("PRAJNA_PY_NAUTILUS"), "real Nautilus only in Full workflow")
    def test_real_accurate_python_run_in_isolated_worker(self):
        settings = prajna.FastEventSettings(
            currency="CNY", initial_cash="100000", sizing="lot", rebalance="full_target",
            minimum_commission="0", portfolios=[prajna.Capital(1, "100000")],
            lot_size_source=prajna.LotSizeSource(),
        )
        engine = prajna.Engine(prajna.EngineKind.ACCURATE_EVENT, settings,
                              prajna.Backend("nautilus_trader", "2.0.0rc6"))
        params = {key: values[0] for key, values in
                  self.definition_data["parameter_space"]["grid"].items()}
        declaration = replace(s2_declaration(params), strategy_id="test.execution")
        experiment = replace(
            self.python_experiment(engine, prajna.List([params])),
            # Fixture quote precision is 0.01; do not request fractional-tick slippage.
            costs=prajna.Costs(commission_rate=0.001),
        )
        # A hostile PYTHONPATH must not override the selected worker venv's Nautilus.
        poison = self.base / "poison"
        poison.mkdir()
        (poison / "nautilus_trader.py").write_text("raise RuntimeError('host path leaked')")
        old = os.environ.get("PYTHONPATH")
        os.environ["PYTHONPATH"] = str(poison)
        try:
            result = experiment.run(
                self.lake, repository=self.repo, strategy=declaration, decide=s2_decide,
                level=prajna.ResultLevel.FULL, backend_python=sys.executable,
                backend_worker=ROOT / "backends/nautilus/worker.py",
            )
        finally:
            if old is None:
                os.environ.pop("PYTHONPATH", None)
            else:
                os.environ["PYTHONPATH"] = old
        run = result.manifest["runs"][0]
        self.assertEqual(run["status"], "completed", run)
        self.assertTrue(run["accounting_verified"])
        self.assertIn("backend_events", run["table_hashes"])
        self.assertEqual(experiment.run(
            self.lake, repository=self.repo, strategy=declaration, decide=s2_decide,
            level=prajna.ResultLevel.FULL, backend_python=sys.executable,
            backend_worker=ROOT / "backends/nautilus/worker.py",
        ).action, "replayed")

# Borrow fixture setup without exposing its test class twice to unittest discovery.
del ExperimentTests
