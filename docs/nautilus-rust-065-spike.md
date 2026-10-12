# 纯 Rust Nautilus 0.65.0 能力面与构建资源 Spike

- 票据：[#135](https://github.com/ZombieIce/PrajnaQuant/issues/135)，父 spec [#133](https://github.com/ZombieIce/PrajnaQuant/issues/133)。
- 日期：2026-10-12；范围：crates.io **已发布的 `0.65.0`**，不是当前 `develop`。
- 性质：限时调研与独立依赖构建探针，**不是 Accurate Event 实现或验收**。
- 决策边界：遵守 [ADR 0020 A5](decisions/0020-accurate-event-adapter-and-ledger-authority.md#worker-与锁步失败策略a5a6)。MVP-4 仍选择独立 Python worker；本记录不授权切换纯 Rust，不改平台账本、成本或执行权威。

## 证据口径与版本身份

能力标记：

- **已证实**：下文限定的源码/API/清单事实有直接证据；不自动表示本地运行通过。
- **不支持**：限定模式有明确源码限制或拒绝路径；不由“没找到文档”推导。
- **Unknown**：本轮没有足够证据，尤其是本项目端到端适配、数值往返与真实回调运行。

分别记录三类证据：**S**＝阅读发布源码/manifest；**T**＝阅读上游测试中的断言，未执行；**R**＝本机实际执行。全部能力调查为 S，部分附 T；R 仅有第六节的依赖构建及打印探针，**没有运行 BacktestEngine、任何订单、HALT、费用或锁步测试**。

公开原文只下载到 Copilot session 的 `files/nautilus-source-research/`，未将仓库代码发送到外部。通过 [crates.io 发布 tarball](https://static.crates.io/crates/nautilus-backtest/nautilus-backtest-0.65.0.crate) 阅读真实包内容。`backtest`、`model`、`execution`、`trader` 的 `.cargo_vcs_info.json` 均指向：

```text
7b766f8825b2539c5b2ac1375e9d97b41c509edb
```

下文 GitHub 源码链接固定这个提交及行号，而不是沿用 README 中的移动 `develop` 示例链接。发布包入口：[backtest][P1]、[model][P2]、[execution][P3]、[trader][P4]；辅助 Rust 策略接口来自同版本 [trading][P5]、[common][P6]。所有链接均为第一方源码或发布包，不采用二手文章。

## 一、能力矩阵

| 能力 / 精确判定范围 | 结论 | 证据及边界 |
| --- | --- | --- |
| 不启用 `python` 的四个精确版本依赖可解析、编译 | **已证实** | S：四个包 `default=[]`；R：本机独立 dev 探针构建成功，解析图无 PyO3，见第五、六节。不是回测执行验证。[P1]–[P4] |
| Rust 直接构造/驱动 `BacktestEngine` | **已证实** | S：公开 `new`、`add_venue`、`add_instrument`、`add_strategy`、`run`、`end`；策略需满足 native trait bounds，不是只实现一个回调即可。[S1] |
| `QuoteTick` / `Bar` 注入并到达撮合 | **已证实** | S：`add_data(Vec<Data>, …)` 与路由 `DataRef::Quote` / `Bar` 分别调用 exchange 的报价/bar 方法；存在公开构造器。[S2]。实际数据集与合成 open bid/ask 未运行。 |
| Rust 市价单提交 | **已证实** | S：`OrderFactory::market` 返回 `OrderAny`；`Strategy::submit_order` 经 binding 发送；matching engine 分派 `OrderType::Market`。[S3] |
| 原生 fill/reject 回调 | **已证实** | S：策略事件分派调用 `on_order_filled` / `on_order_rejected`；公开 Rust behavioral callbacks，不需 Python。[S4]。平台同步收集、重入和实际反馈链仍 Unknown。 |
| 自定义费用：比例佣金＋最低佣金＋卖方税 | **已证实** | S：公开 `FeeModel` trait、`FeeModelHandle` 与 venue 注入路径，可自实现上述函数；不是声称现成内置中国市场费表。T：custom handle 测试断言调用次数及 fill commission。[S5]。项目公式与 rounding 未运行。 |
| `InstrumentStatus(HALT)` 原生拒绝新单 | **已证实** | S：数据→exchange→`process_status(Halt)`→Closed→`process_order` 的非 Open 检查→`generate_order_rejected`，不是仅发现枚举。[S6]。同一时刻事件顺序、恢复、已有 pending 单仍需项目运行验证。 |
| CASH 账户 | **已证实** | S：venue 可配置 Cash，AccountAny 将状态构造为 CashAccount，exchange 明确限制 Cash 不交易期货/永续。[S7]。本项目 CNY 股票/ETF、余额和费用对照未验证。 |
| 普通模式 Price/Quantity 最大小数位 9 | **已证实** | S：`FIXED_PRECISION_STANDARD=9`，raw 分别 i64/u64，构造检查。[S8] |
| `high-precision`（不含 `defi`）最大小数位 16 | **已证实** | S：`FIXED_PRECISION=16`，raw 分别 i128/u128；128 位不意味着任意 scale。[S8] |
| 普通 / 仅 `high-precision` 的任意 scale 18 原生表示 | **不支持** | S：`check_fixed_precision` 对 18 明确报错，不能无损接纳有第 17/18 位有效数字的任意值。[S8]。尾部为零、可降到允许精度的值不能据此判为不可表示。 |
| `defi` 模式存在 precision 18 Price/Quantity | **已证实** | S：`defi` 启用 high-precision，检查上限改为 WEI_PRECISION 18，并有 `from_wei`。这是真实例外，不可把上一行外推到所有 feature。[S9] |
| `defi` precision 18 满足普通股票/ETF 撮合＋scale 18 账本往返 | **Unknown** | 只证明类型/构造支持，不证明全部数值路径、混合 raw scale、费用/账户或本项目适配可用；无高精度/DeFi 构建运行。[S9] |
| Session 锁步：多次 `run(end=…, streaming=true)` / 批次续跑 | **已证实** | S：streaming 不自动 end，end 为含端点，保留未消费数据；T：相邻 end windows 回放首个越界项、批次累计迭代数的断言。[S10]。Session 不是内置交易所日历概念。 |
| 数据 / fill 回调控制反转接入 Rust 决策 | **已证实** | S：`DataActor::on_quote/on_bar`、策略 fill callback 及提交接口提供 hook。[S3][S4][S11]。本项目 close Decision→下一 Session open→Fill→账本→下单的运行证明仍 Unknown。 |
| 严格本项目 Session 锁步闭环、人工金标准通过 | **Unknown** | 现有 S/T 不等于项目集成；未测试多标的同时间戳、回调同步、pending、成本和反馈影响后续订单。 |
| 无 breaking changes 的完整 Rust backtest API 稳定保证 | **不支持** | 固定提交官方 README 明说仍在开发，release 间可 breaking；container 的“小型 stable entry point”仅 re-export core/model/common，不能扩大为 BacktestEngine 保证。[S12] |

## 二、驱动、数据、订单与 native 回调

`BacktestEngine::new(config)` 是公开 Rust 构造器（`engine.rs:145`）；加入 venue（280）、instrument（355）、strategy（608），装入数据并调用 `run(start, end, run_config_id, streaming)`（701）。这些 API 不在 `#[cfg(feature="python")]` 下。[S1]

但 `add_strategy<T>` 明确要求：

```text
Strategy + StrategyNative + DataActorNative + Component + Debug + 'static
```

因此，“存在 `Strategy` trait”不等于“任意简单闭包可直接注册”。原生策略需要拥有/接通 runtime state 与 binding。`Strategy::submit_order` 的默认实现还要求 `StrategyBinding`。这一接口工作量未在本轮用最小策略证明。[S1][S3]

`add_data`（`engine.rs:420–439`）接收 `Vec<Data>`、`client_id`、`validate` 和 `sort`；sort 按 `ts_init`，run 要求数据已排序。`route_data_to_exchange`（1580–1631）先处理 venue，然后分别调用 `process_quote_tick`、`process_bar`、`process_instrument_status`。[S2] Price、quantity、bid/ask size、bar OHLCV 和时间需经数据构造器校验，不能把不合法 instrument precision 当成可用输入。L1 模式可接受 quotes/bars；depth book 模式缺 book 数据会在 `run_impl` 拒绝（`engine.rs:768–789`）。[S1]

市价单链条为 `OrderFactory::market`（`common/factories/order.rs:144`）→`Strategy::submit_order`（175）→native binding/命令队列→matching engine 的 Market 分支（3190）→`process_market_order`（3743）。**该函数明确拒绝 `TimeInForce::AtTheOpen` 和 `AtTheClose`，无 bid/ask 也拒单**；项目的“下一 Session open 下单”不能偷换成原生 MOO/MOC 支持，需要在正确事件时点发送通常市价单。[S3]

`strategy/mod.rs:1443` 将 Filled 分派到 `on_order_filled(&OrderFilled)`（1666）；Rejected 对应 `on_order_rejected`（1606）。`OrderMatchingEngine::set_event_handler`（265）也是公开 native hook，但 `SimulatedExchange::set_event_handler`（526）是 `pub(crate)`，**不可声称外部调用者可以直接替换 BacktestEngine 内部 exchange handler**。可用公开策略回调作为适配 seam。[S4][S6]

## 三、费用、HALT 与 CASH 的实现证据

### 自定义费用不受 `FeeModelAny` 闭集阻断

发布版 `execution/models/fee.rs:124–133` 的 `FeeModelAny` 确实是封闭枚举，Python 变体受 `python` feature gate。只读到这一段会错误地得出 Rust 无法扩展费用模型：

- 同文件 32–61 是公开 **`FeeModel` trait**，`get_commission` 输入包含 `&OrderAny`、实际 fill quantity/price、instrument，返回 `Result<Money>`。
- 65–118 是 `FeeModelHandle(Rc<dyn FeeModel>)`；`new<T: FeeModel + 'static>` 和 `from_rc` 接受外部实现。
- `SimulatedVenueConfig::fee_model`（`backtest/config.rs:299`）直接接收 **handle**；不是强制 FeeModelAny。相反，同文件更高层的序列化 venue config 字段（536）是 `Option<FeeModelAny>`，两条配置入口不应混为一谈。
- matching 的实际 fill 路径（`matching_engine/mod.rs:5651–5686`）调用 handle 的 `get_commission_with_context`，默认方法委托到 `get_commission`，然后生成带 commission 的 fill。[S5]

因此源码证明可以按实际成交额计算 `max(rate × notional, minimum)`，并按 order side 给 Sell 增加税，选用 instrument 对应 Money currency。**这是可实现性，不是已有费表或正确性验证**：最低佣金按 fill 还是按 order/lot、部分成交累计、Decimal/Money rounding、税的分列必须服从 [ADR 0020 A8](decisions/0020-accurate-event-adapter-and-ledger-authority.md#成本与合成滑点报价a8)；平台账本费用仍为权威。返回单个 `Money` 的对照模型不自动保存佣金/税拆分。

上游 `test_custom_fee_model_handle_is_called_by_fill_order`（10279–10339）构造自定义 RecordingFeeModel，断言调用一次、Filled commission 为预期 1.23 USDT；它直接调用 matching `fill_order`，**不是整引擎的中国市场费用测试，也没有被本轮执行**。[S5]

### HALT 有真实匹配检查，但必须遵守事件顺序

`exchange.rs:1082–1106` 将 `InstrumentStatus.action` 传入 matching engine；`process_status`（2521–2547）在 Open 状态接受 Halt/Close，转 Closed；`process_order`（2989–3000）在非 Open 时写入“cannot accept order”原因，3153–3155 调用 `generate_order_rejected`，且发生在 Market 分派之前。[S6]

所以这是 native 拒单，不是 Adapter 合成 Reject；**判定限于状态事件先被处理后提交的新单**。Halt 分支的 guard 是当前 Open，不能外推成所有原状态都转 Closed；pending 单取消/冻结、回到 Trading/PreOpen、与 quote/open order 同 `ts_init` 排序均待集成验证。平台仍按 ADR 共用执行层先拦不可交易标的；专门交叉校验时才可有意识地向 HALT backend 提交测试单，不能冒充真实项目原生事件计数。

### CASH 不是仅枚举

`SimulatedVenueConfig.account_type` 可选 Cash；模型 `AccountAny::try_from_state` 经 `from_state_checked` 的 Cash 分支实际创建 `CashAccount`（`accounts/any.rs:189`），exchange 初始化账户状态，且添加 instrument 时拒绝 Cash + futures/perpetual（`exchange.rs:437–466`）。[S7] `run` 文档及 balance error 检查明确说明账户拒绝负现金更新可以使整个 run abort（`engine.rs:696–717`）。[S10]

支持 CASH 不证明它与平台 Trading Account / VP scale 18 账本完全一致，也不证明 CASH 下所有 instrument 都可用。费用/最低佣金/余额造成的 backend 拒单应按 ADR 分类，不能用 Nautilus 余额覆盖平台权威。

## 四、精度与锁步：已知限制和未决验证

### 精度不能由 raw 位宽推断

`fixed.rs:85–97` 的普通模式 9 位、高精度 16 位适用于本轮构建的非 DeFi 配置。Price raw 为 i64/i128，Quantity 为 u64/u128；声明的数值范围分别为：

| 模式 | Price 最大值 | Quantity 最大值 | 最大小数位 |
| --- | ---: | ---: | ---: |
| normal | 9,223,372,036 | 18,446,744,073 | 9 |
| high-precision，非 DeFi | 17,014,118,346,046 | 34,028,236,692,093 | 16 |

依据是 `PRICE_MAX`（`price.rs:130–134`）、`QUANTITY_MAX`（`quantity.rs:117–121`）以及实际 precision 检查；不是声称每个 constructor 都无误差或每个范围边缘都已经测试。[S8] `new_checked` 使用 f64，不适合作为项目十进制字符串协议的无损转换证明。`from_decimal_dp` 存在纯整数 conversion / half-even 路径（`price.rs:506–566`），但同样必须满足 feature 对应精度和 range。

**DeFi 例外：** `model` 的 `defi` feature 包含 high-precision；`check_fixed_precision`（`fixed.rs:195–211`）在该 feature 下允许到 18。专门 `Price::from_wei` / `Quantity::from_wei` 使用 raw scale 18；f64 constructor 即使开启 DeFi 仍拒绝 precision >16。[S9] 不能写成“Nautilus 所有模式最多 16 位”；也不能因为这个类型分支存在，就声称普通股票/ETF 的所有撮合和核算都支持平台 scale 18。源码自身对 mixed raw scales 的 checked arithmetic 有专门限制（`price.rs:337–375`），需要逐条验证普通与 wei 路径是否混用。

未执行 high-precision/DeFi build、价格数量字符串往返、合成滑点 bid/ask、instrument tick/lot、Money currency precision、佣金税 rounding 或 account 对照。它们均为 **Unknown**；不以通信协议能存 18 位替代 backend 原生数值证明。

### 多次 run 是有证据的入口，不是项目锁步已验收

`run(..., streaming=true)` 不自动 `end`，保留引擎与策略；正常最后一次调用 `end()` 才结束。这个 bool **不受 Cargo `streaming` feature gate**：该 feature 主要开启 persistence / node streaming 配置依赖，不是启用 engine 的 run bool。[P1][S10]

源码 replay loop 先 `peek`，若 `ts_init > end_ns` 则 break，未消费越界项；否则处理并 `advance`，所以 end 含端点。`test_streaming_windows_retain_first_item_past_end`（`tests/integration/backtest_engine.rs:4034–4066`）两次 end=2/3，分别断言记录 [1,2] 和 [1,2,3]。批次测试（4001–4031）证明清数据后加入下一批，迭代数累计 3→5。[S10] 这些是 **T，不是本地测试结果**。

源码公开两种候选 seam：

1. Session 时间窗口：初始注册策略/数据；多次 `run(start, end, …, true)`，在完整 Session 边界收集反馈；最后 `end`。
2. 控制反转：Rust native `on_quote/on_bar` 同步产生订单，`on_order_filled/on_order_rejected` 更新/反馈平台账本。这只证明 hooks 存在；外部 worker IPC、锁/借用、重入和同步阻塞未测试。[S11]

发布注释明确警告每批必须包含最终 `ts_init` 的全部事件，不能切开同一时间戳，否则 timer / venue module 可过早 finalize（`engine.rs:685–689`）。此外每次 `run_impl` 会根据 start / 数据首时间重设 clocks（792–802）；不能由 end-window test 推断任意 start、timer 或同时间戳订单都已正确。Session 是平台日历语义，本轮没有证明日历边界与 backend replay clock 一致。

唯一建议下一步是继续 **ADR 0020 的 Python worker MVP-4 闭环验证**，覆盖 close signal、下一 open、HALT、Fill/Reject 后账本反馈和人工金标准；如另行评估 Rust 替换，需要项目负责人授权，再以独立运行实验验证上述 Unknown，而不是直接迁移。

## 五、不启用 Python 的清单可行性与稳定性声明

发布 manifest 四包默认 feature 都为 `[]`。[P1]–[P4] `backtest/python` 才转发 common/core/data/execution/model/portfolio/risk/system/trading 的 Python feature 并启用可选 PyO3；`extension-module` 也会启用 python。因此 no-python probe 不选二者。`execution/model` 的可选 Python 绑定也显式 gate；`trader` 则是只 re-export core/model/common 的 container，并非自动包含 BacktestEngine。

同时阅读同版本 analysis/data/portfolio/risk/system/common/core/trading manifests：默认均为空、Python 为显式选择。**manifest 说明设计上可行，实际 feature-unified 图另由第六节 cargo metadata/tree 验证**；将来其它依赖开启 python 时不能沿用本次结论。`--no-default-features` 或某个依赖的 `default-features=false` 不能关闭图中其它边已启用的 feature。

特别注意 `backtest` 发布 manifest 的 `package.metadata.docs.rs` 为文档构建显式开启 `defi`、`examples`、`high-precision`、`plugin`、`streaming`；**docs.rs 展示的 API 面并不等于默认 feature 的 API 面**。本报告核对原始 source 的 cfg 与 manifest，而不把文档上可见的 DeFi 构造器认作本轮默认探针已编译的接口。[P1]

固定提交官方 README 的版本说明（198–206）写明 active development，API release 间仍可能 breaking，release notes 仅 best-effort。[S12] `trader/lib.rs:16–20` 的 stable entry point 描述仅指小型 container。**没有据此证明完整 Rust BacktestEngine/strategy API 有稳定兼容承诺**；项目若采用必须钉版本、记录 backend identity，并在升级时复核行为。也不能把 Rust-native / production-grade 宣传用语当稳定性承诺。

## 六、独立资源实验（协调实现 agent 实测）

本节数值来自协调实现 agent 的独立实验，调研 agent 未重复安装、解析依赖或编译。实验全部位于 session `files/nautilus-resource-probe/`，未加入 workspace 成员，未改仓库依赖，不影响轻量 CI。

### 输入、环境、复跑

- 开始时间：2026-10-12T08:05:56Z；仓库基线 `039c19b61ade2512c895c27efdc67e63665cb2f9`，启动时文档 worktree 干净。
- macOS 26.2（25C56），MacBookPro17,1 / M1，8 cores / 16 GiB。
- rustc `1.98.1 (48a229cea 2026-09-01)`；cargo `1.98.1 (797e8a9bc 2026-08-05)`；host `aarch64-apple-darwin`。
- registry 使用已有 Cargo 配置的 USTC mirror，只抓取公开依赖。冷的是**独立 target**，不是 registry cache；未清理或覆盖共享 registry / 仓库 target。
- 探针 lock SHA256：`337acf3070839b0aafe82b78412fffcc2489996ef1895148fd9dbc300e45fcb3`。锁文件不入库，未来重新 resolve 的传递依赖可能不同；hash 是身份记录，不是能从 hash 复原 lock 的承诺。

完整探针 manifest：

```toml
[package]
name = "nautilus-rust-065-resource-probe"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
nautilus-backtest = { version = "=0.65.0", default-features = false }
nautilus-model = { version = "=0.65.0", default-features = false }
nautilus-execution = { version = "=0.65.0", default-features = false }
nautilus-trader = { version = "=0.65.0", default-features = false }
```

`src/main.rs` 只有 `fn main() { println!("Dependency build probe only; no backtest executed."); }`。这会编译声明的依赖，但不构造、调用或运行任何能力 API。

复跑命令模板（`PROBE` 指上述独立 session 目录；需已有本轮同一 lock 和 registry cache）：

```bash
cargo metadata --manifest-path "$PROBE/Cargo.toml" --format-version 1 --locked --offline
cargo tree --manifest-path "$PROBE/Cargo.toml" -e normal,build \
  --target aarch64-apple-darwin --locked --offline
CARGO_TARGET_DIR="$PROBE/target" cargo build \
  --manifest-path "$PROBE/Cargo.toml" --locked --offline --jobs 2
"$PROBE/target/debug/nautilus-rust-065-resource-probe"
```

metadata 计数排除 probe；tree 按唯一 package 身份去重，不把重复边算作额外依赖。实际环境 dev profile 默认 `debug=2`、`incremental=true`；并发 jobs=2。新实验需要先遵守资源闸门，不能直接使用这些命令绕过检查。

### 实测结果与资源闸门

依据 [POC 构建资源边界](poc-0-benchmark-spec.md#构建时间与磁盘预算)，至少保留 **10 GiB**。该文档的旧票据 13 是 [本地归档资源边界票据](../.scratch/poc-0-benchmark/issues/13-build-resource-boundary.md)，不是当前 GitHub #13 的不同主题。

| 项目 | 状态 | 实测 / 限定范围 |
| --- | --- | --- |
| 全 targets resolved deps | **已证实** | 176，排除 probe |
| host normal/build unique deps | **已证实** | 126，排除 probe；13 个 Nautilus 包均 0.65.0 |
| 无 Python / PyO3 / DuckDB 包 | **已证实** | 本轮解析图；Nautilus feature lists 均无 python，execution/model 传递启用的 `default` 为空 |
| 冷 target dev 构建 | **已证实** | exit 0，wall **72.150464917 s**；初始独立 target 不存在 |
| target logical 增量 | **已证实** | **1,223,202,556 bytes** |
| target allocated `du` | **已证实** | **1,200,552 KiB**，不与 logical bytes 混用 |
| 构建前 free | **已证实** | **47,860,666,368 bytes** |
| 2 s 轮询的 36 个样本最小 free | **已证实** | **46,642,737,152 bytes**，采样不能保证捕获瞬时最低点 |
| 构建后 free | **已证实** | **46,642,212,864 bytes** |
| 资源预算 / 中止线 | **已证实** | 对126 host deps、无 DuckDB/Polars 采用12 GiB保守增长预算；运行 free<11 GiB 则中止，2 s 轮询；本次未触发 |
| probe 可执行文件 | **已证实** | exit 0，仅打印上述一句；没有 backtest |
| release build / RSS / 冷 registry | **Unknown** | 本轮未测，不把 dev wall 当 release 性能 |
| 高精度 / DeFi 构建与回测运行资源 | **Unknown** | 本轮 manifest 未启用这些 feature |

free 前后变化是文件系统观察值，可能有其它共享环境活动，不当成全由该 target 引起；target 单独 logical/allocated 测量用于归因。独立 target 测量后由协调 agent 移除；没有 `cargo clean`，既有 targets 保持不动。资源探针验证的是本环境依赖可构建，不证明正式平台集成可运行、轻量 CI 环境必过或性能更优。

## 七、验证结论与开放项

本轮只新增本文，无执行逻辑、schema、依赖或前端修改；Rust/Python/Web 回归套件不适用，未为了文档跑业务测试。上游测试均仅阅读。独立构建探针是唯一 R 证据。

结论是 **源码能力面可行，但平台替换仍 unresolved**：自定义 fee handle、原生 HALT 新单拒绝与 streaming end windows 均有直接实现，不应误报为闭枚举/无暂停/仅状态类型；普通精度与稳定兼容承诺有明确限制。仍需实际最小策略、fill/reject 同步、平台账本权威、费用拆分、CNY CASH、同时间戳顺序、pending、scale 18 转换与金标准验证。资源实验不能替代这些验收，也不改变 A5 Python worker 的选型。

## 固定版本第一方引用

[P1]: https://docs.rs/crate/nautilus-backtest/0.65.0/source/Cargo.toml
[P2]: https://docs.rs/crate/nautilus-model/0.65.0/source/Cargo.toml
[P3]: https://docs.rs/crate/nautilus-execution/0.65.0/source/Cargo.toml
[P4]: https://docs.rs/crate/nautilus-trader/0.65.0/source/Cargo.toml
[P5]: https://docs.rs/crate/nautilus-trading/0.65.0/source/
[P6]: https://docs.rs/crate/nautilus-common/0.65.0/source/

- **S1 驱动**：[engine.rs:145–223、280、355、608–629](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L145-L223)；[策略注册](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L608-L629)；[L1/depth 检查](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L768-L789)。
- **S2 数据**：[add_data:420–439](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L420-L439)；[exchange 路由:1580–1631](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L1580-L1631)；[QuoteTick::new_checked:80](https://docs.rs/crate/nautilus-model/0.65.0/source/src/data/quote.rs#80)；[Bar::new_checked:1133](https://docs.rs/crate/nautilus-model/0.65.0/source/src/data/bar.rs#1133)。
- **S3 订单**：[OrderFactory::market:144](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/common/src/factories/order.rs#L144-L168)；[Strategy::submit_order:175–187](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/trading/src/strategy/mod.rs#L175-L187)；[Market 分派:3190](https://docs.rs/crate/nautilus-execution/0.65.0/source/src/matching_engine/mod.rs#3190)；[process_market_order:3743–3768](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/execution/src/matching_engine/mod.rs#L3743-L3768)。
- **S4 回调**：[事件分派:1430–1447](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/trading/src/strategy/mod.rs#L1430-L1447)；[Rejected:1606 / Filled:1666](https://docs.rs/crate/nautilus-trading/0.65.0/source/src/strategy/mod.rs#1606)。
- **S5 费用**：[trait/handle/enum:32–175](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/execution/src/models/fee.rs#L32-L175)；[venue handle:293–299](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/config.rs#L293-L299)；[fill 调用:5651–5686](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/execution/src/matching_engine/mod.rs#L5651-L5686)；[custom handle 测试:10279–10339](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/execution/src/matching_engine/mod.rs#L10279-L10339)。
- **S6 HALT**：[exchange status:1082–1106](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/exchange.rs#L1082-L1106)；[状态转换:2521–2547](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/execution/src/matching_engine/mod.rs#L2521-L2547)；[非 Open 拒单:2989–3000](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/execution/src/matching_engine/mod.rs#L2989-L3000)；[生成拒绝事件:3153](https://docs.rs/crate/nautilus-execution/0.65.0/source/src/matching_engine/mod.rs#3153)；[exchange handler visibility:526](https://docs.rs/crate/nautilus-backtest/0.65.0/source/src/exchange.rs#526)。
- **S7 CASH**：[venue type:274](https://docs.rs/crate/nautilus-backtest/0.65.0/source/src/config.rs#274)；[CashAccount 构造:189](https://docs.rs/crate/nautilus-model/0.65.0/source/src/accounts/any.rs#189)；[Cash instrument 限制:437–466](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/exchange.rs#L437-L466)。
- **S8 精度**：[fixed constants:85–110](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/model/src/types/fixed.rs#L85-L110)；[Price raw/range:87–141](https://docs.rs/crate/nautilus-model/0.65.0/source/src/types/price.rs#87)；[Quantity raw/range:90–121](https://docs.rs/crate/nautilus-model/0.65.0/source/src/types/quantity.rs#90)；[Price checked constructor:195–216](https://docs.rs/crate/nautilus-model/0.65.0/source/src/types/price.rs#195)；[decimal 路径:506–566](https://docs.rs/crate/nautilus-model/0.65.0/source/src/types/price.rs#506)。
- **S9 DeFi 例外**：[feature manifest][P2]；[check_fixed_precision:187–211](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/model/src/types/fixed.rs#L187-L211)；[Price::from_wei:32–49](https://docs.rs/crate/nautilus-model/0.65.0/source/src/defi/types/price.rs#32)；[Quantity::from_wei:32–43](https://docs.rs/crate/nautilus-model/0.65.0/source/src/defi/types/quantity.rs#32)；[mixed scale checked arithmetic:337–375](https://docs.rs/crate/nautilus-model/0.65.0/source/src/types/price.rs#337)。
- **S10 锁步**：[run 合约:677–750](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L677-L750)；[clock 重设:792–802](https://docs.rs/crate/nautilus-backtest/0.65.0/source/src/engine.rs#792)；[end break / advance / settle:904–972](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/src/engine.rs#L904-L972)；[上游批次和 window 测试:4001–4066](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/backtest/tests/integration/backtest_engine.rs#L4001-L4066)。
- **S11 数据 hook**：[DataActor on_quote/on_bar:509–535](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/common/src/actor/data_actor.rs#L509-L535)；[订阅 quotes:1568 / bars:1823](https://docs.rs/crate/nautilus-common/0.65.0/source/src/actor/data_actor.rs#1568)。
- **S12 稳定性**：[固定提交 README Versioning:198–206](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/README.md#L198-L206)；[container 的有限 stable 表述:16–32](https://github.com/nautechsystems/nautilus_trader/blob/7b766f8825b2539c5b2ac1375e9d97b41c509edb/crates/lib.rs#L16-L32)。

[S1]: #固定版本第一方引用
[S2]: #固定版本第一方引用
[S3]: #固定版本第一方引用
[S4]: #固定版本第一方引用
[S5]: #固定版本第一方引用
[S6]: #固定版本第一方引用
[S7]: #固定版本第一方引用
[S8]: #固定版本第一方引用
[S9]: #固定版本第一方引用
[S10]: #固定版本第一方引用
[S11]: #固定版本第一方引用
[S12]: #固定版本第一方引用
