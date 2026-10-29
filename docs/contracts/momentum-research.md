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
