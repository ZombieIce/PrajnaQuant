# ADR 0016：Factor Cache 以完整输入身份键控且不可变

- 状态：Accepted，2026-10-02；尚未实现

[`ADR 0011`](0011-immutable-data-and-reproducible-runs.md) 要求 Factor Cache 不能仅按因子名命中，但把物理存储与 hash 编码留给后续契约。MVP-1 将其固定：Factor Values 以 restricted-JCS 规范化后取 `fv:sha256:<hex>` 为键，键包含 Factor ID/版本与规范化参数、依赖 Factor 的键（递归）、Dataset Version（DSV）、Static Universe 身份 `uni:sha256:<hex>`、Availability Assumption、计算实现身份（research crate 与 Polars 版本）及 Factor Values 输出 schema 版本；不包含日期区间（总按整份 DSV 计算）、Strategy 参数或线程数。缓存以长表 Parquet 一次写入、原子重命名后不再覆盖；命中时校验内容 hash 与元数据中的键，不一致即报错，不静默删除或重算。

## Considered Options

- 键不含计算实现身份：升级 Polars 后可能无感读到旧实现结果。选择让升级整体失效缓存，以重算成本换取可追溯性。
- 只在进程内缓存：已由 POC sweep 的网格内复用覆盖，不能满足跨 Run/进程复用。
- 校验失败自动重算：会掩盖不可变存储被破坏的事实。
- Universe 放入 D7：Universe 会独立于数据版本演进，且未来要换为 point-in-time 成员资格；故作为独立对象与 DSV 并列进入键。

## Consequences

Static Universe 成员须在给定 DSV 的 instruments 表中存在。Factor Values 用 `status`（`ok`、`insufficient_window`、`missing_input`、`unknown_availability`、`filtered`）说明 null 的原因。依赖升级会导致全量冷缓存，这是有意的代价。
