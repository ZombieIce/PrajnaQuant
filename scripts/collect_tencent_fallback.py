"""Collect explicitly planned missing TDX cells, under #172 §3.

No default scans. Real responses and request logs stay below ~/PrajnaData/mvp-6.
Raw bytes are saved unchanged for later freeze_raw ingestion (not a RawStore).
"""
import argparse
import datetime as dt
import fcntl
import hashlib
import json
from pathlib import Path
import time
import urllib.error
import urllib.parse
import urllib.request

HOST = "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get"
START = dt.date(2022, 10, 1)  # MVP-6 warm-up
END = dt.date(2026, 9, 30)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # Do not follow login/WAF or alternate-host redirects.


class Collector:
    def __init__(self, root, network_path, *, opener=None, sleep=time.sleep,
                 code_ranges=((158000, 159999),)):
        self.root = Path(root).expanduser().resolve()
        allowed = (Path.home() / "PrajnaData/mvp-6").resolve()
        if not self.root.is_relative_to(allowed):
            raise ValueError("output must stay below ~/PrajnaData/mvp-6")
        self.pacing_path = allowed / "acceptance/tencent-pacing.lock"
        if not network_path.strip():
            raise ValueError("record the actual direct domestic network path")
        self.code_ranges = tuple(sorted(code_ranges))
        if not self.code_ranges or any(a < 100000 or a > b or b > 999999 for a, b in self.code_ranges):
            raise ValueError("invalid six-digit code ranges")
        self.network_path = network_path
        # Explicitly ignore proxy environment variables; never send browser cookies.
        self.opener = opener or urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect())
        self.sleep = sleep
        self.stopped = False
        self.access_failures = 0

    def _record(self, url, params, status, body, error):
        digest = hashlib.sha256(body).hexdigest()
        objects = self.root / "raw/tencent-responses"
        objects.mkdir(parents=True, exist_ok=True)
        destination = objects / digest
        if not destination.exists():
            with destination.open("xb") as out:
                out.write(body)
        record = dict(url=url, parameters=params,
                      observed_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                      http_status=status, byte_len=len(body), sha256=digest,
                      network_path=self.network_path, transport="direct_no_proxy",
                      terms_conclusion="issue_172_owner_authorization_for_local_noncommercial_research",
                      error=error, outcome="unresolved" if error else "success")
        logs = self.root / "acceptance"
        logs.mkdir(parents=True, exist_ok=True)
        with (logs / "tencent-requests.jsonl").open("a", encoding="utf-8") as out:
            out.write(json.dumps(record, ensure_ascii=False) + "\n")
        return destination

    def collect(self, symbol, session):
        """One missing cell only. Planner must exclude TDX no-trade observations."""
        day = dt.date.fromisoformat(session)
        if not (len(symbol) == 6 and symbol.isascii() and symbol.isdigit()
                and any(a <= int(symbol) <= b for a, b in self.code_ranges) and START <= day <= END):
            raise ValueError("cell outside declared MVP-6 scope")
        params = {"param": f"sz{symbol},day,{day},{day},1,"}
        url = HOST + "?" + urllib.parse.urlencode(params)
        self.pacing_path.parent.mkdir(parents=True, exist_ok=True)
        with self.pacing_path.open("a+", encoding="ascii") as pacing:
            fcntl.flock(pacing.fileno(), fcntl.LOCK_EX)
            try:
                pacing.seek(0)
                has_requested = bool(pacing.read().strip())
                if self.stopped:
                    raise RuntimeError("source stopped: unresolved access restriction")
                for attempt in range(4):  # initial request + at most three retries
                    if has_requested:
                        self.sleep(1)
                    pacing.seek(0)
                    pacing.truncate()
                    pacing.write("requested\n")
                    pacing.flush()
                    has_requested = True
                    request = urllib.request.Request(url, headers={
                        "User-Agent": "PrajnaQuant/0.1 (personal MVP-6 research collector)"})
                    status, body, error = None, b"", None
                    try:
                        with self.opener.open(request, timeout=30) as response:
                            status = response.status
                            body = response.read()
                    except urllib.error.HTTPError as exc:
                        status, body = exc.code, exc.read()
                        error = f"http_{status}"
                    except (OSError, urllib.error.URLError) as exc:
                        error = type(exc).__name__
                    text = body.decode("utf-8", errors="replace").lower()
                    blocked = status in (403, 501) or any(
                        word in text for word in ("captcha", "验证码", "访问验证"))
                    if blocked:
                        self.access_failures += 1
                        error = "access_restriction"
                        if self.access_failures >= 2:
                            self.stopped = True
                    else:
                        self.access_failures = 0
                    if error is None:
                        try:
                            data = json.loads(body)
                            rows = data["data"][f"sz{symbol}"]["day"]
                            if str(data["code"]) != "0" or not isinstance(rows, list):
                                raise ValueError("bad unadjusted day response")
                            if any(not isinstance(row, list) or len(row) < 6 or row[0] != session for row in rows):
                                raise ValueError("response outside requested session")
                            if not rows:
                                error = "missing_cell"  # Empty is not a successful Bar.
                        except (ValueError, KeyError, TypeError):
                            error = "invalid_unadjusted_response"
                    destination = self._record(url, params, status, body, error)
                    if error is None:
                        return destination
                    if self.stopped:
                        break
                    if attempt < 3:
                        self.sleep(2 ** attempt)
                raise RuntimeError("Tencent cell unresolved; see local request log")
            finally:
                fcntl.flock(pacing.fileno(), fcntl.LOCK_UN)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plan", type=Path, help="local JSON list of {symbol, session, tdx_observation: missing}")
    parser.add_argument("--root", type=Path, default=Path.home() / "PrajnaData/mvp-6")
    parser.add_argument("--network-path", required=True, help="actual domestic direct network path, recorded locally")
    parser.add_argument("--code-range", action="append", help="inclusive FIRST-LAST; repeatable; default 158000-159999")
    args = parser.parse_args()
    ranges = tuple(tuple(map(int, text.split("-"))) for text in (args.code_range or ["158000-159999"]))
    plan = json.loads(args.plan.read_text())
    if not isinstance(plan, list) or any(
        not isinstance(cell, dict) or set(cell) != {"symbol", "session", "tdx_observation"}
        or cell["tdx_observation"] != "missing" for cell in plan
    ):
        parser.error("plan must contain only explicitly missing TDX cells, never no-trade cells")
    collector = Collector(args.root, args.network_path, code_ranges=ranges)
    for cell in plan:
        collector.collect(cell["symbol"], cell["session"])


if __name__ == "__main__":
    main()
