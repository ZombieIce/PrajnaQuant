# ADR 0005: 旧仓缺执行价时延期整次调仓

- Status: Accepted
- Date: 2026-09-24

## Context

`rebalance` previously skipped a missing open for an old off-target holding, then continued buying the new target with residual cash. A Top-N backtest could therefore carry an unsellable old holding and add a new holding, exceeding the target count. The state had no record of a deferred attempt.

## Options Considered

- Continue partial execution and accept that actual holdings can exceed Top-N.
- Sell available old holdings and use remaining cash to buy new targets.
- Defer the entire rebalance when any positive off-target holding lacks an execution-day open.

## Decision

Choose whole-rebalance deferral when any positive held symbol that needs a sale has no execution-day open or fails the applicable status gate. This includes reducing an existing target holding; when that holding has no open, the engine conservatively defers because it cannot determine the needed reduction. No sell or buy leg runs on that attempt; the old holdings and cash remain unchanged. The pending target is retried on the next available market session. A later scheduled rebalance decision replaces the older pending target. If every required sale is executable, sell-before-buy logic continues; target buy legs whose own open is absent are skipped and logged, then may re-enter only through a later scheduled signal.

Each deferral attempt is recorded in `BacktestReport.rebalance_deferrals` with its decision date, attempt date, target symbols, blocked symbols, and reason. This is an audit trace for this specific deferral, not a general order lifecycle. Historical reports deserialize with an empty deferral list.

## Consequences

Starting from a flat account, a blocked old holding cannot trigger a purchase that pushes actual positions above Top-N. A deferred target may become stale, but it is replaced by the latest scheduled signal; otherwise it is retried until executable. If a target buy symbol alone has no bar while no old sale is blocked, other executable legs may proceed, so actual holdings can temporarily underfill the target. Missing-data status is still not inferred as a trading halt, and old experiment files are not rewritten.

The fixed four-session cash/position ledger verifies the bound and NAV identity. A second hand case holds A×200 and B×200 after D2; A's D3 open is missing while both remain targets, so D3 makes zero trades rather than buying more B. D4 with A's open available may reduce A and buy B. Exchange status-source completeness and historical availability remain open; the front end now renders deferrals and skipped legs.
