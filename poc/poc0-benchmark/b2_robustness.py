#!/usr/bin/env python3
"""Run the registered 64x252 B2 robustness workloads after checksum preflight."""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).parent))
import b2_matched

REGISTERED = {
    "s2": {
        "version": "poc0.b3.s2-scale-64x252.v1",
        "dataset": ROOT / "poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v1.json",
        "checksum": "5fa75bb28180d61d1422772fd746f8abc6aa3e8e71e733a550b04f4d36ab3342",
        "parameters": "Momentum 20/60, volatility 20, Top-5, rebalance every 5 eligible sessions",
    },
    "s3": {
        "version": "poc0.b3.s3-scale-64x252.v1",
        "dataset": ROOT / "poc/poc0-benchmark/fixtures/b2-s3-scale-64x252-v1.json",
        "checksum": "0fade879d4715940f343a25de71e96279b2477396feb206d457a17571abffd70",
        "parameters": "MA20/60, fixed rising-symbol fixture",
    },
}


def combine_robustness(
    decision_load_decision: dict[str, Any],
    robustness_status: str,
    robustness_decision: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Apply the preregistered robustness mapping without treating missing data as pass."""
    result = dict(decision_load_decision)
    if robustness_status in {"unverified", "not_run"}:
        result["robustness_status"] = "unverified"
        result["robustness_mapping"] = "decision-load conclusion retained; 64x252 not verified"
        return result
    if robustness_status == "correctness_failed":
        return {
            "status": "unresolved",
            "reason": "unattributed 64x252 cross-candidate parity failure; diagnosis required (ADR 0014)",
            "attribution_required": True,
            "robustness_status": robustness_status,
        }
    if robustness_status != "passed" or robustness_decision is None:
        result["status"] = "unresolved"
        result["reason"] = "robustness workload status or measurement is unresolved"
        result["robustness_status"] = robustness_status
        return result
    result["robustness_status"] = "passed"
    result["robustness_decision"] = robustness_decision
    if result.get("status") == "adopt" and robustness_decision.get("status") != "adopt":
        return {
            "status": "defer",
            "reason": "decision workloads passed but the 64x252 robustness gates did not",
            "robustness_status": "passed",
            "robustness_decision": robustness_decision,
        }
    return result


def _run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)


def _row_examples(expected: list[dict[str, Any]], actual: list[dict[str, Any]], key_fields: tuple[str, ...]) -> dict[str, Any]:
    """Return compact, deterministic examples of rows present on only one side."""
    expected_by_key = {tuple(row.get(field) for field in key_fields): row for row in expected}
    actual_by_key = {tuple(row.get(field) for field in key_fields): row for row in actual}
    expected_only = sorted(set(expected_by_key) - set(actual_by_key), key=repr)
    actual_only = sorted(set(actual_by_key) - set(expected_by_key), key=repr)
    return {
        "expected_only_count": len(expected_only),
        "actual_only_count": len(actual_only),
        "expected_only_examples": [expected_by_key[key] for key in expected_only[:3]],
        "actual_only_examples": [actual_by_key[key] for key in actual_only[:3]],
    }


def _mismatch_diagnostics(
    actual: dict[str, Any], expected: dict[str, Any], dataset: dict[str, Any]
) -> dict[str, Any]:
    expected_ledger = {row["date"]: row for row in expected.get("ledger", [])}
    actual_ledger = {row["date"]: row for row in actual.get("ledger", [])}
    ledger_dates = sorted(set(expected_ledger) & set(actual_ledger))
    ledger_mismatches = []
    for date in ledger_dates:
        left, right = expected_ledger[date], actual_ledger[date]
        left_holdings = {row["symbol"]: row["quantity"] for row in left.get("holdings", [])}
        right_holdings = {row["symbol"]: row["quantity"] for row in right.get("holdings", [])}
        cash_delta = float(right.get("cash", 0.0)) - float(left.get("cash", 0.0))
        nav_delta = float(right.get("nav", 0.0)) - float(left.get("nav", 0.0))
        if abs(cash_delta) > 1e-8 or abs(nav_delta) > 1e-8 or left_holdings != right_holdings:
            ledger_mismatches.append({
                "date": date,
                "expected_cash": left.get("cash"), "actual_cash": right.get("cash"),
                "cash_delta_actual_minus_expected": cash_delta,
                "expected_nav": left.get("nav"), "actual_nav": right.get("nav"),
                "nav_delta_actual_minus_expected": nav_delta,
                "expected_holdings": left_holdings, "actual_holdings": right_holdings,
            })
    cost_fields = ("commission", "tax", "slippage_cost", "total_cost")
    expected_summary = expected.get("summary", {})
    actual_summary = actual.get("summary", {})
    cost_deltas = {
        field: {
            "expected": expected_summary.get(field, 0.0),
            "actual": actual_summary.get(field, 0.0),
            "delta_actual_minus_expected": float(actual_summary.get(field, 0.0))
            - float(expected_summary.get(field, 0.0)),
        }
        for field in cost_fields
    }
    halt_overrides = [
        row for row in dataset.get("execution_status_overrides", [])
        if row.get("trade_status") == "HALTED"
    ]
    halt_attribution = []
    for halt in halt_overrides:
        date, symbol = halt["date"], halt["symbol"]
        def matching(rows: list[dict[str, Any]], date_field: str) -> list[dict[str, Any]]:
            return [row for row in rows if row.get(date_field) == date and row.get("symbol") == symbol]
        halt_attribution.append({
            "date": date, "symbol": symbol, "trade_status": "HALTED",
            "adr_0012_scope": "ADR 0012 excludes native Nautilus order lifecycle only; all project attempts, fills and account fields remain compared",
            "rust_attempt_count": len(matching(expected.get("orders", []), "attempt_date")),
            "nautilus_project_order_count": len(matching(actual.get("orders", []), "attempt_date")),
            "rust_fill_count": len(matching(expected.get("fills", []), "date")),
            "nautilus_fill_count": len(matching(actual.get("fills", []), "date")),
            "rust_attempts": matching(expected.get("orders", []), "attempt_date"),
            "nautilus_project_orders": matching(actual.get("orders", []), "attempt_date"),
            "rust_fills": matching(expected.get("fills", []), "date"),
            "nautilus_fills": matching(actual.get("fills", []), "date"),
        })
    return {
        "orders": _row_examples(
            expected.get("orders", []), actual.get("orders", []),
            ("decision_date", "attempt_date", "symbol", "side", "quantity", "reason"),
        ),
        "fills": _row_examples(expected.get("fills", []), actual.get("fills", []), ("date", "symbol", "side")),
        "daily_ledger_first_mismatches": ledger_mismatches[:3],
        "daily_ledger_mismatch_count": len(ledger_mismatches),
        "summary_costs": cost_deltas,
        "halt_attribution": halt_attribution,
    }


def _repeatability_status(attempts: list[dict[str, Any]]) -> str:
    """Only accept two matching passes or reject two identical, deterministic failures."""
    if len(attempts) != 2 or any(attempt.get("status") not in {"passed", "failed"} for attempt in attempts):
        return "unresolved"
    left, right = attempts
    if left["status"] == right["status"] == "passed":
        return "passed" if left.get("nautilus_checksum_sha256") == right.get("nautilus_checksum_sha256") else "unresolved"
    if left["status"] == right["status"] == "failed":
        return "failed" if (
            left.get("nautilus_checksum_sha256") == right.get("nautilus_checksum_sha256")
            and left.get("failure_fields") == right.get("failure_fields")
        ) else "unresolved"
    return "unresolved"


def _measurement(
    *, python: Path, binary: Path, dataset: Path, strategy: str, checksum: str,
    nautilus_checksum: str, version: str, backend: str, mode: str, group: int, output: Path,
) -> dict[str, Any]:
    wrapper = ROOT / "poc/poc0-benchmark" / (
        "measure-b2-rss.py" if backend == "rust" else "measure-b2-nautilus.py"
    )
    artifact = output.with_name(f"{output.stem}.{strategy}.{backend}.{mode}.{group}.json")
    command = [str(python), str(wrapper), "--strategy", strategy, "--mode", mode,
               "--workers", "1" if mode == "serial" else "2",
               "--runs", "20" if mode == "serial" else "6",
               "--dataset", str(dataset), "--output", str(artifact)]
    if backend == "rust":
        command.extend(["--binary", str(binary), "--dataset-version", version,
                        "--expected-checksum", checksum])
    else:
        command.extend(["--expected-checksum", nautilus_checksum])
    completed = _run(command)
    if completed.returncode:
        raise RuntimeError(f"{backend} {strategy} {mode} failed: {completed.stderr[-3000:]}")
    record = json.loads(artifact.read_text(encoding="utf-8"))
    record["artifact_path"] = artifact.relative_to(ROOT).as_posix()
    return record


def run(binary: Path, python: Path, output: Path, collect: bool) -> dict[str, Any]:
    from nautilus_adapter import _compare, _projection_checksum, _run_nautilus

    report: dict[str, Any] = {
        "schema_version": "poc0.b2-robustness-64x252.v1",
        "status": "unresolved",
        "scope": "64 instruments x 252 synthetic sessions; no independent hand-calculated golden",
        "sampling_protocol": {"warmup_runs_per_worker": 2, "serial_runs": 20,
                               "parallel_workers": 2, "parallel_runs_per_group": 6,
                               "parallel_groups": 5, "candidate_order_alternates": True},
        "loads": {},
        "decision": {"status": "unresolved", "reason": "both robustness workloads are required"},
    }
    all_passed = True
    correctness_failed = False
    for strategy, fixture in REGISTERED.items():
        dataset = json.loads(fixture["dataset"].read_text(encoding="utf-8"))
        load: dict[str, Any] = {
            "dataset_version": fixture["version"],
            "dataset_sha256": hashlib.sha256(fixture["dataset"].read_bytes()).hexdigest(),
            "dataset_content_sha256": None,
            "dimensions": {"instruments": len(dataset["instruments"]), "sessions": len(dataset["calendar"])},
            "strategy_parameters": fixture["parameters"],
            "registered_b3_stress_checksum_sha256": fixture["checksum"],
            "correctness": {"status": "not_run", "basis": "registered B3 Rust account projection stress checksum; not an independent hand golden"},
            "measurements": {"status": "not_collected"},
        }
        report["loads"][strategy] = load
        try:
            preflight_output = output.with_name(f"{output.stem}.{strategy}.rust-preflight.json")
            completed = _run([
                str(binary), "benchmark-poc0-b2", "--strategy", strategy,
                "--dataset", str(fixture["dataset"]), "--dataset-version", fixture["version"],
                "--mode", "serial", "--runs", "1", "--skip-golden-preflight",
                "--expected-checksum", fixture["checksum"], "--include-projection",
                "--output", str(preflight_output),
            ])
            if completed.returncode:
                raise RuntimeError(f"Rust B2 projection did not match registered checksum: {completed.stderr[-2000:]}")
            rust = json.loads(preflight_output.read_text(encoding="utf-8"))
            attempts = []
            for attempt_number in (1, 2):
                nautilus = _run_nautilus(dataset, strategy=strategy)["projection"]
                nautilus_checksum = _projection_checksum(nautilus)
                checks = _compare(nautilus, rust["projection"])
                checks = [
                    {**check, "passed": True if check["field"] == b2_matched.ADR_EXCLUDED else check["passed"]}
                    for check in checks
                ]
                checks.append({
                    "field": "rust_b3_registered_stress_checksum",
                    "passed": rust["run_checksum_sha256"] == fixture["checksum"],
                    "expected": fixture["checksum"], "actual": rust["run_checksum_sha256"],
                })
                passed = all(check["passed"] for check in checks)
                attempts.append({
                    "attempt": attempt_number,
                    "status": "passed" if passed else "failed",
                    "nautilus_checksum_sha256": nautilus_checksum,
                    "failure_fields": [check["field"] for check in checks if not check["passed"]],
                    "checks": checks,
                    "mismatch_diagnostics": _mismatch_diagnostics(nautilus, rust["projection"], dataset),
                })
            repeatability = _repeatability_status(attempts)
            correctness_status = repeatability
            if correctness_status == "failed":
                correctness_failed = True
            if correctness_status != "passed":
                all_passed = False
            load["dataset_content_sha256"] = rust["dataset_content_sha256"]
            load["correctness"] = {
                "status": correctness_status,
                "basis": "two independent Nautilus projection comparisons against Rust; pass requires matching checksums, failure requires identical checksums and failure fields",
                "rust_checksum_sha256": rust["run_checksum_sha256"],
                "nautilus_checksum_sha256": attempts[0]["nautilus_checksum_sha256"],
                "nautilus_serialization_checksum_note": "informational only; Python canonical JSON and Rust struct-field serialization differ",
                "repeatability": {"attempt_count": len(attempts), "status": repeatability,
                                  "checksums_identical": attempts[0]["nautilus_checksum_sha256"] == attempts[1]["nautilus_checksum_sha256"]},
                "attempts": attempts,
                "checks": attempts[0]["checks"],
            }
            if correctness_status != "passed":
                continue
        except Exception as error:
            load["correctness"] = {"status": "unresolved", "error": f"{type(error).__name__}: {error}"}
            all_passed = False
            continue

        if not collect:
            continue
        try:
            serial = {backend: _measurement(
                python=python, binary=binary, dataset=fixture["dataset"], strategy=strategy,
                checksum=fixture["checksum"], nautilus_checksum=load["correctness"]["nautilus_checksum_sha256"],
                version=fixture["version"], backend=backend,
                mode="serial", group=0, output=output,
            ) for backend in ("rust", "nautilus")}
            groups = []
            for group in range(5):
                order = ("rust", "nautilus") if group % 2 == 0 else ("nautilus", "rust")
                groups.append({"group": group + 1, "order": list(order), "runs": {
                    backend: _measurement(
                        python=python, binary=binary, dataset=fixture["dataset"], strategy=strategy,
                        checksum=fixture["checksum"], nautilus_checksum=load["correctness"]["nautilus_checksum_sha256"],
                        version=fixture["version"], backend=backend,
                        mode="parallel", group=group + 1, output=output,
                    ) for backend in order
                }})
            rust_rates = [group["runs"]["rust"]["parallel_runs_per_second"] for group in groups]
            nautilus_rates = [group["runs"]["nautilus"]["parallel_runs_per_second"] for group in groups]
            rust_peak = max(group["runs"]["rust"]["peak_rss_bytes"] for group in groups)
            nautilus_peak = max(group["runs"]["nautilus"]["peak_rss_sum_upper_bound_bytes"] for group in groups)
            decision = b2_matched.decide(
                correctness="passed", protocol="fallback_reset_unverified",
                rust_serial_median_ns=serial["rust"]["median_ns"],
                nautilus_serial_median_ns=serial["nautilus"]["median_ns"],
                rust_parallel_runs_per_second=statistics.median(rust_rates),
                nautilus_parallel_runs_per_second=statistics.median(nautilus_rates),
                rust_peak_rss_bytes=rust_peak,
                nautilus_peak_rss_sum_upper_bound_bytes=nautilus_peak,
            )
            load["measurements"] = {
                "status": "passed", "serial": serial, "parallel_groups": groups,
                "rust_parallel_runs_per_second_median": statistics.median(rust_rates),
                "nautilus_parallel_runs_per_second_median": statistics.median(nautilus_rates),
                "rss": {"rust_process_peak_bytes": rust_peak,
                        "nautilus_worker_peak_bytes": [v for group in groups for v in group["runs"]["nautilus"].get("peak_rss_by_worker_bytes", {}).values()],
                        "nautilus_peak_rss_sum_upper_bound_bytes": nautilus_peak},
                "decision": decision,
            }
        except Exception as error:
            load["measurements"] = {"status": "unverified", "reason": f"{type(error).__name__}: {error}"}
    if correctness_failed:
        report["decision"] = {
            "status": "unresolved",
            "reason": "reproducible but unattributed common-subset parity failure; performance collection skipped for the affected load (ADR 0014)",
            "attribution_required": True,
        }
    elif not all_passed:
        report["decision"] = {"status": "unverified", "reason": "one or more robustness correctness preflights could not run"}
    elif not collect:
        report["decision"] = {"status": "correctness_passed", "reason": "timing collection was not requested"}
    elif all(load["measurements"].get("status") == "passed" for load in report["loads"].values()):
        statuses = [load["measurements"]["decision"]["status"] for load in report["loads"].values()]
        report["decision"] = {"status": "adopt" if all(status == "adopt" for status in statuses) else "defer",
                               "reason": "both 64x252 workloads applied registered latency, throughput, and RSS gates",
                               "per_load": {key: value["measurements"]["decision"] for key, value in report["loads"].items()}}
    else:
        report["decision"] = {"status": "unverified", "reason": "robustness measurement blocked by resource or adapter constraints"}
    report["status"] = report["decision"]["status"]
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/quant-research")
    parser.add_argument("--python", type=Path, default=ROOT / ".venv/bin/python")
    parser.add_argument("--output", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-robustness-64x252.json")
    parser.add_argument("--build-record", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-robustness-release-build-2026-09-28.json")
    parser.add_argument("--decision-load-report", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-matched-decision-loads-2026-09-28.json")
    parser.add_argument("--run-measurements", action="store_true")
    args = parser.parse_args()
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    python = args.python if args.python.is_absolute() else ROOT / args.python
    output = args.output if args.output.is_absolute() else ROOT / args.output
    build_record = args.build_record if args.build_record.is_absolute() else ROOT / args.build_record
    report = run(binary, python, output, args.run_measurements)
    report["environment"] = b2_matched._environment(binary, python, build_record)
    report["rss_baselines"] = b2_matched._rss_baselines(binary, python)
    decision_load_report = args.decision_load_report if args.decision_load_report.is_absolute() else ROOT / args.decision_load_report
    decision_load_decision = {"status": "unresolved", "reason": "decision-load report unavailable"}
    if decision_load_report.exists():
        decision_load_decision = json.loads(decision_load_report.read_text(encoding="utf-8")).get("decision", decision_load_decision)
    if any(value["correctness"].get("status") == "failed" for value in report["loads"].values()):
        robustness_state = "correctness_failed"
    elif any(value["correctness"].get("status") != "passed" for value in report["loads"].values()):
        robustness_state = "unverified"
    elif any(value["measurements"].get("status") == "unverified" for value in report["loads"].values()):
        robustness_state = "unverified"
    elif all(value["measurements"].get("status") == "passed" for value in report["loads"].values()):
        robustness_state = "passed"
    else:
        robustness_state = "not_run"
    report["decision_load_decision"] = decision_load_decision
    report["combined_decision_mapping"] = combine_robustness(
        decision_load_decision, robustness_state, report.get("decision") if robustness_state == "passed" else None
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"status": report["status"], "output": str(output)}))
    concluded = report["status"] in {"correctness_passed", "adopt", "defer", "reject", "unverified"}
    return 0 if concluded or report["decision"].get("attribution_required") else 2


if __name__ == "__main__":
    raise SystemExit(main())
