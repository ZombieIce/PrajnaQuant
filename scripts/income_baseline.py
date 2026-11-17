"""Freeze and run the published ten-stock income baseline from private inputs."""
import argparse
import ast
import importlib
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from unittest.mock import patch

from eternal_baseline import (
    InputGaps, csv_rows, input_gap, private_path,
    seal_manifest, sha256_file, validate_iso_date, write_csv, write_json,
)

REVISION = "d5d48c1"
BALANCE_BASIS = "legacy_ex_date_income_credit"
SCHEMA_VERSION = 4
CODES = ("sh601288", "sh601398", "sh601939", "sh600015", "sh600036", "sh601998",
         "sh600900", "sh601318", "sh601369", "sh600941")
CALENDAR = "sh000300"
FIRST_DATES = {"sh600015": "2017-06-30", "sh600036": "2013-09-06", "sh601318": "2015-07-28"}
TOLERANCES = {"nav": 1e-7, "cash": .00500001, "rate": 1e-7, "ledger": 1e-6}
EVENT_FIELDS = ("EQUITY_RECORD_DATE", "PRETAX_BONUS_RMB", "BONUS_RATIO", "IT_RATIO")


def gap(code, day, field, impact):
    return input_gap(code, day, field, impact, ["核对旧档与原始来源", "提交候选批次由负责人裁决"])


def digest_files(directory):
    files = {}
    for path in sorted(directory.rglob("*")):
        if path.is_symlink():
            raise ValueError("snapshot contains a symbolic link")
        if path.is_file():
            files[path.relative_to(directory).as_posix()] = sha256_file(path)
    return files


def local_path(path, label):
    resolved = private_path(path, label)
    if any((parent / ".git").exists() for parent in (resolved, *resolved.parents)):
        raise ValueError(f"{label} must be outside any Git checkout")
    return resolved


def environment():
    return {"python": platform.python_version(), "implementation": platform.python_implementation(),
            "platform": platform.platform()}


def read_revision(root, filename):
    return subprocess.check_output(
        ["git", "-C", str(root / "strategy-dashboard"), "show", f"{REVISION}:{filename}"],
        stderr=subprocess.DEVNULL,
    )


def imported_sources(root, relative, node):
    if isinstance(node, ast.Import):
        names = [alias.name.split(".") for alias in node.names]
        bases = (Path(), Path("strategy-dashboard"))
    elif isinstance(node, ast.ImportFrom):
        package = relative.parent
        if node.level:
            for _ in range(node.level - 1):
                package = package.parent
            bases = (package,)
        else:
            bases = (Path(), Path("strategy-dashboard"))
        module = node.module.split(".") if node.module else []
        names = [module, *[module + [alias.name] for alias in node.names if alias.name != "*"]]
    else:
        return
    for base in bases:
        for parts in names:
            for depth in range(len(parts) + 1):
                initializer = base.joinpath(*parts[:depth], "__init__.py")
                if (root / initializer).is_file():
                    yield initializer
            if parts:
                module_file = base.joinpath(*parts).with_suffix(".py")
                if (root / module_file).is_file():
                    yield module_file


def archive_source(root, destination):
    root = root.resolve()
    entry = Path("strategy-dashboard/daily_update.py")
    pending = {entry: read_revision(root, "daily_update.py")}
    archived = set()
    while pending:
        relative, source = pending.popitem()
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source)
        archived.add(relative)
        for node in ast.walk(ast.parse(source)):
            for candidate in imported_sources(root, relative, node):
                if candidate in archived or candidate in pending:
                    continue
                if root not in (root / candidate).resolve().parents:
                    raise ValueError("legacy source dependency points outside the source tree")
                pending[candidate] = (root / candidate).read_bytes()


def iso_day(value, field):
    validate_iso_date(value, field)
    return value


