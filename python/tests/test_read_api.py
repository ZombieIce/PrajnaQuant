"""Public read API parity against independently written Rust Parquet."""

import json
import os
from pathlib import Path
import subprocess
import sys
import shutil
import tempfile
import unittest

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

import prajna

ROOT = Path(__file__).resolve().parents[2]


class ReadApiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        cls.lake = Path(cls.temp.name)
        output = subprocess.run(
            ["cargo", "run", "-p", "prajna-py", "--example", "read_fixture",
             "--locked", "--offline", "--", str(cls.lake)],
            cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True,
            env={**os.environ, "PYO3_PYTHON": sys.executable},
        )
        cls.fixture = json.loads(output.stdout)
        subprocess.run(
            ["cargo", "build", "-p", "prajna-experiment", "--locked", "--offline",
             "--target-dir", str(ROOT / "target")],
            cwd=ROOT, check=True,
        )

    def test_dataset_selection_matches_rust_panel(self):
        reference = pq.read_table(self.lake / "panel.parquet")
        reference = reference.cast(pa.schema([
            pa.field(field.name, pa.string() if pa.types.is_string_view(field.type) else field.type)
            for field in reference.schema
        ]))
        actual = prajna.read_dataset(
            str(self.lake), self.fixture["dsv"], self.fixture["universe"], "SYNTH"
        )
        self.assertIsInstance(actual, pa.Table)
        self.assertEqual(actual.to_pydict(), reference.to_pydict())
        dates = reference["session_date"].to_pylist()
        start, end = sorted(set(dates))[1:3]
        columns = ["instrument_id", "session_date", "close", "has_bar"]
        expected = reference.filter(
            pc.and_(pc.greater_equal(reference["session_date"], start),
                    pc.less_equal(reference["session_date"], end))
        ).select(columns)
        selected = prajna.read_dataset(
            str(self.lake), self.fixture["dsv"], self.fixture["universe"], "SYNTH",
            columns=columns, start=str(start), end=str(end),
        )
        self.assertEqual(selected.to_pydict(), expected.to_pydict())
        subset = prajna.read_dataset(
            str(self.lake), self.fixture["dsv"], self.fixture["subset"], "SYNTH"
        )
        self.assertEqual(
            subset.to_pydict(),
            reference.filter(pc.equal(reference["instrument_id"],
                                      reference["instrument_id"][0])).to_pydict(),
        )
        empty = prajna.read_dataset(
            str(self.lake), self.fixture["dsv"], self.fixture["universe"], "SYNTH",
            start="2099-01-01",
        )
        self.assertEqual(empty.num_rows, 0)
        self.assertEqual(empty.schema, actual.schema)

    def test_invalid_dataset_and_factor_requests_are_explicit(self):
        args = (str(self.lake), self.fixture["dsv"], self.fixture["universe"], "SYNTH")
        for options in ({"columns": []}, {"columns": ["bad"]},
                        {"columns": ["close", "close"]}, {"start": "not-a-date"},
                        {"start": "2026-01-08", "end": "2026-01-05"}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                prajna.read_dataset(*args, **options)
        for kind, params in (("custom", {"n": 2}), ("momentum", {"n": 0}),
                             ("volatility", {"n": 1}), ("momentum", {"n": True}),
                             ("momentum", {"n": 2, "extra": 1}),
                             ("momentum", {"n": 1.5})):
            with self.subTest(kind=kind, params=params), self.assertRaises(ValueError):
                prajna.Factor(kind, params)
        with self.assertRaises(ValueError):
            prajna.read_factor(*args, prajna.Factor("momentum", {"n": 2}),
                               availability_assumption="automatic")

    def test_corrupt_factor_cache_is_not_recomputed(self):
        with tempfile.TemporaryDirectory() as directory:
            lake = Path(directory) / "lake"
            shutil.copytree(self.lake, lake)
            args = (str(lake), self.fixture["dsv"], self.fixture["universe"],
                    "SYNTH", prajna.Factor("momentum", {"n": 4}))
            first = prajna.read_factor(*args)
            key = first.schema.metadata[b"prajna.fv"].decode().split(":")[-1]
            path = lake / "features" / key / "values.parquet"
            corrupted = path.read_bytes() + b"corrupted"
            path.write_bytes(corrupted)
            with self.assertRaisesRegex(ValueError, "integrity"):
                prajna.read_factor(*args)
            self.assertEqual(path.read_bytes(), corrupted)

    def test_registered_factors_match_rust_and_persist_cache(self):
        declarations = {
            "momentum": {"n": 2},
            "volatility": {"n": 2},
            "trend_filter": {"n": 2},
            "rotation_score": {
                "short": 2, "long": 3, "vol": 2,
                "w_s": 1.0, "w_l": 1.0, "w_v": 1.0, "trend": None,
            },
        }
        for kind, params in declarations.items():
            with self.subTest(kind=kind):
                factor = prajna.Factor(kind, params)
                args = (str(self.lake), self.fixture["dsv"],
                        self.fixture["universe"], "SYNTH", factor)
                first = prajna.read_factor(*args)
                second = prajna.read_factor(*args)
                self.assertIsInstance(first, pa.Table)
                expected = pq.read_table(self.lake / f"{kind}.parquet")
                self.assertEqual(first.to_pydict(), expected.to_pydict())
                self.assertEqual(second.to_pydict(), first.to_pydict())
                key = self.fixture["factor_keys"][kind]
                self.assertEqual(first.schema.metadata[b"prajna.fv"].decode(), key)
                self.assertEqual(second.schema.metadata[b"prajna.cache_hit"], b"true")
                self.assertEqual(first.schema.metadata[b"prajna.cache_hit"], b"false")
                self.assertIn("available_at", first.column_names)

    def test_run_tables_and_level_errors_match_cli_storage(self):
        for engine, level in (("vector", "summary"), ("vector", "standard"),
                              ("vector", "full"), ("fast_event", "full")):
            with self.subTest(engine=engine, level=level), tempfile.TemporaryDirectory() as directory:
                lake = Path(directory) / "lake"
                shutil.copytree(self.lake, lake)
                definition_path = lake / "experiment.json"
                definition = json.loads(definition_path.read_text())
                definition["engine"] = engine
                if engine == "fast_event":
                    definition["fast_event"] = {
                        "currency": "CNY", "initial_cash": "100000",
                        "sizing": "lot", "rebalance": "full_target",
                        "minimum_commission": "5",
                        "portfolios": [{"id": 1, "capital": "100000"}],
                        "lot_size_source": {"kind": "instrument"},
                    }
                definition_path.write_text(json.dumps(definition))
                factor = prajna.Factor("rotation_score", {
                    "short": 2, "long": 3, "vol": 2, "w_s": 1.0,
                    "w_l": 1.0, "w_v": 1.0, "trend": None,
                })
                prajna.read_factor(str(lake), self.fixture["dsv"],
                                   self.fixture["universe"], "SYNTH", factor)
                repo = Path(directory) / "repo"
                repo.mkdir()
                git_env = {key: value for key, value in os.environ.items()
                           if key not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE",
                                          "GIT_COMMON_DIR")}
                for args in (["init", "-q"], ["-c", "user.name=Test",
                             "-c", "user.email=test@example.invalid",
                             "-c", "commit.gpgsign=false", "commit", "--allow-empty",
                             "-qm", "fixture"]):
                    subprocess.run(["git", *args], cwd=repo, env=git_env, check=True)
                output = subprocess.run(
                    [str(ROOT / "target/debug/prajna-experiment"), "run",
                     str(lake / "experiment.json"), "--lake", str(lake),
                     "--level", level, "--threads", "1"],
                    cwd=repo, env=git_env, check=True, stdout=subprocess.PIPE, text=True,
                )
                identities = json.loads(output.stdout)
                self.assertEqual(identities["failed_count"], 0)
                self.assertEqual(identities["cache"]["compute_count"], 0)
                execution = (lake / "experiments" / identities["exp"].split(":")[-1]
                             / "executions" / identities["exe"].split(":")[-1])
                manifest = json.loads((execution / "execution.json").read_text())
                run = manifest["runs"][0]
                args = (str(lake), identities["exp"], identities["exe"], run["run_id"])
                summary = prajna.read_run(*args, table="summary", result_level=level)
                expected_summary = pq.read_table(execution / "summary.parquet")
                expected_summary = expected_summary.filter(
                    pc.equal(expected_summary["run_id"], run["run_id"])
                )
                self.assertTrue(summary.equals(expected_summary))
                for name in run["table_hashes"]:
                    actual = prajna.read_run(*args, table=name, result_level=level)
                    expected = pq.read_table(
                        execution / "runs" / run["run_id"].split(":")[-1] / f"{name}.parquet"
                    )
                    self.assertTrue(actual.equals(expected))
                unavailable = "orders" if engine == "vector" else "target_weights"
                with self.assertRaises(prajna.EngineNotProducedError):
                    prajna.read_run(*args, table=unavailable, result_level=level)
                if level != "full":
                    with self.assertRaises(prajna.ResultNotSavedError):
                        prajna.read_run(*args, table="summary", result_level="full")
                    with self.assertRaises(prajna.ResultNotSavedError):
                        prajna.read_run(*args, table="decisions", result_level=level)
                with self.assertRaises(ValueError):
                    prajna.read_run(*args, table="../bad", result_level=level)
                with self.assertRaises(ValueError):
                    prajna.read_run(*args, table="summary", result_level="invalid")
                missing_run = (*args[:3], "run:sha256:" + "0" * 64)
                with self.assertRaises(ValueError):
                    prajna.read_run(*missing_run, table="summary")
                summary_path = execution / "summary.parquet"
                corrupted = pq.read_table(summary_path)
                corrupted = corrupted.set_column(
                    corrupted.schema.get_field_index("status"),
                    corrupted.schema.field("status"),
                    pa.array(["failed"] * corrupted.num_rows),
                )
                pq.write_table(corrupted, summary_path, compression="zstd")
                with self.assertRaisesRegex(ValueError, "hash mismatch"):
                    prajna.read_run(*args, table="summary")


if __name__ == "__main__":
    unittest.main()
