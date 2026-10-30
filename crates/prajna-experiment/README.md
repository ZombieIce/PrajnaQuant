# prajna-experiment

Vector / Fast Event / 受限 Accurate Event Experiment CLI，复用 Rust 策略、指标与 ResultLevel 存储契约（ADR 0017/0018/0019/0020）。合成验证与固定名单真实 ETF 工程验收分开；当前最终范围和有保留签收见 [STATUS](../../docs/STATUS.md)，不建立完整历史 Universe 或无偏业绩结论。

```text
prajna-experiment run <experiment.json> --lake <dir> [--level summary|standard|full]
                      [--promote <run-id>...] [--threads N]
                      [--backend-python <python> --backend-worker <worker.py>]
prajna-experiment diff <exe-a> <exe-b> --lake <dir>
prajna-experiment result <exp> <exe> --lake <dir> [--run <run-id>]
prajna-experiment promote <experiment.json> --lake <dir> --level standard|full
                         [--threads N]
```

`result` 只读已保存的 Execution：校验实验/执行身份与 manifest 中每一张已保存表的 hash，输出 `schema_version: 1` 的 Summary/NAV envelope；缺表保持 `not_saved` / `not_produced_by_engine`，不执行、不提升、不重算。字段、错误映射和 HTTP 适配见 [结果读取契约](../../docs/contracts/experiment-result-read.md)。

`--level` 默认 `summary`；省略 `--threads` 时使用 Rayon 默认线程数（包括 `RAYON_NUM_THREADS`），显式值必须大于零。Accurate 中该数值表示 worker 进程池槽位；Backend 不在 Rayon 线程上执行。线程数、ResultLevel 和 worker 路径不进入身份。CLI 从当前 Git 工作树捕获代码来源；lake 和定义可以用任意绝对路径。脏工作树照常运行，`reproducible=false`。

不传 `--promote` 时，新建或重放当前 Execution。传入 `--promote` 时只提升已存在的当前 Execution，不先新建；须同时指定高于各选中 Run 现有等级的 `--level standard|full`。缺失 Execution、未知/重复 Run 或未提高等级均报错；未选中的 Run 不受影响。提升验证 Summary 和已有明细表，补写缺失表；`default_result_level` 保持创建级别。

`promote <experiment.json> --level standard|full` 是同一提升能力的便捷入口：按当前工作树捕获身份，读取已存在的当前 Execution，提升所有仍低于目标级别的 Completed Run；failed Run 保持原级别，不新建 Execution。没有当前 Execution（目录不存在）时 exit 6，`--level summary` 时 exit 2；目录存在但缺少 `execution.json` 视为损坏（exit 3），不回退。没有任何 Completed Run 低于目标级别时以 `action: replayed` 成功返回（failed Run 保持原级别，不阻塞；全 failed 也不写表），但返回前仍校验 Summary/已保存表，损坏则失败。若选中的 Completed Run 在提升重放中失败，整批中止且不发布 partial 表。便于服务端先 promote、再回退到 run。

## 最小示例

从仓库根目录执行，需 Rust、Git、Python 3。示例在临时本地 clone 中记录来源，输出和定义在 clone 外，避免入库文件使工作树变脏。`prepare_fixture` 发布已入库的 **64×252 v2 fixture**（normalizer `synthetic-etf-daily@3`），从 panel 的全部证券保存 Static Universe，再写两个 Run 的 `experiment.json`。

```sh
repo=$(pwd)
cargo build -p prajna-experiment --bin prajna-experiment --example prepare_fixture --locked --offline
# 使用自定义 CARGO_TARGET_DIR 时，把下面路径改为其 debug 目录。
bin="$repo/target/debug/prajna-experiment"
prepare="$repo/target/debug/examples/prepare_fixture"
tmp=$(mktemp -d)
git clone --quiet --local "$repo" "$tmp/code"
"$prepare" "$tmp/lake" "$tmp/experiment.json" > "$tmp/definition.json"
cd "$tmp/code"

"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" > "$tmp/first.json"
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" --threads 1 > "$tmp/replay.json"
# action 分别为 created / replayed，exp 和 exe 相同。

exe_a=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["exe"])' "$tmp/first.json")
run_id=$(python3 - "$tmp/first.json" "$tmp/lake" <<'PYCODE'
import json, pathlib, sys
result = json.load(open(sys.argv[1]))
manifest = pathlib.Path(sys.argv[2]) / "experiments" / result["exp"].split(":")[-1] / "executions" / result["exe"].split(":")[-1] / "execution.json"
print(json.load(open(manifest))["runs"][0]["run_id"])
PYCODE
)
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" --level full --promote "$run_id" > "$tmp/promote-a.json"

# 仅在临时 clone 创建空提交：源码不变，但 git revision 改变，产生第二个 Execution。
git -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit --quiet --allow-empty -m "Second fixture execution provenance"
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" > "$tmp/second.json"
exe_b=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["exe"])' "$tmp/second.json")
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" --level full --promote "$run_id" > "$tmp/promote-b.json"
"$bin" diff "$exe_a" "$exe_b" --lake "$tmp/lake" > "$tmp/diff.json"
# diff 退出 0：同一 exp，Run/指标/已保存明细一致。
cd "$repo"
```

