# 价格因子研究产品契约（动量 / 波动率 / 严格均线趋势）

`/research/momentum` → 本机 `GET/POST /api/v1/momentum-research` →
`prajna-momentum --lake <root>`（JSON stdin/stdout）。HTTP 适配不执行量化计算；
CLI 复用 `prajna-research` Momentum v1、Volatility v1 与 #299 新增的
`trend_filter_strict` v1，以及 FactorCache。路由、CLI 和任务名沿用 momentum
以保留 #298 的旧调用者、任务身份与已存报告；服务配置 `PRAJNA_MOMENTUM_CLI`，
Dataset 根与研究结果共用显式本机 lake。旧 `/factors` 信号报告和旧诊断身份保持原样。

命令：`list` 列出已保存研究；`read` 需要报告 `id`；`submit` 需要 `request`：

```json
{"op":"submit","request":{"dsv":"<fixed DSV>","instrument":"510300.XSHG",
 "adjusted":{"510300.XSHG":"<fixed adjusted version>"},"windows":[20,60,120]}}
```

同一请求可增加可选的 `factor` 选择新增因子种类：

```json
{"op":"submit","request":{"dsv":"<fixed DSV>","instrument":"510300.XSHG",
 "adjusted":{"510300.XSHG":"<fixed adjusted version>"},"windows":[20,60],
 "factor":"volatility"}}
```

```json
{"op":"submit","request":{"dsv":"<fixed DSV>","instrument":"510300.XSHG",
 "adjusted":{"510300.XSHG":"<fixed adjusted version>"},"windows":[20,60],
 "factor":"trend_filter_strict"}}
```

`factor` 只接受 `volatility` 与 `trend_filter_strict`。省略 `factor` 即旧
Momentum 研究：该字段不序列化进报告，所以旧请求与已存报告的字节、id 和
`momentum-research@2` 语义都不变；不接受显式 `momentum` 作为第二种编码。
`volatility` 复用既有 Volatility v1；`trend_filter_strict` 是新增的独立
`FactorKind` v1（`close_t > mean(close_(t-n+1..t))`，相等输出 0），不是把
既有 TrendFilter v1 的 `>=` 悄悄改成 `>`。

`trend_filter_strict` 的数值相等政策：研究价以十进制字符串导入、在 Rust 中以
f64 计算，十进制相等的均值通常不能被 f64 精确表示。因此 v1 仅在
`close - mean > 1e-12 * max(1, |close|, |mean|)` 时输出 1，容差内视为相等并输出 0；
这是对 Spec“均线相等不算高于”的数值实现，避免同一常数序列在不同窗口下因均值
舍入而产生 1。该容差属于 `trend_filter_strict` v1 语义，修改它需要升版本。
`TrendFilter` v1 继续使用原始 f64 `>=`，其身份、Cache 和已存结果不变。

`instrument` 与 `pool: {id, version}` 二选一。池解析不可变已发布版本，
只接受 Dataset 中的 ETF；可在同一国内 Session 轴上研究沪深池，
不表示混合 Venue 策略执行已经交付。每个成员必须恰好绑定一个显式调整版本，
核对 raw DSV/证券且拒绝空序列；不选 latest。窗口为 1–16 个正整数，
按升序去重；Momentum 与 `trend_filter_strict` 最小 1，Volatility v1 最小 2。
页面按因子给默认窗口：动量 20/60/120、波动率 20/60、严格趋势 20/60。
无 forward_return 或评价标签输入。

事件例：Session 0/1/2 调整 close 为 10/11/12，Momentum 窗口 1 时输出
`insufficient_window / 0.1 / 12/11-1`。严格趋势窗口 2 时 Session 1
（close=11 对 mean(10,11)）为 1；若 Session 1 close 改为 10 则均线相等输出 0，
而有 `>=` 语义的 TrendFilter v1 仍输出 1。每个有效值只使用当时及过去
Session close；不是前瞻收益。缺失保持完整 Session 轴，不删行、不填零；
研究 `has_bar` 取决于调整价存在，避免把原始价存在误作调整输入完整。
窗口不足优先于 missing_input。Rust 保存 `definition`（kind/version/params）、
value、status、available_at_ns、状态计数和 10 桶分布；纳秒时刻用十进制字符串
防止浏览器整数精度损失。HTTP 保留 CLI JSON 字节，避免薄适配重新编码浮点数
改变末位。页面只读取并绘图。

报告 `direction`：Momentum 为 `higher_is_stronger`，Volatility 为
`lower_is_stronger`，`trend_filter_strict` 为 `binary_close_above_ma`
（1 表示 close 严格高于均线，相等为 0）。旧 Momentum 报告保持
`semantics:"momentum-research@2"`；新增两类报告为 `semantics:"factor-research@1"`。
直方图最后桶直接采用实际最大值，避免重算边界的浮点舍入漏计；常数样本归最后桶，
空有效样本没有桶。旧版报告仍只读保留；重新提交旧请求仍生成原 Momentum 报告。

