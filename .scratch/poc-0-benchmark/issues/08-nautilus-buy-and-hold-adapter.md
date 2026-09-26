# 08: B2 Nautilus 跑通同一场景

**What to build:** 研究者可以在统一入口选择固定版本的 Nautilus Backend，通过平台语义的 Adapter 运行同一 S1 场景，并逐项比较账本与成本。

**Blocked by:** 07 B2 Fast Event 跑通 Buy & Hold。

**Status:** ready-for-agent

- [ ] Instrument、Order、Fill、账户和结果仍以项目语义呈现；Nautilus 类型及生命周期限制在 Adapter 内。
- [ ] 逐项报告订单、Fill、现金、持仓、交易成本与每日 NAV 的一致性；无法对齐的时序或撮合规则给出具体差异。
- [ ] 转换、初始化、事件处理及端到端耗时分开记录，保留原始样本和依赖版本。
- [ ] 依赖或语义确实无法运行时保存错误、复跑条件及 `unresolved`，不填造对比数字。
