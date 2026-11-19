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
构建时的 macOS 版本取自 `sw_vers -productVersion`，工具的标准错误保留在构建日志中。
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
```

演示目录包含固定 D12 合成日线输入。使用发行包中的生产导入器离线发布一次；此输入不访问网络：

```sh
./prajna cli prajna-import-daily-price --root "$DEMO" \
  --input-root "$DEMO/synthetic-input/daily-price" \
  --config "$DEMO/synthetic-input/daily-price/config.json" \
  --replay replay --receipt demo-daily-price.json
```

导入完成后，receipt 位于 `$DEMO/synthetic-input/daily-price/demo-daily-price.json`。
服务启动后，在 `/datasets` 可查看 receipt 中的 Dataset Version；该短样本用于核对 D12 导入与覆盖，
更长窗口研究使用 `demo-info` 提供的合成 Dataset。

启动服务并确认状态：

```sh
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

本节的 `synthetic` 更新配置用于验证既有多 Venue 更新路径，不是 D12 真实输入模板；
D12 日线研究直接使用第 2/4 节导入 receipt 中的 Dataset 和逐成员复权版本。
日线池内 Top-K 和比较须使用同一 Venue 的固定池，不可沿用本节的跨 Venue 两成员示例。

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
   选择该证券与版本，将「执行模式」显式改为「日线价格模拟」；
   页面应标明 `daily_price_simulation_v1`，不要保留默认的既有执行模式。
   均线窗口填 `2`、调仓频率 `1`、本金 `10000` CNY、
   买卖滑点各 `10` bp、买入税率 `0.001`、卖出税率 `0.002`，提交。
   默认窗口对短样本不可运行时应先显示预检错误，不会伪造结果。
2. 查看已完成任务的现金、持仓、权益、成交、费用和期末 pending。此合成例最后收盘现金应为
   `976.991` CNY，持仓 `900` 份，权益 `10066.991` CNY；原始价格用于成交/估值，
   复权价格仅用于信号。刷新后从最近任务重开。
3. 池内轮动使用较长的合成 Dataset：在 `/pools` 的跨资产池发布 `510300.XSHG` 和
   `510880.XSHG` 两成员版本；选择 `synthetic-momentum-request.json` 中的 130 Session DSV，
   为每个成员选择该 Dataset 下的复权版本。选择「池内动量 Top-K」，动量窗口 `1`、Top-K `1`、
   调仓频率 `5`、本金 `10000`，将「执行模式」改为「日线价格模拟」，
   先预检共同窗口再提交固定池实验并打开结果。
4. `/comparisons` 可对照上述单 ETF 择时与同证券买入持有，或选择「池内轮动 vs 同池期初等权持有」
   和「池内动量 Top-K」。在比较页面也将「执行模式」改为「日线价格模拟」，选择相同固定
   Dataset、池/证券、逐成员复权版本、评价窗口及成本；无需选择公司行动版本或分红税配置。
   提交并检查两侧均为 `daily_price_simulation_v1`、扣费价格收益、同一评价窗口与成本，
   核对绩效及两侧实际持仓和执行明细，再刷新重开。缺失指标显示不可计算及原因，
   不把空指标解释成零收益。严格执行（总回报）是另一路径，仍按原契约选择状态及公司行动输入，
   不用它的前置条件阻断 D12，也不把日线结果宣称为总回报。

信号使用当日收盘可观测价格，下一可用 Session 开盘执行；预热消耗的 Session 不进入可运行评价窗口。
费用、税和滑点以冻结配置为准，滑点已经体现在成交价中，不应重复扣除。
现金与持仓、应收共同构成权益；应收计入权益但不是可花费现金。

## 4. 首次真实输入与更新

