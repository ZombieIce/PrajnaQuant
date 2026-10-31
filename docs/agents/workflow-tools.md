# 工作流辅助工具

适用 #371 P1：一次性、窄范围命令，Python 3.12 标准库 + 已登录 `gh`；不运行后台调度。规则权威仍为 [scope](ticket-scope.md)、[tracker](issue-tracker.md) 与 [verification](verification.md)，集中授权见 [#371](https://github.com/ZombieIce/PrajnaQuant/issues/371#issuecomment-6541421370)。

## 先看证据，再做人工判断

```sh
python3.12 scripts/workflow.py inspect --repo ZombieIce/PrajnaQuant --issue 371 --actor ZombieIce
python3.12 scripts/workflow.py inspect --repo ZombieIce/PrajnaQuant --issue 371 --actor ZombieIce --pr <PR>
python3.12 scripts/workflow.py size --base <固定base> --head <固定head> --estimate-upper <首次获批估算上沿> --hard-budget <明确硬预算>
python3.12 scripts/workflow.py preflight --browser <Chromium可执行文件>
```

`inspect` 只访问明确仓库/Issue/PR：检查 open、唯一 ready-for-agent、认领冲突、正文和原生依赖、closed/completed 前置、当前 head 的 ready/review 声明及 required checks。`not_planned` 不满足前置；404 不推定为空，缺权限/元数据/状态竞态分别报告。正文只解析 `Blocked by` 中的前导依赖列表，括号内的 PR/来源不算 blocker。跨仓库依赖报告待核实，不将同号本仓票代入。

评论采用严格可识别声明：`**Implementer:** Ready for independent review at <SHA>.`；review 索引行 `**Independent reviewer:** Reviewed head: <SHA>`。支持 7–40 位代码 SHA；无法识别报缺口，不从评论任意出现的 SHA 猜结论。声明只证明存在，不证明独立上下文、发现解决、验收完整或审查有效。使用新的异形声明须人工核对；不要为工具通过而补造审查。

工具只汇总机械证据，不输出合并许可。人工依 tracker 区分“已交接 / 已审查但 CI pending / 可交负责人合并”，最后一种须核对有效独立审查、所有发现回应、票内验收及当前必需 CI。状态只读一次快照；head/Issue 在读取期间变化要求刷新，无轮询机器人。

`size` 只读已跟踪文件在两个固定 commit 间的差异，关闭 rename 检测，以新增+删除计量。不会递归扫描湖/研究目录。输出总量与分类：主要实现、测试候选、辅助文档、生成、待核实；二进制无行数单列，不当成零工作量。Rust 含内嵌测试的文件保守记待核实，需人工按内容细分；文件名/目录不是业务契约豁免。纯文档/Skill 票传 `--primary-docs` 将其交付文档记主要实现。当前复合票中仅作为规则交付的部分文档应人工上调，保留原工具结果及校正依据。工具给800/2000及可选首次估算两倍提醒，另报明确硬预算是否突破；未提供估算/硬预算则为null，不将提醒点变成许可。二进制/待核实内容及分类校正仍须人工处理，工具不执行预算许可或判定拆票。

返回码：0=已完成对应机械命令（不等于审查通过）；1=缺口/拒绝/测试失败；2=未知、权限、输入或工具错误。JSON `result`/`reason` 区分原因，原始 GitHub 响应和日志不回显。输出文件路径仅为代码路径；真实研究输入/身份不能传入命令或复制到公开证据。

## 显式标签清理

```sh
# 默认预览
python3.12 scripts/workflow.py cleanup --repo ZombieIce/PrajnaQuant --issue <N>
# 明确选择的已闭票，负责人授权后执行
python3.12 scripts/workflow.py cleanup --repo ZombieIce/PrajnaQuant --issue <N> --apply
```

只逐个 DELETE 五种 triage 标签；不 PATCH 全标签列表，不关闭开放票，不改业务标签、关闭原因、正文或评论。执行前及每次写前重新读取 state/reason；执行后再核对。权限不足原样报告，部分已删除数量可在重跑时核对。GitHub 标签 DELETE 与 Issue reopen 不提供跨资源事务；检查之后发生的并发 reopen 可能被后检发现，报告 race 并交人工处理，不声称原子性，也不自动改回关闭状态。此写入口不接默认 CI，不对无关票测试写入。

## 零匹配与执行记录

