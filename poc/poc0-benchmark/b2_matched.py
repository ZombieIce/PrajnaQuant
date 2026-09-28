#!/usr/bin/env python3
"""S2 matched-boundary B2 remeasurement coordinator for the fixed 3x10 golden."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DATASET = ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
EXPECTED = ROOT / "poc/poc0-benchmark/fixtures/expected-v1.json"
ADR_EXCLUDED = "orders.nautilus_native_lifecycle"
NAUTILUS_DIRECT_REQUIREMENT = "nautilus_trader==2.0.0rc5"


def _code_line_count(path: Path) -> int:
    """Count nonblank, non-comment source lines, excluding Rust inline test modules."""
    content = path.read_text(encoding="utf-8").splitlines()
    count = 0
    in_rust_test_module = False
    test_module_depth = 0
    block_comment_depth = 0
    for line in content:
        stripped = line.strip()
        if path.suffix == ".rs" and stripped == "#[cfg(test)]":
            in_rust_test_module = True
            continue
        if in_rust_test_module:
            test_module_depth += line.count("{") - line.count("}")
            if test_module_depth <= 0 and "{" in line:
                in_rust_test_module = False
                test_module_depth = 0
            elif test_module_depth <= 0 and "}" in line:
                in_rust_test_module = False
                test_module_depth = 0
            continue
        if not stripped:
            continue
        if path.suffix == ".py":
            if stripped.startswith("#"):
                continue
            count += 1
            continue
        # Rust: discard whole-line comments and block-comment-only lines.
        remaining = stripped
        while remaining:
            if block_comment_depth:
                close = remaining.find("*/")
                if close < 0:
                    remaining = ""
                    break
                block_comment_depth -= 1
                remaining = remaining[close + 2 :].strip()
                continue
            if remaining.startswith("//"):
                remaining = ""
                break
            opening = remaining.find("/*")
            if opening < 0:
                break
            close = remaining.find("*/", opening + 2)
            if close < 0:
                block_comment_depth += 1
                remaining = remaining[:opening].strip()
                break
            remaining = (remaining[:opening] + remaining[close + 2 :]).strip()
        if remaining:
            count += 1
    return count


def _resolved_python_transitives(python: Path) -> dict[str, Any]:
    """Count installed requirements reachable from the pinned Nautilus distribution."""
    code = r'''import importlib.metadata as metadata, json, sys
try:
    from packaging.requirements import Requirement
    from packaging.utils import canonicalize_name
except ImportError:
    try:
        from pip._vendor.packaging.requirements import Requirement
        from pip._vendor.packaging.utils import canonicalize_name
    except Exception as error:
        print(json.dumps({"status": "unknown", "reason": f"packaging unavailable: {error}"}))
        raise SystemExit(0)
try:
    root = metadata.distribution("nautilus_trader")
except metadata.PackageNotFoundError:
    print(json.dumps({"status": "unknown", "reason": "pinned nautilus_trader distribution is not installed"}))
    raise SystemExit(0)
root_requirement = Requirement(__ROOT_REQUIREMENT__)
if not root_requirement.specifier.contains(root.version, prereleases=True):
    print(json.dumps({"status": "unknown", "reason": f"installed nautilus_trader {root.version} does not satisfy {root_requirement}"}))
    raise SystemExit(0)
installed = {canonicalize_name(d.metadata["Name"]): d for d in metadata.distributions() if d.metadata.get("Name")}
seen = set()
pending = [root]
missing = set()
version_mismatches = set()
unparseable = set()
while pending:
    current = pending.pop()
    for raw in current.requires or []:
        try:
            requirement = Requirement(raw)
            if requirement.marker and not requirement.marker.evaluate():
                continue
            name = canonicalize_name(requirement.name)
        except Exception:
            unparseable.add(raw)
            continue
        resolved = root if name == "nautilus-trader" else installed.get(name)
        if resolved is None:
            missing.add(name)
        else:
            if requirement.specifier and not requirement.specifier.contains(resolved.version, prereleases=True):
                version_mismatches.add(f"{name}=={resolved.version} does not satisfy {requirement}")
            if name != "nautilus-trader" and name not in seen:
                seen.add(name)
                pending.append(resolved)
packages = sorted(f"{name}=={installed[name].version}" for name in seen)
is_known = not missing and not version_mismatches and not unparseable
print(json.dumps({"status": "known" if is_known else "unknown", "count": len(seen), "packages": packages, "missing": sorted(missing), "version_mismatches": sorted(version_mismatches), "unparseable_requirements": sorted(unparseable)}))'''
    code = code.replace("__ROOT_REQUIREMENT__", repr(NAUTILUS_DIRECT_REQUIREMENT))
    try:
        result = subprocess.run(
            [str(python), "-c", code], cwd=ROOT, text=True, capture_output=True, check=False
        )
    except OSError as error:
        return {"status": "unknown", "count": None, "reason": f"{type(error).__name__}: {error}"}
    if result.returncode:
        return {"status": "unknown", "count": None, "reason": result.stderr[-1000:]}
    try:
        resolved = json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"status": "unknown", "count": None, "reason": "dependency inventory output was invalid"}
    if resolved.get("status") != "known":
        return {
            "status": "unknown",
            "count": None,
            "packages": resolved.get("packages", []),
            "missing_packages": resolved.get("missing", []),
            "version_mismatches": resolved.get("version_mismatches", []),
            "unparseable_requirements": resolved.get("unparseable_requirements", []),
            "reason": resolved.get("reason", "one or more declared dependencies are not installed"),
        }
    return {
        "status": "known",
        "count": resolved["count"],
        "packages": resolved["packages"],
        "method": "transitive distribution closure from nautilus_trader metadata; active environment markers applied",
    }


def maintenance_cost_proxies(python: Path) -> dict[str, Any]:
    """Collect descriptive maintenance proxies; these values are never decision inputs."""
    rust_sources = [
        "crates/quant-research/src/poc0_benchmark.rs",
        "crates/quant-research/src/main.rs",
    ]
    nautilus_sources = ["poc/poc0-benchmark/nautilus_adapter.py"]
    rust_test_names = {
        "benchmark_cli_emits_reproducible_report_after_golden_passes",
        "b2_isolated_cli_measures_fixed_parallel_runs_after_correctness_gate",
        "b2_parallel_warmup_mismatch_returns_instead_of_stranding_workers",
        "fast_event_reports_final_unfilled_target_with_nonzero_buy_tax",
    }
    rust_test_source = ROOT / "crates/quant-research/tests/poc0_benchmark_cli.rs"
    rust_test_content = rust_test_source.read_text(encoding="utf-8")
    rust_tests = sum(
        1 for name in rust_test_names
        if re.search(rf"#\[test\]\s+fn\s+{re.escape(name)}\s*\(", rust_test_content)
    )
    python_test_sources = [
        "poc/poc0-benchmark/test_b2_matched.py",
        "poc/poc0-benchmark/test_nautilus_adapter.py",
    ]
    python_tests = sum(
        len(re.findall(r"^\s*def test_[A-Za-z0-9_]+\s*\(", (ROOT / source).read_text(encoding="utf-8"), re.MULTILINE))
        for source in python_test_sources
    )
    return {
        "decision_inputs": False,
        "code_lines": {
            "method": "physical source lines excluding blank/comment-only lines; Rust #[cfg(test)] modules excluded",
            "fast_event_poc": {
                "count": sum(_code_line_count(ROOT / path) for path in rust_sources),
                "files": rust_sources,
                "scope_note": "shared POC benchmark module and CLI; includes adjacent candidate code in the shared module",
            },
            "nautilus_adapter": {
                "count": sum(_code_line_count(ROOT / path) for path in nautilus_sources),
                "files": nautilus_sources,
                "scope_note": "adapter implementation only; excludes coordinator and measurement wrappers",
            },
        },
        "tests": {
            "method": "count named B2 Rust CLI test cases and all test methods in the listed Nautilus/B2 Python test files",
            "fast_event_poc": {
                "count": rust_tests,
                "status": "known" if rust_tests == len(rust_test_names) else "unknown",
                "files": ["crates/quant-research/tests/poc0_benchmark_cli.rs"],
                "expected_named_cases": sorted(rust_test_names),
                "expected_case_count": len(rust_test_names),
            },
            "nautilus_adapter": {"count": python_tests, "files": python_test_sources},
        },
        "new_direct_dependencies": {
            "method": "B2 candidate-specific direct requirements; existing Rust workspace dependencies are not recounted",
            "fast_event_poc": {"count": 0, "units": "Cargo crates", "items": [], "source": "crates/quant-research/Cargo.toml"},
            "nautilus_adapter": {"count": 1, "units": "Python packages", "items": [NAUTILUS_DIRECT_REQUIREMENT], "source": "poc/poc0-benchmark/requirements-nautilus.txt"},
        },
        "python_transitive_dependencies": {
            **_resolved_python_transitives(python),
            "measurement_python_executable": str(python),
            "scope": "installed dependency closure reachable from the pinned NautilusTrader distribution in the measurement Python environment",
            "complete_lock": False,
            "lock_note": "resolved environment snapshot only; requirements-nautilus.txt pins the direct dependency but no complete Python lock is present",
        },
        "known_irreducible_semantic_differences": [
            {
                "id": "orders.nautilus_native_lifecycle",
                "description": "Adapter project-level execution-status events are distinct from Nautilus native order submissions/lifecycle; excluded from parity and decision gates.",
                "source": "docs/decisions/0012-poc0-nautilus-status-gate.md",
            }
        ],
        "semantic_difference_count": 1,
        "threshold_or_decision_role": "record_only",
    }


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
    python = args.python if args.python.is_absolute() else ROOT / args.python

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
        "maintenance_cost_proxies": maintenance_cost_proxies(python),
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