调整序列按其明确声明的收盘可用假设绑定 Session close；历史发布时间为
Unknown，observed_at 仅为本地观察。报告保存供应商来源/版本、窗口、方向、
缺失规则、追溯修订与可用性假设、固定成员及 Static Universe 幸存者偏差；
不是严格 PIT，也不是公司行动总回报。详情可打开固定输入与来源。

Cache 输入身份使用 `momentum-adjusted@1`、原始 DSV 与全部成员调整版本的
受限 JCS 摘要；新增 Factor 的 kind/version/params 继续进入既有 FactorCache key，
因此 `trend_filter_strict` 与 TrendFilter v1 不共用缓存条目。旧 Momentum、
Volatility、TrendFilter 的 Cache key 与已存条目不变；旧 Cache key/消费者不变。

研究报告保存至 `momentum-research/<id>.json`，id 为固定 JSON 编码 report
payload 字节的 SHA-256（包含浮点值，不冒称受限 JCS）。先写唯一临时文件、
同步并原子不覆盖发布；相同报告复用，读取核验 payload 摘要，损坏不静默
覆盖或当作空报告。报告列表仍显示损坏条目，打开时报错。拒绝受保护 lake、
路径穿越与报告目录/文件符号链接。全部明细/来源/身份只留本机，公开仅合成验证。

验证：`cargo test -p prajna-experiment --test momentum --locked --offline` 覆盖
真实 CLI、130 Session 线性价独立 golden、Momentum 20/60/120/1、Cache 复用、
未来改动、缺失、显式绑定、沪深已发布池、非法窗口/股票、损坏与符号链接；
#299 另覆盖 Volatility v1 的逐行手算样本方差、严格趋势的均线相等（1 Session、
2 Session 与变化窗口的十进制相等边界）、预热、内部缺失、调整序列修订不改过去值、
非法窗口与未知 `factor`；数值相等回归使用非精确可表示常数 `3.826`，经真实
CLI 覆盖 1/2/3/20/60/120 全部窗口输出 0。`prajna-research` 的 factor/base/cache_key
测试核对新 kind 独立 canonical 身份与 Cache key，以及 TrendFilter v1 的 `>=`
行为/身份未变。PyO3 消费者支持该 kind：`prajna-py` clippy/test 与 Python
`Factor("trend_filter_strict", {"n": 2})` 的 read/cache golden 通过。`apps/web/e2e/factor-research.spec.ts` 通过 #341 真实服务/CLI/发布 fixture
选择波动率与严格均线趋势、改窗口、保存并核对持久报告与页面值/图表；
`momentum.spec.ts` 继续覆盖旧动量路径。不 mock 成功。
无浏览器 fixture 根环境变量时，exporter 在自有临时 lake 实际执行导出，
使 CI `--include-ignored` 可运行；浏览器仍用显式根并核查导出文件。
极小正负混合/常数 golden 逐样本核对只落一个桶，计数和等于有效样本数。
真实行情验证仍未执行。没有策略成交或账本变更。

## Durable tasks

HTTP submit additionally requires `idempotency_key` (1-128 characters) and returns
an existing serial worker task. The page polls the durable
`/api/v1/tasks/experiments/{id}` record and reads the report via `result.report_id`.
All three factor kinds use the existing `prajna_momentum` task type; backtest task
identities remain unchanged. An identical key/definition reuses the task; a changed
definition returns 409. Dataset and output roots must be the same lake. The existing
owned process-group cleanup and restart recovery apply: queued/running tasks become
interrupted, completed reports and failure errors remain readable. Cancel/retry
remain deferred under #315/#316. CLI `--input <command.json>` reads the frozen task
definition. Reports also save existing ExecutionIdentity Git revision, dirty-tree
digest and reproducibility flag, Rust compiler and target provenance. Run the CLI
from a Git checkout with its compiler provenance available. Recovery tests cover
both pending states and retained success/error facts; production browser tests
cross-check task, frozen request and report, including factor selection, failed
computation and duplicate conflict.

## 未来收益评价 v1

`submit.request` 可显式选择 `"evaluation":"next_open_to_forward_open_v1"`。
省略字段仍生成原报告（字段不序列化），旧报告可读取；未知版本/`forward_return`
输入被拒绝。新报告用 `semantics:"factor-evaluation@2"`，原因子语义保存在
`factor_semantics`；已存的 `factor-evaluation@1` 报告保持原字节可读。该扩展
适用于上述三类因子，沿用同一 Dataset、成员、每成员复权版本和来源/可用性
假设，不从 latest 或另一种价格口径取标签。

