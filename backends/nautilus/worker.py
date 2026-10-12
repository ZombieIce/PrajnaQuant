"""Pinned Nautilus worker for the backend-neutral prajna-adapter@1 protocol."""

import json
import sys
from datetime import datetime, timedelta, timezone
from decimal import Decimal, localcontext, ROUND_HALF_EVEN
import importlib.metadata
from pathlib import Path

from nautilus_trader.backtest import BacktestEngine
from nautilus_trader.common import LogLevel
from nautilus_trader.config import BacktestEngineConfig, LoggerConfig, StrategyConfig
from nautilus_trader.execution import FeeModel
from nautilus_trader.model import (
    AccountType, ClientOrderId, Currency, Equity, InstrumentId, InstrumentStatus,
    MarketStatusAction, Money, OmsType, OrderSide, Price, Quantity, QuoteTick, Symbol, Venue,
)
from nautilus_trader.trading import Strategy

from packet import SCALE, decimal_value, encode, read_packet

VERSION = "prajna-adapter@1"
PINNED_VERSION = "2.0.0rc6"


def receive() -> dict:
    line = sys.stdin.readline()
    if not line:
        raise EOFError("controller closed the protocol stream")
    message = json.loads(line)
    if not isinstance(message, dict):
        raise ValueError("protocol message must be an object")
    return message


def send(message: dict) -> None:
    print(json.dumps(message, separators=(",", ":")), flush=True)


def exact_price(value: Decimal, increment: Decimal) -> Price:
    with localcontext() as context:
        context.prec = 80
        precision = max(0, -increment.normalize().as_tuple().exponent)
        aligned = value.quantize(Decimal(1).scaleb(-precision))
        if aligned != value or value % increment:
            raise ValueError(f"unrepresentable price {value} at increment {increment}; no rounding permitted")
    price = Price.from_str(encode(aligned))
    if price.as_decimal() != value:
        raise ValueError(f"Backend price conversion lost precision: {value}")
    return price


class Diagnostics:
    def __init__(self, path: Path) -> None:
        self.stream = path.open("x")

    def record(self, category: str, event: str, **fields) -> None:
        self.stream.write(json.dumps({"category": category, "event": event, **fields}) + "\n")
        self.stream.flush()


class ComparisonFees(FeeModel):
    def __init__(self, costs: dict, diagnostics: Diagnostics) -> None:
        self.costs = costs
        self.diagnostics = diagnostics

    def get_commission(self, order, fill_quantity, fill_px, instrument) -> Money:
        with localcontext() as context:
            context.prec = 80
            basis = (fill_quantity.as_decimal() * fill_px.as_decimal()).quantize(SCALE, ROUND_HALF_EVEN)
            proportional = (basis * self.costs["commission_rate"]).quantize(SCALE, ROUND_HALF_EVEN)
            commission = max(proportional, self.costs["minimum_commission"])
            side = "buy" if order.side == OrderSide.BUY else "sell"
            tax = (basis * self.costs[f"{side}_tax_rate"]).quantize(SCALE, ROUND_HALF_EVEN)
            expected = commission + tax
        money = Money.from_str(f"{encode(expected)} {instrument.quote_currency}")
        self.diagnostics.record(
            "backend_comparison", "fee_model", order_id=str(order.client_order_id),
            commission=encode(commission), tax=encode(tax), expected_fee=encode(expected),
            native_fee=encode(money.as_decimal()),
            difference_class="within_tolerance" if money.as_decimal() == expected else "backend_native",
            reason="Nautilus Money uses currency precision",
        )
        return money

    def get_commission_with_context(self, order, fill_quantity, fill_px, instrument, underlying_px=None):
        return self.get_commission(order, fill_quantity, fill_px, instrument)


class ShimConfig(StrategyConfig):
    def __init__(self, sessions, instruments, bars, statuses, costs, diagnostics) -> None:
        super().__init__()
        self.sessions = sessions
        self.instruments = instruments
        self.bars = bars
        self.statuses = statuses
        self.costs = costs
        self.diagnostics = diagnostics


