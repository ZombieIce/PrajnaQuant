# Versioned Shanghai/Shenzhen stock and ETF daily normalizer

`cn-stock-etf-daily` v1 is a public Rust Normalizer for raw, unadjusted daily
bars of XSHG/XSHE stocks and ETFs (MVP-7 T02b-C). The production assembler below
selects collection reports and fixed versions (#289). Tests use synthetic inputs.

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
| Daily responses | JSON `{code: 0, data: {<sh\|sz><six digits>: {day: [...]}}}` (also exact string `code: "0"`) with a `tencent_fqkline_daily` source record whose `method_version` is `tencent-fqkline-day/1` and whose `instrument_id` matches the response key; collector, production assembly and Normalizer share the same success-code check |
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
  symbol, an instrument absent from the snapshot, a non-zero response `code`, or
  a missing/malformed `day` array or row;
- the pinned Raw set contains no daily response (`missing_daily_raw`), or the
  complete pinned daily input set yields no valid Bar (`empty_daily_bars`). A
  well-formed response with an empty `day` array or an empty `data` object is
  accepted when another pinned response yields Bars; its dates remain missing
  and are never turned into zero Bars;
- a row has an invalid date, a date outside the venue's declared coverage or on
  a covered non-session date, a price/volume that is not an exact scale-18
  decimal or is out of range, an invalid OHLC relationship, or a negative
  volume;
- two observations for the same Instrument and date disagree.

## Limits and compatibility

This contract is synthetic-verified only; no real calendar, evidence or market
coverage is asserted. The Normalizer does not choose a source,
derive adjusted series or corporate actions, or open stock research/backtests.
The existing MVP-6 `szse-etf-frozen` rules, versions, identities and archives are
unchanged; this Normalizer has a separate id and version. Its immutable-input
identity basis is recorded in the [ADR 0011 supplement](../decisions/0011-immutable-data-and-reproducible-runs.md).

Production assembly (#289) remains responsible for choosing the exact Raw set:
it must exclude rejected-candidate responses (`response_outcome` other than
`accepted_response`), must not include predecessor/pre-listing rows for a reused
code, and must exclude blocked-evidence instruments from the pinned snapshot
when unrelated securities must publish. v1 intentionally fails the whole
publication for any blocked selected instrument and does not derive a
lifecycle/tradability gate.

## Local production publication and rebuild

`prajna-publish-daily publish --root <LOCAL_OUTPUT> --selection <LOCAL_JSON> --receipt <RELATIVE_JSON> --diagnostics <LOCAL_DIAGNOSTICS>` accepts this explicit selection (paths are local; replace version placeholders with chosen immutable ids):

```json
{
  "raw_root": "/local/raw", "calendar_root": "/local/calendar", "evidence_root": "/local/evidence",
  "report": "/local/raw/reports/daily-raw/selected.json",
  "normalizer_id": "cn-stock-etf-daily", "normalizer_version": "1",
  "calendar_version": "sha256:<chosen calendar>", "security_snapshot": "sha256:<chosen snapshot>",
  "raw_sha256s": ["sha256:<chosen accepted response>"]
}
```

Each publication selects exactly one security in the snapshot and one report; publish unrelated securities independently. Every selected response must be a nonempty accepted complete chunk with exact source/method, security/kind, request range, chunk index, source and outcome provenance. Raw dates must match the report, lie within that chunk and not precede a Known listing date. Unknown listing history stays Unknown; no predecessor history is inferred. Calendar coverage must contain the report request; all Sessions of that explicit calendar version are retained, including dates without Bars. Partial reports may supply explicitly selected good chunks; missing/failed/empty chunks never become Bars or a claim of complete coverage. Blocked constraints, unsupported source/unit rule versions, mismatches and rejected candidates fail before Dataset publication.

The output lake freezes exact report bytes, selected responses/provenance, canonical calendar/snapshot and a content-addressed recipe. Selected response provenance is copied only when the complete source record is absent at the target; retries within the same lake, through aliases, or between different lakes do not reappend identical records, while newly observed source records are preserved. The root-relative receipt binds that recipe to the committed DSV. Repeat the same selection/receipt idempotently; a different selection needs a new receipt. Revised Raw/calendar/evidence produces a new DSV, preserving old versions; rule revisions require a separately implemented Normalizer identity (v1 remains frozen; opt-in v2 is specified below). `prajna-publish-daily rebuild --root <LOCAL_OUTPUT> --receipt <RELATIVE_JSON> --diagnostics <LOCAL_DIAGNOSTICS>` verifies the frozen recipe and rebuilds against the committed manifest without the original report/stores or a latest lookup.

The Dataset manifest remains the atomic commit marker. Interrupted pre-commit publication leaves no committed partial Dataset and is retryable; frozen Raw may remain. A receipt-write failure after the Dataset commit can leave a complete Dataset without a receipt: rerun the unchanged selection to recover it. Receipts cannot overwrite another selection. Output roots exclude the fixed MVP-6 archive; existing symlink destinations are rejected. CLI output hides detailed identities and coverage; errors go to the configurable local diagnostics directory. A diagnostic persistence failure may leave only the generic CLI failure. No real-data validation is asserted.


## Opt-in v2 opening-state evidence

Issue #350 / [ADR 0029](../decisions/0029-cn-daily-opening-evidence.md) adds
`normalizer_version: "2"` and required selection `status_snapshot:
"sha256:<chosen Raw JSON>"`. Import the snapshot and each referenced original
into the explicitly selected `raw_root` using `RawStore::put` with FileImport
provenance. This is fixed evidence import, not a collector or a vendor-status
parser. `OpeningEvidenceSnapshot::parse` exposes the structural contract.

A snapshot uses these fields (all are required unless explicitly nullable):

| Field | Contract |
| --- | --- |
| `schema_version`, `method_version` | `1`, `cn-opening-evidence/1` |
| `evidence_kind` | `synthetic` or `real_source`; labels provenance, not verification |
| `source_id`, `source_version`, `extraction_version` | Nonempty fixed source/extraction identifiers |
| `calendar_version`, `security_snapshot` | Exact selected calendar/security content ids |
| `market_raw_sha256s` | Exact nonempty accepted daily Raw selection, unique |
| `instrument_id`, `venue_id`, `start`, `end` | Exact single-security/report scope, inclusive dates, matching Venue |
| `observed_at` | RFC3339 local collection timestamp; never used as historical availability |
| `evidence` | Array of `{raw_sha256, locator, available_at}`; locator is nonempty, availability nullable RFC3339; original bytes must exist and pass content hash verification |
| `facts` | Array of the fact structure below; an explicit empty array publishes all Unknown |

```json
{
  "session_date": "2026-01-06",
  "open_from": "2026-01-06T09:30:00+08:00",
  "open_to_exclusive": "2026-01-06T09:31:00+08:00",
  "state": "TRADABLE",
  "available_at": "2026-01-06T09:30:00+08:00",
  "evidence_indices": [0],
  "reason": "explicit synthetic opening fact"
}
```

Each fact must cover the pinned Session open with its half-open interval, refer
to at least one valid unique zero-based evidence index, and carry a nonempty
reason. Historical availability must be at least the latest of all necessary
references; any null necessary time or null fact time yields Unknown. A late
precise time stays late. `HALTED`/`UNKNOWN` never permit execution; explicit
`CONFLICT` normalizes to D7 `UNKNOWN` while the frozen snapshot retains its reason.
Exact duplicate facts deduplicate; differing facts at one Session fail, including
different reasons, times or references. Unknown enums/fields, mismatched scope,
non-Session/out-of-range dates, missing/corrupt originals or missing snapshot
fail the whole publication. A locator identifies the provider's claimed evidence;
the importer does not certify that the original text proves that claim.

v2 always publishes `execution_status` for every selected Instrument/Session
within the report range, including dates without Bars. Missing facts produce
`UNKNOWN/false/null` with a missing-source marker. Outside this declared scope,
legacy row-missing behavior remains rejection. D7 reader still errors for an
entire missing table; a missing row is never implicitly tradable. Execution
continues to require `has_bar && is_tradable && available_at <= ts_open`.
No bar, volume, calendar, listing history or absence of halt notices proves
TRADABLE. v1 market `available_at` remains null; experiments must explicitly
choose their existing market-availability assumption independently of status.

Recipe schema 2 freezes the snapshot id, original Raw, market Raw and existing
calendar/security versions; all state bytes enter manifest inputs/content identity
and coverage provenance. Rebuild uses these frozen bytes after source stores are
removed. Revisions require a new snapshot/receipt/DSV; no table is appended to an
old DSV. Schema 1 recipes and v1 normalization/config bytes remain compatible.

Synthetic tests exercise the real publisher CLI, D7 reader, Experiment CLI and
Standard persisted-result read for each Venue. With 10,000 cash and 1% commission,
S1 close decides, S2 open at 10 buys 990 units for 9,900 plus 100 fee; cash is zero,
open equity is 9,900 and close at 10.1 values equity at 9,999. Null/late/halted/
unknown/conflict/missing status and missing-Bar counterexamples cannot trade.
Real source availability, historical coverage and correctness remain unverified;
this does not certify a real research loop. #340 owns subsequent HTTP/T01 replay.
