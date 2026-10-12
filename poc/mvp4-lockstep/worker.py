"""A real Nautilus shim: external orders in, native Fill/Reject out."""

from datetime import datetime, timedelta, timezone
from decimal import Decimal, localcontext
import importlib.metadata
import sys
from pathlib import Path

from nautilus_trader.backtest import BacktestEngine
from nautilus_trader.common import LogLevel
from nautilus_trader.config import BacktestEngineConfig, LoggerConfig, StrategyConfig
from nautilus_trader.execution import FixedFeeModel
from nautilus_trader.model import (
    AccountType, ClientOrderId, Currency, Equity, InstrumentId, InstrumentStatus,
    MarketStatusAction, Money, OmsType, OrderSide, Price, Quantity, QuoteTick,
    Symbol, Venue,
)
from nautilus_trader.trading import Strategy

from protocol import (
    PINNED_VERSION, VERSION, decimal_value, encode, read_packet, receive, send,
    synthetic_prices,
)


def exact_price(text: str, precision: int, increment: Price | None = None) -> Price:
    value = decimal_value(text)
    with localcontext() as context:
        context.prec = 80
        aligned = value.quantize(Decimal(1).scaleb(-precision))
        if aligned != value:
            raise ValueError(f"unrepresentable price {text} at instrument precision {precision}")
        if increment is not None and value % increment.as_decimal() != 0:
            raise ValueError(f"price {text} is not aligned to tick {increment}")
    price = Price.from_str(format(aligned, "f"))
    if price.as_decimal() != value:
        raise ValueError(f"unrepresentable price {text} at instrument precision {precision}")
    return price


class ShimConfig(StrategyConfig):
    def __init__(self, rows: list[dict], ids: list[InstrumentId], venue: Venue,
                 currency: Currency) -> None:
        super().__init__()
        self.rows = rows
        self.ids = ids
        self.venue = venue
        self.currency = currency


class Shim(Strategy):
    def __init__(self, config: ShimConfig) -> None:
        super().__init__(config)
        self.rows = config.rows
        self.ids = config.ids
        self.venue = config.venue
        self.currency = config.currency
        self.orders: dict[str, dict] = {}
        self.failure: str | None = None
        self.open_rows = {r["open_ns"]: r for r in self.rows}
        self.close_rows = {r["close_ns"]: r for r in self.rows}
        self.current_session = ""

    def emit(self, message: dict) -> None:
        send(sys.stdout, message)

    def on_start(self) -> None:
        for instrument_id in self.ids:
            self.subscribe_quotes(instrument_id)
            self.subscribe_instrument_status(instrument_id)

    def on_reset(self) -> None:
        self.orders.clear()
        self.current_session = ""

    def on_quote(self, tick: QuoteTick) -> None:
        # The final quote schedules a barrier after every instrument is updated.
        if tick.instrument_id != self.ids[-1]:
            return
        if tick.ts_event in self.open_rows:
            row = self.open_rows[tick.ts_event]
            kind = "open"
        else:
            row = self.close_rows[tick.ts_event]
            kind = "close"
        when = datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(
            microseconds=tick.ts_event // 1000 + 1000,
        )
        self.clock.set_time_alert(f"{kind}:{row['session_id']}", when)

    def on_time_event(self, event) -> None:
        if self.failure is not None:
            return
        try:
            self.handle_barrier(event)
        except (EOFError, ValueError, KeyError, TypeError) as error:
            # Nautilus logs callback exceptions but may let run() return normally.
            self.failure = f"{type(error).__name__}: {error}"
            self.emit({"type": "error", "reason": self.failure})
            raise

    def handle_barrier(self, event) -> None:
        kind, session = event.name.split(":")
        self.current_session = session
        if kind == "close":
            account = self.cache.account_for_venue(self.venue)
            positions = {
                str(p.instrument_id): encode(p.quantity.as_decimal())
                for p in self.cache.positions_open()
            }
            self.emit({
                "type": "session_close", "session_id": session,
                "backend_cash": encode(account.balance_total(self.currency).as_decimal()),
                "backend_positions": positions, "ts_event": str(event.ts_event),
            })
            return
        self.emit({"type": "session_open", "session_id": session, "ts_event": str(event.ts_event)})
        message = receive(sys.stdin, "orders")
        if message["session_id"] != session:
            raise ValueError("orders Session does not match the open barrier")
        for order in message["orders"]:
            key = order["order_id"]
            if key in self.orders:
                raise ValueError(f"duplicate order_id: {key}")
            instrument_id = InstrumentId.from_str(order["instrument_id"])
            if instrument_id not in self.ids:
                raise ValueError(f"unknown instrument: {instrument_id}")
            if not order["virtual_portfolio_id"]:
                raise ValueError("VirtualPortfolioId is required")
            if order["side"] not in {"BUY", "SELL"}:
                raise ValueError(f"unknown order side: {order['side']}")
            value = decimal_value(order["quantity"])
            if value <= 0 or value != value.to_integral_value():
                raise ValueError("Equity spike requires positive integral quantities")
            quantity = Quantity.from_str(order["quantity"])
            if quantity.as_decimal() != value:
                raise ValueError("Backend quantity conversion lost precision")
            native = self.order_factory.market(
                instrument_id=instrument_id,
                order_side=OrderSide.BUY if order["side"] == "BUY" else OrderSide.SELL,
                quantity=quantity, client_order_id=ClientOrderId(key),
            )
            self.orders[key] = order
            self.submit_order(native)

    def on_order_filled(self, event) -> None:
        order = self.orders[str(event.client_order_id)]
        self.emit({
            "type": "fill", "session_id": self.current_session, **order,
            "quantity": encode(event.last_qty.as_decimal()),
            "price": encode(event.last_px.as_decimal()),
            "backend_fee": encode(event.commission.as_decimal()),
            "ts_event": str(event.ts_event),
        })

    def on_order_rejected(self, event) -> None:
        self.emit({
            "type": "reject", "session_id": self.current_session,
            **self.orders[str(event.client_order_id)], "reason": event.reason,
            "ts_event": str(event.ts_event),
        })

    def on_order_denied(self, event) -> None:
        self.emit({
            "type": "reject", "session_id": self.current_session,
            **self.orders[str(event.client_order_id)], "reason": event.reason,
            "native_event": "OrderDenied", "ts_event": str(event.ts_event),
        })

    def on_instrument_status(self, status: InstrumentStatus) -> None:
        self.emit({
            "type": "backend_status", "instrument_id": str(status.instrument_id),
            "action": status.action.name, "ts_event": str(status.ts_event),
        })


