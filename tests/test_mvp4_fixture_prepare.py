import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "prepare_fixture", ROOT / "poc/mvp4-correctness/prepare_fixture.py"
)
prepare_fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare_fixture)


class FixturePreparationTests(unittest.TestCase):
    def test_hand_lots_and_financial_inputs_are_preserved(self):
        for source in (ROOT / "poc/mvp3-golden/fixtures").glob("*.json"):
            with self.subTest(source=source.name):
                original = json.loads(source.read_text())
                prepared = prepare_fixture.prepare(source)
                self.assertEqual(prepared["costs"], original["costs"])
                self.assertEqual(prepared["calendar"], original["calendar"])
                self.assertEqual(prepared["account"]["initial_cash"], original["account"]["initial_cash"])
                for before, after in zip(original["instruments"], prepared["instruments"]):
                    self.assertEqual(prepared["account"]["lot_size"], before["lot_size"])
                    for key, value in before.items():
                        self.assertEqual(after[key], value)

    def test_ma_expansion_has_130_sessions_and_original_lots_and_prices(self):
        source = ROOT / "poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json"
        original = json.loads(source.read_text())
        prepared = prepare_fixture.prepare(source)
        self.assertEqual(len(prepared["calendar"]), 130)
        self.assertEqual(prepared["account"]["lot_size"], original["lot_size"])
        self.assertEqual(prepared["bar_defaults"]["open"], original["open"])
        self.assertEqual(prepared["instruments"][0]["closes"], [100] * 60 + [110] * 20 + [80] * 50)

    def test_mixed_lots_fail_instead_of_changing_the_fixture(self):
        fixture = json.loads((ROOT / "poc/mvp3-golden/fixtures/hand-retry-v1.json").read_text())
        fixture["instruments"][1]["lot_size"] = 10
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "mixed.json"
            source.write_text(json.dumps(fixture))
            with self.assertRaisesRegex(ValueError, "one explicit common fixture lot size"):
                prepare_fixture.prepare(source)


if __name__ == "__main__":
    unittest.main()
