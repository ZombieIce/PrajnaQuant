# 已保存 Experiment 结果的只读入口（T04a）

本契约固定 `prajna-experiment result` 及其 HTTP 适配的读取边界。入口只读取已持久化的 Experiment/Execution 和已经保存的结果表，不执行、不提升、不重新计算回测结果、不补零，也不修改 lake；展示权重只在 Rust 从已保存估值投影。

## CLI

```sh
prajna-experiment result <exp> <exe> --lake <lake> [--run <run-id>]
```

`exp`、`exe`、`run` 必须是 `exp:sha256:`、`exe:sha256:`、`run:sha256:` 身份。命令校验 Execution manifest 的实验/执行身份，并通过既有 hash 校验读取路径验证 Summary **以及 manifest 中每一张已保存表**；Summary、已保存的 NAV/权益表（Vector `sessions`，Fast/Accurate `account_valuations`）及原生明细进入 envelope。任一已保存表 hash 不符都会使命令失败，即使该表不属于 NAV 序列。`--run` 只过滤返回的 Run，不缩小校验范围。退出码：`0` 成功；`1` 读取、身份、hash 或 I/O 失败；`2` `result` 的参数身份格式错误。错误写 stderr，不输出成功 envelope。

成功只输出一个 JSON envelope：

```json
{
  "schema_version": 1,
  "data": {
    "exp": "exp:sha256:...",
    "exe": "exe:sha256:...",
    "default_result_level": "standard",
    "runs": [
      {
        "run_id": "run:sha256:...",
        "status": "completed",
        "result_level": "standard",
        "accounting_verified": null,
        "error": null,
        "summary": {},
        "series": {"sessions": [{"session_date": "2026-01-05", "nav": 1.0}]},
        "tables": {
          "saved": ["corporate_actions", "executions", "sessions"],
          "not_saved": [
            "target_weights",
            "weights_after_execution",
            "decisions",
            "pending_at_end"
          ],
          "not_produced_by_engine": ["orders", "fills", "cash_ledger"]
        }
      }
    ]
  }
}
```

- `summary` 是已保存 `summary_json` 的原对象；失败 Run 可为 `null`。
- `series` 只含已保存的 NAV/权益表：Vector `sessions`、Fast/Accurate `account_valuations`。行是存储 payload 合并非 `row_id`/`payload` 列后的对象；本入口不重算。Vector `nav`/`net_return` 是已保存的 JSON number；Fast/Accurate `account_valuations` 的定点金额/权益保持字符串。客户端须按表解析，不能把 Vector NAV 当定点字符串。
- `accounting_verified` 对 event Run 是 boolean，对 Vector Run 为 `null`（Vector 没有该账本标记）。
- 失败 Run 返回 `status: "failed"`、原 `result_level`、`summary: null`、空 `series`；`not_saved` 来自 manifest，目标级别表不会被伪造为已保存。
- `tables.saved`、`tables.not_saved`、`tables.not_produced_by_engine` 来自 Execution manifest。`saved` 表示该表已通过 logical hash 校验，而不是“存在但未验证”；缺表不得显示为零交易、零值或已保存。
- `details` 是其余已保存非 Summary 表的对象，按表名返回原生行（包括订单、成交、费用结算、执行尝试、权重、pending、行动账本及已有 Backend 审计）。仅已保存表有键；已保存空表是 `[]`，不把未保存或引擎不产生的表补成空数组。payload 中已有字段优先，附加 Arrow 列只补缺字段，避免 JSON 数组被辅助字符串列替换。
- 明细中的 `executed_at/submitted_at/attempted_at/ts_open` 数字或 RFC3339 时间统一转为十进制字符串，单位 Unix ns，避免浏览器 Number 丢精度；其他金额、数量仍保留原生字符串，Vector 比例保留原生 number。
- `allocations` 是从已保存 `account_valuations` 读取的 Rust 展示投影；未保存该表为 `null`，已保存空表为 `[]`。每行保留日期/估值点、现金、权益、两类应收、持仓数量/价格/市值，并补 `cash_weight`、两类应收 `*_weight`、逐证券 `weight`。权重 = 对应已保存金额 / 含应收总权益，使用既有 scale-18 定点除法，ties-to-even；不改变成交、账户或持久文件。应收计权益但不可花费，现金权重不含应收。缺字段/非法金额/非正权益不能算权重，返回 `null`；总权益不可用时附 `weight_unavailable_reason=missing_or_nonpositive_equity`。不从缺字段反推零持仓/零现金。
- `units` 包含 `currency`、`weight=fraction_of_total_equity`、`vector_cost=fraction_of_previous_nav`、`timestamp=unix_nanoseconds`。币种读取已保存 canonical `experiment.json` 的 `fast_event.currency`，并核验该文件内容身份；文件缺失或 Vector 无账户币种时为 `null`，不假设 CNY。缺定义文件仍可读已保存表；定义存在但损坏/身份不符则读取失败。不依赖原行情或重新运行。

