# `prajna-data`

## Local single-security daily Raw collection

`prajna-collect-daily-raw` collects one explicitly configured Shanghai or Shenzhen stock/ETF from the shared Tencent unadjusted daily transport. It stores response bytes and append-only source provenance in an independent RawStore, then writes a per-invocation coverage/error report below `<raw-root>/reports/daily-raw/`. The binary requires an explicit local root; it resolves existing path components (including symlinks) before refusing the MVP-6 archive root and its descendants, then uses that validated root for Raw, report, and diagnostic writes. It rejects pre-existing symlinks along each Raw object/provenance, report, and diagnostic destination path. No report contains normalized bars and no mutable latest-success marker is written.

Example invocation (use only when you have configured an appropriate local data root):

```sh
cargo run -p prajna-data --bin prajna-collect-daily-raw -- \
  --raw-root /path/to/local/prajna-raw \
  --symbol 510300 --venue XSHG --kind etf \
  --start 2026-01-01 --end 2026-01-30
```

`--symbol` is a six-digit code without the source prefix. `--venue` must be `XSHG` or `XSHE`; `--kind` must be `stock` or `etf`. Both date bounds are inclusive. Ranges are split into non-overlapping windows of at most 800 calendar dates to satisfy the shared source request bound. Requests to the same Tencent registrable domain are spaced by at least one second, including across concurrent collector processes that share the local pacing directory. Separate OS temporary directories do not coordinate pacing. Transient `BodyRead` and exhausted-candidate failures receive at most three retries with 1, 2, and 4 second exponential backoff; the existing consecutive access-restriction stop rule is terminal. A non-`complete` result is persisted and the command exits with status 2; rerunning the same request remains an operator-controlled retry mechanism.

Each report separates the configured range from dates actually present in each response, records calendar dates not returned, dates outside the requested chunk, duplicates, candidate attempts/failures, empty responses, accepted Raw references and local observation times. `complete` means every requested source-limited range returned at least one valid date in range. It does not mean exchange-session completeness or full-history completeness. Calendar gaps do not establish a missing trading session, a suspension, or whether an instrument was tradable. `observed_at` is the local time a response was observed, not a historical vendor publication time or historical `available_at`.

The response is preserved byte-for-byte, including a changed response as a new content-addressed object; previous objects and provenance remain available. Repeated identical bytes reuse the same Raw identity and append another provenance record. Valid empty responses and failed requests are reported without deleting prior Raw data or advancing a success pointer. Read the exact Raw references from the local report. The CLI prints only a report path and aggregate counts, not response bytes or object hashes. Fatal local collection errors are summarized without hashes on stderr; full error details are written to a JSON diagnostic under `<raw-root>/reports/daily-raw/diagnostics/` and remain local.

OHLCV, amount, currency, and timezone units are marked `unknown` unless a later contract establishes them. `established_date`, `listing_date`, `lot_size`, `price_increment`, and `size_increment` use the same field names as the existing `SecurityEvidenceRecord`, but remain `unknown; T02-meta evidence required` here so typed T02-meta evidence can supply them separately. This collector does not infer those values. Raw provenance records the method version, instrument identity, source, requested chunk, response outcome, observation time, and unknown availability/units so a later Dataset publisher can cite the original response and combine field-level T02-meta evidence.

The production HTTP adapter is exercised only through the shared transport. The checked-in tests use synthetic transport replies and cover stocks/ETFs, repeat/revision, empty/error/retry, sequential/concurrent/cross-process request pacing, bounded exponential backoff, terminal BodyRead diagnostics, source-bounded chunking, stop-on-restriction, and hash-free CLI errors with local diagnostics. No live collection, source reachability, or real-market coverage is claimed by these tests. This command does not publish a normalized Dataset, perform adjustment, derive corporate actions, schedule collection, expose a task API, or serve a page.

## Fixed security-evidence snapshots

`SecurityEvidenceStore::select_snapshot` takes explicit `(InstrumentId, SecurityEvidenceVersionId)` pairs, reads those exact versions, and writes a content-addressed snapshot below `<root>/security-evidence-snapshots/`. The snapshot embeds records, version ids, and the publication gate; identical sets are idempotent, revisions create a new id, old ids stay readable, and there is no mutable latest pointer. The gate requires Known positive `lot_size`, `price_increment`, and `size_increment`; Unknown, Conflict, NotApplicable, and non-positive terms stay explicit `PublicationIssue`s without defaults, so a blocked instrument is not constructed as an executable `InstrumentSpec` or published Dataset. Lifecycle evidence remains separate from market coverage and `observed_at`; no tradable window or `available_at` is derived.

Select with `prajna-select-security-evidence --root <LOCAL> --select <INSTRUMENT_ID>=<sha256:EVIDENCE_VERSION> [...]` and read an old selection with `--read <sha256:SNAPSHOT_ID>`. The CLI prints the snapshot id, counts, and blocked field/code reasons. Real evidence and detailed identities remain in the configured local root.

## Independent session calendar

The local import command and fixed-version Rust read API for XSHG/XSHE are documented in the [session calendar input contract](../../docs/contracts/shanghai-shenzhen-session-calendar.md). This input is separate from market Raw; its tests use synthetic calendars only.

## Versioned Shanghai/Shenzhen stock/ETF daily normalizer

