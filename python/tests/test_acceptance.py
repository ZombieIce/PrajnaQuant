"""MVP-5 acceptance on synthetic fixtures, not real-market/PIT evidence."""
from dataclasses import replace
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import prajna
from acceptance_strategies import s2_declaration, s2_decide, s3_declaration, s3_decide
from test_experiment import CLI, ROOT, TARGET


S2_PARAMETERS = ("short", "long", "vol", "w_s", "w_l", "w_v", "trend",
                 "top_k", "rebalance_every")


def exact_values(value):
    if isinstance(value, float):
        return value.hex()
    if isinstance(value, dict):
        return {key: exact_values(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [exact_values(item) for item in value]
    return value


class AcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.lake = self.base / "lake"
        self.repo = self.base / "repository"
        self.repo.mkdir()
        self.git_env = os.environ.copy()
        for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
            self.git_env.pop(key, None)
        self.git_env.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "--quiet")
        self.revision("fixture")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, env=self.git_env, check=True)

    def revision(self, message):
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "--allow-empty", "--quiet",
                 "-m", message)

    def fixture(self, name):
        fixture = json.loads(subprocess.check_output(
            [TARGET / "debug/examples/acceptance_fixture", self.lake, name],
            cwd=ROOT, text=True,
        ))
        self.assertEqual(
            (len(fixture["members"]), fixture["session_count"]),
            (3, 10) if name == "dataset-v1" else (64, 252),
        )
        return fixture

    def engines(self):
        settings = prajna.FastEventSettings(
            currency="CNY", initial_cash="100000", sizing="lot",
            rebalance="full_target", minimum_commission="0",
            portfolios=[prajna.Capital(1, "100000")],
            lot_size_source=prajna.LotSizeSource(),
        )
        return (prajna.Engine(prajna.EngineKind.VECTOR),
                prajna.Engine(prajna.EngineKind.FAST_EVENT, settings))

    def experiment(self, fixture, strategy, params, engine):
        definition = fixture["definition"]
        return prajna.Experiment(
            dataset_version=prajna.DatasetVersion(definition["dsv"]),
            universe=prajna.Universe(definition["universe"]), venue=prajna.Venue("SYNTH"),
            strategy=strategy, engine=engine, costs=prajna.Costs(**definition["costs"]),
            parameter_space=prajna.List(params),
        )

    def table(self, result, run, table):
        return prajna.read_run(
            str(self.lake), result.manifest["experiment_id"],
            result.manifest["execution"]["id"], run["run_id"], table=table,
        )

    def payloads(self, result, run):
        summary = self.table(result, run, "summary").to_pylist()[0]
        # Strategy identities differ by design. Built-in parameter columns are
        # metadata, null for Python refs; every result/metric column is retained.
        for key in ("run_id", *S2_PARAMETERS):
            summary.pop(key)
        summary["summary_json"] = json.loads(summary["summary_json"])
        return exact_values({
            "summary": summary,
            **{table: self.table(result, run, table).to_pylist()
               for table in run["table_hashes"]},
        })

    def test_s2_decisions_match_rust_on_every_session(self):
        for name in ("dataset-v1", "64x252"):
            with self.subTest(fixture=name):
                fixture = self.fixture(name)
                for case in fixture["s2"]:
                    params = case["params"]
                    decisions = prajna.generate_decisions(
                        lake_root=str(self.lake), dsv=fixture["definition"]["dsv"],
                        venue="SYNTH", members=fixture["members"],
                        strategy=s2_declaration(params), decide=s2_decide, params=params,
                    )
                    self.assertTrue(decisions)
                    self.assertEqual(
                        [{"session_date": d.session_date, "targets": d.targets,
                          "ranked": [list(row) for row in d.ranked]} for d in decisions],
                        case["decisions"],
                    )

    def test_s2_cli_identity_and_all_retained_values_match_python_api(self):
        for name in ("dataset-v1", "64x252"):
            fixture = self.fixture(name)
            for engine in self.engines():
                experiment = self.experiment(
                    fixture, prajna.RustStrategy.S2_ROTATION,
                    [case["params"] for case in fixture["s2"]], engine,
                )
                for level in (prajna.ResultLevel.SUMMARY, prajna.ResultLevel.STANDARD):
                    with self.subTest(fixture=name, engine=engine.kind, level=level):
                        self.revision(f"{name}-{engine.kind}-{level}")
                        path = self.base / "experiment.json"
                        cli_definition = dict(fixture["definition"])
                        cli_definition["engine"] = engine.kind.value
                        if engine.kind == prajna.EngineKind.FAST_EVENT:
                            cli_definition["fast_event"] = {
                                "currency": "CNY", "initial_cash": "100000", "sizing": "lot",
                                "rebalance": "full_target", "minimum_commission": "0",
                                "portfolios": [{"id": 1, "capital": "100000"}],
                                "lot_size_source": {"kind": "instrument"},
                            }
                        self.assertEqual(json.loads(experiment.to_json()), cli_definition)
                        path.write_text(json.dumps(cli_definition))
                        cli = json.loads(subprocess.check_output(
                            [CLI, "run", path, "--lake", self.lake, "--level", level.value],
                            cwd=self.repo, env=self.git_env, text=True,
                        ))
                        self.assertEqual(cli["failed_count"], 0)
                        identity = experiment.describe(self.lake)
                        self.assertEqual(identity.experiment_id, cli["exp"])
                        tables = ("summary",)
                        if level == prajna.ResultLevel.STANDARD:
                            tables += (("sessions", "executions")
                                       if engine.kind == prajna.EngineKind.VECTOR
                                       else ("account_valuations", "portfolio_valuations"))
                        before = {
                            run.run_id: {
                                table: exact_values(prajna.read_run(
                                    str(self.lake), cli["exp"], cli["exe"], run.run_id,
                                    table=table,
                                ).to_pylist())
                                for table in tables
                            } for run in identity.runs
                        }
                        result = experiment.run(
                            self.lake, repository=self.repo, level=level, threads=2,
                        )
                        self.assertEqual(result.action, "replayed")
                        self.assertEqual(result.manifest["execution"]["id"], cli["exe"])
                        self.assertEqual([run.run_id for run in identity.runs],
                                         [run["run_id"] for run in result.manifest["runs"]])
                        for run in result.manifest["runs"]:
                            for table, values in before[run["run_id"]].items():
                                self.assertEqual(
                                    exact_values(self.table(result, run, table).to_pylist()), values,
                                )

    def test_s2_python_strategy_summary_standard_and_replay_match_builtin(self):
        for name in ("dataset-v1", "64x252"):
            fixture = self.fixture(name)
            params = [case["params"] for case in fixture["s2"]]
            for engine in self.engines():
                builtin = self.experiment(fixture, prajna.RustStrategy.S2_ROTATION, params, engine)
                python = replace(builtin, strategy=prajna.PythonStrategy("acceptance.s2", "1"))
                for level in (prajna.ResultLevel.SUMMARY, prajna.ResultLevel.STANDARD):
                    with self.subTest(fixture=name, engine=engine.kind, level=level):
                        self.revision(f"{name}-{engine.kind}-{level}")
                        rust = builtin.run(self.lake, repository=self.repo, level=level, threads=2)
                        kwargs = dict(repository=self.repo, level=level, threads=2,
                                      strategy=s2_declaration(params[0]), decide=s2_decide)
                        result = python.run(self.lake, **kwargs)
                        self.assertEqual(len(result.manifest["runs"]), 2)
                        for values in params:
                            rust_id = replace(builtin, parameter_space=prajna.List([values])).describe(self.lake).runs[0].run_id
                            py_id = replace(python, parameter_space=prajna.List([values])).describe(self.lake).runs[0].run_id
                            left = next(r for r in rust.manifest["runs"] if r["run_id"] == rust_id)
                            right = next(r for r in result.manifest["runs"] if r["run_id"] == py_id)
                            self.assertEqual(left["status"], "completed", left)
                            self.assertEqual(right["status"], "completed", right)
                            self.assertEqual(self.payloads(rust, left), self.payloads(result, right))
                            if engine.kind == prajna.EngineKind.FAST_EVENT:
                                self.assertTrue(left["accounting_verified"])
                                self.assertTrue(right["accounting_verified"])
                            if engine.kind == prajna.EngineKind.FAST_EVENT:
                                self.assertTrue(left["accounting_verified"])
                                self.assertTrue(right["accounting_verified"])
                        replay = python.run(self.lake, **kwargs)
                        self.assertEqual(replay.action, "replayed")
                        self.assertEqual(result.manifest["execution"]["id"],
                                         replay.manifest["execution"]["id"])
                        self.assertEqual(replay.statistics["written_bytes"], 0)
                        for left, right in zip(result.manifest["runs"], replay.manifest["runs"]):
                            self.assertEqual(left["run_id"], right["run_id"])
                            self.assertEqual(left["table_hashes"], right["table_hashes"])
                            self.assertEqual(self.payloads(result, left), self.payloads(replay, right))
                        metadata = result.manifest["execution"]["python"]
                        self.assertEqual(metadata["python_version"], sys.version)
                        self.assertEqual(metadata["source_sha256"], hashlib.sha256(
                            Path(metadata["source_file"]).read_bytes()).hexdigest())
                        frozen = subprocess.check_output(
                            [sys.executable, "-m", "pip", "freeze", "--all"], text=True,
                        )
                        self.assertEqual(metadata["dependency_lock"],
                                         "\n".join(sorted(frozen.splitlines())) + "\n")
                        self.assertTrue(result.manifest["execution"]["reproducible"])

    def test_s3_ma20_60_decisions_and_results_match_on_registered_fixtures(self):
        for name in ("dataset-v1", "64x252"):
            with self.subTest(fixture=name):
                fixture = self.fixture(name)
                decisions = prajna.generate_decisions(
                    lake_root=str(self.lake), dsv=fixture["definition"]["dsv"],
                    venue="SYNTH", members=fixture["members"], strategy=s3_declaration(),
                    decide=s3_decide, params={},
                )
                self.assertEqual(
                    [{"session_date": d.session_date, "targets": d.targets,
                      "ranked": [list(row) for row in d.ranked]} for d in decisions],
                    fixture["s3"],
                )
                if name == "dataset-v1":
                    self.assertEqual(decisions, [])  # Ten Sessions cannot warm up MA60.
                else:
                    self.assertEqual(len(decisions), 192)
                params = [{"params_version": 1, "short_window": 20, "long_window": 60}]
                for engine in self.engines():
                    builtin = self.experiment(fixture, prajna.RustStrategy.MA_CROSSOVER,
                                              params, engine)
                    python = replace(builtin, strategy=prajna.PythonStrategy("acceptance.s3", "1"))
                    for level in (prajna.ResultLevel.SUMMARY, prajna.ResultLevel.STANDARD):
                        self.revision(f"s3-{name}-{engine.kind}-{level}")
                        rust = builtin.run(self.lake, repository=self.repo, level=level)
                        kwargs = dict(repository=self.repo, level=level,
                                      strategy=s3_declaration(), decide=s3_decide)
                        result = python.run(self.lake, **kwargs)
                        left, right = rust.manifest["runs"][0], result.manifest["runs"][0]
                        self.assertEqual(left["status"], "completed", left)
                        self.assertEqual(right["status"], "completed", right)
                        self.assertEqual(self.payloads(rust, left), self.payloads(result, right))
                        replay = python.run(self.lake, **kwargs)
                        self.assertEqual(replay.action, "replayed")
                        self.assertEqual(result.manifest["execution"]["id"],
                                         replay.manifest["execution"]["id"])
                        self.assertEqual(self.payloads(result, right),
                                         self.payloads(replay, replay.manifest["runs"][0]))

    def test_future_next_row_and_unavailable_fields_are_blocked_end_to_end(self):
        fixture = self.fixture("dataset-v1")
        source = json.loads((ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json").read_text())
        late = int(datetime(2026, 1, 8, 7, tzinfo=timezone.utc).timestamp()) * 10**9
        overrides = [
            {"instrument_id": "A.SYNTH", "session_date": "2026-01-07",
             "available_at": late},
            {"instrument_id": "B.SYNTH", "session_date": "2026-01-06",
             "available_at": None},
        ]
        source["test_bars_availability"] = overrides
        source["test_execution_status_availability"] = overrides
        path = self.base / "adversarial.json"
        path.write_text(json.dumps(source))
        dsv = subprocess.check_output(
            ["cargo", "run", "-q", "-p", "prajna-py", "--example", "strategy_fixture",
             "--locked", "--offline", "--", self.lake, path],
            cwd=ROOT, text=True, env=os.environ | {"PYO3_PYTHON": sys.executable},
        ).strip()
        fixture["definition"]["dsv"] = dsv
        declaration = prajna.Strategy(
            "acceptance.boundary", "1", lookback_sessions=3, rebalance_every=2,
            factors=(prajna.FactorDependency("momentum", "momentum", {"n": 1}),),
        )
        dates = []

        def boundary(view, params, previous):
            dates.append(view.session_date)
            self.assertFalse(hasattr(view, "account"))
            self.assertFalse(hasattr(view, "holdings"))
            with self.assertRaises(KeyError):
                view.bars["open"]
            with self.assertRaises(IndexError):
                view.bars["close"][view.bars.num_rows]
            for table in (view.bars, view.execution_status, *view.factors.values()):
                for row in table.to_pylist():
                    self.assertLessEqual(row["session_date"].isoformat(), view.session_date)
                    self.assertIsNotNone(row["available_at"])
                    self.assertLessEqual(int(row["available_at"].timestamp()) * 10**9,
                                         view.ts_close)
                for chunk in table["session_date"].chunks:
                    self.assertEqual(chunk.offset, 0)
                    self.assertEqual(chunk.buffers()[1].size, 4 * len(chunk))
            if view.session_date == "2026-01-07":
                keys = {(row["instrument_id"], row["session_date"].isoformat())
                        for row in view.bars.to_pylist()}
                self.assertNotIn(("A.SYNTH", "2026-01-07"), keys)
                self.assertNotIn(("B.SYNTH", "2026-01-06"), keys)
                factor_keys = {(row["instrument_id"], row["session_date"].isoformat())
                               for row in view.factors["momentum"].to_pylist()}
                self.assertNotIn(("A.SYNTH", "2026-01-07"), factor_keys)
                self.assertNotIn(("B.SYNTH", "2026-01-07"), factor_keys)
            return prajna.Decision({})

        def next_row(view, params, previous):
            view.bars["close"][view.bars.num_rows]
            return prajna.Decision({})

        for engine in self.engines():
            experiment = self.experiment(fixture, prajna.PythonStrategy("acceptance.boundary", "1"),
                                         [{}], engine)
            result = experiment.run(self.lake, repository=self.repo,
                                    level=prajna.ResultLevel.STANDARD,
                                    strategy=declaration, decide=boundary)
            self.assertEqual(result.manifest["runs"][0]["status"], "completed")
            self.assertIn("2026-01-07", dates)
            failed = experiment.run(self.lake, repository=self.repo,
                                    level=prajna.ResultLevel.STANDARD,
                                    strategy=declaration, decide=next_row)
            run = failed.manifest["runs"][0]
            self.assertEqual(run["status"], "failed")
            self.assertFalse(run["table_hashes"])
            self.assertIn("IndexError", self.table(failed, run, "summary").to_pylist()[0]["error"])

    def test_invalid_decisions_fail_runs_without_publishing_standard_tables(self):
        fixture = self.fixture("dataset-v1")

        def invalid(view, params, previous):
            targets = (
                {view.universe[0]: -0.1},
                {view.universe[0]: 0.6, view.universe[1]: 0.5},
                {"OUTSIDE.SYNTH": 0.1},
                {view.universe[0]: float("nan")},
                {view.universe[0]: float("inf")},
            )
            return prajna.Decision(targets[params["case"]])

        for engine in self.engines():
            experiment = self.experiment(
                fixture, prajna.PythonStrategy("acceptance.invalid", "1"),
                [{"case": case} for case in range(5)], engine,
            )
            result = experiment.run(
                self.lake, repository=self.repo, level=prajna.ResultLevel.STANDARD,
                strategy=prajna.Strategy("acceptance.invalid", "1"), decide=invalid,
            )
            self.assertEqual(len(result.manifest["runs"]), 5)
            errors = []
            for run in result.manifest["runs"]:
                self.assertEqual(run["status"], "failed")
                self.assertFalse(run["table_hashes"])
                errors.append(self.table(result, run, "summary").to_pylist()[0]["error"])
                table = ("sessions" if engine.kind == prajna.EngineKind.VECTOR
                         else "account_valuations")
                with self.assertRaises(prajna.ResultNotSavedError):
                    self.table(result, run, table)
            for message, count in (("nonnegative", 1), ("sum", 1), ("Universe", 1), ("finite", 2)):
                self.assertEqual(sum(message in error for error in errors), count)