def normalize_events(code, rows, sessions, end, cutoff=""):
    result, issues = {}, []
    prior = dict(zip(sessions[1:], sessions))
    for row in rows:
        if row["ASSIGN_PROGRESS"] != "实施分配":
            continue
        day = iso_day(row["EX_DIVIDEND_DATE"][:10], "ex-dividend date")
        if not cutoff <= day <= end:
            continue
        record_day = iso_day(row["EQUITY_RECORD_DATE"][:10], "record date")
        payout = float(row["PRETAX_BONUS_RMB"])
        bonus, transfer = (float(row[key] or 0) for key in EVENT_FIELDS[2:])
        if (row["SECURITY_CODE"] != code[2:] or not math.isfinite(payout)
                or payout < 0 or bonus or transfer or float(row.get("TRANSFER_RATIO") or 0)
                or float(row.get("BONUS_IT_RATIO") or 0)):
            issues.append(gap(code, day, "dividend_event", "证券、派息或送转超出旧账本契约"))
        if day in prior and record_day != prior[day]:
            issues.append(gap(code, day, "EQUITY_RECORD_DATE", "登记日与除权日不相邻"))
        values = record_day, payout, bonus, transfer
        if day in result and result[day] != values:
            issues.append(gap(code, day, "dividend_event", "同日实施事件字段冲突"))
        result[day] = values
    return issues


def candidate_events(code, payload, missing):
    """Inspect §4.4 raw records without accepting a candidate into the ledger."""
    result = payload.get("result") or {}
    if payload.get("code") != 0 or not isinstance(result.get("data"), list):
        raise ValueError("Eastmoney §4.4 response has no records")
    return [row for row in result["data"] if row.get("SECURITY_CODE") == code[2:]
            and row.get("ASSIGN_PROGRESS") == "实施分配"
            and str(row.get("EX_DIVIDEND_DATE"))[:10] in missing]


def stage_inputs(root, snapshot, end, candidates):
    data = snapshot / "source/data"
    gaps = []
    for folder in ("income", "income_long", "income_plan"):
        for code in (CALENDAR, *CODES):
            for raw in sorted((root / "data" / folder).glob(f"{code}_*.csv.gz")):
                if re.fullmatch(r"(early_)?(daily|dividends?|hfq)", raw.name[9:-7]):
                    target = data / folder / raw.name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(raw, target)

    def rows(folder, name):
        file = data / folder / f"{name}.csv.gz"
        return csv_rows(file)[0] if file.is_file() else []

    calendar = rows("income_plan", f"{CALENDAR}_daily")
    sessions = sorted({row["date"] for row in calendar if row["date"] <= end})
    if end not in sessions:
        gaps.append(gap(CALENDAR, end, "daily", "看板结束日缺指数日期轴"))
    for code in (CALENDAR, *CODES):
        bars = rows("income_plan", f"{code}_daily")
        seen = {}
        for batch in (rows("income_long", f"{code}_early_daily"),
                      rows("income", f"{code}_daily"), bars):
            dates = [row["date"] for row in batch if row["date"] <= end]
            if dates != sorted(set(dates)):
                gaps.append(gap(code, end, "daily.date", "批次日期重复或乱序"))
            for row in batch:
                day = iso_day(row["date"], "daily date")
                if day > end:
                    continue
                values = tuple(float(row[field]) for field in ("open", "high", "low", "close", "volume"))
                if (row["code"] != code or any(not math.isfinite(v) for v in values)
                        or min(values[:4]) <= 0 or values[-1] < 0
                        or day in seen and values != seen[day]):
                    gaps.append(gap(code, day, "OHLCV", "行情身份、数值或跨批字段冲突"))
                seen[day] = values
        if not bars or end not in seen or not seen[end][-1]:
            gaps.append(gap(code, end, "daily", "缺少可用期末未复权行情"))
        if code == CALENDAR:
            continue
        factors = rows("income_plan", f"{code}_hfq") or rows("income", f"{code}_hfq")
        dates = [iso_day(row["date"], "hfq date") for row in factors]
        if not dates or len(set(dates)) != len(dates) or any(
            not math.isfinite(float(row["factor"])) or float(row["factor"]) <= 0 for row in factors
        ):
            gaps.append(gap(code, end, "hfq", "缺失、重复或无效的新浪个股后复权因子"))
        events = rows("income_plan", f"{code}_dividends")
        history = rows("income_long", f"{code}_early_dividend") + rows("income", f"{code}_dividend")
        cutoff = FIRST_DATES.get(code, "")
        gaps.extend(normalize_events(code, history + events, sessions, end, cutoff))
        effective = {r["EX_DIVIDEND_DATE"][:10] for r in history + events
                     if r["ASSIGN_PROGRESS"] == "实施分配"}
        first = min((day for day, values in seen.items() if day >= cutoff and values[-1]), default=end)
        missing = {day for day in dates if first < day <= end} - effective
        for day in sorted({day for day in effective if first < day <= end} - set(dates)):
            gaps.append(gap(code, day, "hfq", "实施日期缺少对应个股复权变动"))
        for day in sorted(missing):
            gaps.append(gap(code, day, "dividend_event", "复权因子有变动而缺少实施事件"))
        candidate = candidates / f"{code}.json" if candidates else None
        if candidate and candidate.is_file():
            raw = snapshot / "candidates" / candidate.name
            raw.parent.mkdir(exist_ok=True)
            shutil.copyfile(candidate, raw)
            write_json(raw.with_suffix(".selected.json"), {
                "status": "pending_decision", "method": "a-stock-data 3.10.4 §4.4",
                "raw_field": "PRETAX_BONUS_RMB", "legacy_unit": "CNY per 10 shares",
                "rows": candidate_events(code, json.loads(raw.read_bytes()), missing),
            })
    if gaps:
        raise InputGaps(gaps)


