#!/usr/bin/env python3
"""Run or verify the local-only MVP-6 engineering acceptance artifacts."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
import json
from pathlib import Path
import subprocess
import sys
from typing import Any


SCHEMA = "mvp6-acceptance-artifacts@1"
EARLY_FAILURE_SCHEMA = "mvp6-early-failure@1"
FIXED_UNIVERSE = {
    "version": "mvp6-fixed-etf@3",
    "codes": ["159919", "159560", "159522", "159612", "159952"],
    "minimum_code_count": 3,
}
PARAMETERS = {"lookback": 60, "top_k": 3, "rebalance_every": 5}
LOT = {
    "currency": "CNY",
    "initial_cash": "1000000",
    "sizing": "lot",
    "rebalance": "full_target",
    "minimum_commission": "5",
    "portfolios": [{"id": 1, "capital": "1000000"}],
    "lot_size_source": {"kind": "instrument"},
}
COSTS = {
    "commission_rate": 0.0003,
    "buy_slippage_bps": 2.0,
    "sell_slippage_bps": 2.0,
    "buy_tax_rate": 0.0,
    "sell_tax_rate": 0.0,
}
TERMINAL = {"semantic_version": "mvp6-terminal-settlement@1"}
TOLERANCE = 1e-12


def dec(value: Any) -> Decimal:
    return Decimal(str(value))


def result(passed: bool, **details: Any) -> dict[str, Any]:
    return {"passed": bool(passed), "status": "passed" if passed else "failed", **details}


def build_pit_input(pit_base: dict[str, Any], frozen_opening: dict[str, Any]) -> dict[str, Any]:
    """Build the identity-bearing MVP-6 inputs from the frozen local evidence."""
    return {
        **pit_base,
        "market": frozen_opening,
        "terminal_settlement": dict(TERMINAL),
        "fixed_universe": {**FIXED_UNIVERSE, "codes": list(FIXED_UNIVERSE["codes"])},
    }


def normalize_unknown_coverage(coverage: dict[str, Any]) -> dict[str, Any]:
    return {
        "engineering_acceptance": coverage["engineering_acceptance"],
        "unknown_cells": coverage["unknown_cells"],
        "denominator_cells": coverage["denominator_cells"],
        "thresholds": coverage["thresholds"],
        "instruments": [
            {
                "symbol": item.get("symbol", item.get("native_symbol")),
                "cells": item["cells"],
                "unknown_cells": item["unknown_cells"],
            }
            for item in coverage["instruments"]
        ],
    }


def raw_integrity_failure(checked: dict[str, bool]) -> str | None:
    invalid = sum(not valid for valid in checked.values())
    return f"frozen Raw integrity failed: {invalid} object missing or mismatched" if invalid else None


def verify_unknown_coverage(coverage: dict[str, Any]) -> dict[str, Any]:
    total_percent = (
        Decimal(100) * dec(coverage["unknown_cells"]) / dec(coverage["denominator_cells"])
        if int(coverage["denominator_cells"]) else Decimal(0)
    )
    total_limit = dec(coverage["thresholds"]["total_unknown_percent"])
    instrument_limits = []
    for instrument in coverage["instruments"]:
        cells = int(instrument["cells"])
        unknown = int(instrument["unknown_cells"])
        percent = Decimal(100) * unknown / cells if cells else Decimal(0)
        instrument_limits.append({"symbol": instrument["symbol"], "percent": float(percent)})
    return result(
        coverage.get("engineering_acceptance") == "passed"
        and total_percent <= total_limit
        and all(item["percent"] <= float(coverage["thresholds"]["per_instrument_unknown_percent"])
                for item in instrument_limits),
        per_instrument=instrument_limits,
    )


def verify_early_failure(data: dict[str, Any]) -> dict[str, Any]:
    failure = data["failure"]
    status = failure.get("status")
    if status not in {"failed", "not_run"}:
        raise ValueError("early failure status must be failed or not_run")
    main = result(
        False,
        status=status,
        stage=failure.get("stage"),
        error=failure.get("error", "early run did not complete"),
        fixed_universe=failure.get("fixed_universe"),
        identity_change=failure.get("identity_change"),
    )
    reason = "The main Run did not complete, so this criterion was not executed."
    checks = {
        "main_run": main,
        "daily_parity": result(False, status="not_run", reason=reason),
        "reproducibility": result(False, status="not_run", reason=reason),
        "python_as_of_and_cli": result(False, status="not_run", reason=reason),
        "daily_accounting": result(False, status="not_run", reason=reason),
        "unknown_floor": verify_unknown_coverage(normalize_unknown_coverage(data["unknown_coverage"])),
        "frozen_raw_immutable": result(data.get("raw_immutable") is True),
    }
    return {
        "schema": "mvp6-acceptance-verification@1",
        "passed": all(item["passed"] for item in checks.values()),
        "checks": checks,
    }


def verify_artifacts(data: dict[str, Any]) -> dict[str, Any]:
    """Evaluate the six blocking criteria from a report or synthetic artifact set."""
    if data.get("schema") == EARLY_FAILURE_SCHEMA:
        return verify_early_failure(data)
    if data.get("schema") != SCHEMA:
        raise ValueError(f"schema must be {SCHEMA}")

    main = data["main_run"]
    actions = main["actions"]
    dividend = any(
        event.get("instrument_id", "").startswith("159919")
        and event.get("kind") in {"dividend_entitlement", "dividend_receivable", "dividend_paid"}
        for event in actions
    )
    split_rows = [
        event for event in actions
        if event.get("instrument_id", "").startswith("159560")
        and event.get("kind") == "share_conversion"
    ]
    split_traceable = bool(split_rows) and all(
        all(event.get(field) is not None for field in (
            "quantity_before", "quantity_after", "theoretical_quantity",
            "residual_quantity", "corporate_action_rounding",
        )) for event in split_rows
    )
    terminal_rows = main.get("terminal_settlements", [])
    terminal_traceable = (
        main.get("terminal_settlement_version") == TERMINAL["semantic_version"]
        and all(all(row.get(field) is not None for field in (
            "session_date", "instrument_id", "quantity", "price", "proceeds",
        )) for row in terminal_rows)
    )
    main_ok = (
        main.get("sizing") == "lot"
        and main.get("rebalance") == "full_target"
        and main.get("return_basis") == "total_return"
        and dividend
        and split_traceable
        and terminal_traceable
    )

    parity_days = data["parity"]["days"]
    parity_errors = []
    for day in parity_days:
        error = abs(float(dec(day["vector_open_nav"])) - float(dec(day["fast_open_nav"])))
        if error > TOLERANCE:
            parity_errors.append({"session_date": day["session_date"], "absolute_error": error})
    parity_ok = bool(parity_days) and not parity_errors

    independent = data["independent_runs"]
    deterministic = len(independent) == 2
    if deterministic:
        deterministic = (
            independent[0]["experiment_id"] == independent[1]["experiment_id"]
            and independent[0]["execution_id"] == independent[1]["execution_id"]
            and independent[0]["summary_logical_hash"] == independent[1]["summary_logical_hash"]
            and set(independent[0]["levels"]) == {"summary", "standard", "full"}
            and independent[0]["levels"] == independent[1]["levels"]
        )
    if deterministic:
        deterministic = all(
            run.get("status") == "completed"
            for run in independent[0]["levels"]["full"].get("runs", [])
        )

    python = data["python_cli"]
    python_cli_ok = (
        python.get("cli") == python.get("python")
        and python.get("api_action") == "replayed"
        and python.get("as_of_view_matches") is True
    )

    accounting = data["accounting"]
    accounting_errors = []
    for row in accounting["valuations"]:
        try:
            equity = dec(row["cash"]) + dec(row["dividend_receivable"])
            equity += dec(row.get("liquidation_receivable", "0"))
            for instrument, position in row["positions"].items():
                quantity, price = dec(position["quantity"]), dec(position["price"])
                market_value = dec(position["market_value"])
                if quantity < 0 or quantity * price != market_value:
                    accounting_errors.append({"session_date": row["session_date"], "instrument_id": instrument})
                equity += market_value
            if equity != dec(row["equity"]):
                accounting_errors.append({"session_date": row["session_date"], "kind": "nav_identity"})
        except (KeyError, InvalidOperation, TypeError):
            accounting_errors.append({"session_date": row.get("session_date"), "kind": "invalid_valuation"})

    fee_errors = []
    for fill in accounting["fills"]:
        try:
            costs = fill["costs"]
            total = sum((dec(costs[key]) for key in (
                "commission", "minimum_commission_top_up", "tax",
            )), Decimal(0)) + dec(fill["slippage"])
            if total != dec(fill["total_fees"]):
                fee_errors.append(True)
        except (KeyError, InvalidOperation, TypeError):
            fee_errors.append(True)

    blocked = {
        (cell["instrument_id"], cell["session_date"])
        for cell in accounting["forbidden_cells"]
        if cell["state"] in {"unknown", "halted"}
    }
    blocked_trades = [
        trade for trade in accounting["trades"]
        if (trade["instrument_id"], trade["session_date"]) in blocked
    ]
    rounding_ok = split_traceable
    accounting_ok = (
        bool(accounting["valuations"])
        and not accounting_errors
        and bool(accounting["fills"])
        and not fee_errors
        and accounting.get("fees_present") is True
        and not blocked_trades
        and rounding_ok
        and terminal_traceable
    )

    checks = {
        "main_run": result(main_ok, dividend_ledger=dividend, split_ledger=split_traceable,
                            terminal_settlement=terminal_traceable),
        "daily_parity": result(parity_ok, sessions=len(parity_days), errors=parity_errors),
        "reproducibility": result(deterministic),
        "python_as_of_and_cli": result(python_cli_ok),
        "daily_accounting": result(accounting_ok, valuation_errors=accounting_errors,
                                   fee_errors=len(fee_errors), blocked_trades=len(blocked_trades)),
        "unknown_floor": verify_unknown_coverage(data["unknown_coverage"]),
        "frozen_raw_immutable": result(data.get("raw_immutable") is True),
    }
    return {
        "schema": "mvp6-acceptance-verification@1",
        "passed": all(item["passed"] for item in checks.values()),
        "checks": checks,
    }


def parse_payload_rows(frame: Any) -> list[dict[str, Any]]:
    rows = []
    for row in frame.to_pylist():
        payload = row.get("payload")
        if not isinstance(payload, str):
            raise ValueError("result table row has no JSON payload")
        rows.append(json.loads(payload))
    return rows


def experiment_from_definition(definition: dict[str, Any]) -> Any:
    import prajna

    engine_kind = prajna.EngineKind(definition["engine"])
    if engine_kind == prajna.EngineKind.FAST_EVENT:
        raw = definition["fast_event"]
        settings = prajna.FastEventSettings(
            currency=raw["currency"], initial_cash=raw["initial_cash"],
            sizing=raw["sizing"], rebalance=raw["rebalance"],
            minimum_commission=raw["minimum_commission"],
            portfolios=[prajna.Capital(item["id"], item["capital"]) for item in raw["portfolios"]],
            lot_size_source=prajna.LotSizeSource(),
        )
        engine = prajna.Engine(engine_kind, settings=settings)
    else:
        engine = prajna.Engine(
            engine_kind, initial_capital=definition["vector_pit"]["initial_capital"],
        )
    strategy = prajna.RustStrategy(definition["strategy"])
    return prajna.Experiment(
        dataset_version=prajna.DatasetVersion(definition["dsv"]),
        universe=prajna.Universe(definition["universe"]),
        venue=prajna.Venue(definition["venue"]),
        strategy=strategy,
        engine=engine,
        costs=prajna.Costs(**definition["costs"]),
        parameter_space=prajna.List(definition["parameter_space"]["list"]),
        availability_assumption=prajna.AvailabilityAssumption(definition["availability_assumption"]),
        sessions_per_year=definition["sessions_per_year"],
        pit=definition["pit"],
    )


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def invoke(argv: list[str], log_path: Path, *, cwd: Path) -> dict[str, Any]:
    completed = subprocess.run(argv, cwd=cwd, capture_output=True, text=True)
    write_json(log_path.with_suffix(".json"), {
        "argv": argv,
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    })
    if completed.returncode:
        raise RuntimeError(f"command failed; diagnostics are local at {log_path.with_suffix('.json')}")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"command did not emit JSON; diagnostics are local at {log_path.with_suffix('.json')}") from error


class RunPhaseError(RuntimeError):
    def __init__(
        self, stage: str, status: str, error: str, *,
        cli_result: dict[str, Any] | None = None, manifest: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(error)
        self.stage = stage
        self.status = status
        self.cli_result = cli_result
        self.manifest = manifest


def fixed_universe_diagnostics(run_spec: dict[str, Any]) -> dict[str, Any]:
    fixed = run_spec.get("pit", {}).get("fixed_universe", {})
    included: set[str] = set()
    excluded: dict[str, set[str]] = {}
    catalog_versions: set[str] = set()
    rule_versions: set[str] = set()
    for session in run_spec.get("session_inputs", []):
        universe = session.get("eligible", {}).get("universe", {}).get("run_spec", {})
        included.update(universe.get("fixed_list", []))
        if universe.get("catalog_version"):
            catalog_versions.add(universe["catalog_version"])
        if universe.get("rule_version"):
            rule_versions.add(universe["rule_version"])
        for code, reason in universe.get("fixed_list_exclusions", {}).items():
            excluded.setdefault(code, set()).add(reason)
    candidates = set(fixed.get("codes", [])) | set(fixed.get("excluded_codes", {}))
    accounted = included | set(excluded)
    return {
        "configuration": fixed,
        "catalog_versions": sorted(catalog_versions),
        "rule_versions": sorted(rule_versions),
        "included_codes": sorted(included),
        "excluded_reasons": {code: sorted(reasons) for code, reasons in sorted(excluded.items())},
        "unresolved_codes": sorted(candidates - accounted),
    }


def unevaluated_fixed_universe(fixed: dict[str, Any]) -> dict[str, Any]:
    return {
        "configuration": fixed,
        "catalog_versions": [],
        "rule_versions": [],
        "included_codes": [],
        "excluded_reasons": {},
        "unresolved_codes": list(fixed["codes"]),
        "status": "not_evaluated_due_to_raw_preflight",
    }


def prior_run_identity(acceptance: Path, current_output: Path) -> dict[str, Any] | None:
    for previous in sorted(acceptance.glob("issue-234-*"), reverse=True):
        if previous == current_output:
            continue
        input_path = previous / "independent-run-1" / "pit-input.json"
        log_path = previous / "independent-run-1" / "main-summary-cli.json"
        if not input_path.is_file() or not log_path.is_file():
            continue
        try:
            prior_input = json.loads(input_path.read_text())
            cli_log = json.loads(log_path.read_text())
            cli_result = json.loads(cli_log["stdout"])
        except (OSError, KeyError, json.JSONDecodeError):
            continue
        if cli_result.get("exp") and cli_result.get("exe"):
            return {
                "experiment_id": cli_result["exp"],
                "execution_id": cli_result["exe"],
                "fixed_universe": prior_input.get("fixed_universe", "omitted; legacy default applied"),
            }
    return None


def identity_change(
    acceptance: Path, current_output: Path, cli_result: dict[str, Any],
) -> dict[str, Any]:
    current = {"experiment_id": cli_result.get("exp"), "execution_id": cli_result.get("exe")}
    previous = prior_run_identity(acceptance, current_output)
    return {
        "previous": previous,
        "current": current,
        "experiment_changed": previous is not None and previous["experiment_id"] != current["experiment_id"],
        "execution_changed": previous is not None and previous["execution_id"] != current["execution_id"],
    }


def write_early_failure_report(
    *, output: Path, acceptance: Path, raw_inventory: Path, coverage: dict[str, Any],
    unknown: dict[str, Any], pit_base: dict[str, Any], failure: dict[str, Any],
) -> dict[str, Any]:
    normalized_unknown = normalize_unknown_coverage(unknown)
    raw_before = failure["raw_hashes_before"]
    raw_after = raw_hashes(acceptance.parent, raw_inventory)
    raw_immutable = raw_before == raw_after and all(raw_before.values()) and all(raw_after.values())
    artifacts = {
        "schema": EARLY_FAILURE_SCHEMA,
        "failure": {key: failure.get(key) for key in (
            "stage", "status", "error", "fixed_universe", "identity_change",
        )},
        "unknown_coverage": normalized_unknown,
        "raw_immutable": raw_immutable,
    }
    verification = verify_early_failure(artifacts)
    report = {
        "schema": "mvp6-local-engineering-report@1",
        "issue": 234,
        "classification": "platform engineering acceptance; not strategy historical performance",
        "verification": verification,
        "failure": artifacts["failure"],
        "raw_immutability": {
            "passed": raw_immutable,
            "inventory_entries": len(raw_before),
            "verified_before_run": sum(raw_before.values()),
            "verified_after_run": sum(raw_after.values()),
            "failed_before_run": sum(not valid for valid in raw_before.values()),
        },
        "non_blocking": {
            name: {"status": "not_run", "reason": "The main Run failed before acceptance could continue."}
            for name in ("small_grid", "accurate_smoke", "legacy_comparison")
        },
        "source_evidence": {
            "decision_window": {"start": "2023-01-01", "end": "2026-09-30", "preheat": "60 Sessions"},
            "return_basis": "total_return (planned; no Run result was produced)",
            "universe_claim": "retrospective_static",
            "survivorship_bias": True,
            "source_status": coverage.get("source_status"),
            "p21_status": coverage.get("p21", {}).get("status"),
            "unknown_semantics": "P27-23.5 narrowed UNKNOWN to data defects; no-Bar and unknown rules are excluded.",
            "risk_notice_count": len(pit_base.get("risk_notices", [])),
            "cninfo_index_supplied": "cninfo_index" in pit_base,
        },
        "local_artifacts": {
            "directory": str(output),
            "rerun_command": "python scripts/mvp6_acceptance.py run --root ~/PrajnaData/mvp-6",
            "frozen_raw_inventory": str(raw_inventory),
        },
        "unknown_coverage": normalized_unknown,
    }
    write_json(output / "mvp6-acceptance-report.json", report)
    safe_summary = {
        "passed": False,
        "checks": {name: check["status"] for name, check in verification["checks"].items()},
        "per_etf_unknown_coverage": [
            {
                "code": item["symbol"],
                "unknown_cells": item["unknown_cells"],
                "cells": item["cells"],
                "unknown_percent": 100 * item["unknown_cells"] / item["cells"] if item["cells"] else 0,
            }
            for item in normalized_unknown["instruments"]
        ],
        "conclusion": "Platform engineering acceptance only; this is not strategy historical performance.",
        "report": str(output / "mvp6-acceptance-report.json"),
    }
    return safe_summary


def raw_hashes(root: Path, inventory: Path) -> dict[str, bool]:
    data = json.loads(inventory.read_text())
    hashes = sorted({item["sha256"] for item in data["request_records"] if item.get("sha256")})
    checked: dict[str, bool] = {}
    for digest in hashes:
        path = root / "raw" / "sha256" / digest[:2] / digest
        if not path.is_file():
            checked[digest] = False
            continue
        import hashlib
        checked[digest] = hashlib.sha256(path.read_bytes()).hexdigest() == digest
    return checked


def local_lake(source: Path, destination: Path) -> Path:
    lake = destination / "lake"
    lake.mkdir(parents=True)
    for name in ("raw", "raw-sources", "manifests", "normalized"):
        target = source / name
        if not target.is_dir():
            raise ValueError(f"frozen input directory is missing: {name}")
        (lake / name).symlink_to(target, target_is_directory=True)
    return lake


def experiment_definition(
    dsv: str, catalog: str, pit: dict[str, Any], engine: str, *, sizing: str = "lot",
) -> dict[str, Any]:
    definition: dict[str, Any] = {
        "experiment_version": 1,
        "dsv": dsv,
        "universe": catalog,
        "venue": "SZSE",
        "strategy": "s2_pit_rotation",
        "engine": engine,
        "availability_assumption": "none",
        "sessions_per_year": 252,
        "costs": COSTS,
        "parameter_space": {"list": [PARAMETERS]},
        "pit": pit,
    }
    if engine == "fast_event":
        definition["fast_event"] = {**LOT, "sizing": sizing}
    else:
        definition["vector_pit"] = {"initial_capital": "1000000"}
    return definition


def run_levels(
    definition: dict[str, Any], lake: Path, cli: Path, repository: Path,
    output: Path, prefix: str, *, levels: tuple[str, ...],
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    import prajna

    typed = experiment_from_definition(definition)
    if json.loads(typed.to_json()) != definition:
        raise ValueError("Python typed definition differs from the CLI definition")
    run_ids: list[str] = []
    cli_results: dict[str, Any] = {}
    api_results: dict[str, Any] = {}
    manifests: dict[str, Any] = {}
    for level in levels:
        argv = [str(cli), "run", str(output / f"{prefix}-experiment.json"), "--lake", str(lake),
                "--level", level, "--threads", "1"]
        if level != "summary" and run_ids:
            argv.extend(["--promote", *run_ids])
        try:
            cli_result = invoke(argv, output / f"{prefix}-{level}-cli", cwd=repository)
        except RuntimeError as error:
            raise RunPhaseError(f"{prefix}_{level}", "failed", str(error)) from error
        experiment_hex = cli_result["exp"].split(":sha256:", 1)[1]
        execution_hex = cli_result["exe"].split(":sha256:", 1)[1]
        cli_manifest_path = lake / "experiments" / experiment_hex / "executions" / execution_hex / "execution.json"
        cli_manifest = json.loads(cli_manifest_path.read_text())
        if cli_result.get("failed_count") != 0:
            failures = [run for run in cli_manifest.get("runs", []) if run.get("status") != "completed"]
            original_error = next((run.get("error") for run in failures if run.get("error")), None)
            raise RunPhaseError(
                f"{prefix}_{level}", "failed",
                original_error or f"CLI recorded {cli_result.get('failed_count')} failed Run(s)",
                cli_result=cli_result, manifest=cli_manifest,
            )
        run_ids = [run["run_id"] for run in cli_manifest["runs"]]
        api_result = typed.run(
            lake, repository=repository, level=prajna.ResultLevel(level), threads=1,
        )
        manifest = json.loads((api_result.path / "execution.json").read_text())
        if api_result.action != "replayed" or cli_manifest != manifest:
            raise RunPhaseError(
                f"{prefix}_{level}_api_replay", "not_run",
                "Python API did not replay CLI Execution: "
                f"action={api_result.action}, "
                f"experiment_match={cli_result.get('exp') == manifest.get('experiment_id')}, "
                f"execution_match={cli_result.get('exe') == manifest.get('execution', {}).get('id')}, "
                f"manifest_match={cli_manifest == manifest}",
                cli_result=cli_result,
                manifest=cli_manifest,
            )
        if cli_result.get("exp") != manifest["experiment_id"] or cli_result.get("exe") != manifest["execution"]["id"]:
            raise RuntimeError("Python API and CLI identities differ")
        api_results[level] = {
            "experiment_id": manifest["experiment_id"],
            "execution_id": manifest["execution"]["id"],
            "action": api_result.action,
            **manifest_results(manifest),
        }
        cli_results[level] = {**cli_result, **manifest_results(cli_manifest)}
        manifests[level] = manifest
    return cli_results, api_results, manifests


def read_table(lake: Path, manifest: dict[str, Any], run_id: str, table: str) -> list[dict[str, Any]]:
    import prajna

    return parse_payload_rows(prajna.read_run(
        str(lake), manifest["experiment_id"], manifest["execution"]["id"], run_id,
        table=table, result_level="full",
    ))


def compare_independent_levels(manifests_by_run: list[dict[str, Any]]) -> list[dict[str, Any]]:
    left, right = manifests_by_run
    reports = []
    for levels in (left, right):
        if set(levels) != {"summary", "standard", "full"}:
            raise RuntimeError("each independent run must contain Summary, Standard and Full")
        manifests = list(levels.values())
        if len({item["experiment_id"] for item in manifests}) != 1 \
                or len({item["execution"]["id"] for item in manifests}) != 1:
            raise RuntimeError("Run/Execution identity changed across result levels")
        reports.append({
            "experiment_id": manifests[0]["experiment_id"],
            "execution_id": manifests[0]["execution"]["id"],
            "summary_logical_hash": manifests[0]["summary_logical_hash"],
            "levels": {
                level: {
                    "run_ids": [run["run_id"] for run in manifest["runs"]],
                    "summary_logical_hash": manifest["summary_logical_hash"],
                    "table_hashes": {run["run_id"]: run["table_hashes"] for run in manifest["runs"]},
                    "runs": [{"status": run["status"]} for run in manifest["runs"]],
                }
                for level, manifest in levels.items()
            },
        })
    return reports


def manifest_results(manifest: dict[str, Any]) -> dict[str, Any]:
    return {
        "summary_logical_hash": manifest["summary_logical_hash"],
        "runs": {run["run_id"]: {"status": run["status"], "table_hashes": run["table_hashes"]}
                 for run in manifest["runs"]},
    }


def as_of_decisions_match(lake: Path, dsv: str, pit: dict[str, Any], vector_manifest: dict[str, Any]) -> bool:
    import prajna

    params = PARAMETERS
    strategy = prajna.Strategy(
        "mvp6_asof_check", "1", bar_columns=(), lookback_sessions=params["lookback"],
        rebalance_every=params["rebalance_every"],
        factors=(prajna.FactorDependency(
            "momentum", "etf_tri_momentum", {"lookback": params["lookback"]},
        ),),
    )

    def decide(view: Any, parameters: dict[str, Any], previous: Any) -> Any:
        rows = [row for row in view.factors["momentum"].to_pylist()
                if str(row["session_date"]) == view.session_date and row["value"] is not None
                and row["instrument_id"] in view.eligible
                and row["instrument_id"] not in view.risk_excluded]
        rows.sort(key=lambda row: (-row["value"], row["instrument_id"]))
        selected = rows[:parameters["top_k"]]
        return prajna.Decision(
            {row["instrument_id"]: 1 / len(selected) for row in selected},
            [(row["instrument_id"], row["value"], index + 1)
             for index, row in enumerate(rows)],
        )

    decisions = prajna.generate_decisions(
        lake_root=str(lake), dsv=dsv, venue="SZSE", members=[], strategy=strategy,
        decide=decide, params=params, pit=pit,
    )
    vector_run = vector_manifest["runs"][0]
    stored = read_table(lake, vector_manifest, vector_run["run_id"], "decisions")
    expected = sorted(
        (row["decision_session"], row["instrument_id"], row["rank"], row["score"], row["target_weight"])
        for row in stored
    )
    actual = sorted(
        (decision.session_date, instrument, rank, score,
         decision.targets.get(instrument))
        for decision in decisions
        for instrument, score, rank in decision.ranked
    )
    return len(expected) == len(actual) and all(
        left[:3] == right[:3]
        and abs(float(left[3]) - float(right[3])) <= TOLERANCE
        and ((left[4] is None and right[4] is None)
             or (left[4] is not None and right[4] is not None
                 and abs(float(left[4]) - float(right[4])) <= TOLERANCE))
        for left, right in zip(expected, actual)
    )


def run_acceptance(args: argparse.Namespace) -> tuple[dict[str, Any], Path]:
    root = Path(args.root).expanduser().resolve()
    expected_root = (Path.home() / "PrajnaData" / "mvp-6").resolve()
    if root != expected_root:
        raise ValueError("real-data execution is restricted to ~/PrajnaData/mvp-6")
    if sys.version_info[:2] != (3, 12):
        raise ValueError("run with the project Python 3.12 environment")
    cli = Path(args.cli).resolve()
    repository = Path(args.repository).resolve()
    if not cli.is_file():
        raise ValueError(f"prajna-experiment binary not found: {cli}")

    acceptance = root / "acceptance"
    input_path = Path(args.pit_input).expanduser().resolve() if args.pit_input else (
        acceptance / "issue-224-rerun-20261022T100126" / "pit-input.json"
    )
    if root not in input_path.parents:
        raise ValueError("real-data inputs must be inside ~/PrajnaData/mvp-6")
    coverage_path = acceptance / "mvp6-coverage.json"
    unknown_path = acceptance / "mvp6-unknown-coverage.json"
    raw_inventory = acceptance / "mvp6-raw-inventory.json"
    coverage = json.loads(coverage_path.read_text())
    unknown = json.loads(unknown_path.read_text())
    pit_base = json.loads(input_path.read_text())
    frozen_opening_path = acceptance / "mvp6-opening-evidence-v3.json"
    frozen_opening = json.loads(frozen_opening_path.read_text())
    if (frozen_opening.get("semantic_version") != "szse-derived-open@3"
            or frozen_opening.get("start") != pit_base["catalog"]["coverage"]["start"]
            or frozen_opening.get("end") != pit_base["catalog"]["coverage"]["end"]):
        raise ValueError("frozen opening evidence does not match the PIT coverage")
    dsv = coverage["corporate_action_dataset"]["dsv"]
    catalog_version = unknown["catalog_version"]

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = acceptance / f"issue-234-{stamp}"
    output.mkdir(parents=True, exist_ok=False)
    before = raw_hashes(root, raw_inventory)
    write_json(output / "frozen-input-evidence.json", {
        "data_read_only": True,
        "raw_inventory_entries": len(before),
        "all_inventory_hashes_verified_before": all(before.values()),
        "raw_integrity_failure": raw_integrity_failure(before),
        "fixed_universe": FIXED_UNIVERSE,
        "source_status": coverage.get("source_status"),
        "p21_status": coverage.get("p21", {}).get("status"),
        "historical_capture_status": coverage.get("historical_capture_status"),
        "terms_conclusion": coverage.get("terms_conclusion"),
        "risk_notice_count": len(pit_base.get("risk_notices", [])),
        "cninfo_index_supplied": "cninfo_index" in pit_base,
    })

    if reason := raw_integrity_failure(before):
        summary = write_early_failure_report(
            output=output,
            acceptance=acceptance,
            raw_inventory=raw_inventory,
            coverage=coverage,
            unknown=unknown,
            pit_base=pit_base,
            failure={
                "stage": "raw_preflight",
                "status": "failed",
                "error": reason,
                "fixed_universe": unevaluated_fixed_universe(FIXED_UNIVERSE),
                "identity_change": None,
                "raw_hashes_before": before,
            },
        )
        return summary, output

    # Two independent result stores use symlinks to the same immutable inputs.
    lake_dirs = [local_lake(root, output / f"independent-run-{index}") for index in (1, 2)]
    pit_inputs = []
    evidence_bytes = frozen_opening_path.read_bytes()
    for index, lake in enumerate(lake_dirs, start=1):
        evidence_path = lake / "acceptance" / frozen_opening_path.name
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        evidence_path.write_bytes(evidence_bytes)
        pit = build_pit_input(pit_base, frozen_opening)
        pit_inputs.append(pit)
        write_json(output / f"independent-run-{index}" / "pit-input.json", pit)

    main_manifests = []
    main_results = []
    for index, (lake, pit) in enumerate(zip(lake_dirs, pit_inputs), start=1):
        definition = experiment_definition(dsv, catalog_version, pit, "fast_event")
        definition_path = output / f"independent-run-{index}" / "main-experiment.json"
        write_json(definition_path, definition)
        try:
            cli_results, api_results, manifests = run_levels(
                definition, lake, cli, repository, output / f"independent-run-{index}", "main",
                levels=("summary", "standard", "full"),
            )
        except Exception as error:
            manifest = getattr(error, "manifest", None) or {}
            runs = manifest.get("runs", [])
            failed_run = next((
                run for run in runs
                if run.get("status") != "completed"
            ), next(iter(runs), {}))
            run_spec = failed_run.get("run_spec", {})
            configured = fixed_universe_diagnostics(run_spec) if run_spec else {
                **unevaluated_fixed_universe(pit["fixed_universe"]),
                "status": "not_evaluated_due_to_run_failure",
            }
            current_cli = getattr(error, "cli_result", None) or {}
            summary = write_early_failure_report(
                output=output,
                acceptance=acceptance,
                raw_inventory=raw_inventory,
                coverage=coverage,
                unknown=unknown,
                pit_base=pit_base,
                failure={
                    "stage": getattr(error, "stage", f"main_run_{index}_setup_or_replay"),
                    "status": getattr(error, "status", "not_run"),
                    "error": str(error),
                    "fixed_universe": configured,
                    "identity_change": identity_change(acceptance, output, current_cli)
                    if current_cli.get("exp") and current_cli.get("exe") else None,
                    "raw_hashes_before": before,
                },
            )
            return summary, output
        main_manifests.append(manifests)
        main_results.append({"cli": cli_results, "api": api_results})

    vector_definition = experiment_definition(dsv, catalog_version, pit_inputs[0], "vector")
    parity_fast_definition = experiment_definition(
        dsv, catalog_version, pit_inputs[0], "fast_event", sizing="vector_parity",
    )
    parity_paths = output / "independent-run-1"
    write_json(parity_paths / "parity-vector-experiment.json", vector_definition)
    write_json(parity_paths / "parity-fast-experiment.json", parity_fast_definition)
    vector_cli, vector_api, vector_manifests = run_levels(
        vector_definition, lake_dirs[0], cli, repository, parity_paths, "parity-vector", levels=("full",),
    )
    fast_cli, fast_api, fast_manifests = run_levels(
        parity_fast_definition, lake_dirs[0], cli, repository, parity_paths, "parity-fast", levels=("full",),
    )
    vector_manifest = vector_manifests["full"]
    fast_manifest = fast_manifests["full"]

    main_full_manifest = main_manifests[0]["full"]
    main_run = main_full_manifest["runs"][0]
    main_lake = lake_dirs[0]
    main_full_tables = {
        name: read_table(main_lake, main_full_manifest, main_run["run_id"], name)
        for name in ("account_valuations", "fills", "fee_settlements", "corporate_actions", "terminal_settlements")
    }
    fast_run = fast_manifest["runs"][0]
    fast_accounts = read_table(lake_dirs[0], fast_manifest, fast_run["run_id"], "account_valuations")
    vector_run = vector_manifest["runs"][0]
    vector_sessions = read_table(lake_dirs[0], vector_manifest, vector_run["run_id"], "sessions")
    vector_decisions = read_table(lake_dirs[0], vector_manifest, vector_run["run_id"], "decisions")

    fast_open = {
        row["session_date"]: float(dec(row["equity"])) / 1_000_000
        for row in fast_accounts
        if row.get("scope") == "account" and row.get("valuation_point") == "open_after"
    }
    parity_days = [{
        "session_date": row["session_date"],
        "vector_open_nav": float(row["nav"]) / (1 + float(row["gross_return"])),
        "fast_open_nav": fast_open[row["session_date"]],
    } for row in vector_sessions]

    excluded = main_run["run_spec"].get("execution_exclusions", [])
    forbidden_cells = [cell for cell in excluded if cell.get("state") in {"unknown", "halted"}]
    trades = []
    for row in main_full_tables["fills"]:
        fill = row["fill"]
        order = fill["order"]
        timestamp = fill["executed_at"]
        trades.append({
            "instrument_id": order["instrument_id"],
            "session_date": timestamp[:10],
        })

    valuations = []
    for row in main_full_tables["account_valuations"]:
        if row.get("scope") != "account":
            continue
        valuations.append({
            "session_date": row["session_date"],
            "valuation_point": row["valuation_point"],
            "cash": row["cash"],
            "dividend_receivable": row["dividend_receivable"],
            "liquidation_receivable": row["liquidation_receivable"],
            "equity": row["equity"],
            "positions": row["positions"],
        })

    all_run_specs = [run["run_spec"] for run in main_full_manifest["runs"]]
    run_spec = all_run_specs[0]
    engine = run_spec["engine"]
    action_rows = main_full_tables["corporate_actions"]
    evidence = {
        "schema": SCHEMA,
        "fixed_universe": fixed_universe_diagnostics(run_spec),
        "identity_change": identity_change(
            acceptance, output, main_results[0]["cli"]["full"],
        ),
        "main_run": {
            "sizing": engine.get("sizing"),
            "rebalance": engine.get("rebalance"),
            "return_basis": run_spec.get("corporate_action_schedule", {}).get("return_basis"),
            "actions": action_rows,
            "terminal_settlement_version": run_spec.get("terminal_settlement", {}).get("semantic_version"),
            "terminal_settlements": main_full_tables["terminal_settlements"],
        },
        "parity": {"days": parity_days},
        "independent_runs": compare_independent_levels(main_manifests),
        "python_cli": {
            "cli": {
                "experiment_id": main_results[0]["cli"]["full"]["exp"],
                "execution_id": main_results[0]["cli"]["full"]["exe"],
                "summary_logical_hash": main_results[0]["cli"]["full"]["summary_logical_hash"],
                "runs": main_results[0]["cli"]["full"]["runs"],
            },
            "python": {
                "experiment_id": main_results[0]["api"]["full"]["experiment_id"],
                "execution_id": main_results[0]["api"]["full"]["execution_id"],
                "summary_logical_hash": main_results[0]["api"]["full"]["summary_logical_hash"],
                "runs": main_results[0]["api"]["full"]["runs"],
            },
            "api_action": main_results[0]["api"]["full"]["action"],
            "as_of_view_matches": as_of_decisions_match(
                lake_dirs[0], dsv, pit_inputs[0], vector_manifest,
            ),
        },
        "accounting": {
            "valuations": valuations,
            "fills": [{
                "total_fees": row["fill"]["total_fees"],
                "costs": row["fill"]["costs"],
                "slippage": row["fill"]["slippage"],
            } for row in main_full_tables["fills"]],
            "forbidden_cells": forbidden_cells,
            "trades": trades,
            "fees_present": bool(main_full_tables["fills"]),
        },
        "unknown_coverage": run_spec["unknown_coverage"],
        "raw_immutable": (
            (after := raw_hashes(root, raw_inventory)) == before
            and all(before.values()) and all(after.values())
        ),
        "non_blocking": {
            "small_grid": {"status": "not_run", "reason": "P16 non-blocking grid is not needed for the engineering ledger gate."},
            "accurate_smoke": {"status": "not_run", "reason": "No Accurate Event worker was configured for this local acceptance."},
            "legacy_comparison": {"status": "not_run", "reason": "The old-system comparator for the legacy window is not available in this workspace."},
        },
        "source_evidence": {
            "market_normalizer": coverage.get("corporate_action_dataset", {}).get("normalizer"),
            "p21_status": coverage.get("p21", {}).get("status"),
            "source_status": coverage.get("source_status"),
            "historical_capture_status": coverage.get("historical_capture_status"),
            "terms_conclusion": coverage.get("terms_conclusion"),
            "decision_window": {"start": "2023-01-01", "end": "2026-09-30", "preheat": "60 Sessions"},
            "universe_claim": "retrospective_static",
            "survivorship_bias": True,
            "unknown_semantics": "P27-23.5 narrowed UNKNOWN to data defects; no-Bar and unknown rules are excluded.",
            "execution_limitations": [
                "The Session close is the market-data availability time; next Session execution uses the first-trade/open price.",
                "Same-Session sells and buys may not share the real intraday funding order.",
                "Orders submitted before open are assumed to participate after an opening halt resumes; this is unverified.",
                "Termination settlement uses the last in-lifecycle Bar close, not actual liquidation amount or date.",
                "No-Bar Sessions are treated as halted and non-tradable, not UNKNOWN.",
            ],
            "risk_notice_count": len(pit_base.get("risk_notices", [])),
            "cninfo_index_supplied": "cninfo_index" in pit_base,
        },
    }
    verification = verify_artifacts(evidence)
    report = {
        "schema": "mvp6-local-engineering-report@1",
        "issue": 234,
        "classification": "platform engineering acceptance; not strategy historical performance",
        "benchmark": None,
        "verification": verification,
        "local_artifacts": {
            "directory": str(output),
            "rerun_command": "python scripts/mvp6_acceptance.py run --root ~/PrajnaData/mvp-6",
            "frozen_raw_inventory": str(raw_inventory),
        },
        "details": evidence,
    }
    write_json(output / "mvp6-acceptance-report.json", report)
    public_unknown = evidence["unknown_coverage"]["instruments"]
    safe_summary = {
        "passed": verification["passed"],
        "checks": {name: item["passed"] for name, item in verification["checks"].items()},
        "per_etf_unknown_coverage": [
            {"code": item["symbol"], "unknown_cells": item["unknown_cells"],
             "cells": item["cells"], "unknown_percent": item["unknown_percent"]}
            for item in public_unknown
        ],
        "conclusion": "Platform engineering acceptance only; this is not strategy historical performance.",
        "report": str(output / "mvp6-acceptance-report.json"),
    }
    return safe_summary, output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    verify = subparsers.add_parser("verify", help="verify a local or synthetic artifact JSON")
    verify.add_argument("artifacts", type=Path)
    run = subparsers.add_parser("run", help="run the acceptance against the frozen local dataset")
    run.add_argument("--root", default="~/PrajnaData/mvp-6")
    run.add_argument("--pit-input", type=Path)
    run.add_argument("--cli", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/prajna-experiment")
    run.add_argument("--repository", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        if args.command == "verify":
            verified = verify_artifacts(json.loads(args.artifacts.read_text()))
            print(json.dumps(verified, indent=2, sort_keys=True))
            return 0 if verified["passed"] else 1
        summary, _ = run_acceptance(args)
        print(json.dumps(summary, indent=2, sort_keys=True))
        return 0 if summary["passed"] else 1
    except Exception as error:
        print(f"MVP-6 acceptance failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
