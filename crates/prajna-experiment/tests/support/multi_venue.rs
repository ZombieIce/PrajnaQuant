//! Test-only synthetic multi-Venue Dataset/definition for the production Experiment
//! CLI regression; publishes through the real `publish_dataset` path. All synthetic.
use std::{fmt::Display, path::Path};

use chrono::NaiveDate;
use prajna_data::{
    ExecutionStatusRow, NormalizationIssue, NormalizationOutput, NormalizedTables, Normalizer,
    NormalizerRegistry, RawInput, RawStore, SourceKind, SourceRecordInput, bars_to_record_batch,
    execution_status_to_record_batch, instruments_to_record_batch, publish_dataset,
    sessions_to_record_batch,
};
use prajna_domain::{
    Bar, BarData, Currency, InstrumentId, InstrumentKind, InstrumentSpec, InstrumentSpecData,
    Price, Quantity, Session, TimestampNs, VenueId,
};
use prajna_research::StaticUniverse;
use serde_json::{Value, json};

const NORMALIZER_ID: &str = "synthetic-multi-venue";
const NORMALIZER_VERSION: &str = "1";
const DATES: [&str; 3] = ["2026-01-05", "2026-01-06", "2026-01-07"];
/// One synthetic ETF per Venue; both share the same synthetic open/close path.
const INSTRUMENTS: [(&str, &str); 2] = [("600519", "XSHG"), ("000001", "XSHE")];

struct MultiVenueNormalizer;

fn rejected(message: impl Display) -> Vec<NormalizationIssue> {
    vec![NormalizationIssue {
        code: "invalid_synthetic_multi_venue_fixture".into(),
        path: "$".into(),
        message: message.to_string(),
    }]
}

fn session(venue: &str, date: &str) -> Session {
    Session::new(
        VenueId::new(venue).expect("static Venue"),
        NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("static date"),
        TimestampNs::parse(&format!("{date}T09:30:00+08:00")).expect("static open"),
        TimestampNs::parse(&format!("{date}T15:00:00+08:00")).expect("static close"),
    )
    .expect("valid synthetic session")
}

impl Normalizer for MultiVenueNormalizer {
    fn id(&self) -> &str {
        NORMALIZER_ID
    }

    fn version(&self) -> &str {
        NORMALIZER_VERSION
    }

    fn config_sha256(&self) -> String {
        format!("sha256:{}", "5".repeat(64))
    }

    fn normalize(
        &self,
        inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        let fixture: Value = serde_json::from_slice(&inputs[0].bytes).map_err(rejected)?;
        let missing = fixture
            .get("missing_xshe_sessions")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut sessions = vec![];
        for venue in ["XSHG", "XSHE"] {
            for date in DATES {
                if venue == "XSHE" && missing.contains(&date) {
                    continue;
                }
                sessions.push(session(venue, date));
            }
        }
        let mut instruments = vec![];
        let mut bars = vec![];
        let mut statuses = vec![];
        for (symbol, venue) in INSTRUMENTS {
            let id = InstrumentId::new(symbol, VenueId::new(venue).expect("static Venue"))
                .expect("static Instrument");
            instruments.push(
                InstrumentSpec::new(InstrumentSpecData {
                    id: id.clone(),
                    kind: InstrumentKind::Etf,
                    native_symbol: symbol.to_owned(),
                    market_segment: None,
                    base_currency: None,
                    quote_currency: Some(Currency::new("CNY").expect("static currency")),
                    settle_currency: None,
                    is_inverse: false,
                    multiplier: None,
                    expiry: None,
                    price_increment: Price::parse("0.001").expect("static increment"),
                    size_increment: Quantity::parse("1").expect("static increment"),
                    lot_size: Some(Quantity::parse("100").expect("static lot")),
                    session_timezone: Some("Asia/Shanghai".into()),
                })
                .expect("valid synthetic instrument"),
            );
            for session in sessions
                .iter()
                .filter(|session| session.venue_id().as_str() == venue)
            {
                bars.push(
                    Bar::new(
                        BarData {
                            instrument_id: id.clone(),
                            bar_spec: "1d@session".parse().expect("static BarSpec"),
                            session_date: session.session_date(),
                            ts_open: session.ts_open(),
                            ts_close: session.ts_close(),
                            open: Price::parse("10").expect("static price"),
                            high: Price::parse("10.1").expect("static price"),
                            low: Price::parse("10").expect("static price"),
                            close: Price::parse("10.1").expect("static price"),
                            volume: Quantity::parse("1000").expect("static volume"),
                            amount: None,
                            available_at: None,
                        },
                        Some(session),
                    )
                    .expect("valid synthetic bar"),
                );
                statuses.push(ExecutionStatusRow {
                    instrument_id: id.clone(),
                    session_date: session.session_date(),
                    trade_status: "TRADABLE".into(),
                    is_tradable: true,
                    available_at: Some(session.ts_open()),
                    source: "synthetic".into(),
                });
            }
        }
        Ok(NormalizationOutput {
            tables: NormalizedTables {
                instruments: instruments_to_record_batch(&instruments).map_err(rejected)?,
                sessions: sessions_to_record_batch(&sessions).map_err(rejected)?,
                bars: bars_to_record_batch(&bars).map_err(rejected)?,
                execution_status: Some(
                    execution_status_to_record_batch(&statuses).map_err(rejected)?,
                ),
                source_crosscheck: None,
                announcements: None,
                corporate_actions: None,
                bar_sources: None,
                tdx_code_table: None,
            },
            coverage: json!({"fixture": "synthetic-multi-venue", "sessions": sessions.len()}),
            synthetic_assumptions: vec![
                "synthetic multi-Venue Experiment fixture; not real market data".into(),
            ],
            ignored_input_fields: vec![],
        })
    }
}