def configuration(dashboard):
    weights = dashboard.portfolios()["original"]
    if (set(weights) != set(CODES) or dashboard.CAPITAL != 500_000
            or dashboard.FIRST_DATES != FIRST_DATES):
        raise ValueError("legacy portfolio differs from the frozen baseline contract")
    return {"weights": weights, "capital": dashboard.CAPITAL, "bank_codes": sorted(dashboard.ORIGINAL_BANKS),
            "bank_band": [.4 - .05, .4 + .05], "core_fraction": .9, "tactical_fraction": .1,
            "policy": "cash_then_sell", "core_entry": "kdj_deadline", "core_filter": dashboard.SHAANGU,
            "range_weeks": 52, "range_maximum": .2, "yield_years": 1, "yield_thresholds": [.2, .8],
            "start": dashboard.LONG_START, "price_calendar": CALENDAR,
            "anchor": dashboard.INCOME_ANCHOR, "first_dates": dashboard.FIRST_DATES}


def suspension_days(snapshot):
    path = snapshot / "suspension-evidence.json"
    if not path.is_file():
        return set()
    evidence = json.loads(path.read_bytes())
    if evidence["method"] != "a-stock-data 3.10.4 §6.5":
        raise ValueError("unsupported suspension evidence method")
    days = set()
    for item in evidence["rows"]:
        code, day = item["code"], iso_day(item["date"], "suspension evidence")
        key = code, day
        record = item["baostock"]
        if (key in days or code not in CODES or record["date"] != day
                or record["code"] != f"{code[:2]}.{code[2:]}" or record["tradestatus"] != "0"):
            raise ValueError("invalid or duplicate BaoStock suspension evidence")
        if any(float(record[field] or 0) != 0 for field in ("volume", "amount")):
            raise ValueError("BaoStock suspension record conflicts with recorded trades")
        tencent, tdx = item["tencent"], item["tdx"]
        if ((tencent is not None and (tencent[0] != day or float(tencent[5]) != 0))
                or (tdx is not None and (tdx["date"] != day or tdx["market"] + tdx["code"] != code
                                        or any(float(tdx[key]) != 0 for key in ("volume", "amount"))))):
            raise ValueError("cross-source evidence conflicts with BaoStock suspension")
        days.add((code, day))
    return days


