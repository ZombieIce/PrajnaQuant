# 13: POC-0 构建资源基线与轻量边界

**What to build:** POC 开发者可以在不重复编译 bundled DuckDB、不为每个候选生成独立 target 的路径上迭代 B1/B2/B3，并在新增重依赖前看到真实构建时间与磁盘成本。

**Blocked by:** 无；03/04 已开展工作保持原实现和验收状态。本票是 05、08、10 等后续资源密集构建的前置条件。

**Status:** resolved

**Conclusion:** 轻量构建边界已实现；冷构建基线仍为 Unknown。

- [x] 只读清点工作区和独立 crate 的依赖图、lockfile、Cargo 默认 profile、target 路径/大小、可用空间、大型 DuckDB 构建目录；快照见 `build-resource-inventory-2026-09-27.json`，图文件见 `dependency-tree-*.txt`。`lsof` 在最终测量前检查两个 workspace `.cargo-lock` 均无持有者；更早的 pilot 前进程枚举不可用，未改缓存。
- [x] 选择 `quant-research` 可选 `app` feature 边界：默认构建保持应用依赖；`--no-default-features` 保留 POC CLI/语义契约，依赖图实测不含 `ashare-warehouse`、`duckdb` 或 `libduckdb-sys`。B1/B2/B3 使用仓库根 `target/`。旧 `poc/b1-layout` 独立 lockfile 固定 Arrow 60.0.0，主 POC 固定 Arrow 58.4.0；版本差异已说明。
- [x] 每次构建前记录空间并按 10 GiB 保留线估算；记录器每两秒轮询，低于 10 GiB 加安全边际时发 SIGINT 停止 Cargo。最终 dev/release monitored reruns 均完成且未触及中断阈值。3 GiB 完工估算的 release 命令被闸门拒绝并保存为 `build-resource-blocked-2026-09-27.json`；更早一次 pilot 在可用空间读数波动时被手动中止，未得到可靠耗时。
- [x] warm-cache dev/release 构建的 wall time、命令、Git/dirty diff、完整 untracked hash、lockfile、feature/profile、共享 target、前后字节和卷空间已存档。初始全增量样本及带 source identity 的最终受监控复跑分别见 `build-resource-*-2026-09-27.json`、`build-resource-*-final-2026-09-27.json`；source 清单见 `build-source-manifest-2026-09-27.json`。冷构建未测，状态 `Unknown`。
- [x] 未释放空间；没有运行 `cargo clean`、删除 target 或改动现有缓存。
- [x] 构建成本作为独立工程成本记录在 POC README 与 JSON，未并入运行时吞吐。03/04 输入/报告契约保持不变。

**Measured build snapshot (2026-09-27, warm shared target):**

- Dev: 39.645 s; `target/` grew 511,089,592 bytes; free space 14,064,726,016 → 13,253,484,544 bytes.
- Release: 264.239 s; `target/` grew 461,279,366 bytes; free space 13,267,410,944 → 12,736,319,488 bytes.
- Both full builds used dependency graph SHA-256 `5aa7f1c14c5b318b00e2b61a3122e99c5214ab19ea79d8e4fafe1d879c3ea1c4` and completed successfully. Final monitored reruns with exact dirty/source identity took 21.175 s (dev) and 3.017 s (release); ten/two free-space samples respectively stayed above 13 GiB. A separate 3 GiB estimate was refused by the gate with 10,964,553,728 bytes free.
- Inventory `du -sh` figures are allocated filesystem space, while `workspace_target_bytes` in resource JSON is the sum of file logical lengths; this explains the apparent 59G vs ~74 GiB difference. Neither is a stable machine specification.
- No cold-build sample. Neither `target/` was cleaned. Runtime benchmark measurements remain separate.

**Acceptance evidence:** 可复跑的轻量构建命令、依赖图证据、无 DuckDB 构建证据、共享缓存路径、前后磁盘快照、增量与 release 构建原始耗时，以及资源不足时的 `unresolved` 样例或明确的未触发说明。
