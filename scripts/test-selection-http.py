#!/usr/bin/env python3
"""Synthetic production CLI -> HTTP -> persisted T01 execution. No external data.

Build quant-research (app), prajna-experiment bins and example selection_fixture;
pass the exclusive target/debug directory. All task identities stay in temp files.
A failed run retains its per-run temporary directory for local inspection.
"""
import contextlib
from decimal import Decimal
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


@contextlib.contextmanager
def retained_temp(prefix):
    """Keep the per-run directory on failure; never write evidence to a fixed path."""
    path = Path(tempfile.mkdtemp(prefix=prefix))
    try:
        yield path
    except BaseException:
        print(f"Selection E2E failed; synthetic evidence retained at {path}", file=sys.stderr)
        raise
    else:
        shutil.rmtree(path, ignore_errors=True)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def start(binaries, root, repo, env, output, dataset_root=None):
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    args = [binaries / "quant-research", "serve", "--output", output,
            "--address", f"127.0.0.1:{port}", "--web-dist", root / "web",
            "--market-data-dir", root / "market"]
    if dataset_root is not None:
        args += ["--dataset-root", dataset_root]
    log = (root / f"server-{port}.log").open("wb")
    service = subprocess.Popen(args, cwd=repo, env=env, stdout=log, stderr=log)

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
    return service, log, call


def main():
    binaries = Path(sys.argv[1]).resolve()
    with retained_temp("prajna-selection-synthetic-") as root:
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
        service, log, call = start(binaries, root, repo, env, lake, lake)
        try:
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
                universe_id = saved["universe"]
                assert universe_id.startswith("uni:sha256:")
                assert (lake / "universes" / f"{universe_id.removeprefix('uni:sha256:')}.json").is_file()
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
                    failure = root / "task.json"
                    failure.write_text(json.dumps(task))
                    raise AssertionError(f"synthetic task failed; local evidence {failure}")
                assert task["result"]["failed_count"] == 0
                status, result = call(f"/api/v1/tasks/experiments/{task_id}/result")
                assert status == 200
                run = result["data"]["runs"][0]
                assert run["accounting_verified"]
                rows = run["series"]["account_valuations"]
                assert len(rows) == 6, rows
                assert {(row["session_date"], row["valuation_point"]) for row in rows} == {
                    (session, point) for session in ["2026-01-05", "2026-01-06", "2026-01-07"]
                    for point in ["open_after", "close"]}
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
            # The legacy user-universe registry shares `universes/` with the
            # content-addressed StaticUniverse files written above.
            status, listed = call("/api/v1/universes")
            assert status == 200 and listed["items"] == [], listed
            assert call("/api/v1/tasks/selections", dict(body, settings=None))[0] == 400
            bad = dict(body, instrument="999999.XSHG", idempotency_key="nonmember")
            assert call("/api/v1/tasks/selections", bad)[0] == 404
            assert call("/api/v1/tasks/selections", body, "https://evil.invalid")[0] == 403
        finally:
            service.terminate()
            service.wait(timeout=10)
            log.close()

        # The production `serve` wiring, not only the private router, owns these
        # root errors; each case starts a real service.
        other = root / "other-dataset"
        other.mkdir()
        for expected_status, code, dataset_root in [
                (503, "dataset_root_unconfigured", None),
                (409, "dataset_root_mismatch", other)]:
            output = root / f"root-{expected_status}"
            output.mkdir()
            second, second_log, second_call = start(binaries, root, repo, env, output, dataset_root)
            try:
                status, failed = second_call("/api/v1/tasks/selections", {
                    "idempotency_key": f"root-{expected_status}", "dsv": "synthetic",
                    "instrument": "510300.XSHG", "strategy": "buy_and_hold"})
                assert status == expected_status and failed["error"]["code"] == code, failed
            finally:
                second.terminate()
                second.wait(timeout=10)
                second_log.close()
    print("Synthetic production selection HTTP/CLI/T01 E2E: PASS (Shanghai/Shenzhen ETFs, formal @2 inputs, actual fills and exact cash/NAV).")


if __name__ == "__main__":
    main()