def session_price_gaps(days, inputs, start):
    """Compare the exchange calendar with the tradable bars used by the legacy loader."""
    gaps = []
    for code, (raw, *_) in inputs.items():
        if not raw:
            continue  # Empty coverage is rejected by the loading or warmup checks.
        first = min(raw)
        for day in days:
            if day < first or day in raw:
                continue
            impact = ("缺可用未复权行情，可能影响开盘成交、收盘估值或信号"
                      if day >= start else "预热缺可用未复权行情，可能影响周线信号")
            gaps.append(input_gap(
                code, day, "daily_bar", impact,
                ["核对交易所停牌或休市证据", "按固定方法准备候选原始行情，交负责人裁决"],
            ))
    return gaps


def check_warmup(days, inputs, start, end, first_dates, histories):
    issues, coverage = [], {}
    for code, (raw, _, _, readings, _) in inputs.items():
        first = min(raw)
        # Legacy listing/cutoff dates can intentionally delay signal readiness.
        cutoff = first_dates.get(code)
        if (first > (cutoff or start) and code != "sh600941"
                or not readings or not histories[code]
                or (code != "sh600941" and cutoff != first and not any(d < start for d in readings))):
            issues.append(gap(code, first, "warmup", "预热覆盖不足或在原起点缺少行情"))
        coverage[code] = {"first_bar": first, "last_bar": max(raw),
                          "first_kdj_reading": min(readings, default=None),
                          "first_yield_rank": min(histories[code], default=None)}
    if not days or days[-1] != end:
        issues.append(gap(CALENDAR, end, "daily", "交易轴未覆盖结束日"))
    if issues:
        raise InputGaps(issues)
    return coverage


def ledger_equal(left, right):
    return math.isclose(left, right, rel_tol=0, abs_tol=TOLERANCES["ledger"])


def ledger(days, daily, inputs, accounts, bank_trades, capital):
    fills, by_day, holdings, marks = [], defaultdict(list), dict.fromkeys(inputs, 0), {}
    for code, (path, _, trades) in accounts.items():
        if [row["day"] for row in path] != days:
            raise ValueError("stock ledger date axis mismatch")
        for trade in trades:
            price_value = trade["shares"] * trade["price"]
            fee = abs(trade["cash_trade"]) - price_value if trade["side"] != "sell" else price_value - trade["cash_trade"]
            fills.append(dict(code=code, fee=fee, **trade))
    fills.extend(dict(row, side=row["action"]) for row in bank_trades if row["shares"])
    fills.sort(key=lambda row: row["day"])
    for fill in fills:
        by_day[fill["day"]].append(fill)
    legacy_balance = capital
    total_fees = 0.0
    audited_daily = []
    for index, row in enumerate(daily):
        day = row["day"]
        gross = sum(path[index]["gross_dividend"] for path, _, _ in accounts.values())
        tax = sum(path[index]["dividend_tax"] for path, _, _ in accounts.values())
        for code, (_, dividends, *_) in inputs.items():
            expected = holdings[code] * dividends.get(day, 0)
            if not ledger_equal(expected, accounts[code][0][index]["gross_dividend"]):
                raise ValueError("dividend differs from opening shares times cash per share")
        if not ledger_equal(tax, sum(fill["tax"] for fill in by_day[day])):
            raise ValueError("dividend tax ledger mismatch")
        # Reconcile the legacy simulation, which credits income at ex-date.
        # No settlement calendar is available to reconstruct spendable cash.
        legacy_balance += gross - tax
        for fill in by_day[day]:
            sign = -1 if fill["side"] in {"sell", "core_sell"} else 1
            holdings[fill["code"]] += sign * fill["shares"]
            legacy_balance -= sign * fill["shares"] * fill["price"] + fill["fee"]
            total_fees += fill["fee"]
        stock_values = {}
        for code, (raw, *_) in inputs.items():
            if day in raw:
                marks[code] = raw[day][1]
            equity = holdings[code] * marks.get(code, 0)
            expected = accounts[code][0][index]["equity"]
            if holdings[code] < 0 or not ledger_equal(equity, expected):
                raise ValueError("holding ledger mismatch")
            stock_values[code] = equity
        if not ledger_equal(legacy_balance + sum(stock_values.values()), row["value"]):
            raise ValueError("legacy balance plus marked shares differs from daily value")
        if "cash" in row and (not ledger_equal(legacy_balance, row["cash"]) or any(
            not ledger_equal(value, row["stock_values"][code]) for code, value in stock_values.items()
        )):
            raise ValueError("reconstructed allocations differ from legacy accounts")
        audited = dict(row, legacy_book_balance=legacy_balance, stock_values=stock_values, holdings=holdings.copy(),
                       marks=marks.copy())
        audited.pop("cash", None)
        audited_daily.append(audited)
    if not ledger_equal(total_fees, sum(detail["fees"] for _, detail, _ in accounts.values())):
        raise ValueError("fee ledger mismatch")
    return fills, audited_daily


