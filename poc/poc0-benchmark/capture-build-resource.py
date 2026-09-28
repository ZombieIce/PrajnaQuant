#!/usr/bin/env python3
"""Run one warm-cache POC build and record its time and disk footprint."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone


ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGET = ROOT / "target"
MIN_FREE_BYTES = 10 * 1024**3


def output(command: list[str], *, cwd: pathlib.Path = ROOT) -> str:
    return subprocess.run(command, cwd=cwd, check=True, text=True, capture_output=True).stdout.strip()


def tree_size(path: pathlib.Path) -> int:
    total = 0
    for directory, _, files in os.walk(path):
        for name in files:
            try:
                total += (pathlib.Path(directory) / name).stat().st_size
            except FileNotFoundError:
                pass
    return total


def snapshot() -> dict[str, int]:
    usage = shutil.disk_usage(ROOT)
    return {
        "volume_free_bytes": usage.free,
        "workspace_target_bytes": tree_size(TARGET),
    }


def write_record(path: pathlib.Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(record, indent=2) + "\n")


def compile_source_identity() -> dict:
    paths = [
        ROOT / "Cargo.toml",
        ROOT / "Cargo.lock",
        ROOT / ".cargo/config.toml",
        ROOT / "crates/quant-research/Cargo.toml",
    ]
    paths.extend(sorted((ROOT / "crates/quant-research/src").rglob("*.rs")))
    paths.extend(sorted((ROOT / "crates/quant-research").glob("build.rs")))
    files = []
    for path in sorted(set(paths)):
        files.append(
            {
                "path": str(path.relative_to(ROOT)),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        )
    manifest = json.dumps(files, sort_keys=True).encode()
    return {"compiled_source_files": files, "compiled_source_sha256": hashlib.sha256(manifest).hexdigest()}


def working_tree_identity() -> dict:
    diff = output(["git", "diff", "HEAD", "--binary"])
    changed = output(["git", "status", "--porcelain", "--untracked-files=all"])
    untracked_paths = output(["git", "ls-files", "--others", "--exclude-standard"]).splitlines()
    untracked = []
    for relative_path in untracked_paths:
        path = ROOT / relative_path
        content_hash = hashlib.sha256(path.read_bytes()).hexdigest()
        untracked.append({"path": relative_path, "sha256": content_hash})
    diff_hash = hashlib.sha256(diff.encode()).hexdigest()
    untracked_manifest = json.dumps(untracked, sort_keys=True).encode()
    return {
        "tracked_diff_sha256": diff_hash,
        "tracked_diff_summary": output(["git", "diff", "HEAD", "--stat"]),
        "changed_paths": [line[3:] for line in changed.splitlines()],
        "untracked_files": untracked,
        "untracked_files_complete": True,
        "dirty_worktree_sha256": hashlib.sha256(diff.encode() + untracked_manifest).hexdigest(),
    }


def cargo_lock_observation() -> dict:
    lsof = shutil.which("lsof")
    if not lsof:
        return {"available": False, "tool": "lsof", "output": "not installed"}
    result = subprocess.run(
        [lsof, str(TARGET / "debug/.cargo-lock"), str(TARGET / "release/.cargo-lock")],
        text=True,
        capture_output=True,
        check=False,
    )
    return {
        "available": True,
        "tool": lsof,
        "exit_code": result.returncode,
        "output": result.stdout.strip(),
        "stderr": result.stderr.strip(),
    }


def python_environment_observation() -> dict:
    interpreter = os.environ.get("PYO3_PYTHON")
    if not interpreter:
        return {"status": "not_configured", "occupancy_bytes": None}
    try:
        identity = json.loads(
            output(
                [
                    interpreter,
                    "-c",
                    "import json,sys; print(json.dumps({'executable':sys.executable,'version':sys.version,'prefix':sys.prefix}))",
                ]
            )
        )
        identity["status"] = "observed"
        identity["occupancy_bytes"] = tree_size(pathlib.Path(identity["prefix"]))
        return identity
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        return {"status": "unavailable", "occupancy_bytes": None, "error": str(error)}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", choices=("dev", "release"), required=True)
    parser.add_argument("--features", choices=("b3-pyo3",))
    parser.add_argument("--action", choices=("build", "test", "clippy"), default="build")
    parser.add_argument("--scope", choices=("poc", "workspace"), default="poc")
    parser.add_argument(
        "--test-target", choices=("poc0_b3_callbacks", "b3_strategy_parity")
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument(
        "--estimated-max-additional-bytes",
        type=int,
        default=2 * 1024**3,
        help="Conservative upper bound used by the 10 GiB free-space gate.",
    )
    parser.add_argument("--poll-interval-seconds", type=float, default=2.0)
    args = parser.parse_args()

    if args.scope == "workspace":
        if args.features or args.action == "build":
            parser.error("workspace scope supports test/clippy without crate-specific features")
        command = ["cargo", args.action, "--workspace", "--locked", "--offline"]
        if args.action == "clippy":
            command.extend(["--all-targets", "--", "-D", "warnings"])
        elif args.test_target:
            parser.error("--test-target is not valid for workspace tests")
    else:
        command = ["cargo", args.action]
        if args.action == "clippy":
            command.extend(
                ["-p", "quant-research", "--no-default-features", "--lib", "--bin", "quant-research"]
            )
        else:
            command.extend(["-p", "quant-research", "--no-default-features"])
        command.extend(["--locked", "--offline"])
        if args.features:
            command.extend(["--features", args.features])
            if args.action == "clippy":
                command.extend(["--test", "poc0_b3_callbacks"])
        if args.action == "test":
            if not args.test_target:
                parser.error("--action test requires --test-target")
            if args.test_target == "b3_strategy_parity":
                command.extend(
                    [
                        "--lib",
                        "s2_s3_python_callbacks_match_fixed_golden_signals_and_accounts",
                    ]
                )
            else:
                command.extend(["--test", args.test_target])
        elif args.test_target:
            parser.error("--test-target is only valid with --action test")
        if args.action == "build":
            command.extend(["--profile", args.profile])
        elif args.action == "clippy":
            command.extend(["--", "-D", "warnings"])
    before = snapshot()
    python_environment = python_environment_observation()
    lock_observation = cargo_lock_observation()
    refusal = None
    if before["volume_free_bytes"] < MIN_FREE_BYTES:
        refusal = f"free space {before['volume_free_bytes']} is below 10 GiB"
    elif before["volume_free_bytes"] - args.estimated_max_additional_bytes < MIN_FREE_BYTES:
        refusal = "estimated completion would leave less than 10 GiB free"
    if refusal:
        write_record(
            args.output,
            {
                "schema_version": 1,
                "status": "blocked_by_space_gate",
                "command": command,
                "working_directory": str(ROOT),
                "target_directory": str(TARGET),
                "estimated_max_additional_bytes": args.estimated_max_additional_bytes,
                "minimum_free_bytes": MIN_FREE_BYTES,
                "before": before,
                "cargo_lock_observation": lock_observation,
                "reason": refusal,
                "finished_at_utc": datetime.now(timezone.utc).isoformat(),
            },
        )
        print(refusal, file=sys.stderr)
        return 3

    if args.scope == "workspace":
        tree_command = ["cargo", "tree", "--workspace", "--locked", "--offline", "-e", "normal"]
    else:
        tree_command = [
            "cargo",
            "tree",
            "-p",
            "quant-research",
            "--no-default-features",
            "--locked",
            "--offline",
            "-e",
            "normal",
        ]
        if args.features:
            tree_command.extend(["--features", args.features])
    dependency_graph = output(tree_command)
    forbidden = ("duckdb", "libduckdb-sys", "ashare-warehouse")
    if args.scope == "poc" and any(name in dependency_graph for name in forbidden):
        raise SystemExit("refusing build: POC dependency graph contains a forbidden package")

    started_at = datetime.now(timezone.utc).isoformat()
    worktree = working_tree_identity()
    source_identity = compile_source_identity()
    started = time.perf_counter()
    with tempfile.TemporaryFile(mode="w+t") as stdout_file, tempfile.TemporaryFile(
        mode="w+t"
    ) as stderr_file:
        process = subprocess.Popen(
            command,
            cwd=ROOT,
            env={**os.environ, "CARGO_TARGET_DIR": str(TARGET)},
            text=True,
            stdout=stdout_file,
            stderr=stderr_file,
            start_new_session=True,
        )
        interrupted = False
        reserve_margin = min(args.estimated_max_additional_bytes, 512 * 1024**2)
        space_samples = []
        storage_guard_triggered = False
        while process.poll() is None:
            try:
                process.wait(timeout=args.poll_interval_seconds)
            except subprocess.TimeoutExpired:
                free_bytes = shutil.disk_usage(ROOT).free
                space_samples.append(
                    {
                        "observed_at_utc": datetime.now(timezone.utc).isoformat(),
                        "volume_free_bytes": free_bytes,
                    }
                )
                if free_bytes < MIN_FREE_BYTES + reserve_margin:
                    storage_guard_triggered = True
                    os.killpg(process.pid, signal.SIGINT)
                    process.wait()
            except KeyboardInterrupt:
                interrupted = True
                os.killpg(process.pid, signal.SIGINT)
                process.wait()
        elapsed = time.perf_counter() - started
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()

    after = snapshot()
    lock_hash = hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest()
    graph_hash = hashlib.sha256(dependency_graph.encode()).hexdigest()
    status = (
        "interrupted_by_space_guard"
        if storage_guard_triggered
        else "interrupted_by_user"
        if interrupted
        else "completed"
        if process.returncode == 0
        else "failed"
    )
    record = {
        "schema_version": 1,
        "started_at_utc": started_at,
        "finished_at_utc": datetime.now(timezone.utc).isoformat(),
        "command": command,
        "working_directory": str(ROOT),
        "target_directory": str(TARGET),
        "cache_state": "warm_existing_workspace_target",
        "estimated_max_additional_bytes": args.estimated_max_additional_bytes,
        "minimum_free_bytes": MIN_FREE_BYTES,
        "runtime_stop_free_bytes": MIN_FREE_BYTES + reserve_margin,
        "space_samples": space_samples,
        "git_revision": output(["git", "rev-parse", "HEAD"]),
        "working_tree_dirty": bool(output(["git", "status", "--porcelain"])),
        **worktree,
        **source_identity,
        "cargo_lock_observation": lock_observation,
        "python_environment": python_environment,
        "cargo_version": output(["cargo", "--version"]),
        "rustc_version": output(["rustc", "--version"]),
        "compiler_environment": {
            "CARGO_TARGET_DIR": str(TARGET),
            "PYO3_PYTHON": os.environ.get("PYO3_PYTHON"),
            "RUSTFLAGS": os.environ.get("RUSTFLAGS"),
            "CARGO_ENCODED_RUSTFLAGS": os.environ.get("CARGO_ENCODED_RUSTFLAGS"),
        },
        "cargo_lock_sha256": lock_hash,
        "dependency_graph_sha256": graph_hash,
        "dependency_graph_contains_duckdb": "duckdb" in dependency_graph,
        "profile": args.profile,
        "action": args.action,
        "scope": args.scope,
        "features": (
            ["workspace default features"]
            if args.scope == "workspace"
            else ["poc0 benchmark path; default app features disabled"]
            + ([args.features] if args.features else [])
        ),
        "build_wall_seconds": elapsed,
        "status": status,
        "exit_code": process.returncode,
        "before": before,
        "after": after,
        "target_delta_bytes": after["workspace_target_bytes"] - before["workspace_target_bytes"],
        "stdout": stdout[-12000:],
        "stderr": stderr[-12000:],
    }
    write_record(args.output, record)
    print(
        json.dumps(
            {
                key: record[key]
                for key in (
                    "profile",
                    "build_wall_seconds",
                    "status",
                    "exit_code",
                    "before",
                    "after",
                    "target_delta_bytes",
                )
            },
            indent=2,
        )
    )
    return process.returncode


if __name__ == "__main__":
    sys.exit(main())
