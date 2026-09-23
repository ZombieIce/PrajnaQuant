# Decision: 当前量化计算的权威实现

## Context

仓库同时有 Rust 仓库和 Python 入库原型，Rust 指标、旧 HTML 自算指标以及 React 派生图表。审计和后续接口需要一个可追溯权威来源。

## Options Considered

- Rust 研究/回测输出为计算权威，Python 做探索，React 做展示。
- Python 或浏览器重新计算并分别作为结果权威。

## Decision

**记录现行边界**：Rust `src/` 是当前数据写入主链，Rust `crates/quant-research/` 是因子评价、策略回测和绩效指标权威。`warehouse.py` 保留为早期原型，不参与生产研究链。React/旧 HTML 的派生计算只能用于展示并必须标注其口径；绩效指标以 `backtest.rs::metrics` 为准。没有在本 ADR 决定未来 Python 接口或因子注册设计。

## Rationale

目前 CLI、实验 JSON 和 API 均直接使用 Rust 报告；Python 没有回测实现。浏览器的旧区间绩效重算与 Rust 年化口径不同，作为权威会造成不一致。

## Consequences

新增指标时优先扩展 Rust 报告和契约；显示层避免再独立定义同名绩效。迁移旧页面功能时必须先对齐算法和时序。当前重复代码不在本次审计删除。

## Status

Accepted as current implementation boundary, 2026-09-23；若未来改职责，须审议替代 ADR。
