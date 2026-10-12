"""Python stand-in for Rust; no execution logic is delegated to the worker."""

import argparse
from decimal import Decimal, localcontext
import json
from pathlib import Path
import platform
from queue import Queue
import subprocess
import sys
import tempfile
from threading import Thread

from protocol import VERSION, WIRE_PROBE, decimal_value, encode, send, write_packet


def order(key: str, vp: str, symbol: str, side: str, quantity: Decimal) -> dict:
    return {
        "order_id": key, "virtual_portfolio_id": vp,
        "instrument_id": f"{symbol}.SIM", "side": side, "quantity": encode(quantity),
    }


def session_orders(session: str, cash: dict[str, Decimal],
                   positions: dict[str, dict[str, Decimal]], *, halt: bool) -> list[dict]:
    if halt:
        return [order(f"{session}-halt", "alpha", "C", "BUY", Decimal(1))]
    if session == "s1":
        return [order("s1-a", "alpha", "A", "BUY", Decimal(10)),
                order("s1-b", "beta", "B", "BUY", Decimal(5))]
    if session == "s2":
        return [
            order("s2-a", "alpha", "A", "SELL", positions["alpha"]["A.SIM"]),
            order("s2-c", "alpha", "C", "BUY", cash["alpha"] // Decimal("30.30")),
        ]
    if session == "s3":
        return [
            order("s3-b", "beta", "B", "SELL", positions["beta"]["B.SIM"]),
            order("s3-c", "alpha", "C", "SELL", positions["alpha"]["C.SIM"]),
            order("s3-a", "alpha", "A", "BUY", cash["alpha"] // Decimal("13.13")),
        ]
    raise ValueError(f"unknown Session: {session}")


def run_fixture(*, halt: bool = False, reset: bool = False) -> dict:
    """Run a fresh worker; stdout must contain only protocol JSON lines."""
    with localcontext() as context:
        context.prec = 80
        return _run_fixture(halt=halt, reset=reset)


def _run_fixture(*, halt: bool, reset: bool) -> dict:
    cash = {"alpha": Decimal("600.00"), "beta": Decimal("400.00")}
    positions: dict[str, dict[str, Decimal]] = {"alpha": {}, "beta": {}}
    events, ledger = [], []
    close_marks = {
        "s1": {"A.SIM": Decimal(11), "B.SIM": Decimal(21), "C.SIM": Decimal(30)},
        "s2": {"A.SIM": Decimal(12), "B.SIM": Decimal(19), "C.SIM": Decimal(31)},
        "s3": {"A.SIM": Decimal(14), "B.SIM": Decimal(18), "C.SIM": Decimal(32)},
    }
    pending: dict[str, dict] = {}
    with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryFile(mode="w+") as stderr:
        packet = Path(directory) / "run.arrow"
        write_packet(packet, halt=halt)
        process = subprocess.Popen(
            [sys.executable, str(Path(__file__).with_name("worker.py"))],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True,
        )
        lines: Queue[str | None] = Queue()

        def read_lines() -> None:
            for line in process.stdout:
                lines.put(line)
            lines.put(None)

        reader = Thread(target=read_lines, daemon=True)
        reader.start()
        try:
            send(process.stdin, {
                "type": "start", "protocol": VERSION, "packet": str(packet),
                "initial_cash": "1000.00", "reset_probe": reset,
                "decimal_probe": WIRE_PROBE,
            })
            while True:
                line = lines.get(timeout=30)
                if line is None:
                    raise RuntimeError("worker exited before done")
                event = json.loads(line)
                events.append(event)
                kind = event["type"]
                if kind == "session_open":
                    if pending:
                        raise RuntimeError(f"orders unresolved at next Session: {pending}")
                    orders = session_orders(event["session_id"], cash, positions, halt=halt)
                    pending = {o["order_id"]: o for o in orders}
                    send(process.stdin, {
                        "type": "orders", "session_id": event["session_id"], "orders": orders,
                    })
                elif kind in {"fill", "reject"}:
                    original = pending.pop(event["order_id"])
                    for key in ("virtual_portfolio_id", "instrument_id", "side", "quantity"):
                        if event[key] != original[key]:
                            raise RuntimeError(f"Fill/Reject attribution mismatch: {event}")
                    if kind == "fill":
                        vp, instrument = event["virtual_portfolio_id"], event["instrument_id"]
                        quantity, price = decimal_value(event["quantity"]), decimal_value(event["price"])
                        if decimal_value(event["backend_fee"]) != 0:
                            raise RuntimeError("zero-fee fixture received a nonzero Backend fee")
                        signed = quantity if event["side"] == "BUY" else -quantity
                        cash[vp] -= signed * price
                        held = positions[vp].get(instrument, Decimal(0)) + signed
                        if cash[vp] < 0 or held < 0:
                            raise RuntimeError("long-only VP violated cash/position constraints")
                        if held:
                            positions[vp][instrument] = held
                        else:
                            positions[vp].pop(instrument)
                elif kind == "session_close":
                    if pending:
                        raise RuntimeError(f"orders unresolved at close: {pending}")
                    total = sum(cash.values())
                    combined: dict[str, Decimal] = {}
                    for holdings in positions.values():
                        for instrument, quantity in holdings.items():
                            combined[instrument] = combined.get(instrument, Decimal(0)) + quantity
                    native_positions = {
                        key: decimal_value(value) for key, value in event["backend_positions"].items()
                    }
                    if total != decimal_value(event["backend_cash"]) or combined != native_positions:
                        raise RuntimeError(f"Backend/account continuity mismatch: {event}")
                    nav = total + sum(q * close_marks[event["session_id"]][i] for i, q in combined.items())
                    ledger.append({
                        "session_id": event["session_id"], "cash": encode(total),
                        "positions": {i: encode(q) for i, q in combined.items()}, "nav": encode(nav),
                        "backend_cash": event["backend_cash"],
                        "virtual_portfolios": {
                            vp: {"cash": encode(cash[vp]),
                                 "positions": {i: encode(q) for i, q in positions[vp].items()}}
                            for vp in cash
                        },
                    })
                elif kind == "done":
                    break
                elif kind == "reset":
                    if not reset or pending or len(ledger) != 3:
                        raise RuntimeError("unexpected reset boundary")
                    cash = {"alpha": Decimal("600.00"), "beta": Decimal("400.00")}
                    positions = {"alpha": {}, "beta": {}}
                elif kind not in {"ready", "backend_status"}:
                    raise RuntimeError(f"unexpected worker message: {event}")
            process.stdin.close()
            if process.wait(timeout=30) != 0:
                raise RuntimeError("worker returned nonzero")
            reader.join(timeout=5)
            if len(ledger) != (6 if reset else 3):
                raise RuntimeError("worker did not complete all Sessions")
        except Exception as error:
            # Include native stderr in failures; never turn a failed probe into success.
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            stderr.seek(0)
            raise RuntimeError(f"{error}\nworker stderr:\n{stderr.read()}") from error
        finally:
            process.stdout.close()
            if not process.stdin.closed:
                process.stdin.close()
        stderr.seek(0)
        native_logs = stderr.read()
        if native_logs:
            raise RuntimeError(f"unexpected native stderr:\n{native_logs}")
    return {
        "protocol": VERSION, "selected_lockstep": "synchronous_stdio_strategy_shim",
        "python": platform.python_version(), "platform": platform.platform(),
        "events": events, "ledger": ledger, "native_stderr": native_logs,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--halt", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = run_fixture(halt=args.halt)
    text = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
