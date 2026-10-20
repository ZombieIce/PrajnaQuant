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

The broad-index list is required only for the legacy broad-index PIT selector.
`CatalogInput.broad_indices` may be omitted for fixed-list MVP-6 builds; those
builds still require complete Catalog lifecycle and coverage evidence. The
existing broad-index selector rejects a Catalog without its versioned list.

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
The caller must not replace publication with local `observed_at`. A legal-name
successor keeps its official `identity_start`/`listing` at the new name's
effective Session, but sets `identity_available_from` to the first Session on
which the rename is knowable under P6; `resolve_row`, derived-market cells,
Normalizer ranges and UNKNOWN coverage start there. The predecessor is **not**
extended to that point: the accepted `[effective, available_from)` gap is not a
fixed-list member, is reported as `listing_not_known`/`outside_catalog_lifecycle`,
and contributes exactly one UNKNOWN coverage cell per Session under the
predecessor InstrumentId.

For v1, internal ID remains
`<code>-<listing YYYYMMDD>-<first 48 uppercase contract hash hex>.SZSE`.
For v2, `identity_start` is derived from the first code-table appearance in the
covered history, each reappearance after a table-absence gap, and each
time-versioned legal full-name change. The earliest known name is the initial
identity even when its effective date falls after the first code-table
appearance; only a later, different name creates a change boundary. The same
rule applies to a reused code's successor run: when its first attribute is
later than the run's first appearance, that attribute is the successor's
initial name rather than a name change from the previous fund. The builder
checks that lifecycle identities match those boundaries, derives each identity's
exclusive end and code-table exit date, and retains each lifecycle's first/last
code-table appearance. Per-lifecycle `ScanObservation` values retain first/last priced
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
Per-Session Engine consumption belongs to #177, Python views to #178, Eligible
Set/liquidity/execution state to #176. This module does not itself enforce
61/60/20-Session eligibility, execute trades, compute performance, or assert
accounting/cost validation. These are downstream contracts, not completed here.

### Fixed-list MVP-6 selector (#231)

`FixedUniverseSpec` provides the separate `retrospective_static` selection path
for the five MVP-6 codes: 159919, 159560, 159522, 159612, and 159952. It
intersects the versioned list with Catalog lifecycle evidence known at each
decision close: pre-listing and post-termination Sessions are excluded, while
fund-category and broad-index classification are ignored. A bare TDX
code-table exit before the official termination does not end fixed-list
membership; the ETF remains a member through the gap and the missing-market
Sessions are reported as UNKNOWN. Membership ends at the official termination
or the next same-code identity's official listing, whichever is earlier; the
TDX `identity_end_exclusive` is only the physical price boundary. Its Universe Run
Spec contains the list, list version, Catalog identity, decision Session,
`history_claim=retrospective_static`, and the `survivorship_bias` warning.
The fixed list and its version also enter Experiment and Run identity. The
MVP-6 codes are the default when `pit.fixed_universe` is omitted; an explicit
`null` selects the existing broad-index PIT path and keeps its serialized
identity unchanged. Eligible Set then applies the same 61/60/20-Session and
execution-state rules.
This label and warning disclose the selected-list survivor bias; they do not
turn the fixed list into a complete or verified PIT universe.

Synthetic tests cover listing before/after eligibility, termination, codes
outside the list, list-sensitive identities, the 61-Session boundary, and
Vector/Fast Event daily parity. They do not use real ETF facts.

### Fixed-list MVP-6 offline assembly (#221)

`prajna_research::pit_catalog_assembly::assemble_mvp6_catalog` builds a Catalog
schema-v2 input from local frozen inputs: a published `szse-etf-frozen@4`
Dataset Version, the public `configs/mvp-6/identity.csv`, and the local
`~/PrajnaData/mvp-6/facts/citations.csv` that binds each identity field to a
frozen Raw announcement. The example writes the input, the built Catalog and a
local-only assembly report:

```sh
cargo run -p prajna-research --example assemble_mvp6_catalog --locked --offline -- \
  "$HOME/PrajnaData/mvp-6" "$DSV" "$HOME/PrajnaData/mvp-6/facts" \
  "$HOME/PrajnaData/mvp-6/acceptance/catalog-assembly/$(date +%Y%m%dT%H%M%S)"
```

The Catalog is a **fixed-list** Catalog: only codes with an identity row enter
it, and the daily TDX code-table snapshots are narrowed to those codes before
the v2 validator derives lifecycle boundaries. A fixed-list code from
`MVP6_FIXED_ETF_CANDIDATES` without an identity row is reported as an exclusion
(`identity_missing`); it is not an un-evidenced lifecycle. Codes outside the
identity table are out of scope for this `retrospective_static` Catalog, not
members of a claimed complete historical universe.

Lifecycle boundaries follow the v2 code-table rules. A reused code needs a
code-table absence gap between its rows. Continuous presence can only split on
an evidenced legal-name change, and that successor row must declare
`change_kind=legal_name_change`, use a later date and a different legal name.
#229's shared row validator accepts exactly that one non-terminated overlap;
unmarked successors remain an overlapping-lifecycle error in both paths, and
`szse-etf-frozen@4` ends the preceding normalized range at the declared change
date. A row whose lifecycle spans an un-evidenced prior run is excluded and
reported rather than joined to another identity. Without a code-table presence,
an uncorroborated **secondary-evidence** lifecycle is excluded because P27-7
makes the code table part of its corroboration; the **official-evidence**
lifecycles for the same reused code are kept with UNKNOWN coverage, and a code
with no official row is excluded as `secondary_code_table_absent`. Pre-window
listings are supported; a pre-window sequence of multiple
identity rows is not representable and is rejected. The v2 builder treats the
earliest known legal name as the initial identity, so a first attribute whose
effective date is later than the first code-table appearance does not invent a
second identity boundary.