新 Execution 保存于 `<lake>/experiments/<exp-hex>/executions/<exe-hex>/`。Summary 不创建 `runs/`。Vector Standard 添加 `sessions`、`executions`、`corporate_actions`；启用终止结算语义时另保存 `terminal_settlements`；Full 再添加权重、排名及 pending 表。没有公司行动的运行仍保存空的 `corporate_actions` 表。每 Run 的 manifest 区分未保存与 Engine 不产生的表；不把缺表解释为零。

## 多 Venue（沪深同池）Experiment（#347）

定义用旧 `venue`（单 Venue，身份不变）或显式 `venues`（非空、无重复、合法 `VenueId`）之一；单元素归一化为
`venue`，多 Venue 归一化为排序后的 `venues`。Run 只经 `load_panel_for_venues` 加载所选集合，要求每个 Venue
有 Instrument/Session 且共享 Session 轴一致；缺 Venue/Instrument、日历不一致、Universe 成员不在所选
Venue 或显式 lot 越界时整体失败，不静默选第一个 Venue。Run Spec 使用 `fast_event@13`（vector_parity）/
`@14`（lot）与 `vector@10`；单 Venue 版本、旧身份与已保存结果不变。Accurate、adapter、PIT 与 Python
策略仍限单 Venue（多 Venue 仅非 PIT、价格收益、`full_target`）。契约见 [ADR 0028](../../docs/decisions/0028-multi-venue-experiment-run.md)。

## 跨 Venue 池选择与基线提交（#295/T06c）

`prajna-freeze-selection` 现在接受同时含 `XSHG`/`XSHE` 成员的已发布池版本：按成员真实 Venue
集合用 `load_panel_for_venues` 加载一次共享 Session 轴并规划同一共同窗口；冻结定义写排序后的
`venues`（单 Venue 仍写旧 `venue` 字段、身份不变），多 Venue 买入持有使用 `full_target` +
`retry` 且省略 `entry_budget`（ADR 0027/0028）。预检 envelope 增加 `venues`，冻结 envelope
增加 `venues` 与 `engine:{kind,semantic_version}`；后者由 `Experiment::engine_identity` 复用
执行时的 Run Spec 规范化，只作展示/交接，不进入身份。共享轴缺日期或开收盘时刻不一致返回
`inconsistent_venue_calendar`，所选 Venue 无 Instrument/Session 返回 `invalid_venue_selection`
（HTTP 422），不补日、不压缩、不选第一个 Venue。多 Venue 基线仍是价格收益，不含公司行动或
PIT；生产数据集按单证券发布，把多个单证券 Dataset 装配成池输入仍不在本入口范围。

## 共同评价窗口与冻结复权研究输入（#297/T08a）

定义新增可选 `adjusted_versions`（逐成员 `adj:sha256`）与 `evaluation`
（`common-window@1`：`warmup_sessions`、`common_start`/`common_end`、
`start`/`end`）。两者进入 Experiment/Run 身份；`Experiment::parse` 在执行侧
重新读取每个版本并校验 raw DSV、证券、窗口覆盖与顺序，失败 fail closed。
`Experiment::plan(lake)` 从绑定复权序列的真实 Session 覆盖和每个参数组合的
声明预热计算 `WindowPlan`（共同窗口/最大预热/评价窗口/逐组合可运行/阻断原因）；
预热缺口只后移评价起点，不删除 Session、不填零、不以原始价替代研究信号。

带 `evaluation` 的 Fast Event/Vector Run 把账本 Panel 切片到评价窗口：预热
Session 不产生 Decision、Fill 或绩效，结果/Summary 从评价起点开始，窗口末
Decision 不成交；Buy & Hold 的建仓锚点移到窗口首 Session。成交/估值仍用原始
Bar，Fast/Vector 语义版本与无窗口旧定义的身份/结果不变。池的选择入口必须逐成员
绑定复权版本（包括 #295 的沪深同池版本）；T04b 单证券未绑定时保留旧路径。完整契约见
[共同评价窗口](../../docs/contracts/evaluation-window.md) 与
[ADR 0031](../../docs/decisions/0031-common-evaluation-window-and-adjusted-binding.md)；
预检/页面由 #356/T08b 承接。

## MVP-6 离线 PIT S2（#177）

