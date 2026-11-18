#!/usr/bin/env python3
"""Run versioned Eternal compatibility configurations against a frozen snapshot."""

import argparse
import hashlib
import json
import math
import os
import platform
import sys
from datetime import date
from pathlib import Path

import eternal_baseline as baseline


RUNNER_VERSION = "eternal_compat@1"
DEFAULT_CONFIG = {
    "initial_nav": 1.0,
    "weights": baseline.WEIGHTS,
    "rebalance_threshold": baseline.THRESHOLD,
    "fee_rate_one_way": baseline.FEE_RATE,
}


class RunnerError(Exception):
    def __init__(self, code, message, details=None):
        super().__init__(message)
        self.code = code
        self.details = details or {}


def canonical_json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def sha256_bytes(value):
    return hashlib.sha256(value).hexdigest()


def sha256_file(path):
    return baseline.sha256_file(path)


def validate_config(value):
    if not isinstance(value, dict):
        raise RunnerError("invalid_config", "config must be a JSON object")
    unknown = set(value) - set(DEFAULT_CONFIG)
    if unknown:
        raise RunnerError("invalid_config", f"unknown config field: {sorted(unknown)[0]}")

    config = dict(DEFAULT_CONFIG)
    config["weights"] = dict(DEFAULT_CONFIG["weights"])
    config.update(value)
    if "weights" in value:
        weights = value["weights"]
        if not isinstance(weights, dict):
            raise RunnerError("invalid_config", "weights must be an object with the five fixed assets")
        if set(weights) != set(baseline.ETF_CODES):
            raise RunnerError("invalid_config", "weights must contain exactly the five fixed assets")
        config["weights"] = weights

    def finite_number(field, minimum, maximum=None):
        number = config[field]
        if isinstance(number, bool) or not isinstance(number, (int, float)) or not math.isfinite(number):
            raise RunnerError("invalid_config", f"{field} must be a finite number")
        if number < minimum or (maximum is not None and number > maximum):
            bound = f"{minimum}..{maximum}" if maximum is not None else f">={minimum}"
            raise RunnerError("invalid_config", f"{field} must be {bound}")
        config[field] = float(number)

    finite_number("initial_nav", 0.0)
    if config["initial_nav"] <= 0:
        raise RunnerError("invalid_config", "initial_nav must be greater than zero model units")
    finite_number("rebalance_threshold", 0.0, 1.0)
    finite_number("fee_rate_one_way", 0.0, 0.999999999)

    weights = config["weights"]
    if any(
        isinstance(weight, bool)
        or not isinstance(weight, (int, float))
        or not math.isfinite(weight)
        or weight < 0
        for weight in weights.values()
    ):
        raise RunnerError("invalid_config", "each weight must be a finite non-negative number")
    if not math.isclose(sum(weights.values()), 1.0, rel_tol=0.0, abs_tol=1e-9):
        raise RunnerError("invalid_config", "weights must sum to 1.0")
    config["weights"] = {code: float(weights[code]) for code in baseline.ETF_CODES}
    return config


def _scaled_result(navs, signals, trades, stats, daily, initial_nav):
    scaled_navs = [value * initial_nav for value in navs]
    scaled_trades = [
        {
            **trade,
            "fees_in_initial_nav": trade["fees_in_initial_nav"] * initial_nav,
        }
        for trade in trades
    ]
    scaled_daily = []
    for row in daily:
        scaled_daily.append({
            **row,
            "nav": row["nav"] * initial_nav,
            "cash": row["cash"] * initial_nav,
            "fees": row["fees"] * initial_nav,
            "units": {key: value * initial_nav for key, value in row["units"].items()},
            "holdings": {key: value * initial_nav for key, value in row["holdings"].items()},
        })
    scaled_stats = {
        **stats,
        "total_return": scaled_navs[-1] / initial_nav - 1.0,
        "fees_in_initial_nav": stats["fees_in_initial_nav"] * initial_nav,
    }
    return scaled_navs, signals, scaled_trades, scaled_stats, scaled_daily


def _load_simulation(snapshot):
    try:
        manifest, reference = baseline.read_snapshot(snapshot)
        baseline.validate_execution_configuration(manifest)
        baseline.validate_replay_source(manifest, Path(baseline.__file__).resolve())
        if (snapshot / "pending-gaps.json").exists():
            raise RunnerError("compat_input_unavailable", "snapshot has unresolved input gaps")

        bars, factors, prices, limits = {}, {}, {}, {}
        for code in baseline.HISTORY_CODES:
            bars[code] = baseline.csv_rows(snapshot / f"bars/{code}.csv.gz")[0]
            factors[code] = baseline.csv_rows(snapshot / f"factors/{code}.csv.gz")[0]
            baseline.ensure_ordered_bars(code, bars[code])
            prices[code], actions = baseline.total_return_prices(bars[code], factors[code])
            limits[code] = baseline.opening_limits(bars[code], actions, baseline.Decimal(".10"))
        anchors = [
            baseline.splice_etf(prices, limits, old, new)
            for old, new in baseline.LEGACY_PAIRS
        ]
        common = set.intersection(*(set(prices[code]) for code in baseline.ETF_CODES))
        reference_rows = reference["history_etf"]
        old_dates = [row[0] for row in reference_rows if row[0] <= baseline.ANCHOR]
        days = (
            [date.fromisoformat(day) for day in old_dates if day in common]
            + [date.fromisoformat(day) for day in sorted(common) if day > baseline.ANCHOR]
        )
        if len(days) < 3:
            raise RunnerError("compat_input_invalid", "common trading sessions contain fewer than three dates")
        opens = [[prices[code][day.isoformat()][0] for code in baseline.ETF_CODES] for day in days]
        closes = [[prices[code][day.isoformat()][1] for code in baseline.ETF_CODES] for day in days]
        day_limits = [
            [limits[code].get(day.isoformat(), (False, False)) for code in baseline.ETF_CODES]
            for day in days
        ]
        return manifest, reference, anchors, days, opens, closes, day_limits
    except RunnerError:
        raise
    except OSError as error:
        raise RunnerError("compat_input_unavailable", f"frozen snapshot is unavailable: {error}") from error
    except (KeyError, TypeError, ValueError, ArithmeticError, json.JSONDecodeError) as error:
        raise RunnerError("compat_input_invalid", f"frozen snapshot is unavailable or invalid: {error}") from error


