//! Decode compatibility JSON without changing the historical parser for native task identity.
use serde_json::{Number, Value, value::RawValue};
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct DecodeError {
    pub field: String,
    reason: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for DecodeError {}

pub fn decode(bytes: &[u8]) -> Result<Value, DecodeError> {
    decode_at(
        serde_json::from_slice::<&RawValue>(bytes).map_err(|error| DecodeError {
            field: "JSON".into(),
            reason: error.to_string(),
        })?,
        "",
        0,
    )
}

fn decode_at(raw: &RawValue, path: &str, depth: usize) -> Result<Value, DecodeError> {
    let invalid = |reason: String| DecodeError {
        field: if path.is_empty() { "JSON" } else { path }.into(),
        reason,
    };
    let token = raw.get();
    if depth >= 128 {
        return Err(invalid("JSON nesting limit exceeded".into()));
    }
    let syntax = |error: serde_json::Error| invalid(error.to_string());
    match token.as_bytes()[0] {
        b'{' => {
            let object: BTreeMap<String, &RawValue> =
                serde_json::from_str(token).map_err(syntax)?;
            let mut values = serde_json::Map::new();
            for (key, value) in object {
                let path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                values.insert(key, decode_at(value, &path, depth + 1)?);
            }
            Ok(Value::Object(values))
        }
        b'[' => {
            let array: Vec<&RawValue> = serde_json::from_str(token).map_err(syntax)?;
            array
                .into_iter()
                .enumerate()
                .map(|(index, item)| decode_at(item, &format!("{path}[{index}]"), depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        b'-' | b'0'..=b'9' => {
            let mantissa = token.split(['e', 'E']).next().expect("number token");
            if mantissa.len() == token.len() && !token.contains('.') && token != "-0" {
                if let Ok(number) = token.parse::<u64>() {
                    return Ok(Value::Number(number.into()));
                }
                if let Ok(number) = token.parse::<i64>() {
                    return Ok(Value::Number(number.into()));
                }
            }
            let number = token
                .parse::<f64>()
                .ok()
                .filter(|number| {
                    *number != 0.0 || !mantissa.bytes().any(|digit| (b'1'..=b'9').contains(&digit))
                })
                .and_then(Number::from_f64);
            number
                .map(Value::Number)
                .ok_or_else(|| invalid("number overflows or underflows binary64".into()))
        }
        _ => serde_json::from_str(token).map_err(syntax),
    }
}
