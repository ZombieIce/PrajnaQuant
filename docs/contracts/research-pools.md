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
未知必要证据 422，损坏 500。定义固定 Universe 内容身份、DSV 及全部有效成本，
通过 `Experiment::parse` 后复用原持久任务提交。成功 selection envelope 增加
`pool:{id,version,universe,members}` 和 `instruments`，任务 experiment.json 的
Universe 指向不可变文件，其 label 可追溯池/版本；不会动态解析未来草稿。

旧单证券字段、响应与 Universe label/身份不变。此提交仍使用已有 entry-only
buy-and-hold，只接受单 Venue；沪深混合池可编辑/发布/读取，但该提交返回明确的
unsupported_venue（400）。本票不改多 Venue 引擎或把 entry-only 改为 full_target；
跨资产池产品执行由 #295 收口。单 Venue 多成员使用既有期初等权买入持有。
原始价格、默认成本、收盘 Decision/下 Session open、未知日线 available_at 的收盘
假设及分红总回报限制沿用[冻结选择契约](single-security-selection.md)。

## 验证范围

公开仅合成：CLI 测试覆盖 13 个预置成员、并发冲突、重复发布、旧版读取、损坏拒绝、
多成员冻结及旧单证券兼容；Playwright 启动真实生产服务/CLI，编辑/发布/读旧版本，
读取正式 Dataset 的不同手数/价格增量，提交固定版本并核对后续编辑不改旧任务。
复用独立手算 100 万/10.002 成交/99,900 数量及现金和权益，验证原时序/成本保持。
不宣称真实行情、全历史 PIT、公司行动总回报或整个 MVP-7 完成。
