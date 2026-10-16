"""Synthetic responses only. No sockets, no actual home-directory writes."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

SPEC = importlib.util.spec_from_file_location(
    "collector", Path(__file__).resolve().parents[1] / "scripts/collect_tencent_fallback.py")
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)


class Response(io.BytesIO):
    status = 200


class Opener:
    def __init__(self, replies):
        self.replies = iter(replies)
        self.requests = []

    def open(self, request, timeout):
        self.requests.append(request)
        reply = next(self.replies)
        if isinstance(reply, Exception):
            raise reply
        return Response(reply)


GOOD = b'{"code":0,"data":{"sz158001":{"day":[["2026-01-05","1","1","1","1","1"]]}}}'


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.now = 0
        self.sleeps = []
        self.home_patch = patch.object(Path, "home", return_value=self.home)
        self.home_patch.start()
        self.addCleanup(self.home_patch.stop)

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds

    def client(self, replies, **kwargs):
        self.opener = Opener(replies)
        self.root = self.home / "PrajnaData/mvp-6"
        return collector.Collector(self.root, "synthetic domestic direct path",
                                   opener=self.opener, clock=lambda: self.now,
                                   sleep=self.sleep, **kwargs)

    def logs(self):
        return [json.loads(line) for line in (self.root / "acceptance/tencent-requests.jsonl").read_text().splitlines()]

    def test_unadjusted_exact_cell_bytes_headers_and_serial_interval(self):
        client = self.client([GOOD, GOOD])
        path = client.collect("158001", "2026-01-05")
        self.assertEqual(path.read_bytes(), GOOD)
        client.collect("158001", "2026-01-05")
        self.assertGreaterEqual(sum(self.sleeps), 1)
        request = self.opener.requests[0]
        self.assertEqual(dict(request.header_items()), {
            "User-agent": "PrajnaQuant/0.1 (personal MVP-6 research collector)"})
        self.assertIn("sz158001%2Cday%2C2026-01-05%2C2026-01-05%2C1%2C", request.full_url)
        self.assertEqual(len(self.logs()), 2)
        self.assertEqual(self.logs()[0]["outcome"], "success")
        self.assertEqual(self.logs()[0]["transport"], "direct_no_proxy")
        self.assertEqual(
            self.logs()[0]["terms_conclusion"],
            "issue_172_owner_authorization_for_local_noncommercial_research",
        )

    def test_maximum_three_retries_exponential_backoff_and_failure_logs(self):
        client = self.client([OSError("synthetic")] * 4)
        with self.assertRaisesRegex(RuntimeError, "unresolved"):
            client.collect("158001", "2026-01-05")
        self.assertEqual(len(self.opener.requests), 4)
        self.assertTrue(all(n in self.sleeps for n in (1, 2, 4)))
        self.assertEqual(len(self.logs()), 4)
        self.assertTrue(all(row["outcome"] == "unresolved" for row in self.logs()))

    def test_access_restrictions_stop_source_across_cells(self):
        for body, status in [(b"forbidden", 403), (b"blocked", 501), (b"captcha", 200)]:
            with self.subTest(status=status):
                replies = [body] * 2 if status == 200 else [
                    urllib.error.HTTPError(collector.HOST, status, "denied", {}, io.BytesIO(body)) for _ in range(2)]
                client = self.client(replies)
                for _ in range(2):
                    with self.assertRaises(RuntimeError):
                        client.collect("158001", "2026-01-05")
                self.assertTrue(client.stopped)
                self.assertEqual(len(self.opener.requests), 2)

    def test_no_proxy_or_redirect_handlers(self):
        with patch.object(collector.urllib.request, "build_opener") as build:
            collector.Collector(self.home / "PrajnaData/mvp-6", "synthetic")
        self.assertEqual(build.call_args.args[0].proxies, {})
        self.assertIsInstance(build.call_args.args[1], collector.NoRedirect)
        self.assertIsNone(collector.NoRedirect().redirect_request(None, None, 302, "", {}, "https://example.com"))

    def test_scope_and_storage_fail_before_request(self):
        client = self.client([])
        for symbol, day in [("000001", "2026-01-05"), ("158001", "2026-10-01")]:
            with self.assertRaises(ValueError):
                client.collect(symbol, day)
        self.assertEqual(self.opener.requests, [])
        with self.assertRaises(ValueError):
            collector.Collector(self.home / "public-repo", "synthetic")

    def test_empty_adjusted_or_out_of_window_responses_never_succeed(self):
        for body in [b'{"code":0,"data":{"sz158001":{"day":[]}}}',
                     GOOD.replace(b'"day"', b'"qfqday"'),
                     GOOD.replace(b'2026-01-05', b'2026-01-06')]:
            with self.subTest(body=body):
                client = self.client([body] * 4)
                with self.assertRaises(RuntimeError):
                    client.collect("158001", "2026-01-05")
                self.assertNotEqual(self.logs()[-1]["outcome"], "success")


if __name__ == "__main__":
    unittest.main()
