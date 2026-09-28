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
from datetime import datetime
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DATASET = ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json"
EXPECTED = ROOT / "poc/poc0-benchmark/fixtures/expected-v1.json"
ADR_EXCLUDED = "orders.nautilus_native_lifecycle"
PROTOCOL_ENGINE_MODES = {
    "matched": "reset",
    "matched_fallback": "cached_conversion_new_engine",
    "fallback_reset_unverified": "cached_conversion_new_engine",
}
# ADR 0013: reset is selected only when parity passes; fallback only when it fails.
ADR_0013_MODE_FOR_PARITY = {"passed": "reset", "failed": "cached_conversion_new_engine"}
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
            if requirement.marker and not requirement.marker.evaluate({"extra": ""}):
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
    mode_selection: dict[str, Any] | None = None,
) -> dict[str, str]:
    """Apply ticket 09 gates; only ticket 07 mode-selection evidence makes a result registered."""
    failures = [
        field for field, passed in (correctness_checks or {}).items()
        if not passed and field != ADR_EXCLUDED
    ]
    if failures:
        return {"status": "reject", "reason": "reproducible correctness failure: " + ", ".join(failures)}
    if correctness != "passed":
        return {"status": "unresolved", "reason": "common-subset correctness gate did not pass"}
    if protocol not in PROTOCOL_ENGINE_MODES:
        return {"status": "unresolved", "reason": "measurement boundaries or Nautilus reuse mode are not matched"}
    evidence_level = "exploratory"
    if mode_selection is not None and protocol != "fallback_reset_unverified":
        selected = mode_selection.get("selected_mode")
        if selected is None or ADR_0013_MODE_FOR_PARITY.get(mode_selection.get("reset_parity")) != selected:
            return {"status": "unresolved", "reason": "mode-selection evidence is inconsistent with ADR 0013"}
        if PROTOCOL_ENGINE_MODES[protocol] != selected:
            return {"status": "unresolved", "reason": "measured Nautilus mode differs from the ticket 07 selection"}
        evidence_level = "registered"
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
        "evidence_level": evidence_level,
    }


def _run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)


def _system_value(*command: str) -> str | None:
    try:
        result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def _hardware_identity() -> tuple[str | None, int | None]:
    """Return non-sensitive CPU and memory identity fields across macOS/Linux."""
    cpu = None
    if platform.system() == "Darwin":
        hardware = _system_value("system_profiler", "SPHardwareDataType")
        if hardware:
            match = re.search(r"^\s*Chip:\s*(.+)$", hardware, re.MULTILINE)
            cpu = match.group(1).strip() if match else None
    cpu = cpu or platform.processor() or platform.machine() or None

    memory = None
    sysctl_memory = _system_value("sysctl", "-n", "hw.memsize")
    if sysctl_memory and sysctl_memory.isdigit():
        memory = int(sysctl_memory)
    else:
        try:
            memory = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
        except (AttributeError, OSError, ValueError):
            pass
    return cpu, memory


def _rss_baselines(binary: Path, python: Path) -> dict[str, Any]:
    rust_code = (
        "import json,resource,subprocess,sys; "
        "p=subprocess.run([sys.argv[1],'--help'],capture_output=True); "
        "r=resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss; "
        "print(json.dumps({'exit_code':p.returncode,'peak_rss_raw':r}))"
    )
    python_code = (
        "import json,resource; import nautilus_trader; "
        "print(json.dumps({'version':getattr(nautilus_trader,'__version__',None),"
        "'peak_rss_raw':resource.getrusage(resource.RUSAGE_SELF).ru_maxrss}))"
    )
    records: dict[str, Any] = {}
    for name, command in (
        ("rust_empty_binary", [str(python), "-c", rust_code, str(binary)]),
        ("python_import_nautilus", [str(python), "-c", python_code]),
    ):
        try:
            result = _run(command)
            parsed = json.loads(result.stdout) if result.returncode == 0 else None
            if parsed is None:
                records[name] = {"status": "unknown", "stderr": result.stderr[-1000:]}
            else:
                raw = parsed["peak_rss_raw"]
                multiplier = 1 if platform.system() == "Darwin" else 1024
                records[name] = {
                    "status": "measured",
                    "peak_rss_raw": raw,
                    "raw_unit": "bytes" if platform.system() == "Darwin" else "KiB",
                    "peak_rss_bytes": int(raw * multiplier),
                    **({"imported_nautilus_version": parsed.get("version")} if name.startswith("python") else {}),
                    "scope": "Rust binary --help startup only" if name.startswith("rust") else "Python process after importing Nautilus",
                }
        except (OSError, json.JSONDecodeError, KeyError) as error:
            records[name] = {"status": "unknown", "reason": f"{type(error).__name__}: {error}"}
    return records