/// Publishes the synthetic Dataset, stores a Static Universe over both Venues and
/// writes the Fast Event definition used by the CLI/storage regression.
/// `missing_xshe_session` drops one XSHE day to exercise the structural failure.
pub fn prepare(lake: &Path, definition: &Path, missing_xshe_session: Option<&str>) {
    let store = RawStore::open(lake).expect("open synthetic lake");
    let raw = store
        .put(
            &serde_json::to_vec(&json!({
                "missing_xshe_sessions": missing_xshe_session.into_iter().collect::<Vec<_>>(),
            }))
            .expect("encode fixture"),
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: "synthetic-multi-venue".into(),
                request: json!({}),
                observed_at: "synthetic".into(),
                ingested_by: "prajna-experiment-tests".into(),
            },
        )
        .expect("store synthetic Raw");
    let mut registry = NormalizerRegistry::new();
    assert!(registry.register(MultiVenueNormalizer));
    let manifest = publish_dataset(
        lake,
        &store,
        &registry,
        NORMALIZER_ID,
        NORMALIZER_VERSION,
        &[raw],
        "2026-10-28T00:00:00Z",
    )
    .expect("publish synthetic multi-Venue Dataset");
    let members = INSTRUMENTS
        .map(|(symbol, venue)| {
            InstrumentId::new(symbol, VenueId::new(venue).expect("static Venue"))
                .expect("static Instrument")
        })
        .to_vec();
    let universe = StaticUniverse::new("all", members).expect("synthetic Universe");
    universe.store(lake).expect("store synthetic Universe");
    let value = json!({
        "experiment_version": 1,
        "dsv": manifest.dsv,
        "universe": universe.id().expect("Universe identity"),
        "venues": ["XSHG", "XSHE"],
        "strategy": "buy_and_hold",
        "engine": "fast_event",
        "availability_assumption": "none",
        "sessions_per_year": 252,
        "costs": {"commission_rate": 0.01, "buy_slippage_bps": 0.0,
            "sell_slippage_bps": 0.0, "buy_tax_rate": 0.0, "sell_tax_rate": 0.0},
        "parameter_space": {"list": [{"params_version": 1}]},
        "unfilled_entry": "retry",
        "fast_event": {"currency": "CNY", "initial_cash": "10000",
            "sizing": "vector_parity", "rebalance": "full_target",
            "minimum_commission": "0",
            "portfolios": [{"id": 1, "capital": "10000"}],
            "lot_size_source": {"kind": "instrument"}},
    });
    std::fs::write(
        definition,
        serde_json::to_vec_pretty(&value).expect("encode"),
    )
    .expect("write definition");
}
