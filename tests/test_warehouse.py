import json
from datetime import date, timedelta
from pathlib import Path
import tempfile
import unittest
import uuid

from warehouse import database, parse_daily, insert_rows, export_snapshot, now, symbol_check

class WarehouseTests(unittest.TestCase):
    def blob(self, rows=None):
        return json.dumps({'code': 0, 'data': {'sh600519': {'day': rows if rows is not None else [['2026-09-18','10','11','12','9','123.45']]}}}).encode()

    def parse(self, blob):
        return parse_daily(blob, 'sh600519', date(2026,9,14), date(2026,9,18))

    def test_unit_and_ohlc(self):
        row = self.parse(self.blob())[0]
        self.assertEqual(str(row[5]), '12345.00')
        self.assertEqual(tuple(map(str, row[1:5])), ('10','12','9','11'))

    def test_invalid_and_duplicate(self):
        base = ['2026-09-18','10','11','12','9','123']
        for rows in [[], [base, base], [['2026-09-19', *base[1:]]],
                     [['2026-09-18','NaN',*base[2:]]],
                     [['2026-09-18','10','11','8','9','123']]]:
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                self.parse(self.blob(rows))

    def test_no_adjusted_substitution(self):
        with self.assertRaises(ValueError):
            self.parse(self.blob().replace(b'"day"', b'"qfqday"'))

    def test_explicit_symbol(self):
        for symbol in ['600519', 'sh000001', 'bj920000', 'SH000001.SZ']:
            with self.assertRaises(ValueError):
                symbol_check(symbol)
        symbol_check('sz000001')

    def test_revisions_idempotence_reversion_and_export(self):
        with tempfile.TemporaryDirectory() as tmp, database(Path(tmp)) as con:
            original = self.parse(self.blob())
            changed = self.parse(self.blob([['2026-09-18','10','10.5','12','9','123.45']]))
            stamp = now()
            for i, (rows, expected) in enumerate([(original,1),(original,0),(changed,1),(original,1)]):
                run = str(uuid.uuid4())
                con.execute("INSERT INTO ops.ingest_run(run_id,source,request_url,started_at,status) VALUES (?,'tencent','fixture',?,'RUNNING')", [run, stamp])
                self.assertEqual(insert_rows(con, run, 'sh600519', rows, stamp+timedelta(seconds=i)), expected)
            self.assertEqual(con.execute('SELECT count(*) FROM staging.daily_bar_revision').fetchone()[0], 3)
            self.assertEqual(con.execute('SELECT count(*) FROM staging.daily_bar_latest').fetchone()[0], 1)
            self.assertEqual(export_snapshot(con, tmp)['rows'], 1)

if __name__ == '__main__':
    unittest.main()
