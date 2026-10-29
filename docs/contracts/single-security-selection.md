# 单证券冻结选择与任务提交（#340）

本入口完成固定 Dataset Version 的单证券选择 → Rust 冻结定义 → 既有 T01
持久任务。首个产品使用单 ETF；通用身份路径兼容沪深股票。原单证券入口保持兼容；#292 新增池引用与页面见[研究池契约](research-pools.md)。本契约不表示跨 Venue
组合、PIT Universe 或完整公司行动总回报入口。浏览器仅提交选择，不计算费用或绩效。

## 生产入口与默认配置

```sh
printf '%s' '{"dsv":"<DSV>","instrument":"<ID>","strategy":"buy_and_hold"}' \
  | prajna-freeze-selection --lake <LOCAL>
```

stdin 为一个 JSON 对象，最多 64 KiB；必填 `dsv`、完整 `instrument`（证券代码及
`XSHG`/`XSHE`）、`strategy: buy_and_hold`。拒绝未知字段、非法身份和其他 venue。
可选 `settings` 的字段及 Rust 默认值如下；省略字段由版本化 Rust 配置补齐，浏览器
不填隐含值。JSON null 不能替代 settings 对象或其数值字段。

| 字段 | 默认值 | 单位 |
| --- | --- | --- |
| `initial_cash` | `"1000000"` | CNY，精确十进制字符串 |
| `commission_rate` | `0.0003` | 单边比例，3bp |
| `minimum_commission` | `"5"` | CNY，精确十进制字符串 |
| `buy_slippage_bps` / `sell_slippage_bps` | `2.0` / `2.0` | 各方向 bp |
| `buy_tax_rate` / `sell_tax_rate` | `0.0` / `0.0` | 各方向比例 |

默认版本/来源为 `mvp7-d7-simulation@1`（Spec #286 D7）；仅为模拟假设，不代表券商
收费。冻结定义显式保存全部实际参数、`fast_event`、CNY、252 Session/年、单 VP
全部本金、`sizing: lot`、`lot_size_source: instrument`、`rebalance: entry_only`、
`entry_budget: remaining_cash`、`unfilled_entry: retry`、buy-and-hold 参数版本 1。
不主动恢复持仓权重；本入口不新增分红再投资或公司行动模型。

CLI 返回前必经 `Experiment::parse`，失败不提交任务。成功 stdout 是
`{schema_version:1,data:{selection_version:1,defaults_source,definition,instrument,warnings,limitations}}`；
错误是 `{schema_version:1,error:{code,message}}`，退出码 2（clap 参数错误使用其标准
stderr/退出 2）。成功退出 0。HTTP 不传播 stderr。

## 证据、持久化与身份

复用 Dataset preview 的正式 manifest/必需表及冻结证据读取，要求证券属于该 DSV、
真实 venue、CNY、ETF/Equity、正 lot/价格/数量增量、publishable 证券条款、Known
上市日期、非空 Bars/Session、首 Bar 不早于上市且每 Bar 有来源。未知/缺失/冲突拒绝，
不猜身份、不补手数/价格、不把缺行情当停牌或可成交。区间缺口继续返回 warning，
不删除 Session，也不套用旧最少三只、61 Session 或成交额门禁。

`StaticUniverse` 仅一个成员，label 固定 `single-security@1:<instrument>`，身份继续使用
既有算法。相同成员跨 DSV/费用场景复用相同 Universe；DSV/有效成本仍进入既有
Experiment 身份。同一 Universe 重复调用不重写其文件。后续 T05 可引用这个身份，
改池时生成新的 Universe，不改写此处已保存文件。

CLI 只新增 `universes/` 中的内容寻址文件及锁，不写行情、Raw、Dataset、结果或旧
MVP-6 存档。内容寻址文件名为 `<sha256>.json`，与旧用户 Universe 注册表共用目录；
旧 `UniverseStore` 只读取/列出 `<uuid>.json`，因此 `GET /api/v1/universes` 不列这些
文件，也不会因它们返回 500。通过证券证据后、定义解析前可能已保存一个可复用
Universe；定义失败不会产生任务或伪成功结果。默认版本/来源在本契约及成功 envelope
中说明；持久任务保存完整有效定义，不依赖未来默认值重建。

## HTTP

