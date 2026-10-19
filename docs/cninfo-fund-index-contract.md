# cninfo 基金公告索引契约

对应 [#202](https://github.com/ZombieIce/PrajnaQuant/issues/202)，按 #171 数据源契约 v3 和 #172 负责人决定 §3 实现。`prajna-data::cninfo` 是 Rust 采集/离线处理模块，不改变旧 `szse-etf-frozen@1/@2` 或已发布 Dataset。真实采集和真实附件效果尚未验证；仓库只有合成 fixture。

## 本机入口

先在本机安装 Poppler，使 `pdftotext` 可执行。无转换器、加密/扫描件、未识别排版和歧义字段均保留 unknown，不使用 OCR、价格跳变或简称补全事实。用户须在国内直连网络执行；程序禁用环境代理，不自动判定出口地理位置。

```sh
cargo run -p prajna-data --bin prajna-cninfo --locked --offline -- \
  2022-10-01 2026-09-30 /absolute/local/path/historical-codes.txt \
  "<operator-confirmed direct domestic egress description>"
```

代码文件每行一个历史候选代码；不能以查询成功名单替代历史候选集。代码文件由后续 TDX 名录流程提供。输出固定在 `~/PrajnaData/mvp-6/`，根目录不允许符号链接：

- `raw/`：原始响应与 RawStore 追加来源记录，包括失败响应；记录 URL、参数、采集时刻、状态、长度、SHA-256、重试次序、操作者提供的直连网络路径说明及 #172 授权结论。
- `acceptance/cninfo/<DSV hex>/`：`coverage.json`、`cninfo_index.parquet` 和标准 `manifests/` 文件。stdout/stderr 不输出真实代码、URL、hash、覆盖数或正文。发现缺口退出非零，报告仍保存在本机。
- `cninfo.lock`：同一命令的进程互斥；进程内所有请求串行、间隔至少 1 秒，失败退避 1/2/4 秒，最多重试三次。首次 401/403/501、验证码或登录控制即停止整个来源；不跟随重定向，不伪造 Referer/Cookie，不切换协议或代理规避控制。Raw 持久化失败立即中止请求。

所有真实 Raw、索引、报告、hash、计数和日志只留本机，不提交 GitHub。命令记录的是操作者报告的网络路径，不独立验证出口地理位置；它不证明来源全集或第三方数据完整性。

## 查询与覆盖

先分页查询 `hisAnnouncement/query` 的 `column=fund`、声明日期窗口，按公告 ID 去重，保留 158xxx/159xxx 的管理人 orgId。候选集与发现的深市 ETF 代码取并集；已终止/没有 orgId 的候选仍留在报告中，不使用 `topSearch`。每个 `(code, orgId)` 再以 `stock=code,orgId` 查询，核对证券及管理人身份。

每个查询验证连续页号、稳定声明总数、唯一 ID 总数相等；同 ID 内容矛盾拒绝。重叠页可去重，但不能用请求行数代替唯一 ID 总数。提前结束、空页但未达总数、数量变化、非法响应和安全页数上限均标 unresolved。这里只证明本次响应的一致性，不能证明公告全集或来源正确。不同 orgId 不自动解释为代码复用，也不合并生命周期。

`FundIndex.unknown` 按基金/公告列出 orgId、全称、查询、附件或字段缺口；`IndexDataset.unknown` 保留发现阶段失败。下游必须检查数据集与基金缺口，再按字段和期间处理；`require_usable()` 仅检查索引覆盖缺口，不是交易可用性或完整总回报许可。

## 标题、名称与时间

`cninfo_title_rules_v2` 为多标签分类：收益分配、折算、终止、清算/剩余财产、停牌、复牌、持有人大会、上市交易公告书、标的指数/更名、定期报告。季度/年度使用稳定类别词，不要求年份或季度为阿拉伯数字；计票日停牌可同时具有大会、停牌及复牌标签。

`full_names` 只接受标题中的明确完整名称及明确引号内的更名目标，不从 `secName` 场内简称、管理人名字或缺失 PDF 猜测。无法识别的名称和更名/指数变更需复核。序列按公告时间和 ID 排序，每项保留公告 ID、原始元数据 Raw hash；同一时刻不同全称报冲突。重复名称事实仍保留来源。

`announced_ms` 是原始公告时间；`available_at` 明确为 null。后续 Catalog 必须按 P6 和交易日历计算下一 Session 收盘可用时间：不得把索引中的公告时间直接当可用时间，不得向前填充首条全称。首条事实以前、未解析变更以后及日历未覆盖期间按 unknown 排除并报告。`observed_at` 始终只是本地采集时间。

#205 的 `RiskSignals::build_from_cninfo` / Experiment `pit.cninfo_index` 已连接风险提示分类与 P6：取公告时间的上海日期，再用 Catalog 日历推到下一 Session close，大会/计票停牌/合同终止/清算类别产生持久 Risk Exit 输入。它检查索引/基金 unknown、分类版本、窗口与 Raw 引用，不据标题断言开盘停牌区间或清算金额。完整状态与时间边界见 [派生输入契约](derived-etf-input-contract.md)；仅合成验证。

## PDF 字段与公司行动

`cninfo_pdf_fields_v1` 使用本机 `pdftotext -layout -enc UTF-8`，保守匹配带标签的日期与每份金额；字段重复且有歧义、无效日期或转换失败为 null。数额保留十进制字符串。每个非 null 字段的 `fieldSources` 都引用原始 PDF Raw SHA-256；整份 PDF hash 也进入 Manifest inputs。转换器环境差异若改变提取结果，会改变输出 logical hash 与 DSV。

- 收益分配：`payDate`（现金红利发放日）。不从 PDF 提取分红金额或登记日。
- 清算/剩余财产分配：`payDate`、明确标注的 `liquidationCashPerShare`。总资产/总金额不能当每份金额。
- 停复牌：`suspensionStart` 与 `resumptionDate`。日期边界的开盘覆盖解释留给状态构建层；不将复牌日解释为整日停牌。未写明日内时间不推测日内区间。

事件 envelope 标为 `cninfo_corporateActions_structured_v1`，`corporateActions` 使用现有 v1 字段名。该结构是待连接事实：`recordDate/exDate/cashPerShare/effectiveDate` 等未提供值为 null，不能直接通过现有严格 Normalizer 或入账。#204 连接新浪金额/比例及三方一致性；登记日必须按除权日前一 Session 推导，不能从 PDF 登记日覆盖。未知清算金额不置零，不删除持仓。此票不改变成交时序、成本、现金或 NAV。

## Dataset Version

`cninfo_index@1` 是独立单行 Arrow JSON payload 表（键 `id=cninfo-index`），payload 包含查询窗口、来源集合、逐基金公告/分类/名称/事件与 unknown 报告。`IndexDataset::manifest` 使用现有标准 `Manifest::new`、受限 JCS、schema fingerprint 和 logical hash；Normalizer 身份为 `cninfo-fund-index@1`。分类规则、PDF 提取器、v1 事件 schema 与登记日规则都进入 `config_sha256`，不放在不参与身份的 provenance 中。

它不是 Bars Dataset，不注册到旧 Bars Normalizer，也不支持旧 `rebuild_dataset` 自动重建。可从本机冻结响应重放 `PageSet`、`FundIndex`、`extract_fields` 和 `manifest`；后续消费者必须显式连接名称/P6、事件、Session 与行情契约，不将本票当 #203～#206 或真实收益验收完成。

## 合成验证

`cargo test -p prajna-data --test cninfo --locked --offline` 使用虚构元数据、固定 PDF 转换器边界、Mock HTTP，不联网；覆盖 orgId 查询参数、请求间隔、访问拒绝留痕、分页冲突/重复、158xxx、中文年份季度、多标签、全称变更序列、PDF 字段缺失/冲突及来源 hash、完整采集到事件/Manifest 的连接、版本影响 DSV。Poppler 外部工具及真实 PDF 排版不由该合成测试认证。

## MVP-6 固定名单 v1（#229）

`Collector::collect_fixed_list` 是新增入口，不改变既有 `Collector::collect`、`IndexDataset`、Normalizer 或 Catalog 身份。调用方提供明确候选代码、TDX 简称与存续期；代码必须在 158xxx/159xxx 范围。它先用 `topSearch` 的完全匹配建议解析全部 `orgId`，没有精确结果时才按简称和最多 365 日窗口分页查公告，并只保留完全匹配 `secCode` 的行。每个 `(code, orgId)` 独立按最多 365 日窗口查询；每个区间的 `PageSet` 必须完整，否则返回 `pagination_incomplete`。响应仍只冻结进本机 Raw Store，含请求参数和 `observed_at`。

新增入口识别上市交易公告书、基金合同/招募说明书、份额折算/拆分、持有人大会计票/表决结果、终止上市/终止基金合同与清算报告。采集 PDF 时把 `purpose=mvp6_pdf` 和该公告 ID 写入冻结 provenance，字段引用据此证明 PDF 属于该公告，而不是只靠清单声明。定期报告被排除。基金合同/招募说明书只在上市交易公告书的 PDF 文本没有同时写明基金类别和标的指数（标题后须有非空值），或调用方显式请求对应类别时下载；字段值缺失、为空、仅转引其他文档（如“详见基金合同”“参阅招募说明书”）、为占位内容（如“待定”“未知”“N/A”）或无法确认时按“未写明”处理，不抑制下载。此门禁保守处理含转引的值（即使转引前也有名称），不是事实字段提取器；真实 PDF 排版仍需本机验证。附件仍要求 `finalpage/` 路径且禁止 `..`。访问控制与限速沿用同一 `fetch` 实现。

`configs/mvp-6/{identity,actions,events}.csv` 目前只定义 Issue #229 指定的列头，**没有带入未经证据复核的真实事实**。公开事实表可收录公告中的身份与公司行动；Raw 对象、请求日志和 hash 不得进入仓库。`Mvp6Facts::from_csv` 检查固定列。`validate_and_convert` 检查冻结开市日历、生命周期不重叠与必填字段，并要求每个非空事实字段各自有一条已校验的字段级引用；可选字段为空时不要求引用，原文未支持的值不能出现。

`validate_and_convert` 产出 `Mvp6Session`：`catalog_lifecycles`（`code, org_id, legal_name, listing_date, termination_date, fund_category, tracked_index, evidence_level, source_announcement_id, source_page`）、`corporate_actions` 和 `risk_events`。`corporate_actions` 使用独立的新版本 envelope `{"schema":"cninfo_corporateActions_structured_v2","version":2,"actions":[...]}`；每个 action 含 `code, kind, announcement_date, record_date, ex_date, pay_date, cash_per_share, split_ratio, liquidation_per_share, evidence_level, source_announcement_id, source_page`，日期为 ISO-8601、可选值为 null。`risk_events` 每行含 `code, kind, announcement_date, effective_date, session_part, evidence_level, source_announcement_id, source_page`，`session_part` ∈ `open/morning/full_day`。这与上文 #202 `IndexDataset` 的 v1 `cninfo_corporateActions_structured_v1`/`corporateActions` envelope 是不同版本，不复用其字段命名。

字段级引用契约（`citations.csv`）为 `fact_source,field,announcement_id,page,raw_sha256,excerpt,verified_by,verified_at`。`fact_source` 是该事实行的 `source_announcement_id`；`announcement_id` 是实际陈述该字段的冻结文档，可以与 `fact_source` 不同（例如 `org_id` 来自冻结的索引响应，上市日来自上市交易公告书 PDF）。校验分两层：

1. `Citation::from_store` / `read_citations_csv` 只能从本机 Raw Store 构造引用：Raw 必须存在且 hash 匹配；其字节或请求 provenance 必须引用 `announcement_id`（MVP-6 采集 PDF 时由 collector 写入该 provenance）；`page` 必须落在冻结文档内（PDF 页数来自本机文本转换器，非 PDF 只有第 1 页）；`excerpt` 去除空白后必须出现在该页；`verified_by` 必须是操作者标识，`verified_at` 必须是 RFC 3339 且不得早于 Raw 采集时间。引用字段的字段名私有，不能用结构体字面量绕过校验。
2. `validate_and_convert` 要求每个非空字段都有引用，且记录值可从 `excerpt` 还原：日期匹配 ISO 或中文写法，金额/比例匹配相等数值，其余文本按去空白后字面包含；`kind` 必须是 excerpt 能分类出的公告类别（`split` 与 `conversion` 共用拆分/折算类别）；`fund_category`、`session_part` 是分类代码而非原文，仍要求逐字 excerpt 在该页并由具名验证者声明映射。空字段、未知字段或重复字段的引用会被拒绝；事实行的 `source_page` 必须与至少一条字段引用一致，否则报错。

`crosscheck_v4` 只返回以下差异：上市日 vs 首个有成交 Bar、终止日 vs 最后有成交 Bar、除息/折算日 `bar_sources.reference_price` 相对前一有成交 close 至少变动一个 0.001 价格刻度、停牌生效日是否对应 keyed no-trade，以及 `action_outside_lifecycle` / `suspension_outside_lifecycle`（日期不在任何生命周期内）和 `action_without_prior_close`（生命周期内没有前一有成交 close）。所有比较都按事实身份生命周期限定：被复用代码的更早生命周期 Bar 不会移动到下一生命周期的上市/终止边界，也不会成为其前一 close 基线。它不改输入；keyed no-trade 观测不移动上市/终止边界。

本机固定名单命令读取 `code,short_name,active_start,active_end` CSV，结果和 Raw 都写入 `~/PrajnaData/mvp-6/`：

```sh
cargo run -p prajna-data --bin prajna-cninfo-fixed --locked --offline -- \
  /absolute/local/path/fixed-candidates.csv \
  "<operator-confirmed direct domestic egress description>" \
  "fund_contract,prospectus"
```

公开事实表的本地工作流使用 `~/PrajnaData/mvp-6/facts/`：`identity.csv`、`actions.csv`、`events.csv` 使用与 `configs/mvp-6/` 相同的列头；`citations.csv` 使用上面的字段级引用列，`excerpt` 是引用页上的逐字文本，`raw_sha256` 取 `collection.json` 中对应公告索引或 PDF 的冻结 hash。该目录存在时，上述命令在采集后读取 `acceptance/mvp6-coverage.json` 的已发布 v4 DSV，加载 `sessions`、`bars`、`tdx_code_table`，再写入同一 acceptance 子目录的 `facts-session.json` 与本机 `crosscheck.json`；facts 目录不存在时只写 `collection.json`。v4 中“在 code table 内但没有 Bar”且日期落在事实身份生命周期内的记录被视为 keyed no-trade；生命周期外不生成该观测。

合成边界测试位于 `cargo test -p prajna-data --test cninfo_mvp6 --locked --offline`；它使用 mock HTTP、合成文本/PDF 引用、合成事实和手写 Parquet/manifest 的合成 v4 数据集，不连接真实来源。测试覆盖 Raw 不存在、公告 ID 不绑定、重复引用、页号越界、excerpt 不在引用页、excerpt 不支持记录值、空/未知字段引用、空验证者、验证时间早于采集、代码复用生命周期限定、v4 投影读取和 `crosscheck.json` 落盘。该测试不证明真实 v4 数据、真实公告、真实 PDF 排版或真实交叉核对结果。

## MVP-6 corporate action Dataset v6（#223）

`szse-etf-frozen@6` 是新增的只读消费端，不改变 v4/v5 的配置或输出身份。它把同一批 v4 行情 Raw（日历、TDX、可选的腾讯补位）与 `facts-session.json`（`contract=mvp-6-cninfo-facts-v1`）一起发布；`facts-session.dsv` 必须等于本次消费的已发布 v4 DSV，Normalizer 的 `config_sha256` 也绑定该 DSV，因此公告事实不能配到另一版行情。

- `catalog_lifecycles` 决定本期范围：只有落在公告上市日（含）到终止日（含）之间的 TDX 有价 Bar 进入 Dataset。某只有事实身份的 ETF 若在公告存续期外仍有有价 Bar，整只 ETF 从本次发布排除并写 `excluded_etfs`，其余 ETF 继续（P27-19 第 2 条）。
- `corporate_actions` 必须是 `cninfo_corporateActions_structured_v2`。`kind` 的 `dividend`、`split`/`conversion`、`liquidation` 分别映射为 `dividend`、`share_conversion`、`liquidation`；日期为 ISO-8601，可选字段保持 null。公告只有日期时，`announced_at` 取公告日收盘、`available_at` 取公告日之后第一个 Session 收盘（P6）。动作行的 `source_raw_sha256` 是 facts-session Raw，`normalization_version` 为 `szse-corporate-action@2`。
- P21 以 TDX 除权参考价对照生命周期内上一有成交 close：参考价至少变动一个 0.001 刻度即视为跳变。除息/折算日有公告且有跳变时，还用公告的每份现金或折算比例计算期望参考价（`previous_close - cash_per_share` 或 `previous_close / split_ratio`），差异超过一个 0.001 刻度记 `announced_action_amount_mismatch`。有公告无跳变、无公告有跳变、金额/比例与跳变幅度不符、登记日与除息日前一 Session 不符、公告必填字段缺失及 `UNKNOWN` 参考价均记受影响 Session 为 `UNKNOWN`，写入 `p21_mismatches`/`unknown_sessions`，但**不阻断发布**（P27-19 第 3 条）。复合动作或金额缺失时只检查跳变存在性。总回报运行必须把 `p21.unknown_sessions` 映射为 `UNKNOWN` 并执行 UNKNOWN 底线；本 Dataset 只提供逐格报告，不代替运行门禁。生命周期首个有价 Session 没有可比前收，不算来源缺口。`checked_no_event` 只表示该 Session 未观察到跳变且没有公告动作，不证明不存在公司行动（ADR 0023）。
- 新浪 qfq/hfq 是可选二级对拍：`acceptance/sina/collection.json` 中完成且 Raw 存在的配对才进入输入。完全没有任何新浪输入时为 `not_present`（不是“无事件”，也不是来源缺口）；已有部分输入但某只缺 qfq/hfq 配对时记 `sina_adjust_factor.source_gaps`；采集报告存在但所有结果都是 gap/未尝试、因而没有 Raw 进入 Dataset 时，Dataset 仍是 `not_present`，本机覆盖报告把 `sina_optional_crosscheck` 升级为 `source_gap` 并保留 `selection_gaps`，不把失败采集当作“未采集”。qfq/hfq 不一致、与公告金额/比例或 TDX 跳变不一致只列入 `mismatches`，不阻断。代码复用的因子序列按 Catalog 生命周期分别推导，累计值重置不当作减少错误。
- 本机命令只读取本机冻结输入，只写本机 Raw 与 acceptance，不做网络请求：

  ```sh
  cargo run -p prajna-data --bin prajna-mvp6-publish-actions --locked --offline -- \
    "$HOME/PrajnaData/mvp-6" "<published-v4-dsv>" \
    "$HOME/PrajnaData/mvp-6/acceptance/cninfo-fixed/<run>/facts-session.json"
  ```

  结果写入 `normalized/` 和 manifest；`acceptance/mvp6-coverage.json` 增加 `corporate_action_dataset`、`p21`、`p21_mismatches`、`excluded_etfs`、`corporate_actions_with_missing_fields`、`tdx_out_of_scope_priced_bars`、`sina_optional_crosscheck` 与 `sina_input_selection`，不覆盖 v4 的 `dsv`，也不把 `mvp6_complete` 置真。非法新浪来源记录记为可报告的 source gap，不中止 Dataset。stdout/stderr 不输出真实代码、DSV、hash 或覆盖数。

合成端到端测试位于 `cargo test -p prajna-data --test mvp6_corporate_actions --locked --offline`：3 只以上 ETF × 10 Session 覆盖分红、折算、公告字段缺失、无法解释的参考价跳变、代码复用、新浪缺失/不一致、存续期外 Bar 排除，并验证两次构建的 DSV/Manifest core 相同。仅合成数据验证；未执行真实公告与行情发布，不构成 P21 或 MVP-6 验收通过。