An official listing date earlier than the first TDX code-table appearance stays
in `Lifecycle.listing`; the physical `identity_start` remains the TDX
appearance, so `resolve_row` has no price row before it. The fixed-list selector
uses the listing date for membership (P27-19), so that Session can be a member
with no price row and is reported as UNKNOWN by the coverage layer. The same
applies to a reused code's successor run: a TDX appearance before the
successor's official listing is a disclosed `listing_date_vs_code_table_first`
difference, not an exclusion. A TDX presence before the official listing is
likewise disclosed as an `IdentityDifference` rather than excluded; official
dates win, and only a priced Bar outside the official lifetime excludes the
whole code. A bare TDX exit before the official termination keeps the identity
in the fixed list until the official termination or the next identity's
official listing, whichever is earlier; the coverage grid reports the gap as
UNKNOWN. An identity
row with an empty `org_id` is excluded as `org_id_unresolved` (P27-19) instead
of failing the run.

Each lifecycle's listing, termination and attribute evidence resolves to the
frozen Raw cited for the field, and the evidence locator is the cited
`announcement_id` plus page. `AttributeFact` carries one evidence for the
name/category/index tuple, so those citations must all resolve to the same Raw
page or the assembly fails closed. The lifecycle's `contract_raw_sha256` is the
Raw cited by the row's required `field=contract` citation, whose excerpt must
state the legal name; it is not taken from the legal-name announcement. The
announcement date is read from the Raw's
cninfo `finalpage/YYYY-MM-DD/` path or from the matching `announcementTime` in a
frozen JSON page; local `observed_at` and the fact's effective date are never
substituted. Missing or ambiguous announcement dates fail closed. The resulting
date-only publication uses the normal P6 rule (first close of a covered Session
strictly after the announcement date). A publication date on or after the last
covered Session is an error. A legal-name change whose publication is only
usable at the next close leaves that one Session outside both identities: the
successor is reported as `listing_not_known` rather than becoming a member under
the old identity with new-name data. This is a conservative reported gap, not a
future-information claim.

The report records listing and termination differences from the TDX code table
(`listing_date_vs_code_table_first`, `termination_vs_code_table_presence`,
`code_table_exit_before_termination`, `code_table_presence_outside_lifecycle`,
`priced_bar_outside_tdx_presence`) with the identity's evidence level; the kind
and resolution are typed report enums. A bare TDX presence strictly before the
first identity or strictly after the last one is reported as
`code_table_presence_outside_lifecycle` and does not exclude the code when it has
no priced Bar. Official evidence wins and the difference is disclosed; a
secondary identity that disagrees is excluded. A priced Bar **outside the
official lifespan** excludes the code under P27-19. A priced Bar inside the
official lifespan but beyond a TDX presence run is a source disagreement
reported as `priced_bar_outside_tdx_presence` with an unresolved resolution; the
code stays in the fixed list and that Session is UNKNOWN. An identity with
official dates but no TDX code-table presence at all stays in the official
lifetime: it is reported as `code_table_absent` (unresolved), the Catalog
discloses `lifecycle_without_code_table_identity`, and the fixed list keeps the
code with UNKNOWN coverage. An uncorroborated **secondary-evidence** lifecycle
with no TDX presence is excluded instead, because the code table is part of its
corroboration under P27-7; the official lifecycle for the same reused code stays
in the fixed list, and a code with no official row is excluded as
`secondary_code_table_absent`. The UNKNOWN coverage denominator uses the legal
lifetime: the official listing date (not `identity_start`) starts it, and it
ends at the official termination or the next same-code identity boundary, so
Sessions missing from the TDX code table still count as UNKNOWN and are not
dropped. Every code-scoped exclusion adds a
`mvp6_excluded:<code>:<reason>` coverage gap. `fixed_universe` turns that gap
into a per-code exclusion and records the reason in
`run_spec.fixed_list_exclusions`; other gaps remain run-wide failures. The local
assembly report also lists the reason in `excluded_codes`, and the report must be
read alongside the Run Spec so the exclusion is auditable.

Synthetic tests cover pre-window and in-window listing, post-termination, code
reuse, an explicitly declared legal-name change with P6 availability and its
rejection when unmarked, a publication-later-than-effective rename whose physical
range starts only when knowable, contract evidence, shared attribute pages, missing
identity rows, unresolved orgIds, TDX disagreements, secondary evidence
exclusion, code-table absence, secondary no-presence exclusion,
official no-presence identities that stay in the
official lifetime, priced Bars outside the official lifespan, priced Bars
inside the official lifespan but beyond TDX presence, bare reuse
post-termination runs, TDX presence before the official listing without a
priced Bar, missing/ambiguous announcement dates, missing/invalid v4 input,
invalid Raw references, official-lifetime UNKNOWN coverage and equivalence
with a handwritten `CatalogInput`. No real v4 Dataset, identity
table or citation manifest is validated here.

Synthetic tests cover v1 compatibility and v2 code ranges/snapshots, code-table
gaps and reuse, legal-name identity changes, exact #202 publication mapping,
name-list matching and identity, as well as listing, termination, coverage,
Raw integrity and replay. Real P7 reconstruction, reviewed full-history
official extraction, complete announcement/code-table coverage and an
evidence-backed real domestic broad-index list remain **Unresolved** under
#172's source gate. No real Catalog or PIT Universe has been built or validated.
Therefore #175's real-data goal and platform Run/result integration cannot be
represented as accepted by this PR.
