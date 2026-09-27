#!/usr/bin/env python3
"""Guard the pinned Nautilus install and shared-target build before running POC-0."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
MIN_FREE_BYTES = 10 * 1024**3
PINNED_VERSION = "2.0.0rc5"
REQUIREMENTS = ROOT / "poc/poc0-benchmark/requirements-nautilus.txt"
BUILD_RECORDER = ROOT / "poc/poc0-benchmark/capture-build-resource.py"


def _write(path: Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")


def _installed_version(python: Path) -> str | None:
    probe = subprocess.run(
        [str(python), "-c", "import importlib.metadata; print(importlib.metadata.version('nautilus-trader'))"],
        cwd=ROOT, text=True, capture_output=True, check=False,
    )
    return probe.stdout.strip() if probe.returncode == 0 else None


def _refusal(free_bytes: int, estimated_bytes: int) -> str | None:
    if free_bytes < MIN_FREE_BYTES:
        return f"available space {free_bytes} is below the 10 GiB reserve"
    if free_bytes - estimated_bytes < MIN_FREE_BYTES:
        return f"projected remaining space {free_bytes - estimated_bytes} is below the 10 GiB reserve"
    return None


def _guarded_pip_install(command: list[str]) -> tuple[int, str]:
    process = subprocess.Popen(
        command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    interrupted_for_space = False
    while True:
        try:
            stdout, _ = process.communicate(timeout=2)
            break
        except subprocess.TimeoutExpired:
            if shutil.disk_usage(ROOT).free < MIN_FREE_BYTES + 512 * 1024**2:
                interrupted_for_space = True
                os.killpg(process.pid, signal.SIGINT)
                stdout, _ = process.communicate()
                break
    if interrupted_for_space:
        return 3, "pip install stopped because free space approached the 10 GiB reserve\n" + stdout[-4000:]
    return process.returncode, stdout[-4000:]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--python", type=Path, default=ROOT / ".venv/bin/python")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--preflight-record", type=Path, required=True)
    parser.add_argument("--build-record", type=Path, required=True)
    parser.add_argument("--estimated-install-bytes", type=int, default=1024**3)
    parser.add_argument("--estimated-build-bytes", type=int, default=2 * 1024**3)
    args = parser.parse_args(argv)
    if args.estimated_install_bytes < 0 or args.estimated_build_bytes < 0:
        parser.error("space estimates must be non-negative")
    output_path = args.output.resolve()
    preflight_path = args.preflight_record.resolve()
    build_path = args.build_record.resolve()
    # Keep the venv symlink path: resolving it selects Homebrew's managed base interpreter.
    python = Path(os.path.abspath(args.python))
    observed_version = _installed_version(python)
    install_needed = observed_version != PINNED_VERSION
    estimated_bytes = args.estimated_build_bytes + (args.estimated_install_bytes if install_needed else 0)
    free_bytes = shutil.disk_usage(ROOT).free
    reason = _refusal(free_bytes, estimated_bytes)
    record = {
        "schema_version": "poc0.nautilus-preflight.v1",
        "checked_at_utc": datetime.now(timezone.utc).isoformat(),
        "status": "unresolved" if reason else "preflight_passed",
        "reason": reason,
        "minimum_free_bytes": MIN_FREE_BYTES,
        "free_bytes_before": free_bytes,
        "estimated_install_bytes": args.estimated_install_bytes if install_needed else 0,
        "estimated_build_bytes": args.estimated_build_bytes,
        "projected_remaining_bytes": free_bytes - estimated_bytes,
        "estimate_basis": "1 GiB pinned wheel/install allowance; 2 GiB shared-target warm build allowance from ticket 13",
        "installed_version_before": observed_version,
        "required_version": PINNED_VERSION,
        "python": str(python),
        "build_command": ["cargo", "build", "-p", "quant-research", "--no-default-features", "--locked", "--offline", "--profile", "release"],
        "build_record": str(build_path),
        "report_output": str(output_path),
    }
    _write(preflight_path, record)
    if reason:
        print(reason, file=sys.stderr)
        return 3

    if install_needed:
        command = [str(python), "-m", "pip", "install", "--disable-pip-version-check", "--no-cache-dir", "-r", str(REQUIREMENTS)]
        code, detail = _guarded_pip_install(command)
        record["install_command"] = command
        record["install_exit_code"] = code
        record["install_output_tail"] = detail
        if code != 0 or _installed_version(python) != PINNED_VERSION:
            record.update(status="unresolved", reason="pinned Nautilus installation failed or version did not match")
            _write(preflight_path, record)
            return 3
    else:
        record["install_status"] = "skipped_exact_version_already_installed"

    build = subprocess.run(
        [str(python), str(BUILD_RECORDER), "--profile", "release", "--output", str(build_path),
         "--estimated-max-additional-bytes", str(args.estimated_build_bytes)],
        cwd=ROOT, text=True, capture_output=True, check=False,
    )
    record["build_exit_code"] = build.returncode
    record["build_output_tail"] = (build.stdout + build.stderr)[-4000:]
    if build.returncode != 0:
        record.update(status="unresolved", reason="guarded shared-target build did not complete")
        _write(preflight_path, record)
        return 3

    record.update(status="ready_to_run", free_bytes_after_build=shutil.disk_usage(ROOT).free)
    _write(preflight_path, record)
    command = [str(ROOT / "target/release/quant-research"), "benchmark-poc0", "--backend", "nautilus", "--python", str(python), "--output", str(output_path)]
    run = subprocess.run(command, cwd=ROOT, env={**os.environ, "NAUTILUS_POC_PREFLIGHT": str(preflight_path)}, check=False)
    record["benchmark_exit_code"] = run.returncode
    record["benchmark_command"] = command
    if run.returncode == 0:
        adapter_status = json.loads(output_path.read_text(encoding="utf-8"))["nautilus_adapter"]["status"]
        record["adapter_status"] = adapter_status
        record["status"] = "completed_with_semantic_unresolved" if adapter_status == "unresolved" else "completed"
    else:
        record["status"] = "unresolved"
        record["reason"] = "benchmark command failed; inspect the saved report and command output"
    _write(preflight_path, record)
    return run.returncode


if __name__ == "__main__":
    raise SystemExit(main())
