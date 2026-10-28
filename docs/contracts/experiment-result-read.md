# 已保存 Experiment 结果的只读入口（T04a）

本契约固定 `prajna-experiment result` 及其 HTTP 适配的读取边界。入口只读取已持久化的 Experiment/Execution 和已经保存的结果表，不执行、不提升、不重算、不补零，也不修改 lake。

## CLI

```sh
prajna-experiment result <exp> <exe> --lake <lake> [--run <run-id>]
```

`exp`、`exe`、`run` 必须是 `exp:sha256:`、`exe:sha256:`、`run:sha256:` 身份。命令校验 Execution manifest 的实验/执行身份，并通过既有 hash 校验读取路径读取 Summary；Run 的 `sessions`（Vector）或 `account_valuations`（Fast Event）只在已保存时读取。退出码：`0` 成功；`1` 读取、身份、hash 或 I/O 失败；`2` 参数身份格式错误。错误写 stderr，不输出成功 envelope。

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
        "accounting_verified": true,
        "error": null,
        "summary": {},
        "series": {"sessions": []},
        "tables": {
          "saved": ["sessions"],
          "not_saved": ["target_weights"],
          "not_produced_by_engine": ["orders", "fills", "cash_ledger"]
        }
      }
    ]
  }
}
```

- `summary` 是已保存 `summary_json` 的原对象；失败 Run 可为 `null`。
- `series` 只含已保存的 NAV/权益表：Vector `sessions`、Fast Event `account_valuations`。行是存储 payload 合并非 `row_id`/`payload` 列后的对象；定点金额、数量、价格保持字符串，不由本入口转成浮点或重算。
- `tables.saved`、`tables.not_saved`、`tables.not_produced_by_engine` 来自 Execution manifest。缺表不得显示为零交易、零值或已保存。
- 本入口不返回 orders、fills、权重、排名等明细；后续明细票按 ResultLevel 扩展。

## 任务级别

`POST /api/v1/tasks/experiments` 增加可选 `result_level`：

- 省略或 `"summary"`：保持 T01 行为；不写该字段，旧请求 hash、旧 task.json 与旧 Summary 结果不变。
- `"standard"`：执行 `prajna-experiment run ... --level standard`，保存 Summary 加 Standard 表（含 NAV/权益序列）。该级别进入规范请求 hash 和任务记录；同一 `idempotency_key` 以不同级别或定义重提交返回 `409 idempotency_conflict`。
- 其他值返回 `400 invalid_request`。

## HTTP

`GET /api/v1/tasks/experiments/{id}/result`：

- 仅对 `succeeded` 任务调用上述 CLI；任务不存在 `404`，未成功 `409 task_not_completed`，结果身份缺失 `500`，CLI 不可执行 `503`，CLI 读取失败 `500`，协议/envelope 异常 `502`。
- 成功体逐字节保留 CLI envelope；响应 `application/json`、`Cache-Control: no-store`。
- 路由沿用本机 Host/Origin 边界，外部 Host、外部/opaque Origin 和预检返回 `403 result_origin_forbidden`，不继承旧 permissive CORS。
- 服务不新增 `prajna-*` Rust 依赖；CLI 经参数调用，不经 shell。

## 限制

- 本票只读 Summary 与 NAV 两张序列表；大 lake 性能、分页、按需明细和完整图表不在本契约。
- 公开验证只用合成 fixture；真实结果、明细、hash 和 Run/Execution 身份留在本机。
