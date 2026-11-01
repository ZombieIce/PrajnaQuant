# 本机 ETF 研究工作台：RC 用户指南

本指南对应 `v0.1.0-rc.1` 候选包。它是**研究用发布候选**，不是稳定版、投资建议或实盘系统；
独立新用户验收、最终审查和发布决定仍以候选交接为准。首批范围仅为 **macOS arm64、
单用户、127.0.0.1 本机访问**。不要将服务绑定到网络或把真实行情、报告、运行身份和日志发到公开位置。
下面的价格和证券只供完全合成演示，不是真实行情。

## 1. 前置与安装

准备 macOS arm64、现代浏览器、Python 3.12、Git、`ps` 和与包内 `release.json` 的
`rustc_version` **完全相同**的 Rust 工具链，并确保它们从终端 PATH 可执行。工作台启动器
使用 `python3`；研究任务在包内独立的干净 Git 源码快照中运行，以记录真实代码和编译器来源。
它不需要开发 checkout、Cargo、Node、npm 或开发 `target` 来**运行**发行包。
不要修改包内 `source`、二进制、前端或 `release.json`；不伪造 Git/rustc 身份。
系统动态库要求以包内二进制 `otool -L` 与本机验收为准，系统安全阻断应正常处理，不绕过限制。

在终端进入发行包和对应 `.sha256` 文件所在目录，校验后解压，例如：

```sh
shasum -a 256 -c prajna-quant-v0.1.0-rc.1-macos-arm64.tar.gz.sha256
tar -xzf prajna-quant-v0.1.0-rc.1-macos-arm64.tar.gz
cd prajna-quant-v0.1.0-rc.1-macos-arm64
python3 --version
git --version
rustc --version
```

校验失败时不要安装。`release.json` 记录候选源码提交、构建工具版本和运行文件摘要；
`release-files.json` 列出初始包内容及大小、摘要；压缩包摘要保存在包外。
源码快照是仅含研究程序相关源码的浅、稀疏 Git 仓库，不是完整源码构建树；
它没有配置远程地址。可将**整个解压目录**移动到其他位置，数据根应独立于该目录和 home 根。
手工删除或替换必要文件会使启动/CLI 显式失败。

## 2. 安装演示并启动

在包目录内执行。`DEMO` 是新的绝对路径；不能指向 home 本身、发行包或非空目录。
它只会装入合成数据，不要使用真实数据根。启动时数据根同时作为 Dataset 根与任务输出根：

```sh
DEMO="$HOME/PrajnaDemo"
./prajna demo --data-root "$DEMO"
./prajna demo-info --data-root "$DEMO"
./prajna start --data-root "$DEMO" --port 7878
./prajna status --data-root "$DEMO"
```

用浏览器打开 <http://127.0.0.1:7878/datasets>。若端口被占用，选 1024–65535 中未使用的端口，
并在 URL 中使用相同端口；不要终止其他程序。命令返回 ready 后仍应检查页面与任务状态。
日志位于数据根 `.prajna-rc/`，可能包含本机敏感信息。

使用完毕：

```sh
./prajna stop --data-root "$DEMO"
./prajna start --data-root "$DEMO" --port 7878
```

停止只管理本启动器拥有且可验证的进程；未验证的清理会报错，不会宣称成功。正常关闭后完成的报告、
任务与结果保留；服务非正常中断时，原有排队/运行中的任务按既有恢复规则标记中断，不自动重试。
同一数据根同一时刻只允许一个启动器服务。终端断开不会等同于停止服务，应使用上述 `stop`。
完整命令行帮助例如 `./prajna cli prajna-experiment --help`；`cli` 固定使用包内对应可执行文件，
不会以 PATH 中的其他 Prajna 可执行文件替代。

## 3. 一轮完全合成研究

以下步骤从新安装的演示开始，故池的初始版本为 1；再次使用已有数据根时版本号可能不同，
应选择实际刚发布的固定版本。页面中的长身份可在本机查看，不需要预先猜测。

### 3.1 Dataset、池与手动更新

先通过发行包中的正式 CLI 重新发布一份已备好的合成 ETF 输入；同一输入幂等，新的 receipt 不覆盖旧的：

```sh
./prajna cli prajna-publish-daily publish --root "$DEMO" \
  --selection "$DEMO/etf.json-selection.json" --receipt demo-etf-repeat.json \
  --diagnostics "$DEMO/demo-etf-diagnostics"
```

1. 在 `/datasets` 选择一条 Dataset，再选择证券；查看原始行情、覆盖和缺口。没有 Bar 不代表零价。
   也可切换固定复权版本，查看其来源、方法和实际覆盖。目录中的一证券两 Bar 示例是预览 fixture，
   不等于满足回测门槛。
