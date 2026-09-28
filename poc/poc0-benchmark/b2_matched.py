#!/usr/bin/env python3
"""S2 matched-boundary B2 remeasurement coordinator for the fixed 3x10 golden."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DATASET = ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
EXPECTED = ROOT / "poc/poc0-benchmark/fixtures/expected-v1.json"
ADR_EXCLUDED = "orders.nautilus_native_lifecycle"


def decide(
    *,
    correctness: str,
    protocol: str,
    rust_serial_median_ns: float | None,
    nautilus_serial_median_ns: float | None,
    rust_parallel_runs_per_second: float | None,
    nautilus_parallel_runs_per_second: float | None,
    rust_peak_rss_bytes: int | None,
    nautilus_peak_rss_sum_upper_bound_bytes: int | None,
    correctness_checks: dict[str, bool] | None = None,
) -> dict[str, str]:
    """Apply ticket 09 gates without letting ADR 0012's excluded field fail parity."""
    failures = [
        field for field, passed in (correctness_checks or {}).items()
        if not passed and field != ADR_EXCLUDED
    ]
    if failures:
        return {"status": "reject", "reason": "reproducible correctness failure: " + ", ".join(failures)}
    if correctness != "passed":
        return {"status": "unresolved", "reason": "common-subset correctness gate did not pass"}
    if protocol not in {"matched", "matched_fallback"}:
        return {"status": "unresolved", "reason": "measurement boundaries or Nautilus reuse mode are not matched"}
    metrics = (
        rust_serial_median_ns,
        nautilus_serial_median_ns,
        rust_parallel_runs_per_second,
        nautilus_parallel_runs_per_second,
        rust_peak_rss_bytes,
        nautilus_peak_rss_sum_upper_bound_bytes,
    )
    if any(value is None for value in metrics):
        return {"status": "unresolved", "reason": "required timing or RSS measurement is missing"}
    assert rust_serial_median_ns is not None and nautilus_serial_median_ns is not None
    assert rust_parallel_runs_per_second is not None and nautilus_parallel_runs_per_second is not None
    assert rust_peak_rss_bytes is not None and nautilus_peak_rss_sum_upper_bound_bytes is not None
    passes = (
        rust_serial_median_ns * 2 <= nautilus_serial_median_ns
        and rust_parallel_runs_per_second >= 2 * nautilus_parallel_runs_per_second
        and rust_peak_rss_bytes <= nautilus_peak_rss_sum_upper_bound_bytes
    )
    return {
        "status": "adopt" if passes else "defer",
        "reason": "all registered speed and RSS gates passed" if passes else "one or more registered speed or RSS gates missed",
    }


