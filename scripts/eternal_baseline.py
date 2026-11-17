#!/usr/bin/env python3
"""Freeze and replay the local Eternal ETF baseline without network access."""

import argparse
import ast
import csv
import gzip
import hashlib
import json
import math
import os
import platform
import shutil
import sys
import tempfile
import time
from bisect import bisect_right
from datetime import date, datetime, timedelta, timezone
from decimal import Decimal, ROUND_HALF_UP
from pathlib import Path
from statistics import stdev
from urllib.parse import urlencode
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
ANCHOR = '2026-11-09'
ETF_CODES = ('sh511260', 'sh518880', 'sh515080', 'sh513300', 'sz159612')
LEGACY_PAIRS = (('sh513100', 'sh513300'), ('sh513500', 'sz159612'))
HISTORY_CODES = (*ETF_CODES, 'sh513100', 'sh513500')
WEIGHTS = dict(zip(ETF_CODES, (.30, .20, .20, .25, .05), strict=True))
THRESHOLD = .10
FEE_RATE = .0005
NAV_TOLERANCE = 1e-7
TENCENT_HOSTS = (
    'https://web.ifzq.gtimg.cn',
    'https://proxy.finance.qq.com/ifzqgtimg',
    'https://ifzq.gtimg.cn',
)
SINA_URL = 'https://finance.sina.com.cn/realstock/company/{code}/hfq.js'
RAW_BAR_FIELDS = ('date', 'code', 'open', 'close', 'high', 'low', 'volume', 'source')


class InputGaps(ValueError):
    def __init__(self, gaps):
        self.gaps = gaps
        super().__init__(f'输入缺口待负责人裁决: {gaps[0]["impact"]}')


def input_gap(code, day, field, impact, candidates):
    return {'asset': code, 'date': day, 'field': field, 'impact': impact,
            'candidate_measures': candidates}


def legacy_source_closure(root):
    pending = [root / 'backtest.py', root / 'dividend_data.py',
               root / 'strategy-dashboard/daily_update.py']
    found = set()
    search_roots = (root, root / 'strategy-dashboard')
    while pending:
        path = pending.pop()
        if path in found:
            continue
        found.add(path)
        tree = ast.parse(path.read_text(encoding='utf-8-sig'), filename=str(path))
        for node in ast.walk(tree):
            names = []
            if isinstance(node, ast.Import):
                names = [alias.name for alias in node.names]
                bases = search_roots
            elif isinstance(node, ast.ImportFrom):
                if node.level:
                    base = path.parent
                    for _ in range(node.level - 1):
                        base = base.parent
                    bases = (base,)
                else:
                    bases = search_roots
                module = node.module or ''
                names = [module] if module else []
                names.extend(f'{module}.{alias.name}'.strip('.') for alias in node.names
                             if alias.name != '*')
            else:
                continue
            for name in names:
                for base in bases:
                    module_path = base.joinpath(*name.split('.'))
                    for candidate in (module_path.with_suffix('.py'), module_path / '__init__.py'):
                        if candidate.is_file() and root in candidate.resolve().parents:
                            pending.append(candidate)
    return sorted(found)


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def csv_rows(path, compressed=None):
    if compressed is None:
        compressed = path.suffix == '.gz'
    opener = gzip.open if compressed else open
    with opener(path, 'rt', encoding='utf-8-sig', newline='') as stream:
        reader = csv.DictReader(stream)
        if not reader.fieldnames:
            raise ValueError(f'{path.name} 缺少 CSV 表头')
        return list(reader), list(reader.fieldnames)


def write_csv(path, rows, fields):
    path.parent.mkdir(parents=True, exist_ok=True)
    opener = gzip.open if path.suffix == '.gz' else open
    with opener(path, 'wt', encoding='utf-8', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=fields, extrasaction='raise')
        writer.writeheader()
        writer.writerows(rows)


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False,
                  default=json_default)
        stream.write('\n')


def json_default(value):
    if isinstance(value, date):
        return value.isoformat()
    raise TypeError(f'not JSON serializable: {type(value).__name__}')


