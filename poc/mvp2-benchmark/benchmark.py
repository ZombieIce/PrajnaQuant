#!/usr/bin/env python3
"""Measure the synthetic MVP-2 matrix; no performance pass threshold."""

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json"
DEFINITION = ROOT / "poc/mvp2-benchmark/experiment.json"


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def command_text(command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def file_totals(directory):
    files = [path for path in directory.rglob("*") if path.is_file()]
    return {"bytes": sum(path.stat().st_size for path in files), "files": len(files)}


def workload(base):
    fixture = json.loads(FIXTURE.read_text())
    base["parameter_space"] = {"grid": {
        "short": [5, 10, 20], "long": [40, 60], "vol": [10, 20],
        "w_s": [fixture["strategy"]["short_momentum_weight"]],
        "w_l": [fixture["strategy"]["long_momentum_weight"]],
        "w_v": [fixture["strategy"]["volatility_weight"]],
        "trend": [None, 20], "top_k": [1, 3, 5],
        "rebalance_every": [1, 5, 10, 20],
    }}
    return base


def summarize(record):
    repeats = record["repeats"]
    if repeats < 3 or len(record["measurements"]) != 16 * repeats:
        raise ValueError("matrix must have every cell with at least 3 repeats")
    if len({sample["summary_logical_hash"] for sample in record["measurements"]}) != 1:
        raise ValueError("Summary logical hash differs across measurements")
    cells = []
    for threads in (1, 2, 4, 8):
        for cache in ("cold", "hot"):
            for level in ("summary", "full"):
                samples = [
                    sample for sample in record["measurements"]
                    if (sample["threads"], sample["cache_state"], sample["result_level"])
                    == (threads, cache, level)
                ]
                if sorted(sample["repeat"] for sample in samples) != list(range(1, repeats + 1)):
                    raise ValueError("missing or duplicate repeat in matrix cell")
                raw = {name: [] for name in (
                    "wall_seconds", "total_ms", "factors_ms", "runs_ms",
                    "runs_per_second", "peak_rss_bytes", "compute_count",
                    "hit_count", "written_bytes", "written_files",
                )}
                for sample in samples:
                    cli = sample["cli"]
                    if (cli["run_count"] != 288 or cli["failed_count"] != 0
                            or cli["action"] != "created" or cli["threads"] != threads):
                        raise ValueError("expected a newly created, successful 288-Run execution")
                    if level == "summary" and sample["summary_no_runs"] is not True:
                        raise ValueError("summary execution unexpectedly contains runs/")
                    if (cache == "hot" and (cli["cache"]["compute_count"] != 0
                                           or cli["cache"]["hit_count"] <= 0)):
                        raise ValueError("hot factors must be cache hits with zero computes")
                    if cache == "cold" and cli["cache"]["compute_count"] <= 0:
                        raise ValueError("cold factors must be computed")
                    if not math.isfinite(sample["wall_seconds"]) or sample["wall_seconds"] <= 0:
                        raise ValueError("wall time must be positive and finite")
                    rss = re.search(
                        r"^\s*(\d+)\s+maximum resident set size\s*$",
                        sample["time_output"], re.MULTILINE,
                    )
                    if rss is None or int(rss.group(1)) <= 0:
                        raise ValueError("missing or invalid macOS time peak RSS")
                    values = {
                        "wall_seconds": sample["wall_seconds"],
                        "total_ms": cli["timings_ms"]["total"],
                        "factors_ms": cli["timings_ms"]["factors"],
                        "runs_ms": cli["timings_ms"]["runs"],
                        "runs_per_second": cli["run_count"] / sample["wall_seconds"],
                        "peak_rss_bytes": int(rss.group(1)),
                        "compute_count": cli["cache"]["compute_count"],
                        "hit_count": cli["cache"]["hit_count"],
                        "written_bytes": cli["written_bytes"],
                        "written_files": cli["written_files"],
                    }
                    for name, value in values.items():
                        raw[name].append(value)
                cells.append({
                    "threads": threads, "cache_state": cache, "result_level": level,
                    "sample_count": len(samples), "raw": raw,
                    "median": {name: statistics.median(values) for name, values in raw.items()},
                    "min": {name: min(values) for name, values in raw.items()},
                    "max": {name: max(values) for name, values in raw.items()},
                    "p95_nearest_rank": {
                        name: sorted(values)[math.ceil(0.95 * len(values)) - 1]
                        for name, values in raw.items()
                    },
                })
    return {
        "cells": cells,
        "all_reproducible": all(sample["cli"]["reproducible"] for sample in record["measurements"]),
        "summary_logical_hash": record["measurements"][0]["summary_logical_hash"],
    }


def measure(args):
    if platform.system() != "Darwin":
        raise ValueError("measurement requires macOS /usr/bin/time -l (RSS is bytes)")
    if args.repeats < 3:
        raise ValueError("at least 3 repeats per cell are required")
    output = args.output.resolve()
    if output == ROOT or ROOT in output.parents:
        raise ValueError("measurement output must be outside the worktree to keep provenance clean")
    if output.exists():
        raise ValueError("output directory already exists; use a new directory")
    status = command_text(["git", "status", "--porcelain=v1", "--untracked-files=all"])
    if status:
        raise ValueError("commit all benchmark changes before measuring: worktree must be clean")
    target = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))).resolve()
    binary = target / "release/prajna-experiment"
    prepare = target / "release/examples/prepare_fixture"
    for executable in (binary, prepare):
        if not executable.is_file():
            raise ValueError("missing release executable: {}".format(executable))
    output.mkdir(parents=True)
    record = {
        "schema_version": "poc-mvp2-benchmark.v1",
        "status": "incomplete", "started_at_utc": utc_now(), "repeats": args.repeats,
        "scope": "synthetic 64 instruments x 252 sessions, 288 Vector Runs only",
        "provenance": {
            "git_revision": command_text(["git", "rev-parse", "HEAD"]),
            "git_status_porcelain": status,
            "reproducible": True,
            "rustc_version": command_text(["rustc", "--version"]),
            "rustc_verbose": command_text(["rustc", "-vV"]),
            "python_version": platform.python_version(),
            "operating_system": platform.platform(),
            "architecture": platform.machine(),
            "cpu_model": command_text(["sysctl", "-n", "machdep.cpu.brand_string"]),
            "logical_cpu_count": int(command_text(["sysctl", "-n", "hw.logicalcpu"])),
            "physical_cpu_count": int(command_text(["sysctl", "-n", "hw.physicalcpu"])),
            "memory_bytes": int(command_text(["sysctl", "-n", "hw.memsize"])),
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "prepare_binary_sha256": hashlib.sha256(prepare.read_bytes()).hexdigest(),
            "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
            "fixture_sha256": hashlib.sha256(FIXTURE.read_bytes()).hexdigest(),
            "build_command": "cargo build --release -p prajna-experiment --locked",
            "build_profile": "release; default features",
            "environment": {name: os.environ.get(name) for name in (
                "RAYON_NUM_THREADS", "POLARS_MAX_THREADS", "RUSTFLAGS", "CARGO_BUILD_TARGET",
            )},
            "other_machine_load": "not controlled; serial samples, no concurrent build started by runner",
        },
        "rss_scope": "whole CLI process, macOS /usr/bin/time -l maximum resident set size in bytes",
        "throughput_scope": "288 / wall_seconds, including process startup and result persistence",
        "cache_scope": "Factor Cache only; OS page cache is not flushed",
        "measurements": [],
    }
    raw_path = output / "raw-measurements.json"
    write_json(raw_path, record)
    with tempfile.TemporaryDirectory(prefix="prajna-mvp2-benchmark-") as directory:
        temporary = Path(directory)
        seed = temporary / "seed"
        base_path = temporary / "base.json"
        subprocess.run([str(prepare), str(seed), str(base_path)], cwd=ROOT,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        definition = workload(json.loads(base_path.read_text()))
        if definition != json.loads(DEFINITION.read_text()):
            raise ValueError("published fixture definition differs from checked-in experiment.json")
        definition_path = temporary / "experiment.json"
        write_json(definition_path, definition)
        write_json(output / "experiment.json", definition)
        record["experiment"] = definition
        warm = temporary / "warm"
        shutil.copytree(seed, warm)
        warm_command = [str(binary), "run", str(definition_path), "--lake", str(warm),
                        "--level", "summary", "--threads", "1"]
        warm_result = subprocess.run(warm_command, cwd=ROOT, capture_output=True, text=True, check=True)
        record["warmup"] = {
            "command": warm_command, "stdout": warm_result.stdout, "stderr": warm_result.stderr,
        }
        write_json(raw_path, record)
        for threads in (1, 2, 4, 8):
            for cache in ("cold", "hot"):
                for level in ("summary", "full"):
                    for repeat in range(1, args.repeats + 1):
                        name = "{}-{}-{}-{}".format(threads, cache, level, repeat)
                        lake = temporary / name
                        shutil.copytree(seed, lake)
                        if cache == "hot":
                            shutil.copytree(warm / "features", lake / "features")
                        if (lake / "experiments").exists() or (cache == "cold" and (lake / "features").exists()):
                            raise ValueError("measurement lake is not initially data/Universe only")
                        before = file_totals(lake)
                        timing_path = temporary / (name + ".time.txt")
                        command = [
                            "/usr/bin/time", "-l", "-o", str(timing_path), str(binary),
                            "run", str(definition_path), "--lake", str(lake),
                            "--level", level, "--threads", str(threads),
                        ]
                        started_at = utc_now()
                        started = time.perf_counter()
                        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
                        wall_seconds = time.perf_counter() - started
                        sample = {
                            "threads": threads, "cache_state": cache, "result_level": level,
                            "repeat": repeat, "started_at_utc": started_at,
                            "finished_at_utc": utc_now(), "command": command,
                            "exit_code": result.returncode, "wall_seconds": wall_seconds,
                            "stdout": result.stdout, "stderr": result.stderr,
                            "time_output": timing_path.read_text(),
                            "lake_before": before, "lake_after": file_totals(lake),
                        }
                        record["measurements"].append(sample)
                        write_json(raw_path, record)
                        if result.returncode != 0:
                            raise ValueError("CLI failed for {}; see {}".format(name, raw_path))
                        cli = json.loads(result.stdout)
                        execution = (
                            lake / "experiments" / cli["exp"].split(":")[-1]
                            / "executions" / cli["exe"].split(":")[-1]
                        )
                        manifest = json.loads((execution / "execution.json").read_text())
                        sample.update({
                            "cli": cli, "execution_provenance": manifest["execution"],
                            "execution_machine": manifest["machine"],
                            "summary_logical_hash": manifest["summary_logical_hash"],
                            "summary_no_runs": not (execution / "runs").exists() if level == "summary" else None,
                        })
                        write_json(raw_path, record)
                        if level == "summary" and not sample["summary_no_runs"]:
                            raise ValueError("summary execution contains runs/: {}".format(name))
                        if not cli["reproducible"] or manifest["execution"]["git_revision"] != record["provenance"]["git_revision"]:
                            raise ValueError("provenance changed during measurement")
                        shutil.rmtree(lake)
        report = summarize(record)
        record["status"] = "measured"
        record["finished_at_utc"] = utc_now()
        write_json(raw_path, record)
        write_json(output / "medians.json", report)
    print(json.dumps({"raw": str(raw_path), "summary": str(output / "medians.json"),
                      "samples": len(record["measurements"])}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    report = commands.add_parser("summarize")
    report.add_argument("raw", type=Path)
    run = commands.add_parser("run")
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    if args.command == "run":
        measure(args)
    else:
        print(json.dumps(summarize(json.loads(args.raw.read_text())), indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print("benchmark: {}".format(error), file=sys.stderr)
        sys.exit(1)