Rust `prajna_research::evaluation` 独立于 FactorKind、FactorCache 和 Decision。
`evaluation.version="next_open_to_forward_open@1"`；每成员每个 Session 都有
1/5/20 三行标签：T=Session[i] 收盘评分，entry=Session[i+1] open，
exit=Session[i+1+N] open，`value=exit/entry-1`，不扣交易费。
日轴是 Dataset 的完整交易所 Session（沪深严格对齐），不是有效 bar 日期并集。
例如 open 为 10、12、15 的连续三 Session，第一日 H=1 标签为 15/12-1=0.25；
第二日的 H=1 缺未来退出 Session，返回 null；不能以 close 代替 open。
标签不意味着端点一定可成交，也不是含现金分红的账户总回报。

输出保存 `instrument/date/horizon/entry_date/exit_date/value/status`；端点 Session
存在而该复权 open 缺失时为 `missing_entry_open` 或 `missing_exit_open`；尾端不足为
`insufficient_future_sessions`；极端有限价产生非有限比值为 `non_finite_return`。
这些状态均 value=null，不跳缺失日期、不填零；中间缺 bar 不压缩期限。
非正/非有限 open、错误成员/日历、重复成员或不严格递增 Session 拒绝计算。

每个 factor 的 `relationships` 保存三个期限的有效配对点（证券、日期、因子值、
后续收益）、expected/effective/missing 数量与 missing_ratio，均由 Rust 生成。
分母是固定成员 × 全部报告 Session，预热因子和尾端标签缺失均计入配对缺失；
这与标签本身缺失率不同。页面只画已配对的散点，展示端点/null/原因与样本数，
不重算收益或统计；静态事后 Universe 与小样本限制继续披露。
重叠标签不复利为 NAV；IC/Rank IC 与样本诊断见下一节，分组结果仍不在本扩展内。
调整 open 只传到评价模块，绝不写入因子 Panel/评分或策略信息集；复权版本的
回溯性假设仍然存在，不能因未来数据隔离测试通过就声称严格历史 PIT。

定向证据入口：`prajna-research/tests/evaluation.rs` 独立合成端点/多成员/周末/
缺失/数值边界 golden；`tests/strategy.rs` 未来 open 改价只改变标签而不改变因子与
Decision；`prajna-experiment/tests/momentum.rs` 生产 CLI 持久/read/旧版兼容与未来
复权 open 隔离；`apps/web/e2e/evaluation.spec.ts` 真实 API/队列/CLI/持久报告/页面
散点样本闭环。全部使用合成输入，真实数据与真实标签未验证。

## 截面 IC 与样本诊断 v1（#301）

`evaluation:"next_open_to_forward_open_v1"` 的报告在每个因子保存 `statistics`
数组（1/5/20 期限各一项），版本 `cross_section_ic@1`；输入是同一固定成员、
Session 轴和每成员复权版本下的因子值与标签值，不读取原始价，也不新增
Factor/Cache/Decision 输入。
逐期保存 `date`、`expected_count`、`paired_count`、`pearson_ic`、`rank_ic`、
`status`；期限级保存因子/标签可用计数、paired/missing 总数、缺失比例和
`status_counts`。Pearson IC 对配对原始值计算；Rank IC 对两侧取 0 基平均秩
（并列共享平均位置）后计算 Pearson。至少 3 个有效配对才计算，不足为双 null +
`insufficient_cross_section`；一侧只有 1 个不同值为 `zero_variance_factor` /
`zero_variance_label`（因子优先）；非有限的那一项为 null + `non_finite_statistic`。
任何 null 都不是 0。

汇总 `summary` 只用有定义的逐期 IC：可计算/不可计算截面数、Pearson 均值、
Rank 均值/样本标准差/IR/严格正比例；`reasons` 只列 null 字段原因。零标准差 IR
为 null + `zero_dispersion`；单期标准差/IR 为 null + `insufficient_series`；
无可定义截面的均值/比例为 null + `no_evaluated_cross_sections`。例：3 成员因子
[1,2,3]、H=1 标签 [4,9,16] 时 Pearson=0.9954022744967959、Rank=1；4 成员因子
[1,2,2,3] 与标签 [1,2,4,8] 的平均秩 Rank=0.9486832980505138（独立手算 golden）。

报告语义 `factor-evaluation@2`，`evaluation.statistics_version` 为
`cross_section_ic@1`；`factor-evaluation@1` 旧报告与省略 `evaluation` 的旧请求
均不变。页面只读取时序、汇总、样本与原因并绘图，缺失显示 `—`。IC 是重叠标签的
关联诊断，不是可交易 NAV 或稳健有效性证明；小池截面只说明可计算，不说明显著。
分组后续收益仍由 #302 承担。验证见 `prajna-research/tests/evaluation.rs` 手算
golden、`prajna-experiment/tests/momentum.rs` 真实 CLI 单成员 null 持久化和
`apps/web/e2e/ic.spec.ts` 三成员生产页面闭环；均为合成输入，真实行情未验证。
