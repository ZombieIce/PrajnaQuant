# Read-only Dataset preview entry

`prajna-preview-dataset` (MVP-7 T03a, Issue #335) reads formally committed Dataset
Versions through the existing manifest and table readers. It never normalizes,
publishes, collects, downloads or rewrites a Dataset, Raw object, receipt or the
MVP-6 archive. The command is the production read entry behind the local API and
page in #290; both layers only display what this entry returns.

## Root and discovery

`--root` is required. It is resolved to an absolute path and rejected when it is
the fixed MVP-6 archive (`$HOME/PrajnaData/mvp-6`) or one of its descendants. The
root must already exist; the command does not create it. Tests use an explicit
protected root to exercise the same rejection without touching a real archive.

`catalog` reads `<root>/manifests/*.json`, validates each manifest through
`read_manifest` (recomputed DSV, core integrity), then reads the required
`instruments`, `sessions` and `bars` tables and checks their readable row counts
against the manifest before listing the Dataset. It never chooses a "latest"
version. A missing or unreadable manifest, a missing table, a corrupt table, or a
row-count mismatch is returned in `invalid_manifests` with its file identity and
error; valid Datasets in the same root remain readable. An empty root or an empty
`manifests/` directory returns an empty catalog, not an error.

## Commands

```sh
prajna-preview-dataset catalog --root <LOCAL>
prajna-preview-dataset show --root <LOCAL> --dsv <DSV>
prajna-preview-dataset bars --root <LOCAL> --dsv <DSV> --instrument <INSTRUMENT_ID> \
  [--start YYYY-MM-DD] [--end YYYY-MM-DD] [--limit 1..=1000] [--offset N] \
  [--adjusted-version <ADJ_VERSION>]
```

`<DSV>` is `dsv:sha256:<64 lowercase hex>` or the bare 64 hex; otherwise the
request is invalid. Success prints one JSON object
`{"schema_version":1,"data":...}`. An error prints
`{"schema_version":1,"error":{"code":"...","message":"..."}}` and exits `2` for
invalid input/not found, or `1` for an I/O, manifest, table or root failure.
`--limit` defaults to 250; the CLI caps it at 1000.

## Catalog

Each dataset summary carries the full DSV, creation time, Normalizer id/version,
readable instrument/session/bar counts, the first and last Session, venues, and
the descriptive coverage fields `source`, `calendar_version`,
`security_snapshot` and `completeness_claim` when present. Descriptive
provenance is not part of the DSV identity; the manifest is the commit marker.

## Detail

`show` returns:

- Dataset identity, Normalizer identity, the descriptive `coverage`, per-venue
  Session coverage (count, first/last Session, first open/last close), instruments
  and warnings.
- The adjusted-version catalog for this DSV. If only that catalog is unreadable,
  raw detail still returns with `adjusted_series.error` and a warning.
- One entry per `instruments` row: real instrument identity and venue, kind,
  native symbol, base/quote currency, timezone, lot size, price and size
  increments, lifecycle evidence, actual Bar coverage and source names.
- `lifecycle` comes from the frozen #329 security-evidence snapshot that is one
  of the Dataset's committed inputs and is named by
  `coverage.security_snapshot`. Known values include their evidence source id and
  local observation time; Unknown, NotApplicable and Conflict remain explicit
  states. No listing date is derived from market coverage, and a
  missing/unreadable/foreign snapshot leaves lifecycle Unknown with a warning.
- `coverage.session_gaps` lists Sessions inside the observed Bar range
  (`first_bar..=last_bar`) that have no Bar, capped at 200 dates
  (`session_gaps_truncated`). `sessions_before_first_bar` and
  `sessions_after_last_bar` are counts only; they are not called gaps or halts.
  Gaps are never filled with a zero Bar, and no Session is deleted.
- The descriptive `coverage` object keeps the Normalizer's declarations for raw
  price, units, `amount` absence and `available_at` Unknown; the Bars response
  labels price/volume units and states that amount is unknown. Execution status
  is not derived, and missing quotes stay missing.
- `bar_sources` is optional. When present, each instrument lists distinct source
  names; when absent or incomplete, `bars_without_source` and warnings say so
  instead of inventing a source. Per-Bar source and source Raw hash appear in the
  Bars response.

## Bars

`bars` verifies the requested instrument is part of the Dataset, applies the
inclusive date range, sorts by Session date and returns
`total/offset/next_offset` with at most `--limit` items. Each item contains the
raw unadjusted OHLC, volume, nullable amount/available_at, per-Bar source and
source Raw hash, and a nullable reference price. Prices and quantities are
decimal strings so scale-18 values are not rounded through JSON floating point.
The response echoes the DSV/Normalizer identity, instrument identity, requested
range, first/last Bar, unit labels and warnings (for example a missing source).
With `--adjusted-version`, the command reads one explicit supplier-adjusted
version through the [adjusted research series](adjusted-research-series.md)
reader. That response is an adjusted sibling payload rather than a raw Bars
superset; the contract lists the omitted raw-only fields.

## Corporate-action fact coverage

The same read-only command lists persisted #360 corporate-action fact versions
and reads one fixed version, security and closed range. It never publishes,
downloads, recalculates, rewrites a fact file or chooses a latest version.

```sh
prajna-preview-dataset actions-catalog --root <LOCAL>
prajna-preview-dataset actions --root <LOCAL> --version <CAF_VERSION> \
  --instrument <INSTRUMENT_ID> --start YYYY-MM-DD --end YYYY-MM-DD
```

`actions-catalog` reads `<root>/corporate-facts/*.json` and revalidates every
version through the production fact reader. Each summary carries the fixed
`caf:sha256:` version, its local `observed_at` and each declared scope's
instrument identity, kind, closed range, four-state `state`, reason, event count
and incomplete-event count. Versions are ordered by `observed_at` descending and
then version id; an old or superseded revision stays listed, and no version is
marked latest. A missing `--root` directory is a root error, not an empty version
list; an empty or absent `corporate-facts/` directory inside an existing root
returns an empty list. A version whose bytes no longer match its identity or no longer satisfy the
contract is reported in `invalid_versions` with its identity and error while
valid versions in the same root remain readable.

`actions` requires an explicit version, instrument and closed range, and forwards
them to `corporate_facts::query`. The response is #360's scoped payload inside
the same `schema_version`/`data` envelope: state, the original declared scope
(including its facts and source/history availability), `missing` field names per
event, the version's `observed_at` and the read-only notice. A range not wholly
inside one declared scope is `unknown`; ranges are never stitched together and
missing dates/amounts/source/availability remain null rather than being shown as
present. Events are returned for the declared scope, not clipped to the queried
sub-range. `observed_at` is local observation time, not historical availability.

The local service exposes the catalog at `/api/v1/corporate-actions` and the
scoped read at `/api/v1/corporate-actions/{version}?instrument=&start=&end=`,
with the same Host/HTTP-Origin restriction, no-store responses, argv-only CLI
call and status mapping as the Dataset routes (invalid input 400, missing version
404, unavailable root/CLI 503, corrupt version or I/O failure 500, invalid CLI
envelope 502). The `/datasets` page renders this independently from Dataset
browsing, so a missing or corrupt fact version cannot hide unrelated Dataset or
factor views.

## Limits and compatibility

- Reads whole Parquet tables into memory per request; no pushdown or cache is
  claimed. `catalog` reads and validates all required tables for every DSV, so
  its cost grows with the lake size. Bars are paged after the Dataset is read.
- Raw unadjusted Bars remain the default. The optional [adjusted research series](adjusted-research-series.md) contract exposes explicit imported supplier versions through `--adjusted-version`; its Bars payload is a documented sibling, not an additive superset. Corporate-action facts are readable only through the separate coverage modes above; this preview entry does not derive, publish or apply them. Execution status and factor/label/ledger results remain outside this preview entry.
- The existing MVP-6 SZSE Normalizers, readers, results and archive identities
  are unchanged; this entry rejects the archive root rather than mixing them.
- Checked-in tests publish synthetic XSHG/XSHE stock and ETF Datasets through
  `prajna-publish-daily`, then cover catalog/detail/bars, gaps, Unknown
  lifecycle, revisions, invalid manifests, missing tables, a committed Dataset
  with empty tables, pagination and read-only behavior. No real calendar, evidence, market coverage or Hash is
  validated or published by these tests.

## Local HTTP/page adapter

The local Axum service exposes catalog/detail/bars under `/api/v1/datasets`
and a read-only `/datasets` page, restricted to its bound Host/HTTP Origin; external and opaque origins (including preflights) are rejected. It invokes this CLI over an argv subprocess
boundary and preserves its versioned JSON envelope, decimal strings, Unknown
states and missing quotes. `serve --dataset-root` is optional for compatibility;
when absent the Dataset APIs explicitly return unavailable. It never falls back
to old snapshot/output/archive roots. See [API configuration and synthetic checks](../api.md#已发布研究-dataset-预览).
