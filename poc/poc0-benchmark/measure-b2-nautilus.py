#!/usr/bin/env python3
"""Isolated Nautilus S2 serial or two-worker measurement process."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).parent))
from nautilus_adapter import (  # noqa: E402
    PINNED_NAUTILUS_VERSION,
    _version_probe,
    parallel_digest_runs,
    serial_digest_runs,
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("serial", "parallel"), required=True)
    parser.add_argument("--dataset", type=Path, default=ROOT / "poc/poc0-benchmark/fixtures/dataset-v1.json")
    parser.add_argument("--expected-checksum", required=True, help="checksum of the separately checked Nautilus golden projection")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--runs", type=int)
    args = parser.parse_args()
    version, error = _version_probe()
    if error:
        report = {"status": "unresolved", "reason": error, "nautilus_required": PINNED_NAUTILUS_VERSION, "nautilus_observed": version}
    else:
        dataset_path = args.dataset if args.dataset.is_absolute() else ROOT / args.dataset
        dataset = json.loads(dataset_path.read_text(encoding="utf-8"))
        runs = args.runs if args.runs is not None else (20 if args.mode == "serial" else 6)
        if args.mode == "serial":
            report = serial_digest_runs(dataset, "s2", args.expected_checksum, runs=runs)
        else:
            report = parallel_digest_runs(
                dataset,
                "s2",
                args.expected_checksum,
                workers=args.workers,
                runs=runs,
            )
    output = args.output if args.output.is_absolute() else ROOT / args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": report["status"], "mode": args.mode, "output": str(output)}))
    return 0 if report["status"] == "passed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
