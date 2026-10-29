# ADR 0032：沪深结构化公司行动事实版本

- 状态：Accepted，2026-10-29；实现范围 [#360](https://github.com/ZombieIce/PrajnaQuant/issues/360)，父 spec [#286](https://github.com/ZombieIce/PrajnaQuant/issues/286) D2/D5。
- 边界：离线事实版本；覆盖页面由 #361 承接，账户接入与权益数学由 #307 承接。本契约不修改 MVP-6 normalizer、身份或存档，不宣称真实数据验收或公告全集完备。

## 输入与覆盖语义

Rust `corporate_facts::Input` schema 1 包含本地 `observed_at` 和非空 `coverage`。每项固定六位证券代码及真实 XSHG/XSHE Venue、`kind`（stock/etf）、闭区间 `start/end`、`state`、`reason`、可空来源/历史可用时刻及 `facts`。同证券区间不得重叠，证券身份不跨 Venue 合并；身份由导入者明确提供，不从代码猜交易所或重建历史身份。

来源复用 `CalendarSource` 的 source_id/source_version/source_ref；值存在时必须完整非空。`available_at` 为来源支持的历史可用 RFC3339 时刻，缺证据用 null；本地采集 `observed_at` 不代替它，也不从日期虚构时刻。来源声明是输入事实而非独立审计证明，消费者须按用途评价其证据。

| state | 含义与约束 |
| --- | --- |
| covered | 该证券/范围有已提供事件，每个事件的必要字段完整；范围声明本身须有来源与历史可用时刻 |
| no_events | 来源对该证券/范围明确声明无事件；facts 为空，声明有来源与历史可用时刻 |
| missing_facts | 已知范围有事实缺口，reason 必须说明；允许空事件集或带 null 字段的部分事件 |
| unknown | 未建立范围事实，facts 为空；查询未覆盖证券/范围也返回 unknown |

“已覆盖”仅是所声明范围内的结构化事实，不是所有历史公告完备证明。查询闭区间必须完整包含于一条范围声明内；跨范围/超界返回 unknown，不拼接成完整证明。响应同时给查询范围和原始 coverage（含其范围及事件）；事件不按查询子区间裁剪，消费者必须使用原声明范围。每个事件额外输出 missing 字段名；null 不视为零、无事件或已知发布时间。

## 事件与精确字段

`event_id` 在同证券版本内唯一（含分离区间）；修订属于新版本。`kind` 复用 `CorporateActionKind`。事件来源和 available_at 与范围声明分别保留，不能用范围来源补成事件完整。

- dividend 必需 record_date、ex_date、pay_date、cash_per_share。
- share_conversion（含拆分/合并）必需 effective_date、share_ratio；登记/除权日期若来源提供则保留，不隐式填值。
- liquidation 沿用既有类型，必需 effective_date、pay_date、cash_per_share；不进入清算账本。

现金复用 Domain Amount、比例复用 Quantity 的精确 scale-18 解析，只接受正数字符串；不经 f64，不从价格跳变反推。不允许把现金放到折算事件或把份额比例放到现金事件。登记不晚于除权，除权/生效不晚于发放；股息以 ex_date、其他以 effective_date 为覆盖锚点，发放可在范围外。缺必要日期/金额/来源/可用时刻的事件只能位于 missing_facts，covered 输入直接失败。该层不推算交易 Session、不进行税/舍入、权益或绩效数学。

## 发布、读取与兼容

调用方显式指定独立本机 root；保护旧 MVP-6 根，目的子路径不得经过 symlink。输入 JSON 拒绝未知字段；非法/冲突输入在发布前失败。原始文件字节 sha256 形成 `caf:sha256:<64 lowercase hex>`，文件保存在 `corporate-facts/<hex>.json`，来源、未知事实和观察时间均参与身份，格式变化也产生新版本。

通过同目录临时文件、sync、persist_noclobber 发布；同字节幂等，不覆写已有版本。读取固定版本时重算 hash 并重新校验契约；缺版本、损坏、非法输入分别返回结构化错误。没有隐式 latest，也没有将版本自动绑定到账本或研究身份。

```sh
cargo run -p prajna-data --bin prajna-corporate-facts -- import --root /LOCAL/FACTS --input /LOCAL/actions.json
cargo run -p prajna-data --bin prajna-corporate-facts -- read --root /LOCAL/FACTS --version caf:sha256:HEX --instrument 510300.XSHG --start 2026-01-01 --end 2026-12-31
```

Rust 消费者使用 import/read/query；CLI stdout 为 schema 1 JSON。真实输入、详细覆盖和版本身份只留本机，不复制到聊天或 GitHub。公开验证使用合成 fixture。

## 验证范围

`prajna-data/tests/corporate_facts.rs` 通过生产 CLI 验证输入→持久版本→读取输出、四态、沪深股票/ETF、精确金额/比例、缺项、幂等/修订/旧版、非法/冲突/损坏及受保护根/symlink。`prajna-experiment/tests/momentum.rs` 在 missing_facts 存在时调用真实动量研究 CLI，仍保存并读取与独立手算价格因子一致的结果。该隔离不表示总回报账本可忽略缺口；账本接入尚未交付。未涉及账户/执行数学，不扩大旧 golden 的结论。
