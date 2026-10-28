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
`CARGO_TARGET_DIR` may select a task-owned cache; default is repository `target/`.
`PRAJNA_E2E_PORT` selects a loopback port (default 18789). An occupied port fails;
existing servers/lakes are never reused. Direct `playwright test` is only supported
for listing; execute tests through the npm wrapper so an isolated root exists.
Arguments pass through, for example `npm run test:e2e -- --grep 'corrupt version'`.

`test:e2e:check` typechecks the configuration/specs and checks test discovery.

The Playwright webServer performs these steps, all from this checkout:

1. `npm run build` produces the production `apps/web/dist`.
2. Build `quant-research` and `prajna-experiment` with `--locked --offline`.
3. Run `prajna-data`'s opt-in `export_synthetic_preview_fixture` integration test.
   This invokes the real `prajna-publish-daily` CLI with synthetic source bytes,
   pinned calendars and security evidence, publishing an XSHG ETF and XSHE stock.
   The fixture has Known/Unknown lifecycle dates, two Bars and deliberate gaps.
   Cargo builds the real Dataset reader/publisher binaries for the integration test.
4. Check the real `prajna-experiment --help`, bind it through
   `PRAJNA_EXPERIMENT_CLI`, bind `PRAJNA_DATASET_PREVIEW_CLI`, and launch
   `quant-research serve` with explicit dist, lake, output and market directories.
5. Wait for real `/api/v1/health`, then operate `/datasets` in Chromium. Requests
   go to production API → Dataset CLI → published Parquet/manifest. No success
   responses or production endpoints are mocked or added for testing.

The spec verifies selection, lifecycle, coverage gaps, exact raw prices, an empty
Bar range, refresh, corrupted version isolation, empty catalog and missing lake.
Fault cases mutate only the invocation's synthetic lake and restore it in
`finally`; one worker and no parallel tests prevent fixture races. Preparation,
server, assertion and browser-launch failures produce a nonzero exit, never a skip.
Playwright tears down the owned server process tree after success or failure.

`server.log`, failure screenshots/traces and browser diagnostics remain in the
printed local temporary directory. Inspect locally (for example with
`npx playwright show-trace /absolute/path/to/trace.zip`); remove only your known
owned directory after investigation. Artifacts are not attached to GitHub or
committed. These are synthetic software checks, not real market evidence, PIT
validation, strategy performance or completion of the backtest product loop.
The Experiment CLI is built/bound but no Experiment is submitted by this ticket.

For later page tickets, add `*.spec.ts` under `e2e/`, reuse this server lifecycle,
and extend committed synthetic fixture inputs through existing production CLI
entry points as required. Fix each task's selected DSV, Universe/definition and
output root explicitly; never pick a user's lake or fall back to real downloads.
Assert persisted API values and visible page outcomes; do not substitute helper
reports or mocked success for the product path. Keep numerical assertions at Rust
seams, and state synthetic versus local real-data validation separately. If a new
page needs mutable shared state, restore it or give it a separate fixture root;
keep one worker until fixture ownership supports parallelism.
