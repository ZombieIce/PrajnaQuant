# 05: B1 Parquet 与超内存数据路径

**What to build:** 研究者可以从版本化 Parquet 输入运行三种 B1 候选，看到列和日期裁剪、扫描/解码、转换、计算与端到端的独立成本，并验证大于可用内存的数据可分块完成。

**Blocked by:** 03 B1 Arrow 完整运行同一策略；04 B1 Polars 完整运行同一策略；13 POC-0 构建资源基线与轻量边界。

**Status:** ready-for-agent

- [ ] 三种候选在相同投影、日期和标的过滤条件下输出同一 S2 结果与 Dataset 内容身份。
- [ ] 报告分开记录数据生成、Parquet 扫描/解码、布局转换、计算、结果序列化及总耗时；冷热缓存条件明确。
- [ ] 用受限内存的大样本证明列/日期裁剪和分块实际生效，记录峰值 RSS；资源不足时保存失败证据并标为 `unresolved`。
- [ ] 读取路径不改变窗口、缺失、排序及组合收益语义。
- [ ] 大数据生成/扫描前记录可用空间与预估产物；预计完成后不足 10 GiB 时停止该规模，保存失败依据和复跑条件，将该比较标为 `unresolved`，继续可运行的小规模正确性检查。
- [ ] 三种候选复用票据 13 的构建缓存；报告分别列出构建时间/产物增量与 Parquet 扫描、计算和运行时 RSS，不将构建耗时计入 rows/s。

## Implementation attempt — 2026-09-27

未实现，票据保持 `ready-for-agent`。2026-09-27 执行 `cargo tree -p quant-research --no-default-features --locked --offline -e normal` 时，Polars facade 的 `parquet` feature 因缺 `polars-sql` 索引元数据无法解析；改用直接 `polars-io/parquet` 后，同一依赖图命令因缺 `brotli` 索引元数据无法解析。联网命令 `cargo tree -p quant-research --no-default-features --locked -e normal` 因 DNS `Could not resolve host: mirrors.ustc.edu.cn` 失败。对 facade 与 `polars-io` 两条配置分别调用 `python3 poc/poc0-benchmark/capture-build-resource.py --profile dev --estimated-max-additional-bytes 2147483648 --output poc/poc0-benchmark/results/build-resource-poc0-05-dev-2026-09-27.json`，两次均在依赖图解析阶段退出、没有启动构建，也未写入 Cargo.lock 或产生 POC 05 源码变更；recorder 在异常退出前未保存 JSON 记录。

尝试时 `df -h .` 显示约 13 GiB 可用；预计增量按 recorder 默认的 2 GiB 上界估算，完成后约保留 11 GiB，高于 10 GiB 停止线。没有生成/扫描大样本，也未启动任何构建，所以未产生数据文件或 target 增量。本次阻塞来自依赖索引/网络，不是空间闸门。恢复锁定索引或 mirror 连接后，从 Parquet I/O feature 依赖图和共享 `target/` 构建资源记录开始重试；在此之前 out-of-core、RSS 与三候选 Parquet 对拍仍为 Unknown/Unresolved。
