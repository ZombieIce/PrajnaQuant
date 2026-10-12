use std::collections::{BTreeMap, BTreeSet};

use prajna_account::{Fill, Order, OrderId, Side};
use prajna_domain::{InstrumentId, Price};
use prajna_research::{
    ExecutionStatusMap, Panel, PanelSession,
    fast_event::{
        FastEventConfig, FastEventCosts, FastEventError, FastEventResult, MarketExecution,
        PortfolioRun, lot_execution_price, lot_fill_at_price, run_lot_with_execution,
    },
};

use crate::{
    AdapterError, RunPacket, invalid,
    protocol::{
        self, Request, Response, WireFill, WireOrder, WireQuote, WireReject, decode_line,
        encode_line,
    },
};

/// Implementations own transport/lifecycle only. Sizing and the ledger stay in Rust.
pub trait Backend {
    fn begin_run(&mut self, packet: &RunPacket, costs: FastEventCosts) -> Result<(), AdapterError>;
    fn exchange(&mut self, request: &str) -> Result<String, AdapterError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterEvent {
    SyntheticOpen {
        session_date: String,
        quotes: Vec<WireQuote>,
    },
    PlatformGate {
        session_date: String,
        instrument_id: InstrumentId,
    },
}

#[derive(Debug)]
pub struct AdapterResult {
    pub execution: FastEventResult,
    pub adapter_events: Vec<AdapterEvent>,
    /// Includes Backend fees for comparison; never used as ledger costs.
    pub backend_events: Vec<Response>,
}

struct Execution<'a> {
    backend: &'a mut dyn Backend,
    adapter_events: Vec<AdapterEvent>,
    backend_events: Vec<Response>,
}

impl Execution<'_> {
    fn exchange(&mut self, request: &Request) -> Result<Response, AdapterError> {
        decode_line(&self.backend.exchange(&encode_line(request)?)?).map_err(Into::into)
    }
}

fn execution_error(error: impl std::fmt::Display) -> FastEventError {
    FastEventError::Execution(error.to_string())
}
fn wire_side(side: Side) -> protocol::Side {
    match side {
        Side::Buy => protocol::Side::Buy,
        Side::Sell => protocol::Side::Sell,
    }
}
fn wire_order(order: &Order) -> WireOrder {
    WireOrder {
        order_id: order.id().0.to_string(),
        portfolio_id: order.portfolio_id().0.to_string(),
        instrument_id: order.instrument_id().clone(),
        side: wire_side(order.side()),
        quantity: order.quantity(),
        submitted_at: order.submitted_at().as_unix_nanos().to_string(),
    }
}

impl MarketExecution for Execution<'_> {
    fn open(
        &mut self,
        session: &PanelSession,
        prices: &BTreeMap<InstrumentId, Price>,
        gates: &BTreeMap<InstrumentId, bool>,
        costs: FastEventCosts,
    ) -> Result<(), FastEventError> {
        let day = session.session_date.to_string();
        let quotes = gates
            .iter()
            .filter_map(|(id, tradable)| {
                prices.get(id).map(|raw| {
                    Ok(WireQuote {
                        instrument_id: id.clone(),
                        raw_open: *raw,
                        bid: lot_execution_price(*raw, Side::Sell, costs)?,
                        ask: lot_execution_price(*raw, Side::Buy, costs)?,
                        tradable: *tradable,
                    })
                })
            })
            .collect::<Result<Vec<_>, FastEventError>>()?;
        self.adapter_events.push(AdapterEvent::SyntheticOpen {
            session_date: day.clone(),
            quotes: quotes.clone(),
        });
        for (id, tradable) in gates {
            if !tradable {
                self.adapter_events.push(AdapterEvent::PlatformGate {
                    session_date: day.clone(),
                    instrument_id: id.clone(),
                });
            }
        }
        let response = self
            .exchange(&Request::Open {
                session_date: day.clone(),
                ts_open: session.ts_open.as_unix_nanos().to_string(),
                quotes,
            })
            .map_err(execution_error)?;
        if response != (Response::Opened { session_date: day }) {
            return Err(execution_error("Backend did not acknowledge Session open"));
        }
        Ok(())
    }

    fn execute(
        &mut self,
        order: &Order,
        raw: Price,
        costs: FastEventCosts,
    ) -> Result<Option<Fill>, FastEventError> {
        let expected = wire_order(order);
        let response = self
            .exchange(&Request::Order(expected.clone()))
            .map_err(execution_error)?;
        let result = match &response {
            Response::Fill(fill)
                if fill.order_id == expected.order_id
                    && fill.portfolio_id == expected.portfolio_id
                    && fill.instrument_id == expected.instrument_id
                    && fill.side == expected.side
                    && fill.quantity == expected.quantity
                    && fill.executed_at == expected.submitted_at
                    && fill.fee >= prajna_domain::Notional::ZERO =>
            {
                Some(lot_fill_at_price(order.clone(), raw, fill.price, costs)?)
            }
            Response::Reject(reject)
                if reject.order_id == expected.order_id
                    && reject.portfolio_id == expected.portfolio_id
                    && reject.instrument_id == expected.instrument_id
                    && !reject.reason.trim().is_empty() =>
            {
                None
            }
            _ => {
                return Err(execution_error(
                    "unmapped, partial, stale or mismatched Backend response",
                ));
            }
        };
        self.backend_events.push(response);
        Ok(result)
    }

    fn close(&mut self, session: &PanelSession) -> Result<(), FastEventError> {
        let day = session.session_date.to_string();
        let response = self
            .exchange(&Request::Close {
                session_date: day.clone(),
                ts_close: session.ts_close.as_unix_nanos().to_string(),
            })
            .map_err(execution_error)?;
        if response != (Response::Closed { session_date: day }) {
            return Err(execution_error("Backend did not acknowledge Session close"));
        }
        Ok(())
    }
}

