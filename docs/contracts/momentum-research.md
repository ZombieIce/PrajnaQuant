# 动量研究产品契约

`/research/momentum` → 本机 `GET/POST /api/v1/momentum-research` →
`prajna-momentum --lake <root>`（JSON stdin/stdout）。HTTP 适配不执行量化计算；
CLI 复用 `prajna-research` Momentum v1 与 FactorCache。服务配置
`PRAJNA_MOMENTUM_CLI`；Dataset 根与研究结果共用显式本机 lake。
旧 `/factors` 信号报告和旧诊断身份保持原样。

命令：`list` 列出已保存研究；`read` 需要报告 `id`；`submit` 需要 `request`：

```json
{"op":"submit","request":{"dsv":"<fixed DSV>","instrument":"510300.XSHG",
 "adjusted":{"510300.XSHG":"<fixed adjusted version>"},"windows":[20,60,120]}}
```

`instrument` 与 `pool: {id, version}` 二选一。池解析不可变已发布版本，
只接受 Dataset 中的 ETF；可在同一国内 Session 轴上研究沪深池，
不表示混合 Venue 策略执行已经交付。每个成员必须恰好绑定一个显式调整版本，
核对 raw DSV/证券且拒绝空序列；不选 latest。窗口为 1–16 个正整数，
按升序去重，默认 20/60/120 Session。无 forward_return 或评价标签输入。

事件例：Session 0/1/2 调整 close 为 10/11/12，窗口 1 时输出
`insufficient_window / 0.1 / (12/11-1)`。每个有效值使用当时与第 n 个过去
Session close；不是前瞻收益。方向为越高越强。缺失保持完整 Session 轴，
不删行、不填零；研究 `has_bar` 取决于调整价存在，避免把原始价存在误作
调整输入完整。窗口不足优先于 missing_input，内部无关 Session 缺失不影响
只依赖两端的 Momentum。Rust 保存 value、status、available_at_ns、状态计数
和 10 桶分布；纳秒时刻用十进制字符串防止浏览器整数精度损失。HTTP 保留
CLI JSON 字节，避免薄适配重新编码浮点数改变末位。页面只读取并绘图。直方图最后桶包含最大值，常数样本归最后桶，
空有效样本没有桶。

调整序列按其明确声明的收盘可用假设绑定 Session close；历史发布时间为
Unknown，observed_at 仅为本地观察。报告保存供应商来源/版本、窗口、方向、
缺失规则、追溯修订与可用性假设、固定成员及 Static Universe 幸存者偏差；
不是严格 PIT，也不是公司行动总回报。详情可打开固定输入与来源。

Cache 输入身份使用 `momentum-adjusted@1`、原始 DSV 与全部成员调整版本的
受限 JCS 摘要，Universe 与已注册 Factor 定义继续进入既有 Cache key，
防止不同调整修订共用旧值。旧 Cache key/消费者不变。

研究报告保存至 `momentum-research/<id>.json`，id 为固定 JSON 编码 report
payload 字节的 SHA-256（包含浮点值，不冒称受限 JCS）。先写唯一临时文件、
同步并原子不覆盖发布；相同报告复用，读取核验 payload 摘要，损坏不静默
覆盖或当作空报告。报告列表仍显示损坏条目，打开时报错。拒绝受保护 lake、
路径穿越与报告目录/文件符号链接。全部明细/来源/身份只留本机，公开仅合成验证。

验证：`cargo test -p prajna-experiment --test momentum --locked --offline` 覆盖
真实 CLI、130 Session 线性价独立 golden、20/60/120/1、Cache 复用、未来改动、
缺失、显式绑定、沪深已发布池、非法窗口/股票、损坏与符号链接。
`apps/web/e2e/momentum.spec.ts` 使用 #341 真实服务/CLI/发布 fixture，
核对持久报告与页面值、图表、缺失、来源、刷新后打开和损坏错误；不 mock 成功。
真实行情验证仍未执行。没有策略成交或账本变更。

## Durable tasks

HTTP submit additionally requires `idempotency_key` (1-128 characters) and returns
an existing serial worker task. The page polls the durable
`/api/v1/tasks/experiments/{id}` record and reads the report via `result.report_id`.
Momentum tasks use `prajna_momentum`; backtest task identities remain unchanged.
An identical key/definition reuses the task; a changed definition returns 409.
Dataset and output roots must be the same lake. The existing owned process-group
cleanup and restart recovery apply: queued/running tasks become interrupted,
completed reports and failure errors remain readable. Cancel/retry remain deferred
under #315/#316. CLI `--input <command.json>` reads the frozen task definition.
Reports also save existing ExecutionIdentity Git revision, dirty-tree digest and
reproducibility flag, Rust compiler and target provenance. Run the CLI from a Git
checkout with its compiler provenance available. Recovery tests cover both pending
states and retained success/error facts; production browser tests cross-check task,
frozen request and report, including failed computation and duplicate conflict.