def _run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/quant-research")
    parser.add_argument("--python", type=Path, default=ROOT / ".venv/bin/python")
    parser.add_argument("--output", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-matched-s2.json")
    parser.add_argument("--build-record", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-matched-s2-release-build-final-2026-09-28.json")
    parser.add_argument("--run-measurements", action="store_true", help="collect timing after independent golden preflight")
    args = parser.parse_args()
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    output = args.output if args.output.is_absolute() else ROOT / args.output

    preflight_path = output.with_suffix(".preflight.json")
    try:
        preflight = _run([str(binary), "benchmark-poc0", "--candidate", "soa", "--dataset", str(DATASET), "--expected", str(EXPECTED), "--output", str(preflight_path)])
        preflight_error = None
    except OSError as error:
        preflight = subprocess.CompletedProcess([], 127, "", "")
        preflight_error = f"{type(error).__name__}: {error}"
    report: dict[str, Any] = {
        "schema_version": "poc0.b2-matched-s2.v1",
        "status": "unresolved",
        "strategy": "S2 Momentum Rotation",
        "dataset_version": "poc0.synthetic.etf-daily.v1",
        "input_sha256": hashlib.sha256(DATASET.read_bytes()).hexdigest(),
        "correctness": {"status": "failed", "preflight_process_exit": preflight.returncode},
        "measurements": {"status": "not_collected"},
        "decision": {"status": "unresolved", "reason": "golden preflight failed"},
        "adr_0012_exclusions": [ADR_EXCLUDED],
        "environment": {
            "python": sys.version,
            "platform": platform.platform(),
            "cpu": platform.processor(),
            "logical_cpu_count": os.cpu_count(),
            "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest() if binary.exists() else None,
            "git_revision": _run(["git", "rev-parse", "HEAD"]).stdout.strip(),
            "dirty_paths": [line[3:] for line in _run(["git", "status", "--porcelain"]).stdout.splitlines()],
            "build_record": str(args.build_record),
            "build_record_sha256": hashlib.sha256((args.build_record if args.build_record.is_absolute() else ROOT / args.build_record).read_bytes()).hexdigest() if (args.build_record if args.build_record.is_absolute() else ROOT / args.build_record).exists() else None,
        },
    }
    if preflight.returncode != 0:
        report["correctness"]["stderr"] = preflight_error or preflight.stderr[-4000:]
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return preflight.returncode
    golden = json.loads(preflight_path.read_text(encoding="utf-8"))
    golden_s2 = golden["b2_fast_event_momentum_rotation"]
    expected_checksum = golden_s2["checksum_sha256"]
    report["correctness"] = {"status": "passed", "preflight_process": str(preflight_path), "rust_s2_checksum_sha256": expected_checksum}

    try:
        from nautilus_adapter import _compare, _projection_checksum, _run_nautilus

        dataset = json.loads(DATASET.read_text(encoding="utf-8"))
        from nautilus_adapter import _version_probe
        report["environment"]["nautilus_version_probe"] = _version_probe()
        nautilus_projection = _run_nautilus(dataset, strategy="s2")["projection"]
        nautilus_golden_checksum = _projection_checksum(nautilus_projection)
        checks = _compare(nautilus_projection, golden_s2["projection"])
        non_excluded_failures = [
            item["field"] for item in checks
            if not item["passed"] and item["field"] != ADR_EXCLUDED
        ]
        report["correctness"].update({
            "nautilus_checks": checks,
            "status": "passed" if not non_excluded_failures else "failed",
            "nautilus_checked_projection_checksum_sha256": nautilus_golden_checksum,
            "non_excluded_failures": non_excluded_failures,
            "adr_0012_excluded_failure": any(not item["passed"] and item["field"] == ADR_EXCLUDED for item in checks),
        })
        if non_excluded_failures or not args.run_measurements:
            report["decision"] = decide(correctness=report["correctness"]["status"], protocol="unmatched", rust_serial_median_ns=None, nautilus_serial_median_ns=None, rust_parallel_runs_per_second=None, nautilus_parallel_runs_per_second=None, rust_peak_rss_bytes=None, nautilus_peak_rss_sum_upper_bound_bytes=None, correctness_checks={item["field"]: item["passed"] for item in checks})
        else:
            rss_wrapper = ROOT / "poc/poc0-benchmark/measure-b2-rss.py"
            nautilus_wrapper = ROOT / "poc/poc0-benchmark/measure-b2-nautilus.py"
            python = args.python if args.python.is_absolute() else ROOT / args.python
            def collect(label: str, backend: str, mode: str, group: int = 0) -> dict[str, Any]:
                measurement_output = output.with_name(f"{output.stem}.{label}.{mode}.{group}.json")
                if backend == "rust":
                    command = [str(python), str(rss_wrapper), "--binary", str(binary), "--strategy", "s2", "--mode", mode, "--workers", "1" if mode == "serial" else "2", "--runs", "20" if mode == "serial" else "6", "--expected-checksum", expected_checksum, "--output", str(measurement_output)]
                else:
                    command = [str(python), str(nautilus_wrapper), "--mode", mode, "--workers", "1" if mode == "serial" else "2", "--runs", "20" if mode == "serial" else "6", "--expected-checksum", nautilus_golden_checksum, "--output", str(measurement_output)]
                result = _run(command)
                if result.returncode:
                    raise RuntimeError(f"{backend} {mode} measurement failed: {result.stderr[-4000:]}")
                measurement = json.loads(measurement_output.read_text(encoding="utf-8"))
                measurement["artifact_path"] = measurement_output.relative_to(ROOT).as_posix()
                return measurement

            rust_serial = collect("rust", "rust", "serial")
            nautilus_serial = collect("nautilus", "nautilus", "serial")
            parallel_groups = []
            for group in range(5):
                order = ("rust", "nautilus") if group % 2 == 0 else ("nautilus", "rust")
                group_runs = {}
                for backend in order:
                    group_runs[backend] = collect(backend, backend, "parallel", group + 1)
                parallel_groups.append({"order": list(order), "runs": group_runs})
            rust_rates = [group["runs"]["rust"]["parallel_runs_per_second"] for group in parallel_groups]
            nautilus_rates = [group["runs"]["nautilus"]["parallel_runs_per_second"] for group in parallel_groups]
            rust_peak = max(group["runs"]["rust"]["peak_rss_bytes"] for group in parallel_groups)
            nautilus_peak = max(group["runs"]["nautilus"]["peak_rss_sum_upper_bound_bytes"] for group in parallel_groups)
            protocol = (
                "matched_fallback"
                if nautilus_serial.get("engine_mode") == "cached_conversion_new_engine"
                and all(group["runs"]["nautilus"].get("engine_mode") == "cached_conversion_new_engine" for group in parallel_groups)
                else "unmatched"
            )
            report["measurements"] = {
                "rust_serial": rust_serial,
                "nautilus_serial": nautilus_serial,
                "parallel_groups": parallel_groups,
                "rust_parallel_runs_per_second_median": statistics.median(rust_rates),
                "nautilus_parallel_runs_per_second_median": statistics.median(nautilus_rates),
                "rust_process_peak_rss_bytes": rust_peak,
                "nautilus_peak_rss_sum_upper_bound_bytes": nautilus_peak,
                "nautilus_engine_mode": nautilus_serial.get("engine_mode"),
                "nautilus_engine_mode_reason": nautilus_serial.get("engine_mode_reason"),
                "protocol_status": protocol,
                "measurement_groups": 5,
                "parallel_order_alternated": True,
            }
            report["decision"] = decide(
                correctness=report["correctness"]["status"], protocol=protocol,
                rust_serial_median_ns=rust_serial.get("median_ns"),
                nautilus_serial_median_ns=nautilus_serial.get("median_ns"),
                rust_parallel_runs_per_second=statistics.median(rust_rates),
                nautilus_parallel_runs_per_second=statistics.median(nautilus_rates),
                rust_peak_rss_bytes=rust_peak,
                nautilus_peak_rss_sum_upper_bound_bytes=nautilus_peak,
            )
            report["decision"]["scope"] = "S2 3x10 matched fallback only; not a full B2 selection"
    except Exception as error:
        report["execution_error"] = f"{type(error).__name__}: {error}"
        report["decision"] = {"status": "unresolved", "reason": "Nautilus runtime or measurement failed"}

    report["status"] = report["decision"]["status"]
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"status": report["status"], "output": str(output)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
