#!/usr/bin/env python3
"""Verify the ADR 0013 Nautilus engine-reset mode against four fixed workloads."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).parent))
from nautilus_adapter import _ma_dataset, verify_reset_parity  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--golden-checksums", type=Path, help="JSON mapping decision workload names to independent checksum SHA-256 values")
    parser.add_argument("--output", type=Path, default=ROOT / "poc/poc0-benchmark/results/b2-nautilus-reset-parity.json")
    args = parser.parse_args()
    checksums = {}
    if args.golden_checksums is not None:
        path = args.golden_checksums if args.golden_checksums.is_absolute() else ROOT / args.golden_checksums
        checksums = json.loads(path.read_text(encoding="utf-8"))

    fixtures = ROOT / "poc/poc0-benchmark/fixtures"
    workloads = [
        {
            "name": "s2_decision",
            "strategy": "s2",
            "dataset": json.loads((fixtures / "dataset-v1.json").read_text(encoding="utf-8")),
            "golden_checksum_sha256": checksums.get("s2_decision"),
        },
        {
            "name": "s3_decision",
            "strategy": "s3",
            "dataset": _ma_dataset(json.loads((fixtures / "b2-ma20-60-v1.json").read_text(encoding="utf-8"))),
            "golden_checksum_sha256": checksums.get("s3_decision"),
        },
        {
            "name": "s2_robustness",
            "strategy": "s2",
            "dataset": json.loads((fixtures / "b2-s2-scale-64x252-v1.json").read_text(encoding="utf-8")),
            "golden_checksum_sha256": checksums.get("s2_robustness"),
            "golden_required": False,
        },
        {
            "name": "s3_robustness",
            "strategy": "s3",
            "dataset": json.loads((fixtures / "b2-s3-scale-64x252-v1.json").read_text(encoding="utf-8")),
            "golden_checksum_sha256": checksums.get("s3_robustness"),
            "golden_required": False,
        },
    ]
    report = verify_reset_parity(workloads)
    if args.golden_checksums is not None:
        report["golden_checksum_source"] = str(args.golden_checksums)
    output = args.output if args.output.is_absolute() else ROOT / args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"reset_parity": report["reset_parity"], "selected_mode": report["selected_mode"], "output": str(output)}))
    return 0 if report["reset_parity"] in {"passed", "failed", "unresolved"} else 2


if __name__ == "__main__":
    raise SystemExit(main())