`cn-stock-etf-daily` v1 is constructed with one explicit #328 calendar version and one explicit #329 security-evidence snapshot id via `NormalizerRegistry::with_cn_stock_etf_daily_v1`. The Raw set must contain those byte-exact versions plus Tencent unadjusted daily responses with a pinned `tencent-fqkline-day/1` source record. Prices stay exact scale-18 decimals, Equity/ETF volume is declared lots × 100, amount stays `null` because the source has no amount field, and `available_at` stays Unknown. Missing quotes stay missing; Sessions are never deleted; identical duplicates deduplicate and contradictory duplicates fail closed. Later appended source records or a revised evidence/calendar version cannot change an already published Dataset Version. The existing MVP-6 SZSE Normalizers and identities are untouched.

Opt-in `cn-stock-etf-daily` v2 imports a fixed, versioned opening-evidence Raw snapshot and its referenced originals, publishes complete scoped D7 status rows, and preserves missing/unknown/late evidence gates. Snapshot revisions change Dataset identity; v1 stays frozen. Synthetic production publisher → Experiment CLI → persisted Standard results are verified; real state-source correctness/coverage remains unverified. See [opening-state contract](../../docs/contracts/shanghai-shenzhen-daily-normalizer.md#opt-in-v2-opening-state-evidence).

The local `prajna-publish-daily` command assembles explicit collection-report/Raw selections with pinned calendar/security versions, publishes one security per Dataset, and saves frozen rebuild receipts. CLI-to-Dataset tests are synthetic only; real coverage, adjustment and corporate actions remain unverified. Commands, selection JSON and failure/retry semantics: [Shanghai/Shenzhen daily normalizer](../../docs/contracts/shanghai-shenzhen-daily-normalizer.md).

## Read-only Dataset preview

`prajna-preview-dataset` lists every committed Dataset Version in one explicit
local root and reads one version's instruments, raw unadjusted daily Bars, frozen
security-evidence lifecycle fields, actual Session/Bar coverage, gaps and Bar
sources. It never normalizes, publishes, collects, downloads or writes a Dataset,
Raw object, receipt or the MVP-6 archive; a missing root is an error rather than a
new directory. Catalog entries are discovered from verified manifests with no
mutable "latest" pointer; the required instruments, sessions and bars tables are
read and row-count checked before listing, and invalid manifests or unreadable
tables are reported separately so they cannot masquerade as coverage.

```sh
prajna-preview-dataset catalog --root <LOCAL>
prajna-preview-dataset show --root <LOCAL> --dsv <DSV>
prajna-preview-dataset bars --root <LOCAL> --dsv <DSV> --instrument <ID> \
  [--start YYYY-MM-DD] [--end YYYY-MM-DD] [--limit 1..=1000] [--offset N] \
  [--adjusted-version <ADJ_VERSION>]
prajna-preview-dataset actions-catalog --root <LOCAL>
prajna-preview-dataset actions --root <LOCAL> --version <CAF_VERSION> \
  --instrument <ID> --start YYYY-MM-DD --end YYYY-MM-DD
```

`actions-catalog` lists every persisted corporate-action fact version below
`<LOCAL>/corporate-facts` with its declared scopes and four-state labels, and
isolates invalid versions; `actions` passes one fixed version, security and
closed range through the #360 `query` reader. Neither mode publishes, downloads,
recalculates, chooses a latest version or merges partial ranges into coverage.

Stdout is one JSON envelope (`schema_version` plus `data` or `error`); invalid
input or not-found exits 2, I/O or integrity failures exit 1. Unknown and missing
facts stay explicit: amount/available_at remain null with Unknown semantics,
lifecycle without a committed evidence snapshot stays Unknown, and Sessions in
the observed Bar range without a Bar are listed as gaps rather than halts or zero
Bars. Tests publish synthetic XSHG/XSHE stock and ETF Datasets through the
production publisher and cover revisions, invalid manifests, missing tables,
an empty committed Dataset, pagination and read-only behavior; no real data is
validated. Full fields,
limits and compatibility: [Dataset preview contract](../../docs/contracts/dataset-preview-read.md).
The local service exposes this reader at `/api/v1/datasets` and `/datasets`
with an explicit `serve --dataset-root`; configuration and synthetic checks are
in [the API guide](../../docs/api.md#已发布研究-dataset-预览). The same page
exposes the read-only `/api/v1/corporate-actions` view over persisted fact
versions; missing facts never hide Dataset or factor browsing.

## Supplier-adjusted research prices

`prajna-import-adjusted --root <LOCAL_LAKE> --input <SUPPLIER_JSON>` preserves an
explicit supplier file as an immutable version linked to an exact raw DSV. Rust
fixed-version reads and `/datasets` expose OHLC, method/source/assumptions and
Session gaps; raw Bars remain execution/valuation prices. Each frozen object
remains bound to its exact raw DSV; deleting that raw Dataset makes the object
unreadable rather than rebinding it. This is retrospective research input, not
strict PIT, sequential TRI or dividend cash flow. Synthetic CLI/golden/browser
evidence only. Schema, commands and limits: [contract](../../docs/contracts/adjusted-research-series.md).

## Offline corporate-action facts

`prajna-corporate-facts import/read` publishes and reads fixed Shanghai/Shenzhen stock/ETF fact versions in an explicit local root. Four scoped states preserve missing amounts/dates/source/history availability; price research stays independent. Commands, fields, scope semantics and compatibility: [ADR 0032](../../docs/decisions/0032-cn-corporate-action-fact-versions.md). The read-only coverage view is delivered through `/datasets`; synthetic tests only, and ledger integration remains separate.
