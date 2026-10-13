import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "backends/nautilus"))
from test_worker import packet, execute


class PreparedWorkerTests(unittest.TestCase):
    def test_cached_runs_match_fresh_run_fills_cash_and_positions(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = packet(Path(directory))
            prepare = {**messages[0], "type": "prepare"}
            requests = [prepare] + messages * 2 + [{"type": "shutdown"}]
            result = subprocess.run(
                [sys.executable, str(Path(__file__).with_name("prepared_worker.py"))],
                input="".join(json.dumps(m) + "\n" for m in requests),
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            responses = [json.loads(line) for line in result.stdout.splitlines()]
            self.assertEqual(responses[0]["type"], "baseline")
            prepared = responses[1]
            self.assertEqual(prepared["type"], "prepared")
            done = [i for i, row in enumerate(responses) if row["type"] == "done"]
            self.assertEqual(len(done), 2)
            first = responses[2:done[0] + 1]
            second = responses[done[0] + 2:done[1] + 1]
            self.assertEqual(first, second)
            diagnostic = Path(directory) / "backend-diagnostics.jsonl"
            cached_balances = [json.loads(line) for line in diagnostic.read_text().splitlines()
                               if json.loads(line)["event"] == "account_balance"]
            diagnostic.unlink()
            fresh = execute(messages)
            self.assertEqual(fresh.returncode, 0, fresh.stderr)
            self.assertEqual(first, [json.loads(line) for line in fresh.stdout.splitlines()])
            fresh_balances = [json.loads(line) for line in diagnostic.read_text().splitlines()
                              if json.loads(line)["event"] == "account_balance"]
            self.assertEqual(cached_balances, fresh_balances)
            self.assertEqual(cached_balances[-1]["positions"], {})
            self.assertEqual([r["quantity"] for r in first if r["type"] == "fill"], ["10", "10"])
            usage = [r for r in responses if r["type"] == "usage"]
            self.assertEqual(len(usage), 2)
            self.assertTrue(all(r["rss_bytes"] >= responses[0]["rss_bytes"] for r in usage))


if __name__ == "__main__":
    unittest.main()
