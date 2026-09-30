# Schema 7 发布清单与 DSV Manifest 映射

本文记录现有 schema 7 日线快照发布清单与 D8 `Manifest { dsv, core, provenance }` 的关系。此映射不要求或引入发布链迁移；schema 7 仍是 `ashare-warehouse` / `src/` 与只读 resolver 使用的契约，DSV 是 `prajna-data` 面向规范化数据集的内容身份。

## 对照依据

- schema 版本：`src/lib.rs` 中 `SCHEMA_VERSION = 7`。
- schema 7 字段清单与写入：`src/lib.rs::Warehouse::publish_daily_sync_with_hook`。`serde_json::json!` 中构造的对象是清单字段的权威清单；嵌套 `source_attempts` 来自 `ops.daily_sync_attempt` 查询，`audit` 在同一函数中计算。
- resolver：`src/lib.rs::resolve_published_daily_snapshot`。它验证 snapshot ID、Parquet 文件 hash、目录 sidecar hash，以及 current pointer 中的 manifest hash。
- DSV 结构：`crates/prajna-data/src/manifest.rs` 中 `Manifest`、`ManifestCore`、`ManifestInput`、`ManifestTable`、`ManifestProvenance` 与 `ManifestFile`；身份计算由 `crates/prajna-data/src/manifest.rs` 对 `core` 做受限 JCS 后计算。

表中“有对应”表示字段含义可直接落到目标字段；“语义不同”表示有关联或可供 provenance 记录，但不能互换；“无对应”表示 D8 没有相应字段。D8 的 `core` 决定 dsv；`provenance` 不参与 dsv。

## Schema 7 顶层字段

