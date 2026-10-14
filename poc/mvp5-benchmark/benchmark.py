#!/usr/bin/env python3
"""Record-only MVP-5 synthetic throughput through the real Experiment API."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json"


def write(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def matrix():
    return [(strategy, implementation, threads)
            for strategy in ("s2", "s3")
            for implementation in ("rust", "python")
            for threads in (1, 2)]


def series(values):
    if not values or any(not math.isfinite(value) or value <= 0 for value in values):
        raise ValueError("positive finite measurements required")
    return {
        "median": statistics.median(values), "min": min(values), "max": max(values),
        "p95_nearest_rank": sorted(values)[math.ceil(len(values) * .95) - 1],
    }


def summarize(record):
    if (record["status"] != "measured" or record["correctness"]["returncode"] != 0
            or "skipped" in record["correctness"].get("stderr", "")):
        raise ValueError("successful correctness gate and complete measurement required")
    repeats = record["repeats"]
    if repeats < 3 or len(record["measurements"]) != 8 * repeats:
        raise ValueError("complete matrix requires at least three repeats per cell")
    cells = []
    hashes = {}
    for strategy, implementation, threads in matrix():
        samples = [sample for sample in record["measurements"]
                   if (sample["strategy"], sample["implementation"], sample["threads"])
                   == (strategy, implementation, threads)]
        if sorted(sample["repeat"] for sample in samples) != list(range(1, repeats + 1)):
            raise ValueError("missing or duplicate repeat in matrix cell")
        raw = {name: [] for name in ("wall_seconds", "runs_per_second", "peak_rss_bytes",
                                    "run_time_ms")}
        views = []
        if implementation == "python":
            raw.update(stage1_ms=[], stage2_ms=[])
        for sample in samples:
            count = 2 if strategy == "s2" else 1
            if (sample["run_count"] != count or sample["action"] != "created"
                    or sample["statuses"] != ["completed"] * count):
                raise ValueError("expected newly created, successful canonical Runs")
            key = (strategy, implementation)
            digest = sample["summary_logical_hash"]
            if not digest or hashes.setdefault(key, digest) != digest:
                raise ValueError("Summary logical hash changed across repeats or threads")
            wall = sample["wall_seconds"]
            series([wall, sample["peak_rss_bytes"]])
            stats = sample["statistics"]
            run_ms = stats["run_time_ms"]
            if not math.isfinite(run_ms) or not 0 <= run_ms <= wall * 1000:
                raise ValueError("invalid Rayon Run timing")
            for name, value in (
                ("wall_seconds", wall), ("runs_per_second", count / wall),
                ("peak_rss_bytes", sample["peak_rss_bytes"]), ("run_time_ms", run_ms),
            ):
                raw[name].append(value)
            if implementation == "python":
                timings = stats["python"]
                current_views = timings["asof_view_ms"]
                if len(current_views) != (86 if strategy == "s2" else 192):
                    raise ValueError("As-of timing must cover every scheduled callback")
                series(current_views)
                first, second = timings["stage1_ms"], timings["stage2_ms"]
                series([first, second])
                if (first < sum(current_views) or second < run_ms
                        or first + second > wall * 1000):
                    raise ValueError("stage timers must enclose views/Runs within wall time")
                raw["stage1_ms"].append(first)
                raw["stage2_ms"].append(second)
                views.extend(current_views)
        cells.append({
            "strategy": strategy, "implementation": implementation, "threads": threads,
            "run_count": count, "sample_count": len(samples), "raw": raw,
            "median": {name: statistics.median(values) for name, values in raw.items()},
            "asof_view_ms": {"count": len(views), **series(views)} if views else None,
        })
    return {"conclusion": "record_only", "cells": cells}


def command(command, record):
    started = time.perf_counter()
    result = subprocess.run([str(arg) for arg in command], cwd=ROOT,
                            text=True, capture_output=True)
    log = {"command": [str(arg) for arg in command], "returncode": result.returncode,
           "stdout": result.stdout, "stderr": result.stderr,
           "wall_seconds": time.perf_counter() - started}
    record["commands"].append(log)
    return log


def checked(command_args, record):
    log = command(command_args, record)
    if log["returncode"]:
        raise RuntimeError(f"command failed: {log}")
    return log["stdout"].strip()


def worker(args):
    import prajna
    sys.path.insert(0, str(ROOT / "python/tests"))
    from acceptance_strategies import s2_declaration, s2_decide, s3_declaration, s3_decide

    load_before = os.getloadavg()
    fixture = json.loads(args.fixture.read_text())
    definition = fixture["definition"]
    if args.strategy == "s2":
        params = [case["params"] for case in fixture["s2"]]
        declaration, decide = s2_declaration(params[0]), s2_decide
        builtin = prajna.RustStrategy.S2_ROTATION
    else:
        params = [{"params_version": 1, "short_window": 20, "long_window": 60}]
        declaration, decide = s3_declaration(), s3_decide
        builtin = prajna.RustStrategy.MA_CROSSOVER
    experiment = prajna.Experiment(
        dataset_version=prajna.DatasetVersion(definition["dsv"]),
        universe=prajna.Universe(definition["universe"]), venue=prajna.Venue("SYNTH"),
        strategy=(prajna.PythonStrategy(declaration.strategy_id, declaration.version)
                  if args.implementation == "python" else builtin),
        engine=prajna.Engine(prajna.EngineKind.VECTOR),
        costs=prajna.Costs(**definition["costs"]), parameter_space=prajna.List(params),
    )
    kwargs = {"strategy": declaration, "decide": decide} if args.implementation == "python" else {}
    with tempfile.TemporaryDirectory(prefix="prajna-mvp5-worker-") as directory:
        for name in ("warmup", "measured"):
            lake = Path(directory) / name
            shutil.copytree(args.lake, lake)
            started = time.perf_counter()
            result = experiment.run(lake, repository=ROOT, threads=args.threads, **kwargs)
            wall = time.perf_counter() - started
            runs = result.manifest["runs"]
            if result.action != "created" or any(run["status"] != "completed" for run in runs):
                raise RuntimeError(f"benchmark Run failed or replayed: {result.manifest}")
        peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        if sys.platform != "darwin":
            peak *= 1024
        return {
            "strategy": args.strategy, "implementation": args.implementation,
            "threads": args.threads, "run_count": len(runs), "action": result.action,
            "wall_seconds": wall, "peak_rss_bytes": peak,
            "statuses": [run["status"] for run in runs], "statistics": result.statistics,
            "summary_logical_hash": result.manifest["summary_logical_hash"],
            "execution": result.manifest["execution"], "definition": experiment.to_definition(),
            "load_average_before": load_before, "load_average_after": os.getloadavg(),
        }


def measure(args):
    if args.repeats < 3:
        raise ValueError("at least three repeats required")
    record = {
        "status": "measuring", "repeats": args.repeats, "commands": [], "measurements": [],
        "started_at": datetime.now(timezone.utc).isoformat(),
    }
    write(args.output, record)
    correctness = command(
        [sys.executable, "-m", "unittest", "discover", "-s", "python/tests",
         "-p", "test_acceptance.py", "-v"], record,
    )
    record["correctness"] = correctness
    write(args.output, record)
    if correctness["returncode"] or "skipped" in correctness["stderr"]:
        raise RuntimeError("correctness acceptance must pass without skips before measurement")
    paths = [
        "Cargo.lock", "poc/mvp5-benchmark/benchmark.py",
        "python/tests/acceptance_strategies.py", "python/tests/test_acceptance.py",
        "crates/prajna-py/src/strategy.rs", "crates/prajna-py/src/execution.rs",
    ]
    import prajna._native as native
    record["environment"] = {
        "python": sys.version, "python_executable": sys.executable,
        "platform": platform.platform(), "machine": platform.machine(),
        "cpu_count": os.cpu_count(), "extension_sha256": sha(Path(native.__file__)),
        "fixture_sha256": sha(FIXTURE), "fixture_binary_sha256": sha(args.fixture_binary),
        "source_sha256": {path: sha(ROOT / path) for path in paths},
        "git_revision": checked(["git", "rev-parse", "HEAD"], record),
        "git_status": checked(["git", "status", "--short"], record),
        "git_diff_sha256": hashlib.sha256(
            checked(["git", "diff", "--binary", "HEAD"], record).encode()).hexdigest(),
        "rustc": checked(["rustc", "-Vv"], record),
        "cargo": checked(["cargo", "-V"], record),
        "dependency_snapshot": checked([sys.executable, "-m", "pip", "freeze", "--all"], record),
        "env": {key: os.environ.get(key) for key in (
            "PYO3_PYTHON", "DYLD_LIBRARY_PATH", "RAYON_NUM_THREADS", "POLARS_MAX_THREADS",
        )},
    }
    if sys.platform == "darwin":
        record["environment"]["hardware"] = checked(
            ["sysctl", "hw.model", "hw.memsize", "hw.physicalcpu", "hw.logicalcpu"], record,
        )
    with tempfile.TemporaryDirectory(prefix="prajna-mvp5-") as directory:
        base = Path(directory)
        lake, fixture_path = base / "lake", base / "fixture.json"
        fixture_text = checked([args.fixture_binary, lake, "64x252"], record)
        fixture = json.loads(fixture_text)
        if (len(fixture["members"]), fixture["session_count"]) != (64, 252):
            raise ValueError("expected registered 64x252 fixture")
        fixture_path.write_text(fixture_text)
        record["fixture"] = fixture
        for repeat in range(1, args.repeats + 1):
            # Alternate ordering to reduce systematic host-drift advantage.
            cells = matrix() if repeat % 2 else list(reversed(matrix()))
            for strategy, implementation, threads in cells:
                log = command([
                    sys.executable, Path(__file__).resolve(), "worker",
                    "--fixture", fixture_path, "--lake", lake, "--strategy", strategy,
                    "--implementation", implementation, "--threads", threads,
                ], record)
                write(args.output, record)
                if log["returncode"]:
                    raise RuntimeError(f"measurement child failed: {log}")
                sample = json.loads(log["stdout"])
                sample["repeat"] = repeat
                record["measurements"].append(sample)
                write(args.output, record)
    record["status"] = "measured"
    summarize(record)
    record["finished_at"] = datetime.now(timezone.utc).isoformat()
    write(args.output, record)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("measure")
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--repeats", type=int, default=5)
    run.add_argument("--fixture-binary", type=Path,
                     default=ROOT / "target/release/examples/acceptance_fixture")
    report = commands.add_parser("summarize")
    report.add_argument("source", type=Path)
    child = commands.add_parser("worker")
    child.add_argument("--fixture", type=Path, required=True)
    child.add_argument("--lake", type=Path, required=True)
    child.add_argument("--strategy", choices=("s2", "s3"), required=True)
    child.add_argument("--implementation", choices=("rust", "python"), required=True)
    child.add_argument("--threads", type=int, choices=(1, 2), required=True)
    args = parser.parse_args()
    if args.command == "summarize":
        print(json.dumps(summarize(json.loads(args.source.read_text())), indent=2, allow_nan=False))
    elif args.command == "worker":
        print(json.dumps(worker(args), allow_nan=False))
    else:
        measure(args)


if __name__ == "__main__":
    main()