class Shim(Strategy):
    def __init__(self, config: ShimConfig) -> None:
        super().__init__(config)
        self.sessions = config.sessions
        self.instruments = config.instruments
        self.bars = config.bars
        self.statuses = config.statuses
        self.costs = config.costs
        self.diagnostics = config.diagnostics
        self.failure = None
        self.pending_open = None
        self.pending_close = None
        self.active_order = None
        self.seen_orders = set()
        self.session = None
        self.finished = False

    def on_start(self) -> None:
        for instrument in self.instruments.values():
            self.subscribe_quotes(instrument.id)
            self.subscribe_instrument_status(instrument.id)
        for session in self.sessions:
            day = session["session_date"].isoformat()
            for kind in ("open", "close"):
                ns = session[f"ts_{kind}"] + 1_000_000
                when = datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=ns // 1000)
                self.clock.set_time_alert(f"{kind}:{day}", when)

    def on_time_event(self, event) -> None:
        self.guarded(self.barrier, event)

    def guarded(self, callback, *args) -> None:
        if self.failure is not None:
            return
        try:
            callback(*args)
        except (ValueError, KeyError, TypeError, EOFError, OSError, RuntimeError) as error:
            self.failure = error
            print(f"worker callback failed: {error}", file=sys.stderr)
            raise

    def barrier(self, event) -> None:
        kind, day = event.name.split(":")
        self.session = next(s for s in self.sessions if s["session_date"].isoformat() == day)
        if kind == "open":
            request = self.pending_open
            if request is None or request["type"] != "open":
                raise ValueError("expected Session open")
            if request["session_date"] != day or request["ts_open"] != str(self.session["ts_open"]):
                raise ValueError("Session open mismatch")
            expected_ids = {key[0] for key in self.bars if key[1] == day}
            quotes = request["quotes"]
            if len({q["instrument_id"] for q in quotes}) != len(quotes):
                raise ValueError("duplicate open quote")
            quoted_ids = {q["instrument_id"] for q in quotes}
            if not expected_ids <= quoted_ids or not quoted_ids <= {str(i) for i in self.instruments}:
                raise ValueError("open quotes differ from Run packet Instruments/bars")
            for quote in quotes:
                key = (quote["instrument_id"], day)
                if key not in self.bars:
                    if quote["tradable"] is not False:
                        raise ValueError("missing bar cannot be tradable")
                    for name in ("raw_open", "bid", "ask"):
                        decimal_value(quote[name])
                    continue
                raw = self.bars[key]["open"]
                bid, ask = synthetic_prices(raw, self.costs)
                if ([decimal_value(quote[name]) for name in ("raw_open", "bid", "ask")]
                        != [raw, bid, ask] or not isinstance(quote["tradable"], bool)):
                    raise ValueError("open quote differs from registered synthetic prices")
                if quote["tradable"] and not self.statuses[key]["is_tradable"]:
                    raise ValueError("open quote contradicts packet execution status")
            send({"type": "opened", "session_date": day})
            self.command()
        else:
            if self.pending_close is None:
                raise ValueError("Session advanced without close request")
            self.balance()
            send({"type": "closed", "session_date": day})
            message = receive()
            if day == self.sessions[-1]["session_date"].isoformat():
                if message != {"type": "end_run"}:
                    raise ValueError("expected end_run")
                self.finished = True
            else:
                self.pending_open = message
            self.pending_close = None

    def command(self) -> None:
        message = receive()
        day = self.session["session_date"].isoformat()
        if message["type"] == "close":
            if message["session_date"] != day or message["ts_close"] != str(self.session["ts_close"]):
                raise ValueError("Session close mismatch")
            self.pending_close = message
            return
        if message["type"] != "order" or self.active_order is not None:
            raise ValueError("expected an Order or Session close")
        if set(message) != {"type", "order_id", "portfolio_id", "instrument_id", "side", "quantity", "submitted_at"}:
            raise ValueError("unexpected Order fields")
        if (not isinstance(message["order_id"], str) or not message["order_id"]
                or message["order_id"] in self.seen_orders or not isinstance(message["portfolio_id"], str)
                or not message["portfolio_id"] or message["submitted_at"] != str(self.session["ts_open"])):
            raise ValueError("duplicate, stale or unidentified Order")
        instrument_id = InstrumentId.from_str(message["instrument_id"])
        instrument = self.instruments[instrument_id]
        quantity = decimal_value(message["quantity"])
        if quantity <= 0 or quantity % instrument.lot_size.as_decimal() != 0:
            raise ValueError("Order quantity must be a positive whole lot")
        native_quantity = Quantity.from_str(message["quantity"])
        if native_quantity.as_decimal() != quantity:
            raise ValueError("Backend quantity conversion lost precision")
        if message["side"] not in ("buy", "sell"):
            raise ValueError("unknown Order side")
        self.seen_orders.add(message["order_id"])
        self.active_order = message
        self.submit_order(self.order_factory.market(
            instrument_id=instrument.id,
            order_side=OrderSide.BUY if message["side"] == "buy" else OrderSide.SELL,
            quantity=native_quantity, client_order_id=ClientOrderId(message["order_id"]),
        ))

    def on_order_filled(self, event) -> None:
        self.guarded(self.filled, event)

    def filled(self, event) -> None:
        order = self.active_order
        if (order is None or str(event.client_order_id) != order["order_id"]
                or event.last_qty.as_decimal() != decimal_value(order["quantity"])):
            raise ValueError("unmapped or partial native Fill")
        self.diagnostics.record(
            "backend_native", "fill", order_id=order["order_id"], portfolio_id=order["portfolio_id"],
            native_ts_event=str(event.ts_event), protocol_executed_at=order["submitted_at"],
            instrument_id=order["instrument_id"], quantity=encode(event.last_qty.as_decimal()),
            price=encode(event.last_px.as_decimal()), fee=encode(event.commission.as_decimal()),
        )
        send({
            "type": "fill", **{k: v for k, v in order.items() if k not in ("type", "submitted_at")},
            "quantity": encode(event.last_qty.as_decimal()), "price": encode(event.last_px.as_decimal()),
            "fee": encode(event.commission.as_decimal()), "executed_at": order["submitted_at"],
        })
        self.active_order = None
        self.command()

    def on_order_rejected(self, event) -> None:
        self.guarded(self.rejected, event)

    def on_order_denied(self, event) -> None:
        self.guarded(self.rejected, event)

    def rejected(self, event) -> None:
        order = self.active_order
        if order is None or str(event.client_order_id) != order["order_id"]:
            raise ValueError("unmapped native rejection")
        self.diagnostics.record("backend_native", type(event).__name__, **order, reason=event.reason)
        send({k: order[k] for k in ("order_id", "portfolio_id", "instrument_id")} | {
            "type": "reject", "reason": event.reason,
        })
        self.active_order = None
        self.command()

    def on_instrument_status(self, event) -> None:
        self.diagnostics.record(
            "backend_native", "instrument_status", instrument_id=str(event.instrument_id),
            action=event.action.name, ts_event=str(event.ts_event),
        )

    def balance(self) -> None:
        instrument = next(iter(self.instruments.values()))
        account = self.cache.account_for_venue(instrument.id.venue)
        self.diagnostics.record(
            "backend_comparison", "account_balance",
            session_date=self.session["session_date"].isoformat(),
            currency=str(instrument.quote_currency),
            cash=encode(account.balance_total(instrument.quote_currency).as_decimal()),
            positions={next(str(domain) for domain, native in self.instruments.items()
                            if native.id == p.instrument_id): encode(p.quantity.as_decimal())
                       for p in self.cache.positions_open()},
        )


