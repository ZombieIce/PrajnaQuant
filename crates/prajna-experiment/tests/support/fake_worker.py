"""Transport test double, NOT Nautilus integration or #133 A15 evidence."""
import json
import sys
from decimal import Decimal, localcontext
from pathlib import Path


def send(message):
    print(json.dumps(message), flush=True)


def main():
    begin = json.loads(sys.stdin.readline())
    packet = Path(begin["packet"])
    config = json.loads((packet / "worker.json").read_text())
    cash = Decimal(config["initial_cash"])
    costs = {key: Decimal(value) for key, value in config["costs"].items()}
    positions = {}
    quotes = {}
    with (packet / "backend-diagnostics.jsonl").open("x") as diagnostics:
        def record(category, event, **fields):
            diagnostics.write(json.dumps({
                "category": category, "event": event, "transport_test_double": True, **fields,
            }) + "\n")
            diagnostics.flush()

        record("adapter_construction", "transport_test_double")
        send({"type": "ready", "protocol": begin["protocol"]})
        for line in sys.stdin:
            request = json.loads(line)
            kind = request["type"]
            if kind == "open":
                quotes = {q["instrument_id"]: q for q in request["quotes"]}
                send({"type": "opened", "session_date": request["session_date"]})
            elif kind == "order":
                instrument = request["instrument_id"]
                quantity = Decimal(request["quantity"])
                price = Decimal(quotes[instrument]["ask" if request["side"] == "buy" else "bid"])
                basis = (quantity * price).quantize(Decimal("1e-18"))
                commission = max(
                    (basis * costs["commission_rate"]).quantize(Decimal("1e-18")),
                    costs["minimum_commission"],
                )
                tax = (basis * costs[request["side"] + "_tax_rate"]).quantize(Decimal("1e-18"))
                fee = (commission + tax).quantize(Decimal("0.01"))
                sign = 1 if request["side"] == "buy" else -1
                cash -= sign * basis + fee
                positions[instrument] = positions.get(instrument, Decimal(0)) + sign * quantity
                fill = {key: request[key] for key in (
                    "order_id", "portfolio_id", "instrument_id", "side", "quantity",
                )}
                fill.update(price=str(price), fee=str(fee), executed_at=request["submitted_at"])
                record("backend_native", "transport_fill", **fill)
                send({"type": "fill", **fill})
            elif kind == "close":
                record(
                    "backend_comparison", "account_balance",
                    session_date=request["session_date"], currency="CNY", cash=str(cash),
                    positions={key: str(value) for key, value in positions.items() if value},
                )
                send({"type": "closed", "session_date": request["session_date"]})
            elif kind == "end_run":
                send({"type": "done"})
                return
            else:
                raise ValueError(f"unexpected request {kind}")
    raise EOFError("missing end_run")


if __name__ == "__main__":
    with localcontext() as context:
        context.prec = 80
        main()