2. 打开 `/pools`，选择「国债/政金债」池（`bonds`），在「草稿成员」填入以下两行，保存草稿，
   再「发布固定版本」：

   ```text
   159952.XSHE
   510300.XSHG
   ```

3. 选择新发布的版本 2、更新配置 `synthetic`，点「更新选定池」。等待队列结束，逐成员检查
   `collection/publication`、实际覆盖、复权输入和行动覆盖。演示配置使用明确的合成已有采集报告，
   不是新鲜网络采集。看到「价格研究输入：已就绪」后，点「使用此固定版本研究」。
4. 选择「买入持有」、预检共同窗口，确认「可运行」后提交固定池实验；从「查看固定池任务」
   打开结果。之后再修改草稿或运行更新，不应改变已经冻结的版本和任务。
   原始行情已发布、价格就绪和行动覆盖是不同状态，任务成功本身也不证明总回报完整。

演示可测试缺输入：先复制保存 `$DEMO/pool-updates/configs/synthetic.json`，在**副本对应的合成数据根**
中将一个成员的 `adjusted_file` 和 `actions_file` 设为 JSON `null`，发起新的更新。
它应显示价格待补充、不能选择该次作为研究输入；把配置恢复后对该次选择「补跑本计划」。
补跑只允许改变供应映射，不可改变已冻结的池、日期、行情和证据；旧结果不覆盖。
不要对真实已冻结的 Raw、manifest 或身份文件做此操作。

### 3.2 因子、标签和 IC

1. 运行上面的 `demo-info`，其中 `synthetic-momentum-request.json` 给出合成 130 Session Dataset、
   证券和固定 `adjusted` 版本。打开 `/research/momentum`，照此选择 Dataset、`510300.XSHG`
   和其复权版本，选择「动量」，窗口填 `20,60,120`，点「计算并保存研究」。
2. 等待任务成功，检查时序、缺失状态、分布、图表与来源；刷新并从最近报告重新打开。
   还可分别选择「波动率」和「均线趋势（严格 close > MA）」提交；等于均线不算高于。
3. 若要观察 IC，先在 `/pools` 的跨资产池发布由 `synthetic-ic-request.json` 的 `adjusted`
   三个键构成的固定版本。回到因子页选择该池版本、同一 DSV 和文件中的逐成员复权版本，
   窗口填 `1`、勾选「评价后续收益」，提交后查看截面样本数、Pearson/Rank IC 与缺失原因。
   截面不足或方差为零显示不可计算而不是 0；未来收益是评价标签，不参与因子评分。

### 3.3 单 ETF 择时、池内轮动与基准

1. 打开 `/backtests`，选「单 ETF 趋势择时」。在 Dataset 中选择包含 `510300.XSHG`、
   且该证券有 `synthetic-timing@1` 复权版本的 3 Session 合成 Dataset；
   选择该证券与版本。均线窗口填 `2`、调仓频率 `1`、本金 `10000` CNY、
   买卖滑点各 `10` bp、买入税率 `0.001`、卖出税率 `0.002`，提交。
   默认窗口对短样本不可运行时应先显示预检错误，不会伪造结果。
2. 查看已完成任务的现金、持仓、权益、成交、费用和期末 pending。此合成例最后收盘现金应为
   `976.991` CNY，持仓 `900` 份，权益 `10066.991` CNY；原始价格用于成交/估值，
   复权价格仅用于信号。刷新后从最近任务重开。
3. 池内轮动使用较长的合成 Dataset：在 `/pools` 的跨资产池发布 `510300.XSHG` 和
   `510880.XSHG` 两成员版本；选择 `synthetic-momentum-request.json` 中的 130 Session DSV，
   为每个成员选择该 Dataset 下的复权版本。选择「池内动量 Top-K」，动量窗口 `1`、Top-K `1`、
   调仓频率 `5`、本金 `10000`，先预检共同窗口再提交固定池实验并打开结果。
4. `/comparisons` 可对照上述单 ETF 择时与同证券买入持有，或选择「池内轮动 vs 同池期初等权持有」
   和「池内动量 Top-K」。分别选择相同固定 Dataset、池/证券、复权、公司行动事实版本及成本；
   演示的公司行动版本有原版和修订版，可选择较早的原版。提交并检查对照可比性、不可用原因、
   绩效及两侧实际持仓和执行明细，再刷新重开。若事实覆盖不足，结果明确标记不完整，
   不把空指标解释成零收益或声称完整总回报。

信号使用当日收盘可观测价格，下一可用 Session 开盘执行；预热消耗的 Session 不进入可运行评价窗口。
费用、税和滑点以冻结配置为准，滑点已经体现在成交价中，不应重复扣除。
现金与持仓、应收共同构成权益；应收计入权益但不是可花费现金。

## 4. 首次真实输入与更新

