"""A股研究库验证底座。接口/字段依据用户提供 skill §1.5，独立实现入库契约。

这里只支持显式沪深个股代码、最多 700 天日线窗口，不宣称全市场完整。
"""
import argparse
from contextlib import contextmanager
from datetime import date, datetime, timezone
from decimal import Decimal, InvalidOperation
import fcntl
import hashlib
import json
from pathlib import Path
import re
import time
import urllib.parse
import urllib.request
import uuid

import duckdb

ROOT = Path(__file__).resolve().parent
ADAPTER_VERSION = 'tencent-day-v1'

def now():
    return datetime.now(timezone.utc)

def digest(blob):
    return hashlib.sha256(blob).hexdigest()

@contextmanager
def database(data_dir):
    data_dir = Path(data_dir)
    data_dir.mkdir(parents=True, exist_ok=True)
    with (data_dir / '.writer.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        con = duckdb.connect(str(data_dir / 'market.duckdb'))
        try:
            con.execute((ROOT / 'sql/schema.sql').read_text())
            yield con
        finally:
            con.close()

def symbol_check(symbol):
    # 只接受明确的沪深 A 股号段。证券身份的最终认证仍需证券主表。
    if not re.fullmatch(r'(sh6\d{5}|sz[03]\d{5})', symbol):
        raise ValueError('样本适配器仅支持 sh600519 / sz000001 等显式沪深个股代码')

def number(value):
    if isinstance(value, bool) or value is None:
        raise ValueError('非法数值')
    try:
        result = Decimal(str(value))
    except InvalidOperation as exc:
        raise ValueError('非法数值') from exc
    if not result.is_finite():
        raise ValueError('NaN/Inf 不允许入库')
    return result

def parse_daily(blob, symbol, start, end):
    payload = json.loads(blob)
    if not isinstance(payload, dict) or payload.get('code') not in (0, '0'):
        raise ValueError('源返回错误状态')
    node = payload.get('data', {}).get(symbol)
    if not isinstance(node, dict) or not isinstance(node.get('day'), list):
        raise ValueError('缺少不复权 day 字段，拒绝用复权数据替代')
    rows, seen = [], set()
    for item in node['day']:
        if not isinstance(item, list) or len(item) < 6:
            raise ValueError('K 线字段结构改变')
        day = date.fromisoformat(item[0])
        if not start <= day <= end or day in seen:
            raise ValueError('日期越界或重复')
        seen.add(day)
        o, c, h, l, volume = map(number, item[1:6])
        if not (0 < l <= min(o, c) <= max(o, c) <= h) or volume < 0:
            raise ValueError('OHLC 或成交量校验失败')
        values = [day.isoformat(), str(o), str(h), str(l), str(c), str(volume * 100)]
        rows.append((day, o, h, l, c, volume * 100, digest(json.dumps(values).encode())))
    if not rows:
        raise ValueError('空窗口：需进一步区分未上市、停牌、源异常，不能视为成功')
    return rows

def insert_rows(con, run_id, symbol, rows, observed_at):
    inserted = 0
    con.execute('BEGIN')
    try:
        for day, o, h, l, c, volume, row_hash in rows:
            previous = con.execute("SELECT row_hash FROM staging.daily_bar_latest WHERE symbol=? AND trade_date=? AND source='tencent'", [symbol, day]).fetchone()
            if previous and previous[0] == row_hash:
                continue
            con.execute('INSERT INTO staging.daily_bar_revision VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                        [str(uuid.uuid4()), symbol, day, 'tencent', 'none', o, h, l, c,
                         volume, None, observed_at, run_id, row_hash])
            inserted += 1
        con.execute("UPDATE ops.ingest_run SET status='SUCCESS', finished_at=?, row_count=?, inserted_count=? WHERE run_id=?",
                    [now(), len(rows), inserted, run_id])
        con.execute('COMMIT')
    except Exception:
        con.execute('ROLLBACK')
        raise
    return inserted

