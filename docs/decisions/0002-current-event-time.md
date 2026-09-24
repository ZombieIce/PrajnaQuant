# Decision: 当前 ETF 日频事件顺序

## Context

收盘因子若以同日开盘或同日收盘成交，会改变可知性和收益。现有 `run_with_scores` 有固定事件顺序，但此前没有独立时间契约。

## Options Considered

- T 日收盘观察和选股，下一组行情日开盘执行。
- T 日收盘观察、T 日收盘执行。
- 使用更细粒度报价/订单事件。

## Decision

**记录已实现行为**：T 日完整收盘价用于信号与 Top-N 决策，目标名单入 `pending`，下一组有 ETF 行情的日期开盘按现有 bar 执行，再用该日收盘估值；`forward_return` 只作因子评价标签。不存在独立 order_time/position_effective_time 字段。更详细的边界见 `docs/time-model.md`。

## Rationale

这是 `backtest.rs` 的实际事件循环。将其明文化可防止后续误把 T close 当作同日可成交价格。

## Consequences

缺 bar 待执行目标语义由 ADR 0005 补充：需要卖出的旧仓缺 open 时整次延期、可重试且由较新的调仓信号替换。下一次改执行模型须同时更新事件测试、`time-model.md`、实验假设与 API 口径。交易状态、历史 universe、分红/总回报和基准对齐仍是未决正确性问题；本 ADR 不把它们视作已解决。

## Status

Accepted as description of current behavior, 2026-09-23；更严格交易模型尚未决定。
