# 08: B2 Nautilus 跑通同一场景

**What to build:** 研究者可以在统一入口选择固定版本的 Nautilus Backend，通过平台语义的 Adapter 运行同一 S1 场景，并逐项比较账本与成本。

**Blocked by:** 07 B2 Fast Event 跑通 Buy & Hold；13 POC-0 构建资源基线与轻量边界。

**Status:** resolved

**Conclusion:** S1 共同子集（Fill、现金、持仓、费用、每日 NAV）对拍通过。Nautilus 原生停牌拒单能力已由[票据 14](14-nautilus-halt-native-reject-isolation.md)证实。剩余的订单生命周期差异（Adapter 项目层拦单，Nautilus 实际提交 3 条 vs Rust 4 条）是已知且已解释的差异，按 ADR 0012 排除在 B2 判据之外；是否改用原生状态处理留给 MVP-4 Adapter。跨引擎吞吐不在本票裁决，由 [B2 remeasure 票据 06](../../poc-0-b2-matched-remeasure/issues/06-formal-measurement-and-conclusion.md) 在 S2/S3 上给出。本票不再有待补证据。

- [x] Instrument、Order、Fill、账户和结果仍以项目语义呈现；Nautilus 类型及生命周期限制在 Adapter 内。
- [x] 逐项报告订单、Fill、现金、持仓、交易成本与每日 NAV 的一致性；无法对齐的时序或撮合规则给出具体差异。
- [x] 转换、初始化、事件处理及端到端耗时分开记录，保留原始样本和依赖版本。
- [x] 依赖或语义确实无法运行时保存错误、复跑条件及 `unresolved`，不填造对比数字。
- [x] 安装/构建固定版本前由 `nautilus_preflight.py` 实测空间并按预计增量守住 10 GiB；不足时保存 `unresolved` 记录并拒绝执行。warm-cache 构建增量、耗时与身份已记录；cold build 为 Unknown，未清理缓存。

## Comments

2026-09-27：统一 `benchmark-poc0 --backend nautilus` 入口已加入，固定使用 `nautilus_trader==2.0.0rc5`。成功安装 macOS ARM64 CPython 3.12 官方 wheel 后，通过 `Equity`、开盘/收盘 `QuoteTick` 和下一开盘回调运行同一 S1 fixture。三笔成交均为 300 股、100.1 CNY、佣金 100 CNY；三笔分别在 A/C 的 Jan 13 open 和 B 的 Jan 14 open。直接从 Nautilus `Portfolio` 读取的逐日账户 cash/positions，及根据 Fill 与 close mark 得出的 NAV、佣金、滑点、总成本和期末权益均与 Rust 对拍通过（6 项中的 5 项）。Order lifecycle 唯一差异：B 的 Jan 13 HALTED 被适配为该日无 QuoteTick，Nautilus 保留一个 pending order；Rust 报告 HALTED rejection 后 Jan 14 新 retry order。订单事件数为 3 vs 4，因此整体仍 `unresolved`。报告保存全部对比数值和一条 conversion/init/event/end-to-end 原始测量样本；该单次样本不作性能结论。wheel 首次受 DNS 限制、随后成功的安装记录及 dev/release target 增量见 `poc/poc0-benchmark/results/nautilus-install-attempt-2026-09-27.json` 和两份 build JSON；全过程保留 10 GiB 空间余量，未清 target。完整结果与复跑入口见 POC README Nautilus 小节。

2026-09-27 后续修复与复审：Adapter 在 B 的 HALTED 日开盘执行项目状态门槛，记录 `quantity=0, reason=HALTED`，次日提交新 Nautilus 订单。此前把这条 Adapter 自行生成的拒单计入 Nautilus 原生订单一致性，错误地将票据标成 `resolved`；该结论已撤回。当前报告分别列出 Adapter 项目事件 4 条（与 Rust 4 条对齐）及 Nautilus 真实提交 3 条（与 Rust 4 条不一致）。Fill、现金、持仓、费用和每日 NAV 的固定样本对拍通过；原生 HALTED 拒单没有被 Nautilus 撮合引擎验证，跨引擎订单生命周期及吞吐结论仍 `unresolved`。时间边界与手算序列见 ADR 0012。

新增 `nautilus_preflight.py` 在 pip 安装与共享 target release 构建之前检查当前及预计剩余空间；强制过大构建估算的拒绝原始记录和成功预检/构建记录均入库。报告保存 1 次预热后 5 次转换、初始化、事件处理及端到端原始样本、median/p95/range、投影 checksum、依赖身份与冷构建 Unknown。五次重复仅支持 Nautilus 内部稳定性探针，不支持 Fast Event 对 Nautilus 的吞吐裁决。当前复核报告见 `poc/poc0-benchmark/results/nautilus-adapter-review-2026-09-27.json`；原两份报告保留为历史证据。

2026-09-28 重新评估：原 `unresolved` 的两项已各有归属。原生 HALT 拒单能力由票据 14 证实，但 S1 Adapter 仍自行拦单、未送入 `InstrumentStatus`，所以 3 vs 4 的提交数差异保持不变；它已被解释并按 ADR 0012 排除，不再是待证问题，接入原生状态需在 MVP-4 验证状态源可用时刻后决定。跨引擎吞吐已由 B2 remeasure 接管，S1 不在其判定负载内。
