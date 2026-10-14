# Historical SZSE ETF Catalog / PIT Universe v1

Issue [#175](https://github.com/ZombieIce/PrajnaQuant/issues/175), spec
[#171](https://github.com/ZombieIce/PrajnaQuant/issues/171) P7/P8 and
[ADR 0022](decisions/0022-historical-etf-catalog-and-derived-execution-status.md).

## Implemented boundary

`prajna_research::pit_universe` builds a frozen Catalog from explicit, reviewed
statement extraction and historical scan observations. It verifies every Raw
reference with MVP-0 `RawStore::get`; it does not parse PDF statements, fetch
vendor originals, infer fund category from names, or use announcement title
classification as attribute evidence. A hash establishes byte integrity, not
that the extracted statement or completeness declaration is factually correct.
Review of official attachments and the declared calendar/coverage remains required.

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

`CatalogInput.schema_version = 1`; explicit `rule_version` identifies extraction
and selection rules. `sessions` contains validated SZSE `venue_id`,
`session_date`, `ts_open`, `ts_close`; `calendar_start/end` and Raw calendar
references declare the covered calendar, including non-session days. This
calendar must be complete, rather than a list of successful price dates.

A lifecycle records native symbol, contract attachment hash, listing and
optional termination facts, and time-versioned category/index statements.
Each statement has an effective date and `Evidence` (Raw SHA-256, announcement
identity, page/section locator, historical publication). Evidence is extracted
from the listing notice, official contract/prospectus or change announcement.
The caller must not replace publication with local `observed_at`.

Internal ID is `<code>-<listing YYYYMMDD>-<first 48 uppercase contract hash hex>.SZSE`.
The full contract hash remains in the Catalog. Duplicate identities and
physical overlapping lifecycles are errors. The native symbol is retained;
the ID string is not an attribute classifier. `resolve_row(code, date)` assigns
a price row only within the evidenced listing interval, excluding termination
date; unmapped rows are errors and must be quarantined. Call it for **every**
row before downstream price/factor/action binding. This ticket does not rewrite
#174's bare-code Dataset Versions or existing Static Universe identities.

`Coverage` declares start/end, scan and announcement retrieval methods,
announcement Raw references (including pagination, attachments and pre-window
contracts), explicit gaps and adjudicated difference explanations. `scans`
requires at most one full-window observation per code 159000–159999:
`observed`, `no_rows`, or `unresolved`. Missing/unresolved codes create gaps.
Observed endpoints without a lifecycle create gaps unless explicitly resolved
as scan-only differences. NoRows never proves that a fund did not exist.

Two-way crosscheck reports `scan_only` and `announcement_only`. Each difference
needs a nonempty explanation and official evidence. Resolutions of nonexistent
differences, duplicate codes and malformed dates/hashes are errors. Any coverage
gap blocks **all** Universe builds; an empty member set cannot hide coverage
failure. Raw verification and scan endpoint checking are necessary checks,
not independent proof of attachment pagination or of every interior price row.

Broad-index membership uses provider-qualified IDs and an explicitly versioned
`BroadIndexList`, backed by official evidence. No real whitelist is silently
provided. A list whose evidence is unavailable at the decision is rejected.
Scope/list revisions change the content identity. This is a frozen research
policy: a new list creates a new Run input rather than rewriting an old Run.

Catalog content hash includes canonical sorted facts, sessions, scan outcomes,
source references, rule/index versions and coverage. `compare_current` is a
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
the broad-index list have separate exclusion reasons. A later change has no
retroactive effect. Lifecycle and selection exclusions remain in each result.

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

Synthetic tests cover three lifecycle identities over ten Sessions, listing,
termination, code reuse, late publication, attribute changes/conflicts, current
list invariance, coverage/difference gates, Raw integrity, identity and replay.
Real P7 reconstruction, reviewed full-history official extraction, complete
announcement/scan coverage and an evidence-backed real domestic broad-index
list remain **Unresolved** under #172's source gate. No real Catalog or PIT
Universe has been built or validated. Therefore #175's real-data goal and the
platform Run/result integration cannot be represented as accepted by this PR.