def build_engine(packet: Path, initial_cash: str) -> tuple[BacktestEngine, Shim]:
    version = importlib.metadata.version("nautilus_trader")
    if version != PINNED_VERSION:
        raise RuntimeError(f"requires {PINNED_VERSION}, installed {version}")
    specs, rows, buy, sell, probe = read_packet(packet)
    engine = BacktestEngine(BacktestEngineConfig(
        logging=LoggerConfig(stdout_level=LogLevel.ERROR),
    ))
    venue, currency = Venue("SIM"), Currency.from_str("USD")
    if decimal_value(initial_cash) <= 0:
        raise ValueError("initial_cash must be positive")
    engine.add_venue(
        venue=venue, oms_type=OmsType.NETTING, account_type=AccountType.CASH,
        starting_balances=[Money.from_str(f"{initial_cash} USD")], base_currency=currency,
        fee_model=FixedFeeModel(Money.from_str("0.00 USD")),
    )
    ids = []
    precisions = {}
    increments = {}
    for spec in specs:
        instrument_id = InstrumentId.from_str(spec["instrument_id"])
        ids.append(instrument_id)
        precision = int(spec["price_precision"])
        precisions[str(instrument_id)] = precision
        increments[str(instrument_id)] = exact_price(spec["price_increment"], precision)
        engine.add_instrument(Equity(
            instrument_id=instrument_id, raw_symbol=Symbol(instrument_id.symbol.value),
            currency=currency, price_precision=precision,
            price_increment=increments[str(instrument_id)],
            lot_size=Quantity.from_str(spec["lot_size"]), ts_event=0, ts_init=0,
        ))
    data = []
    for row in rows:
        instrument_id = InstrumentId.from_str(row["instrument_id"])
        precision = precisions[str(instrument_id)]
        bid, ask = synthetic_prices(row["open"], buy, sell)
        for ts, bid_text, ask_text in (
            (row["open_ns"], bid, ask), (row["close_ns"], row["close"], row["close"]),
        ):
            data.append(QuoteTick(
                instrument_id,
                exact_price(bid_text, precision, increments[str(instrument_id)]),
                exact_price(ask_text, precision, increments[str(instrument_id)]),
                Quantity.from_int(1_000_000), Quantity.from_int(1_000_000), ts, ts,
            ))
        action = (MarketStatusAction.HALT if row["execution_status"] == "HALT"
                  else MarketStatusAction.TRADING)
        status_ts = row["open_ns"] + 500_000
        data.append(InstrumentStatus(instrument_id, action, status_ts, status_ts))
    engine.add_data(sorted(data, key=lambda item: item.ts_event))
    shim = Shim(ShimConfig(rows=rows, ids=ids, venue=venue, currency=currency))
    shim.decimal_probe = probe
    engine.add_strategy(shim)
    return engine, shim


def main() -> None:
    request = receive(sys.stdin, "start")
    if request["protocol"] != VERSION:
        raise ValueError("unsupported JSON-lines protocol")
    engine, shim = build_engine(Path(request["packet"]), request["initial_cash"])
    try:
        send(sys.stdout, {
            "type": "ready", "protocol": VERSION,
            "backend": {"name": "nautilus_trader", "version": PINNED_VERSION},
            "decimal_probe": {
                "packet": shim.decimal_probe,
                "stdio": {
                    key: encode(decimal_value(value))
                    for key, value in request["decimal_probe"].items()
                },
            },
        })
        engine.run(end=max(shim.close_rows) + 2_000_000)
        if shim.failure is not None:
            raise RuntimeError(shim.failure)
        if request["reset_probe"]:
            engine.reset()
            send(sys.stdout, {"type": "reset"})
            engine.run(end=max(shim.close_rows) + 2_000_000)
            if shim.failure is not None:
                raise RuntimeError(shim.failure)
        send(sys.stdout, {"type": "done"})
    finally:
        engine.dispose()


if __name__ == "__main__":
    main()