def _environment(binary: Path, python: Path, build_record: Path) -> dict[str, Any]:
    rustc = _system_value("rustc", "-Vv")
    cpu, memory_bytes = _hardware_identity()
    revision = _run(["git", "rev-parse", "HEAD"]).stdout.strip()
    dirty = _run(["git", "status", "--porcelain"]).stdout.splitlines()
    try:
        python_version = _run([str(python), "--version"]).stdout.strip()
    except OSError as error:
        python_version = f"unavailable: {error}"
    try:
        import nautilus_adapter
        observed, error = nautilus_adapter._version_probe()
        nautilus_version = {"version": observed, "error": error}
    except Exception as error:
        nautilus_version = {"version": None, "error": f"{type(error).__name__}: {error}"}
    build_data = json.loads(build_record.read_text(encoding="utf-8")) if build_record.exists() else None
    try:
        build_record_path = build_record.relative_to(ROOT).as_posix()
    except ValueError:
        build_record_path = str(build_record)
    return {
        "cpu": cpu,
        "logical_cpu_count": os.cpu_count(),
        "memory_bytes": memory_bytes,
        "os": platform.platform(),
        "rustc_vv": rustc,
        "python": python_version,
        "nautilus_version": nautilus_version,
        "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest() if binary.exists() else None,
        "git_revision": revision,
        "dirty_worktree": bool(dirty),
        "dirty_paths": [line[3:] for line in dirty],
        "python_transitive_dependency_snapshot": _resolved_python_transitives(python),
        "build": {
            "record_path": build_record_path,
            "record_sha256": hashlib.sha256(build_record.read_bytes()).hexdigest() if build_record.exists() else None,
            "record": build_data,
            "build_time_seconds": build_data.get("build_wall_seconds") if build_data else None,
            "record_interval_seconds": _elapsed_seconds(build_data),
            "target_increment_bytes": build_data.get("target_delta_bytes") if build_data else None,
            "gate_minimum_free_bytes": build_data.get("minimum_free_bytes") if build_data else 10 * 1024**3,
        },
    }