pub fn run_adapter(
    packet: &RunPacket,
    panel: &Panel,
    statuses: &ExecutionStatusMap,
    config: &FastEventConfig,
    portfolios: &[PortfolioRun],
    backend: &mut dyn Backend,
) -> Result<AdapterResult, AdapterError> {
    packet.validate()?;
    let ids = packet
        .instruments
        .iter()
        .map(|i| i.id().clone())
        .collect::<BTreeSet<_>>();
    if ids != panel.instruments.iter().cloned().collect()
        || packet
            .instruments
            .iter()
            .any(|i| config.lot_sizes.get(i.id()).copied() != i.lot_size())
        || packet
            .instruments
            .iter()
            .any(|i| i.quote_currency() != Some(&config.currency))
    {
        return Err(invalid(
            "Run packet Instrument/lot/currency specifications differ from execution inputs",
        ));
    }
    validate_inputs(packet, panel, statuses)?;
    backend.begin_run(packet, config.costs)?;
    let mut execution = Execution {
        backend,
        adapter_events: vec![],
        backend_events: vec![],
    };
    let result = run_lot_with_execution(panel, statuses, config, portfolios, &mut execution)?;
    if execution.exchange(&Request::EndRun)? != Response::Done {
        return Err(invalid("Backend did not acknowledge Run end"));
    }

    fn validate_inputs(
        packet: &RunPacket,
        panel: &Panel,
        statuses: &ExecutionStatusMap,
    ) -> Result<(), AdapterError> {
        let packet_sessions = packet
            .sessions
            .iter()
            .map(|s| (s.session_date(), s.ts_open(), s.ts_close()))
            .collect::<BTreeSet<_>>();
        let panel_sessions = panel
            .sessions
            .iter()
            .map(|s| (s.session_date, s.ts_open, s.ts_close))
            .collect::<BTreeSet<_>>();
        let packet_statuses = packet
            .statuses
            .iter()
            .map(|s| ((s.instrument_id.clone(), s.session_date), s.clone()))
            .collect::<ExecutionStatusMap>();
        if packet_sessions != panel_sessions
            || &packet_statuses != statuses
            || panel.grid.height() != panel.instruments.len() * panel.sessions.len()
        {
            return Err(invalid(
                "Run packet Sessions/statuses differ from execution inputs",
            ));
        }
        let bars = packet
            .bars
            .iter()
            .map(|b| ((b.instrument_id(), b.session_date()), b))
            .collect::<BTreeMap<_, _>>();
        let check = || -> Result<(), FastEventError> {
            let open = panel.grid.column("open_decimal")?.str()?;
            let close = panel.grid.column("close_decimal")?.str()?;
            let present = panel.grid.column("has_bar")?.bool()?;
            for (i, id) in panel.instruments.iter().enumerate() {
                for (j, session) in panel.sessions.iter().enumerate() {
                    let index = i * panel.sessions.len() + j;
                    let bar = bars.get(&(id, session.session_date));
                    if present.get(index) != Some(bar.is_some()) {
                        return Err(execution_error(
                            "Run packet bars differ from execution inputs",
                        ));
                    }
                    match bar {
                        Some(bar)
                            if open.get(index).map(Price::parse).transpose()?
                                == Some(bar.open())
                                && close.get(index).map(Price::parse).transpose()?
                                    == Some(bar.close()) => {}
                        None if open.get(index).is_none() && close.get(index).is_none() => {}
                        _ => {
                            return Err(execution_error(
                                "Run packet prices differ from execution inputs",
                            ));
                        }
                    }
                }
            }
            Ok(())
        };
        check().map_err(Into::into)
    }
    Ok(AdapterResult {
        execution: result,
        adapter_events: execution.adapter_events,
        backend_events: execution.backend_events,
    })
}

