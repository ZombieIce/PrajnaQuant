#!/usr/bin/env python3
"""Capture the peak RSS of one correctness-gated Rust B2 throughput process."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import resource
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--strategy", choices=("s2", "s3"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/quant-research")
    args = parser.parse_args()
    output = args.output if args.output.is_absolute() else ROOT / args.output
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    command = [str(binary), "benchmark-poc0-b2", "--strategy", args.strategy,
               "--threads", "2", "--runs", "6", "--output", str(output)]
    completed = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    peak_bytes = int(peak if platform.system() == "Darwin" else peak * 1024)
    if completed.returncode != 0:
        print(completed.stderr)
        return completed.returncode
    record = json.loads(output.read_text(encoding="utf-8"))
    record["peak_rss_bytes"] = peak_bytes
    record["memory_status"] = "whole Rust child process peak RSS (includes golden precheck)"
    record["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    output.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"strategy": args.strategy, "peak_rss_bytes": peak_bytes,
                      "parallel_runs_per_second": record["parallel_runs_per_second"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())