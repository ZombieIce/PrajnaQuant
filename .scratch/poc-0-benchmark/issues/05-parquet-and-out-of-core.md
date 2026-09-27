# 05: B1 Parquet 与超内存数据路径

**What to build:** 研究者可以从版本化 Parquet 输入运行三种 B1 候选，看到列和日期裁剪、扫描/解码、转换、计算与端到端的独立成本，并验证大于可用内存的数据可分块完成。

**Blocked by:** 03 B1 Arrow 完整运行同一策略；04 B1 Polars 完整运行同一策略；13 POC-0 构建资源基线与轻量边界。

**Status:** ready-for-human（256 MiB 受限复读通过；待验收）

- [x] 三种候选在相同投影、日期和标的过滤条件下输出同一 S2 结果与 Dataset 内容身份。
- [x] 报告分开记录数据生成、Parquet 扫描/解码、布局转换、计算、结果序列化及总耗时；冷热缓存条件明确。
- [x] 用受限内存的大样本证明列/日期裁剪和分块实际生效，记录峰值 RSS；资源不足时保存失败证据并标为 `unresolved`。
- [x] 读取路径不改变窗口、缺失、排序及组合收益语义。
- [x] 大数据生成/扫描前记录可用空间与预估产物；预计完成后不足 10 GiB 时停止该规模，保存失败依据和复跑条件，将该比较标为 `unresolved`，继续可运行的小规模正确性检查。
- [x] 三种候选复用票据 13 的构建缓存；报告分别列出构建时间/产物增量与 Parquet 扫描、计算和运行时 RSS，不将构建耗时计入 rows/s。

## Implementation continuation — 2026-09-27

在本机 Colima Linux arm64 / cgroup v2 上，以独立 Docker 卷冷构建 `--no-default-features` 二进制，重新生成 10,000,029 行、404,509,661 字节的同内容 Parquet。256 MiB RAM、256 MiB RAM+swap（无额外 swap）容器内 `--reuse --symbol A --start 2026-01-06 --end 2026-01-07` 连续两次退出 0、无 OOM；留档的第二次运行得到三候选 `passed`，只读取 2,448 组中的 1 组、7 列中的 3 列，投影 hash `75747678129f7004a1f75d6e60937d705ba48a887f4aa290b315e510bed06d75`。子进程峰值 RSS 58,933,248 字节；容器 cgroup `memory.max=memory.peak=268435456` 字节，`memory.events.max=646`、`oom=0`、`oom_kill=0`。cgroup 峰值包括 Python 测量进程及文件缓存，触顶说明发生回收压力，不能当作 Rust 进程 RSS；容器退出 0 及零 OOM 才是受限成功证据。原始报告、RSS 和容器配置/计数分别见 `poc/poc0-benchmark/results/parquet-large-10m-limited-cgroup-2026-09-27.json`、同名前缀 `.rss.json`、`.cgroup.json`。缓存不受控，单次耗时不能作布局选型。macOS `RLIMIT_AS` 失败记录仍保留，不作为本次 Linux cgroup 通过的证据。

本地 vendor 的 `polars-io 0.55.2` 仅删除 Parquet feature 对缺失 `brotli` 元数据的可选压缩依赖，writer 明确用 `Uncompressed`；`cargo check --locked --offline` 已通过。新增 `benchmark-poc0-parquet`：固定 fixture 写 Parquet + manifest、`--reuse` 校验 SHA-256 后复跑，列投影仅解码 `date/symbol/close`，按 symbol/date row group 跳过无关块。日期筛选保留起点之前完整历史及终点后一交易日的收益评价数据。三候选对拍同一筛选投影；全量还与原 fixture 逐 bar 和 B1 输出对拍。报告拆分生成、扫描解码、扫描转换、候选计算、布局转换、序列化、总耗时，并明确缓存未受控。该投影仍是评价收益，不是成交账本。

`--filler-rows 10000000 --symbol A --start 2026-01-06 --end 2026-01-07` 生成 10,000,029 行、404,509,661 字节 Parquet；仅扫描 2,448 组中的 1 组，候选校验通过。独立复读进程峰值 RSS 57,638,912 字节，明显小于文件大小；这是分块/裁剪有效的实测证据，但不是受限内存运行成功。256 MiB `RLIMIT_AS` 在本机 `setrlimit`/`preexec_fn` 阶段失败，子进程未启动，因此第三项保持 **unresolved**。原始记录见 `poc/poc0-benchmark/results/parquet-large-10m-2026-09-27.json`、`parquet-large-10m-rss-2026-09-27.rss.json`、`parquet-large-10m-limited-256-2026-09-27.rss.json`。复跑条件：可设置进程地址空间上限的主机或容器，使用 `measure-parquet-rss.py --limit-mib 256` 对同一 Parquet `--reuse` 运行；若地址空间限制与运行时映射不兼容，改用有可审计内存上限的容器。

继续复核发现 `ParallelStrategy::None` 在本地 Polars 的 row-group 循环里对零重叠组仍进入列读取路径；改用 `RowGroups` 后，读取器在解码前跳过零重叠组。相同 10M 文件复读再次通过，2,448 组中命中 1 组、3/7 列，峰值 RSS 57,573,376 字节；原始结果见 `poc/poc0-benchmark/results/parquet-large-10m-rowgroups-2026-09-27.json` 和同名前缀的 `.rss.json`。两次缓存状态未控制，不用其耗时差作性能结论；256 MiB 硬限制仍未验证。

共享 `target/` 的 dev 构建资源记录为 `build-resource-poc0-05-final-dev-2026-09-27.json`：6.32 秒、target 逻辑增量 463,309,462 字节，构建后可用 13,605,449,728 字节。较早的空间拒绝记录、仅删除一个已确认闲置的旧 DuckDB 编译缓存记录分别在 `build-resource-poc0-05-space-blocked-2026-09-27.json`、`poc0-05-cache-prune-2026-09-27.json`。构建成本独立于 Parquet 运行耗时。

## Implementation attempt — 2026-09-27

未实现，票据保持 `ready-for-agent`。2026-09-27 执行 `cargo tree -p quant-research --no-default-features --locked --offline -e normal` 时，Polars facade 的 `parquet` feature 因缺 `polars-sql` 索引元数据无法解析；改用直接 `polars-io/parquet` 后，同一依赖图命令因缺 `brotli` 索引元数据无法解析。联网命令 `cargo tree -p quant-research --no-default-features --locked -e normal` 因 DNS `Could not resolve host: mirrors.ustc.edu.cn` 失败。对 facade 与 `polars-io` 两条配置分别调用 `python3 poc/poc0-benchmark/capture-build-resource.py --profile dev --estimated-max-additional-bytes 2147483648 --output poc/poc0-benchmark/results/build-resource-poc0-05-dev-2026-09-27.json`，两次均在依赖图解析阶段退出、没有启动构建，也未写入 Cargo.lock 或产生 POC 05 源码变更；recorder 在异常退出前未保存 JSON 记录。

尝试时 `df -h .` 显示约 13 GiB 可用；预计增量按 recorder 默认的 2 GiB 上界估算，完成后约保留 11 GiB，高于 10 GiB 停止线。没有生成/扫描大样本，也未启动任何构建，所以未产生数据文件或 target 增量。本次阻塞来自依赖索引/网络，不是空间闸门。恢复锁定索引或 mirror 连接后，从 Parquet I/O feature 依赖图和共享 `target/` 构建资源记录开始重试；在此之前 out-of-core、RSS 与三候选 Parquet 对拍仍为 Unknown/Unresolved。
