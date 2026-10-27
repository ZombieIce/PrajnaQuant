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

The tests are synthetic only and cover XSHG/XSHE stock/ETF publication, Dataset read/rebuild, pinned-version isolation, invalid/out-of-range/inexact decimals, invalid OHLC, negative volume, missing dates and duplicate conflicts. No production collection-report assembly, real coverage, adjustment or corporate action is included. Contract: [Shanghai/Shenzhen daily normalizer](../../docs/contracts/shanghai-shenzhen-daily-normalizer.md).