新增、显式选择 `strategy: "s2_pit_rotation"`；旧 `s2_rotation`、Static Universe、
Factor 与旧 Run 身份不变。此路径只接受 SZSE、`availability_assumption: "none"`、
`unfilled_entry: "skip"` 和 `full_target`，支持 Vector 与 Fast Event，不接 Accurate；Python 入口见 `prajna-py`。
此节记录 #177 初始契约，DSV 从 `szse-etf-frozen@1`（6a）/`@2`（结构化公司行动表）起步；后续固定名单与含行动版本由现有 PIT 输入路径消费，最终真实工程范围见 [STATUS](../../docs/STATUS.md)。CLI 运行消费本机冻结输入，不在回测中补采数据。

定义新增 `pit` 对象（不得用已计算的候选集/任意评分代替原始证据）：

| 字段 | 契约 |
| --- | --- |
| `catalog` | #175 `CatalogInput`，其 content identity 填入顶层 `universe` |
| `market` | #176 `MarketEvidence`；从冻结 DSV Raw 重建并核对表 hash |
| `empty_actions` | `@1` 必需的 #176 `EmptyActionCoverage`；`@2` 有 `corporate_actions` 表时忽略 |
| `corporate_action_rules` | `@2` 必需；绑定 TRI 跳变阈值/语义版本，进入 PIT、Cache 与 Run 身份 |
| `dividend_tax_rate` | 可选 scale-18 十进制字符串（如 `"0.03"`），默认 `"0"`；`@2` 进入 Corporate Action schedule 与 Run 身份 |
| `terminal_settlement` | 可选；仅接受 `semantic_version: "mvp6-terminal-settlement@1"`。启用后，Catalog 终止时对仍持有标的按运行窗口内最后一个终止前 Bar 的原始 close 直接结转现金，不生成 Order/Fill 或结算费用；Run Spec 披露实际清算款/日期未使用，未启用时既有语义与身份不变 |
| `eligibility` | 可省略；默认上市 61 Session、金额窗口 20、日均 1 千万元；`momentum_sessions` 由每 Run 的 `lookback` 覆盖 |
| `risk_notices` | 可省略；`instrument_id` 为生命周期 ID，`kind` 与 `evidence` 引用冻结官方附件；Catalog 正式终止事实自动加入 |

参数组合为 `{"lookback":60,"top_k":3,"rebalance_every":5}`，支持 `list` 或
`grid`（例如 lookback 20/60 × top_k 1/3 × rebalance_every 5/20）。要求正整数，
不因 Catalog/Eligible 少于 Top-K 拒绝；实际合格标的间等权，空集合为现金。
常规决策从覆盖首 Session 开始每 N Session 在 close 产生，下一 open 执行；
窗口不足时显式产生现金决策，不预热成交。

主运行 Fast Event 配置为 `lot`、lot 100（Instrument 或显式 native-code lot 来源）、
初始资金及 VP 资本 1,000,000、佣金 0.0003、最低佣金 5、买卖滑点各 2bp、税 0。
既有 `costs`/`fast_event` 字段仍须显式填写；没有隐藏的成本默认值。
对拍运行改为 `vector_parity + full_target`，忽略最低佣金与 lot，Vector 沿用同样比例成本。
PIT Vector 必须显式提供 `"vector_pit":{"initial_capital":"1000000"}`，
与所对拍的 Fast VP capital 一致；资本影响金额与数量的 scale-18 舍入边界，
不能以归一化资本 1 替代。该值进入 Experiment 身份和 Run Spec 的
`engine.initial_capital`。旧 PIT Vector 定义须补此字段；旧 Static Universe Vector 不变。
Native DSV 价格按 Catalog 拆成生命周期面板，不把复用代码的另一生命周期价格携带进来。

