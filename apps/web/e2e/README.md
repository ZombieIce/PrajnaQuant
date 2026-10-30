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
   `prajna-freeze-selection` and the `selection_fixture` example),
   `prajna-publish-daily` and `prajna-corporate-facts` with
   `--locked --offline`.
3. Run `prajna-data`'s opt-in `export_synthetic_preview_fixture` integration test.
   This invokes the real `prajna-publish-daily` CLI with synthetic source bytes,
   pinned calendars and security evidence, publishing an XSHG ETF and XSHE stock.
   The fixture has Known/Unknown lifecycle dates, two Bars and deliberate gaps.
   Cargo builds the real Dataset reader/publisher binaries for the integration test.
4. Run the `selection_fixture` example against that same lake. It publishes two
   #350 `cn-stock-etf-daily@2` status Datasets through the real publisher, so the
   lake now holds four Datasets: two v1 preview fixtures and two v2 selection
   fixtures. The v2 XSHG dataset executes; the v1 XSHG dataset stays as a real
   missing-`execution_status` failure case. The ignored `export_synthetic_pool_fixture`
   then publishes two multi-Venue Datasets through the real `publish_dataset` path
   with a test-only wrapper around the production v1 Normalizer: one executable
   XSHG+XSHE fixture (synthetic TRADABLE `execution_status`) and one with an XSHE
   Session missing from both its calendar and source rows for the structural
   shared-axis failure case. Neither is a second production publication path.
5. Run `prajna-data`'s opt-in `export_synthetic_corporate_facts_fixture`
   integration test and publish its two synthetic inputs (an original and a
   revision) with the real `prajna-corporate-facts import`, so the same lake also
   holds two fixed fact versions. The fixture covers XSHG/XSHE stock and ETF
   securities and all four scoped states, including a dividend with an exact
   amount, a split, and an event missing its amount and availability.
6. Check the real `prajna-experiment --help`, bind it through
   `PRAJNA_EXPERIMENT_CLI`, bind `PRAJNA_DATASET_PREVIEW_CLI` and
   `PRAJNA_SELECTION_CLI`, and launch `quant-research serve` with explicit dist,
   market directory and one synthetic lake used as both `--dataset-root` and
   `--output` (T04b rejects different canonical roots).
7. Wait for real `/api/v1/health`, then operate `/datasets` and `/backtests` in
   Chromium. Requests go to production API → CLI → published Parquet/manifest,
   persisted fact versions and task/result files. No success responses or
   production endpoints are mocked or added for testing.

`datasets.spec.ts` verifies selection, lifecycle, coverage gaps, exact raw prices,
an empty Bar range, refresh, corrupted version isolation, empty catalog and missing
lake. Its option-count assertions include the extra v2 and multi-Venue Datasets and its loops
filter the v1 preview fixtures by `1 证券 / 2 Bars`; it does not rely on the lake
containing only the preview fixtures. When the whole lake is unavailable the
independent corporate-action panel also reports an error, so that spec checks
that at least one alert is visible rather than requiring a single alert.

`actions.spec.ts` operates the read-only corporate-action coverage panel through
the production `/api/v1/corporate-actions` routes and CLI. It selects a revision
and the older version by their real catalog identities and checks the exact
scale-18 dividend, split fields, explicit no-events, missing fields
(`available_at`, `cash_per_share`), an explicit unknown scope and an out-of-scope
unknown range. It corrupts one persisted version after the catalog load to prove
the fixed read fails visibly, refreshes to isolate it, hides the facts directory
to prove an explicit empty state, and keeps Dataset detail readable throughout.
Rendered values are compared with real API payloads; no mock is used, and fault
mutations restore the invocation-owned lake in `finally`.

`comparisons.spec.ts` operates the production `/comparisons` page and the new
comparison task/result routes. It freezes a single-ETF timing strategy through the
real selection/comparison CLI, executes and reads both authoritative roles, checks
rendered rows and metrics against the result API, reloads and reopens the persisted
task, corrupts the saved benchmark `account_valuations.parquet`, and removes the
frozen benchmark role to prove explicit failure states. A second flow publishes a
temporary same-pool version, runs an equal-weight pool comparison with incomplete
corporate-action facts, and checks that the page keeps both authoritative curves
while marking the comparison incomplete. A third flow imports an explicit adjusted
version for the v1 XSHG Dataset and proves a real comparison task fails with its
persisted CLI error and no fabricated result. These are synthetic checks; they do
not establish real total return or PIT completeness.

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

`pools.spec.ts` also consumes T08b preflight through the production CLI/HTTP.
It selects immutable adjusted versions per member, checks missing bindings/members,
XSHG+XSHE Venue scope, insufficient warm-up, early research end and internal
Session gaps, and compares the displayed plan with the persisted
adjusted_versions/evaluation and actual result Sessions. Refresh preserves explicit
choices but requires a new preflight; later pool edits cannot change the saved task.

`cross-asset.spec.ts` covers #295: it publishes one mixed-venue pool version through
the production pool entry, selects the executable XSHG+XSHE Dataset and per-member
adjusted versions, runs the preflight (both Venues and the Rust-frozen window are
displayed), submits the buy-and-hold baseline, and reads the persisted status,
Summary and valuation rows on `/backtests` after refresh. The frozen `experiment.json` carries sorted `venues`, `full_target`, `retry` and no `venue`
field; the rendered/API rows match an independent hand calculation (50,000 and
49,900 units, cash `500.44006`, equity `999500.44006`). Selecting the mismatched
shared-axis Dataset surfaces `inconsistent_venue_calendar` (422) with no runnable
plan. Only synthetic inputs are exported; no real-data, PIT or full total-return
validation is implied.

`momentum.spec.ts` 使用额外130 Session合成 Dataset，默认20/60/120窗口，核对真实 CLI 持久报告与页面时序/缺失、分布图、来源及刷新/损坏行为。线性价格独立数值 golden 位于 `prajna-experiment/tests/momentum.rs`；该 fixture 与三日池 fixture 分开，不改变原有池预检假设。

`factor-research.spec.ts` 覆盖 #299：同一生产因子研究页选择 Volatility v1 与新增严格均线趋势 `trend_filter_strict` v1，修改窗口后经真实任务/CLI 保存报告，核对持久 report 的 semantics/direction/definition/时序值与页面表格/图表一致；严格趋势窗口 1（close 等于自身均值）逐值为 0，窗口 60 在合成递增复权序列上为 1；波动率窗口 1 的失败任务经页面可见。`momentum.spec.ts` 继续覆盖旧动量路径，两者共用同一 fixture/服务生命周期且报告互不覆盖。
