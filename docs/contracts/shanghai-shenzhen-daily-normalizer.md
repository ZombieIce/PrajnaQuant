# Versioned Shanghai/Shenzhen stock and ETF daily normalizer

`cn-stock-etf-daily` v1 is a public Rust Normalizer for raw, unadjusted daily
bars of XSHG/XSHE stocks and ETFs (MVP-7 T02b-C). It is not a production input
assembler: selecting a collection report, calendar version and evidence version
from a local lake remains #289 work. The tests use only synthetic inputs.

## Fixed inputs and identity

The Normalizer is constructed with exactly one #328
[calendar version](shanghai-shenzhen-session-calendar.md) and exactly one #329
security-evidence snapshot id. Both ids enter `config_sha256`. The Raw set must
contain the byte-exact versions named by those ids, plus one or more Tencent
unadjusted `fqkline` daily responses:

| Input | Binding |
| --- | --- |
| Calendar version | Raw `sha256` equals the configured calendar version; both XSHG and XSHE are required, with their own source, coverage and sessions |
| Security snapshot | Raw `sha256` equals the configured snapshot id; every instrument in the snapshot must be `publishable` with Known positive `lot_size`, `price_increment` and `size_increment` |
| Daily responses | JSON `{code: 0, data: {<sh\|sz><six digits>: {day: [...]}}}` with a `tencent_fqkline_daily` source record whose `method_version` is `tencent-fqkline-day/1` and whose `instrument_id` matches the response key |
| Unit/source rules | Fixed in `config_sha256`; a rule change requires a new Normalizer version and identity |

No calendar or evidence store is read during `normalize` or rebuild. Later
appended source records, a revised evidence version, or a new calendar version
therefore cannot change an existing Dataset Version. The caller must persist and
reuse the same pinned identities; there is no "latest" fallback.

## Source and unit rules

Tencent `day` rows are `[date, open, close, high, low, lots]`. Prices are exact
decimal literals converted directly to scale-18 `Price` values; no binary
floating point is used. Volume is lots multiplied by 100, separately declared
for each supported security category:

| Category | Price unit | Volume unit | Amount |
| --- | --- | --- | --- |
| `Equity` | CNY per share, exact decimal | lots × 100 → shares | source has no amount field; `null` |
| `Etf` | CNY per fund unit, exact decimal | lots × 100 → fund units | source has no amount field; `null` |

`lot_size`, `price_increment` and `size_increment` come from the pinned evidence
snapshot, not from a default or from the security category. Currency is CNY and
the session timezone is `Asia/Shanghai`; each Session comes from the pinned
calendar, so XSHG and XSHE keep independent dates and open/close instants.

Rows with fields beyond the six documented ones are not consumed (`amount`
remains unknown), and this is reported in `ignored_input_fields`.

## Output and semantics

The published Dataset contains `instruments`, `sessions`, `bars` and
`bar_sources`. `bars` use `1d@session`, the exact pinned Session bounds, and
only the observed dates. `available_at` is always `null` (Unknown): neither
`observed_at` nor the Session close is used as a historical availability time.
`amount` is `null`. No execution status, adjustment, corporate action,
`forward_return`, or tradability window is derived. Listing and establishment
evidence remain evidence; they are not market coverage and are not converted
into a `tradable_from` window.

A Session present in the calendar is published even when no Bar exists for a
pinned instrument. A missing quote stays missing: it is never filled with a zero
Bar, and no Session is deleted to compress the calendar. Identical duplicate
facts are deduplicated with the minimum Raw hash; contradictory duplicates are
rejected. `provenance.coverage` reports pinned source/calendar/evidence
identities, per-venue Sessions, per-kind instruments, bars, duplicate counts and
the number of absent instrument-session pairs as `unknown`, not as halts.

## Rejections

Publication fails before any normalized file or manifest is written when:

- a pinned calendar or security snapshot is missing, malformed or has the wrong
  identity; an unexpected calendar/evidence version or unknown Raw shape is
  present; a Raw hash does not match its bytes or is listed twice;
- a security-evidence publication gate is blocked (Unknown, Conflict,
  NotApplicable or non-positive execution term);
- a daily response has no pinned source/method record, an unsupported native
  symbol, an instrument absent from the snapshot, or a missing/malformed `day`
  array or row;
- the complete pinned daily input set yields no valid Bar
  (`empty_daily_bars`). A well-formed response with an empty `day` array or an
  empty `data` object is accepted when another pinned response yields Bars; its
  dates remain missing and are never turned into zero Bars;
- a row has an invalid date, a date outside the venue's declared coverage or on
  a covered non-session date, a price/volume that is not an exact scale-18
  decimal or is out of range, an invalid OHLC relationship, or a negative
  volume;
- two observations for the same Instrument and date disagree.

## Limits and compatibility

This contract is synthetic-verified only; no real calendar, evidence or market
coverage is asserted. It does not publish a collection report, choose a source,
derive adjusted series or corporate actions, or open stock research/backtests.
The existing MVP-6 `szse-etf-frozen` rules, versions, identities and archives are
unchanged; this Normalizer has a separate id and version. Its immutable-input
identity basis is recorded in the [ADR 0011 supplement](../decisions/0011-immutable-data-and-reproducible-runs.md).

Production assembly (#289) remains responsible for choosing the exact Raw set:
it must not include predecessor/pre-listing rows for a reused code, and it must
exclude blocked-evidence instruments from the pinned snapshot when unrelated
securities must publish. v1 intentionally fails the whole publication for any
blocked selected instrument and does not derive a lifecycle/tradability gate.