常规方向门禁按费用/数量 sizing 后的实际交易侧判断，而非目标成员身份：
真正必需的受阻卖腿令整次目标延期；受阻买腿保留当前数量、不扣该腿费用，
重算其他腿并保留现金，不重归一目标。涨停持仓可减仓、跌停持仓可加仓。
PIT Vector/Fast 共用 F8 **金额与数量** sizing：按可投资权益算逐项费用，再按扣费后权益
及原始价格算目标数量，所有操作按 Domain scale-18/half-even 的相同次序进行，
不先对归一化费用或目标市值舍入。只有数量精确不变才回退到 fee-only 的 fee_side，
不设 no-trade epsilon。Vector 保留精确 sizing 输入状态，不提交订单或成交账本；含公司行动的 `@2` PIT
Run 还将投资持仓、可用现金、分红/清算应收分桶，并用连续份额口径与 Fast Event
逐日对拍。Fast Event 的定点现金、持仓、应收和绩效账本仍是权威；
`CorporateActionSchedule`、税率、Return Basis 和逐标的覆盖状态进入 Run Spec。
覆盖仅为 `observed_raw_set_only` 或 UNKNOWN 时，Summary 明确标记总回报不完整。
`pit-s2@4` 仍记录原 PIT sizing/capital 语义；公司行动账本使用独立
`corporate_action_ledger@1` 版本。自 #274 起正常 parity full-target 的目标权重、drift、候选费用基数与扣费后
目标数量按可投资权益（可用现金＋证券，排除分红/清算应收）计算，应收仍计入总 NAV。
`vector_parity + full_target` 在卖出结算后按实际现金（不含应收）预留未付费用并按
InstrumentId 只削减必要买腿，进入 `fast_event@7/@8/@9` 与 `vector@7/@8/@9`；
lot、独立 retry 与旧 Vector 仍为 `fast_event@1/@2/@3`、`vector@1`。
Vector `executions[].pit_sizing` 保存计划的总 NAV（`equity`）、可投资权益
（`investable_equity`）、费用、目标数量和必需方向；
延期计划不代表已成交。手算回归见 `prajna-research/tests/pit_sides.rs`。

Risk Notice 类型为 `holders_meeting`、`vote_suspension`、`termination_proposal`、`liquidation_notice`、
`official_termination`、`low_nav_warning`；最后一种只标注。Exact 发布时间沿用原时刻，
DateOnly(D) 沿用 #175 的 D 后下一 Session close。风险退出在信息已知后的下一
**严格晚于可用时刻、明确可卖** open 单独清掉风险持仓；HALTED/UNKNOWN/跌停持续等待，
普通 cadence、新信号或 Universe 移除都不能取消它。只卖风险腿，不调整其他持仓数量；
常规延期目标仍可由新 close 决策替换。风险收益暂存现金，只有实际执行常规目标时再分配。

#205 增加可选 `pit.cninfo_index`（#202 的 `IndexDataset`），与手填 `risk_notices`
共同构建 Rust RiskSignals。检查分类版本、窗口覆盖、unknown 和 Raw 引用；
公告毫秒时间保守取上海当地日期，再按 P6 推到下一 Session close，不以采集时间倒填。
大会/计票停牌/终止/清算分类触发风险提示，不据标题推断实际停牌时段。
`pit.market.semantic_version = "szse-derived-open@2"` 显式选择 TDX 主源状态，
只消费 v3/v4 DSV 的 TDX 昨收及带官方文号/类别的 PIT 限价规则；省略仍保留旧版。
完整字段、缺失处理及合成序列见 [派生输入契约](../../docs/derived-etf-input-contract.md)。

Run Spec 保存 Catalog/市场/TRI/规则/风险身份、逐 Session Universe/Eligible 与排除原因、
以及 HALTED/UNKNOWN/方向受限格子的 `execution_exclusions`。`execution.json` 的 PIT
`StoredRun.run_spec` 在 **Summary/Standard/Full** 均保留，读取核对 Run content hash。
Vector `executions` 与 Fast Full `orders` 附风险尝试及期末等待记录；Summary 的
`pending_at_end` 同时包含未退出风险腿。旧 Run 不新增此可选 manifest 字段。

3 ETF × 10 Session 的供应商格式合成 Raw、逐日手算现金/持仓/费用/开收盘 NAV 和
Vector/Fast 日对拍见 [`prajna-research/tests/pit_s2.rs`](../prajna-research/tests/pit_s2.rs)；
公司行动 golden 在 [`corporate_actions.rs`](../prajna-research/tests/corporate_actions.rs)
覆盖分红登记/除息/发放、应收不投资、
折算取整、终止清算和未知金额。Summary/Vector Result 声明 Return Basis、税率、
覆盖报告及不完整原因；Standard 结果另保存公司行动事件行。这里的结构化 Normalizer 不自行证明 cninfo
公告全集；真实固定名单验收已按后续裁决完成，不外推完整公告覆盖、历史
控制样本或广义 P0-2 无偏业绩。#178 Python PIT 视图复用同一 Rust 输入和执行层，
见 [`prajna-py`](../prajna-py/README.md)。

## stdout JSON 与退出码

`run` 成功只输出一个 JSON 对象：`exp`、`exe`、`reproducible`、`run_count`、`failed_count`、`action`（`created`/`replayed`/`promoted` 三选一）、本次线程数、`timings_ms`（定义校验/展开、因子阶段、Run 阶段、总计）、`cache.compute_count`/`hit_count`、`written_bytes`/`written_files`。

