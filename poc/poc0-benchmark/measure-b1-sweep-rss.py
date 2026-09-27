#!/usr/bin/env python3
"""Run a built B1 sweep and record whole-process peak RSS."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import resource
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def decide(report: dict) -> dict:
    threshold = report["decision_threshold"]["minimum_throughput_gain_pct"]
    target = report["target_load"]
    if report.get("status") != "passed":
        return {
            "status": "unresolved",
            "custom_soa": "unresolved",
            "reason": "Correctness gate failed; performance is not decision evidence.",
        }
    if target["instrument_count"] < 64 or target["session_count"] < 252:
        return {
            "status": "unresolved",
            "custom_soa": "unresolved",
            "reason": "Workload is below the registered 64-instrument by 252-session target.",
        }
    if report.get("process_peak_rss_bytes") is None:
        return {
            "status": "unresolved",
            "custom_soa": "unresolved",
            "reason": "Whole-process peak RSS is missing.",
        }

    gains = {}
    for condition in ("cache_miss", "cache_hit"):
        soa = report["measurements"]["soa"][condition]["parallel_runs_per_second"]
        alternatives = [
            report["measurements"][layout][condition]["parallel_runs_per_second"]
            for layout in ("arrow", "polars")
        ]
        best_alternative = max(alternatives)
        gains[condition] = (soa / best_alternative - 1.0) * 100.0

    if all(gain >= threshold for gain in gains.values()):
        status = "adopt"
        reason = "Custom SoA exceeds the best alternative by the registered threshold in both cache conditions on the target synthetic workload."
    elif all(gain <= -threshold for gain in gains.values()):
        status = "reject"
        reason = "Custom SoA trails the best alternative by at least the registered threshold in both cache conditions on the target synthetic workload."
    else:
        status = "defer"
        reason = "The throughput advantage does not clear the registered threshold consistently across cache conditions."
    return {
        "status": status,
        "custom_soa": status,
        "throughput_gain_vs_best_alternative_pct": gains,
        "threshold_pct": threshold,
        "reason": reason,
        "scope": "64-instrument by 252-session deterministic synthetic weekday workload only",
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/release/quant-research"))
    parser.add_argument("--dataset", type=Path, default=Path("poc/poc0-benchmark/fixtures/dataset-v1.json"))
    parser.add_argument("--expected", type=Path, default=Path("poc/poc0-benchmark/fixtures/expected-v1.json"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--instruments", type=int, default=64)
    parser.add_argument("--sessions", type=int, default=252)
    parser.add_argument("--dev-build-record", type=Path)
    parser.add_argument("--release-build-record", type=Path)
    args = parser.parse_args()

    output = args.output if args.output.is_absolute() else ROOT / args.output
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    dataset = args.dataset if args.dataset.is_absolute() else ROOT / args.dataset
    expected = args.expected if args.expected.is_absolute() else ROOT / args.expected
    output.parent.mkdir(parents=True, exist_ok=True)
    command = [
        str(binary),
        "benchmark-poc0-sweep",
        "--dataset",
        str(dataset),
        "--expected",
        str(expected),
        "--output",
        str(output),
        "--instruments",
        str(args.instruments),
        "--sessions",
        str(args.sessions),
    ]
    started_at = datetime.now(timezone.utc).isoformat()
    started = time.perf_counter_ns()
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    wall_ns = time.perf_counter_ns() - started
    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    peak_bytes = int(peak if platform.system() == "Darwin" else peak * 1024)

    record = {
        "schema_version": "poc0-b1-sweep-rss.v1",
        "started_at_utc": started_at,
        "finished_at_utc": datetime.now(timezone.utc).isoformat(),
        "status": "passed" if result.returncode == 0 else "failed",
        "command": command,
        "exit_code": result.returncode,
        "wall_ns": wall_ns,
        "peak_rss_bytes": peak_bytes,
        "peak_rss_scope": "entire benchmark-poc0-sweep child process; not attributed by layout or cache condition",
        "operating_system": platform.platform(),
        "architecture": platform.machine(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "stdout": result.stdout,
        "stderr": result.stderr,
    }
    rss_path = output.with_suffix(".rss.json")
    rss_path.write_text(json.dumps(record, indent=2) + "\n")
    if result.returncode == 0:
        report = json.loads(output.read_text())
        try:
            rss_record_path = rss_path.relative_to(ROOT)
        except ValueError:
            rss_record_path = rss_path
        report["process_peak_rss_bytes"] = peak_bytes
        report["process_peak_rss_status"] = "measured by getrusage(RUSAGE_CHILDREN)"
        report["resource_measurement"] = {
            "peak_rss_bytes": peak_bytes,
            "scope": record["peak_rss_scope"],
            "raw_record": str(rss_record_path),
        }
        build_records = {}
        for profile, path in (
            ("dev", args.dev_build_record),
            ("release", args.release_build_record),
        ):
            if path is None:
                continue
            resolved = path if path.is_absolute() else ROOT / path
            build = json.loads(resolved.read_text())
            build_records[profile] = {
                "status": build.get("status"),
                "build_wall_seconds": build.get("build_wall_seconds"),
                "target_delta_bytes": build.get("target_delta_bytes"),
                "record": str(resolved.relative_to(ROOT)),
            }
        complete_build_records = all(
            build_records.get(profile, {}).get("status") == "completed"
            for profile in ("dev", "release")
        )
        report["build_cost_evidence"] = {
            "status": "measured" if complete_build_records else "unknown",
            "cache_state": "warm existing shared workspace target",
            "target_directory": "target/",
            "cold_build": "unknown; no target cache was cleared",
            "per_layout_candidate_attribution": "unknown; all three layouts compile into one quant-research binary and share the Cargo target cache",
            "profiles": build_records,
        }
        report["conclusion"] = decide(report)
        output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"status": record["status"], "report": str(output), "rss_record": str(rss_path)}))
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
