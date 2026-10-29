//! Frozen common evaluation window and maximum warm-up planning.
//!
//! The plan is derived from the bound adjusted research series' actual Session
//! coverage and the declared research window of every parameter combination. It
//! never deletes Sessions, fills missing prices, or substitutes raw prices for
//! research signals. Raw Bars remain the execution and valuation authority.

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::{Value, json};

/// Version of the identity-bearing `evaluation` definition object.
pub const EVALUATION_SEMANTIC_VERSION: &str = "common-window@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocking {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemberWindow {
    pub instrument: String,
    pub first: Option<String>,
    pub last: Option<String>,
    pub research_sessions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParameterWindow {
    pub label: String,
    pub warmup_sessions: u64,
    pub runnable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Serialized precheck of the common data window, maximum warm-up and the shared
/// evaluation window. Dates are canonical `YYYY-MM-DD` strings; no Session is
/// dropped from the common window when a member's adjusted series has a gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowPlan {
    pub semantic_version: &'static str,
    pub members: Vec<MemberWindow>,
    pub parameters: Vec<ParameterWindow>,
    pub warmup_sessions: u64,
    pub warmup_sessions_used: u64,
    pub common_start: Option<String>,
    pub common_end: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub runnable: bool,
    pub blocking: Vec<Blocking>,
    pub warnings: Vec<String>,
}

impl WindowPlan {
    /// The identity-bearing evaluation object; only a runnable plan has one.
    pub fn evaluation(&self) -> Option<Value> {
        let (start, end) = (self.start.as_ref()?, self.end.as_ref()?);
        let (common_start, common_end) = (self.common_start.as_ref()?, self.common_end.as_ref()?);
        Some(json!({
            "semantic_version": self.semantic_version,
            "warmup_sessions": self.warmup_sessions,
            "common_start": common_start,
            "common_end": common_end,
            "start": start,
            "end": end,
        }))
    }