def execute(snapshot, reference, *, diagnostic=False):
    root = snapshot / "source"
    sys.path.insert(0, str(root / "strategy-dashboard"))
    # Run in the CLI's own process and never write bytecode into the snapshot.
    sys.dont_write_bytecode = True
    dashboard = importlib.import_module("daily_update")
    bank = importlib.import_module("income_bank_rebalance")
    config = configuration(dashboard)
    end = reference["stats"]["income"]["end"]
    market, coverage, loaded, details, accounts = {}, {}, {}, {}, {}
    price_gaps = []
    suspended = suspension_days(snapshot)
    for code in (CALENDAR, *CODES):
        factor_rows = [] if code == CALENDAR else dashboard.load(root / "data/income_plan" / f"{code}_hfq.csv.gz")
        market[code] = [], factor_rows

    def load_inputs(*args, **kwargs):
        days, inputs = original_load(*args, **kwargs)
        histories = dashboard.yield_history(days, inputs, 1)
        detected = session_price_gaps(days, inputs, config["start"])
        detected_keys = {(item["asset"], item["date"]) for item in detected}
        if suspended - detected_keys:
            raise ValueError("suspension evidence refers to days outside the detected gaps")
        price_gaps.extend(item for item in detected if (item["asset"], item["date"]) not in suspended)
        try:
            coverage.update(check_warmup(days, inputs, config["start"], end, config["first_dates"], histories))
        except InputGaps as exc:
            raise InputGaps(price_gaps + exc.gaps) from exc
        if price_gaps and not diagnostic:
            raise InputGaps(price_gaps)
        loaded.update(inputs)
        details["signals"] = histories
        return days, inputs

    def capture_stock(sessions, inputs, *args, **kwargs):
        output = yield from original_stock(sessions, inputs, *args, **kwargs)
        accounts[next(code for code, member in loaded.items() if inputs is member)] = output
        return output

    def simulate(*args, **kwargs):
        summary, daily, events, trades = original_simulate(*args, **kwargs)
        fills, audited_daily = ledger(args[0], daily, loaded, accounts, trades, config["capital"])
        details.update(summary=summary, daily=audited_daily, rebalance_events=events,
                       rebalance_transfers=trades, fills=fills, stock_ledgers=accounts)
        return summary, daily, events, trades

    def reject_network(*args, **kwargs):
        raise RuntimeError("network unavailable during replay")

    original_load, original_stock, original_simulate = dashboard.load_inputs, bank.stock_sessions, dashboard.income_simulate
    with (patch("socket.socket.connect", reject_network), patch("socket.getaddrinfo", reject_network),
          patch.object(dashboard, "load_inputs", load_inputs), patch.object(bank, "stock_sessions", capture_stock),
          patch.object(dashboard, "income_simulate", simulate), patch.object(dashboard, "append_dividends", return_value=[])):
        result = dashboard.income_replay(reference, market, end)
    return config, coverage, result, details, price_gaps


