# MVP-6 local capture and freeze workflow

Issue #206 keeps response bodies, derived files, hashes, request inventories and
coverage reports under `~/PrajnaData/mvp-6/`. None of these files belong in the
repository, a PR, or CI artifacts.

## Import the existing market captures

`prajna-mvp6-freeze import-existing` performs no network requests. It reads the
TDX and SZSE request/validation logs and matching response bodies, verifies the
logged byte lengths and SHA-256 values, preserves successful and failed
responses in the local Raw Store, then publishes `szse-etf-frozen@4` only when
the requested open sessions, complete calendar months, and local open-date
inventory agree.
Validation booleans written by Python's CSV writer are accepted, and captured
`.partial` response bodies are retained as failed Raw objects rather than DSV
inputs.

Historical failed-attempt provenance is separate from successful-input coverage
(#206 owner-approved continuation policy). Explicit transport failures with no
HTTP status retain a null status rather than a fabricated HTTP response code.
Missing or malformed failed-attempt metadata and unavailable failed bodies stay
in `historical_capture_issues` with `historical_capture_status=unresolved`; the
original CSV rows remain in the Raw inventory and are never rewritten.
Recoverable failed bodies are still verified by length and SHA-256 before being
stored. For both ordinary request rows and rows carrying a logged request
error, HTTP-200 bodies that fail ZIP integrity checks are retained as failed
Raw with a separate `validation_error`; the captured error and HTTP status are
preserved without inventing a transport error.
These historical failures never enter Dataset inputs and do not block a
validated successful retry. Every requested session must still have a validated
successful package; invalid successful provenance, missing validation for a
readable archive, wrong embedded sessions and conflicting successful revisions
remain publication blockers. A corrupt archive whose validation log claims
success is a blocking integrity contradiction, not a quarantinable historical
failure. Publication does not resolve historical audit gaps
or establish complete #206 acceptance.

```sh
cargo run -p prajna-data --bin prajna-mvp6-freeze -- import-existing \
  --root "$HOME/PrajnaData/mvp-6" \
  --start 2022-10-01 \
  --end 2026-09-30
```

The default window is the specification window. A partial window can publish a
diagnostic v4 dataset for local repair, but the report marks
`spec_window_complete=false` and the candidate exporter refuses it. It cannot
be used to complete MVP-6. The local
`acceptance/mvp6-coverage.json` records `import_status`, the last validation stage,
source status, and unresolved gaps; the adjacent
`mvp6-raw-inventory.json` preserves the request URL, query, observation time,
status, byte length, and SHA-256. A failed import keeps every previously
inventoried request record and appends only new ones, so earlier Raw objects
stay listed. Failed response objects are retained for
audit and are excluded from DSV inputs.
Freeze, Sina, and cninfo serialize coverage-report updates through the local
`mvp6-coverage.lock`; freeze reloads the latest report while holding that lock
before preserving collector coverage and writing its import results.

The command intentionally fails closed when a requested SZSE session has no
validated TDX package, a month is incomplete, response bytes do not match the
successful capture log, or the captured open-date list disagrees with the monthly source.
It does not infer that a missing TDX row means a halt.

## Prepare historical candidate files

After a v4 DSV is published, export the union of its daily TDX code tables for
the local cninfo and Sina collectors:

```sh
cargo run -p prajna-data --bin prajna-mvp6-export-codes -- \
  "$HOME/PrajnaData/mvp-6"
```

This writes `acceptance/mvp6-candidate-codes.txt` and
`acceptance/mvp6-sina-symbols.txt` locally. The candidate set comes from the
frozen TDX code-table snapshots, so codes that later disappear remain in the
historical collection plan.

## Collect Sina factors

The Sina command requires an operator-supplied description of the confirmed
direct domestic egress path. It disables proxies and redirects, uses serial
requests with the existing one-second minimum spacing, retry/backoff and
stop rules, and writes every response (including failures) only to the local
Raw Store. A deterministic 404, an invalid/empty HTTP 200 response, or a
qfq/hfq mismatch records a reason for that symbol and continues with the next
candidate. Redirects, access-control/captcha/login responses, and exhausted
transport retries stop the source.

`acceptance/sina/collection.json` records one terminal result per candidate:
`completed`, `gap` with its reason, or `not_attempted` with the source-stop
reason. The coverage report distinguishes all pairs captured
(`captured_raw_pairs_not_normalized`), per-symbol gaps
(`captured_with_symbol_gaps`), and a source stop
(`source_stopped_unresolved`). These are collection outcomes only; they do not
claim factor normalization, real-source completeness, or successful MVP-6
acceptance.

```sh
cargo run -p prajna-data --bin prajna-mvp6-collect-sina -- \
  "$HOME/PrajnaData/mvp-6" 2022-10-01 2026-09-30 \
  "<operator-confirmed direct domestic egress description>"
```

Collected qfq/hfq Raw pairs are recorded locally. This command does not publish
`szse-etf-frozen@5`: v5 also requires validated Catalog lifecycle ranges and
structured cninfo corporate-action inputs.

## Collect cninfo and Tencent fallback

Use `prajna-cninfo` with the exported candidate code file. It captures discovery,
per-code and per-orgId searches plus relevant category PDFs. Pagination,
missing orgIds, missing legal names, PDF extraction gaps and unknown facts stay
in the local cninfo acceptance artifacts. Pass the operator-confirmed egress
description as the final argument. A missing orgId is not repaired by guessing.

```sh
cargo run -p prajna-data --bin prajna-cninfo -- 2022-10-01 2026-09-30 \
  "$HOME/PrajnaData/mvp-6/acceptance/mvp6-candidate-codes.txt" \
  "<operator-confirmed direct domestic egress description>"
```

`scripts/collect_tencent_fallback.py` accepts only an explicit local plan whose
entries state that the TDX observation is `missing`. Do not include a keyed TDX
no-trade observation in that plan. The tool requires the confirmed direct
domestic network path and keeps its JSONL request log and response bytes local.

## Validate public facts and cross-check v4

Put the three public fact tables and the citation manifest under the local data
root for validation. Only the three public fact tables may later be committed
under `configs/mvp-6/`; `citations.csv` (it carries Raw hashes and excerpts),
the candidate CSV, and every response or PDF stay local.

```text
~/PrajnaData/mvp-6/facts/identity.csv
~/PrajnaData/mvp-6/facts/actions.csv
~/PrajnaData/mvp-6/facts/events.csv
~/PrajnaData/mvp-6/facts/citations.csv
```

`citations.csv` has one row per fact field:
`fact_source,field,announcement_id,page,raw_sha256,excerpt,verified_by,verified_at`.
`fact_source` is the fact row's `source_announcement_id`; `announcement_id`
is the frozen document that states the field and may differ from it, for
example when `code`/`org_id` come from the frozen index response and the
listing date from the listing-notice PDF. `excerpt` is verbatim text from the
cited page; whitespace is ignored when matching. A citation is accepted only
when:

- the Raw object exists with that hash and its bytes or request provenance
  reference `announcement_id`; collected MVP-6 PDFs record that binding in
  their frozen provenance;
- the page exists in the frozen document (PDF pages come from the local text
  converter; a non-PDF Raw is page 1) and the excerpt occurs on that page;
- `verified_by` names the operator and `verified_at` is an RFC 3339 time that
  does not precede the first observed capture of that Raw; re-capturing
  identical bytes later does not move the freeze time;
- the recorded field value is recoverable from the excerpt: dates in ISO,
  Arabic-Chinese or Chinese-numeral form (`二〇二六年一月五日`), decimals as an
  equal number, other text literally. `cash_per_share` is per share even when
  the notice states `元/10份基金份额`: the number must follow that explicit
  unit on the page and equal ten times the recorded value, and no other unit
  conversion is inferred. `kind` must
  be an announcement category that the excerpt classifies into (`split` and
  `conversion` share the 拆分/折算 category). `fund_category` and
  `session_part` are not literal source text: their excerpt must still be on
  the page, and the named verifier attests the mapping.

`announcement_date` is a publication date and may be a weekend or holiday
inside the window; `record_date`, `ex_date`, `pay_date` and `effective_date`
must be frozen sessions. `listing_date` and `termination_date` may fall outside
the market-data window because the fixed list includes funds listed years
before it; a boundary inside the window that is not a session still fails.

Every populated fact field needs its own citation. A duplicate field, an
unknown field, a citation for a field the row leaves empty, or a row whose
`source_page` is not bound to one of its citations fails validation. PDF
citations require `pdftotext`, the same converter the collector uses. Build
hashes from the `source_raw_sha256` or `pdf_raw_sha256_by_announcement` values
in the local cninfo `collection.json`.

```sh
cargo run -p prajna-data --bin prajna-cninfo-fixed --locked --offline -- \
  /absolute/local/path/fixed-candidates.csv \
  "<operator-confirmed direct domestic egress description>" \
  "fund_contract,prospectus"
```

When `facts/` exists, the command reads the published v4 DSV from
`acceptance/mvp6-coverage.json`, loads the `sessions`, `bars`, `bar_sources` and
`tdx_code_table` tables, validates the fact tables, and writes
`facts-session.json` plus the local-only `crosscheck.json` next to
`collection.json`. A code-table observation inside any fact lifecycle with no
Bar is treated as a keyed no-trade observation; dates outside all lifecycles
for that code are not invented. The cross-check compares listing and termination
with traded Bars, action ex-dates with `bar_sources.reference_price` against the
previous traded close at the 0.001 tick, and suspension facts with keyed
no-trade observations. Each comparison is scoped to the identity lifecycle that
owns the date: a reused fund code's earlier Bars never move the next lifecycle's
listing/termination boundary and never supply its prior-close baseline. The
cross-check never edits the facts or the v4 Dataset.

The coverage report must keep P21 comparisons and unknown rule sessions marked
unresolved until the corresponding source facts and rule evidence have been
checked. Publishing the v4 market DSV alone is not full MVP-6 acceptance.
