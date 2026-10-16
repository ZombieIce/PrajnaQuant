# Historical SZSE ETF Catalog / PIT Universe v1 and v2

Issue [#175](https://github.com/ZombieIce/PrajnaQuant/issues/175), spec
[#171](https://github.com/ZombieIce/PrajnaQuant/issues/171) P7/P8 and
[ADR 0022](decisions/0022-historical-etf-catalog-and-derived-execution-status.md).

## Implemented boundary

`prajna_research::pit_universe` retains Catalog schema v1 and adds schema v2.
V1 uses the original explicit statement/scan input and preserves its identity
and 159xxx-only semantics. V2 consumes complete daily TDX code-table snapshots
published by `szse-etf-frozen@4` and explicit #202 name facts. It verifies every
Raw reference with MVP-0 `RawStore::get`; it does not fetch vendor originals or
parse PDF statements. A hash establishes byte integrity, not that an
extraction, official index list, or completeness declaration is factually
correct. Review of official attachments and declared calendar/coverage remains
required.

The offline example accepts a `CatalogInput` JSON:

```sh
cargo run -p prajna-research --example build_etf_catalog --locked --offline -- \
  /local/lake/raw /local/extracted-catalog.json /local/frozen-catalog.json 2023-01-03
```

Diagnostics (Catalog version, bidirectional differences and gaps) go to stderr;
the successful close-session `UniverseBuildResult` JSON goes to stdout.
Coverage failure exits with an error, while preserving the diagnostic frozen
Catalog for review. Frozen files use create-new semantics; revise to a new file.
`HistoricalCatalog::load(path, expected_version, raw)` revalidates Raw integrity,
structure and the expected content identity. Do not publish real source files
or price rows in this public repository.

## Input and identity

`CatalogInput.schema_version = 1` uses the legacy scan contract. Schema v2
requires explicit, disjoint six-digit `code_ranges` matching the TDX normalizer
configuration and one `CodeTableSnapshot` per covered Session. Each snapshot
contains the sorted scoped codes and TDX Raw SHA-256; an empty symbol set is a
real empty-table observation, while a missing Session snapshot is an error.
These snapshots come from the `tdx_code_table` table emitted only by
`szse-etf-frozen@4`; v1/v2/v3 Normalizer output and Catalog v1 identities remain
unchanged. The v4 table also stores its configured ranges so a Catalog cannot
silently widen or narrow the source scope.

An explicit `rule_version` identifies extraction and selection rules.
`sessions` contains validated SZSE `venue_id`,
`session_date`, `ts_open`, `ts_close`; `calendar_start/end` and Raw calendar
references declare the covered calendar, including non-session days. This
calendar must be complete, rather than a list of successful price dates.

A lifecycle records native symbol, contract attachment hash, listing and
optional termination facts, and time-versioned category/index/name statements.
Each statement has an effective date and `Evidence` (Raw SHA-256, announcement
identity, page/section locator, historical publication). Evidence is extracted
from the listing notice, official contract/prospectus or change announcement.
The caller must not replace publication with local `observed_at`.

For v1, internal ID remains
`<code>-<listing YYYYMMDD>-<first 48 uppercase contract hash hex>.SZSE`.
For v2, `identity_start` is derived from the first code-table appearance in the
covered history, each reappearance after a table-absence gap, and each
time-versioned legal full-name change. The builder checks that lifecycle
identities match those boundaries, derives each identity's exclusive end and
code-table exit date, and retains each lifecycle's first/last code-table
appearance. Per-lifecycle `ScanObservation` values retain first/last priced
Sessions. Missing snapshots or missing/extra lifecycle boundaries create
coverage gaps; they cannot silently join two code periods. V2 IDs use
`<code>-<identity_start YYYYMMDD>-<first 48 uppercase contract hash hex>.SZSE`.
The full contract hash remains in the Catalog. Duplicate identities and
overlapping lifecycles are errors. `resolve_row(code, date)` assigns a price row
only inside its lifecycle and legal listing/termination interval; unmapped rows
are errors and must be quarantined. Call it for **every** row before downstream
price/factor/action binding. This ticket does not rewrite #174's v1-v3 Dataset
Versions or existing Static Universe identities.

`Coverage` declares start/end, scan and announcement retrieval methods,
announcement Raw references (including pagination, attachments and pre-window
contracts), explicit gaps and adjudicated difference explanations. V1 `scans`
requires at most one full-window observation per code 159000–159999:
`observed`, `no_rows`, or `unresolved`. V2's full daily code table is
authoritative for code presence within its configured scope; each lifecycle
has a separate first/last-priced-Session observation. Missing/unresolved v1
scans create gaps. `NoRows` never proves that a fund did not exist.

Catalog v1's two-way crosscheck reports `scan_only` and `announcement_only`.
Each difference needs a nonempty explanation and official evidence. These
resolutions apply to Catalog v1's scan crosscheck; Catalog v2 uses its daily
code tables as the authority and requires `coverage.resolutions` to be empty.
Resolutions of nonexistent differences, duplicate codes and malformed
dates/hashes are errors.
Any coverage gap blocks **all** Universe builds; an empty member set cannot hide
coverage failure. Raw verification and scan endpoint checking are necessary
checks, not independent proof of attachment pagination or of every interior
price row.

V1 broad-index membership uses provider-qualified IDs. V2 uses an explicitly
versioned `BroadIndexList` of official scale-index names and curated aliases,
backed by official evidence; its provider-qualified IDs must match the named
entries used for classification. The current repository does not silently provide
or claim a real, independently verified whitelist. The latest known legal fund
full name must contain a configured name/alias after whitespace normalization.
A match immediately followed by another ASCII digit is rejected to prevent
shorter numeric names from matching a different index (for example,
`中证100` matching `中证1000`). No venue abbreviation, inferred keyword, or
`secName` is used. The #202 `NameFact.announced_ms` is retained as exact
publication evidence and is evaluated at that timestamp, never at local
`observed_at`. Missing or unmatched names are excluded and reported for that
Session. A list whose evidence is unavailable at the decision is rejected.
Scope/list revisions change the Catalog and Universe identities; they do not
rewrite prior inputs.

Catalog v1 content hash includes its canonical sorted facts, sessions, scan
outcomes, source references, rule/index versions and coverage. Catalog v2
content hash additionally includes configured code ranges, all daily
code-table snapshots, lifecycle/price boundaries, legal-name facts and the
name/alias list. `compare_current` is a
pure diagnostic function; today’s directory is excluded from this identity and
from historical selection. Replaying a frozen version gives the same results.

## Close-session knowledge and exclusions

A date-only announcement D is first usable at the close of the first covered
Session strictly after D. Missing calendar coverage or no next Session is an
error. Exact publication timestamps need explicit evidence and are used as-is.
Listing requires both effective date and available evidence. Termination
excludes new membership once effective **and known**; a later-discovered notice
is not backfilled into earlier decisions. Physical row mapping still uses the
legal lifecycle interval. If a reused code is simultaneously eligible under
as-of knowledge, Universe generation fails as an identity conflict.

For attributes, choose the latest effective date among statements already
available by decision close. Inconsistent values at that date exclude the
instrument as `attribute_conflict`; no source wins by input order or by local
collection time. Missing attributes, non-equity category and indexes outside
the broad-index list have separate exclusion reasons. In v1, missing/conflicting
classification of an active fund also creates decision-specific membership
gaps: `universe` fails, because the unknown fund could belong to the requested
pool. V2 maps #202 `NameFact.announced_ms` to an exact publication timestamp and
uses the timestamp's Asia/Shanghai date as the name's effective date; a
pre-populated `available_at` is rejected so availability is resolved from the
source publication rather than collection time. Missing or nonmatching names
are reported as per-Session exclusions, not as proof of a complete index
population. Unresolved identity boundaries and code-table coverage still fail
closed. A later change has no retroactive effect. Lifecycle and selection
exclusions remain in each result.

Example: listing effective S2 is disclosed on date S3; S2/S3 exclude it, S4 close
first includes it, and execution remains a subsequent-open concern. An index
change effective S2 but announced on date S4 does not affect S4 close; it first
changes S5 close membership. The tests use ten synthetic Sessions (including
weekend dates intentionally) rather than making claims about a real calendar.

## Run/result contract and remaining work

Every `UniverseBuildResult` includes `universe_version`, members, per-ID
exclusions and its identity-bearing `UniverseRunSpec` fragment: Catalog,
broad-index and rule versions, source hashes, declared coverage, coverage gaps,
decision Session/time. The Universe hash covers this fragment and selection
results. Empty successful Universes retain all provenance. Incomplete coverage
produces Catalog diagnostics, never a successful build result.

This is an **input-build Run Spec fragment**, not an executed backtest Run.
The existing `prajna-experiment` Engine Run Specs/results still use Static
Universe. Per-Session Engine consumption belongs to #177, Python views to #178,
Eligible Set/liquidity/execution state to #176. This module does not enforce
61/60/20-Session eligibility, execute trades, compute performance, or assert
accounting/cost validation. These are downstream contracts, not completed here.

Synthetic tests cover v1 compatibility and v2 code ranges/snapshots, code-table
gaps and reuse, legal-name identity changes, exact #202 publication mapping,
name-list matching and identity, as well as listing, termination, coverage,
Raw integrity and replay. Real P7 reconstruction, reviewed full-history
official extraction, complete announcement/code-table coverage and an
evidence-backed real domestic broad-index list remain **Unresolved** under
#172's source gate. No real Catalog or PIT Universe has been built or validated.
Therefore #175's real-data goal and platform Run/result integration cannot be
represented as accepted by this PR.
