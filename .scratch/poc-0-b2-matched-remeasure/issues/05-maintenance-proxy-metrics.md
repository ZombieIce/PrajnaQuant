# 05: 维护成本代理指标

**What to build:** 项目负责人在 B2 比较报告中看到 Fast Event 与 Nautilus Adapter 的维护成本代理指标，与速度/RSS 一起阅读；这些指标只记录，不参与判定。参见 [spec](../spec.md)。

**Blocked by:** 02

**Status:** resolved

- [x] 报告新增维护成本节，列出 Fast Event POC 路径与 Nautilus Adapter 各自的非测试代码行数和测试数，并保存路径范围与计数方法。
- [x] 列出新增直接依赖数（Cargo crate / Python 包）与 Python 传递依赖解析数，并注明统计方法与范围。
- [x] 列出已知的不可消除语义差异数，每项引用出处（ADR 0012）。
- [x] 判定函数不读取该节；测试改变代理值后判定结果不变。
- [x] 无法统计的项标 `Unknown`，不按零处理。

实现记录：`b2_matched.py` 将 `maintenance_cost_proxies` 写入比较报告，判定输入保持不变。计数器按共享 Rust POC 模块/CLI 与 Nautilus Adapter 源文件范围统计非空、非注释行，并排除 Rust 内联测试区；测试数按明确列出的 B2 Rust 用例与两个 Python 测试文件统计。Python 依赖快照保存闭包包名和版本，并注明它不是完整 lock；固定 Nautilus pin 或主动依赖版本不匹配、主动依赖未安装、依赖元数据无法解析时闭包标为 `Unknown`。独立 review 发现并修复的风险包括缺失依赖误计、版本不匹配未校验、测试名漂移和票据状态标签不规范。

## 验收记录（2026-09-28）

五项验收标准在当前 HEAD 通过：统一报告 `poc/poc0-benchmark/results/b2-matched-decision-loads-2026-09-28.json` 保存源码/测试路径与计数方法、直接依赖、按 active marker 解析的传递发行包快照和 ADR 0012 语义差异引用；代理字段不进入 `decide()`。报告中的传递依赖数为已解析的 0（不是缺失数据），未安装或依赖冲突时测试验证 `status: unknown`、`count: null`。独立只读复核无剩余票据 05 缺项；`.venv/bin/python -m unittest discover -s poc/poc0-benchmark -p test_b2_matched.py -v` 为 17/17 通过。该验收不裁决票据 04/07/06 的稳健性、reset 模式及整体 B2 结论。
