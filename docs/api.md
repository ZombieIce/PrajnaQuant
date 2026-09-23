# 已实现 API 与规划边界

服务入口 `quant-research serve`（`server.rs`），默认 `127.0.0.1:7878`；只读 GET，Axum 托管 `apps/web/dist`。CORS 当前 permissive，服务无身份认证。下表是**实际路由**，未出现的接口不得按已实现使用。

| 分类 | 方法/路径 | 现状/响应 |
| --- | --- | --- |
| Metadata | `GET /api/v1/health` | Implemented；`status/service` |
| Factor | `GET /api/v1/signals` | Implemented；固定信号目录 |
| Factor | `GET /api/v1/signals/{key}` | Implemented；单个定义，未知 key 为 404 |
| Result / Factor | `GET /api/research/signals/{key}/report` | Implemented；按文件修改时间找最新已保存 report，未运行则 404 |
| Result / Strategy | `GET /api/experiments` | Implemented；枚举 `experiments/*/experiment.json` 汇总，最新在前 |
| Result / Backtest | `GET /api/experiments/{id}` | Implemented；返回完整 config、snapshot、factor、backtest、trades、equity/benchmark curves |

**未实现**：策略/因子配置的 CRUD、POST 回测/网格执行、batch 查询路由、日期范围/aggregation/downsampling/pagination 参数、独立成交/仓位/时序查询、计算基准/超额指标 API。上述是未来需要讨论的 API 方向，不是已承诺的路由。当前配置和运行通过 CLI；保存结果文件，不提供服务端作业状态。

## 规模与契约风险

实验详情一次序列化返回全部结果，包括 factor.daily、equity_curve、benchmark_curve、trades。当前本机多个实验 JSON 约 2.7–3.0 MB；十年更多因子/交易会放大响应、浏览器内存和重绘成本。`/api/experiments` 会逐文件读取并解析所有实验以生成目录；因子目录在浏览器对十个信号分别请求已保存报告。React ECharts 有 tooltip、inside/slider zoom，曲线 `showSymbol:false`；没有 brush/crosshair、显式抽样/虚拟化或十年大数据性能验收。新 API 设计应先定义时间范围、分页和是否由后端降采样；不要在本轮改路由。

`server.rs` 以 `web_dist` 静态回退，仓库中旧 `dashboard.html` 和 `factor_dashboard.html` 当前未在服务端引用。`/api/research/...` 与 `/api/v1/...` 路径版本风格不统一；变更现有路径前检查前端调用。
