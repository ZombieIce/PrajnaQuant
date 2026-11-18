#!/usr/bin/env python3
"""Run versioned Eternal compatibility configurations against a frozen snapshot."""

import argparse
import hashlib
import json
import math
import os
import platform
import stat
import sys
from datetime import date
from decimal import Decimal, InvalidOperation
from pathlib import Path

import eternal_baseline as baseline


RUNNER_VERSION = "eternal_compat@1"
DEFAULT_CONFIG = {
    "initial_nav": 1.0,
    "weights": baseline.WEIGHTS,
    "rebalance_threshold": baseline.THRESHOLD,
    "fee_rate_one_way": baseline.FEE_RATE,
}
PENDING_GAP_CHECKS = (
    "snapshot_manifest_validation",
    "snapshot_file_inventory_validation",
    "snapshot_configuration_validation",
    "frozen_source_validation",
    "bar_and_factor_validation",
    "total_return_price_and_limit_construction",
    "historical_overlap_and_splice_validation",
    "common_session_validation",
    "simulation",
    "frozen_reference_comparison",
)


class RunnerError(Exception):
    def __init__(self, code, message, details=None):
        super().__init__(message)
        self.code = code
        self.details = details or {}


class NonRepresentableJsonNumber(ValueError):
    pass


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

    def finite_number(field, minimum=None, maximum=None):
        number = config[field]
        if isinstance(number, bool) or not isinstance(number, (int, float)) or not math.isfinite(number):
            raise RunnerError("invalid_config", f"{field} must be a finite number")
        if (minimum is not None and number < minimum) or (
            maximum is not None and number > maximum
        ):
            bound = (
                f"{minimum}..{maximum}"
                if minimum is not None and maximum is not None
                else f">={minimum}" if minimum is not None else f"<={maximum}"
            )
            raise RunnerError("invalid_config", f"{field} must be {bound}")
        config[field] = float(number)

    finite_number("initial_nav")
    if config["initial_nav"] <= 0:
        raise RunnerError("invalid_config", "initial_nav must be greater than zero model units")
    if config["initial_nav"] < sys.float_info.min:
        raise RunnerError(
            "invalid_config",
            "initial_nav must be at least the minimum normal positive f64 value",
        )
    finite_number("rebalance_threshold", 0.0, 1.0)
    finite_number("fee_rate_one_way", 0.0)
    if config["fee_rate_one_way"] >= 1.0:
        raise RunnerError("invalid_config", "fee_rate_one_way must be less than 1.0")

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
    def scale(value, field, asset=None):
        scaled = value * initial_nav
        details = {"field": field}
        if asset is not None:
            details["asset"] = asset
        if not math.isfinite(scaled):
            raise RunnerError(
                "compat_scaled_value_unrepresentable",
                "scaled compatibility result value is not representable",
                details,
            )
        if value > 0 and scaled == 0:
            raise RunnerError(
                "compat_scaled_value_underflow",
                "positive compatibility result value underflowed during capital scaling",
                details,
            )
        return scaled

    scaled_navs = [scale(value, "daily.nav") for value in navs]
    scaled_trades = [
        {
            **trade,
            "fees_in_initial_nav": scale(
                trade["fees_in_initial_nav"],
                "trades.fees_in_initial_nav",
            ),
        }
        for trade in trades
    ]
    scaled_daily = []
    for row in daily:
        scaled_daily.append({
            **row,
            "nav": scale(row["nav"], "daily.nav"),
            "cash": scale(row["cash"], "daily.cash"),
            "fees": scale(row["fees"], "daily.fees"),
            "units": {
                key: scale(value, "daily.units", key)
                for key, value in row["units"].items()
            },
            "holdings": {
                key: scale(value, "daily.holdings", key)
                for key, value in row["holdings"].items()
            },
        })
    scaled_stats = {
        **stats,
        "fees_in_initial_nav": scale(
            stats["fees_in_initial_nav"],
            "summary.fees_in_initial_nav",
        ),
    }
    return scaled_navs, signals, scaled_trades, scaled_stats, scaled_daily


def _pending_gap_error(code, message, report_status):
    reason = "blocked by the pending-gap report"
    return RunnerError(
        code,
        message,
        {
            "report_file": "pending-gaps.json",
            "report_status": report_status,
            "checks_not_run": [
                {"name": name, "status": "not_run", "reason": reason}
                for name in PENDING_GAP_CHECKS
            ],
        },
    )


def _unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON object key: {key}")
        result[key] = value
    return result


def _reject_json_constant(value):
    raise ValueError(f"non-standard JSON constant: {value}")


def _parse_compatible_json_int(value):
    number = int(value)
    if number < -(2**63) or number > 2**64 - 1:
        raise NonRepresentableJsonNumber
    return number


def _parse_compatible_json_float(value):
    try:
        number = float(value)
        exact = Decimal(value)
    except (InvalidOperation, OverflowError, ValueError) as error:
        raise NonRepresentableJsonNumber from error
    if not math.isfinite(number) or (number == 0 and not exact.is_zero()):
        raise NonRepresentableJsonNumber
    return number