def compare(reference, result):
    nav, stats, _, dividends, allocations = result
    checked = {}

    def compare_series(name, expected, actual, tolerance):
        expected_days = [row[0] for row in expected]
        actual_days = [row[0] for row in actual]
        differences = [day for (day, baseline), (_, value) in zip(expected, actual)
                       if not math.isclose(baseline, value, rel_tol=0, abs_tol=tolerance)]
        checked[name] = {"match": expected_days == actual_days and not differences,
                         "date_axis_match": expected_days == actual_days, "mismatch_dates": differences}

    compare_series("nav", reference["history"]["income"], nav, TOLERANCES["nav"])
    if "incomeDividends" in reference:
        compare_series("net_dividends", reference["incomeDividends"], dividends, TOLERANCES["cash"])
    for field, value in stats.items():
        if field in reference["stats"]["income"]:
            expected = reference["stats"]["income"][field]
            match = expected == value if isinstance(value, str) else math.isclose(
                expected, value, rel_tol=0, abs_tol=TOLERANCES["cash" if field == "net_dividends" else "rate"])
            checked[field + "_summary"] = {"match": match}
    for day, row in reference.get("incomeAllocations", {}).items():
        current = allocations.get(day, {})
        compared = {"cash": current.get("cash"), **current.get("stocks", {})}
        expected = {"cash": row["cash"], **row["stocks"]}
        checked[f"allocations:{day}"] = {"match": compared.keys() == expected.keys() and all(
            math.isclose(compared[k], value, rel_tol=0, abs_tol=TOLERANCES["cash"])
            for k, value in expected.items())}
    unavailable = ["action_dates", "fills", "fees", "share_quantities"]
    if not reference.get("incomeAllocations"):
        unavailable.extend(("legacy_book_balance", "stock_values"))
    return {"fields": checked, "reference_unavailable": unavailable,
            "match": all(field["match"] for field in checked.values())}


def study_reference(root, snapshot, start, end):
    folder = root / "results/income/bank_selection/midea_replacement"
    for name in ("daily", "summary", "rebalance_events"):
        file = folder / f"{name}.csv"
        if not file.is_file():
            continue
        rows, fields = csv_rows(file)
        selected = [row for row in rows if row["scenario"] == "original" and row["policy"] == "cash_then_sell"
                    and (start <= row["day"] <= end if "day" in row else row["start"] == start and row["end"] <= end)]
        if selected:
            write_csv(snapshot / "study" / file.name, selected, fields)


