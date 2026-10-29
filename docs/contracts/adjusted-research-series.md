# Supplier-adjusted research-price input

`prajna-import-adjusted` imports an explicit local supplier file into the same
independent lake as its committed `cn-stock-etf-daily` raw Dataset. It does not
fetch, compute adjustments, publish raw Bars, derive corporate actions or book
cash. [ADR 0030](../decisions/0030-supplier-adjusted-research-series.md) fixes the
price roles and compatibility boundary.

```sh
prajna-import-adjusted --root <LOCAL_LAKE> --input <SUPPLIER_JSON>
prajna-preview-dataset show --root <LOCAL_LAKE> --dsv <RAW_DSV>
prajna-preview-dataset bars --root <LOCAL_LAKE> --dsv <RAW_DSV> \
  --instrument <INSTRUMENT_ID> --adjusted-version <ADJ_VERSION> \
  --start 2026-01-05 --end 2026-01-09 --limit 100 --offset 0
```

The import schema is closed. The example is completely synthetic; replace the
raw DSV with an existing committed local Dataset for that security:

```json
{
  "schema_version": 1,
  "raw_dsv": "dsv:sha256:<64 lowercase hex>",
  "instrument_id": "510300.XSHG",
  "method": "forward",
  "method_version": "synthetic-half@1",
  "source": {
    "source_id": "synthetic-supplier",
    "source_version": "1",
    "source_ref": "synthetic://source-file"
  },
  "observed_at": "2026-10-28T00:00:00Z",
  "retrospective_assumption": "Supplier may revise history after later events; not strict PIT",
  "close_availability_assumption": "Assume domestic daily close known after Session close",
  "start": "2026-01-05",
  "end": "2026-01-09",
  "rows": [
    {"trade_date":"2026-01-05","open":"4.95","high":"5.1","low":"4.9","close":"5"}
  ]
}
```

Methods are `forward` and `backward`; method version must describe the supplier's
actual rule. This field records that rule, not independent proof of its math or
source completeness. Source id/version/reference and both assumptions must be
nonempty; observed time must be RFC3339. Source reference identifies the operator's
local source evidence; no upstream availability or fetch success is inferred.
The imported JSON itself is preserved byte-for-byte, including its provenance.

`raw_dsv` must be the full exact immutable DSV. The security must belong to that
Dataset. The inclusive declared range stays within its venue's Session axis.
Rows may be empty or partial, but duplicates, dates outside the range, missing raw
Bar/Session, numeric JSON prices, invalid decimals, nonpositive prices and invalid
OHLC ordering fail. Decimal strings use Domain scale-18 precision; no float passes
through the browser. Read output is sorted by date and uses canonical decimal
strings. Volume/amount are absent from adjusted prices, never copied or adjusted.

The version is SHA-256 of the exact imported file bytes, prefixed `adj:sha256:`.
It commits every input field and row. Whitespace/order changes can create a new
version; repeating identical bytes is a no-op. Objects are atomically installed
without replacement at `adjusted-series/<hex>.json`. Revisions coexist with old
objects. A corrupt same-id file fails rather than being repaired by import.
No raw Dataset/manifest/table is overwritten. Readers verify the object hash and
revalidate the committed raw Dataset, security and dates. The Rust production
entry `adjusted_series::read(root, version)` returns validated `AdjustedInput`;
research callers freeze both identities and get errors for missing/corrupt input.
No API silently chooses a latest or substitutes another raw/adjusted version.

`show` adds `adjusted_series.versions` for that raw DSV with method/source,
assumptions and coverage; unreadable objects are visible in `invalid_versions`
(root-wide because their raw identity cannot be trusted). Coverage reports actual
row count/first/last and all missing Sessions in the declared range, including
missing endpoints; gap dates are capped at 200 with total count and truncation.
Missing data does not become a zero Bar or an implicit halt. `available_at` stays
null; local observation time is never historical publication time.

The local API adds the optional `adjusted_version` bars query parameter, forwarded
as `--adjusted-version` across the existing subprocess boundary. Omitting it keeps
the existing raw response. `/datasets` lets users select a specific series version,
inspect its source/assumptions/gaps, and switch back to raw prices. It displays empty
and corrupt versions honestly. Decimal strings and warnings come from Rust.
The schema-v1 preview envelope is retained with additive detail fields.

Limits: all tables/files are loaded in memory; catalog validates each adjusted
object, so large version catalogs incur repeated reads. There is no adjustment
collector, strict historical PIT claim, sequential total-return index, cash-flow
inference, task/Factor integration or real-market validation in this ticket.
Supplier revisions must be imported explicitly and selected by a new frozen
research definition. True source/raw documents and production details stay local.