计数和因子/Run 耗时来自**本次调用**；重放不会复用原 manifest 的旧耗时。总耗时还含数据加载、来源捕获、存储/重放校验等工作。写入量只统计本次发布的 Experiment/Execution 定义、Summary 和明细表；替换的提升 manifest 算一次，排除 Factor Cache、锁文件和临时校验写入。普通重放的发布写入量为零。manifest 保留首次运行的线程数、起止时间和阶段耗时。

`diff` 输出 #102 的按 Run 对齐报告：只出现在一侧的 Run、status/pending 变化、各指标差值（右减左）、明细表 hash 差异及首个不同的 key/字段。额外的 `summary_changed` 比较两份 Summary logical hash，覆盖数值/null、null 原因和回撤峰谷等变化。只在一侧存在的明细表也算差异；比较同等级结果可避免单纯由提升造成的差异。跨 Experiment 拒绝，不输出成功报告。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功；个别 Run failed 仍完成 Execution，在报告中计数 |
| 1 | I/O、来源捕获、执行/存储错误、`run --promote` 的 Execution 不存在、`result` 读取或 hash 错误、`diff` 的非法身份 |
| 2 | CLI 参数、定义/展开错误、`result` 的非法身份格式或非法提升请求 |
| 3 | 重放不一致或已保存结果损坏 |
| 4 | 同 Experiment diff 有差异（stdout 保留差异 JSON） |
| 5 | 跨 Experiment diff |
| 6 | `promote` 没有当前 Execution（服务端据此回退到 `run`） |

运行 `--help`、`run --help`、`result --help`、`promote --help` 或 `diff --help` 也可查看退出码。错误诊断写 stderr；错误路径不输出成功 JSON。

## 验证

```sh
cargo fmt --all -- --check
cargo clippy -p prajna-experiment --all-targets --locked --offline -- -D warnings
cargo test -p prajna-experiment --locked --offline
```

轻量 CI 已运行该 crate 的 all-targets clippy 与完整测试，包含 CLI 集成测试。测试直接启动已编译二进制，用入库 fixture 和独立临时 lake/Git 仓库覆盖新建、重放、提升、diff、损坏结果及退出码；不依赖调用者工作目录或未入库的数据产物。

## Fast Event 与三种策略（MVP-3）

`engine` 接受 `vector` / `fast_event` / `accurate_event`（语义版本 `vector@1` / `fast_event@1` / `accurate_event@1`）。
`strategy` 接受 `s2_rotation`（TopKRank）、`buy_and_hold`、`ma_crossover`。
定义解析还接受 Python 策略引用字符串 `strategy_id@version`（两部分非空，
无空白或额外 `@`），参数为任意 JSON object，仍使用 Grid/List。
Rust `Experiment::run_definitions` 可在不执行策略时返回排序去重的 Run Spec/ID，
`canonical_json` 保留引用；CLI `run` 显式报错 `requires Python host`，退出 2，
不创建 Execution 或静默跳过。Python 的类型化定义与现有 Rust 策略执行入口见
[`prajna-py`](../prajna-py/README.md)；Python Decision 已经由本地包的两阶段执行接通，CLI 仍不直接运行 Python 引用。
S2 的九个参数及默认 Run Spec 字节保持不变；显式 `unfilled_entry: "skip"`
与缺省同一身份，`retry` 进入身份。BuyAndHold 固定 retry；MA 默认 skip，也可显式 retry。
运行前通过 `prajna-research::engine::Engine` 校验 Strategy × Engine × 模式：
Vector 继续要求 `Vectorizable`；Fast Event 的封闭 Decision 策略族接受
`Vectorizable` 或 `EventDriven`，声明 `OrderBookDriven`（即使同时声明前两者）
或未声明 Decision 能力时拒绝。原生 BuyAndHold / MA 为内建 Vectorizable Decision
策略。参数展开与每 Run 决策前均校验，不把不支持的组合送入成交/账本。

`Engine::AccurateEvent` 使用 MVP-4 路由规则：相同 Decision 策略族、仅 `lot`，
调仓为 `full_target` 或 `entry_only`；拒绝 `vector_parity` 和 `OrderBookDriven`。
Experiment 通过 Backend 中立的 Order / Fill / Reject 闭环接入；真实 Python worker
由 #139 提供，fake 契约/transport 测试不证明真实 Nautilus 或 #133 A15 验收完成。
Rust 调用者可用 `Engine::validate` / `decide` / `decide_panel` 在数据计算前校验；
底层 `run_fast_event` 只消费已生成的 Decision tape，不携带策略能力声明。

新策略同样支持 `parameter_space.list` 与 `grid`：

```json
{"strategy":"buy_and_hold","parameter_space":{"list":[{"params_version":1}]}}
```

```json
{"strategy":"ma_crossover","parameter_space":{"grid":{"params_version":[1],"short_window":[20],"long_window":[60]}}}
```

