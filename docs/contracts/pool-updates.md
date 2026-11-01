# Fixed-pool manual updates

The local `/pools` workbench updates an explicitly published pool version through
the existing serial task queue and `prajna-pool-update`. Rust owns collection,
publication, input validation, coverage and readiness. Authority:
[#313 decision](https://github.com/ZombieIce/PrajnaQuant/issues/313#issuecomment-6553232202)
and [ADR 0038](../decisions/0038-fixed-pool-production-update.md).

## Local input configuration

Use the same independent local root for `serve --dataset-root` and `--output`.
Set `PRAJNA_POOL_UPDATE_CLI` to the built executable (default: PATH; the task worker
also resolves the sibling of `PRAJNA_EXPERIMENT_CLI`). No new data supplier,
adjustment calculation, scheduler or generic task retry is introduced.

Create `pool-updates/configs/<name>.json` under that root. Names contain only ASCII
letters, digits and hyphens, at most 80 characters. Every fixed pool member must
have an entry. The following is a **shape example**, not usable market evidence:

```json
{
  "schema_version": 1,
  "pool": {"id": "bonds", "version": 2},
  "start": "2026-01-05",
  "end": "2026-01-07",
  "members": {
    "510300.XSHG": {
      "collect": true,
      "publication": {
        "raw_root": "/local/raw",
        "calendar_root": "/local/calendar",
        "evidence_root": "/local/evidence",
        "report": "",
        "normalizer_id": "cn-stock-etf-daily",
        "normalizer_version": "2",
        "calendar_version": "sha256:<explicit>",
        "security_snapshot": "sha256:<explicit>",
        "status_snapshot": "sha256:<explicit>",
        "raw_sha256s": []
      },
      "adjusted_file": "/local/supplier/510300.json"
    }
  },
  "actions_file": "/local/supplier/actions.json",
  "actions_version": null
}
```

The example assumes that published version contains only the shown member;
multi-member pools require **all** entries. `collect: true` uses the existing
Tencent collector for exactly that member/date range, with its source restrictions,
backoff and one-second request spacing. An access-control/source-stop response
prevents remaining network collections in that attempt; it is not retried through
another supplier. `collect: false` explicitly consumes the supplied existing
collector report. This is labelled `explicit_report`, never a fresh network
success. Selected accepted Raw hashes come from the report, not unchecked config
hashes. The report must match the member, ETF kind and exact requested range.

Calendar, security and optional opening-state versions remain explicit; v1 allows
raw research publication without opening evidence, v2 validates the exact pinned
opening snapshot and its market Raw scope. An old status snapshot cannot be
silently rebound to revised market Raw. Missing/incorrect evidence fails the
affected publication; supply a correct explicit new plan when these fixed inputs
change. No status is inferred from Bars. A single shared calendar version is
required, and used Venues must have identical Session dates/open/close instants.

## Supplier prices and action inputs

`adjusted_file` accepts a complete existing `AdjustedInput` whose `raw_dsv`
**already matches** the published pool DSV, or this minimal new source format:
use the [AdjustedInput fields](adjusted-research-series.md), omit `raw_dsv`, set
`schema_version` to `"supplier-adjusted/1"` and add
`"price_unit": "CNY_per_fund_unit"`. Exact decimal string OHLC, method, version,
source, dates and availability/retrospective assumptions are still mandatory.
Unknown units, fields or ambiguous shapes fail; no values are calculated.

Preparation freezes the exact source file bytes in Raw, including errors for
missing files. After committing the new DSV, the adapter constructs a **new**
version-1 import request with that DSV and passes the existing full validation and
immutable importer. It stores a `supplier-adapter/1` import record linking source
Raw, DSV, member, unit and imported version. It never edits an existing adjusted
object or rewrites an explicitly supplied `raw_dsv`. Changing a source file after
submission cannot alter that attempt. Complete price readiness requires actual
adjusted rows for every requested Session per member, not only declared endpoints;
partial imports remain immutable and visible without claiming readiness.

Choose either `actions_file` (the existing structured fact schema) or
`actions_version` (a fixed stored fact version), or neither. Both is an error.
Queries use each member and the exact requested interval; existing covered,
no_events, missing_facts and unknown semantics are unchanged. Coverage cannot be
extended by an update. `actions.complete` is a fact-scope statement, not an engine
approval. `actions.total_return` is `incomplete_facts`, `unsupported_mixed_venue`,
or `requires_strategy_preflight`. Unknown action inputs do not block price factors.

## Plans, attempts and retry

```sh
echo '{"op":"list"}' | prajna-pool-update --lake <LOCAL>
echo '{"op":"prepare","config":"my-pool"}' | prajna-pool-update --lake <LOCAL>
echo '{"op":"pool_update","attempt":"<prepared-id>"}' | prajna-pool-update --lake <LOCAL>
echo '{"op":"retry","attempt":"<finished-id>"}' | prajna-pool-update --lake <LOCAL>
```

CLI envelopes are `{schema_version:1,data:...}` (exit 0) or
`{schema_version:1,error:{code:"pool_update_failed",message}}` (exit 2).
Output is local evidence and must not be copied from real runs into chat/CI/PRs.
`--input <file>` supplies the same bounded JSON command for queued execution.

`plans/<hash>.json` fixes configuration, source locations, pool Universe and
versions; hash validation detects changes. `attempt-inputs/<id>.json` freezes
that attempt's supplied files and explicit reused results. `attempts/<id>.json`
is atomically updated progress. An exclusive attempt lock prevents concurrent
execution/retry. Completed attempts are returned unchanged; an interrupted attempt
is retried as a **new** attempt. Each successful member records its receipt,
actual coverage and success time. Partial collections retain Raw/partial
publication, but do not advance member success or complete the pool.

Retry follows the original plan, snapshots newly supplied files, and explicitly
reuses only its previous successful members. Each reused receipt is rebuilt and
checked against member, dates, calendar/security/status and Normalizer versions.
No global latest selection, missing-member removal or implicit old-DSV substitution
is permitted. Changing fixed scope means a new configuration/plan, not retry.
Failed or incomplete attempts preserve earlier ready records and datasets.

No worker waits for the user to provide files. The task can finish `succeeded`
while its persisted domain result is `needs_input`; the workbench distinguishes
execution, raw publication, price readiness, action coverage and total-return
applicability. Its latest success displays are derived in Rust from stored
successful attempts, never from the requested end date.

## HTTP and browser

`GET /api/v1/pool-updates` reads configurations, attempts, last price/member
successes and associated task states. `POST` accepts only `prepare` and `retry`.
The server freezes inputs then submits `prajna_pool_update@1` to the existing FIFO
worker/process-group boundary. Direct submission of this type through the legacy
permissive task POST is rejected. Local Host/Origin checks and `no-store` apply.
Bad operation/body is 400, an active retry 409, CLI input rejection 422,
unconfigured/mismatched root or unavailable CLI 503, protocol failure 502.

Unfinished tasks recover as `interrupted`. Retry refuses a currently queued or
running task; after interruption/failure (or preparation that could not enqueue),
the server explicitly reconciles the unfinished attempt before creating its
successor. Direct CLI callers may pass `"interrupted":true` only after confirming
the prior task stopped; the attempt lock still refuses a live worker.

The page lists configurations for the exact selected pool version. Selecting a
ready result is explicit and refreshes the Dataset catalog, then binds that
result's DSV and adjustments for the existing pool preflight/freeze workflow.
Old tasks/definitions/results do not follow this selection or later pool edits.

## Production pool publication

`prajna-publish-daily pool --root <LOCAL> --selection <JSON> --receipt <RELATIVE>
--diagnostics <LOCAL>` accepts `{schema_version:1,members:{<instrument>:<receipt>}}`.
Each receipt is an existing single-security `recipe_sha256,dsv` pair in the same
lake. `rebuild-pool` takes the receipt without `--selection`.

The new `cn-stock-etf-daily@3` pool Normalizer re-runs each pinned v1/v2 Normalizer
on its exact Raw set, preserves real Instrument/Session/status and source data,
and produces one ordinary atomic Dataset manifest. Inputs include the selected
member recipes/reports, their Raw dependencies, combined validated security
snapshot and pool recipe. Duplicate/conflicting members, provenance or Session
axes fail. Missing Bars/status remain missing/unknown. It does not copy published
Parquet or fabricate a new DSV over old tables. Rebuild uses frozen inputs only;
single-security v1/v2 identities, receipts and rebuild semantics remain unchanged.

Validation uses synthetic production CLI and browser paths, two true Venue
identities, independent cash/equity constants, negative inputs, explicit retries
and old frozen results. It does not establish real-market coverage, automatic
supplier-price/action acquisition, full-history PIT, scheduled updates, or
MVP-7 final acceptance.
