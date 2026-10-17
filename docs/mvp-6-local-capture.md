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
status, byte length, and SHA-256. Failed response objects are retained for
audit and are excluded from DSV inputs.
Freeze, Sina, and cninfo serialize coverage-report updates through the local
`mvp6-coverage.lock`; freeze reloads the latest report while holding that lock
before preserving collector coverage and writing its import results.

The command intentionally fails closed when a requested SZSE session has no
validated TDX package, a month is incomplete, response bytes do not match the
capture log, or the captured open-date list disagrees with the monthly source.
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
requests and the collector's retry/stop rules, and writes responses only to
the local Raw Store. If access control is returned, it stops the source and
records the unresolved symbol in the local acceptance report.

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

The coverage report must keep P21 comparisons and unknown rule sessions marked
unresolved until the corresponding source facts and rule evidence have been
checked. Publishing the v4 market DSV alone is not full MVP-6 acceptance.
