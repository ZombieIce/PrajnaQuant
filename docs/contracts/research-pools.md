# 研究池版本发布与固定引用（#292）

## 行为与身份

`/pools` 管理 Spec #286 的跨资产、红利、国债/政金债三池，初始成员为
5/4/4 只 ETF。名单是事后选定的 Static Universe，保留幸存者偏差，
不表示历史 PIT 名单或投资推荐。三池独立，不做跨池资金配置。

首次读取在显式本机 lake 中初始化三个草稿并发布版本 1；后续启动不覆盖用户编辑。
池只保存完整 InstrumentId，不复制成立/上市日期、覆盖或交易规则。
页面从显式所选固定 Dataset 读取这些元数据；缺成员/未知字段明确显示 Unknown，
不补手数、价格或上市日。编辑没有行情的名单仍然允许，实验提交必须通过现有证据门禁。

草稿与发布分开。保存要求 1–1000 个不重复的六位数字 XSHG/XSHE 身份并排序；
发布为 `research-pool@1:<pool-id>:<ordinal>` 的既有 `StaticUniverse`，
沿用 `uni:sha256` 内容身份及不可覆盖存储，不改变算法/引擎版本。
相同草稿重复发布不产生新版本。旧版本/旧 Universe 不随草稿或后续发布修改。

`research-pools/registry.json` 是 schema_version 1 的草稿/版本索引，
与旧 UUID Universe 注册表分开。进程共享 `registry.lock` 串行读改写，
唯一临时文件写入、sync 后原子替换 registry。每次加载核对 schema、预置池身份、
版本序号、规范成员及其 StaticUniverse 内容身份/文件。损坏失败，不自动重置。
发布先存不可变 Universe 再存索引；中途失败可留下未引用 Universe，不产生伪发布版本。

## CLI / HTTP

`prajna-pools --lake <LOCAL>` 消费最多 64 KiB stdin JSON：

```json
{"op":"list"}
{"op":"save","id":"dividend","expected":["510880.XSHG"],"members":["510300.XSHG"]}
{"op":"publish","id":"dividend","expected":["510300.XSHG"]}
```

示例 expected 只是字段形状，实际必须为当前完整规范草稿；陈旧保存/发布返回冲突。
拒绝未知字段。成功退出 0，返回 `{schema_version:1,data:{schema_version:1,pools:[...]}}`；
池字段为 `id,name,draft,versions`，版本为 `version,universe,members`。
错误退出 2，返回 `{schema_version:1,error:{code,message}}`。

服务配置 `PRAJNA_POOLS_CLI`（默认 PATH 的 `prajna-pools`），沿 ADR 0025
子进程边界，不将新平台 crate 编译进旧服务。`GET /api/v1/research-pools` 等价 list；
`POST` 接受上述命令，返回 CLI envelope。根为 `serve --dataset-root`；未配置/CLI
不可用/根不存在 503，invalid_pool 400，pool_not_found 404，pool_conflict 409，
pool_store_failed 500，协议异常 502。响应 no-store，按实际本机 Host/Origin 限定，
路由位于旧 permissive CORS 外。该服务是本机单人入口，没有远程认证。

## 实验冻结与限制

`POST /api/v1/tasks/selections` / `prajna-freeze-selection` 新增可选
`pool:{id,version}`，与非空 `instrument` 互斥。只有明确发布序号可消费，没有 latest。
选择固定 DSV，加载该版本 StaticUniverse，逐成员核对 ETF、真实 venue、生命周期、
覆盖和正交易增量等现有门禁；不在浏览器计算。池缺版本 404，缺 Dataset 成员 404，
未知必要证据 422，损坏 500。**池提交还必须逐成员绑定 T07 adjusted version** 并
冻结共同评价窗口（`adjusted_versions` + `evaluation`），见
[共同评价窗口契约](evaluation-window.md)；缺失/损坏/DSV 或证券不符 fail closed。
定义固定 Universe 内容身份、DSV、逐成员复权版本、评价窗口及全部有效成本，
通过 `Experiment::parse` 后复用原持久任务提交。成功 selection envelope 增加
`pool:{id,version,universe,members}`、`instruments`、`adjusted`、`venues`、
`engine` 与 `window`（单 Venue 预检仍保留 `venue`），
任务 experiment.json 的 Universe 指向不可变文件，其 label 可追溯池/版本；
不会动态解析未来草稿。

旧单证券字段、Universe label/身份与已保存结果不变；成功响应只新增下文附加字段。默认提交仍使用已有 entry-only
buy-and-hold；T08b 可显式传入既有 native 参数组合（见共同窗口契约）。

**跨 Venue 池（#295/T06c）**：已发布版本可同时含 `XSHG` 与 `XSHE`。选择入口按池
成员的真实 Venue 集合加载一次共享 Session 轴，定义写排序后的 `venues`（单 Venue 仍写旧 `venue` 字段并保持旧身份）；只有多 Venue Run 改用 `full_target`
（ADR 0027/0028 要求），`buy_and_hold` 保持 `retry` 并省略 `entry_budget`。
预检 envelope 增加 `venues`，冻结 envelope 增加 `venues` 与 `engine`（`kind`、`semantic_version`，由 Rust 复用 Run Spec 规范化投影，例如多 Venue lot
为 `fast_event@14`）。单 Venue 多成员仍使用既有期初等权买入持有 entry-only 定义，
数值口径与多 Venue 基线不同。共享 Session 轴缺失日期或开收盘时刻不一致返回结构性 `inconsistent_venue_calendar`（HTTP 422），所选 Venue 无 Instrument/Session 返回
`invalid_venue_selection`（422）；不补齐、不压缩日历，也不选第一个 Venue。多 Venue 基线仍是价格收益、非 PIT、无公司行动，不作为总回报结论。
原始价格、默认成本、收盘 Decision/下 Session open、未知日线 available_at 的收盘
假设及分红总回报限制沿用[冻结选择契约](single-security-selection.md)。

## 验证范围

公开仅合成：CLI 测试覆盖 13 个预置成员、并发冲突、重复发布、旧版读取、损坏拒绝、
多成员冻结及旧单证券兼容；测试用 wrapper Normalizer 经正式 `publish_dataset` 发布
可执行沪深同池 Dataset（补合成 TRADABLE 状态；不是第二条生产发布路径），
`prajna-freeze-selection`/`prajna-experiment run`/`result` 跑通多 Venue 基线并核对
独立手算 1,000,000 本金、50,000/49,900 数量、现金 `500.44006` 与权益
`999500.44006`；共享轴损坏在两个入口均返回结构性错误且不写 Execution。Playwright
启动真实生产服务/CLI，编辑/发布/读旧版本，读取正式 Dataset 的不同手数/价格增量，
提交单 Venue 固定版本并核对后续编辑不改旧任务，另以跨资产版本走 `/pools` 预检、
提交、冻结身份展示与 `/backtests` 持久结果读取。复用独立手算 100 万/10.002 成交/
99,900 数量及现金和权益，验证原时序/成本保持。不宣称真实行情、全历史 PIT、
公司行动总回报、真实跨 Venue 数据装配或整个 MVP-7 完成。