```sh
python3.12 scripts/workflow.py run --kind cargo --executor implementer \
  --selection comparison_api::tests --log /tmp/app-read.log --output /tmp/app-read.json \
  -- cargo test -p quant-research --features app --lib comparison_api::tests --locked --offline
python3.12 scripts/workflow.py run --kind unittest --executor implementer \
  --selection tests.test_workflow --log /tmp/workflow-tests.log \
  -- python3.12 -m unittest tests.test_workflow -v
```

支持 Cargo libtest、Python unittest、Playwright list reporter 汇总；不认识的输出失败，单纯 build/discovery 不算执行。记录显式命令/选择、执行者、代码 SHA、环境、数量、结果和 wall time；工作区尚有改动时 SHA 只是锚点，交接需说明 dirty 范围及对最终 head 的适用依据。原始日志必须在仓库外，文件独占创建，不覆盖历史证据；子输出不回显，敏感内容仍仅本机保存。显式 `--timeout` 超时终止本次拥有的进程组；启动前及运行中监测10GiB磁盘保留线，不足拒绝/停止并记录exit125、原因及最小余量。只控制本次拥有的进程组，不连接已有服务。

- 零执行默认失败；明确允许空集时传 `--allow-empty '适用理由'`，仍须有可信汇总，不能把缺日志当空集通过。
- Cargo 默认 ignored 测试单列，实际执行正数即可；选择要求 ignored 时显式 `-- --ignored --exact`。手写 fixture 的生产浏览器准备已接入此记录器，四个导出必须实际执行，且保留原产物检查。
- unittest/Playwright 有 skip 时传 `--allow-skips '具体允许的范围/原因'` 才能通过；全部 skipped 仍按零执行失败。不能把“允许可选 Backend skip”推定所有必需用例可跳过。
- 汇总数量不证明每个必需场景执行。多选择/多 harness 的必跑项按验收映射独立记录，调用者检查命令、数量与 skip 明细。

## 前端 PR 验证与门禁

[ci.yml](../../.github/workflows/ci.yml) 是命令权威。`core` 保留原 lightweight 全部守卫、fmt/clippy、Rust、Python/PyO3、Golden 与兼容验证；不启用 docs-only 快路由。缓存复用原 key，不作为新功能或测试证据。

新增 `paths` 在固定 base/head 检查以下路径：`apps/web/`、`crates/`、`src/`、Cargo/工具链、`.cargo/`、相关 workflow、`scripts/`、`python/`、`tests/`、vendor 依赖，以及其他 Rust/TOML 文件。命中则 `web` 必需，删除路径同样触发；未命中明确 N/A，路径任务失败不允许跳过。审批依据为 #371 集中决定。未知受影响代码新增路径时默认扩展覆盖并补测试，不在这里引入第二套变更规则。

`web` 使用 Ubuntu、Node22、Python3.12、Rust1.98.1、锁定 Playwright Chromium，正常进程/端口权限。复用 npm/Cargo cache，显式 fetch lockfile 依赖后保留 E2E offline 构建：`quant-research app`（bundled DuckDB）、现有 CLI/fixture。build/typecheck/discovery、`comparison_api::tests` app 读取侧、全套现有 production API → Rust 持久结果 → 浏览器 E2E 均须实际成功。各 fixture 的数量记录及准备子步骤计时留本次独享临时目录；准备阶段仍20分钟、job60分钟、单测试超时不改。不上传服务日志、结果身份、截图或 traces。

`lightweight` 保留现有 required 名称作为 `always()` 汇总：core/path 必须 success；web 命中必须 success，未命中才允许显式 skipped。缺失、cancelled、failure、未授权 skip 都失败；不修改 branch protection，也不移除 Full 原有覆盖。CI pending 可立即交接并启动独立 Review，工具不以 pending 冒充合并就绪。

## 回顾、测量与回退

历史基线、CI 逐步时间及后续 3–5 票比较方法见 [P1 Retro](workflow-retro-p1.md)。普通票不强制 retro；两轮实质返工、重复审批或环境阻塞触发不阻塞修复的专项回顾，阶段收口复用已有记录。每轮最多三个行动，可零建议；优先工具/测试，新增规则须有证据，不创建定时机器人/提醒。

回退可分别进行：停用 CLI 写入口，保留 gh 人工入口；撤销新增 web/paths 与汇总时将原 core 恢复名 `lightweight`，保留所有原必需步骤，不能留下失联 required 名称；未获实测收益的串并行拓扑保持原样。规模规则回退须在负责人裁决中明确适用票、已迁移授权的保留方式及生效基线，不静默恢复旧限制。