def _elapsed_seconds(record: dict[str, Any] | None) -> float | None:
    if not record or not record.get("started_at_utc") or not record.get("finished_at_utc"):
        return None
    try:
        start = datetime.fromisoformat(record["started_at_utc"])
        finish = datetime.fromisoformat(record["finished_at_utc"])
        return (finish - start).total_seconds()
    except (TypeError, ValueError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/quant-research")
    parser.add_argument("--python", type=Path, default=ROOT / ".venv/bin/python")
    parser.add_argument("--output", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-matched-decision-loads.json")
    parser.add_argument("--build-record", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-matched-s2-release-build-boundary-2026-09-28.json")
    parser.add_argument("--run-measurements", action="store_true", help="collect timing after independent golden preflight")
    args = parser.parse_args()
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    output = args.output if args.output.is_absolute() else ROOT / args.output
    python = args.python if args.python.is_absolute() else ROOT / args.python

    fixtures = {
        "s2": {"dataset": DATASET, "expected": EXPECTED, "version": "poc0.synthetic.etf-daily.v1"},
        "s3": {
            "dataset": ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json",
            "expected": ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-expected-v1.json",
            "version": "poc0.b2.ma20-60.v1",
        },
    }
    preflight_path = output.with_suffix(".preflight.json")
    try:
        preflight = _run([str(binary), "benchmark-poc0", "--candidate", "soa", "--dataset", str(DATASET), "--expected", str(EXPECTED), "--output", str(preflight_path)])
        preflight_error = None
    except OSError as error:
        preflight = subprocess.CompletedProcess([], 127, "", "")
        preflight_error = f"{type(error).__name__}: {error}"
    report: dict[str, Any] = {
        "schema_version": "poc0.b2-matched-decision-loads.v2",
        "status": "unresolved",
        "decision_loads": {
            key: {
                "strategy": "S2 Momentum Rotation" if key == "s2" else "S3 MA20/60",
                "dataset_version": value["version"],
                "source_fixture_sha256": hashlib.sha256(value["dataset"].read_bytes()).hexdigest(),
                "correctness": {"status": "not_run"},
                "measurements": {"status": "not_collected"},
            }
            for key, value in fixtures.items()
        },
        "decision": {"status": "unresolved", "reason": "both registered decision workloads are required"},
        "preregistered_prediction": {
            "source": "docs/decisions/0013-poc0-b2-matched-remeasure.md",
            "predicted_speed_gates_pass_on_s2_and_s3": True,
            "predicted_rust_rss_lower_than_nautilus": True,
            "decision_input": False,
        },
        "timing_boundaries": {
            "primary": "worker-local prepared candidate state; per-Run strategy/factor, replay, accounting and projection; serialization/checksum excluded",
            "secondary": {
                "label": "end-to-end per Run from canonical bars, including conversion and engine/context initialization",
                "decision_input": False,
                "status": "not_collected",
            },
        },
        "sampling_protocol": {
            "warmup_runs_per_worker": 2,
            "serial_workers": 1,
            "serial_runs": 20,
            "parallel_workers": 2,
            "parallel_runs_per_group": 6,
            "parallel_groups": 5,
            "candidate_order_alternates_between_groups": True,
            "raw_samples_saved_per_run": True,
        },
        "maintenance_cost_proxies": maintenance_cost_proxies(python),
        "adr_0012_exclusions": [ADR_EXCLUDED],
        "environment": {},
        "rss_baselines": {},
    }
    build_record = args.build_record if args.build_record.is_absolute() else ROOT / args.build_record
    report["environment"] = _environment(binary, python, build_record)
    report["rss_baselines"] = _rss_baselines(binary, python)
    if preflight.returncode != 0:
        report["preflight_error"] = preflight_error or preflight.stderr[-4000:]
        report["decision"]["reason"] = "independent Rust golden preflight failed"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return preflight.returncode
    golden = json.loads(preflight_path.read_text(encoding="utf-8"))
    try:
        from nautilus_adapter import _compare, _projection_checksum, _run_nautilus
        rust_keys = {"s2": "b2_fast_event_momentum_rotation", "s3": "b2_fast_event_ma20_60"}
        workload_results: dict[str, dict[str, Any]] = {}
        for strategy, fixture in fixtures.items():
            rust_golden = golden[rust_keys[strategy]]
            report["decision_loads"][strategy]["dataset_content_sha256"] = rust_golden.get("dataset_content_sha256", rust_golden.get("input_sha256"))
            dataset = json.loads(fixture["dataset"].read_text(encoding="utf-8"))
            if strategy == "s3":
                from nautilus_adapter import _ma_dataset
                dataset = _ma_dataset(dataset)
            try:
                nautilus_projection = _run_nautilus(dataset, strategy=strategy)["projection"]
                checks = _compare(nautilus_projection, rust_golden["projection"])
                nautilus_checksum = _projection_checksum(nautilus_projection)
                checks.append({"field": "signals.rank_topk_targets", "passed": nautilus_projection.get("signals") == rust_golden.get("signals")})
                if strategy == "s2":
                    actual_rankings = nautilus_projection.get("rankings", [])
                    expected_rankings = rust_golden.get("rankings", [])
                    rank_equal = len(actual_rankings) == len(expected_rankings) and all(
                        actual.get("date") == expected.get("date")
                        and len(actual.get("candidates", [])) == len(expected.get("candidates", []))
                        and all(
                            left.get("symbol") == right.get("symbol")
                            and abs(left.get("score", 0.0) - right.get("score", 0.0)) <= 1e-8
                            for left, right in zip(actual.get("candidates", []), expected.get("candidates", []), strict=True)
                        )
                        for actual, expected in zip(actual_rankings, expected_rankings, strict=True)
                    )
                    checks.append({"field": "signals.factor_rankings", "passed": rank_equal})
            except Exception as error:
                checks = []
                nautilus_checksum = None
                error_text = f"{type(error).__name__}: {error}"
            else:
                error_text = None
            non_excluded_failures = [item["field"] for item in checks if not item["passed"] and item["field"] != ADR_EXCLUDED]
            correctness_status = "passed" if checks and not non_excluded_failures else "failed" if checks else "unresolved"
            load = report["decision_loads"][strategy]
            load["correctness"] = {
                "status": correctness_status,
                "rust_checksum_sha256": rust_golden.get("checksum_sha256"),
                "nautilus_checksum_sha256": nautilus_checksum,
                "checks": checks,
                "non_excluded_failures": non_excluded_failures,
                "error": error_text,
                "independent_golden_preflight": str(preflight_path.relative_to(ROOT)),
            }
            workload_results[strategy] = {"dataset": fixture["dataset"], "correctness": correctness_status, "rust_golden": rust_golden, "nautilus_checksum": nautilus_checksum}

        if not args.run_measurements:
            report["decision"]["reason"] = "correctness captured; formal measurement collection was not requested"
        elif any(item["correctness"] != "passed" for item in workload_results.values()):
            report["decision"] = {"status": "unresolved", "reason": "at least one required S2/S3 correctness gate did not pass; measurements skipped"}
        else:
            rss_wrapper = ROOT / "poc/poc0-benchmark/measure-b2-rss.py"
            nautilus_wrapper = ROOT / "poc/poc0-benchmark/measure-b2-nautilus.py"
            for strategy, item in workload_results.items():
                load = report["decision_loads"][strategy]
                rust_checksum = item["rust_golden"]["checksum_sha256"]
                nautilus_checksum = item["nautilus_checksum"]
                def collect(backend: str, mode: str, group: int = 0) -> dict[str, Any]:
                    measurement_output = output.with_name(f"{output.stem}.{strategy}.{backend}.{mode}.{group}.json")
                    wrapper = rss_wrapper if backend == "rust" else nautilus_wrapper
                    command = [str(python), str(wrapper), "--strategy", strategy, "--mode", mode,
                               "--workers", "1" if mode == "serial" else "2",
                               "--runs", "20" if mode == "serial" else "6",
                               "--dataset", str(item["dataset"]), "--output", str(measurement_output)]
                    if backend == "rust":
                        command.extend(["--binary", str(binary), "--expected-checksum", rust_checksum])
                    else:
                        command.extend(["--expected-checksum", nautilus_checksum])
                    result = _run(command)
                    if result.returncode:
                        raise RuntimeError(f"{backend} {strategy} {mode} measurement failed: {result.stderr[-3000:]}")
                    measurement = json.loads(measurement_output.read_text(encoding="utf-8"))
                    measurement["artifact_path"] = measurement_output.relative_to(ROOT).as_posix()
                    return measurement
                serial = {backend: collect(backend, "serial") for backend in ("rust", "nautilus")}
                groups = []
                for group in range(5):
                    order = ("rust", "nautilus") if group % 2 == 0 else ("nautilus", "rust")
                    groups.append({"group": group + 1, "order": list(order), "runs": {backend: collect(backend, "parallel", group + 1) for backend in order}})
                rust_rates = [group["runs"]["rust"]["parallel_runs_per_second"] for group in groups]
                nautilus_rates = [group["runs"]["nautilus"]["parallel_runs_per_second"] for group in groups]
                rust_peak = max(group["runs"]["rust"]["peak_rss_bytes"] for group in groups)
                nautilus_peak = max(group["runs"]["nautilus"]["peak_rss_sum_upper_bound_bytes"] for group in groups)
                engine_mode = serial["nautilus"].get("engine_mode")
                protocol = "fallback_reset_unverified" if engine_mode == "cached_conversion_new_engine" and all(group["runs"]["nautilus"].get("engine_mode") == engine_mode for group in groups) else "unmatched"
                serial_median = {backend: serial[backend].get("median_ns") for backend in serial}
                primary = {
                    "serial": serial,
                    "parallel_groups": groups,
                    "rust_parallel_runs_per_second_median": statistics.median(rust_rates),
                    "nautilus_parallel_runs_per_second_median": statistics.median(nautilus_rates),
                    "rss": {
                        "rust_process_peak_bytes": rust_peak,
                        "nautilus_worker_peak_bytes": [entry for group in groups for entry in group["runs"]["nautilus"].get("peak_rss_by_worker_bytes", {}).values()],
                        "nautilus_peak_rss_sum_upper_bound_bytes": nautilus_peak,
                        "upper_bound_note": "sum of per-worker peaks; worker peaks may not coincide",
                    },
                    "nautilus_engine_mode": engine_mode,
                    "nautilus_engine_mode_reason": serial["nautilus"].get("engine_mode_reason"),
                    "protocol_status": protocol,
                    "decision_input": True,
                }
                load["measurements"] = {
                    "status": "passed",
                    "primary_boundary": primary,
                    "secondary_boundary": {
                        "status": "measured",
                        "decision_input": False,
                        "definition": "one Run from canonical source bars; includes conversion, factor/signal work, engine/context initialization, replay and projection",
                        "serial": {backend: serial[backend].get("secondary_boundary_samples_ns", []) for backend in serial},
                        "parallel_groups": [
                            {"group": group["group"], "runs": {
                                backend: group["runs"][backend].get("secondary_boundary_samples_ns", [])
                                for backend in group["runs"]
                            }}
                            for group in groups
                        ],
                    },
                }
                load_decision = decide(
                    correctness=item["correctness"], protocol=protocol,
                    rust_serial_median_ns=serial_median["rust"], nautilus_serial_median_ns=serial_median["nautilus"],
                    rust_parallel_runs_per_second=statistics.median(rust_rates), nautilus_parallel_runs_per_second=statistics.median(nautilus_rates),
                    rust_peak_rss_bytes=rust_peak, nautilus_peak_rss_sum_upper_bound_bytes=nautilus_peak,
                    correctness_checks={check["field"]: check["passed"] for check in load["correctness"]["checks"]},
                )
                if load_decision.get("evidence_level") == "exploratory":
                    predicted_pass = True
                    observed_pass = load_decision["status"] == "adopt"
                    load["prediction_comparison"] = {
                        "predicted_speed_and_rss_gates_pass": predicted_pass,
                        "observed_speed_and_rss_gates_pass": observed_pass,
                        "prediction_matches": predicted_pass == observed_pass,
                        "observed_gate_preliminary_status": load_decision["status"],
                    }
                    load_decision["preliminary_status"] = load_decision["status"]
                    load_decision["status"] = "unresolved"
                    load_decision["reason"] = "fallback mode is exploratory because Nautilus reset parity has not been verified"
                load["decision"] = load_decision
            decisions = [report["decision_loads"][key].get("decision", {"status": "unresolved"}) for key in ("s2", "s3")]
            statuses = [decision["status"] for decision in decisions]
            if "reject" in statuses:
                overall = "reject"
            elif "unresolved" in statuses:
                overall = "unresolved"
            else:
                overall = "adopt" if all(status == "adopt" for status in statuses) else "defer"
            report["decision"] = {
                "status": overall,
                "reason": "both S2 and S3 have exploratory fallback measurements; formal adoption remains unresolved until Nautilus reset parity is established",
                "per_load": {key: value.get("decision", {"status": "unresolved"}) for key, value in report["decision_loads"].items()},
            }
            report["timing_boundaries"]["secondary"]["status"] = "measured_and_excluded_from_decision"
        report["measurement_environment"] = {
            "rss_scope": "measurement child process(es) only; coordinator excluded",
            "golden_preflight_scope": "separate process; excluded from measured RSS",
            "python_dependency_snapshot_is_complete_lock": False,
            "target_build_record_included": bool(report["environment"]["build"]["record"]),
        }
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