/// Deterministic full-market-fill fake. It cannot satisfy #133 A15.
#[derive(Default)]
pub struct FakeBackend {
    pub reject_orders: BTreeSet<OrderId>,
    costs: Option<FastEventCosts>,
    session: Option<(String, String)>,
    quotes: BTreeMap<InstrumentId, WireQuote>,
    submitted: BTreeSet<String>,
}

impl Backend for FakeBackend {
    fn begin_run(&mut self, packet: &RunPacket, costs: FastEventCosts) -> Result<(), AdapterError> {
        packet.validate()?;
        self.costs = Some(costs);
        self.session = None;
        self.quotes.clear();
        self.submitted.clear();
        Ok(())
    }

    fn exchange(&mut self, request: &str) -> Result<String, AdapterError> {
        if self.costs.is_none() {
            return Err(invalid("fake Backend Run not initialized"));
        }
        let response = match decode_line::<Request>(request)? {
            Request::Open {
                session_date,
                ts_open,
                quotes,
            } => {
                if self.session.is_some() {
                    return Err(invalid("Session already open"));
                }
                self.quotes = quotes
                    .into_iter()
                    .map(|q| (q.instrument_id.clone(), q))
                    .collect();
                self.session = Some((session_date.clone(), ts_open));
                Response::Opened { session_date }
            }
            Request::Order(order) => {
                let (_, ts_open) = self
                    .session
                    .as_ref()
                    .ok_or_else(|| invalid("Order outside open Session"))?;
                if &order.submitted_at != ts_open || !self.submitted.insert(order.order_id.clone())
                {
                    return Err(invalid("stale or duplicate Order"));
                }
                let quote = self
                    .quotes
                    .get(&order.instrument_id)
                    .ok_or_else(|| invalid("unknown quote"))?;
                let id = order
                    .order_id
                    .parse::<u64>()
                    .map_err(|e| invalid(e.to_string()))?;
                if self.reject_orders.contains(&OrderId(id)) || !quote.tradable {
                    Response::Reject(WireReject {
                        order_id: order.order_id,
                        portfolio_id: order.portfolio_id,
                        instrument_id: order.instrument_id,
                        reason: "configured rejection or non-tradable".into(),
                    })
                } else {
                    Response::Fill(WireFill {
                        order_id: order.order_id,
                        portfolio_id: order.portfolio_id,
                        instrument_id: order.instrument_id,
                        side: order.side,
                        quantity: order.quantity,
                        price: if order.side == protocol::Side::Buy {
                            quote.ask
                        } else {
                            quote.bid
                        },
                        fee: prajna_domain::Notional::ZERO,
                        executed_at: order.submitted_at,
                    })
                }
            }
            Request::Close { session_date, .. } => {
                if self.session.as_ref().map(|s| &s.0) != Some(&session_date) {
                    return Err(invalid("Session close mismatch"));
                }
                self.session = None;
                Response::Closed { session_date }
            }
            Request::EndRun if self.session.is_none() => {
                self.costs = None;
                Response::Done
            }
            _ => return Err(invalid("unexpected fake Backend message")),
        };
        encode_line(&response).map_err(Into::into)
    }
}
