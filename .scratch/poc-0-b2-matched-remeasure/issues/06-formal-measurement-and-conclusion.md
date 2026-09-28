# 06: 正式 release 测量、B2 结论与文档同步

**What to build:** 项目负责人拿到一份在 release 构建上按预登记协议正式测得的 B2 结论，能对照书面预测，并在综合报告、状态和路线图中看到一致的证据链与 MVP-3 前置条件。参见 [spec](../spec.md)。

**Blocked by:** 01, 03, 04, 05, 07, 08

**Status:** ready-for-agent

- [ ] 确认票据 01 的 ADR 与预测保存早于本次测量，并记录两者的 revision。
- [ ] Nautilus 计时模式采用票据 07 选定的模式，并在报告中引用其证据。
- [ ] 在 `--no-default-features` release 构建上运行判定负载与稳健性负载；原始比较报告归档到 POC-0 结果目录。
- [ ] 结论为 `adopt / defer / reject / unresolved` 之一，附理由、预测是否成立，以及结论对口径是否敏感。
- [ ] 更新 POC-0 综合报告的 B2 行与 B2 章节、票据 09 的 `Conclusion` 行与证据链接、STATUS 与 HANDOFF；路线图写明 B2 结论是 MVP-3 的前置条件。
- [ ] 声明结论只覆盖本机器、固定 Nautilus 版本和测过的负载，不构成生产 Engine 选型或真实 ETF 业绩验证。
- [ ] 完成独立 agent review 并处理发现；交接写明已验证命令、review 结果、开放问题和唯一建议下一步。