| Schema 7 字段 | 映射 | DSV 字段 / 原因 |
|---|---|---|
| `schema_version` | 语义不同 | D8 `core.manifest_version` 是 DSV manifest 格式版本（当前为 1）；schema 7 是旧发布清单的 schema 版本（7），两者版本范围不同。 |
| `snapshot_id` | 无对应 | UUID 是一次发布快照的随机身份；`dsv` 是规范化内容身份。同一内容可在 schema 7 下产生不同 snapshot ID。 |
| `job_id` | 无对应 | 是旧同步作业 ID。D8 没有同步作业字段。 |
| `created_at` | 有对应 | 可记录到 `provenance.created_at`；二者均描述生成时间。它不进入 DSV。 |
| `source` | 语义不同 | 描述旧发布所用行情来源/优先规则；D8 以 `core.inputs` 的 Raw 内容 hash 和 `core.normalizer` 身份描述可重建输入与转换器。来源文字本身不能替代这些身份。 |
| `source_attempts` | 语义不同 | 旧同步请求的逐次尝试记录；其中实际参与规范化的原文 hash 可成为 `core.inputs[].raw_sha256`，但失败、空响应或未被选用的尝试不因此成为 DSV 输入。详细字段见下表。 |
| `symbols` | 语义不同 | 旧快照请求的证券清单；规范化后证券身份和值应体现在 `core.tables` 的逻辑内容中。请求列表可作为 `provenance.coverage`，但它不是 `core.inputs` 或表 row count。 |
| `revision_lookback_days` | 无对应 | 旧同步时重取修订的窗口参数。若会改变规范化内容，应由 D8 的 normalizer 配置/版本体现；没有同名 provenance 字段。 |
| `revision_lookback_start` | 语义不同 | 旧行情修订回看范围起点；可记入覆盖 provenance，但它不是 D8 数据身份的独立字段。 |
| `requested_start` | 语义不同 | 请求区间起点；可记入 `provenance.coverage`。D8 没有固定的请求区间模型。 |
| `as_of_date` | 语义不同 | 同步作业的区间终点；可记入覆盖 provenance。它不是规范化表的时间范围声明或输入可用时间。 |
| `data_cutoff_date` | 语义不同 | 当前发布数据截止日；可记入 `provenance.coverage`。D8 没有等价的顶层字段，不能据此声称严格 PIT。 |
| `coverage_status` | 语义不同 | `complete`/`gaps` 是旧发布审计的汇总判定；D8 `provenance.coverage` 可保存范围与审计明细，但其任意 JSON 值没有相同枚举或判定规则。 |
| `price_adjustment` | 语义不同 | 旧日线价格调整口径（当前为 `none`）；应作为来源/合成假设或 normalizer 配置记录。D8 没有标准化的复权字段。 |
| `calendar_source` | 语义不同 | 描述旧发布日历来源；实际日历 sidecar 的 hash 可放入 `provenance.files`，来源信息可放入 provenance，但不构成 `core.inputs` 的 Raw 身份，除非它作为 normalizer 输入被明确纳入。 |
| `calendar_file` | 语义不同 | 日历 sidecar 路径可记录在 `provenance.files[].path`；D8 不定义固定 sidecar 文件名。 |
| `calendar_sha256` | 语义不同 | 是日历 sidecar 的字节级 hash，可记录在 `provenance.files[].sha256`；它不是规范化表的 `logical_hash`，也不会自动成为 `core.inputs`。 |
| `catalog_file` | 语义不同 | 证券目录 sidecar 路径可记录在 `provenance.files[].path`；D8 不定义固定目录 sidecar。 |
| `catalog_sha256` | 语义不同 | 是证券目录文件的字节级 hash，可记入 `provenance.files[].sha256`；不等于其对应规范化表的 `logical_hash`。 |
| `data_file` | 语义不同 | Parquet 路径可记录在 `provenance.files[].path`。文件路径/编码不是逻辑表身份。 |
| `data_sha256` | 语义不同 | 是 `daily.parquet` 全文件字节 hash，可记录为 `provenance.files[].sha256`。D8 `core.tables[].logical_hash` 对解码后的规范化值按主键与类型规则计算；二者目的与稳定性不同，不能互换。 |
| `rows` | 语义不同 | 是旧日线 Parquet 的行数；若其数据被映射为一张 DSV 表，可与该表的 `core.tables[].row_count` 核对。schema 7 的行数不覆盖 DSV 中其他表，也不建立表身份。 |
| `audit` | 语义不同 | 旧发布检查的结果与计数，可部分保存在 `provenance.coverage`。D8 没有相同的审计结构或 schema 7 完整性判定。详细字段见下表。 |
| `status_unknowns_preserved` | 语义不同 | 是 schema 7 导出对未知交易状态保留行为的声明；D8 可把此假设写入 `provenance.synthetic_assumptions` 或 `provenance.coverage`，但没有对应的标准字段或验证语义。 |

## `source_attempts[]` 字段

| Schema 7 字段 | 映射 | DSV 字段 / 原因 |
|---|---|---|
| `symbol` | 语义不同 | 说明尝试针对哪个证券，可进入覆盖 provenance；不是 Raw 内容身份。 |
| `start` | 语义不同 | 本次请求窗口起点，可进入覆盖 provenance；不是 DSV 的固定字段。 |
| `end` | 语义不同 | 本次请求窗口终点，可进入覆盖 provenance；不是 DSV 的固定字段。 |
| `attempt` | 无对应 | 旧任务内的尝试序号；D8 不表示重试次数。 |
| `status` | 无对应 | 旧来源请求状态；不是 DSV 输入或规范化表状态。 |
| `source_rows` | 语义不同 | 来源响应行数；不等于最终 DSV 表的 `row_count`。 |
| `inserted_revisions` | 无对应 | 写入旧 revision 表的计数；DSV 不保存仓库修订写入计数。 |
| `raw_path` | 语义不同 | 旧 ingest 原文路径，可作为 `provenance.files[].path` 的线索；仅当原文对象按 D8 Raw 契约保存并被规范化器消费时，才与一个 DSV 输入关联。 |
| `raw_sha256` | 语义不同 | 候选旧响应的原文字节 hash；只有该原文实际进入重建链时，对应 hash 才应列入 `core.inputs[].raw_sha256`。尝试记录可为空、失败或未被采用，因此不能直接等同于 `core.inputs`。 |
| `request_url` | 语义不同 | 请求来源细节，可在 `provenance` 脱敏后记录；不属于 `core`。 |
| `error_class` | 无对应 | 旧同步错误分类；D8 manifest 没有采集尝试错误字段。 |
| `error` | 无对应 | 旧同步错误文本；不属于 DSV 身份，且不能原样作为通用 provenance 契约。 |
| `source_run_id` | 无对应 | 旧 ingest run UUID；不属于 DSV 身份。 |