Fast Event 必须增加以下配置；金额/数量用十进制字符串，规范化为 Domain scale-18
数值的唯一字符串，不经过 f64。比例成本沿用顶层五项 `costs` 的 f64 输入及 bits 身份，
执行时将其最短十进制表示转为 Domain 定点值（bps 用定点除以 10000）。

```json
{
  "engine":"fast_event",
  "fast_event":{
    "currency":"CNY",
    "initial_cash":"100000",
    "sizing":"lot",
    "rebalance":"full_target",
    "minimum_commission":"100",
    "portfolios":[{"id":1,"capital":"80000"}],
    "lot_size_source":{"kind":"instrument"}
  }
}
```

每个 VP 使用该 Run 的同一策略/参数与声明 Universe；VP 各自生成独立订单/Fill，
资本只在初始分配，余额为 Unallocated Capital。此输入不提供混合策略 VP 定义。
`lot_size_source.kind=instrument` 从该 DSV 的 Instrument 表读取，缺 lot size 不默认补齐。
也可用 `{"kind":"explicit","values":{"A.SYNTH":"100","B.SYNTH":"100"}}`：
显式值须正数并属于数据集 Venue，lot 模式须覆盖整个 Venue panel（包括非 Universe 标的）。
来源种类与已解析 lot sizes 都进入身份；账户币种须匹配 Universe Instrument quote_currency。
`vector_parity` 下 lot size 和最低佣金不参与成交，但显式配置仍进入 Run Spec。

`rebalance=entry_only` 为 POC 专用，必须 `sizing=lot`、`unfilled_entry=retry`，并显式
增加 `entry_budget=fixed_initial|remaining_cash`；不得暗中特化策略或修改信号时间。
S1 仍首 Session close 决策、下一 open 成交；这里不提供 POC S1 的延迟首次决策变体。
策略版本、sizing、rebalance、entry budget、retry、VP ID/资本、初始资本、币种、
lot 来源/解析值和最低佣金均进入 Run Spec；线程数及 ResultLevel 均不进入身份。

| ResultLevel | Fast Event 保存内容 |
| --- | --- |
| Summary | E11 指标；每 Session open 后和 close 的账户/VP 恒等式仍在 Run 内精确验证；不创建 `runs/` |
| Standard | Summary + `account_valuations`、`portfolio_valuations`：两点现金/持仓/权益、估值价、来源 Session/字段与账户未分配资本；启用终止结算时另有 `terminal_settlements`（Session、生命周期标的、数量、价格、结算金额） |
| Full | Standard + `orders`（提交订单及 execution gate 阻塞/跳过/retry/pending 尝试）、`fills`（raw/实际成交额、比例佣金、最低佣金补足、税、滑点、现金变动及 F8 费用基数/方向）、`fee_settlements`（费用非零但数量不变的 F8 腿） |

Fast Event 表用稳定 `row_id` + JSON `payload` 的 Parquet schema：所有账本数字为精确
scale-18 字符串；空表仍保存并登记 logical hash。Orders 中 `record_type=attempt`
是执行诊断，不冒充正数量 Domain Order；`record_type=sizing_adjustment` 保存 lot_rounding / insufficient_cash 的请求及实际数量（可为零）；不可交易的持仓/买腿明确标出原因，末日 pending
保留原目标。成交前现金削减/整手裁减另存原因和前后数量；attempt 的 applied 是执行决策目标，
并非承诺的成交数量。完整现金重放需同时计入 Fill 与 fee-only settlement。

Run manifest 的 `accounting_verified=true` 只表示成功 Run 已通过运行内两层、两点的
精确恒等式；失败 Run 保存 `status=failed`、错误和 `accounting_verified=false`，无指标，
其余 Run 继续。`not_saved` 区分 ResultLevel 省略的表，`not_produced_by_engine` 则是
Engine 不产生的表；Vector 保持原有 orders/fills/cash_ledger 不产生声明。
Fast Event Summary 在摘要完成后释放明细，不跨整个 Rayon 扫描保留所有 Run 账本。
提升、重放和 diff 复用 MVP-2 存储规则，已有表与 Summary 必须 logical hash 一致。
重放 Summary 时若已有提升明细，会重新生成所需明细并校验，不能跳过验证。

Fast Event E11 以**账户每日 close 权益 / 初始账户资本**为 NAV，收益从第二 Session
开始，年化按 `sessions_per_year`；成交次数是 VP 执行尝试次数，不是 Fill 笔数。
`total_turnover` 为各 Session 换手分子 / open 前总 NAV 之和：正常非 retry parity
full-target 使用已执行计划的候选费用基数（含 fee-only，不重复计数），lot、retry 与
Accurate 路径保持实际 raw Fill 成交额；`total_cost` 为实际费用 / 同一 open 前总 NAV 之和，
均为无量纲值。Open 前总 NAV = open 后权益 + 当次全部费用，包含应收。精确账本与费用字符串不被指标 f64 替代。Vector 继续使用 ADR 0017 的
open NAV；两者估值点不同，不能直接用 Summary Sharpe 当作引擎对拍。
仅验证合成数据，不建立真实市场、PIT、分红总回报、T+1/涨跌停或实盘结论。
吞吐/RSS 原始证据和复跑命令见 [`poc/mvp3-benchmark`](../../poc/mvp3-benchmark/README.md)。

