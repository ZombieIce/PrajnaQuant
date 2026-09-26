"""Capture one B1 scalar-access run with its input and build provenance."""

import argparse
import hashlib
import json
import pathlib
import platform
import subprocess


def checked_output(*command):
    return subprocess.check_output(command, text=True, stderr=subprocess.DEVNULL).strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=pathlib.Path)
    parser.add_argument("--symbols", type=int, default=128)
    parser.add_argument("--days", type=int, default=4096)
    parser.add_argument("--lookback", type=int, default=20)
    parser.add_argument("--repetitions", type=int, default=10)
    args = parser.parse_args()

    root = pathlib.Path(__file__).resolve().parents[2]
    crate = root / "poc/b1-layout"
    command = [
        "cargo", "run", "--manifest-path", str(crate / "Cargo.toml"),
        "--release", "--locked", "--offline", "--",
        str(args.symbols), str(args.days), str(args.lookback), str(args.repetitions),
    ]
    completed = subprocess.run(command, cwd=root, text=True, capture_output=True, check=True)
    rows = [json.loads(line) for line in completed.stdout.splitlines()]
    assert len(rows) == 4 and rows[0]["kind"] == "input"
    assert {row["candidate"] for row in rows[1:]} == {"soa", "arrow", "polars"}

    try:
        cpu_model = checked_output("sysctl", "-n", "machdep.cpu.brand_string")
    except (OSError, subprocess.CalledProcessError):
        cpu_model = None
    record = {
        "status": "smoke_only_no_architecture_decision",
        "scope": "scalar nullable close access and momentum; no Polars expressions, Parquet IO, ranking, portfolio, or out-of-core",
        "command": command,
        "git_revision": checked_output("git", "-C", str(root), "rev-parse", "HEAD"),
        "worktree_dirty": bool(checked_output("git", "-C", str(root), "status", "--porcelain")),
        "source_sha256": hashlib.sha256((crate / "src/main.rs").read_bytes()).hexdigest(),
        "lock_sha256": hashlib.sha256((crate / "Cargo.lock").read_bytes()).hexdigest(),
        "capture_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
        "rustc": checked_output("rustc", "--version"),
        "cargo": checked_output("cargo", "--version"),
        "platform": platform.platform(),
        "cpu_arch": platform.machine(),
        "cpu_model": cpu_model,
        "compiler_stderr": completed.stderr.splitlines()[-3:],
        "measurements": rows,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")
    print(args.output)


if __name__ == "__main__":
    main()