def run(request, output):
    if not isinstance(request, dict):
        raise RunnerError("invalid_request", "runner request must be a JSON object")
    task_id = request.get("task_id")
    snapshot_value = request.get("snapshot_path")
    if not isinstance(task_id, str) or not task_id:
        raise RunnerError("invalid_request", "task_id is required")
    if not isinstance(snapshot_value, str) or not snapshot_value:
        raise RunnerError("invalid_request", "snapshot_path is required")
    snapshot = Path(snapshot_value).expanduser()
    if not snapshot.is_absolute():
        raise RunnerError("invalid_request", "snapshot_path must be absolute")
    output = Path(output).resolve()
    if output.exists():
        raise RunnerError("compat_output_exists", "compatibility result output already exists")

    config = validate_config(request.get("config", {}))
    manifest, reference, anchors, days, opens, closes, day_limits = _load_simulation(snapshot)
    navs, signals, trades, stats, daily = baseline.simulate(
        days,
        opens,
        closes,
        day_limits,
        config["rebalance_threshold"],
        config["fee_rate_one_way"],
        config["weights"],
    )
    navs, signals, trades, stats, daily = _scaled_result(
        navs, signals, trades, stats, daily, config["initial_nav"],
    )
    comparison = baseline.compare_reference(reference, days, navs, stats, anchors)
    is_default_config = config == validate_config({})
    comparison["evaluation_mode"] = (
        "required_default_parity" if is_default_config else "informational_variant"
    )
    if is_default_config and comparison["overall_status"] != "match":
        raise RunnerError(
            "compat_default_reference_mismatch",
            "frozen-reference comparison mismatch for the effective default configuration",
            {
                "comparison": {
                    "overall_status": comparison["overall_status"],
                    "dates": comparison["dates"]["status"],
                    "daily_nav": comparison["daily_nav"]["status"],
                }
            },
        )
    runner_sha256 = sha256_file(Path(__file__).resolve())
    baseline_sha256 = sha256_file(Path(baseline.__file__).resolve())
    identity_material = {
        "task_id": task_id,
        "runner": RUNNER_VERSION,
        "runner_sha256": runner_sha256,
        "baseline_sha256": baseline_sha256,
        "input_version": manifest["input_version"],
        "config": config,
    }
    compat_run_id = sha256_bytes(canonical_json(identity_material))
    result = {
        "schema_version": 1,
        "identity": {
            "compat_run_id": compat_run_id,
            "task_type": RUNNER_VERSION,
            "task_id": task_id,
            "strategy_version": RUNNER_VERSION,
            "runner_sha256": runner_sha256,
            "frozen_replay_source_sha256": baseline_sha256,
            "input_version": manifest["input_version"],
            "input_manifest_sha256": manifest["manifest_sha256"],
        },
        "effective_config": {
            **config,
            "initial_capital_currency_amount": None,
            "initial_capital_unit": "model_nav",
            "initial_capital_note": "absolute CNY amount is unavailable",
        },
        "runtime": {"python": sys.version, "platform": platform.platform()},
        "summary": {
            **stats,
            "nasdaqAnchor": anchors[0][0],
            "nasdaqSwitch": anchors[0][1],
            "sp500Anchor": anchors[1][0],
            "sp500Switch": anchors[1][1],
        },
        "daily": daily,
        "signals": signals,
        "trades": trades,
        "frozen_reference_comparison": comparison,
        "risk_disclosures": {
            "historical_universe": "unverified; fixed/proxy assets may introduce survivorship or selection bias",
            "total_return_pit": "unverified; historical listing, distributions and factor availability are not proven point-in-time",
        },
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(
        result,
        ensure_ascii=False,
        indent=2,
        allow_nan=False,
        default=baseline.json_default,
    ).encode() + b"\n"
    with output.open("xb") as stream:
        stream.write(payload)
        stream.flush()
        os.fsync(stream.fileno())
    return {"compat_run_id": compat_run_id, "result_sha256": sha256_bytes(payload)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run_parser = commands.add_parser("run")
    run_parser.add_argument("--request", type=Path, required=True)
    run_parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        request = json.loads(args.request.read_text(encoding="utf-8"))
        print(json.dumps(run(request, args.output), separators=(",", ":")))
        return 0
    except RunnerError as error:
        details = {"code": error.code, "message": str(error)}
        if error.details:
            details["details"] = error.details
        print(json.dumps({"error": details}), file=sys.stderr)
        return 2
    except OSError as error:
        wrapped = RunnerError("compat_input_unavailable", f"runner input or output is unavailable: {error}")
        print(json.dumps({"error": {"code": wrapped.code, "message": str(wrapped)}}), file=sys.stderr)
        return 2
    except Exception as error:
        wrapped = RunnerError("compat_runner_failed", str(error))
        print(json.dumps({"error": {"code": wrapped.code, "message": str(wrapped)}}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
