#!/usr/bin/env python3
"""Idempotently create/update and publish the audited five-ETF manual Universe."""

import argparse
import json
import urllib.error
import urllib.request


SOURCE_REF = "prajnaquant:batch1-c:etf-five-mvp:v1"
NAME = "Batch 1 C — ETF Rotation MVP 五 ETF"
DESCRIPTION = (
    "ETF MVP; manually curated retrospective_static set, assumed investable only inside "
    "the independently audited 2025-09-23 through 2026-09-21 market-data window; "
    "zero ETF distributions; raw-price return, not total return. Membership is not verified PIT."
)
MEMBER_SOURCE = (
    "manual audited scenario; assumed investable within audited market-data window; "
    "no historical listing-status/PIT claim"
)
MEMBERS = [
    ("XSHG", "513300", "纳斯达克ETF华夏"),
    ("XSHG", "518880", "黄金ETF华安"),
    ("XSHE", "159612", "标普500ETF国泰"),
    ("XSHG", "510320", "沪深300ETF中金"),
    ("XSHE", "159952", "创业板ETF广发"),
]


def request_json(base_url, method, path, payload=None):
    body = None if payload is None else json.dumps(payload, ensure_ascii=False).encode()
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}{path}",
        data=body,
        method=method,
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")
        raise RuntimeError(f"{method} {path}: HTTP {error.code}: {detail}") from error


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:7878")
    args = parser.parse_args()
    base = args.base_url

    listing = request_json(base, "GET", "/api/v1/universes")
    matches = [item for item in listing["items"] if item["source_ref"] == SOURCE_REF]
    if len(matches) > 1:
        raise RuntimeError(f"multiple Universes use the reserved source_ref {SOURCE_REF!r}")

    created_universe = False
    if matches:
        universe_id = matches[0]["universe_id"]
        if matches[0]["status"] == "archived":
            raise RuntimeError(f"reserved Universe {universe_id} is archived; refusing to create a duplicate")
    else:
        created = request_json(
            base,
            "POST",
            "/api/v1/universes",
            {
                "name": NAME,
                "description": DESCRIPTION,
                "asset_scope": "etf",
                "source_kind": "manual",
                "source_ref": SOURCE_REF,
            },
        )
        universe_id = created["universe"]["universe_id"]
        created_universe = True

    members = [
        {
            "instrument": {
                "instrument_id": f"{exchange}:{code}",
                "exchange": exchange,
                "code": code,
                "name": name,
                "asset_type": "etf",
            },
            "effective_from": "2025-09-23",
            "effective_to": "2026-09-22",
            "source_ref": MEMBER_SOURCE,
        }
        for exchange, code, name in MEMBERS
    ]
    request_json(
        base,
        "PATCH",
        f"/api/v1/universes/{universe_id}/draft",
        {"name": NAME, "description": DESCRIPTION, "source_ref": SOURCE_REF, "members": members},
    )
    preview = request_json(
        base, "GET", f"/api/v1/universes/{universe_id}/versions/preview"
    )
    published = request_json(
        base,
        "POST",
        f"/api/v1/universes/{universe_id}/versions",
        {"expected_draft_hash": preview["draft_hash"]},
    )
    current = request_json(base, "GET", f"/api/v1/universes/{universe_id}")
    version = next(
        item for item in current["versions"] if item["version_id"] == published["version"]["version_id"]
    )
    member_snapshot = request_json(
        base,
        "GET",
        f"/api/v1/universes/{universe_id}/members?version_id={version['version_id']}&as_of=2026-09-21",
    )
    result = {
        "created_universe": created_universe,
        "universe_id": universe_id,
        "version_id": published["version"]["version_id"],
        "version_created": published["created"],
        "version_count": len(current["versions"]),
        "content_hash": version["content_hash"],
        "members": [member["instrument"]["code"] for member in member_snapshot["members"]],
        "pit_status": member_snapshot["pit_status"],
        "assumptions": [
            "assumed investable only from 2025-09-23 through 2026-09-21",
            "zero ETF distributions; raw-price return, not total return",
            "not verified historical PIT membership",
        ],
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
