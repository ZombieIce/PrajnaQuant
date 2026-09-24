# Batch 1 C — 五 ETF Universe 发布与回测追溯

日期：2026-09-24。此交接记录本任务新增证据；不要用它替换共享的 `docs/STATUS.md`、`HANDOFF.md` 或 `docs/api.md`。代码/schema、共享 API 语义和量化状态仍以源码、测试与统筹更新为准。

## 实际身份

| 对象 | 身份 |
| --- | --- |
| Universe | `d8811237-6c37-4189-86b8-9b05fbccc405` |
| Published version 1 | `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da` |
| Definition content hash | `91aa64f7872ced85b4513f224fa617a14fa12ae99c86f5936085a86466a4ba18` |
| First member hash | 2025-09-23: `41fbf188a1a69e48c37e28e0cfb6cc3fc2cf65c59c7bfd98ccc7668c29e4c3c7` |
| Last member hash | 2026-09-21: `949216f061c8dbb6b48fd5669d588e7b1bcd64ea0be11c5bbe6a4ecfcfdebfc1` |
| Locked snapshot | `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34` |
| ETF Parquet SHA-256 | `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872` |
| Trading calendar SHA-256 | `0701455f5d65e317974bf61b223a0cbdb36250079d1dca7ca4e3ba01b17bcfee` |
| Experiment | `3fad2150-e5cc-46ff-a32d-3fc6a6ba8e20` |
| Experiment file | `research-output/experiments/3fad2150-e5cc-46ff-a32d-3fc6a6ba8e20/experiment.json` |

Members are `513300`, `518880`, `159612`, `510320`, and `159952`, with names and exchange identities frozen in the version. Each effective interval is `[2025-09-23, 2026-09-22)`, sourced from the manual audited scenario. The chosen scope remains `retrospective_static`; this is not verified PIT membership.

## Reproduction

The management API must be running against the intended output directory. To ensure the Universe without creating another collection or unchanged version:

```bash
python3 scripts/ensure_etf_universe_mvp.py --base-url http://127.0.0.1:7878
```

The script identifies this collection by the reserved `source_ref`, updates only that draft, previews the server hash, publishes with the expected hash, and reads the result back. It refuses ambiguous duplicate markers or an archived collection. On the current project output it returned `created_universe=false`, `version_created=false`, `version_count=1` and preserved the IDs and hash above. It was repeated against both a temporary output and the project output.

Run using the published definition loaded from Universe storage and the fixed snapshot ID above:

```bash
cargo run --offline -p quant-research --example etf_universe_mvp -- \
  --output research-output \
  --universe-id d8811237-6c37-4189-86b8-9b05fbccc405 \
  --version-id 3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da
```

The example does not create a temporary definition or select the newest snapshot. It checks the locked snapshot ID/file/calendar hashes, requires the five exact published members and effective intervals to match the independently audited window, includes pre-window bars for factor warmup, and configures the strategy dates to 2025-09-23 through 2026-09-21. Running it creates a new experiment UUID, as expected for a distinct run; it does not overwrite prior experiments.

The report assumptions state: close-time signal and next available open execution; zero cash interest; zero ETF distributions; raw-price return, not total return; and no verified historical PIT. Benchmark bars were intentionally excluded from this isolated scenario. Do not present the result as unbiased historical performance.

## Snapshot audit and current coverage limitation

The immutable manifest and both Parquet hashes matched. Against the hash-checked SZSE daily calendar, the requested interval contains 241 open dates. Each ETF has 241 rows from 2025-09-23 through 2026-09-21, with zero missing dates, non-open dates, duplicate dates, or invalid OHLC rows. The runner loaded 1,605 ETF bars total, of which 1,205 fall in the configured run range; the remainder provide warmup history.

The audit's calendar result is `complete`, but its overall `status` is `unverified`: the ETF Parquet does not carry a per-row `source` column, so the current audit cannot check source provenance for each row. File hash and manifest selection rule are available. The latest code does not report a source-provenance-complete market audit, so this limitation must stay visible.

The published version has no persisted `CoverageSegment`: current `UniverseDraft`/PATCH cannot store one. Consequently runner identity reports Universe `coverage=gaps`, while the management coverage API separately reports `membership_coverage=retrospective_static` and, when explicitly queried for 2025-09-23—2026-09-21, a complete calendar and all five 241-row member audits. These fields describe different things and must not be collapsed. The management page currently requests coverage without the audited range; it displays `行情 unverified` and no expected open dates because the calendar sidecar does not cover the entire 2016–2026 ETF snapshot range. At 2026-09-21, its date-specific member view visibly lists all five members and preserves `retrospective_static`.

Minimum integration requests for the responsible owner:

1. Extend the Universe storage/API draft contract to preserve explicitly asserted coverage segments and their source revision hash, if the coordinator wants the experiment's membership coverage to reflect this audited scenario. Publish a new immutable version after that change and rerun; do not rewrite version 1.
2. Have the management coverage view query the manually selected members' effective date range (or expose an explicit range), while keeping membership/PIT status separate from market-data coverage.
3. Either include source provenance in future ETF snapshots or keep audit status `unverified` when row-level provenance is unavailable. Do not upgrade `retrospective_static` to `verified_pit`.