## `audit` 字段

| Schema 7 字段 | 映射 | DSV 字段 / 原因 |
|---|---|---|
| `status` | 语义不同 | 旧审计的 `complete`/`gaps` 汇总；只能作为 `provenance.coverage` 的说明，D8 不定义该状态机。 |
| `coverage_basis` | 语义不同 | 解释旧覆盖率按显式证券 × 已确认深交所开市日核算；可原样作为 coverage provenance 内容，但 D8 不定义其计算口径。 |
| `requested_start` | 语义不同 | 审计使用的请求起点；可记录到 coverage provenance。 |
| `requested_end` | 语义不同 | 审计使用的请求终点；可记录到 coverage provenance。 |
| `symbols` | 语义不同 | 审计所覆盖的证券列表；可记录到 coverage provenance，不是 DSV 输入列表。 |
| `calendar_days` | 语义不同 | 区间内已保存日历日数；D8 不定义日历覆盖计数。 |
| `natural_days` | 语义不同 | 请求区间自然日数；D8 不定义请求窗口审计。 |
| `confirmed_open_days` | 语义不同 | 已确认开市日计数；如需保留应存入 coverage provenance。 |
| `missing_symbol_days` | 语义不同 | 缺失的证券×开市日列表；可存为 coverage 细节，不是规范化数据本身的缺失填充值或 DSV 字段。 |
| `empty_source_attempts` | 语义不同 | 空响应尝试数；来源审计 provenance，不是 DSV 输入数。 |
| `bars_on_closed_or_uncovered_dates` | 语义不同 | 落在闭市或无日历覆盖日期上的行情行数；schema 7 审计计数，D8 不定义。 |
| `duplicate_symbol_date_source_keys` | 语义不同 | 旧来源业务键冲突计数；D8 会按目标表主键规则验证重复，但不复用此计数或来源键定义。 |
| `ohlc_or_volume_unit_anomalies` | 语义不同 | 旧 OHLC/成交量异常计数；D8 normalizer 可执行自己的校验，D8 manifest 不提供此字段。 |
| `unknown_state_security_days` | 语义不同 | 未知状态证券日计数；可写入 coverage provenance，不能映射成可成交状态或 DSV 输入身份。 |
| `conflicting_state_security_days` | 语义不同 | 状态冲突证券日计数；旧发布审计计数，D8 不定义。 |
| `unverified_or_gapped_state_coverage_records` | 语义不同 | 状态覆盖声明未验证或有缺口的记录数；可作为 coverage 说明，D8 没有相同验证状态。 |
| `state_semantics` | 语义不同 | 说明缺失状态仍为 UNKNOWN 的旧链语义；应作为 provenance 假设/覆盖说明保留，不是 DSV 标准字段。 |

## 关键边界

- `snapshot_id` 与 `dsv` 不等价：前者标识一次旧快照发布，后者由规范化数据集的 `core` 内容确定。
- `data_sha256` 与 `core.tables[].logical_hash` 不等价：前者校验 Parquet 文件字节，后者校验按契约解码、排序后的逻辑值。重写 Parquet 编码可能改变文件 hash 而不改变逻辑 hash。
- `source_attempts[].raw_sha256` 与 `core.inputs` 仅有条件关联：DSV 的输入只列真实参与构建且可重建的 Raw 对象；旧尝试记录还可能覆盖失败、空响应和未采用响应。
- `coverage_status` 与覆盖率 provenance 不等价：schema 7 的汇总状态由旧发布审计规则产生；D8 provenance 可记录这些证据，但不会自动继承该判定，也不会改变 DSV。
- 发布链继续使用 schema 7、随机 snapshot ID 和现有 resolver。本文是字段语义映射，不表示新旧 manifest 已自动互转或 Rust 实现已集成。
