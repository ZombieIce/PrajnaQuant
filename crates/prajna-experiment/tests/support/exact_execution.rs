use std::collections::BTreeMap;

use prajna_research::fast_event::FastEventResult;
use serde_json::{Value, json};

pub fn exact_projection(result: &FastEventResult) -> Value {
    json!({
        "orders":result.account.orders(),
        "fills":result.account.fills(),
        "attempts":result.attempts.iter().map(|a| json!({
            "portfolio_id":a.portfolio_id,"event":format!("{:?}",a.event)
        })).collect::<Vec<_>>(),
        "sessions":result.sessions.iter().map(|s| json!({
            "session_date":s.session.session_date.to_string(),"open":s.open_after,"close":s.close,
            "open_sources":s.open_sources.iter().map(|(id,p)|(id.to_string(),format!("{p:?}"))).collect::<BTreeMap<_,_>>(),
            "close_sources":s.close_sources.iter().map(|(id,p)|(id.to_string(),format!("{p:?}"))).collect::<BTreeMap<_,_>>(),
        })).collect::<Vec<_>>()
    })
}
