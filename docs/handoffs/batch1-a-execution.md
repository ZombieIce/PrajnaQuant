# Batch 1A — Rust execution gates and unexecuted-order audit

Date: 2026-09-24
Scope: `backtest.rs`, `runner.rs`, frozen execution-status input, and focused hand-calculated tests.

## Completed in this task

- Reviewed the existing status-aware snapshot/backtest path and retained its frozen-input boundary: `runner.rs` loads execution statuses from the snapshot Parquet file before calling the backtest; the backtest does not query the mutable production database. The snapshot hash covers the embedded status columns.
- Added a preflight for every planned sale before any fill. This closes a gap where a currently targeted holding that needed to be reduced could bypass the execution-status gate, even though off-target exits already deferred under ADR 0005. A blocked sale records an unexecuted `SELL` and defers the whole rebalance.
- Added an audit record for a pending `TARGET` that reaches the final available date without a later execution session (`no_future_execution_session`). Its `attempt_date` is the final report cutoff, not a fill attempt; `desired_quantity` is unknown because no execution open exists. `side=TARGET` distinguishes this terminal intent from an order that reached an execution attempt.
- Added a missing-target-open regression with a later fresh signal. The first BUY is skipped and recorded as `missing_open`; ADR 0005 retries only whole rebalances blocked by old-holding exits. A later scheduled signal can select and buy the symbol when an execution open is present.
- Extended the missing-bar case to assert that the existing latest-close mark remains in the daily position snapshot and that the blocked SELL reason is auditable.
- Existing independent tests already cover old off-target holdings missing an open and whole-rebalance deferral, deferred target replacement by a newer scheduled signal, explicit `HALTED` and `UNKNOWN`, missing status fail-closed, runner snapshot integration, nonzero commissions/slippage, daily cash/NAV reconciliation, and instrument P&L attribution.

## Execution-status input contract and integration point

The current ETF backtest input is keyed by `(symbol, effective_date)` (the row date is the trade date) and contains:

- `trade_status`: source-declared `TRADABLE`, `HALTED`, or `UNKNOWN`; the research snapshot additionally derives `CONFLICT` when latest per-source rows disagree.
- `is_tradable`: true only when all represented source rows explicitly say `TRADABLE`.
- `status_sources`: the sorted source names contributing to the daily aggregate.
- Missing status row: remains missing in the frozen snapshot and fails closed in a status-aware run. Missing price bars are reported as missing execution price; they are not inferred to be a halt.

The source schema `core.security_status_revision` has `symbol`, `effective_date`, `trade_status`, `is_st`, `limit_rule_id`, `source`, `observed_at`, `run_id`, and `row_hash`. Latest revisions are chosen per symbol/date/source. Status-row `observed_at` is ingestion time, not historical publication or availability time. The schema has no `available_at` / historical publication field; no such time is fabricated. The current general daily snapshot sidecar in `data.rs::ExecutionStateInput` carries `instrument_id`, `symbol`, `trade_date`, `status`, `source`, `covered`, and `observed_at`, but its `observed_at` comes from the selected market bar (`b.observed_at`), not the status revision. Also, `covered` currently means a status source row exists for that date; it is not proof of complete source coverage. The embedded status snapshot is immutable for the run, but it is a snapshot of the latest available revisions and does not prove that each status was knowable at the historical decision cutoff.

Integration point: snapshot creation in `data.rs` embeds the status aggregate in the ETF Parquet; `load_snapshot_execution_statuses` reads that Parquet; `runner.rs` passes the in-memory map to `run_with_scores_and_statuses`; `backtest.rs` applies it on execution dates. A snapshot lacking the status columns is legacy bar-only behavior. The current report has no field identifying that mode, so consumers must not infer that an old report passed the execution-status gate.

## Event rules and hand-calculated evidence

- T-close scores create a pending target; the next date represented by the frozen bars is the execution opportunity. The schedule is based on observed ETF bar dates, not a proved exchange-session calendar.
- Positive off-target holdings (and any target holding requiring a reduction) must have a same-date open and an explicitly tradable status before any rebalance leg executes. Otherwise no leg executes, the pending target remains, and ADR 0005's whole-rebalance deferral rule applies.
- A target BUY missing its open or explicit tradable status is skipped and logged per order. This isolated skipped buy does not retain/retry the old target; only a later scheduled target can buy it. This keeps ADR 0005's retry scope unchanged. A target still pending at end of input is a `TARGET` audit item, not a simulated BUY order.
- A later scheduled rebalance replaces an older pending target. The final-date target has no future execution session and is logged as such.
- Current-close marks continue to be used for valuation. When a held symbol has no new bar, the prior close is reused; the report currently does not say which date that mark came from or how stale it is.

The existing 3-ETF/10-date fixture hand-computes D3 old-holding A missing-open deferral and D4 replacement by C: cash remains 4,000 and A×100 stays held on D3; D4 sells A×100 and buys C×200, leaving 2,000 cash; daily NAV remains 10,000 and positive positions stay within Top-1. The added target-buy fixture proves B's D2 missing-open skip and a later D3 signal executing at D4. The status-aware hand ledger covers UNKNOWN BUY, HALTED SELL, blocked whole-rebalance and recovery, with commissions 300, slippage 270, total cost 570. Existing golden ledger covers nonzero fee/slippage arithmetic and symbol P&L reconciliation.

These are synthetic code-behavior tests only; they do not validate real status-source completeness, provenance truth, historical availability time, exchange halt truth, limit-up/down execution, or price queue/fill feasibility.

## Account, cost, and look-ahead review

