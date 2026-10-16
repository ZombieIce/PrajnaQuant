import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest

import prajna


ROOT = Path(__file__).resolve().parents[2]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
if not TARGET.is_absolute():
    TARGET = ROOT / TARGET
CLI = TARGET / "debug" / "prajna-experiment"
PREPARE = TARGET / "debug" / "examples" / "prepare_fixture"


class ExperimentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not CLI.is_file() or not PREPARE.is_file():
            raise RuntimeError(
                "Build fixtures first: cargo build -p prajna-experiment "
                "--bin prajna-experiment --example prepare_fixture --locked --offline"
            )

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.lake = self.base / "lake"
        self.definition = self.base / "experiment.json"
        self.repo = self.base / "repository"
        self.repo.mkdir()
        self.definition_data = json.loads(
            subprocess.check_output([PREPARE, self.lake, self.definition], text=True)
        )
        self.git_env = os.environ.copy()
        for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
            self.git_env.pop(key, None)
        self.git_env.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "--quiet")
        self.git(
            "-c", "user.name=Python Test",
            "-c", "user.email=python@example.invalid",
            "-c", "commit.gpgsign=false",
            "commit", "--allow-empty", "--quiet", "-m", "fixture",
        )

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, env=self.git_env, check=True)

    def experiment(self, strategy, space, engine=None):
        return prajna.Experiment(
            dataset_version=prajna.DatasetVersion(self.definition_data["dsv"]),
            universe=prajna.Universe(self.definition_data["universe"]),
            venue=prajna.Venue("SYNTH"),
            strategy=strategy,
            engine=engine or prajna.Engine(prajna.EngineKind.VECTOR),
            availability_assumption=prajna.AvailabilityAssumption.NONE,
            costs=prajna.Costs(0.001, 10.0, 10.0, 0.0, 0.0),
            parameter_space=space,
        )

    def test_typed_s2_experiment_replays_cli_identity_and_saved_results(self):
        definition = self.definition_data
        experiment = prajna.Experiment(
            dataset_version=prajna.DatasetVersion(definition["dsv"]),
            universe=prajna.Universe(definition["universe"]),
            venue=prajna.Venue("SYNTH"),
            strategy=prajna.RustStrategy.S2_ROTATION,
            engine=prajna.Engine(prajna.EngineKind.VECTOR),
            availability_assumption=prajna.AvailabilityAssumption.NONE,
            costs=prajna.Costs(0.001, 10.0, 10.0, 0.0, 0.0),
            parameter_space=prajna.Grid(definition["parameter_space"]["grid"]),
        )
        self.assertEqual(json.loads(experiment.to_json()), definition)
        cli = json.loads(subprocess.check_output(
            [CLI, "run", self.definition, "--lake", self.lake, "--level", "full"],
            cwd=self.repo, env=self.git_env, text=True,
        ))
        identity = experiment.describe(self.lake)
        self.assertEqual(identity.experiment_id, cli["exp"])
        result = experiment.run(
            self.lake, repository=self.repo, level=prajna.ResultLevel.FULL, threads=1,
        )
        self.assertEqual(result.action, "replayed")
        self.assertEqual(result.manifest["execution"]["id"], cli["exe"])
        self.assertEqual(
            [run.run_id for run in identity.runs],
            [run["run_id"] for run in result.manifest["runs"]],
        )
        self.assertEqual(result.statistics["written_bytes"], 0)
        self.assertEqual(result.statistics["written_files"], 0)

    def test_s1_s2_s3_grid_and_list_match_cli_across_vector_and_fast_event(self):
        s2_grid = self.definition_data["parameter_space"]["grid"]
        s2_params = {key: values[0] for key, values in s2_grid.items()}
        cases = [
            (prajna.RustStrategy.S2_ROTATION, prajna.List([s2_params, s2_params]),
             {"list": [s2_params, s2_params]}),
            (prajna.RustStrategy.BUY_AND_HOLD, prajna.List([{"params_version": 1}]),
             {"list": [{"params_version": 1}]}),
            (prajna.RustStrategy.BUY_AND_HOLD, prajna.Grid({"params_version": [1]}),
             {"grid": {"params_version": [1]}}),
            (prajna.RustStrategy.MA_CROSSOVER,
             prajna.List([{"params_version": 1, "short_window": 20, "long_window": 60}]),
             {"list": [{"params_version": 1, "short_window": 20, "long_window": 60}]}),
            (prajna.RustStrategy.MA_CROSSOVER,
             prajna.Grid({"params_version": [1], "short_window": [20], "long_window": [60]}),
             {"grid": {"params_version": [1], "short_window": [20], "long_window": [60]}}),
        ]
        settings = prajna.FastEventSettings(
            currency="CNY", initial_cash="100000", sizing="lot",
            rebalance="full_target", minimum_commission="0",
            portfolios=[prajna.Capital(1, "100000")],
            lot_size_source=prajna.LotSizeSource(),
        )
        for engine in (
            prajna.Engine(prajna.EngineKind.VECTOR),
            prajna.Engine(prajna.EngineKind.FAST_EVENT, settings=settings),
        ):
            for strategy, space, cli_space in cases:
                with self.subTest(strategy=strategy, space=type(space), engine=engine.kind):
                    definition = dict(self.definition_data)
                    definition["strategy"] = strategy.value
                    definition["engine"] = engine.kind.value
                    definition["parameter_space"] = cli_space
                    if engine.settings is not None:
                        # Independent CLI literals, not serialized Python settings.
                        definition["fast_event"] = {
                            "currency": "CNY", "initial_cash": "100000",
                            "sizing": "lot", "rebalance": "full_target",
                            "minimum_commission": "0",
                            "portfolios": [{"id": 1, "capital": "100000"}],
                            "lot_size_source": {"kind": "instrument"},
                        }
                    self.definition.write_text(json.dumps(definition))
                    cli = json.loads(subprocess.check_output(
                        [CLI, "run", self.definition, "--lake", self.lake, "--level", "full"],
                        cwd=self.repo, env=self.git_env, text=True,
                    ))
                    experiment = self.experiment(strategy, space, engine)
                    self.assertEqual(json.loads(experiment.to_json()), definition)
                    identity = experiment.describe(self.lake)
                    self.assertEqual(identity.experiment_id, cli["exp"])
                    result = experiment.run(
                        self.lake, repository=self.repo, level=prajna.ResultLevel.FULL, threads=1,
                    )
                    self.assertEqual(result.action, "replayed")
                    self.assertEqual(result.manifest["execution"]["id"], cli["exe"])
                    self.assertEqual(cli["failed_count"], 0)
                    self.assertEqual(
                        [run.run_id for run in identity.runs],
                        [run["run_id"] for run in result.manifest["runs"]],
                    )
                    self.assertEqual(
                        (result.path.parent.parent / "experiment.json").read_text(),
                        identity.canonical_json,
                    )
                    for run in result.manifest["runs"]:
                        self.assertEqual(run["status"], "completed")
                        self.assertTrue(run["table_hashes"])
                        if engine.kind == prajna.EngineKind.FAST_EVENT:
                            self.assertTrue(run["accounting_verified"])
                    self.assertEqual(result.statistics["written_bytes"], 0)

    def test_real_experiment_execution_releases_gil(self):
        experiment = self.experiment(
            prajna.RustStrategy.S2_ROTATION,
            prajna.Grid(self.definition_data["parameter_space"]["grid"]),
        )
        ready = threading.Event()
        progress = []

        def work():
            ready.set()
            time.sleep(0.05)
            progress.append(time.monotonic())

        worker = threading.Thread(target=work)
        original_interval = sys.getswitchinterval()
        try:
            sys.setswitchinterval(5.0)
            worker.start()
            self.assertTrue(ready.wait(timeout=5))
            started = time.monotonic()
            result = experiment.run(self.lake, repository=self.repo, threads=1)
            finished = time.monotonic()
        finally:
            sys.setswitchinterval(original_interval)
            worker.join(timeout=5)
        self.assertFalse(worker.is_alive())
        self.assertEqual(result.action, "created")
        self.assertEqual(len(progress), 1)
        self.assertGreater(progress[0], started)
        self.assertLess(progress[0], finished)

    def test_python_reference_is_serialized_and_identified_but_not_executed(self):
        experiment = self.experiment(
            prajna.PythonStrategy("research.rotation", "1"),
            prajna.List([{"nested": {"weight": 0.5}, "window": 5}]),
        )
        self.definition.write_text(experiment.to_json())
        identity = experiment.describe(self.lake)
        self.assertEqual(json.loads(identity.canonical_json)["strategy"], "research.rotation@1")
        self.assertEqual(identity.runs[0].run_spec["strategy"]["kind"], "research.rotation@1")
        with self.assertRaisesRegex(ValueError, "requires Python host"):
            experiment.run(self.lake, repository=self.repo)
        cli = subprocess.run(
            [CLI, "run", self.definition, "--lake", self.lake],
            cwd=self.repo, env=self.git_env, text=True, capture_output=True,
        )
        self.assertEqual(cli.returncode, 2)
        self.assertIn("requires Python host", cli.stderr)
        self.assertFalse((self.lake / "experiments").exists())

    def test_python_reference_grid_deduplicates_runs_and_preserves_json_types(self):
        first = self.experiment(
            prajna.PythonStrategy("research.rotation", "1"),
            prajna.Grid({"weight": [0.5, 0.5], "label": ["a", "b"]}),
        ).describe(self.lake)
        second = self.experiment(
            prajna.PythonStrategy("research.rotation", "1"),
            prajna.List([{"label": "a", "weight": 0.5}, {"weight": 0.5, "label": "b"}]),
        ).describe(self.lake)
        self.assertEqual(len(first.runs), 2)
        self.assertEqual(first.runs, second.runs)
        self.assertNotEqual(first.experiment_id, second.experiment_id)
        third = self.experiment(
            prajna.PythonStrategy("research.rotation", "2"),
            prajna.List([{"label": "a", "weight": 0.5}, {"weight": 0.5, "label": "b"}]),
        ).describe(self.lake)
        self.assertNotEqual(second.experiment_id, third.experiment_id)
        self.assertNotEqual(second.runs, third.runs)

    def test_result_levels_and_replay_limits_use_rust_storage(self):
        experiment = self.experiment(
            prajna.RustStrategy.BUY_AND_HOLD, prajna.List([{"params_version": 1}]),
        )
        run_id = None
        for level in prajna.ResultLevel:
            if run_id is not None:
                with self.assertRaisesRegex(RuntimeError, "use promote"):
                    experiment.run(self.lake, repository=self.repo, level=level, threads=1)
                self.git(
                    "-c", "user.name=Python Test",
                    "-c", "user.email=python@example.invalid",
                    "-c", "commit.gpgsign=false",
                    "commit", "--allow-empty", "--quiet", "-m", level.value,
                )
            result = experiment.run(self.lake, repository=self.repo, level=level, threads=1)
            run = result.manifest["runs"][0]
            self.assertEqual(run["result_level"], level.value)
            self.assertEqual(run["status"], "completed")
            if run_id is not None:
                self.assertEqual(run_id, run["run_id"])
            run_id = run["run_id"]
            self.assertEqual(
                set(run["table_hashes"]),
                {
                    prajna.ResultLevel.SUMMARY: set(),
                    prajna.ResultLevel.STANDARD: {
                        "sessions", "executions", "corporate_actions",
                    },
                    prajna.ResultLevel.FULL: {
                        "sessions", "executions", "corporate_actions", "target_weights",
                        "weights_after_execution", "decisions", "pending_at_end",
                    },
                }[level],
            )
            if level != prajna.ResultLevel.SUMMARY:
                actions = prajna.read_run(
                    str(self.lake), result.manifest["experiment_id"],
                    result.manifest["execution"]["id"], run_id,
                    table="corporate_actions", result_level=level,
                )
                self.assertEqual(actions.num_rows, 0)

    def test_invalid_parameters_and_runtime_options_are_explicit_errors(self):
        for params in ({1: "value"}, {"bad": object()}, {"bad": float("nan")}):
            with self.subTest(params=params):
                experiment = self.experiment(
                    prajna.PythonStrategy("research.rotation", "1"), prajna.List([params]),
                )
                with self.assertRaises((TypeError, ValueError)):
                    experiment.to_json()
        for params in ({"big": 2**53}, {"bad": float("inf")}):
            with self.subTest(params=params):
                experiment = self.experiment(
                    prajna.PythonStrategy("research.rotation", "1"), prajna.List([params]),
                )
                with self.assertRaises(ValueError):
                    experiment.describe(self.lake)
        for strategy in (
            prajna.PythonStrategy("", "1"), prajna.PythonStrategy("a@b", "1"),
            prajna.PythonStrategy("a", " "), prajna.PythonStrategy("a", "1@2"),
        ):
            with self.subTest(strategy=strategy), self.assertRaises(ValueError):
                self.experiment(strategy, prajna.List([{}])).to_json()
        experiment = self.experiment(
            prajna.RustStrategy.BUY_AND_HOLD, prajna.List([{"params_version": 1}]),
        )
        with self.assertRaisesRegex(ValueError, "threads must be greater than zero"):
            experiment.run(self.lake, repository=self.repo, threads=0)
        with self.assertRaisesRegex(ValueError, "supplied together"):
            experiment.run(self.lake, repository=self.repo, backend_python=sys.executable)
        with self.assertRaisesRegex(ValueError, "invalid dataset manifest"):
            experiment.describe(self.base / "missing")
        self.assertFalse((self.lake / "experiments").exists())


if __name__ == "__main__":
    unittest.main()