## Accurate Event 与 Backend 身份（MVP-4）

Accurate 复用上面的 `fast_event` 配置块，含 lot、成本、VP 资本与调仓语义；这是共用
执行配置，不表示调用 Fast Event 模拟成交。另外必须写明 Backend 身份：

```json
{
  "engine": "accurate_event",
  "backend": {"name": "nautilus_trader", "version": "2.0.0rc6"}
}
```

名称/版本为非空、无首尾空白的字符串，整个对象同时进入 Experiment 与 Run Spec；
任何名称或版本变化都改变 Run 身份。旧 Vector / Fast Event 不新增 `backend` 字段，
拒绝提供 Backend 配置，规范化 Run Spec 保持原字节。seed 仍必须缺省或 null。
Python transport 只支持 `nautilus_trader`，运行前通过指定 Python 的包 metadata
核对版本，不能用另一版本生成成功结果。路径必须显式给出且成对出现，例如：

```sh
prajna-experiment run experiment.json --lake /absolute/lake --level full --threads 2 \
  --backend-python /absolute/rc6-venv/bin/python \
  --backend-worker /absolute/checkout/backends/nautilus/worker.py
```

调用者须使用记录代码来源的 checkout 中的 worker；外部脚本路径不是代码来源证明。
API 调用者可用 `with_backend_factory(Arc<dyn BackendFactory>)` 注入系统边界：
每个 Run 创建一个 `ExperimentBackend`，结束时收集 `BackendDiagnostics`。
`BackendDiagnostics::fake()` 明确标记 fake，CLI 没有 fake 开关，不会静默回退。
Summary assumptions 保留证据类型和未分类的账户差异；fake 永远不构成 Nautilus 验收。

控制器为每个 Run 写一次不可覆盖的中立 Parquet 包，lot 字段使用已进入 Run Spec
的有效解析值（可显式覆盖 DSV 的 lot 字段，不改写 DSV）。旁边的 `worker.json` 含
账户初始现金和与平台相同的六项十进制成本。随后一个新 worker 进程处理整个 Run，
Rust 从实际 Backend Fill 入账后再计算后续订单，VP 订单不跨 VP 净额化。
每次写入、响应和正常退出的 transport deadline 为 60 秒；API 可调整
`WorkerCommand.timeout`，不进入身份。EOF、终止 error、非零退出、超时、缺少或
无法映射的 diagnostics 都使 Run failed；每个 worker 置于独立 Unix 进程组，
关闭/回收该 worker 与继承进程组的后代，管道线程回收同样有 deadline，并释放临时
数据包。仅支持 macOS/Linux；不支持的平台显式报错，不退回无隔离进程。
退出观察使用 non-reaping wait；先清理所属进程组再 reap leader，避免组 ID 被复用后
误伤其他进程。清理失败或超出 deadline 会明确写 stderr，不无限等待管道线程。
预检/初始化配置错误仍按 CLI 既有错误码报告；单 Run failed 保持既有 Execution
报告/退出码契约，须查看 `failed_count`，不能把退出 0 当全部 Run 正确。

| ResultLevel | Accurate Event 保存内容 |
| --- | --- |
| Summary | 同 Fast Event E11 与运行内账户/VP 两点恒等式；无明细表 |
| Standard | 加逐 Session `account_valuations` / `portfolio_valuations`，含估值来源 |
| Full | 加 orders / fills / fee_settlements，以及下列三个分列诊断表 |

`adapter_events` 分开保存平台合成 open/门禁与 worker `adapter_construction`；
`backend_events` 分开保存 mapped Fill/Reject 响应与 `backend_native` 原生事件，
映射后的 Session-open 时间不冒充原生事件时间。
`backend_comparisons` 保存 evidence、费用对照、每 Session 原生 CASH/持仓与平台
close 现金/持仓/权益（权威仍为 `prajna-account`）。现金差异用返回 Fill 的原生费用
与平台佣金/税之差逐笔核对；持仓差异或无法由费用解释的现金差异明确为
`unclassified`，不是自动认定为 Backend 原生差异，也不宣称 #133 A15 通过。
真实 transport 必须返回每 Session 的唯一账户快照；缺失即失败。
fake 的原生账户显式为 unavailable，不伪造原生快照。

