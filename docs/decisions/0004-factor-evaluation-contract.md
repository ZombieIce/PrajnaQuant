# ADR 0004: 因子评价标签与可缺失统计

- Status: Accepted
- Date: 2026-09-24

## Context

因子评价把收盘后可知的分数与未来收益标签关联。旧接口只含 close-to-close 标签和 Rank IC；当截面不足时缺少清晰状态，coverage 分母也不明确。前端将重叠标签收益复利展示为累计净值，容易被误读成可交易策略表现。

## Decision

- 因子分数只由 T 日收盘及之前数据产生，评价标签不进入回测评分。
- 默认标签 `next_open_to_forward_open`：T 日收盘后生成信号，下一市场日开盘作为入场，H 个市场日区间后开盘作为退出。评价日历优先使用快照 manifest 指定的开市日历，否则使用输入 ETF 行情日期并集，并在报告中记录 `calendar_basis`。
- 保留 `close_to_close` 作为明确标注的诊断口径；其 H 按单只证券有效 bar 序号推进，不等同市场交易日。
- 任一标签所需 bar 缺失时，该证券该日标签为缺失，不插值，不假设可以成交。coverage 分母由调用方按期望 universe 提供；无分母时 coverage 为 null。
- 横截面有效标签数量小于 `max(quantiles, 3)` 时不计算 IC 或分组收益。缺失统计量序列化为 null，评价状态标明 `insufficient_cross_section` 或 `no_valid_ic`，不以 0 代替。
- 输出 Pearson IC 和 Spearman Rank IC。Q 高－Q 低是前瞻标签差，不是成本后的可交易组合收益；报告与页面不得将逐期标签复利为 NAV。
- 额外报告相邻报告日同证券因子分数 Pearson 自相关和 Top 分位成员更换比例。该比例不是订单或组合换手。

## Consequences

因子报告 JSON 增加标签方法、日历来源、评价状态、coverage/missing 计数、Pearson IC 和稳定性字段，原有旧报告字段以默认值兼容读取；部分统计字段现在可为 null。前端因子详情展示逐期标签和缺失值，不再展示伪累计曲线。

市场日历若来自不完整快照或观察 bar 日期并集，不能证明全市场没有共同缺失日期。真实数据覆盖仍须由独立日历和历史 Universe 证据验证。该 ADR 不定义基准收益、超额收益或信息比率。


## MVP-7 新平台显式扩展（#300）

新平台不依赖旧 `quant-research` 评价器。可选
`next_open_to_forward_open_v1` 请求生成 `next_open_to_forward_open@1` 标签及
`factor-evaluation@1` 研究报告；省略时保留旧请求和报告身份。
按 Dataset 的完整、严格对齐 Session 轴确定 i+1/i+1+N 端点，使用因子绑定的
同一供应商复权版本 open，不扣交易费，不采用观察 bar 日期并集回退。
缺端点或未来 Session 不足为 null，保存端点与原因；标签不进入 Factor/Cache/
Decision。散点有效配对数量与缺失分母由 Rust 生成，页面不复利标签为 NAV。
此处不改变上文旧报告的诊断口径、IC/分组契约或历史结论；新平台 IC 已由 #301
交付，分组仍由后续独立票承担。完整输入/结果/兼容及验证入口见
[价格因子研究契约](../contracts/momentum-research.md#未来收益评价-v1)。

## MVP-7 新平台截面 IC 扩展（#301）

新平台在 `next_open_to_forward_open_v1` 标签之上增加独立版本化
`cross_section_ic@1`：逐 Session 截面 Pearson IC 与 Spearman Rank IC（并列取
平均秩后作 Pearson），至少 3 个有效配对才计算（沿用本 ADR `max(quantiles,3)`，
新平台无分组故为 3）。逐日保存有效配对与状态，期限级保存因子/标签可用计数和
缺失比例。不足 3 对、常数因子、常数标签与非有限统计分别为
`insufficient_cross_section`、`zero_variance_factor`、`zero_variance_label`、
`non_finite_statistic`，相应值为 null，不以 0 代替；常数判定在阈值之后且因子
优先，秩对并列值取平均位置。
汇总只使用有定义的逐期 IC：Pearson 均值，Rank 均值、样本标准差（n-1）、
IR=均值/标准差（标准差为 0 时 null + `zero_dispersion`）和严格大于 0 的比例；
没有可定义截面或仅单期给出 `no_evaluated_cross_sections` 或
`insufficient_series`。时间序列长度不构成统计显著性；几只 ETF 的 IC 只是研究
诊断，不包装成稳健有效性证明，也不复利重叠标签为 NAV。

新报告语义为 `factor-evaluation@2` 并保存 `statistics_version`；已存的
`factor-evaluation@1` 报告继续按原字节读取，页面在缺少统计字段时保持旧展示。
本扩展只进入评价报告，不进入 Factor/Cache/Decision 或策略；分组后续收益仍由
独立票交付。旧 `quant-research` 的 IC/分组报告与口径不变。
