# 07: Nautilus reset 一致性验证与计时模式选定

**What to build:** 项目负责人能从证据看到，正式测量采用的 Nautilus 计时模式（每 Run 重置引擎，或回退为每 Run 新建引擎）是按 ADR 0013 的触发条件实测选定的，而不是未经验证就采用回退。票据 02 在未测试 reset 的情况下直接采用了回退模式，本票补齐这一前提。参见 [spec](../spec.md)。

**Blocked by:** 03, 04

**Status:** ready-for-agent

- [ ] 在固定 Nautilus 版本的公开 `BacktestEngine` 接口下，实现 reset 模式：worker 内一次性加入 venue/instrument/数据；每 Run 重置引擎并加入新的策略实例；不改变策略参数、成本或 T 收盘信号 → 下一可用 open 的执行时序。
- [ ] 对 S2 3×10、S3 3×130、S2/S3 64×252 四个负载，在同一 worker 内连续运行至少 3 次 reset Run，每次投影 checksum 须同时等于新建引擎投影；两个判定负载还须等于独立 golden。
- [ ] 一致性检查覆盖 ADR 0012 以外的共同子集字段：订单/Fill、现金、持仓、成本、每日 NAV、PortfolioResult；用例能发现跨 Run 的状态残留（账户、仓位、挂单、时钟、缓存）。
- [ ] 模式选定规则：四个负载全部一致时，正式测量统一用 reset 模式；任一负载不一致时统一用回退模式，报告列出不一致的负载、首个差异字段和原因。不按负载混用模式。
- [ ] 比较报告的"所用 Nautilus 模式"字段引用本票的测试证据（Nautilus 版本、revision、checksum 列表），不再写"未验证"。
- [ ] 若 reset 模式被选定，在本票的 `## Answer` 中说明：02 的 S2 回退测量不能作为正式判定依据，由 06 按 reset 模式重测。
- [ ] 测试在固定 Nautilus 版本不可用时跳过，报告标 `unresolved`；不断言绝对耗时。
