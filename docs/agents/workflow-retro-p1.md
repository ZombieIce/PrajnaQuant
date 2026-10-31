# P1 工作流 Retro 基线

采集于2026-10-31；基于公开 GitHub 元数据/评论和固定 main `b8d89cf`。这是流程事实基线，不是提速结果；不追改旧验收。原响应/日志留本机，公开只保留下列工程元数据及来源。

## 周期与审查记录

PR创建→合并为 GitHub created_at/merged_at 的差；Claim→Merge 使用首次公开认领/开始 Prepare 评论时间作为代理，包含退回 triage、裁决和验证间隔，不能等同编码时间或人的等待。聊天内更早开始时间 Unknown。表内时间是相对持续时长，不涉及本地时区换算。

| Issue / PR | PR创建→合并 | 首次公开Claim/Prepare→合并 | 初审/复审公开记录 |
|---|---:|---:|---|
| [#297](https://github.com/ZombieIce/PrajnaQuant/issues/297) / [#357](https://github.com/ZombieIce/PrajnaQuant/pull/357) | 2h28m49s | 4h37m10s（[起点](https://github.com/ZombieIce/PrajnaQuant/issues/297#issuecomment-6486574196)） | [初审](https://github.com/ZombieIce/PrajnaQuant/pull/357#issuecomment-6489398695)，[复审1](https://github.com/ZombieIce/PrajnaQuant/pull/357#issuecomment-6490142919)，[复审2](https://github.com/ZombieIce/PrajnaQuant/pull/357#issuecomment-6490635483) |
| [#298](https://github.com/ZombieIce/PrajnaQuant/issues/298) / [#359](https://github.com/ZombieIce/PrajnaQuant/pull/359) | 1h13m30s | 2h27m49s（[起点](https://github.com/ZombieIce/PrajnaQuant/issues/298#issuecomment-6492730673)） | [初审](https://github.com/ZombieIce/PrajnaQuant/pull/359#issuecomment-6494372135)，[复审](https://github.com/ZombieIce/PrajnaQuant/pull/359#issuecomment-6495417803) |
| [#299](https://github.com/ZombieIce/PrajnaQuant/issues/299) / [#364](https://github.com/ZombieIce/PrajnaQuant/pull/364) | 2h19m38s | 2h44m31s（[起点](https://github.com/ZombieIce/PrajnaQuant/issues/299#issuecomment-6498771925)） | [初审](https://github.com/ZombieIce/PrajnaQuant/pull/364#issuecomment-6499870624)，[复审](https://github.com/ZombieIce/PrajnaQuant/pull/364#issuecomment-6502557828) |
| [#307](https://github.com/ZombieIce/PrajnaQuant/issues/307) / [#368](https://github.com/ZombieIce/PrajnaQuant/pull/368) | 0h34m39s | 1h02m20s（[起点](https://github.com/ZombieIce/PrajnaQuant/issues/307#issuecomment-6512546647)） | [初审](https://github.com/ZombieIce/PrajnaQuant/pull/368#issuecomment-6512989122)，[复审](https://github.com/ZombieIce/PrajnaQuant/pull/368#issuecomment-6513149329) |

这些是审查结果的发布时间，不是 reviewer 开始/结束工作的计时。修复周期须按对应 head 的发现/回应/新 ready 组合核对；评论数不等于返工次数。PR内分轴记录也不能算作多次返工。

## 范围审批与纯预算审批分开

- #297 的[复权输入绑定决定](https://github.com/ZombieIce/PrajnaQuant/issues/297#issuecomment-6486812951)及[拆分 T08a/T08b](https://github.com/ZombieIce/PrajnaQuant/issues/297#issuecomment-6487516859)改变契约/范围，属于真实范围裁决；不能作为应消除审批。其后固定 T08a 的[1400授权](https://github.com/ZombieIce/PrajnaQuant/issues/297#issuecomment-6487582826)及[3000修复预算](https://github.com/ZombieIce/PrajnaQuant/issues/297#issuecomment-6489759809)是总量授权，仍须保留原适用范围。
- #298 [1650授权](https://github.com/ZombieIce/PrajnaQuant/issues/298#issuecomment-6492932097)延续已描述的单纵向行为，归纯预算审批（聊天授权由实现者转录，不能推定聊天等待时长）。#299 [估算620且继续](https://github.com/ZombieIce/PrajnaQuant/issues/299#issuecomment-6498832354)，未在该记录申请预算例外。#307 [1500授权](https://github.com/ZombieIce/PrajnaQuant/issues/307#issuecomment-6512606747)固定全部原验收和单市场范围，归预算/保留单票决定。
- 本票只迁移获批的13张开放票：#302、#304、#305、#309–#318；它们原条款相同，原文及替代授权逐票留评论，见[#371决定](https://github.com/ZombieIce/PrajnaQuant/issues/371#issuecomment-6541421370)。#301现已完成不迁移，#286历史拆分保留。

## 有限旧票迁移索引

原条款统一为：“开工前重新估算含测试的改动量；预计超过约 800 行或涉及多个独立接口，先在本 Issue 评论提出进一步拆分，等负责人确认后再实施。”替代为[主要实现提醒/强化复核/明确硬预算规则](ticket-scope.md)，保留原文及原验收、依赖、排期、状态；这是负责人批准的有限名单，不迁移完成票。

| 开放票（记录时） | 原条款与替代授权 |
|---|---|
| #302 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/302#issuecomment-6541425865) |
| #304 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/304#issuecomment-6541426585) |
| #305 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/305#issuecomment-6541427376) |
| #309 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/309#issuecomment-6541427983) |
| #310 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/310#issuecomment-6541428766) |
| #311 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/311#issuecomment-6541429428) |
| #312 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/312#issuecomment-6541430006) |
| #313 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/313#issuecomment-6541430649) |
| #314 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/314#issuecomment-6541431308) |
| #315 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/315#issuecomment-6541432019) |
| #316 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/316#issuecomment-6541432768) |
| #317 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/317#issuecomment-6541433550) |
| #318 | [迁移记录](https://github.com/ZombieIce/PrajnaQuant/issues/318#issuecomment-6541434374) |

## 最近案例的环境、预算和交接

- [PR #369](https://github.com/ZombieIce/PrajnaQuant/pull/369)：公开记录含[初审](https://github.com/ZombieIce/PrajnaQuant/pull/369#issuecomment-6516239326)与三份后续审查记录（[1](https://github.com/ZombieIce/PrajnaQuant/pull/369#issuecomment-6516535441)、[2](https://github.com/ZombieIce/PrajnaQuant/pull/369#issuecomment-6516881332)、[3](https://github.com/ZombieIce/PrajnaQuant/pull/369#issuecomment-6517228569)）。按发现/修复head核对，而非把每次ready都算独立实现。权限与预算是否变化需引用具体裁决，不能从耗时反推。
- [PR #380 ready](https://github.com/ZombieIce/PrajnaQuant/pull/380#issuecomment-6537579544)发生在CI queued时，说明并行交接已有实例；本票统一执行方式，不将整段CI时长声称为新增节省。审查记录见[初审](https://github.com/ZombieIce/PrajnaQuant/pull/380#issuecomment-6537658392)。
- [PR #381](https://github.com/ZombieIce/PrajnaQuant/pull/381)：[初审](https://github.com/ZombieIce/PrajnaQuant/pull/381#issuecomment-6539156642)、[后续审查](https://github.com/ZombieIce/PrajnaQuant/pull/381#issuecomment-6541088558)和最终分轴记录（[Spec](https://github.com/ZombieIce/PrajnaQuant/pull/381#issuecomment-6541198119)、[Standards](https://github.com/ZombieIce/PrajnaQuant/pull/381#issuecomment-6541201925)）分开计量。最终[分类校正](https://github.com/ZombieIce/PrajnaQuant/pull/381#issuecomment-6541215915)为主要生产597/手写测试543/文档59/生成0=1199；[测试后续处置](https://github.com/ZombieIce/PrajnaQuant/pull/381#issuecomment-6541215559)明确受1200预算及head冻结约束。这是避免质量证据受数字挤压的依据，不撤销历史非阻塞结论。
- [PR #382](https://github.com/ZombieIce/PrajnaQuant/pull/382)：[初审缺口](https://github.com/ZombieIce/PrajnaQuant/pull/382#issuecomment-6540607694)是ps受限导致生产任务入口E2E未完成；[协调者补验](https://github.com/ZombieIce/PrajnaQuant/pull/382#issuecomment-6540832961)记录正常权限环境4/4通过，[复审](https://github.com/ZombieIce/PrajnaQuant/pull/382#issuecomment-6540943733)覆盖该证据。环境失败不转成通过；检查环境与实际执行是两个动作。

补充判读：#369 [1000行/窗口范围/放宽本地E2E权限的集中授权](https://github.com/ZombieIce/PrajnaQuant/issues/295#issuecomment-6515148168)包含真实契约与环境决定，不能全算纯行数审批；[PR正文](https://github.com/ZombieIce/PrajnaQuant/pull/369)保留早期ps失败和旧断言修复，后由正常权限环境全套合成验证覆盖。#380 [初审](https://github.com/ZombieIce/PrajnaQuant/pull/380#issuecomment-6537658392)未指出范围/预算例外，不从它的短周期推断已优化。#381 [PR证据](https://github.com/ZombieIce/PrajnaQuant/pull/381)显示环境补验后还发现可选字段省略/null和execution级存储路径断言错误；这些是应修复的测试假设，不因起初环境失败而掩盖。#382 [复审](https://github.com/ZombieIce/PrajnaQuant/pull/382#issuecomment-6540943733)记录一次803行授权、最终800行及分类；[授权](https://github.com/ZombieIce/PrajnaQuant/issues/301#issuecomment-6540622805)同时处理E2E执行环境，纯数字与权限应分开计量。

人等候、Token、模型费用、构建CPU时间、可计费runner成本均 Unknown；不能从评论间隔/job wall time伪造。分配/排队的可见时间戳只作GitHub事件差值。

## 指定 CI run 逐步 wall time

来源为 GitHub Actions jobs REST 的 started_at/completed_at（连接器简化输出不含时间，使用gh只读补取）。没有抓取或公开原日志/研究明细；下表是整个step的墙钟差，不是CPU时间，缓存下载/排队和子进程构建不混称。

| run | job wall秒 | run创建→job开始秒（含调度/排队，非纯队列） | runner |
|---|---:|---:|---|
| [run 41183582562](https://github.com/ZombieIce/PrajnaQuant/actions/runs/41183582562) | 926 | 2 | ubuntu-latest |
| [run 41181043720](https://github.com/ZombieIce/PrajnaQuant/actions/runs/41181043720) | 885 | 2 | ubuntu-latest |
| [run 41103679447](https://github.com/ZombieIce/PrajnaQuant/actions/runs/41103679447) | 890 | 3 | ubuntu-latest |

| step | 41183582562 | 41181043720 | 41103679447 |
|---|---:|---:|---:|
| 1 Set up job | 1 | 2 | 1 |
| 2 Run actions/checkout@v4 | 1 | 2 | 2 |
| 3 Run dtolnay/rust-toolchain@master | 9 | 9 | 10 |
| 4 Run Swatinem/rust-cache@v2 | 18 | 20 | 20 |
| 5 Guard - lightweight path must not pull in bundled DuckDB | 4 | 3 | 3 |
| 6 Guard - prajna-domain and prajna-data must not pull in legacy warehouse dependencies | 1 | 1 | 2 |
| 7 Guard - shared Tencent transport must stay independent of legacy and JSON identities | 1 | 0 | 0 |
| 8 Guard - prajna-experiment must not depend on legacy execution stacks | 0 | 1 | 1 |
| 9 Guard - prajna-research must not depend on the legacy warehouse or quant engine | 0 | 0 | 0 |
| 10 Guard - prajna-py must not depend on DuckDB or quant-research | 0 | 0 | 0 |
| 11 Run cargo fmt --all -- --check | 2 | 2 | 2 |
| 12 Run cargo clippy -p quant-research --no-default-features --all-targets --locked -- -D warnings | 9 | 8 | 8 |
| 13 Run cargo test -p quant-research --no-default-features --locked | 27 | 25 | 25 |
| 14 Guard - prajna-account must only use Domain and lightweight dependencies | 0 | 0 | 0 |
| 15 Run cargo clippy -p prajna-account --all-targets --locked -- -D warnings | 1 | 1 | 1 |
| 16 Run cargo test -p prajna-account --locked | 1 | 1 | 1 |
| 17 Guard - prajna-adapter must remain Rust-only and lightweight | 0 | 0 | 0 |
| 18 Run cargo clippy -p prajna-adapter --all-targets --locked -- -D warnings | 11 | 10 | 10 |
| 19 Run cargo test -p prajna-adapter --locked | 30 | 27 | 27 |
| 20 Run cargo clippy -p prajna-domain --all-targets --locked -- -D warnings | 0 | 1 | 1 |
| 21 Run cargo test -p prajna-domain --locked | 1 | 0 | 0 |
| 22 Run cargo clippy -p prajna-data --all-targets --locked -- -D warnings | 6 | 6 | 6 |
| 23 Run cargo test -p prajna-data --locked | 131 | 127 | 130 |
| 24 Run cargo clippy -p prajna-research --all-targets --locked -- -D warnings | 5 | 5 | 4 |
| 25 Run cargo test -p prajna-research --locked | 63 | 56 | 60 |
| 26 Run cargo clippy -p prajna-experiment --all-targets --locked -- -D warnings | 12 | 13 | 12 |
| 27 Run actions/setup-python@v5 | 1 | 0 | 0 |
| 28 Run cargo test -p prajna-experiment --locked -- --include-ignored | 166 | 174 | 168 |
| 29 A16 per-Run research timing regression (no Nautilus required) | 28 | 26 | 25 |
| 30 Run cargo clippy -p prajna-py --all-targets --locked -- -D warnings | 15 | 13 | 13 |
| 31 Run cargo test -p prajna-py --locked | 34 | 33 | 34 |
| 32 Build prajna extension in a fresh Python 3.12 venv | 19 | 16 | 18 |
| 33 Prajna Python tests | 298 | 274 | 277 |
| 34 Run cargo check -p quant-research --no-default-features --features b3-pyo3 --all-targets --locked | 9 | 8 | 9 |
| 35 POC Python tests (Nautilus not installed) | 2 | 2 | 1 |
| 36 Tencent fallback collector (synthetic responses only) | 2 | 2 | 2 |
| 37 MVP-1 Vector golden tests | 9 | 7 | 7 |
| 38 MVP-2 benchmark report tests | 0 | 1 | 1 |
| 39 MVP-3 independent Fast Event golden tests | 5 | 4 | 4 |
| 40 MVP-4 synthetic fixture preparation (no Nautilus required) | 0 | 0 | 0 |
| 41 MVP-4 cost report and resource gate (no Nautilus required) | 0 | 0 | 0 |
| 42 MVP-5 record-only throughput report tests | 1 | 1 | 1 |
| 82 Post Run actions/setup-python@v5 | 0 | 0 | 0 |
| 83 Post Run Swatinem/rust-cache@v2 | 0 | 0 | 0 |
| 84 Post Run actions/checkout@v4 | 0 | 0 | 0 |
| 85 Complete job | 1 | 0 | 1 |

数据测试127–131秒、Experiment166–174秒、Prajna Python step274–298秒。后者混合fixture构建与Python执行，不能据总耗时推定Python解释器是瓶颈。Rust/Cargo和Node缓存已存在；本票不将已有缓存认作新增功能。

## 后续3–5张可比票回看

选择相同产品阶段、相近主要实现量/风险、相同runner/工具链及相同合成验证覆盖的连续3–5张票，保留选择理由与排除项；规则/基础设施票和量化高风险业务票分别比较。每票引用已有PR记录，不强制另写长retro。

记录：PR创建→合并、首次公开Claim→Merge、实质修复周期（按SHA和发现）、真实范围/纯行数审批、环境导致的返工、各必需入口实际执行与零匹配/skip、合并后缺陷及严重程度。质量反指标包括遗漏验收、回归、放宽门禁和公开证据泄露；速度变快但质量变差不算改善。

CI/Review重叠：Review没有开始/结束记录时标Unknown，不能用ready→评论当纯审查工时；有实际区间时取与CI运行区间交集并报告分母，区分queued与执行。最终head覆盖、验证证据复用和有效skip各自记录。比较中位数及范围，小样本只作方向性线索，不宣称因果或Token节省。

两轮实质返工、重复审批/环境阻塞时专项回顾不阻塞修复；每轮最多三个可验证行动，可零建议，优先工具/测试，新增规则有证据。本票交付方法即可，不等未来票完成，不建定时提醒。

当前行动：将前检及生产E2E前移；接入app读取侧/零匹配门禁；按已批准规则迁移开放票并以实际审查安排替代纯行数暂停。尚未声称这些动作已改善业务交付周期。