def ingest(con, data_dir, symbol, start, end):
    symbol_check(symbol)
    if start > end or (end-start).days >= 700:
        raise ValueError('每个采集窗口须为 1–700 个自然日')
    # 收盘发布判定由后续交易日历调度实现；样本只采集已结束的日期。
    from zoneinfo import ZoneInfo
    if end >= datetime.now(ZoneInfo('Asia/Shanghai')).date():
        raise ValueError('样本仅允许今天之前的日期')
    param = f'{symbol},day,{start},{end},640,'
    url = 'https://web.ifzq.gtimg.cn/appstock/app/fqkline/get?' + urllib.parse.urlencode({'param': param})
    run_id = str(uuid.uuid4())
    con.execute("INSERT INTO ops.ingest_run(run_id,source,request_url,started_at,status) VALUES (?,'tencent',?,?,'RUNNING')", [run_id, url, now()])
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0', 'Referer': 'https://gu.qq.com/'})
        with urllib.request.urlopen(req, timeout=25) as response:
            blob = response.read(10_000_001)
            if len(blob) > 10_000_000:
                raise ValueError('响应超出 10MB 样本限制')
        fetched = now()
        raw_dir = Path(data_dir) / 'raw' / 'tencent' / run_id
        raw_dir.mkdir(parents=True)
        raw_path = raw_dir / 'response.json'
        raw_path.write_bytes(blob)
        meta = {'url': url, 'fetched_at': fetched.isoformat(), 'sha256': digest(blob),
                'adapter_version': ADAPTER_VERSION, 'symbol': symbol, 'start': str(start), 'end': str(end)}
        (raw_dir / 'manifest.json').write_text(json.dumps(meta, ensure_ascii=False, indent=2))
        con.execute('UPDATE ops.ingest_run SET raw_path=?,raw_sha256=? WHERE run_id=?', [str(raw_path.resolve()), digest(blob), run_id])
        rows = parse_daily(blob, symbol, start, end)
        inserted = insert_rows(con, run_id, symbol, rows, fetched)
        return {'symbol': symbol, 'rows': len(rows), 'new_revisions': inserted, 'run_id': run_id}
    except Exception as exc:
        con.execute("UPDATE ops.ingest_run SET status='FAILED',finished_at=?,error=? WHERE run_id=?", [now(), f'{type(exc).__name__}: {exc}', run_id])
        raise

def export_snapshot(con, data_dir):
    folder = Path(data_dir) / 'snapshots' / str(uuid.uuid4())
    folder.mkdir(parents=True)
    target = folder / 'daily_latest.parquet'
    escaped = str(target.resolve()).replace("'", "''")
    con.execute(f"COPY staging.daily_bar_latest TO '{escaped}' (FORMAT PARQUET, COMPRESSION ZSTD)")
    count = con.execute('SELECT count(*) FROM staging.daily_bar_latest').fetchone()[0]
    check = con.execute('SELECT count(*) FROM read_parquet(?)', [str(target)]).fetchone()[0]
    if count != check:
        raise RuntimeError('Parquet 回读行数不一致')
    manifest = {'created_at': now().isoformat(), 'rows': count, 'sha256': digest(target.read_bytes()),
                'scope': 'staging sample only', 'file': target.name}
    (folder / 'manifest.json').write_text(json.dumps(manifest, indent=2))
    return {'snapshot': str(target), 'rows': count}

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--data-dir', type=Path, default=ROOT / 'data')
    parser.add_argument('command', choices=['init', 'sample', 'status', 'export'])
    parser.add_argument('--symbols', nargs='+', default=['sh600519', 'sz000001', 'sz300750'])
    parser.add_argument('--start', type=date.fromisoformat, default=date(2026, 9, 14))
    parser.add_argument('--end', type=date.fromisoformat, default=date(2026, 9, 18))
    args = parser.parse_args()
    with database(args.data_dir) as con:
        if args.command == 'sample':
            for idx, symbol in enumerate(args.symbols):
                if idx:
                    time.sleep(2)
                print(json.dumps(ingest(con, args.data_dir, symbol, args.start, args.end), ensure_ascii=False))
        elif args.command == 'export':
            print(json.dumps(export_snapshot(con, args.data_dir)))
        else:
            print(json.dumps({'database': str(args.data_dir / 'market.duckdb'),
                              'revisions': con.execute('SELECT count(*) FROM staging.daily_bar_revision').fetchone()[0],
                              'latest_rows': con.execute('SELECT count(*) FROM staging.daily_bar_latest').fetchone()[0],
                              'runs': con.execute('SELECT status, count(*) FROM ops.ingest_run GROUP BY status').fetchall()}))

if __name__ == '__main__':
    main()