`POST /api/v1/tasks/selections`：同上字段，另有必填 `idempotency_key`，可选
`result_level: summary|standard`（沿用 T01 缺省 summary）。无测试专用生产端点。
服务调用 `PRAJNA_SELECTION_CLI`（缺省 PATH 中 `prajna-freeze-selection`）并把 stdin
转给 CLI，不直接依赖新平台 crate。CLI 成功后仅调用既有 T01 提交函数。

`serve --dataset-root <LOCAL> --output <LOCAL>` 必须 canonicalize 为同一实际目录；
原因是 T01 固定在 `--output` 用 `--lake` 执行/读取。不复制或迁移 Dataset。预览仍
可使用独立 root，只有这个提交入口要求相同。符号链接/相对路径按 canonical root
比较；目录必须存在。未配置/不可读 503，根不匹配 409。

HTTP 成功为 `{task,created,selection}`，新任务 202，幂等复用 200；任务定义、请求、
级别、状态与结果继续保存在 `output/tasks/<id>/`，之后使用既有任务 GET/结果入口。
同键不同冻结定义/级别返回 T01 的 409；队列满/非法键/非法级别沿用 T01 错误。
CLI 输入/定义/venue 无效 400，Dataset/成员不存在 404，必要证据不足 422，
根/可执行文件不可用 503，manifest/表损坏、读取或保存失败 500，CLI 协议不一致 502。
错误按 `UniverseError`/`ExperimentError` 变体分类，不把 manifest、表、读取失败
统一折叠为调用方 400；`MissingMembers` 按成员不存在处理。本机 Host/Origin 边界复用
Dataset/结果路由，位于旧 permissive CORS 之外。

## 时间、手算与限制

沿用 Rust BuyAndHold 的收盘 Decision → 下一 Session open 尝试，不改引擎时序。
合成 3 Session、open=10、close=10.1、lot=100、本金 100万：首日无仓；第二日成交价
10.002，买 99,900 股/份，成交额 999,199.8，佣金 299.75994（高于最低 5），
现金 500.44006；开盘后按 10 估值权益 999,500.44006，收盘按 10.1 估值权益
1,009,490.44006；第三日不主动再平衡。E2E 逐日核对上述值以及
`equity = cash + positions`（应收为零），首日无仓证明未按同日 close 交易。

成功链使用 #350 正式固定状态证据输入，经 `prajna-publish-daily` 发布
`cn-stock-etf-daily@2` / recipe2；状态证据明确标为合成，不从 Bar 推导。
已有 v1 无 execution_status 时仍沿原执行器报错，冻结定义可被 parse 接受不表示
所有必要执行输入均可用。整表缺失、单行 Unknown/缺失或晚到不被此入口放宽。

仅合成验证。Static Universe 有事后选择/幸存者风险；日线 Unknown available_at
显式按 close 可用假设，非历史发布时间证明。使用原始价格，没有公司行动完整性
证明，不宣称分红总回报已验收。缺行情/期末 pending 仍由既有引擎如实记录。

```sh
cargo test -p prajna-experiment --test selection --locked --offline
cargo test -p quant-research --lib selection_api --locked --offline
cargo build -p prajna-data --bin prajna-publish-daily --locked --offline
cargo build -p prajna-experiment --bins --example selection_fixture --locked --offline
cargo build -p quant-research --bin quant-research --locked --offline
python3.12 scripts/test-selection-http.py target/debug
```

选择/错误测试用合成沪深 ETF/股票；成功 E2E 复用 #350 的沪深 ETF 状态 fixture
并通过实际生产发布 CLI，不访问外部源。单元测试覆盖逐项证券证据门禁分支和
`UniverseError`/`ExperimentError` 的 400/500 分类；E2E 在真实选择后检查
`GET /api/v1/universes` 仍为 200 且不列内容寻址文件，并逐一启动缺少/不匹配
`--dataset-root` 的生产服务验证 503/409。最后一个脚本真实启动生产 HTTP 服务、
冻结/执行 CLI、读取持久结果；身份与明细仅存在每次运行的临时本机目录，成功时删除，
失败时保留并只在终端报告该临时路径；不提供把缺表失败当成功的验收模式。
HTTP 单元测试的模拟 CLI 仅验证适配错误/持久化，不替代此 E2E。