三张诊断表沿用精确字符串 JSON payload 的 Parquet logical hash；Full 的空表仍保存，
较低等级只在 `not_saved` 中声明省略，不能解释为零事件。重放/提升会校验 Summary
和全部已存明细，包括诊断；证据变化或诊断不一致不能覆盖原 Execution。
上述 transport 契约与 fake 测试本身不等于真实 Backend 验收。真实 worker 的独立合成
对拍及差异分类见 [A15](../../poc/mvp4-correctness/README.md)，转换成本与未测规模
见 [A16](../../poc/mvp4-cost/adr0013/README.md)；不设扫描吞吐门槛，不外推 PIT ETF 支持。


正常 Fast Event `lot + full_target` 自 #275 使用 `fast_event@10`（行动 `@11`，terminal `@12`），目标基数为 pre-trade 现金＋证券估值，排除未支付应收，目标总和不足 1 不重新归一化。仍按整手、现金限量、最低佣金及实际成交费用执行；实际权重/成本率/换手率分母为总 NAV，lot 换手分子为实际成交本金。发放只转桶，后续正常调仓再投资。独立 retry、entry_only 与 Accurate 契约不扩展；历史结果不覆盖，旧 revision 重现旧行为。

## 单证券冻结选择

`prajna-freeze-selection --lake <LOCAL>` 从 stdin 读取固定 DSV/完整证券身份和
`buy_and_hold` 选择，生成/复用单成员 StaticUniverse、显式 D7 成本与 Fast Event
定义，并在返回前用 `Experiment::parse` 校验。默认配置 `mvp7-d7-simulation@1`；
不是完整分红总回报或历史 PIT 声明。生产 HTTP 经此 CLI 提交既有 T01 任务，
配置、响应、合成验证与限制见[冻结选择契约](../../docs/contracts/single-security-selection.md)。

研究池：`prajna-pools --lake <LOCAL>` 通过 JSON 命令管理三池草稿/不可变版本；`prajna-freeze-selection` 可引用显式池版本，见[契约](../../docs/contracts/research-pools.md)。旧单证券选择保持兼容；跨 Venue entry-only 提交仍拒绝。

共同窗口预检：`prajna-freeze-selection --lake <LOCAL> --plan` 只读返回逐成员
覆盖、参数组合最大预热、共同评价窗口及阻断原因；HTTP
`POST /api/v1/tasks/selections/preflight` 与 `/pools` 使用同一 Rust 结果。
显式 `parameter_space` 可沿用 native list/grid，提交冻结相同绑定和 evaluation；
协议、支持范围及合成验证见[共同评价窗口契约](../../docs/contracts/evaluation-window.md)。

## 价格因子研究（动量 / 波动率 / 严格均线趋势）

`prajna-momentum --lake <root>` 提供显式版本的价格因子研究 JSON 命令，复用新平台 Factor/Cache：省略 `factor` 为 Momentum v1；`"factor":"volatility"` 复用 Volatility v1；`"factor":"trend_filter_strict"` 使用 #299 新增的严格 `close > MA` 定义（均线相等/`1e-12` 相对容差内为 0，不改 TrendFilter v1 的 `>=`）。生产页面 `/research/momentum` 可选择因子与窗口。输入、持久化与验证范围见[价格因子研究契约](../../docs/contracts/momentum-research.md)。

单 ETF 趋势择时：`single_etf_timing` 的 version 1 参数为 `params_version`、
`ma_window`、`rebalance_every`；显式绑定复权版本并冻结评价窗口，页面可配置成本。
信号复用严格趋势因子，原始成交账本不变；见[模板契约](../../docs/single-etf-timing.md)。

### MVP-7 固定事实总回报账本

冻结选择与原生 Experiment 定义可带 `corporate_actions: {"version":"caf:sha256:HEX",
"dividend_tax_rate":"0"}`。将事实导入同一配置 lake，固定版本读取且无隐式 latest；
支持单市场 Static Universe Fast Event lot/full_target，保存 Standard 后可通过 result
与 read_run_table 读取 Summary/账户估值/行动表。发放只转现金，下一正常调仓再投资；
原始价成交与估值，adjusted 研究不会再次给账户加分红。缺事实不伪装完整，
省略绑定和 MVP-6 继续既有路径。详见 [ADR 0033](../../docs/decisions/0033-static-corporate-action-ledger.md)。


价格因子研究可增加 `"evaluation":"next_open_to_forward_open_v1"`，生成 Rust
1/5/20 Session 固定开盘端点评价标签及持久因子收益散点样本；生产页面可选开启。
省略保持旧报告语义，标签不能输入因子或策略，重叠标签不复利为 NAV。
见[评价 v1 契约](../../docs/contracts/momentum-research.md#未来收益评价-v1)。
