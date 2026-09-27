#!/usr/bin/env python3
"""Measure a built POC-0 Parquet command in a separate process.

Build first; this script measures only the runtime child. On macOS ru_maxrss is
bytes; on Linux it is KiB. The optional address-space limit is an experiment,
not a claim about physical RAM availability.
"""
import argparse
import json
import platform
import resource
import subprocess
import time
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/quant-research"))
    parser.add_argument("--parquet", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--symbol", action="append", required=True)
    parser.add_argument("--start")
    parser.add_argument("--end")
    parser.add_argument("--limit-mib", type=int)
    args = parser.parse_args()
    command = [str(args.binary.resolve()), "benchmark-poc0-parquet", "--reuse",
               "--parquet", str(args.parquet), "--output", str(args.output)]
    for symbol in args.symbol:
        command += ["--symbol", symbol]
    if args.start:
        command += ["--start", args.start]
    if args.end:
        command += ["--end", args.end]

    def apply_limit() -> None:
        if args.limit_mib:
            limit = args.limit_mib * 1024**2
            resource.setrlimit(resource.RLIMIT_AS, (limit, limit))

    start = time.monotonic_ns()
    launch_error = None
    try:
        result = subprocess.run(command, capture_output=True, text=True,
                                preexec_fn=apply_limit if args.limit_mib else None)
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        launch_error = str(exc)
        result = None
    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    peak_bytes = peak if platform.system() == "Darwin" else peak * 1024
    record = {
        "status": "passed" if result and result.returncode == 0 else "unresolved",
        "command": command,
        "exit_code": result.returncode if result else None,
        "launch_error": launch_error,
        "wall_ns": time.monotonic_ns() - start,
        "peak_rss_bytes": peak_bytes,
        "address_space_limit_bytes": args.limit_mib * 1024**2 if args.limit_mib else None,
        "source_bytes": args.parquet.stat().st_size,
        "stdout": result.stdout if result else "",
        "stderr": result.stderr if result else "",
    }
    record_path = args.output.with_suffix(".rss.json")
    record_path.write_text(json.dumps(record, indent=2) + "\n")
    print(record_path)
    return result.returncode if result else 1


if __name__ == "__main__":
    raise SystemExit(main())
