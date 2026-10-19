//! Local-only comparison of MVP-6 public facts against the published v4
//! projection. Lifecycle boundaries and prior-close baselines are scoped to the
//! identity lifecycle that owns each date, so a reused fund code cannot compare
//! one lifecycle against another's Bars.

use super::Mvp6Facts;
use chrono::NaiveDate;
use prajna_domain::Price;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrosscheckMismatch {
    pub code: String,
    pub kind: String,
    pub expected_date: Option<NaiveDate>,
    pub observed_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrosscheckReport {
    pub mismatches: Vec<CrosscheckMismatch>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct V4DailyBar {
    pub code: String,
    pub date: NaiveDate,
    /// Traded close, or `None` for a keyed no-trade observation.
    pub close: Option<Price>,
    /// Exchange ex-rights reference from v4 `bar_sources.reference_price`.
    /// This is not the previous observed close.
    pub reference_price: Option<Price>,
    pub keyed_no_trade: bool,
}

/// Compare public facts with a synthetic or locally frozen v4 row set. The
/// function returns discrepancies only; it never rewrites either input.
///
/// Listing and termination are compared with the first and last *traded* Bar
/// inside the identity lifecycle that starts at `listing_date`. Action and
/// suspension checks are also scoped to the lifecycle containing their date,
/// and an action's baseline is the last traded close inside that same lifecycle.
pub fn crosscheck_v4(facts: &Mvp6Facts, bars: &[V4DailyBar]) -> CrosscheckReport {
    let mut mismatches = Vec::new();
    for identity in &facts.identities {
        let Ok(listing) = NaiveDate::parse_from_str(&identity.listing_date, "%Y-%m-%d") else {
            continue;
        };
        let termination = NaiveDate::parse_from_str(&identity.termination_date, "%Y-%m-%d").ok();
        let scoped = scoped_bars(bars, &identity.code, listing, termination);
        let mut traded = scoped
            .iter()
            .filter(|bar| bar.close.is_some())
            .map(|bar| bar.date)
            .collect::<Vec<_>>();
        traded.sort_unstable();
        let first = traded.first().copied();
        let last = traded.last().copied();
        if first != Some(listing) {
            mismatches.push(mismatch(
                &identity.code,
                "listing_vs_first_bar",
                Some(listing),
                first,
            ));
        }
        if let Some(expected) = termination {
            if last != Some(expected) {
                mismatches.push(mismatch(
                    &identity.code,
                    "termination_vs_last_bar",
                    Some(expected),
                    last,
                ));
            }
        }
    }
    for action in &facts.actions {
        if !matches!(action.kind.as_str(), "dividend" | "split" | "conversion") {
            continue;
        }
        let Ok(expected) = NaiveDate::parse_from_str(&action.ex_date, "%Y-%m-%d") else {
            continue;
        };
        let Some((listing, termination)) = lifecycle_containing(facts, &action.code, expected)
        else {
            mismatches.push(mismatch(
                &action.code,
                "action_outside_lifecycle",
                Some(expected),
                None,
            ));
            continue;
        };
        let Some(bar) = bars
            .iter()
            .find(|bar| bar.code == action.code && bar.date == expected)
        else {
            mismatches.push(mismatch(
                &action.code,
                "action_date_missing_bar",
                Some(expected),
                None,
            ));
            continue;
        };
        let previous_close = bars
            .iter()
            .filter(|candidate| {
                candidate.code == action.code
                    && candidate.close.is_some()
                    && candidate.date < expected
                    && candidate.date >= listing
                    && termination.is_none_or(|end| candidate.date <= end)
            })
            .max_by_key(|candidate| candidate.date)
            .and_then(|candidate| candidate.close);
        let Some(previous_close) = previous_close else {
            mismatches.push(mismatch(
                &action.code,
                "action_without_prior_close",
                Some(expected),
                Some(bar.date),
            ));
            continue;
        };
        let jump = bar.reference_price.is_some_and(|reference| {
            previous_close > Price::ZERO && differs_by_at_least_one_tick(reference, previous_close)
        });
        if !jump {
            mismatches.push(mismatch(
                &action.code,
                "action_date_vs_reference_price_jump",
                Some(expected),
                Some(bar.date),
            ));
        }
    }
    for event in &facts.events {
        // Daily keyed no-trade can only confirm a full-day halt; a morning or
        // open-ended partial halt still trades later that session.
        if event.kind != "suspension" || event.session_part != "full_day" {
            continue;
        }
        let Ok(expected) = NaiveDate::parse_from_str(&event.effective_date, "%Y-%m-%d") else {
            continue;
        };
        if lifecycle_containing(facts, &event.code, expected).is_none() {
            mismatches.push(mismatch(
                &event.code,
                "suspension_outside_lifecycle",
                Some(expected),
                None,
            ));
            continue;
        }
        if let Some(bar) = bars
            .iter()
            .find(|bar| bar.code == event.code && bar.date == expected)
        {
            if !bar.keyed_no_trade {
                mismatches.push(mismatch(
                    &event.code,
                    "suspension_vs_keyed_no_trade",
                    Some(expected),
                    Some(bar.date),
                ));
            }
        } else {
            mismatches.push(mismatch(
                &event.code,
                "suspension_missing_bar",
                Some(expected),
                None,
            ));
        }
    }
    mismatches.sort_by(|a, b| {
        (&a.code, &a.kind, a.expected_date).cmp(&(&b.code, &b.kind, b.expected_date))
    });
    CrosscheckReport { mismatches }
}

fn scoped_bars<'a>(
    bars: &'a [V4DailyBar],
    code: &str,
    listing: NaiveDate,
    termination: Option<NaiveDate>,
) -> Vec<&'a V4DailyBar> {
    bars.iter()
        .filter(|bar| {
            bar.code == code && bar.date >= listing && termination.is_none_or(|end| bar.date <= end)
        })
        .collect()
}

/// The identity lifecycle that owns `date`, or `None` when no lifecycle for the
/// code covers it. Sequential lifecycles for a reused code never merge.
fn lifecycle_containing(
    facts: &Mvp6Facts,
    code: &str,
    date: NaiveDate,
) -> Option<(NaiveDate, Option<NaiveDate>)> {
    facts.identities.iter().find_map(|identity| {
        if identity.code != code {
            return None;
        }
        let listing = NaiveDate::parse_from_str(&identity.listing_date, "%Y-%m-%d").ok()?;
        let termination = NaiveDate::parse_from_str(&identity.termination_date, "%Y-%m-%d").ok();
        (date >= listing && termination.is_none_or(|end| date <= end))
            .then_some((listing, termination))
    })
}

/// Compare two scale-18 prices at the v4 0.001 price increment. A missing
/// difference (overflow) is a jump by definition; no binary-float tolerance is
/// used.
fn differs_by_at_least_one_tick(left: Price, right: Price) -> bool {
    const ONE_TICK_MANTISSA: u128 = 1_000_000_000_000_000; // 0.001 at scale 18
    match left.mantissa().checked_sub(right.mantissa()) {
        Some(difference) => difference.unsigned_abs() >= ONE_TICK_MANTISSA,
        None => true,
    }
}

fn mismatch(
    code: &str,
    kind: &str,
    expected_date: Option<NaiveDate>,
    observed_date: Option<NaiveDate>,
) -> CrosscheckMismatch {
    CrosscheckMismatch {
        code: code.into(),
        kind: kind.into(),
        expected_date,
        observed_date,
    }
}
