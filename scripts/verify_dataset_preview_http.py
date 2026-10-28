"""Synthetic production HTTP/CLI parity check; run against the exported fixture only."""
import argparse
import hashlib
import json
import subprocess
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:7878")
    parser.add_argument("--reader", required=True)
    parser.add_argument("--fixture-root", type=Path, required=True)
    args = parser.parse_args()
    assert urllib.parse.urlparse(args.url).hostname in ("127.0.0.1", "localhost", "::1")
    assert (args.fixture_root / "synthetic-preview-fixture").read_text() == "synthetic only\n"
    lake = args.fixture_root / "lake"

    def tree():
        return {str(p.relative_to(lake)): hashlib.sha256(p.read_bytes()).digest()
                for p in lake.rglob("*") if p.is_file()}

    def http(path, status=200, headers=None, method="GET"):
        request = urllib.request.Request(args.url.rstrip("/") + path, headers=headers or {}, method=method)
        try:
            response = urllib.request.urlopen(request, timeout=60)
        except urllib.error.HTTPError as error:
            response = error
        assert response.status == status, (path, response.status, status)
        body = response.read()
        assert response.headers["Cache-Control"] == "no-store"
        assert response.headers.get("Access-Control-Allow-Origin") is None
        assert response.headers["Content-Type"].startswith("application/json")
        return body

    def parity(path, *command):
        expected = subprocess.check_output([args.reader, *command, "--root", str(lake)])
        actual = http(path)
        assert actual == expected, "HTTP must preserve production CLI envelope byte-for-byte"
        return json.loads(actual)["data"]

    before = tree()
    for origin in ("https://synthetic-review.invalid", "null", "http://127.0.0.1:1"):
        for method in ("GET", "OPTIONS"):
            for path in ("/api/v1/datasets", "/api/v1/datasets/synthetic", "/api/v1/datasets/synthetic/bars?instrument=x"):
                http(path, 403, {"Origin": origin, "Access-Control-Request-Method": "GET"}, method)
    http("/api/v1/datasets", headers={"Origin": args.url.rstrip("/")})
    http("/api/v1/datasets", 403, {"Host": "synthetic-review.invalid"})
    catalog = parity("/api/v1/datasets", "catalog")
    assert len(catalog["datasets"]) == 2 and not catalog["invalid_manifests"]
    kinds = set()
    for dataset in catalog["datasets"]:
        dsv = dataset["dsv"]
        path = "/api/v1/datasets/" + urllib.parse.quote(dsv, safe="")
        detail = parity(path, "show", "--dsv", dsv)
        instrument = detail["instruments"][0]
        kinds.add(instrument["kind"])
        assert instrument["coverage"]["session_gaps"] == ["2026-01-06", "2026-01-07"]
        if instrument["kind"] == "equity":
            assert instrument["lifecycle"]["listing_date"]["state"] == "unknown"
        for offset in (0, 1):
            query = {"instrument": instrument["instrument_id"], "limit": "1", "offset": str(offset)}
            bars = parity(path + "/bars?" + urllib.parse.urlencode(query), "bars", "--dsv", dsv,
                          "--instrument", instrument["instrument_id"], "--limit", "1", "--offset", str(offset))
            assert bars["total"] == 2 and len(bars["items"]) == 1
            assert bars["items"][0]["amount"] is None
            assert bars["items"][0]["available_at"] is None
            assert isinstance(bars["items"][0]["close"], str)
        base = path + "/bars?" + urllib.parse.urlencode({"instrument": instrument["instrument_id"]})
        for suffix in ("&limit=0", "&limit=1001", "&offset=-1", "&start=bad", "&start=2026-01-09&end=2026-01-05", "&adjustment=qfq"):
            http(base + suffix, 400)
        http(path + "/bars?instrument=foreign", 404)
        empty = json.loads(http(base + "&start=2030-01-01"))["data"]
        assert empty["items"] == [] and empty["total"] == 0
    assert kinds == {"etf", "equity"}
    http("/api/v1/datasets/invalid", 400)
    http("/api/v1/datasets/" + "0" * 64, 404)
    assert before == tree(), "HTTP preview must not modify the lake"
    print("PASS: synthetic stock/ETF HTTP equals production CLI; Unknown/gaps, pagination, errors, empty range and read-only lake verified.")


if __name__ == "__main__":
    main()
