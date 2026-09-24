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
