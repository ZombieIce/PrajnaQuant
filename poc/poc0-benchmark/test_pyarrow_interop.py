import json
import pathlib
import re
import subprocess
import tempfile
import unittest
from decimal import Decimal

try:
    import pyarrow as pa
    import pyarrow.parquet as pq
except ImportError:
    pa = None
    pq = None


ROOT = pathlib.Path(__file__).resolve().parents[2]


@unittest.skipIf(pa is None, "pyarrow is not installed in lightweight CI")
class PyArrowInteropTest(unittest.TestCase):
    def test_fixture_v1_tables_are_readable_with_contract_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                [
                    "cargo",
                    "run",
                    "-p",
                    "prajna-data",
                    "--example",
                    "fixture_v1",
                    "--locked",
                    "--",
                    directory,
                ],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            match = re.search(r"^manifest_path=(.+)$", result.stdout, re.MULTILINE)
            self.assertIsNotNone(match)
            manifest_path = pathlib.Path(match.group(1))
            manifest = json.loads(manifest_path.read_text())
            dsv = manifest["dsv"].removeprefix("dsv:sha256:")

            expected = {
                "instruments": (3, ["instrument_id", "venue_id", "kind", "native_symbol",
                                    "market_segment", "base_currency", "quote_currency",
                                    "settle_currency", "is_inverse", "multiplier", "expiry",
                                    "price_increment", "size_increment", "lot_size",
                                    "session_timezone"]),
                "sessions": (10, ["venue_id", "session_date", "ts_open", "ts_close"]),
                "bars": (29, ["instrument_id", "bar_spec", "session_date", "ts_open", "ts_close",
                              "open", "high", "low", "close", "volume", "amount", "available_at"]),
            }
            for table_name, (row_count, fields) in expected.items():
                path = manifest_path.parent.parent / "normalized" / table_name / dsv / "part-00000.parquet"
                table = pq.read_table(path)
                self.assertEqual(table.num_rows, row_count)
                self.assertEqual(table.column_names, fields)
                self.assertEqual(table.schema.metadata[b"prajna.table"].decode(), table_name)
                self.assertEqual(table.schema.metadata[b"prajna.schema_version"], b"1")
                self.assertEqual(
                    table.schema.metadata[b"prajna.dsv"].decode(), manifest["dsv"]
                )
                for field in table.schema:
                    metadata = field.metadata or {}
                    if pa.types.is_decimal128(field.type):
                        self.assertEqual(field.type, pa.decimal128(38, 18))
                        self.assertEqual(metadata[b"prajna.decimal_scale"], b"18")
                for name in ("ts_open", "ts_close", "available_at"):
                    if name in table.column_names:
                        field = table.schema.field(name)
                        self.assertEqual(field.type, pa.timestamp("ns", tz="UTC"))
                        self.assertEqual(
                            field.metadata[b"prajna.time_role"], name.encode()
                        )
                if table_name == "bars":
                    bars = table.to_pydict()
                    self.assertEqual(bars["close"][0], Decimal("100.000000000000000000"))
                    self.assertEqual(bars["close"][1], Decimal("101.000000000000000000"))
                    self.assertEqual(
                        bars["volume"][0], Decimal("1000000.000000000000000000")
                    )
                    self.assertEqual(bars["ts_open"][0].isoformat(), "2026-01-05T01:30:00+00:00")