def canonical_json(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()


def execution_configuration():
    return {
        'assets': list(ETF_CODES),
        'weights': WEIGHTS,
        'threshold': THRESHOLD,
        'fee_rate_one_way': FEE_RATE,
        'rebalance_execution': 'signal at close; execute whole portfolio next common open',
        'initial_nav_basis': 1.0,
        'initial_capital_currency_amount': None,
        'initial_capital_note': 'legacy simulator normalizes initial NAV to 1.0; no CNY amount is available',
        'share_precision_note': 'holdings are legacy effective total-return-index units, not ETF share quantities',
    }


def manifest_identity_material(manifest):
    return {
        key: value for key, value in manifest.items()
        if key not in {'input_version', 'manifest_sha256'}
    }


def manifest_digest_material(manifest):
    return {key: value for key, value in manifest.items() if key != 'manifest_sha256'}


def seal_manifest(manifest):
    manifest['input_version'] = hashlib.sha256(
        canonical_json(manifest_identity_material(manifest)),
    ).hexdigest()
    manifest['manifest_sha256'] = hashlib.sha256(
        canonical_json(manifest_digest_material(manifest)),
    ).hexdigest()
    return manifest


def validate_execution_configuration(manifest):
    configuration = manifest.get('configuration')
    if not isinstance(configuration, dict) or canonical_json(configuration) != canonical_json(
        execution_configuration(),
    ):
        raise ValueError('snapshot execution configuration does not match replay source')


def validate_replay_source(manifest, replay_script):
    source_hash = manifest['files']['source/eternal_baseline.py']['sha256']
    replay_hash = sha256_file(replay_script)
    if replay_hash != source_hash:
        raise ValueError('回放源码与 snapshot 冻结源码不一致')
    return replay_hash, source_hash


def validate_iso_date(value, label):
    if not isinstance(value, str):
        raise ValueError(f'{label} 日期不是字符串')
    try:
        parsed = date.fromisoformat(value)
    except ValueError as exc:
        raise ValueError(f'{label} 日期无效') from exc
    if parsed.isoformat() != value:
        raise ValueError(f'{label} 日期不是 ISO 格式')


def parse_tencent_bars(body, code, start, end):
    payload = json.loads(body.decode('utf-8-sig'))
    market_data = payload.get('data')
    code_data = market_data.get(code) if isinstance(market_data, dict) else None
    records = code_data.get('day') if isinstance(code_data, dict) else None
    if not isinstance(records, list):
        raise ValueError(f'{code} 腾讯日线响应缺少 day 列表')
    rows = []
    for record in records:
        if not isinstance(record, list) or len(record) < 6:
            raise ValueError(f'{code} 腾讯日线记录结构无效')
        day, open_, close, high, low, volume, *_ = record
        validate_iso_date(day, code)
        if not start <= day <= end:
            raise ValueError(f'{code} 腾讯日线返回日期越界')
        rows.append({
            'date': day, 'code': code, 'open': open_, 'close': close,
            'high': high, 'low': low, 'volume': volume, 'source': 'tencent_response',
        })
    dates = [row['date'] for row in rows]
    if not dates or dates != sorted(set(dates)):
        raise ValueError(f'{code} 腾讯日线为空或日期异常')
    return rows


def fetch_tencent_bars(code, start, end, response_dir, request_log):
    cursor, stop = date.fromisoformat(start), date.fromisoformat(end)
    result = []
    while cursor <= stop:
        last = min(cursor + timedelta(days=699), stop)
        params = urlencode({'param': f'{code},day,{cursor},{last},640,'})
        for host_index, host in enumerate(TENCENT_HOSTS):
            url = f'{host}/appstock/app/fqkline/get?{params}'
            request = Request(url, headers={
                'Referer': 'https://gu.qq.com/',
                'User-Agent': 'Mozilla/5.0',
            })
            attempt = {
                'source': 'Tencent daily',
                'code': code,
                'start': cursor.isoformat(),
                'end': last.isoformat(),
                'url': url,
                'started_at_utc': datetime.now(timezone.utc).isoformat(),
            }
            try:
                with urlopen(request, timeout=35) as response:
                    body = response.read()
                    attempt['http_status'] = response.getcode()
                    attempt['response_url'] = response.geturl()
                raw_path = response_dir / f'{code}-{cursor}-{last}-{host_index}.json'
                raw_path.parent.mkdir(parents=True, exist_ok=True)
                raw_path.write_bytes(body)
                attempt['raw_response'] = raw_path.relative_to(response_dir.parent.parent).as_posix()
                result.extend(parse_tencent_bars(body, code, cursor.isoformat(), last.isoformat()))
                attempt['status'] = 'success'
                attempt['response_bytes'] = len(body)
                break
            except (OSError, KeyError, TypeError, ValueError) as exc:
                attempt['status'] = 'failed'
                attempt['error'] = f'{type(exc).__name__}: {exc}'
            finally:
                attempt['completed_at_utc'] = datetime.now(timezone.utc).isoformat()
                request_log.append(attempt)
        else:
            raise InputGaps([input_gap(
                code, f'{cursor}/{last}', 'daily_bar',
                '腾讯日线取数失败，无法确认行情覆盖',
                ['核查请求记录及原始来源', '由负责人裁决补采或其他措施'],
            )])
        cursor = last + timedelta(days=1)
    dates = [row['date'] for row in result]
    if dates != sorted(set(dates)):
        raise ValueError(f'{code} 腾讯日线分段后有重复或乱序日期')
    return result


def parse_sina_factors(body, code):
    text = body.decode('utf-8-sig')
    start = text.find('{')
    if start < 0:
        raise ValueError(f'{code} 新浪复权因子响应缺少 JSON')
    payload, _ = json.JSONDecoder().raw_decode(text[start:])
    records = payload.get('data')
    if not isinstance(records, list) or not all(isinstance(row, dict) for row in records):
        raise ValueError(f'{code} 新浪复权因子响应缺少 data 列表')
    rows = [
        {'date': row['d'], 'split': row['s'], 'dividend': row['u']}
        for row in records if float(row['f']) == 1
    ]
    if not rows or len(rows) != int(payload['total']):
        raise ValueError(f'{code} 新浪基金复权因子不完整')
    rows.sort(key=lambda row: row['date'])
    dates = [row['date'] for row in rows]
    for day in dates:
        validate_iso_date(day, code)
    if dates != sorted(set(dates)):
        raise ValueError(f'{code} 新浪复权因子日期重复或无序')
    if any(not math.isfinite(float(row['split'])) or float(row['split']) <= 0
           or not math.isfinite(float(row['dividend'])) for row in rows):
        raise ValueError(f'{code} 新浪复权因子数值无效')
    return rows


def fetch_sina_factors(code, raw_path, request_log):
    url = SINA_URL.format(code=code)
    request = Request(url, headers={
        'Referer': 'https://finance.sina.com.cn/',
        'User-Agent': 'Mozilla/5.0',
    })
    attempt = {
        'source': 'Sina fund factors',
        'code': code,
        'url': url,
        'started_at_utc': datetime.now(timezone.utc).isoformat(),
    }
    try:
        with urlopen(request, timeout=35) as response:
            body = response.read()
            attempt['http_status'] = response.getcode()
            attempt['response_url'] = response.geturl()
        raw_path.parent.mkdir(parents=True, exist_ok=True)
        raw_path.write_bytes(body)
        attempt['raw_response'] = raw_path.relative_to(raw_path.parents[2]).as_posix()
        rows = parse_sina_factors(body, code)
        attempt['status'] = 'success'
        attempt['response_bytes'] = len(body)
        return rows
    except (OSError, KeyError, TypeError, ValueError) as exc:
        attempt['status'] = 'failed'
        attempt['error'] = f'{type(exc).__name__}: {exc}'
        raise InputGaps([input_gap(
            code, None, 'factor', '新浪基金复权因子取数或校验失败',
            ['核查请求记录及原始因子', '由负责人裁决补采或其他措施'],
        )]) from exc
    finally:
        attempt['completed_at_utc'] = datetime.now(timezone.utc).isoformat()
        request_log.append(attempt)


def selected_rows(path, predicate):
    rows, fields = csv_rows(path)
    selected = [row for row in rows if predicate(row)]
    return selected, fields


def normalize_bar(row, source):
    missing = [field for field in ('date', 'code', 'open', 'close', 'high', 'low', 'volume')
               if field not in row]
    if missing:
        raise ValueError(f'行情记录缺少必要字段: {", ".join(missing)}')
    return {**{field: row[field] for field in RAW_BAR_FIELDS[:-1]}, 'source': source}


def ensure_ordered_bars(code, rows):
    dates = [row['date'] for row in rows]
    if not dates or dates != sorted(set(dates)):
        raise ValueError(f'{code} 行情为空、日期重复或非递增')
    for day in dates:
        validate_iso_date(day, code)
    if any(row['code'] != code for row in rows):
        raise ValueError(f'{code} 行情包含错误标的')
    for row in rows:
        try:
            volume = float(row['volume'])
        except (KeyError, TypeError, ValueError) as exc:
            raise InputGaps([input_gap(
                code, row['date'], 'volume', '成交量无效，不能作为可成交行情',
                ['核查原始行情', '由负责人决定如何处理'],
            )]) from exc
        if not math.isfinite(volume) or volume <= 0:
            raise InputGaps([input_gap(
                code, row['date'], 'volume', '不能作为可成交行情',
                ['核查停牌和行情来源', '由负责人决定如何处理'],
            )])


def private_path(path, label):
    resolved = path.expanduser().resolve()
    roots = [ROOT]
    cwd = Path.cwd().resolve()
    roots.extend(parent for parent in (cwd, *cwd.parents) if (parent / '.git').exists())
    additional_root = os.environ.get('PRAJNA_ETERNAL_BASELINE_REPOSITORY_ROOT')
    if additional_root:
        roots.append(Path(additional_root).expanduser().resolve())
    if any(resolved == root or root in resolved.parents for root in roots):
        raise ValueError(f'{label} 必须位于代码仓库之外')
    return resolved


def freeze(args):
    source_root = args.legacy_root.expanduser().resolve()
    target = private_path(args.snapshot, 'snapshot')
    if target == source_root or source_root in target.parents:
        raise ValueError('snapshot 不能写入旧项目目录')
    if target.exists():
        raise FileExistsError(f'snapshot 已存在，不能覆盖: {target}')
    source_paths = {
        'tencent_archive': source_root / 'data/long_history/tencent_daily.csv.gz',
        'nasdaq_archive': source_root / 'strategy-dashboard/nasdaq-513300.csv.gz',
        'etf_archive': source_root / 'data/etf_daily.csv',
        'reference_output': source_root / 'strategy-dashboard/data.json',
        'legacy_backtest': source_root / 'backtest.py',
        'legacy_dashboard': source_root / 'strategy-dashboard/daily_update.py',
        'legacy_data_source': source_root / 'dividend_data.py',
    }
    missing = [name for name, path in source_paths.items() if not path.is_file()]
    if missing:
        raise FileNotFoundError(f'缺少本机输入文件: {", ".join(missing)}')
    source_hashes_before = {name: sha256_file(path) for name, path in source_paths.items()}
    legacy_sources = legacy_source_closure(source_root)
    legacy_hashes_before = {path.relative_to(source_root).as_posix(): sha256_file(path)
                            for path in legacy_sources}
    reference_data = json.loads(source_paths['reference_output'].read_text(encoding='utf-8'))
    try:
        reference_rows = reference_data['history']['etf']
        reference_stats = reference_data['stats']['etf']
    except (KeyError, TypeError) as exc:
        raise ValueError('参考输出缺少 history.etf 或 stats.etf') from exc
    if (not isinstance(reference_rows, list) or not reference_rows
            or any(not isinstance(row, list) or len(row) != 2 for row in reference_rows)
            or not isinstance(reference_stats, dict)):
        raise ValueError('参考输出的 ETF 净值序列为空或结构无效')
    reference_dates = [row[0] for row in reference_rows]
    for row in reference_rows:
        validate_iso_date(row[0], '参考输出')
        if not isinstance(row[1], (int, float)) or not math.isfinite(float(row[1])):
            raise ValueError('参考输出含无效 NAV')
    if reference_dates != sorted(set(reference_dates)):
        raise ValueError('参考输出日期重复或非递增')
    start, end = reference_dates[0], reference_dates[-1]
    if any(date.fromisoformat(day).isoformat() != day for day in reference_dates):
        raise ValueError('参考输出包含无效日期')

    parent = target.parent
    parent.mkdir(parents=True, exist_ok=True)
    lock_path = parent / f'.{target.name}.freeze.lock'
    lock_fd = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(lock_fd)
    work = None
    request_log = []
    try:
        work = Path(tempfile.mkdtemp(prefix=f'.{target.name}.', suffix='.part', dir=parent))
        tencent_rows, tencent_fields = selected_rows(
            source_paths['tencent_archive'], lambda row: row.get('code') in HISTORY_CODES)
        tencent_by_code = {code: [] for code in HISTORY_CODES}
        for row in tencent_rows:
            tencent_by_code[row['code']].append(row)
        nasdaq_rows, nasdaq_fields = csv_rows(source_paths['nasdaq_archive'])
        if not nasdaq_rows or any(row.get('code') != 'sh513300' for row in nasdaq_rows):
            raise ValueError('本机纳指档案为空或包含其他标的')
        etf_rows, etf_fields = selected_rows(
            source_paths['etf_archive'],
            lambda row: row.get('code') == 'sz159612'
            and row.get('date', '') <= ANCHOR,
        )
        if not nasdaq_rows or not etf_rows or any(not tencent_by_code[code] for code in HISTORY_CODES
                                                  if code not in {'sh513300', 'sz159612'}):
            raise ValueError('本机历史行情档案缺少必需标的')

        write_csv(work / 'source_rows/tencent_daily.csv.gz', tencent_rows, tencent_fields)
        write_csv(work / 'source_rows/nasdaq-513300.csv.gz', nasdaq_rows, nasdaq_fields)
        write_csv(work / 'source_rows/etf_daily_sp500_pre_anchor.csv.gz', etf_rows, etf_fields)
        write_json(work / 'reference.json', {
            'history_etf': reference_rows,
            'stats_etf': reference_stats,
        })
        (work / 'source').mkdir(parents=True, exist_ok=True)
        for name in ('legacy_backtest', 'legacy_dashboard', 'legacy_data_source'):
            shutil.copyfile(source_paths[name], work / 'source' / source_paths[name].name)
        for path in legacy_sources:
            destination = work / 'source/legacy' / path.relative_to(source_root)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
        shutil.copyfile(Path(__file__).resolve(), work / 'source/eternal_baseline.py')

        saved = {code: tencent_by_code[code] for code in HISTORY_CODES}
        saved['sh513300'] = nasdaq_rows
        saved['sz159612'] = etf_rows
        fetched = {}
        response_dir = work / 'responses/bars'
        if end > ANCHOR:
            for index, code in enumerate(HISTORY_CODES):
                fetched[code] = fetch_tencent_bars(code, ANCHOR, end, response_dir, request_log)
                if index + 1 < len(HISTORY_CODES):
                    time.sleep(.25)
        else:
            fetched = {code: [] for code in HISTORY_CODES}

        factors_dir = work / 'responses/factors'
        factors = {}
        for index, code in enumerate(HISTORY_CODES):
            factors[code] = fetch_sina_factors(code, factors_dir / f'{code}.js', request_log)
            write_csv(work / f'factors/{code}.csv.gz', factors[code],
                      ('date', 'split', 'dividend'))
            if index + 1 < len(HISTORY_CODES):
                time.sleep(.25)
        write_json(work / 'requests.json', request_log)

        for code in HISTORY_CODES:
            rows = [normalize_bar(row, 'local_archive') for row in saved[code]]
            rows.extend(
                normalize_bar(row, 'tencent_response')
                for row in fetched[code] if row['date'] > ANCHOR
            )
            ensure_ordered_bars(code, rows)
            write_csv(work / f'bars/{code}.csv.gz', rows, RAW_BAR_FIELDS)

        source_inputs = {
            name: {'file': path.name, 'sha256': sha256_file(path), 'bytes': path.stat().st_size}
            for name, path in source_paths.items()
        }
        changed_inputs = [
            name for name, details in source_inputs.items()
            if details['sha256'] != source_hashes_before[name]
        ]
        if changed_inputs:
            raise RuntimeError(f'冻结期间本机输入发生变化: {", ".join(changed_inputs)}')
        if any(sha256_file(path) != legacy_hashes_before[path.relative_to(source_root).as_posix()]
               for path in legacy_sources):
            raise RuntimeError('冻结期间旧源码依赖发生变化')
        source_hashes = {
            path.name: sha256_file(path)
            for path in source_paths.values()
            if path.name in {'backtest.py', 'daily_update.py', 'dividend_data.py'}
        }
        bars_by_code = {
            code: csv_rows(work / f'bars/{code}.csv.gz')[0] for code in HISTORY_CODES
        }
        factors_by_code = {
            code: csv_rows(work / f'factors/{code}.csv.gz')[0] for code in HISTORY_CODES
        }
        validated_prices, validated_limits = {}, {}
        for code in HISTORY_CODES:
            ensure_ordered_bars(code, bars_by_code[code])
            validated_prices[code], actions = total_return_prices(
                bars_by_code[code], factors_by_code[code],
            )
            validated_limits[code] = opening_limits(
                bars_by_code[code], actions, Decimal('.10'),
            )
        for legacy_code, current_code in LEGACY_PAIRS:
            splice_etf(validated_prices, validated_limits, legacy_code, current_code)
        gaps = [
            input_gap(code, day, 'daily_bar', '参考日期缺少共同价格，影响回放和对照',
                      ['核查停牌及原始档案', '由负责人裁决补采或其他措施'])
            for code in ETF_CODES for day in reference_dates
            if day not in validated_prices[code]
        ]
        if gaps:
            raise InputGaps(gaps)
        if not set.intersection(*(set(validated_prices[code]) for code in ETF_CODES)):
            raise ValueError('所需 ETF 没有共同交易日')
        input_start_by_asset = {code: rows[0]['date'] for code, rows in bars_by_code.items()}
        input_end_by_asset = {code: rows[-1]['date'] for code, rows in bars_by_code.items()}
        factor_start_by_asset = {code: rows[0]['date'] for code, rows in factors_by_code.items()}
        factor_end_by_asset = {code: rows[-1]['date'] for code, rows in factors_by_code.items()}
        warmup_days = [
            row['date'] for rows in bars_by_code.values() for row in rows
            if row['date'] < start
        ]
        inventory = {}
        for path in sorted(p for p in work.rglob('*') if p.is_file()):
            relative = path.relative_to(work).as_posix()
            inventory[relative] = {'sha256': sha256_file(path), 'bytes': path.stat().st_size}
        manifest = {
            'schema_version': 3,
            'created_at_utc': datetime.now(timezone.utc).isoformat(),
            'source': {
                'issue_declared_revision': 'd5d48c1',
                'issue_revision_verified': False,
                'revision_limit': 'local legacy source copy has no verifiable Git history',
                'captured_source_sha256': source_hashes,
                'legacy_local_sources': legacy_hashes_before,
                'legacy_closure_limit': 'static local imports only; dynamic imports and external dependencies unverified',
                'replay_entrypoint': 'source/eternal_baseline.py',
                'legacy_source_role': 'archived for provenance; not invoked by the replay adapter',
            },
            'source_inputs': source_inputs,
            'files': inventory,
            'runtime': {
                'python': sys.version,
                'platform': platform.platform(),
                'replay_dependencies': 'Python standard library only',
                'capture_methods': {
                    'daily_bars': 'a-stock-data 3.10.4 §1.2 Tencent unadjusted daily',
                    'fund_factors': 'a-stock-data 3.10.4 §1.6 Sina hfq.js s/u',
                },
            },
            'configuration': execution_configuration(),
            'date_ranges': {
                'input_warmup_start': min(warmup_days) if warmup_days else None,
                'input_warmup_end': max(warmup_days) if warmup_days else None,
                'baseline_start': start,
                'baseline_end': end,
                'bars_by_asset': {
                    code: {'start': input_start_by_asset[code], 'end': input_end_by_asset[code]}
                    for code in HISTORY_CODES
                },
                'factors_by_asset': {
                    code: {'start': factor_start_by_asset[code], 'end': factor_end_by_asset[code]}
                    for code in HISTORY_CODES
                },
            },
            'reference_fields': {
                'available': ['dates', 'daily_nav', *reference_stats.keys()],
                'unavailable': ['signals', 'trades', 'fees', 'cash', 'holdings'],
                'impact': 'The captured reference can validate date/NAV and listed summary fields only; execution detail comparisons are unavailable.',
            },
        }
        seal_manifest(manifest)
        write_json(work / 'manifest.json', manifest)
        if target.exists():
            raise FileExistsError(f'snapshot 已存在，不能覆盖: {target}')
        os.rename(work, target)
    except InputGaps as exc:
        if work is not None and work.exists():
            if not (work / 'requests.json').exists():
                write_json(work / 'requests.json', request_log)
            write_json(work / 'pending-gaps.json', {'status': 'pending_decision', 'gaps': exc.gaps})
            if target.exists():
                raise FileExistsError(f'pending snapshot 已存在，不能覆盖: {target}') from exc
            os.rename(work, target)
        raise
    except BaseException:
        if work is not None and work.exists():
            shutil.rmtree(work)
        raise
    finally:
        lock_path.unlink(missing_ok=True)
    print('snapshot frozen; input and raw responses remain outside the repository')


def total_return_prices(bars, factors):
    factor_dates = [row['date'] for row in factors]
    if not factor_dates:
        raise InputGaps([input_gap(
            bars[0]['code'] if bars else None, bars[0]['date'] if bars else None,
            'factor', '缺少有效因子，无法计算总回报',
            ['核查原始因子覆盖', '由负责人裁决补采或其他措施'],
        )])
    if sorted(set(factor_dates)) != factor_dates:
        raise ValueError('复权因子缺失或日期重复')
    previous_close = None
    previous_split = 1.0
    previous_cash = 0.0
    index_close = 1.0
    prices, actions = {}, []
    for row in bars:
        day = row['date']
        factor_index = bisect_right(factor_dates, day) - 1
        if factor_index < 0:
            raise InputGaps([input_gap(
                row['code'], day, 'factor', '缺少有效因子，无法计算总回报',
                ['核查原始因子覆盖', '由负责人裁决补采或其他措施'],
            )])
        if day in prices:
            raise ValueError(f'{row["code"]} 行情缺少有效因子或日期重复')
        factor = factors[factor_index]
        split = float(factor['split'])
        dividend = float(factor['dividend'])
        cumulative_cash = split * dividend
        raw_open, raw_close = float(row['open']), float(row['close'])
        if (not all(math.isfinite(value) and value > 0 for value in (raw_open, raw_close, split))
                or not math.isfinite(dividend) or not math.isfinite(cumulative_cash)):
            raise ValueError(f'{row["code"]} 开收盘价或份额折算比例无效')
        cash = (cumulative_cash - previous_cash) / previous_split
        ratio = split / previous_split
        if previous_close is None:
            index_open = 1.0
        else:
            if cash < -1e-8:
                raise ValueError(f'{row["code"]} 累计分红倒退')
            index_open = index_close * (raw_open * ratio + cash) / previous_close
            if not math.isclose(ratio, 1.0) or not math.isclose(cash, 0.0, abs_tol=1e-8):
                actions.append({
                    'date': day,
                    'code': row['code'],
                    'cash_per_old_share': round(cash, 10),
                    'shares_per_old_share': ratio,
                })
        index_close = index_open * raw_close / raw_open
        prices[day] = (index_open, index_close)
        previous_close, previous_split, previous_cash = raw_close, split, cumulative_cash
    if not prices:
        raise ValueError('行情序列为空')
    return prices, actions


def opening_limits(bars, actions, rate):
    ex_dates = {row['date']: row for row in actions}
    tick = Decimal('0.001')
    flags = {}
    for previous, row in zip(bars, bars[1:]):
        reference = Decimal(previous['close'])
        if action := ex_dates.get(row['date']):
            cash = Decimal(str(action['cash_per_old_share']))
            ratio = Decimal(str(action['shares_per_old_share']))
            reference = ((reference - cash) / ratio).quantize(tick, rounding=ROUND_HALF_UP)
        opening = Decimal(row['open'])
        upper = (reference * (1 + rate)).quantize(tick, rounding=ROUND_HALF_UP)
        lower = (reference * (1 - rate)).quantize(tick, rounding=ROUND_HALF_UP)
        flags[row['date']] = (opening == upper, opening == lower)
    return flags


def splice_etf(prices, limits, legacy_code, current_code):
    legacy, current = prices[legacy_code], prices[current_code]
    overlap = legacy.keys() & current.keys()
    if not overlap:
        legacy_span = f'{min(legacy)}..{max(legacy)}' if legacy else 'empty'
        current_span = f'{min(current)}..{max(current)}' if current else 'empty'
        raise InputGaps([input_gap(
            f'{legacy_code}/{current_code}',
            f'{legacy_span} / {current_span}', 'overlap',
            '缺少共同收盘日，无法拼接',
            ['核查两标的原始行情', '由负责人裁决补采或其他措施'],
        )])
    anchor = min(overlap)
    scale = legacy[anchor][1] / current[anchor][1]
    dates = sorted(day for day in current if day > anchor)
    if not dates:
        raise InputGaps([input_gap(
            current_code, anchor, 'daily_bar', '拼接后没有可用交易日',
            ['核查原始行情', '由负责人裁决补采或其他措施'],
        )])
    prices[current_code] = {
        **{day: pair for day, pair in legacy.items() if day <= anchor},
        **{day: tuple(value * scale for value in current[day]) for day in dates},
    }
    limits[current_code] = {
        **{day: flag for day, flag in limits[legacy_code].items() if day <= anchor},
        **{day: limits[current_code][day] for day in dates if day in limits[current_code]},
    }
    return anchor, dates[0]


def rebalance(current, weights, fee_rate, cash=0.0):
    capital = sum(current) + cash
    net = capital
    for _ in range(12):
        desired = [net * weight for weight in weights]
        traded = sum(abs(target - held) for target, held in zip(desired, current))
        new_net = capital - fee_rate * traded
        if math.isclose(new_net, net, rel_tol=1e-13):
            return desired, traded, fee_rate * traded
        net = new_net
    raise ArithmeticError('交易费用计算未收敛')


def simulate(days, opens, closes, limits, threshold, fee_rate, portfolio=WEIGHTS):
    if len(days) < 3 or days[0] >= days[-1]:
        raise ValueError('共同交易日少于三个不同日期，无法计算年化收益与波动率')
    weights = list(portfolio.values())
    thresholds = threshold if isinstance(threshold, dict) else dict.fromkeys(portfolio, threshold)
    initial_value = 1.0 / (1.0 + fee_rate)
    initial_fee = 1.0 - initial_value
    cash = 0.0
    units = [initial_value * weight / price for weight, price in zip(weights, opens[0])]
    navs, signals, trades, daily = [], [], [], []
    peak, drawdown = 1.0, 0.0
    basis = asset_peaks = opens[0]
    pending = None
    blocked_days = 0
    for i, day in enumerate(days):
        daily_fees = initial_fee if i == 0 else 0.0
        if pending:
            holdings = [unit * price for unit, price in zip(units, opens[i])]
            capital = sum(holdings) + cash
            desired, turnover, fees = rebalance(holdings, weights, fee_rate, cash=cash)
            orders = [new - old for new, old in zip(desired, holdings)]
            blocked = any((upper and order > 1e-12) or (lower and order < -1e-12)
                          for (upper, lower), order in zip(limits[i], orders))
            if blocked:
                blocked_days += 1
            else:
                signal_index, triggers = pending
                units = [value / price for value, price in zip(desired, opens[i])]
                cash = 0.0
                daily_fees += fees
                trades.append({
                    'signal_date': days[signal_index],
                    'intended_date': days[signal_index + 1],
                    'trade_date': day,
                    'trigger_details': triggers,
                    'gross_turnover': turnover / capital,
                    'fees_in_initial_nav': fees,
                    **{f'net_trade_{code[2:]}': order / capital
                       for code, order in zip(portfolio, orders)},
                })
                basis = asset_peaks = opens[i]
                pending = None

        nav = cash + sum(unit * price for unit, price in zip(units, closes[i]))
        peak = max(peak, nav)
        drawdown = min(drawdown, nav / peak - 1)
        navs.append(nav)
        daily.append({
            'date': day,
            'nav': nav,
            'cash': cash,
            'fees': daily_fees,
            'units': dict(zip(portfolio, units)),
            'holdings': {
                code: unit * price
                for code, unit, price in zip(portfolio, units, closes[i])
            },
        })

        if threshold is not None and pending is None:
            asset_peaks = [max(high, price) for high, price in zip(asset_peaks, closes[i])]
            triggers = []
            for code, price, entry, high in zip(portfolio, closes[i], basis, asset_peaks):
                gain = price / entry - 1.0
                loss_from_peak = 1.0 - price / high
                if gain + 1e-12 >= thresholds[code]:
                    triggers.append(f'{code[2:]}:gain:{gain:.8f}')
                if loss_from_peak + 1e-12 >= thresholds[code]:
                    triggers.append(f'{code[2:]}:drawdown:{loss_from_peak:.8f}')
            if triggers:
                trigger_details = '|'.join(triggers)
                signals.append({
                    'signal_date': day,
                    'intended_date': days[i + 1] if i + 1 < len(days) else None,
                    'trigger_details': trigger_details,
                })
                pending = (i, trigger_details)

    daily_returns = [after / before - 1.0 for before, after in zip(navs, navs[1:])]
    years = (days[-1] - days[0]).days / 365.2425
    stats = {
        'start': days[0],
        'end': days[-1],
        'total_return': navs[-1] - 1.0,
        'fee_bps': round(fee_rate * 10_000, 4),
        'cagr': navs[-1] ** (1.0 / years) - 1.0,
        'annual_volatility': stdev(daily_returns) * math.sqrt(252),
        'max_drawdown': drawdown,
        'rebalances': len(trades),
        'blocked_open_days': blocked_days,
        'pending_at_end': pending is not None,
        'gross_turnover': sum(row['gross_turnover'] for row in trades),
        'fees_in_initial_nav': initial_fee + sum(row['fees_in_initial_nav'] for row in trades),
    }
    return navs, signals, trades, stats, daily


def read_snapshot(snapshot):
    manifest_path = snapshot / 'manifest.json'
    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    if manifest.get('schema_version') != 3:
        raise ValueError('unsupported snapshot manifest schema; create a new snapshot')
    expected = manifest.get('files')
    if not isinstance(expected, dict):
        raise ValueError('snapshot manifest 缺少文件摘要')
    if 'source/eternal_baseline.py' not in expected:
        raise ValueError('snapshot 缺少回放源码副本')
    source = manifest.get('source')
    captured_source_hashes = source.get('captured_source_sha256') if isinstance(source, dict) else None
    required_sources = {'backtest.py', 'daily_update.py', 'dividend_data.py'}
    if not isinstance(captured_source_hashes, dict) or not required_sources.issubset(captured_source_hashes):
        raise ValueError('snapshot manifest 缺少冻结源码摘要')
    for name, digest in captured_source_hashes.items():
        details = expected.get(f'source/{name}')
        if not isinstance(details, dict) or details.get('sha256') != digest:
            raise ValueError(f'snapshot 源码摘要与文件清单不一致: {name}')
    legacy_sources = source.get('legacy_local_sources')
    if not isinstance(legacy_sources, dict) or not {
        'backtest.py', 'dividend_data.py', 'strategy-dashboard/daily_update.py',
    }.issubset(legacy_sources):
        raise ValueError('snapshot 缺少旧源码依赖闭包')
    for name, digest in legacy_sources.items():
        details = expected.get(f'source/legacy/{name}')
        if not isinstance(details, dict) or details.get('sha256') != digest:
            raise ValueError(f'snapshot 旧源码依赖摘要不一致: {name}')
    actual_paths = {path.relative_to(snapshot).as_posix()
                    for path in snapshot.rglob('*') if path.is_file() and path != manifest_path}
    if actual_paths != set(expected):
        raise ValueError('snapshot 文件集合与 manifest 不一致')
    for relative, details in expected.items():
        path = (snapshot / relative).resolve()
        if snapshot.resolve() not in path.parents or sha256_file(path) != details['sha256']:
            raise ValueError(f'snapshot 文件摘要不匹配: {relative}')
    input_version = hashlib.sha256(canonical_json(manifest_identity_material(manifest))).hexdigest()
    if input_version != manifest.get('input_version'):
        raise ValueError('snapshot input_version 不匹配')
    manifest_sha256 = hashlib.sha256(canonical_json(manifest_digest_material(manifest))).hexdigest()
    if manifest_sha256 != manifest.get('manifest_sha256'):
        raise ValueError('snapshot manifest 摘要不匹配')
    reference = json.loads((snapshot / 'reference.json').read_text(encoding='utf-8'))
    return manifest, reference


def compare_reference(reference, dates, navs, stats, anchors):
    baseline = reference['history_etf']
    reference_dates = [row[0] for row in baseline]
    reference_nav = dict(baseline)
    current_nav = dict(zip((day.isoformat() for day in dates), navs))
    compared_dates = sorted(reference_nav.keys() & current_nav.keys())
    errors = [abs(reference_nav[day] - current_nav[day]) for day in compared_dates]
    old_stats = reference['stats_etf']
    actual_stats = {
        **stats,
        'start': stats['start'].isoformat(),
        'end': stats['end'].isoformat(),
        'nasdaqAnchor': anchors[0][0],
        'nasdaqSwitch': anchors[0][1],
        'sp500Anchor': anchors[1][0],
        'sp500Switch': anchors[1][1],
    }
    fields = dict.fromkeys((
        'start', 'end', 'nasdaqAnchor', 'nasdaqSwitch', 'sp500Anchor',
        'sp500Switch', 'cagr', 'max_drawdown', 'rebalances',
        *actual_stats, *old_stats,
    ))
    metric_results = {}
    for field in fields:
        if field not in old_stats:
            metric_results[field] = {'status': 'unavailable_in_reference'}
        elif field not in actual_stats:
            metric_results[field] = {'status': 'unavailable_in_replay'}
        elif (isinstance(old_stats[field], (int, float))
              and not isinstance(old_stats[field], bool)
              and isinstance(actual_stats[field], (int, float))
              and not isinstance(actual_stats[field], bool)):
            difference = abs(float(old_stats[field]) - float(actual_stats[field]))
            metric_results[field] = {
                'status': 'match' if difference <= NAV_TOLERANCE else 'mismatch',
                'absolute_difference': difference,
                'tolerance': NAV_TOLERANCE,
            }
        else:
            metric_results[field] = {
                'status': 'match' if old_stats[field] == actual_stats[field] else 'mismatch',
            }
    date_match = reference_dates == [day.isoformat() for day in dates]
    nav_match = date_match and len(compared_dates) == len(reference_dates) and all(
        error <= NAV_TOLERANCE for error in errors
    )
    matched_metrics = all(
        result['status'] in {'match', 'unavailable_in_reference'}
        for result in metric_results.values()
    )
    return {
        'tolerance': NAV_TOLERANCE,
        'dates': {
            'status': 'match' if date_match else 'mismatch',
            'reference_count': len(reference_dates),
            'replay_count': len(dates),
        },
        'daily_nav': {
            'status': 'match' if nav_match else 'mismatch',
            'compared_count': len(compared_dates),
            'mismatch_count': sum(error > NAV_TOLERANCE for error in errors),
            'maximum_absolute_difference': max(errors, default=None),
        },
        'metrics': metric_results,
        'execution_detail_comparison': {
            field: 'unavailable_in_reference'
            for field in ('signals', 'trades', 'fees', 'cash', 'holdings')
        },
        'overall_status': 'match' if date_match and nav_match and matched_metrics else 'mismatch',
    }


def replay(args):
    snapshot = private_path(args.snapshot, 'snapshot')
    output = private_path(args.output, 'replay output')
    if snapshot == output or snapshot in output.parents:
        raise ValueError('replay output 不能写入 snapshot 目录')
    run_manifest_path = output.with_name(output.name + '.run.json')
    if output.exists():
        raise FileExistsError(f'replay output 已存在，不能覆盖: {output}')
    if run_manifest_path.exists():
        raise FileExistsError(f'replay run manifest 已存在，不能覆盖: {run_manifest_path}')
    manifest, reference = read_snapshot(snapshot)
    validate_execution_configuration(manifest)
    replay_code_sha256, snapshot_code_sha256 = validate_replay_source(
        manifest, Path(__file__).resolve(),
    )
    bars, factors, prices, limits = {}, {}, {}, {}
    for code in HISTORY_CODES:
        bars[code] = csv_rows(snapshot / f'bars/{code}.csv.gz')[0]
        factors[code] = csv_rows(snapshot / f'factors/{code}.csv.gz')[0]
        ensure_ordered_bars(code, bars[code])
        prices[code], actions = total_return_prices(bars[code], factors[code])
        limits[code] = opening_limits(bars[code], actions, Decimal('.10'))
    anchors = [
        splice_etf(prices, limits, old, new)
        for old, new in LEGACY_PAIRS
    ]
    common = set.intersection(*(set(prices[code]) for code in ETF_CODES))
    reference_rows = reference['history_etf']
    old_dates = [row[0] for row in reference_rows if row[0] <= ANCHOR]
    days = ([date.fromisoformat(day) for day in old_dates if day in common]
            + [date.fromisoformat(day) for day in sorted(common) if day > ANCHOR])
    if len(days) < 3:
        raise ValueError('回放共同交易日少于三天，无法计算年化收益与波动率')
    opens = [[prices[code][day.isoformat()][0] for code in ETF_CODES] for day in days]
    closes = [[prices[code][day.isoformat()][1] for code in ETF_CODES] for day in days]
    daily_limits = [[limits[code].get(day.isoformat(), (False, False)) for code in ETF_CODES]
                    for day in days]
    configuration = manifest['configuration']
    navs, signals, trades, stats, daily = simulate(
        days, opens, closes, daily_limits,
        configuration['threshold'], configuration['fee_rate_one_way'],
        {code: configuration['weights'][code] for code in ETF_CODES},
    )
    comparison = compare_reference(reference, days, navs, stats, anchors)
    result = {
        'schema_version': 1,
        'input_version': manifest['input_version'],
        'configuration': manifest['configuration'],
        'summary': {
            **stats,
            'nasdaqAnchor': anchors[0][0],
            'nasdaqSwitch': anchors[0][1],
            'sp500Anchor': anchors[1][0],
            'sp500Switch': anchors[1][1],
        },
        'daily': daily,
        'signals': signals,
        'trades': trades,
        'comparison': comparison,
        'risk_disclosures': {
            'historical_universe': 'unverified; fixed/proxy assets may introduce survivorship or selection bias',
            'total_return_pit': 'unverified; historical listing, distributions and factor availability are not proven point-in-time',
        },
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    write_json(output, result)
    output_sha256 = sha256_file(output)
    write_json(run_manifest_path, {
        'schema_version': 1,
        'run_id': hashlib.sha256(
            f'{manifest["input_version"]}:{output_sha256}:{replay_code_sha256}'.encode()
        ).hexdigest(),
        'input_version': manifest['input_version'],
        'result_sha256': output_sha256,
        'replay_code_sha256': replay_code_sha256,
        'snapshot_replay_code_sha256': snapshot_code_sha256,
        'replay_code_matches_snapshot': replay_code_sha256 == snapshot_code_sha256,
        'comparison_status': comparison['overall_status'],
        'created_at_utc': datetime.now(timezone.utc).isoformat(),
        'runtime': {'python': sys.version, 'platform': platform.platform()},
    })
    print(f'replay written; comparison={comparison["overall_status"]}')
    return 0 if comparison['overall_status'] == 'match' else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    freeze_parser = commands.add_parser('freeze')
    freeze_parser.add_argument('--legacy-root', type=Path, required=True)
    freeze_parser.add_argument('--snapshot', type=Path, required=True)
    replay_parser = commands.add_parser('replay')
    replay_parser.add_argument('--snapshot', type=Path, required=True)
    replay_parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'freeze':
        freeze(args)
        return 0
    return replay(args)


if __name__ == '__main__':
    raise SystemExit(main())