def _read_pending_gap_report(snapshot):
    path = snapshot / "pending-gaps.json"
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise _pending_gap_error(
            "compat_pending_gaps_unreadable",
            "pending gap report could not be read",
            "unreadable",
        ) from error

    if not stat.S_ISREG(metadata.st_mode):
        raise _pending_gap_error(
            "compat_pending_gaps_unsupported",
            "pending gap report must be a regular file",
            "unsupported_file_type",
        )

    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise _pending_gap_error(
            "compat_pending_gaps_unreadable",
            "pending gap report could not be read",
            "unreadable",
        ) from error

    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise _pending_gap_error(
                "compat_pending_gaps_unsupported",
                "pending gap report must be a regular file",
                "unsupported_file_type",
            )
        with os.fdopen(descriptor, "rb") as stream:
            descriptor = None
            payload = stream.read()
    except OSError as error:
        raise _pending_gap_error(
            "compat_pending_gaps_unreadable",
            "pending gap report could not be read",
            "unreadable",
        ) from error
    finally:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass

    try:
        report = json.loads(
            payload.decode("utf-8"),
            object_pairs_hook=_unique_json_object,
            parse_constant=_reject_json_constant,
            parse_int=_parse_compatible_json_int,
            parse_float=_parse_compatible_json_float,
        )
    except NonRepresentableJsonNumber as error:
        failure = _pending_gap_error(
            "compat_pending_gaps_invalid",
            "pending gap report contains a non-representable number",
            "non_representable_number",
        )
        failure.details["report_contents_persisted"] = False
        failure.details["report_contents_not_persisted_reason"] = (
            "report contains a number outside the portable JSON numeric range"
        )
        raise failure from error
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError, RecursionError) as error:
        failure = _pending_gap_error(
            "compat_pending_gaps_invalid",
            "pending gap report is invalid JSON",
            "invalid",
        )
        failure.details["report_contents_persisted"] = False
        failure.details["report_contents_not_persisted_reason"] = (
            "report is not valid standard JSON"
        )
        raise failure from error

    if (
        not isinstance(report, dict)
        or report.get("status") != "pending_decision"
        or not isinstance(report.get("gaps"), list)
        or not report["gaps"]
        or any(not isinstance(gap, dict) for gap in report["gaps"])
    ):
        failure = _pending_gap_error(
            "compat_pending_gaps_invalid",
            "pending gap report has an invalid structure",
            "invalid",
        )
        failure.details["report_contents_persisted"] = False
        failure.details["report_contents_not_persisted_reason"] = (
            "report structure is invalid"
        )
        raise failure

    return report


def _load_simulation(snapshot):
    try:
        pending_gap_report = _read_pending_gap_report(snapshot)
        if pending_gap_report is not None:
            details = _pending_gap_error(
                "compat_input_gaps_pending",
                "snapshot has unresolved input gaps; no replay checks were run",
                "pending_decision",
            ).details
            details["pending_gap_report"] = pending_gap_report
            raise RunnerError(
                "compat_input_gaps_pending",
                "snapshot has unresolved input gaps; no replay checks were run",
                details,
            )

        manifest, reference = baseline.read_snapshot(snapshot)
        baseline.validate_execution_configuration(manifest)
        baseline.validate_replay_source(manifest, Path(baseline.__file__).resolve())

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
    comparison = baseline.compare_reference(reference, days, navs, stats, anchors)
    navs, signals, trades, stats, daily = _scaled_result(
        navs, signals, trades, stats, daily, config["initial_nav"],
    )
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


def _persist_failure(output, error):
    failure_path = Path(output).resolve().with_name("compat-failure.json")
    temporary_path = failure_path.with_name(
        f".{failure_path.name}.{os.getpid()}.tmp"
    )
    code = error.code if isinstance(error.code, str) else "compat_runner_failed"
    message = str(error)
    failure = {"error": {"code": code, "message": message, "details": error.details}}
    details_complete = True
    try:
        payload = canonical_json(failure) + b"\n"
    except (TypeError, ValueError, OverflowError, RecursionError, UnicodeError):
        details_complete = False
        failure = {
            "error": {
                "code": code,
                "message": message,
                "details": {
                    "failure_details_omitted_reason": (
                        "original failure details were not JSON serializable"
                    )
                },
            },
            "structured_details_complete": False,
        }
        payload = canonical_json(failure) + b"\n"
    try:
        failure_path.parent.mkdir(parents=True, exist_ok=True)
        with temporary_path.open("xb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, failure_path)
        return True, details_complete
    except OSError:
        try:
            temporary_path.unlink(missing_ok=True)
        except OSError:
            pass
        return False, False


def _emit_failure(output, error):
    details_persisted, details_complete = _persist_failure(output, error)
    summary = {
        "code": error.code if isinstance(error.code, str) else "compat_runner_failed",
        "message": str(error),
        "details": {
            "structured_details_persisted": details_persisted,
            "structured_details_complete": details_complete,
        },
    }
    if not details_persisted:
        summary["details"]["structured_details_persistence_failed"] = True
    print(json.dumps({"error": summary}), file=sys.stderr)


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
        _emit_failure(args.output, error)
        return 2
    except OSError as error:
        wrapped = RunnerError("compat_input_unavailable", f"runner input or output is unavailable: {error}")
        _emit_failure(args.output, wrapped)
        return 2
    except Exception as error:
        wrapped = RunnerError("compat_runner_failed", str(error))
        _emit_failure(args.output, wrapped)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