def synthetic_prices(raw: Decimal, costs: dict) -> tuple[Decimal, Decimal]:
    with localcontext() as context:
        context.prec = 80
        prices = (raw * (1 - costs["sell_slippage_rate"]), raw * (1 + costs["buy_slippage_rate"]))
        if any(value != value.quantize(SCALE, ROUND_HALF_EVEN) for value in prices):
            raise ValueError(f"unrepresentable synthetic price {prices} at protocol scale 18; no rounding permitted")
        return prices


def run(request: dict) -> None:
    if importlib.metadata.version("nautilus_trader") != PINNED_VERSION:
        raise RuntimeError(f"worker requires nautilus_trader=={PINNED_VERSION}")
    path = Path(request["packet"])
    specs, sessions, bars, statuses = read_packet(path)
    config = json.loads((path / "worker.json").read_text())
    initial_cash = decimal_value(config["initial_cash"])
    costs = {name: decimal_value(config["costs"][name]) for name in (
        "commission_rate", "minimum_commission", "buy_tax_rate", "sell_tax_rate",
        "buy_slippage_rate", "sell_slippage_rate",
    )}
    if initial_cash <= 0 or any(value < 0 for value in costs.values()):
        raise ValueError("positive initial_cash and nonnegative costs required")
    if any(costs[name] >= 1 for name in costs if name != "minimum_commission"):
        raise ValueError("cost rates must be less than one")
    diagnostics = Diagnostics(path / "backend-diagnostics.jsonl")
    engine = BacktestEngine(BacktestEngineConfig(logging=LoggerConfig(stdout_level=LogLevel.ERROR)))
    try:
        # SYNTH is a reserved Nautilus synthetic Venue, not an execution Venue.
        venue = Venue("PRAJNA")
        currency = Currency.from_str(specs[0]["quote_currency"])
        cash = Money.from_str(f"{encode(initial_cash)} {currency}")
        if cash.as_decimal() != initial_cash:
            raise ValueError("initial_cash cannot be represented by native currency precision")
        engine.add_venue(
            venue=venue, oms_type=OmsType.NETTING, account_type=AccountType.CASH,
            starting_balances=[cash], base_currency=currency,
            fee_model=ComparisonFees(costs, diagnostics),
        )
        instruments = {}
        for spec in specs:
            instrument_id = InstrumentId.from_str(spec["instrument_id"])
            native_id = InstrumentId.from_str(f"{spec['native_symbol']}.{venue}")
            if any(i.id == native_id for i in instruments.values()):
                raise ValueError("native Instrument mapping is not one-to-one")
            increment = exact_price(spec["price_increment"], spec["price_increment"])
            if increment.precision > 16:
                raise ValueError(
                    f"instrument precision {increment.precision} exceeds rc6 Position maximum 16 "
                    f"(price_increment={spec['price_increment']}); no rounding permitted"
                )
            instrument = Equity(
                instrument_id=native_id, raw_symbol=Symbol(spec["native_symbol"]),
                currency=currency, price_precision=increment.precision, price_increment=increment,
                lot_size=Quantity.from_str(encode(spec["lot_size"])), ts_event=0, ts_init=0,
            )
            instruments[instrument_id] = instrument
            engine.add_instrument(instrument)
            diagnostics.record(
                "adapter_construction", "instrument_mapping",
                instrument_id=str(instrument_id), native_instrument_id=str(native_id),
            )
        data = []
        for session in sessions:
            day = session["session_date"].isoformat()
            for instrument_id, instrument in instruments.items():
                key = (str(instrument_id), day)
                bar = bars.get(key)
                if bar is not None:
                    bid, ask = synthetic_prices(bar["open"], costs)
                    diagnostics.record(
                        "adapter_construction", "synthetic_bid_ask", session_date=day,
                        instrument_id=str(instrument_id), raw_open=encode(bar["open"]),
                        bid=encode(bid), ask=encode(ask),
                    )
                    for ts, bid_value, ask_value in (
                        (session["ts_open"], bid, ask),
                        (session["ts_close"], bar["close"], bar["close"]),
                    ):
                        data.append(QuoteTick(
                            instrument.id,
                            exact_price(bid_value, instrument.price_increment.as_decimal()),
                            exact_price(ask_value, instrument.price_increment.as_decimal()),
                            Quantity.from_int(1_000_000_000), Quantity.from_int(1_000_000_000), ts, ts,
                        ))
                action = (MarketStatusAction.TRADING if statuses[key]["is_tradable"]
                          else MarketStatusAction.HALT)
                ts = session["ts_open"] + 500_000
                data.append(InstrumentStatus(instrument.id, action, ts, ts))
                diagnostics.record(
                    "adapter_construction", "instrument_status", session_date=day,
                    instrument_id=str(instrument_id), action=action.name, ts_event=str(ts),
                )
        diagnostics.record(
            "adapter_construction", "lockstep_barrier", offset_ns="1000000",
            executed_at_mapping="native callback time -> requested Session open",
            fee_model="proportional + minimum commission + side tax; comparison only",
            backend={"name": "nautilus_trader", "version": PINNED_VERSION},
            lifecycle="new engine per Run",
        )
        engine.add_data(sorted(data, key=lambda item: item.ts_event))
        shim = Shim(ShimConfig(
            sessions=sessions, instruments=instruments, bars=bars,
            statuses=statuses, costs=costs, diagnostics=diagnostics,
        ))
        engine.add_strategy(shim)
        send({"type": "ready", "protocol": VERSION})
        shim.pending_open = receive()
        engine.run(end=sessions[-1]["ts_close"] + 2_000_000)
        if shim.failure is not None:
            raise RuntimeError(f"shim failed: {shim.failure}") from shim.failure
        if not shim.finished:
            raise RuntimeError("Backend exited before end_run")
        send({"type": "done"})
    except (ValueError, KeyError, TypeError, EOFError, OSError, RuntimeError) as error:
        diagnostics.record("adapter_error", "run_failed", reason=f"{type(error).__name__}: {error}")
        raise
    finally:
        engine.dispose()
        diagnostics.stream.close()


def main() -> None:
    request = receive()
    if request.get("type") != "begin_run" or request.get("protocol") != VERSION:
        raise ValueError("unsupported begin_run protocol")
    run(request)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, EOFError, OSError, RuntimeError) as error:
        send({"type": "error", "reason": f"{type(error).__name__}: {error}"})
        print(str(error), file=sys.stderr)
        sys.exit(1)
