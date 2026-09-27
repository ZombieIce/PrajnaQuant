# 08: B2 Nautilus 跑通同一场景

**What to build:** 研究者可以在统一入口选择固定版本的 Nautilus Backend，通过平台语义的 Adapter 运行同一 S1 场景，并逐项比较账本与成本。

**Blocked by:** 07 B2 Fast Event 跑通 Buy & Hold；13 POC-0 构建资源基线与轻量边界。

**Status:** resolved (fixed S1 fixture; Adapter status gate documented)

- [x] Instrument、Order、Fill、账户和结果仍以项目语义呈现；Nautilus 类型及生命周期限制在 Adapter 内。
- [x] 逐项报告订单、Fill、现金、持仓、交易成本与每日 NAV 的一致性；无法对齐的时序或撮合规则给出具体差异。
- [x] 转换、初始化、事件处理及端到端耗时分开记录，保留原始样本和依赖版本。
- [x] 依赖或语义确实无法运行时保存错误、复跑条件及 `unresolved`，不填造对比数字。
- [x] 安装/构建固定版本前检查至少 10 GiB 当前及预计剩余空间；wheel 与 dev/release wrapper build 的 target 增量、耗时及缓存身份已记录，未清理任何缓存。

## Comments

2026-09-27：统一 `benchmark-poc0 --backend nautilus` 入口已加入，固定使用 `nautilus_trader==2.0.0rc5`。成功安装 macOS ARM64 CPython 3.12 官方 wheel 后，通过 `Equity`、开盘/收盘 `QuoteTick` 和下一开盘回调运行同一 S1 fixture。三笔成交均为 300 股、100.1 CNY、佣金 100 CNY；三笔分别在 A/C 的 Jan 13 open 和 B 的 Jan 14 open。直接从 Nautilus `Portfolio` 读取的逐日账户 cash/positions，及根据 Fill 与 close mark 得出的 NAV、佣金、滑点、总成本和期末权益均与 Rust 对拍通过（6 项中的 5 项）。Order lifecycle 唯一差异：B 的 Jan 13 HALTED 被适配为该日无 QuoteTick，Nautilus 保留一个 pending order；Rust 报告 HALTED rejection 后 Jan 14 新 retry order。订单事件数为 3 vs 4，因此整体仍 `unresolved`。报告保存全部对比数值和一条 conversion/init/event/end-to-end 原始测量样本；该单次样本不作性能结论。wheel 首次受 DNS 限制、随后成功的安装记录及 dev/release target 增量见 `poc/poc0-benchmark/results/nautilus-install-attempt-2026-09-27.json` 和两份 build JSON；全过程保留 10 GiB 空间余量，未清 target。完整结果与复跑入口见 POC README Nautilus 小节。

2026-09-27 后续修复：保留 B 在 HALTED 日的合成行情事件，在开盘回调用 08:50 已知的项目执行状态阻止 Nautilus 下单并记录 `quantity=0, reason=HALTED` 的项目拒单；Jan 14 可交易开盘再提交新的 Nautilus 订单。订单比较改为逐条核对 `decision_date`、`attempt_date`、证券、方向、数量及原因，不再只比较事件数。固定 fixture 的 6/6 项均通过，`unresolved_reasons=[]`；原失败报告保留，新证据见 `poc/poc0-benchmark/results/nautilus-adapter-comparison-status-gated-2026-09-27.json`。拒单发生在 Adapter 状态门槛，并非 Nautilus 撮合引擎原生拒单；其他合成行情与费用映射边界仍见 README。状态 `resolved` 仅指此 S1 固定场景语义对拍，不代表生产 Accurate Engine 或吞吐选型。