    /// Human-readable blocker for a rejected freeze submission.
    pub fn block_message(&self) -> String {
        self.blocking
            .iter()
            .map(|blocking| format!("{}: {}", blocking.code, blocking.message))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// One participating security's research coverage on the shared Session axis.
#[derive(Debug, Clone)]
pub struct Coverage {
    pub instrument: String,
    /// `present[index]` is true when the bound adjusted series has a row for the
    /// Session at that index. Length must equal the Session axis length.
    pub present: Vec<bool>,
}

/// One comparison parameter combination's declared research warm-up.
#[derive(Debug, Clone)]
pub struct Parameter {
    pub label: String,
    /// Sessions strictly before the first evaluable Session; `0` needs only the
    /// current Session (for example an entry-only Buy & Hold).
    pub warmup_sessions: u64,
}

/// Plans the shared research window over the frozen Session axis and coverage.
///
/// The evaluable Session of a parameter is the first Session whose trailing
/// `warmup_sessions + 1` Sessions all have bound adjusted rows for every member.
/// Gaps therefore move the evaluation start later without deleting Sessions.
/// The batch starts at the latest parameter start and ends at the earliest
/// member's last research Session.
pub fn plan(
    sessions: &[NaiveDate],
    coverage: &[Coverage],
    parameters: &[Parameter],
) -> Result<WindowPlan, String> {
    if sessions.is_empty() {
        return Err("common window requires a nonempty Session axis".into());
    }
    if coverage.is_empty() {
        return Err("common window requires at least one participating security".into());
    }
    for member in coverage {
        if member.present.len() != sessions.len() {
            return Err(format!(
                "research coverage for {} has {} entries for {} Sessions",
                member.instrument,
                member.present.len(),
                sessions.len()
            ));
        }
    }
    if parameters.is_empty() {
        return Err("common window requires at least one parameter combination".into());
    }
    let warmup_sessions = parameters
        .iter()
        .map(|parameter| parameter.warmup_sessions)
        .max()
        .unwrap_or(0);

    let mut blocking = Vec::new();
    let mut members = Vec::new();
    let mut first_indices = Vec::new();
    let mut last_indices = Vec::new();
    for member in coverage {
        let first = member.present.iter().position(|present| *present);
        let last = member.present.iter().rposition(|present| *present);
        members.push(MemberWindow {
            instrument: member.instrument.clone(),
            first: first.map(|index| sessions[index].to_string()),
            last: last.map(|index| sessions[index].to_string()),
            research_sessions: member.present.iter().filter(|present| **present).count(),
        });
        match (first, last) {
            (Some(first), Some(last)) => {
                first_indices.push(first);
                last_indices.push(last);
            }
            _ => blocking.push(Blocking {
                code: "member_without_research_coverage",
                message: format!(
                    "{} has no bound adjusted research rows and cannot join a common window",
                    member.instrument
                ),
            }),
        }
    }
    if !blocking.is_empty() {
        return Ok(WindowPlan {
            semantic_version: EVALUATION_SEMANTIC_VERSION,
            members,
            parameters: parameters
                .iter()
                .map(|parameter| ParameterWindow {
                    label: parameter.label.clone(),
                    warmup_sessions: parameter.warmup_sessions,
                    runnable: false,
                    reason: Some("no common research coverage".into()),
                })
                .collect(),
            warmup_sessions,
            warmup_sessions_used: 0,
            common_start: None,
            common_end: None,
            start: None,
            end: None,
            runnable: false,
            blocking,
            warnings: Vec::new(),
        });
    }

    let common_start = *first_indices.iter().max().expect("nonempty first indices");
    let common_end = *last_indices.iter().min().expect("nonempty last indices");
    let common_start_date = sessions[common_start].to_string();
    let common_end_date = sessions[common_end].to_string();
    if common_start > common_end {
        return Ok(WindowPlan {
            semantic_version: EVALUATION_SEMANTIC_VERSION,
            members,
            parameters: parameters
                .iter()
                .map(|parameter| ParameterWindow {
                    label: parameter.label.clone(),
                    warmup_sessions: parameter.warmup_sessions,
                    runnable: false,
                    reason: Some("research coverage does not overlap".into()),
                })
                .collect(),
            warmup_sessions,
            warmup_sessions_used: 0,
            common_start: Some(common_start_date),
            common_end: Some(common_end_date),
            start: None,
            end: None,
            runnable: false,
            blocking: vec![Blocking {
                code: "no_common_window",
                message:
                    "the participating securities have no overlapping adjusted research Sessions"
                        .into(),
            }],
            warnings: Vec::new(),
        });
    }

    let mut parameter_windows = Vec::new();
    let mut first_ready = Vec::new();
    for parameter in parameters {
        let required = usize::try_from(parameter.warmup_sessions)
            .ok()
            .and_then(|warmup| warmup.checked_add(1))
            .ok_or_else(|| format!("parameter {} warm-up is too large", parameter.label))?;
        let mut ready = None;
        for index in common_start..=common_end {
            if index + 1 < required {
                continue;
            }
            let window_start = index + 1 - required;
            let complete = (window_start..=index).all(|row| {
                coverage
                    .iter()
                    .all(|member| member.present.get(row).copied().unwrap_or(false))
            });
            if complete {
                ready = Some(index);
                break;
            }
        }
        match ready {
            Some(index) => {
                first_ready.push(index);
                parameter_windows.push(ParameterWindow {
                    label: parameter.label.clone(),
                    warmup_sessions: parameter.warmup_sessions,
                    runnable: true,
                    reason: None,
                });
            }
            None => {
                let reason = format!(
                    "{} requires {} warm-up Session(s) with complete adjusted research coverage; the common window cannot supply them",
                    parameter.label, parameter.warmup_sessions
                );
                blocking.push(Blocking {
                    code: "insufficient_warmup",
                    message: reason.clone(),
                });
                parameter_windows.push(ParameterWindow {
                    label: parameter.label.clone(),
                    warmup_sessions: parameter.warmup_sessions,
                    runnable: false,
                    reason: Some(reason),
                });
            }
        }
    }

    let Some(start) = first_ready.iter().copied().max() else {
        return Ok(WindowPlan {
            semantic_version: EVALUATION_SEMANTIC_VERSION,
            members,
            parameters: parameter_windows,
            warmup_sessions,
            warmup_sessions_used: 0,
            common_start: Some(common_start_date),
            common_end: Some(common_end_date),
            start: None,
            end: None,
            runnable: false,
            blocking,
            warnings: Vec::new(),
        });
    };
    if blocking.is_empty() && start >= common_end {
        blocking.push(Blocking {
            code: "evaluation_window_too_short",
            message:
                "the evaluation window must contain at least two Sessions after the maximum warm-up"
                    .into(),
        });
    }

    let mut warnings = Vec::new();
    if blocking.is_empty() {
        for (member, coverage) in coverage.iter().zip(&members) {
            let gaps = (start..=common_end)
                .filter(|index| !member.present[*index])
                .count();
            if gaps > 0 {
                warnings.push(format!(
                    "{}: {} evaluation-window Session(s) have no bound adjusted research row; Sessions are preserved, not filled with zero or raw prices",
                    coverage.instrument, gaps
                ));
            }
        }
    }
    Ok(WindowPlan {
        semantic_version: EVALUATION_SEMANTIC_VERSION,
        members,
        parameters: parameter_windows,
        warmup_sessions,
        warmup_sessions_used: u64::try_from(start - common_start).unwrap_or(0),
        common_start: Some(common_start_date),
        common_end: Some(common_end_date),
        start: blocking.is_empty().then(|| sessions[start].to_string()),
        end: blocking
            .is_empty()
            .then(|| sessions[common_end].to_string()),
        runnable: blocking.is_empty(),
        blocking,
        warnings,
    })
}

/// Declared research warm-up of one native parameter combination.
///
/// `warmup_sessions` counts Sessions strictly before the first evaluable
/// Session. A strategy that requires `L` trailing observations has
/// `warmup_sessions = L - 1`. Strategies without a declared native research
/// window are rejected instead of being silently planned with zero warm-up.
pub fn parameter_warmup(kind: &str, parameters: &Value) -> Result<u64, String> {
    fn positive(value: Option<&Value>, name: &str) -> Result<u64, String> {
        match value.and_then(Value::as_u64) {
            Some(value) if value > 0 => Ok(value),
            _ => Err(format!("{name} must be a positive integer")),
        }
    }
    let window = |name: &str, value: u64| {
        value
            .checked_sub(1)
            .ok_or_else(|| format!("{name} window must be at least one Session"))
    };
    match kind {
        "buy_and_hold" => Ok(0),
        "ma_crossover" => window(
            "ma_crossover long_window",
            positive(parameters.get("long_window"), "long_window")?,
        ),
        "s2_rotation" => {
            let long = positive(parameters.get("long"), "long")?;
            let vol = positive(parameters.get("vol"), "vol")?;
            let trend = parameters.get("trend").and_then(Value::as_u64).unwrap_or(0);
            window("s2_rotation", long.max(vol).max(trend))
        }
        _ => Err(format!(
            "strategy {kind} does not declare a native research warm-up window"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dates(values: &[&str]) -> Vec<NaiveDate> {
        values
            .iter()
            .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap())
            .collect()
    }

    fn coverage(instrument: &str, present: &[bool]) -> Coverage {
        Coverage {
            instrument: instrument.into(),
            present: present.to_vec(),
        }
    }

    fn parameter(label: &str, warmup_sessions: u64) -> Parameter {
        Parameter {
            label: label.into(),
            warmup_sessions,
        }
    }

    #[test]
    fn entry_only_plan_uses_the_whole_common_coverage_without_gap_compression() {
        let sessions = dates(&["2026-01-05", "2026-01-06", "2026-01-07", "2026-01-08"]);
        // A missing adjusted row in the middle is preserved: the plan does not
        // shorten the calendar to hide it.
        let plan = plan(
            &sessions,
            &[
                coverage("510300.XSHG", &[true, true, true, true]),
                coverage("510880.XSHG", &[true, false, true, true]),
            ],
            &[parameter("buy_and_hold", 0)],
        )
        .unwrap();
        assert!(plan.runnable);
        assert_eq!(plan.common_start.as_deref(), Some("2026-01-05"));
        assert_eq!(plan.common_end.as_deref(), Some("2026-01-08"));
        assert_eq!(plan.start.as_deref(), Some("2026-01-05"));
        assert_eq!(plan.end.as_deref(), Some("2026-01-08"));
        assert_eq!(plan.warmup_sessions, 0);
        assert_eq!(plan.warmup_sessions_used, 0);
        assert!(plan.blocking.is_empty());
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("510880.XSHG"));
    }

    #[test]
    fn common_window_is_the_member_intersection_not_the_union() {
        let sessions = dates(&["2026-01-05", "2026-01-06", "2026-01-07", "2026-01-08"]);
        let plan = plan(
            &sessions,
            &[
                coverage("EARLY.XSHG", &[true, true, true, true]),
                coverage("LATE.XSHG", &[false, true, true, true]),
                coverage("SHORT.XSHG", &[true, true, true, false]),
            ],
            &[parameter("buy_and_hold", 0)],
        )
        .unwrap();
        assert!(plan.runnable, "{plan:?}");
        assert_eq!(plan.common_start.as_deref(), Some("2026-01-06"));
        assert_eq!(plan.common_end.as_deref(), Some("2026-01-07"));
        assert_eq!(plan.start.as_deref(), Some("2026-01-06"));
        assert_eq!(plan.end.as_deref(), Some("2026-01-07"));
    }

    #[test]
    fn warmup_moves_the_evaluation_start_and_keeps_prior_sessions() {
        let sessions = dates(&[
            "2026-01-05",
            "2026-01-06",
            "2026-01-07",
            "2026-01-08",
            "2026-01-09",
        ]);
        let plan = plan(
            &sessions,
            &[coverage("510300.XSHG", &[true; 5])],
            &[parameter("ma_crossover", 2)],
        )
        .unwrap();
        assert!(plan.runnable);
        assert_eq!(plan.common_start.as_deref(), Some("2026-01-05"));
        // Two warm-up Sessions are required, so the first evaluable Session is
        // 2026-01-07; earlier Sessions remain in the common data window.
        assert_eq!(plan.start.as_deref(), Some("2026-01-07"));
        assert_eq!(plan.end.as_deref(), Some("2026-01-09"));
        assert_eq!(plan.warmup_sessions, 2);
        assert_eq!(plan.warmup_sessions_used, 2);
    }

    #[test]
    fn a_gap_inside_the_warmup_window_delays_the_evaluation_start() {
        let sessions = dates(&[
            "2026-01-05",
            "2026-01-06",
            "2026-01-07",
            "2026-01-08",
            "2026-01-09",
            "2026-01-10",
        ]);
        // The row for 2026-01-06 is missing, so a 2-Session warm-up is only
        // complete at 2026-01-09 (window 01-07..01-09); no Session is removed.
        let plan = plan(
            &sessions,
            &[coverage(
                "510300.XSHG",
                &[true, false, true, true, true, true],
            )],
            &[parameter("ma_crossover", 2)],
        )
        .unwrap();
        assert!(plan.runnable, "{plan:?}");
        assert_eq!(plan.common_start.as_deref(), Some("2026-01-05"));
        assert_eq!(plan.start.as_deref(), Some("2026-01-09"));
        assert_eq!(plan.end.as_deref(), Some("2026-01-10"));
        assert_eq!(plan.warmup_sessions, 2);
        assert_eq!(plan.warmup_sessions_used, 4);
        assert!(plan.blocking.is_empty());
    }

    #[test]
    fn a_warmup_that_leaves_one_evaluation_session_is_not_runnable() {
        let sessions = dates(&[
            "2026-01-05",
            "2026-01-06",
            "2026-01-07",
            "2026-01-08",
            "2026-01-09",
        ]);
        let plan = plan(
            &sessions,
            &[coverage("510300.XSHG", &[true, false, true, true, true])],
            &[parameter("ma_crossover", 2)],
        )
        .unwrap();
        assert!(!plan.runnable);
        assert!(
            plan.blocking
                .iter()
                .any(|blocking| { blocking.code == "evaluation_window_too_short" })
        );
        assert_eq!(plan.start, None);
        assert_eq!(plan.end, None);
    }

    #[test]
    fn maximum_warmup_and_per_parameter_readiness_are_reported() {
        let sessions = dates(&["2026-01-05", "2026-01-06", "2026-01-07", "2026-01-08"]);
        let plan = plan(
            &sessions,
            &[coverage("510300.XSHG", &[true; 4])],
            &[
                parameter("buy_and_hold", 0),
                parameter("ma_crossover", 2),
                parameter("too_long", 9),
            ],
        )
        .unwrap();
        assert!(!plan.runnable);
        assert_eq!(plan.warmup_sessions, 9);
        assert_eq!(plan.parameters.len(), 3);
        assert!(plan.parameters[0].runnable);
        assert!(plan.parameters[1].runnable);
        assert!(!plan.parameters[2].runnable);
        assert!(plan.blocking.iter().any(|blocking| {
            blocking.code == "insufficient_warmup" && blocking.message.contains("too_long")
        }));
        assert_eq!(plan.start, None);
        assert_eq!(plan.end, None);
    }

    #[test]
    fn a_single_evaluation_session_is_not_runnable() {
        let sessions = dates(&["2026-01-05", "2026-01-06"]);
        let plan = plan(
            &sessions,
            &[coverage("510300.XSHG", &[true, true])],
            &[parameter("ma_crossover", 1)],
        )
        .unwrap();
        assert!(!plan.runnable);
        assert!(
            plan.blocking
                .iter()
                .any(|blocking| { blocking.code == "evaluation_window_too_short" })
        );
    }

    #[test]
    fn empty_coverage_and_nonoverlapping_windows_are_explicit_blockers() {
        let sessions = dates(&["2026-01-05", "2026-01-06"]);
        let empty = plan(
            &sessions,
            &[coverage("510300.XSHG", &[false, false])],
            &[parameter("buy_and_hold", 0)],
        )
        .unwrap();
        assert!(!empty.runnable);
        assert!(
            empty
                .blocking
                .iter()
                .any(|blocking| { blocking.code == "member_without_research_coverage" })
        );
        let disjoint = plan(
            &sessions,
            &[
                coverage("A.XSHG", &[true, false]),
                coverage("B.XSHG", &[false, true]),
            ],
            &[parameter("buy_and_hold", 0)],
        )
        .unwrap();
        assert!(!disjoint.runnable);
        assert!(
            disjoint
                .blocking
                .iter()
                .any(|blocking| blocking.code == "no_common_window"),
            "{disjoint:?}"
        );
    }

    #[test]
    fn native_parameter_warmups_are_declared_and_unknown_strategies_are_rejected() {
        assert_eq!(
            parameter_warmup("buy_and_hold", &json!({"params_version":1})),
            Ok(0)
        );
        assert_eq!(
            parameter_warmup("ma_crossover", &json!({"long_window":60})),
            Ok(59)
        );
        assert_eq!(
            parameter_warmup("s2_rotation", &json!({"long":120,"vol":60,"trend":20})),
            Ok(119)
        );
        assert_eq!(
            parameter_warmup("s2_rotation", &json!({"long":20,"vol":60,"trend":null})),
            Ok(59)
        );
        assert!(parameter_warmup("research.rotation@1", &json!({})).is_err());
        assert!(parameter_warmup("ma_crossover", &json!({"long_window":0})).is_err());
    }

    #[test]
    fn evaluation_object_is_only_emitted_for_a_runnable_plan() {
        let sessions = dates(&["2026-01-05", "2026-01-06"]);
        let runnable = plan(
            &sessions,
            &[coverage("510300.XSHG", &[true, true])],
            &[parameter("buy_and_hold", 0)],
        )
        .unwrap();
        let evaluation = runnable.evaluation().expect("runnable plan");
        assert_eq!(evaluation["semantic_version"], EVALUATION_SEMANTIC_VERSION);
        assert_eq!(evaluation["start"], "2026-01-05");
        assert_eq!(evaluation["end"], "2026-01-06");
        let blocked = plan(
            &sessions,
            &[coverage("510300.XSHG", &[false, true])],
            &[parameter("ma_crossover", 2)],
        )
        .unwrap();
        assert!(blocked.evaluation().is_none());
        assert!(!blocked.block_message().is_empty());
    }
}