先准备**已获授权**的独立本机数据根、真实日期范围和池版本。发行包不附真实行情、历史池、
复权供应文件、开盘交易状态或公司行动公告，也不证明其来源完整。原始未复权行情由现有 Tencent
采集入口获取或使用显式已有采集报告；遵守来源许可、限速与停源/访问控制，不自动切换供应商。
日历必须由独立来源提供沪深两个 Venue 的完整覆盖和时刻，不能从价格或工作日猜测。
证券生命周期、交易单位及开盘状态必须有可核验的固定证据；Unknown 不可视为可交易。
供应商复权数据需明确方法、单位 `CNY_per_fund_unit`、来源、实际行覆盖、追溯修订与收盘可用假设；
公司行动需结构化范围、事件和可用时刻。`observed_at` 是本地观察时间，不是历史发布时间。
发行物中的证券证据 CLI 只选择和读取**已有**证据版本，不从公告自动生成证据库或开盘状态快照；
若没有预先准备好的兼容证据库和所需状态快照，真实执行输入仍受阻，不能用演示数据代替。

从包内终端可查询工具帮助，并以自己的合格本机文件操作：

```sh
./prajna cli prajna-session-calendar --help
./prajna cli prajna-collect-daily-raw --help
./prajna cli prajna-select-security-evidence --help
./prajna cli prajna-import-adjusted --help
```

按[日历契约](contracts/shanghai-shenzhen-session-calendar.md)导入并保存固定 receipt，
按[日频发布契约](contracts/shanghai-shenzhen-daily-normalizer.md)准备 Raw、证券和状态证据；
按[复权契约](contracts/adjusted-research-series.md)和
[公司行动契约](decisions/0032-cn-corporate-action-fact-versions.md)准备供应文件/固定版本。
在数据根的 `pool-updates/configs/` 新建与实际**已发布池版本、全部成员和日期一致**的配置；
字段和供应文件的明确要求见[手动更新契约](contracts/pool-updates.md)。`synthetic.json` 仅供演示，
契约中的 JSON 形状也不是合格的真实证据。选择配置更新后逐成员核对实际覆盖，缺输入则补充并
按原计划补跑；不要手改旧 DSV、复权对象、行动对象或实验定义。

固定池是事后 Static Universe，不保证历史成分 PIT；三池独立、不做跨池资金配置。
跨资产池可能跨 Venue，行情 Session 轴不一致会阻断；混合 Venue 行动总回报不支持。
红利池和债券池同样取决于各自实际覆盖、状态及行动证据，不因预置名单而自动可用。
真实执行状态、行动与总回报缺口须分别披露；无合格状态不能用合成 `TRADABLE` 补造。

## 5. 结果、备份、升级与故障

- 查看[共同窗口](contracts/evaluation-window.md)、[因子](contracts/momentum-research.md)、
  [研究池](contracts/research-pools.md)和[结果明细](contracts/experiment-result-read.md)的具体口径。
  来源、成本、评价窗口和 Run 固定输入均以 Rust 保存结果为准；未保存/损坏不等于零。
- 备份前先正常停止服务，复制**整个数据根**到新位置并单独保存发行压缩包及其校验和；
  校验备份可读后再进行升级。不要用新包覆盖旧包或覆盖旧 Raw、版本及结果。真实备份受本机权限保护。
- 并排解压新版本，记录所用包及运行依赖，先在数据副本验证再切换；回滚时停止新服务并使用保留的旧包。
  旧程序可能不识别 `@3` 等新产物，回滚不会删除新数据，也不保证旧程序能读取它们。
  若移动**数据根**，需检查并更新仅供未来尝试使用的可变配置中的绝对供应路径；
  不要重写已冻结计划或不可变身份。移动发行目录本身不要求更改数据根。
- 端口占用：换端口；数据根不存在/非空：选择独立路径，演示不会覆盖已有内容。
  `rustc` 不匹配、Git 缺失或源码脏：恢复完整原包及匹配工具链，不伪造身份。
  查看 `.prajna-rc/` 内对应日志，确认服务健康和所属进程；清理未验证或目录/权限错误应先解决原因。
- Dataset/结果损坏、来源缺失、预热不足或行动 Unknown：读取页面的具体错误和范围，恢复原始备份或补充
  有效输入并创建新的尝试；不要将损坏视为空结果。不要公开真实日志、原文、覆盖明细或研究身份。

从源码构建候选是维护者操作：在干净的候选提交、macOS arm64、已安装锁定 Cargo/npm 依赖、
Node 20+ 与足够磁盘（保留至少 10 GiB）的环境运行
`python3 scripts/rc_build.py --output /absolute/empty/local-output --version v0.1.0-rc.1`。
脚本执行 release 构建、合成导出与打包，不创建 tag、发布 Release 或修改数据根；
`build.log`、压缩包及摘要留在受控本机输出目录。它要求可离线构建，不能用发行包内稀疏快照代替完整 checkout。
