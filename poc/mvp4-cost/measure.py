"""Record-only Accurate adapter measurements; never apply an adoption threshold."""

import argparse
import ast
import hashlib
import importlib.metadata
import json
import math
import os
from pathlib import Path
import platform
import re
import signal
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[2]
GIB = 1024 ** 3


def write(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def text(command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def series(values):
    if not values or any(not math.isfinite(v) or v <= 0 for v in values):
        raise ValueError("positive finite measurements required")
    ordered = sorted(values)
    return {"values": values, "median": statistics.median(values),
            "p95": ordered[math.ceil(len(values) * .95) - 1], "min": min(values), "max": max(values)}


def summarize_cell(cell):
    if cell.get("status") == "missing":
        return {"status": "missing", "reason": cell["reason"], "metrics": None}
    if len(cell["main"]) != 20 or len(cell["end_to_end"]) != 20 or len(cell["parallel"]) != 5:
        raise ValueError("incomplete 20/20/5 measurement matrix")
    samples = cell["main"] + cell["end_to_end"]
    for group in cell["parallel"]:
        if len(group["workers"]) != 2 or any(len(w["samples"]) != 6 for w in group["workers"]):
            raise ValueError("parallel requires two workers with six Runs each")
        if group["wall_ns"] < max(sum(s["wall_ns"] for s in w["samples"]) for w in group["workers"]):
            raise ValueError("parallel wall time must enclose every worker's timed execution")
        samples += [s for w in group["workers"] for s in w["samples"]]
    if len({s["checksum"] for s in samples}) != 1:
        raise ValueError("result checksum changed")
    peaks = [max(s["usage"]["rss_bytes"] for s in w["samples"])
             for group in cell["parallel"] for w in group["workers"]]
    baselines = [w["baseline"]["rss_bytes"] for group in cell["parallel"] for w in group["workers"]]
    result = {
        "main_ns": series([s["wall_ns"] for s in cell["main"]]),
        "end_to_end_ns": series([s["wall_ns"] for s in cell["end_to_end"]]),
        "parallel_runs_per_second": series([12 * 1e9 / g["wall_ns"] for g in cell["parallel"]]),
        "rss_worker_peaks_bytes": peaks, "rss_worker_baselines_bytes": baselines,
        "rss_worker_peak_sum_upper_bound_bytes": max(sum(peaks[i:i+2]) for i in range(0, 10, 2)),
        "rss_baseline_sum_bytes": max(sum(baselines[i:i+2]) for i in range(0, 10, 2)),
        "serial_peak_bytes": max(s["usage"]["rss_bytes"] for s in cell["main"] + cell["end_to_end"]),
        "serial_baseline_bytes": cell["baseline"]["rss_bytes"],
        "checksum": samples[0]["checksum"],
    }
    if all("backend_conversion" in s for s in cell["end_to_end"]):
        result["backend_conversion_ns"] = series([s["backend_conversion"]["conversion_ns"] for s in cell["end_to_end"]])
    return result


def guarded(command, report, output, *, estimate=64 * 1024 ** 2, timeout=1800, env=None):
    free = shutil.disk_usage(ROOT)[2]
    record = {"command": [str(c) for c in command], "free_bytes_before": free,
              "estimated_additional_bytes": estimate, "started_at": datetime.now(timezone.utc).isoformat(),
              "space_samples": []}
    report["commands"].append(record)
    if free - estimate < 10 * GIB:
        record["status"] = "blocked"
        report.update(status="missing", reason="10 GiB resource gate")
        write(output, report)
        return False
    log = output.parent / f"command-{len(report['commands']):02d}.log"
    start = time.monotonic()
    with log.open("w") as stream:
        process = subprocess.Popen(command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT,
                                   env=env, start_new_session=True)
        while process.poll() is None:
            elapsed = time.monotonic() - start
            free = shutil.disk_usage(ROOT)[2]
            record["space_samples"].append({"elapsed_seconds": elapsed, "free_bytes": free})
            if free < 11 * GIB or elapsed > timeout:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                record["status"] = "aborted"
                report.update(status="missing", reason="10 GiB resource gate" if free < 11 * GIB else "timeout")
                break
            time.sleep(.5)
        record.update(exit_code=process.wait(), wall_seconds=time.monotonic() - start,
                      free_bytes_after=shutil.disk_usage(ROOT)[2], log=log.name)
    if "status" not in record:
        record["status"] = "completed" if record["exit_code"] == 0 else "failed"
        if record["exit_code"] != 0:
            report.update(status="failed", reason=f"command failed: {log.name}")
    write(output, report)
    return record["status"] == "completed"


def maintenance():
    rust_paths = sorted((ROOT / "crates/prajna-adapter/src").glob("*.rs"))
    rust_paths += [ROOT / "crates/prajna-experiment/src/worker.rs"]
    python_paths = [ROOT / f"backends/nautilus/{name}.py" for name in ("worker", "packet")]
    def loc(paths, rust=False):
        return sum(sum(bool(line.strip()) and not line.lstrip().startswith(("//",) if rust else ("#",))
                       for line in (p.read_text().split("#[cfg(test)]")[0] if rust else p.read_text()).splitlines())
                   for p in paths)
    tree = text(["cargo", "tree", "-p", "prajna-adapter", "--edges", "normal", "--prefix", "depth",
                 "--no-dedupe", "--locked", "--offline"])
    packages = set(re.findall(r"^\d+(\S+ v\S+)", tree, re.M))
    direct = tomllib.loads((ROOT / "crates/prajna-adapter/Cargo.toml").read_text())["dependencies"]
    requirements = [line for line in (ROOT / "backends/nautilus/requirements.txt").read_text().splitlines()
                    if line and not line.startswith("#")]
    discovered = {}
    pending = [r.split("==")[0] for r in requirements]
    while pending:
        name = pending.pop().lower().replace("_", "-")
        if name in discovered:
            continue
        dist = importlib.metadata.distribution(name)
        dependencies = []
        for requirement in dist.requires or []:
            # Evaluate PEP 508 markers using pip's vendored parser, without installing tooling.
            from pip._vendor.packaging.requirements import Requirement
            parsed = Requirement(requirement)
            if parsed.marker is None or parsed.marker.evaluate({"extra": ""}):
                dependencies.append(parsed.name)
        discovered[name] = {"version": dist.version, "requires": dependencies}
        pending += dependencies
    rust_test_paths = list((ROOT / "crates/prajna-adapter").rglob("*.rs"))
    rust_test_paths += [ROOT / f"crates/prajna-experiment/tests/{name}.rs"
                       for name in ("worker_failures", "accurate_event", "accurate_correctness")]
    rust_test_paths = [p for p in rust_test_paths if p.exists()]
    python_test = ROOT / "backends/nautilus/test_worker.py"
    return {
        "loc_rule": "nonblank non-comment-prefix lines; Rust inline cfg(test) suffix excluded; docstrings count",
        "rust_files": {str(p.relative_to(ROOT)): sha(p) for p in rust_paths},
        "python_files": {str(p.relative_to(ROOT)): sha(p) for p in python_paths},
        "rust_non_test_loc": loc(rust_paths, True), "python_non_test_loc": loc(python_paths),
        "rust_test_count": sum(len(re.findall(r"#\[test\]", p.read_text())) for p in rust_test_paths),
        "rust_test_paths": [str(p.relative_to(ROOT)) for p in rust_test_paths],
        "python_test_count": sum(isinstance(n, ast.FunctionDef) and n.name.startswith("test_")
                                 for n in ast.walk(ast.parse(python_test.read_text()))),
        "rust_dependency_scope": "prajna-adapter normal edges only; process host experiment dependencies excluded",
        "rust_direct_dependencies": sorted(direct), "rust_direct_count": len(direct),
        "rust_reachable_count_excluding_root": len(packages) - 1,
        "rust_transitive_count_excluding_direct_and_root": sum(
            p.split(" v")[0] not in set(direct) | {"prajna-adapter"} for p in packages),
        "rust_dependency_tree": tree,
        "python_direct_requirements": requirements, "python_direct_count": len(requirements),
        "python_resolved_count_including_direct": len(discovered), "python_dependency_snapshot": discovered,
        "python_transitive_count_excluding_direct": len(discovered) - len(requirements),
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["run", "summarize"])
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--input", type=Path)
    args = parser.parse_args(argv)
    if args.command == "summarize":
        if args.input is None:
            parser.error("summarize requires --input")
        report = json.loads(args.input.read_text())
        if report["status"] not in ("measured", "measured_with_missing"):
            raise ValueError("cannot summarize missing or failed measurements")
        write(args.out, {key: summarize_cell(cell) for key, cell in report["measurements"].items()})
        return 0
    free = shutil.disk_usage(ROOT)[2]
    report = {
        "schema_version": "mvp4.cost@1",
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "thresholds": None,
        "scope": "synthetic cost only; not production Engine selection or real-market performance",
        "resource_gate": {"free_bytes": free, "reserve_bytes": 10 * GIB},
        "status": "missing", "reason": "10 GiB resource gate", "measurements": None, "commands": [],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    if free < 10 * GIB:
        write(args.out, report)
        print(report["reason"], file=sys.stderr)
        return 2
    report.update(status="running", reason=None)
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    report["identity"] = {
        "git_revision": text(["git", "rev-parse", "HEAD"]),
        "git_status": text(["git", "status", "--short"]),
        "dirty_diff_sha256": hashlib.sha256(text(["git", "diff", "HEAD"]).encode()).hexdigest(),
        "python": sys.version, "backend": {"name": "nautilus_trader", "version": importlib.metadata.version("nautilus_trader")},
        "rustc": text(["rustc", "-Vv"]), "cargo_lock_sha256": sha(ROOT / "Cargo.lock"),
        "machine": platform.platform(), "architecture": platform.machine(), "logical_cores": os.cpu_count(),
        "cpu": text(["sysctl", "-n", "machdep.cpu.brand_string"]) if sys.platform == "darwin" else platform.processor(),
        "memory_bytes": int(text(["sysctl", "-n", "hw.memsize"])) if sys.platform == "darwin" else None,
        "target": str(target), "profile": "release", "cold_build": "Unknown; shared caches not cleared",
        "thread_environment": {k: v for k, v in os.environ.items() if k in ("RAYON_NUM_THREADS", "OMP_NUM_THREADS")},
    }
    source_paths = [
        "backends/nautilus/worker.py", "backends/nautilus/packet.py",
        "crates/prajna-experiment/examples/measure_accurate.rs",
        "crates/prajna-experiment/tests/support/exact_execution.rs",
        "crates/prajna-adapter/src/execution.rs", "crates/prajna-research/src/fast_event.rs",
        "poc/mvp4-cost/measure.py", "poc/mvp4-cost/prepared_worker.py",
        "poc/mvp3-golden/fast_event_golden.py", "poc/mvp4-correctness/prepare_fixture.py",
    ]
    report["identity"]["source_sha256"] = {p: sha(ROOT / p) for p in source_paths}
    if report["identity"]["backend"]["version"] != "2.0.0rc6":
        raise ValueError("real pinned rc6 required; no fallback")
    report["maintenance"] = maintenance()
    report["boundary"] = {
        "mode": "cached_conversion_new_engine",
        "reason": "production lifecycle is new engine per Run; reset not certified by this ticket",
        "main": "prepared canonical packet + cached Backend instruments/quotes/status; clone Decision tape, new engine, Rust closed-loop ledger, Domain Fill/results",
        "end_to_end": "canonical in-memory RunPacket -> Parquet + config -> Backend objects -> new engine -> Rust closed-loop -> Domain Fill/results",
        "excluded": "source fixture/golden generation, dataset publication/loading, process-pool startup, result serialization/checksum; factors/signals precomputed Decision tape",
        "rss": "ru_maxrss whole Python worker incl imports and conversion; sum of two peaks is an upper bound; Rust controller excluded",
        "repeats": {"warmup": 2, "serial_main": 20, "serial_end_to_end": 20, "parallel_groups": 5, "workers": 2, "runs_per_worker": 6},
        "comparison": "no Fast speed ratio, adoption threshold or correctness claim for 64x252",
        "scale_mapping_policy": "owner chose original Instrument increment; unrepresentable synthetic prices remain missing, no rounding or precision variant",
    }
    build = ["cargo", "build", "--release", "-p", "prajna-experiment", "--features",
             "nautilus-integration", "--example", "measure_accurate", "--test", "accurate_correctness", "--locked", "--offline"]
    # Conservative staged allowance, informed by the archived MVP-3 lightweight build.
    target_bytes = lambda: sum(p.stat().st_size for p in target.rglob("*") if p.is_file())
    build_bytes_before = target_bytes()
    if not guarded(build, report, args.out, estimate=6 * GIB):
        return 2
    report["commands"][-1].update(target_bytes_before=build_bytes_before, target_bytes_after=target_bytes(),
                                  estimate_basis="conservative 6 GiB allowance; archived MVP-3 release/lightweight dependency path; guarded staged build, cold cost Unknown")
    env = {**os.environ, "PRAJNA_NAUTILUS_PYTHON": sys.executable,
           "PRAJNA_CORRECTNESS_REPORT": str(args.out.parent.resolve() / "correctness.json")}
    if not guarded(["cargo", "test", "--release", "-p", "prajna-experiment", "--features",
                    "nautilus-integration", "--test", "accurate_correctness", "--locked", "--offline"],
                   report, args.out, env=env):
        return 2
    correctness = json.loads(Path(env["PRAJNA_CORRECTNESS_REPORT"]).read_text())
    if correctness["conclusion"] != "pass" or correctness["adapter_defects"] or correctness["unclassified_differences"]:
        raise ValueError("A15 correctness prerequisite failed")
    report["correctness"] = {"report_sha256": sha(Path(env["PRAJNA_CORRECTNESS_REPORT"])), "conclusion": "pass"}
    cases = [(name, strategy) for name, strategy in [
        ("hand-v1", "s1"), ("hand-retry-v1", "s1"), ("hand-rebalance-v1", "s2"),
        ("hand-fee-only-v1", "s2"), ("dataset-v1", "s1"), ("dataset-v1", "s2"),
        ("dataset-v1", "s3"), ("b2-ma20-60-v1", "s3"),
        ("b2-s2-scale-64x252-v2", "s2"), ("b2-s3-scale-64x252-v1", "s3"),
    ]]
    report["measurements"] = {}
    for name, strategy in cases:
        fixture = ROOT / ("poc/mvp3-golden/fixtures" if name.startswith("hand") else "poc/poc0-benchmark/fixtures") / f"{name}.json"
        with tempfile.TemporaryDirectory(prefix="mvp4-cost-") as directory:
            directory = Path(directory)
            golden, normalized, measured = [directory / f"{n}.json" for n in ("golden", "normalized", "measured")]
            blocked = False
            for command in [
                [sys.executable, ROOT / "poc/mvp3-golden/fast_event_golden.py", "--fixture", fixture,
                 "--strategy", strategy, "--sizing", "lot", "--rebalance", "full_target", "--out", golden],
                [sys.executable, ROOT / "poc/mvp4-correctness/prepare_fixture.py", fixture, normalized],
                [target / "release/examples/measure_accurate", sys.executable, normalized, golden, measured],
            ]:
                if not guarded(command, report, args.out):
                    failure = report["commands"][-1]
                    log = args.out.parent / failure["log"] if "log" in failure else None
                    if ("64x252" in name and failure["status"] == "failed" and log is not None
                            and "unrepresentable price" in log.read_text()):
                        report["measurements"][f"{name}.{strategy}"] = {
                            "status": "missing", "reason": "Backend mapping limitation: original Instrument increment rejects synthetic slippage price",
                            "metrics": None, "fixture_sha256": sha(fixture),
                            "parameters": json.loads(golden.read_text())["parameters"],
                            "role": "robustness/cost only", "failure_log": log.name,
                        }
                        report.update(status="running", reason=None)
                        blocked = True
                        write(args.out, report)
                        break
                    return 2
            if blocked:
                continue
            cell = json.loads(measured.read_text())
            cell.update(fixture_sha256=sha(fixture), strategy=strategy,
                        parameters=json.loads(golden.read_text())["parameters"],
                        role="robustness/cost only" if "64x252" in name else "fixed fixture")
            summarize_cell(cell)
            report["measurements"][f"{name}.{strategy}"] = cell
            write(args.out, report)
    report.update(status="measured_with_missing" if any(c.get("status") == "missing" for c in report["measurements"].values())
                  else "measured", finished_at=datetime.now(timezone.utc).isoformat())
    report["binary_sha256"] = sha(target / "release/examples/measure_accurate")
    if report["identity"]["source_sha256"] != {p: sha(ROOT / p) for p in source_paths}:
        raise ValueError("measurement source changed during run")
    write(args.out, report)
    write(args.out.with_name("summary.json"), {key: summarize_cell(cell) for key, cell in report["measurements"].items()})
    return 0


if __name__ == "__main__":
    sys.exit(main())
