//! Explicit holding benchmarks reuse the account ledger and raw execution prices.
use crate::*;
use prajna_research::strategy::Decision;

pub const SEMANTIC_VERSION: &str = "holding-benchmark@1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub semantic_version: String,
    pub kind: Kind,
    pub reinvestment_calendar: Calendar,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    SingleEtf,
    EqualWeightPool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calendar {
    pub semantic_version: String,
    /// Normal decision closes; execution is the following Session open.
    pub decision_dates: Vec<NaiveDate>,
}
#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub binding: Binding,
    pub identity: Value,
    pub price_ticks: BTreeMap<InstrumentId, prajna_domain::Price>,
}
impl Context {
    pub fn load(
        input: &DefinitionInput,
        root: &Path,
        evaluation: Option<&Evaluation>,
    ) -> Result<Option<Self>, ExperimentError> {
        let Some(binding) = &input.holding_benchmark else {
            return Ok(None);
        };
        let evaluation =
            evaluation.ok_or_else(|| invalid("holding benchmark requires evaluation"))?;
        if input.strategy != "buy_and_hold"
            || input.corporate_actions.is_none()
            || binding.semantic_version != SEMANTIC_VERSION
            || binding.reinvestment_calendar.semantic_version != "session-close-calendar@1"
        {
            return Err(invalid(
                "holding benchmark requires buy_and_hold, corporate_actions and supported explicit versions",
            ));
        }
        let universe = StaticUniverse::load(root, &input.universe)?;
        if matches!(binding.kind, Kind::SingleEtf) && universe.members().len() != 1 {
            return Err(invalid(
                "single_etf holding benchmark requires exactly one ETF",
            ));
        }
        let preview = prajna_data::dataset_preview::show(root, &input.dsv)
            .map_err(|e| invalid(e.to_string()))?;
        let mut rules = Vec::new();
        let mut price_ticks = BTreeMap::new();
        for member in universe.members() {
            let row = preview["instruments"]
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|r| r["instrument_id"] == member.to_string())
                })
                .ok_or_else(|| invalid("holding benchmark member missing"))?;
            if row["kind"] != "etf" {
                return Err(invalid("holding benchmarks require ETF members"));
            }
            let tick = row["price_increment"]
                .as_str()
                .ok_or_else(|| invalid("missing price increment"))?
                .parse()
                .map_err(|e| invalid(format!("price increment: {e}")))?;
            price_ticks.insert(member.clone(), tick);
            rules.push(serde_json::json!({"instrument_id":member,"price_increment":row["price_increment"],"size_increment":row["size_increment"],"lot_size":row["lot_size"]}));
        }
        let venues = parse_venues(input)?.into_iter().collect::<Vec<_>>();
        let panel =
            load_panel_for_venues(root, &input.dsv, &venues).map_err(|e| invalid(e.to_string()))?;
        if [evaluation.start(), evaluation.end()]
            .iter()
            .any(|date| !panel.sessions.iter().any(|s| s.session_date == *date))
        {
            return Err(invalid(
                "holding evaluation boundaries must be actual Sessions",
            ));
        }
        let dates = &binding.reinvestment_calendar.decision_dates;
        if dates.windows(2).any(|pair| pair[0] >= pair[1])
            || dates.iter().any(|date| {
                *date <= evaluation.start()
                    || *date > evaluation.end()
                    || !panel.sessions.iter().any(|s| s.session_date == *date)
            })
        {
            return Err(invalid(
                "reinvestment dates must be increasing Session closes after entry decision and within evaluation",
            ));
        }
        let identity = serde_json::json!({"definition":binding,"execution_rules":rules,
            "allocation":"equal_cash_slices_then_instrument_order_fees@1",
            "untradable":"skip_until_next_declared_decision", "return_basis":"raw_prices_plus_net_cash_actions@1"});
        Ok(Some(Self {
            binding: binding.clone(),
            identity,
            price_ticks,
        }))
    }
    pub fn decisions(&self, panel: &Panel, members: &[InstrumentId]) -> Vec<Decision> {
        panel
            .sessions
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                *i == 0
                    || self
                        .binding
                        .reinvestment_calendar
                        .decision_dates
                        .contains(&s.session_date)
            })
            .map(|(_, session)| Decision {
                decision_session: session.clone(),
                ranked: vec![],
                targets: members
                    .iter()
                    .map(|id| (id.clone(), 1.0 / members.len() as f64))
                    .collect(),
            })
            .collect()
    }
}
