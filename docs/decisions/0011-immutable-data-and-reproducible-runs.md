# ADR 0011：不可变原文、版本化数据和可追溯 Run 身份

- 状态：Accepted as target architecture，2026-09-26；部分原则已有实现

大规模扫描和后续数据修订会反复使用同一因子与行情；若原文覆盖写、Normalizer 原地修复或结果只记一个随机 ID，历史实验无法说明输入和代码来源。目标 Data Lake 保留原始字节及来源/hash，不覆盖既有 Raw；修订规范化规则或数据产生新的 Dataset Version。Factor Cache 的键必须涵盖因子定义/版本、参数、依赖、Universe、输入数据版本、时间/缺失口径与输出 schema，不能仅按因子名称命中。

Run 的可追溯身份由规范化有效配置、策略与引擎版本、代码修订、Dataset manifest、成本模型和 seed 共同决定。结果分 `Summary`、`Standard`、`Full` 保存，并明确哪些明细未持久化；未保存不得被解释为零交易或零成本。当前仓库已有原文/hash 和不可变发布边界（[`ADR 0008`](0008-published-daily-snapshot-boundary.md)），但还没有目标 Factor Cache、通用 Run 身份和结果等级。物理存储路径与 hash 编码方案留给后续契约设计，不在此 ADR 固定。

MVP-0 的物理布局与 hash 编码见 [ADR 0015](0015-mvp-0-data-contracts.md)。Run 身份边界由 [ADR 0018](0018-run-spec-experiment-execution-identity.md) 细化：代码修订决定 Experiment Execution 身份，不进入 Run Spec 身份。

## 2026-10-27 补充：证券证据固定快照与执行约束门槛

`SecurityEvidenceStore` 的逐字段证据版本继续按内容 hash 不可变保存。固定选择以显式 `(InstrumentId, SecurityEvidenceVersionId)` 写入另一个内容寻址快照；快照内嵌所选记录、版本 id 和门禁结果，重复相同选择幂等，证据修订产生新快照且旧快照仍可按 id 读取。读取路径不接受“最新证据”或按 Instrument 自动选择版本。

发布门禁只对执行必需的 `lot_size`、`price_increment`、`size_increment` 生效：三者均须为 Known 正值才暴露 `ExecutionTerms`；Unknown、Conflict、NotApplicable 或非正值保留为显式 `PublicationIssue`，不填默认手数/增量，也不构造可执行 `InstrumentSpec`。该门槛不发布 Dataset、不开放股票策略回测，也不把成立日、上市日或 `observed_at` 当成行情覆盖或历史可用时刻；真实证据与详细身份只存本机。后续日线发布票必须消费固定快照，不得隐式改选最新版本。

## 2026-10-27 补充：沪深股票/ETF 日频 Normalizer 的固定输入

`cn-stock-etf-daily` v1 的 Dataset 身份由 Normalizer id/version/config、显式固定的 #328 日历版本、#329 证券证据快照 id 和实际消费的全部 Raw hash 共同决定。Normalizer 只解析这些字节级输入，不在规范化或重建时读取可变的日历/证据存储，也不接受后续追加来源记录或“最新”版本改变既有 Dataset Version；规则修订产生新的 Normalizer 版本。契约见 [日频 Normalizer](../contracts/shanghai-shenzhen-daily-normalizer.md)。

来源版本为 `tencent-fqkline-day/1` 时，六字段日线行的价格按精确十进制保存，股票/ETF 成交量分别声明为手 × 100；该来源不提供成交额，因此 `amount` 保持 null。历史 `available_at` 无依据时保持 Unknown，不用 `observed_at` 或 Session 收盘填充；日历内的缺失行情保持缺失，不补零、不删除 Session，也不据此推导停牌或可交易性。生命周期证据只作为固定快照输入保存，不转换为既有 Instrument 的交易窗口。覆盖与 Session 只来自所选 #328 日历版本的声明覆盖，不从行情日期推断。#289 生产入口按单证券隔离发布，显式选择报告与 accepted Raw、日历/证券快照/Normalizer 版本，并冻结报告和 recipe；本机 receipt 绑定 recipe 与 DSV，重建不读取原输入目录或最新版本。真实接入仍需复核来源实际单位，合成验证不证明真实覆盖。
