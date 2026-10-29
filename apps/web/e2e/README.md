# Synthetic production browser E2E

Use Node 20+ (Playwright ESM requires at least 18.19), npm, the workspace Rust
stable toolchain and its offline Cargo dependencies. The service uses the default
`quant-research` `app` feature, including bundled DuckDB: allow enough build time
and disk space (keep the repository's 10 GiB reserve). No real data or collector
network access is needed. Install dependencies/browser once with network access:

```sh
cd apps/web
npm ci
npx playwright install chromium
npm run test:e2e:check
npm run test:e2e
```

An installed Chromium/Chrome may be selected explicitly instead of downloading:

```sh
PRAJNA_CHROME_EXECUTABLE='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' npm run test:e2e
```

`test:e2e` creates a fresh owned temporary directory and prints its location.
The suite starts real T01 experiment tasks, so the host must allow `ps`
process-group inspection. In a sandbox that denies `ps`, every experiment task
fails with `process_group_ownership_unverified` because the service refuses
unverified cleanup; run the suite from a normal local shell with full process
visibility instead of relaxing that production check.
`CARGO_TARGET_DIR` may select a task-owned cache; default is repository `target/`.
`PRAJNA_E2E_PORT` selects a loopback port (default 18789). An occupied port fails;
existing servers/lakes are never reused. Direct `playwright test` is only supported
for listing; execute tests through the npm wrapper so an isolated root exists.
Arguments pass through, for example `npm run test:e2e -- --grep 'corrupt version'`.

`test:e2e:check` typechecks the configuration/specs and checks test discovery.

The Playwright webServer performs these steps, all from this checkout:

1. `npm run build` produces the production `apps/web/dist`.
2. Build `quant-research`, `prajna-experiment` (including
   `prajna-freeze-selection` and the `selection_fixture` example) and
   `prajna-publish-daily` with `--locked --offline`.
3. Run `prajna-data`'s opt-in `export_synthetic_preview_fixture` integration test.
   This invokes the real `prajna-publish-daily` CLI with synthetic source bytes,
   pinned calendars and security evidence, publishing an XSHG ETF and XSHE stock.
   The fixture has Known/Unknown lifecycle dates, two Bars and deliberate gaps.
   Cargo builds the real Dataset reader/publisher binaries for the integration test.
4. Run the `selection_fixture` example against that same lake. It publishes two
   #350 `cn-stock-etf-daily@2` status Datasets through the real publisher, so the
   lake now holds four Datasets: two v1 preview fixtures and two v2 selection
   fixtures. The v2 XSHG dataset executes; the v1 XSHG dataset stays as a real
   missing-`execution_status` failure case.
5. Check the real `prajna-experiment --help`, bind it through
   `PRAJNA_EXPERIMENT_CLI`, bind `PRAJNA_DATASET_PREVIEW_CLI` and
   `PRAJNA_SELECTION_CLI`, and launch `quant-research serve` with explicit dist,
   market directory and one synthetic lake used as both `--dataset-root` and
   `--output` (T04b rejects different canonical roots).
6. Wait for real `/api/v1/health`, then operate `/datasets` and `/backtests` in
   Chromium. Requests go to production API → CLI → published Parquet/manifest and
   persisted task/result files. No success responses or production endpoints are
   mocked or added for testing.

`datasets.spec.ts` verifies selection, lifecycle, coverage gaps, exact raw prices,
an empty Bar range, refresh, corrupted version isolation, empty catalog and missing
lake. Its option-count assertions include the two extra v2 Datasets and its loops
filter the v1 preview fixtures by `1 证券 / 2 Bars`; it does not rely on the lake
containing only the preview fixtures.

`backtests.spec.ts` operates the production `/backtests` page: choose the v2 XSHG
ETF, submit through `POST /api/v1/tasks/selections`, observe the persisted task
become `succeeded`, read `GET /api/v1/tasks/experiments/{id}/result`, and assert
the rendered valuation rows equal the persisted result rows and the rendered
total-return metric equals the persisted Summary `total_return` (including the
hand-calculated 99,900 units, cash 500.44006, close equity 1,009,490.44006 and
0.95% total return); the other rendered Summary metrics are not cross-checked by
the spec. It then reloads and reopens the task from the recent list,
corrupts the saved `account_valuations.parquet` to prove a failed result read is
visible and restores it, and submits the v1 XSHG ETF to prove a real missing
`execution_status` task failure is shown with its persisted error code and no
result values. A separate legacy check keeps `GET /api/v1/runs` and the old
diagnostic panel working.

Fault cases mutate only the invocation's synthetic lake and restore it in
`finally`; one worker and no parallel tests prevent fixture races. Preparation,
server, assertion and browser-launch failures produce a nonzero exit, never a skip.
Playwright tears down the owned server process tree after success or failure.

`server.log`, failure screenshots/traces and browser diagnostics remain in the
printed local temporary directory. Inspect locally (for example with
`npx playwright show-trace /absolute/path/to/trace.zip`); remove only your known
owned directory after investigation. Artifacts are not attached to GitHub or
committed. These are synthetic software checks, not real market evidence, PIT
validation, strategy performance or a general backtest product loop. Task and
result identities printed in local logs must not be copied into GitHub or chat.

For later page tickets, add `*.spec.ts` under `e2e/`, reuse this server lifecycle,
and extend committed synthetic fixture inputs through existing production CLI
entry points as required. Fix each task's selected DSV, Universe/definition and
output root explicitly; never pick a user's lake or fall back to real downloads.
Assert persisted API values and visible page outcomes; do not substitute helper
reports or mocked success for the product path. Keep numerical assertions at Rust
seams, and state synthetic versus local real-data validation separately. If a new
page needs mutable shared state, restore it or give it a separate fixture root;
keep one worker until fixture ownership supports parallelism. Any new
output-producing page must use the same `--dataset-root`/`--output` lake as
`/backtests`, or start its own server, because T04b rejects unequal roots; do not
weaken that production check for tests.

`adjusted.spec.ts` reuses the production publisher/exporter plus the real
`prajna-import-adjusted` CLI. It checks pinned revisions, source/assumptions,
missing endpoints, empty input, raw-price switching and a real corrupt-file API
error. The exporter imports only synthetic supplier JSON. Rust integration tests
separately check literal hand-calculated OHLC; browser checks are transport/display
evidence. Existing Node must support the Playwright ESM config (18.19+); use a
newer local runtime and `PRAJNA_CHROME_EXECUTABLE` when needed.