## 明细页面（T21）

`/backtests` 复用同一结果入口，逐 Run 展示实际持仓/现金/应收与权重、成交及比例佣金/最低佣金补足/税/滑点、独立费用结算、订单/延期/跳过/数量调整和期末目标/风险退出 pending。Summary `executed_count` 为执行事件数（包括成功全现金目标），不是 Fill 数；数量调整不是成交；含滑点成交价的滑点金额仅作解释，不能再次扣费。Vector 展示已保存开盘执行后权重、Session 的开盘/收盘现金及应收比例与成本率，不伪造金额账本或逐笔 Fill。

每表分别显示 manifest 的未保存、引擎不产生、不可用、已保存空表。仅通过校验的已保存空成交表可以说明该表确认无成交，仍提示核对费用与受阻执行。失败/损坏结果沿用整体错误，不显示空交易。旧结果缺明细/缺资金构成保持不可用，页面不自动提升保存级别或执行。

## 任务级别

`POST /api/v1/tasks/experiments` 增加可选 `result_level`：

- 省略或 `"summary"`：保持 T01 行为；不写该字段，旧请求 hash、旧 task.json 与旧 Summary 结果不变。
- `"standard"`：服务先调用 `prajna-experiment promote <experiment.json> --lake ... --level standard`。已有 Summary Execution 时只提升仍低于 Standard 的 **Completed** Run（failed Run 保持原级别，不阻塞）；没有可提升的 Completed Run 时返回 `action: replayed`（含全 failed，Standard 任务仍 succeeded 并保留 `failed_count`）；该 no-op 仍校验 Summary/已保存表，损坏则失败。混合结果由 completed Run 提升、failed Run 保持原级别。选中 Completed Run 若在提升重放中失败，promote 整体失败、不发布 partial 表，任务 failed 并保留 Run id/原因；没有当前 Execution（目录不存在，promote exit 6）时才回退到 `prajna-experiment run ... --level standard`；目录存在但 manifest 缺失/损坏或其他 promote 失败不回退，避免掩盖真实错误。`default_result_level` 保持 Execution 创建时的级别，`promote` 只提高 Run 级别；重复的 Standard 任务仍走 promote 的 no-op/replay。该级别进入规范请求 hash 和任务记录；同一 `idempotency_key` 以不同级别或定义重提交返回 `409 idempotency_conflict`。
- 其他值返回 `400 invalid_request`。

## HTTP

`GET /api/v1/tasks/experiments/{id}/result`：

- 仅对 `succeeded` 任务调用上述 CLI；任务不存在 `404`，未成功 `409 task_not_completed`，结果身份缺失 `500`，CLI 不可执行 `503`，CLI 读取失败 `500`，协议/envelope 异常 `502`。
- 成功体逐字节保留 CLI envelope；成功与错误响应均为 `application/json`、`Cache-Control: no-store`。
- 路由沿用本机 Host/Origin 边界，外部 Host、外部/opaque Origin 和预检返回 `403 result_origin_forbidden`，不继承旧 permissive CORS。
- 服务不新增 `prajna-*` Rust 依赖；CLI 经参数调用，不经 shell。

## 限制

- 本入口逐表校验并返回已保存结果；大 lake 性能、分页、按需提升和完整图表不在本契约，当前明细为整体返回。
- 公开验证只用合成 fixture；真实结果、明细、hash 和 Run/Execution 身份留在本机。

## 策略—基准对照结果

`GET /api/v1/tasks/comparisons/{id}/result` 复用本契约的每角色
`prajna-experiment result`，校验持久 exp/Run 身份、共同 evaluation 起止与 close
日期集、会计/收益口径/公司行动版本，再组合为 `strategy`/`benchmark` 两个角色；
结构性不一致、缺基准和行动事实不完整的错误/`comparable=false` 语义见
[ADR 0035](../decisions/0035-strategy-benchmark-comparison.md)。旧单 Run 结果入口
及其错误映射不变。

对照的 `comparability.validation_version=effective-rules-calendar@2` 表示已核验冻结
行情滑点买价与 tick 对齐、再投资日历来自实际策略 Decision。旧比较缺此版本时
保留权威结果读取，但 `comparable=false`，原因 `comparison:effective_rules_calendar_unverified`。