- Existing equity timing remains: execute at the next available bar open with direction-specific slippage, then mark at close; no T-close fill was added.
- Sell-before-buy order, lot rounding, fees/taxes, cash changes, and weighted-average P&L methods are unchanged in the normal tradable path.
- Existing and added daily assertions check `equity = cash + marked positions`, nonnegative cash/long quantity, Top-N bound in the missing-entry fixture, and fixed trade/cost results. The final target has no fill and does not alter the ledger.
- Future label isolation remains covered by `runner::tests::future_return_labels_do_not_change_decision_scores`.
- Stale mark evidence is only partial: the test shows the old close still values a holding on a missing-bar date. No age/date field is exposed, so the requested stale-valuation audit is not complete.

## Files changed by this task

- `crates/quant-research/src/backtest.rs`
- `crates/quant-research/src/runner.rs`
- `docs/handoffs/batch1-a-execution.md`

`data.rs` was reviewed but not changed by this task. It contains shared, uncommitted changes from the parallel worktree. Do not attribute those broad changes to this task.

## Verification

- `cargo test -p quant-research --lib --locked --offline`: passed once, 45 tests, 0 failures. A final rerun after shared changes began failed to compile because `data.rs::MarketCoverageAudit` gained `source_provenance_available` but `server.rs` has two initializers without that field (`server.rs:775`, `server.rs:834`).
- `cargo test -p quant-research --locked --offline`: failed while building the shared `examples/etf_universe_mvp.rs`: missing `anyhow::Context` and `sha2::Digest` imports, plus a `BTreeMap<&str, ...>` index type mismatch. The example is outside this task's write boundary and was not changed here.
- `rustfmt --edition 2024 --check crates/quant-research/src/backtest.rs crates/quant-research/src/runner.rs`: passed.
- `cargo fmt --all -- --check`: failed on formatting differences in shared `data.rs` and `tests/etf_universe_mvp.rs`; no global formatter write was run.
- `git diff --check`: passed.
- `cargo clippy -p quant-research --lib --locked --offline -- -D warnings`: passed once before the latest shared change; final rerun is blocked by the same `server.rs` compile error.
- `cargo clippy -p quant-research --all-targets --locked --offline -- -D warnings`: failed because the shared example has the compile errors listed above.

## Open / blocked items

1. Actual execution-state source completeness and historical availability are unverified. The source table has status ingestion `observed_at`, but no historical `available_at`. The current generic snapshot's `observed_at` is bar ingestion time and `covered` only means some status source row exists. Snapshot hashing freezes chosen rows but does not make them historical point-in-time facts. Keep P0-3 open and do not claim real execution validation.
2. Legacy reports remain readable through serde defaults but have no explicit execution-status mode. Old Parquet snapshots still use bar-only behavior.
3. Mark price and valuation price are distinguishable only by context today; there is no per-position mark date or staleness age. A report/API extension is needed.
4. Buy-side skip and old-exit deferral semantics are synthetic-test verified; limit rules, partial fills, liquidity, and source correctness remain unsupported/unverified.
5. Latest shared `data.rs`/`server.rs` changes block lib tests and lib Clippy because two `MarketCoverageAudit` initializers omit `source_provenance_available`. Shared example compile errors and formatter diffs additionally block full package tests, all-target Clippy, and global formatting check. Do not overwrite that parallel work; coordinate resolution with the responsible parallel owner/统筹 before final integration verification.

## Requested shared-document / API updates for integrator

- `STATUS.md`: keep P0-3 unresolved. State that the status-aware code path and synthetic tests exist, but source vintage/coverage, `observed_at` vs historical availability, legacy bar-only snapshots/reports, and stale-mark age are not accepted. Missing bars mean missing price, not halt.
- `HANDOFF.md`: replace the next step with source-vintage/availability audit and report-mode/stale-mark contract, after resolving the shared compile blocker.
- ADR 0005: no policy change; optionally clarify that a blocked target buy alone is skipped/logged and is not retried as the same pending target; later scheduled targets may re-enter it.
- ADR 0007: document that snapshot values are frozen but latest revisions lack historical availability proof; specify explicit legacy/status-aware report mode once the contract is approved.
- `ExecutionStatus` / runner input (interface request only; no `core.rs` field changed here): distinguish `covered` from `unknown`, carry source/status provenance, and carry `status_observed_at` (the status table's ingestion time, not an `available_at`). The current generic sidecar's `observed_at` must not be reused for status provenance because it belongs to the bar.
- API/backtest report contract (interface request only; no API fields changed here): add an execution-status mode such as `status_gated` / `legacy_bar_only`, plus per-holding `mark_date` and `stale_calendar_days` (or an explicit no-mark gap). Ensure defaults keep old reports readable. The integrator owns compatibility and UI decisions.

## Integrator acceptance steps

1. Have the owner of the shared data/API changes resolve the `MarketCoverageAudit` initializers and shared example compile/formatter issues; retain that parallel work.
2. Re-run this backtest suite and inspect the new target-buy, target-reduction, final-target, and prior P0-3 fixtures; check exact costs and daily accounting.
3. Run workspace format check, locked offline workspace tests, and Clippy; do not close P0-3 solely on synthetic results.
4. Review/approve the core execution-input and report output-field requests with their owners; audit actual source samples before any real-data execution claim.

**唯一建议下一步：**统筹先协调共享 example/data 改动的负责人修复包级构建与格式问题，再完成整个 workspace 的测试/formatter/Clippy 集成验收；真实状态来源的历史可知性有证据前，保持 P0-3 开放。
