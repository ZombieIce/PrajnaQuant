# ADR 0011：不可变原文、版本化数据和可追溯 Run 身份

- 状态：Accepted as target architecture，2026-09-26；部分原则已有实现

大规模扫描和后续数据修订会反复使用同一因子与行情；若原文覆盖写、Normalizer 原地修复或结果只记一个随机 ID，历史实验无法说明输入和代码来源。目标 Data Lake 保留原始字节及来源/hash，不覆盖既有 Raw；修订规范化规则或数据产生新的 Dataset Version。Factor Cache 的键必须涵盖因子定义/版本、参数、依赖、Universe、输入数据版本、时间/缺失口径与输出 schema，不能仅按因子名称命中。

Run 的可追溯身份由规范化有效配置、策略与引擎版本、代码修订、Dataset manifest、成本模型和 seed 共同决定。结果分 `Summary`、`Standard`、`Full` 保存，并明确哪些明细未持久化；未保存不得被解释为零交易或零成本。当前仓库已有原文/hash 和不可变发布边界（[`ADR 0008`](0008-published-daily-snapshot-boundary.md)），但还没有目标 Factor Cache、通用 Run 身份和结果等级。物理存储路径与 hash 编码方案留给后续契约设计，不在此 ADR 固定。

MVP-0 的物理布局与 hash 编码见 [ADR 0015](0015-mvp-0-data-contracts.md)。
