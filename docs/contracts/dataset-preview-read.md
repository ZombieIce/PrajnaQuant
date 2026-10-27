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
`read_manifest` (recomputed DSV, core integrity), and lists them without a
"latest" pointer. A missing or unreadable manifest, a manifest missing a required
table, or a manifest whose `sessions` table cannot be read is returned in
`invalid_manifests` with its file identity and error; valid Datasets in the same
root remain readable. An empty root or an empty `manifests/` directory returns
an empty catalog, not an error.

## Commands

```sh
prajna-preview-dataset catalog --root <LOCAL>
prajna-preview-dataset show --root <LOCAL> --dsv <DSV>
prajna-preview-dataset bars --root <LOCAL> --dsv <DSV> --instrument <INSTRUMENT_ID> \
  [--start YYYY-MM-DD] [--end YYYY-MM-DD] [--limit 1..=1000] [--offset N]
```

`<DSV>` is `dsv:sha256:<64 lowercase hex>` or the bare 64 hex; otherwise the
request is invalid. Success prints one JSON object
`{"schema_version":1,"data":...}`. An error prints
`{"schema_version":1,"error":{"code":"...","message":"..."}}` and exits `2` for
invalid input/not found, or `1` for an I/O, manifest, table or root failure.
`--limit` defaults to 250; the CLI caps it at 1000.

## Catalog

Each dataset summary carries the full DSV, creation time, Normalizer id/version,
table row counts, instrument/session/bar counts, the first and last Session,
venues, and the descriptive coverage fields `source`, `calendar_version`,
`security_snapshot` and `completeness_claim` when present. Descriptive
provenance is not part of the DSV identity; the manifest is the commit marker.

## Detail

`show` returns:

- Dataset identity, Normalizer identity, the descriptive `coverage`, per-venue
  Session coverage (count, first/last Session, first open/last close), instruments
  and warnings.
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

## Limits and compatibility

- Reads whole Parquet tables into memory per request; no pushdown or cache is
  claimed. Bars are paged after the Dataset is read.
- Only raw unadjusted daily Bars and the frozen evidence fields above are
  exposed. Adjustments, corporate actions, execution status, research values,
  factor/label/ledger results and HTTP endpoints are out of scope for this entry.
- The existing MVP-6 SZSE Normalizers, readers, results and archive identities
  are unchanged; this entry rejects the archive root rather than mixing them.
- Checked-in tests publish synthetic XSHG/XSHE stock and ETF Datasets through
  `prajna-publish-daily`, then cover catalog/detail/bars, gaps, Unknown
  lifecycle, revisions, invalid manifests, missing tables, a committed Dataset
  with empty tables, pagination and read-only behavior. No real calendar, evidence, market coverage or Hash is
  validated or published by these tests.
