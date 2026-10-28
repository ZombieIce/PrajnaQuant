#!/usr/bin/env python3
"""Synthetic production CLI -> HTTP -> persisted T01 execution. No external data.

Build quant-research (app), prajna-experiment bins and example selection_fixture;
pass the exclusive target/debug directory. All task identities stay in temp files.
"""
import json
from decimal import Decimal
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


def main():
    binaries = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="prajna-selection-synthetic-") as temp:
        root = Path(temp)
        lake = root / "lake"
        selections = json.loads(subprocess.check_output(
            [binaries / "examples/selection_fixture", lake], text=True
        ))
        env = dict(os.environ, PRAJNA_SELECTION_CLI=str(binaries / "prajna-freeze-selection"),
                   PRAJNA_EXPERIMENT_CLI=str(binaries / "prajna-experiment"), RAYON_NUM_THREADS="2")
        for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"]:
            env.pop(name, None)
        # A tiny clean synthetic repository supplies Experiment execution provenance.
        repo = root / "code"
        repo.mkdir()
        subprocess.run(["git", "init", "--quiet", repo], check=True)
        (repo / "source").write_text("synthetic selection e2e\n")
        subprocess.run(["git", "-C", repo, "add", "source"], check=True)
        subprocess.run(["git", "-C", repo, "-c", "user.name=Synthetic", "-c",
                        "user.email=synthetic@example.invalid", "-c", "commit.gpgsign=false",
                        "commit", "--quiet", "-m", "synthetic"], check=True)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        with (root / "server.log").open("wb") as log:
            service = subprocess.Popen(
                [binaries / "quant-research", "serve", "--output", lake, "--dataset-root", lake,
                 "--address", f"127.0.0.1:{port}", "--web-dist", root / "web",
                 "--market-data-dir", root / "market"], cwd=repo, env=env, stdout=log, stderr=log)
            try:
                def call(path, body=None, origin=None):
                    headers = {"Content-Type": "application/json"}
                    if origin:
                        headers["Origin"] = origin
                    req = Request(base + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
                    try:
                        response = urlopen(req, timeout=10)
                    except HTTPError as error:
                        response = error
                    with response:
                        return response.status, json.load(response)

                deadline = time.monotonic() + 20
                while True:
                    try:
                        call("/api/v1/health")
                        break
                    except URLError:
                        assert service.poll() is None, "service exited"
                        assert time.monotonic() < deadline, "service did not start"
                        time.sleep(0.05)
                for number, (instrument, dsv) in enumerate(selections.items()):
                    body = {"idempotency_key": f"synthetic-{number}", "dsv": dsv,
                            "instrument": instrument, "strategy": "buy_and_hold", "result_level": "standard"}
                    status, accepted = call("/api/v1/tasks/selections", body)
                    assert status == 202
                    task_id = accepted["task"]["task_id"]
                    directory = lake / "tasks" / task_id
                    saved = json.loads((directory / "experiment.json").read_text())
                    assert saved == accepted["selection"]["definition"]
                    assert saved["fast_event"]["initial_cash"] == "1000000"
                    status, reused = call("/api/v1/tasks/selections", body)
                    assert status == 200 and reused["task"]["task_id"] == task_id
                    conflict = dict(body, settings={"initial_cash": "2000000"})
                    assert call("/api/v1/tasks/selections", conflict)[0] == 409
                    deadline = time.monotonic() + 30
                    while True:
                        status, response = call(f"/api/v1/tasks/experiments/{task_id}")
                        task = response
                        if task["status"] in ["succeeded", "failed", "interrupted"]:
                            break
                        assert time.monotonic() < deadline, "task timed out"
                        time.sleep(0.05)
                    if task["status"] != "succeeded":
                        Path("/private/tmp/prajna-340-e2e-failure.json").write_text(json.dumps(task))
                        raise AssertionError("synthetic task failed; inspect local failure JSON")
                    assert task["result"]["failed_count"] == 0
                    status, result = call(f"/api/v1/tasks/experiments/{task_id}/result")
                    assert status == 200
                    run = result["data"]["runs"][0]
                    assert run["accounting_verified"]
                    rows = run["series"]["account_valuations"]
                    for row in rows:
                        positions = sum((Decimal(p["market_value"]) for p in row["positions"].values()), Decimal(0))
                        assert Decimal(row["equity"]) == Decimal(row["cash"]) + positions
                        assert Decimal(row["dividend_receivable"]) == 0
                        assert Decimal(row["liquidation_receivable"]) == 0
                        if row["session_date"] == "2026-01-05":
                            assert row["positions"] == {} and Decimal(row["equity"]) == 1000000
                        else:
                            assert Decimal(row["positions"][instrument]["quantity"]) == 99900
                            assert Decimal(row["cash"]) == Decimal("500.44006")
                            assert Decimal(row["equity"]) == Decimal("999500.44006" if row["valuation_point"] == "open_after" else "1009490.44006")
                assert call("/api/v1/tasks/selections", dict(body, settings=None))[0] == 400
                bad = dict(body, instrument="999999.XSHG", idempotency_key="nonmember")
                assert call("/api/v1/tasks/selections", bad)[0] == 404
                assert call("/api/v1/tasks/selections", body, "https://evil.invalid")[0] == 403
            finally:
                service.terminate()
                service.wait(timeout=10)
    print("Synthetic production selection HTTP/CLI/T01 E2E: PASS (Shanghai/Shenzhen ETFs, formal @2 inputs, actual fills and exact cash/NAV).")


if __name__ == "__main__":
    main()