def compare_study(snapshot, details):
    study = snapshot / "study"
    if not study.exists():
        return {"available": False}
    checks = {}
    actions = {
        "daily": ("day", TOLERANCES["nav"], ("nav", "bank_weight", "stock_weight")),
        "rebalance_events": ("day", TOLERANCES["rate"], ("bank_weight_before", "target_boundary", "bank_weight_after_close")),
        "summary": ("end", TOLERANCES["rate"], ()),
    }
    for name, (date_field, tolerance, rate_fields) in actions.items():
        file = study / f"{name}.csv"
        if not file.is_file():
            continue
        expected, _ = csv_rows(file)
        if name == "summary":
            end = max(row["end"] for row in expected)
            expected = [row for row in expected if row["end"] == end]
            fills = [fill for fill in details["fills"] if fill["day"] <= end]
            actual = [{"end": end, "total_fees": sum(fill["fee"] for fill in fills),
                       "total_dividend_tax": sum(fill["tax"] for fill in fills)}]
            money_fields, text_fields = ("total_fees", "total_dividend_tax"), ()
        else:
            end = max(row["day"] for row in expected)
            actual = [row for row in details[name] if row["day"] <= end]
            money_fields = (("value", "net_dividends") if name == "daily" else
                            ("shifted", "income_used", "free_cash_used", "gross_sold", "fees", "dividend_tax"))
            text_fields = () if name == "daily" else ("check_day", "status")
        mismatches = []
        for historical, current in zip(expected, actual):
            fields = [key for key in text_fields if historical[key] != current[key]]
            for limits, keys in ((tolerance, rate_fields), (TOLERANCES["cash"], money_fields)):
                for key in keys:
                    if current[key] is None and historical[key] == "":
                        continue
                    if current[key] is None or not historical[key] or not math.isclose(
                        float(historical[key]), current[key], rel_tol=0, abs_tol=limits
                    ):
                        fields.append(key)
            if fields:
                mismatches.append({"date": historical[date_field], "fields": fields})
        same_dates = [row[date_field] for row in expected] == [row[date_field] for row in actual]
        checks[name] = {"match": same_dates and not mismatches, "date_axis_match": same_dates, "mismatches": mismatches}
    return {"available": True, "fields": checks}


def result_fields(config, comparison, secondary, details):
    """Label model-specific cash fields at the serialization boundary."""
    names = {
        "cash": "modeled_transfer_amount",
        "cash_trade": "modeled_trade_cash_flow",
        "dividend_cash": "legacy_income_balance",
        "free_cash_used": "legacy_free_cash_used",
        "income_used": "legacy_income_used",
        "rebalanced_cash": "legacy_rebalanced_amount",
    }

    def rename(row):
        return {names.get(key, key): value for key, value in row.items()}

    return {
        **details,
        "summary": rename(details["summary"]),
        "rebalance_events": [rename(row) for row in details["rebalance_events"]],
        "rebalance_transfers": [rename(row) for row in details["rebalance_transfers"]],
        "fills": [rename(row) for row in details["fills"]],
        "stock_ledgers": {code: (path, rename(summary), [rename(row) for row in trades])
                          for code, (path, summary, trades) in details["stock_ledgers"].items()},
        "environment": environment(), "configuration": config, "balance_basis": BALANCE_BASIS,
        "settled_cash_available": None, "comparison": comparison, "study_comparison": secondary,
    }


def freeze(args):
    root = args.legacy_root.resolve()
    snapshot = local_path(args.snapshot, "snapshot")
    snapshot.mkdir(mode=0o700, parents=True)
    archive_source(root, snapshot / "source")
    for path in (Path(__file__), Path(__file__).with_name("eternal_baseline.py")):
        shutil.copyfile(path, snapshot / "source" / path.name)
    full_reference = json.loads(read_revision(root, "data.json"))
    reference = {key: full_reference[key] for key in
                 ("stocks", "incomeInitialCapital", "incomeDividends", "incomeAllocations") if key in full_reference}
    for key in ("history", "stats"):
        reference[key] = {"income": full_reference[key]["income"]}
    write_json(snapshot / "reference.json", reference)
    end = reference["stats"]["income"]["end"]
    try:
        if args.suspension_evidence is not None:
            evidence = local_path(args.suspension_evidence, "suspension evidence")
            shutil.copyfile(evidence, snapshot / "suspension-evidence.json")
        stage_inputs(root, snapshot, end, args.candidates)
        config, coverage, result, details, price_gaps = execute(snapshot, reference, diagnostic=True)
        if config["capital"] != reference["incomeInitialCapital"]:
            raise ValueError("historical starting capital differs from the legacy configuration")
        comparison = compare(reference, result)
        parity_gaps = [gap("portfolio", end, field, "固定看板参考对拍不符")
                       for field, check in comparison["fields"].items() if not check["match"]]
        study_reference(root, snapshot, config["start"], end)
        if price_gaps or parity_gaps:
            diagnostic = result_fields(config, comparison, compare_study(snapshot, details), details)
            write_json(snapshot / "pending-diagnostic.json", {"status": "pending_decision", **diagnostic})
            raise InputGaps(price_gaps + parity_gaps)
    except (ValueError, RuntimeError, KeyError, TypeError, AssertionError) as exc:
        match = re.search(r"(sh\d{6})(?: (\d{4}-\d{2}-\d{2}))?", str(exc))
        gaps = exc.gaps if isinstance(exc, InputGaps) else [
            gap(match[1] if match else "portfolio", (match[2] if match else None) or end,
                "legacy_input_or_ledger", str(exc))]
        write_json(snapshot / "pending-gaps.json", {"status": "pending_decision", "gaps": gaps})
        return 2
    manifest = {"schema_version": SCHEMA_VERSION, "strategy": "income", "balance_basis": BALANCE_BASIS, "dashboard_revision": REVISION,
                "source_provenance": "dashboard files from Git; adjacent legacy modules frozen from working files",
                "environment": environment(), "configuration": config, "coverage": coverage, "end": end,
                "method": "a-stock-data 3.10.4", "volume_unit": "Tencent raw lots",
                "event_payout_unit": "CNY per 10 shares", "comparison_tolerances": TOLERANCES,
                "files": digest_files(snapshot)}
    write_json(snapshot / "manifest.json", seal_manifest(manifest))
    return 0