按 [MVP-7 D12 日线价格研究与统一验收](https://github.com/ZombieIce/PrajnaQuant/issues/286#d12-日线价格研究与统一验收)
和[ADR 0039](decisions/0039-etf-daily-price-research.md)执行首发日线流程。
先准备**已获授权**的独立本机输入目录、研究数据根、真实日期范围和池版本；两目录均须独立于发行包。
发行包不附真实行情、历史池或真实基础配置，也不证明其来源完整。D12 导入器取得未复权和供应商
前复权日线，或重放已授权的固定原始响应；遵守来源许可、限速与停源/访问控制，不自动切换供应商。
日历必须由独立来源提供沪深两个 Venue 的完整覆盖和时刻，不能从价格或工作日猜测。
证券身份、生命周期、交易单位、价格及数量增量须来自可发布的固定基础配置。
供应商复权数据需明确方法、单位 `CNY_per_fund_unit`、来源、实际行覆盖、追溯修订与收盘可用假设；
缺 `qfqday` 的分段保持复权缺口，不用未复权价填充。`observed_at` 是本地观察时间，不是历史发布时间。
D12 假设历史复权收盘在 T 日收盘后可用、下一 Session 有效未复权开盘价可用于模拟；
**公司行动文件、逐日开盘状态表、历史发布时间及逐条来源佐证不是该模式前置**。
这不证明真实开盘可交易或历史复权 PIT；停牌、缺价及除权跳变风险仍须报告。
发行物中的证券证据 CLI 只选择和读取**已有**证据版本，不从公告自动生成证据库或开盘状态快照；
没有可发布的证券基础配置或固定日历时导入仍受阻，不能用演示数据代替真实基础配置。

按[日历契约](contracts/shanghai-shenzhen-session-calendar.md)导入本机固定日历，使用已有、可发布的证券证据版本
选择固定快照；发行包不创建证券事实或证据版本。导入器配置应将日历、证券证据根及固定版本指向这些
本机输入，日期范围覆盖预热与后续成交/估值 Session，成员仅列目标 ETF。按以下配置形状新建
`$INPUT/config.json`，用已导入的真实版本替换占位符；它们不是可直接运行的身份：

```sh
INPUT="$HOME/PrajnaInput"
LAKE="$HOME/PrajnaData"
mkdir -p "$INPUT" "$LAKE"
```

对已准备并核验来源的日历执行导入；证券证据版本必须已在指定本机证据库中，快照选择只引用这些固定版本：

```sh
./prajna cli prajna-session-calendar import --root "$INPUT" \
  --input "$INPUT/calendar.json" --receipt calendar-receipt.json
./prajna cli prajna-select-security-evidence --root "$INPUT" \
  --select 510300.XSHG=sha256:<existing-evidence-version>
```

日历 receipt 提供 `calendar_version`；证券选择命令输出的 snapshot ID 填入下方 `members`，不要填单条 evidence ID。
这些命令不会为缺失的源事实造数据；没有可发布版本时应先补齐授权证据。

```json
{
  "calendar_root": "/absolute/local/path/input",
  "calendar_version": "sha256:<imported-calendar-version>",
  "evidence_root": "/absolute/local/path/input",
  "start": "2026-01-05",
  "end": "2026-01-30",
  "members": {
    "510300.XSHG": "sha256:<publishable-security-snapshot>"
  }
}
```

把实际版本 ID 填入新配置，不修改既有 Dataset、实验或运行身份。发行包中的日线导入器同时获取未复权
与供应商前复权序列，并经生产 Raw、Dataset 与复权版本路径保存 receipt：

```sh
./prajna cli prajna-import-daily-price --root "$LAKE" --input-root "$INPUT" \
  --config "$INPUT/config.json" --fetch --receipt receipt.json
```

### 4.1 固定输入重导入：不联网、不复制旧结果

保留首次导入的 `$INPUT/config.json`、其引用的固定日历/证券证据库、`$INPUT/receipt.json` 及
receipt 的 `replay_dir` 指向的全部响应文件。`--replay` 参数是**相对于 `$INPUT` 的目录**，
不是绝对路径；不能只保留 receipt 或只复制已发布 Dataset。若没有首次导入保存的响应，
不能将已有 Dataset/结果冒充已完成重新导入，也不要为了复跑擅自重新取数。

同一数据根重新导入可验证幂等，但不能排除读取已有发布缓存。要验证从固定输入重新发布，
选择独立于发行包、原 `$LAKE` 及 `$INPUT` 的**全新不存在**目录；保留原始输入根和配置不变：

```sh
REPLAY_DIR="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["replay_dir"])' "$INPUT/receipt.json")"
REIMPORT_LAKE="$HOME/PrajnaReimport-$(date +%Y%m%d-%H%M%S)"
test ! -e "$REIMPORT_LAKE" || exit 1
test ! -e "$INPUT/reimport-receipt.json" || exit 1
./prajna cli prajna-import-daily-price --root "$REIMPORT_LAKE" --input-root "$INPUT" \
  --config "$INPUT/config.json" --replay "$REPLAY_DIR" --receipt reimport-receipt.json
```

receipt 保存在输入根而非新数据根；再次验证时选新的 receipt 名称，不覆盖首次记录。
命令的真实 stdout/stderr 和详细比对只写受控本机日志，不发到聊天或 PR。
在本机对照两份 receipt 的 `dsv`、`adjustments`、`coverage`、固定日历和证券快照；
固定版本是否一致由实际校验决定，不能手改身份来制造一致。网络/重放批次来源记录可以不同。
向全新根发布时，新来源记录可能使 Dataset 和复权身份不同；这不自动说明价格不同，也不能直接
认定复算一致。应分别用各自 receipt 的身份读取并比较未复权/复权价格行、单位、日期轴及缺口，
再核对研究和执行输出；保留身份变化的来源解释。相同根的幂等复用与全新根重建分别记录。

### 4.2 日线研究、重读与全新复算

首次研究使用 `$LAKE`，重导入后的独立复算使用 `$REIMPORT_LAKE`，不要复制旧数据根里的任务、
实验、Execution 或结果。先停止原服务，再以新根启动同一个未改动的发行包：

```sh
./prajna stop --data-root "$LAKE"
./prajna start --data-root "$REIMPORT_LAKE" --port 7878
./prajna status --data-root "$REIMPORT_LAKE"
```

仅在原服务确由此启动器启动时运行 `stop`；若原本未运行则直接启动新根。
从 `/datasets` 选择新 receipt 中的 Dataset，并逐成员绑定该 receipt 的复权版本；
因子页使用相同窗口、缺失规则及标签配置，保存新报告。按第 3.3 节执行单 ETF 择时、
同 Venue 池内 Top-K 和基准比较；新根须重新发布相同成员的固定池，记录实际池版本，
不要凭旧池身份猜测新版本。「执行模式」在回测、池实验和比较页面分别显式选择「日线价格模拟」。
有限样本不满足多成员/预热门槛时如实记录受限，不添加未授权成员或缩短窗口迎合结果。

保存后在本机核对因子缺失与有效值、至少一笔实际模拟成交、费用/现金/持仓/pending、
同窗口同成本的价格收益基准和 Rust 持久化绩效；任务完成本身不算成交或数学核对通过。
刷新、关闭页面重开以及停止/重启后再次读取结果，这证明持久化读取；在新根实际提交并完成任务才是复算。
冻结相同参数、模式、输入、代码来源和运行环境，比较保存结果与实际覆盖，
保留不同任务/实验/Execution 身份及差异原因，不要求通过伪造身份使它们相同。
真实明细、报告及身份仅留本机。以下工具可查看固定日历、已有证券证据和旧版路径的具体参数：

```sh
./prajna cli prajna-session-calendar --help
./prajna cli prajna-select-security-evidence --help
./prajna cli prajna-collect-daily-raw --help
./prajna cli prajna-import-daily-price --help
./prajna cli prajna-import-adjusted --help
```

### 4.3 既有更新与严格执行路径的边界

以下是另行选择既有更新/严格执行时的契约，不是 D12 固定响应导入及日线模拟的额外前置。
按[日频发布契约](contracts/shanghai-shenzhen-daily-normalizer.md)准备 Raw、证券和相应模式的状态证据；
按[复权契约](contracts/adjusted-research-series.md)和
[公司行动契约](decisions/0032-cn-corporate-action-fact-versions.md)准备供应文件/固定版本。
在数据根的 `pool-updates/configs/` 新建与实际**已发布池版本、全部成员和日期一致**的配置；
字段和供应文件的明确要求见[手动更新契约](contracts/pool-updates.md)。`synthetic.json` 仅供演示，
契约中的 JSON 形状也不是合格的真实证据。选择配置更新后逐成员核对实际覆盖，缺输入则补充并
按原计划补跑；不要手改旧 DSV、复权对象、行动对象或实验定义。

固定池是事后 Static Universe，不保证历史成分 PIT；三池独立、不做跨池资金配置。
跨资产池可能跨 Venue，行情 Session 轴不一致会阻断；混合 Venue 行动总回报不支持。
红利池和债券池的 D12 可用范围取决于各自实际日线与基础配置，不因预置名单而自动可用；
严格执行/总回报另受状态及行动证据约束。真实执行状态、行动与总回报缺口分别披露，
无合格状态不能用合成 `TRADABLE` 补造，也不能把日线模型假设当作状态证据。

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
构建前的版本冲突检查需要访问 GitHub API，查询 `ZombieIce/PrajnaQuant` 的 tag 和 Release；
网络、权限或限流导致无法确认时会报错停止。该检查不预留版本，正式发布前仍须重新核对。