This task made no functional changes to `universe_api.rs`, `server.rs`, public API docs, shared status docs, front-end pages, `backtest.rs`, `runner.rs`, `data.rs`, or schemas. The Universe was persisted only through the management API. The experiment file was created by the runner in the normal output directory. Those parallel files already contained broad uncommitted work before this task.

## Result and account verification

- The new result API filter for the exact Universe/version returned exactly experiment `3fad2150-e5cc-46ff-a32d-3fc6a6ba8e20`; the same Universe with a different version UUID returned zero rows.
- The unfiltered actual output contained 11 experiments, including 5 legacy items surfaced as `legacy_universe_unknown`; these remained readable and did not match the new version filter.
- The result page in the browser showed the published Universe and version selectors; selecting version 1 showed the new experiment. The Universe page showed version 1, the five named members at 2026-09-21, and the retrospective static label. Its broad default coverage request displayed `unverified` as described above.
- An independent integration test rebuilt cash, quantities, and close marks from fills for every one of 241 equity dates. Maximum `cash + marked holdings − NAV` error was 0.0; the persisted position curve agreed with daily NAV. Costs reconcile: commission 2,991.47 + tax 0 + slippage 1,981.61 = total 4,973.08.
- Instrument total P&L sums to the final NAV change (1,179,713.32 − 1,000,000 = 179,713.32). End quantities are 65,000 of `sh518880` and 285,600 of `sz159612`; the other three end at zero. This checks the report's cost and attribution ledger, not future market returns.
- Current engine output: 241 equity points, 32 trades, final NAV 1,179,713.32, raw-price return 17.9713%, max drawdown −15.6544%. These values describe this run only and are not hard-coded as an invariant for later engine versions.

## Changed files and integration points

- `crates/quant-research/examples/etf_universe_mvp.rs`: loads `server::load_universe_version`, locks and verifies the real snapshot, confirms member intervals and audits, applies warmup/run date limits, saves the experiment, and emits trace identities and the independent cash/NAV reconciliation.
- `crates/quant-research/tests/etf_universe_mvp.rs`: opt-in live-data integration test loads a published version and fixed snapshot, runs the version-aware runner, checks the daily account and P&L identities, and round-trips the result JSON.
- `scripts/ensure_etf_universe_mvp.py`: idempotent API-backed draft reuse/publish and membership readback.
- `docs/handoffs/batch1-c-universe.md`: this task handoff only; shared status, handoff, and API documents are left for the coordinator.

## Verification evidence

Commands and outcomes:

```text
cargo fmt --all -- --check                                      passed
cargo test --workspace --locked --offline                       passed (10 warehouse + 46 research tests)
cargo clippy --workspace --all-targets --locked --offline -- -D warnings  passed
PRAJNA_ETF_MVP_OUTPUT=research-output \
PRAJNA_ETF_MVP_UNIVERSE_ID=d8811237-6c37-4189-86b8-9b05fbccc405 \
PRAJNA_ETF_MVP_VERSION_ID=3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da \
  cargo test -p quant-research --test etf_universe_mvp --locked --offline -- --ignored --exact \
  published_version_and_locked_snapshot_round_trip_with_account_ledger  passed on the real snapshot/version
cargo run --offline -p quant-research --example etf_universe_mvp -- --output research-output --universe-id ... --version-id ...  passed; saved experiment above
```

The HTTP checks ran on isolated loopback ports 18787 (temporary output) and 18789 (project output), not the default port. `GET /api/v1/health`, Universe listing/detail, date-specific members, range-specific coverage, exact result filtering, wrong-version empty filtering, and legacy compatibility were checked. The browser was opened against the current built local UI: the management page and strategy result page both showed the new records. The manager's default full-snapshot coverage display is an observed limitation, not a browser acceptance pass for complete market coverage. No front-end code was changed, so no npm build was needed.

`git diff --check` is required after coordinator review. Workspace files already had broad uncommitted changes before this task; preserve them. While verification was in progress, the parallel data/server edits briefly caused compile failures (DuckDB `list` decoding and missing coverage-audit initializer fields); the current workspace passed all commands above after those shared files settled. Do not attribute those fixes to this task.

## STATUS / HANDOFF / API update suggestions

- **STATUS:** record the real stored Universe/version/experiment IDs and the locked snapshot; say the run is traceable to a published retrospective static version, not PIT or unbiased performance. Report 241/241 member bars and the complete official-calendar interval separately from overall `unverified` row-source audit status. Keep zero distributions/raw-price return explicit.
- **HANDOFF:** mark the five-ETF persisted-run/filter/account loop as completed. Carry the `CoverageSegment` storage gap, missing Parquet row-source provenance and full-snapshot UI date-range issue as open integration items. Do not add a second recommendation; use the single next step below.
- **API:** document that `UniverseDraft` currently cannot persist manual coverage segments and that omitted coverage date bounds span the full snapshot. Distinguish `membership_coverage=retrospective_static`, member fact coverage and market-data coverage; state that row provenance unavailable yields `unverified` even with zero date/OHLC gaps.

## 唯一建议下一步

等 Agent A 的执行规则合入后，由统筹在当前五 ETF 已发布版本和锁定快照上复跑一次最终 API、结果筛选、页面与账户集成验收；如需为成员范围保存显式 coverage segment，先由统筹指定 UniverseStore/API 负责人，再发版新 Universe 版本并复跑。