def replay(args):
    snapshot = local_path(args.snapshot, "snapshot")
    output = local_path(args.output, "output")
    if snapshot == output or snapshot in output.parents:
        raise ValueError("output overlaps snapshot")
    if (snapshot / "pending-gaps.json").exists():
        raise ValueError("snapshot requires input decisions")
    manifest_file = snapshot / "manifest.json"
    manifest = json.loads(manifest_file.read_bytes())
    expected = dict(manifest)
    actual_files = digest_files(snapshot)
    del actual_files["manifest.json"]
    if (manifest["schema_version"] != SCHEMA_VERSION or manifest["strategy"] != "income"
            or manifest["balance_basis"] != BALANCE_BASIS
            or seal_manifest(expected) != manifest or manifest["files"] != actual_files):
        raise ValueError("snapshot integrity check failed")
    for file in (Path(__file__), Path(__file__).with_name("eternal_baseline.py")):
        if sha256_file(file) != manifest["files"][f"source/{file.name}"]:
            raise ValueError("replay adapter version differs from snapshot")
    if manifest["environment"] != environment() or manifest["comparison_tolerances"] != TOLERANCES:
        raise ValueError("replay environment or tolerances differ from snapshot")
    reference = json.loads((snapshot / "reference.json").read_bytes())
    config, coverage, result, details, _ = execute(snapshot, reference)
    if config != manifest["configuration"] or coverage != manifest["coverage"]:
        raise ValueError("effective configuration or coverage differs from snapshot")
    comparison = compare(reference, result)
    secondary = compare_study(snapshot, details)
    write_json(output, {"input_version": manifest["input_version"],
                        **result_fields(config, comparison, secondary, details)})
    return 0 if comparison["match"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    for name, handler in (("freeze", freeze), ("replay", replay)):
        command = subcommands.add_parser(name)
        command.add_argument("--snapshot", type=Path, required=True)
        command.set_defaults(handler=handler)
    subcommands.choices["freeze"].add_argument("--legacy-root", type=Path, required=True)
    subcommands.choices["freeze"].add_argument("--candidates", type=Path)
    subcommands.choices["freeze"].add_argument("--suspension-evidence", type=Path)
    subcommands.choices["replay"].add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        return args.handler(args)
    except (OSError, ValueError, RuntimeError, KeyError, TypeError, AssertionError, subprocess.CalledProcessError):
        parser.exit(2, "income baseline failed; check private paths, snapshot and input files\n")


if __name__ == "__main__":
    sys.exit(main())
