# ADR 0015：MVP-0 数据契约（身份、定点、切日、Raw 布局、DSV）

- 状态：Accepted，2026-10-01；实现范围限于 MVP-0 合成 fixture
- 补充对象：[ADR 0011](0011-immutable-data-and-reproducible-runs.md) 中“物理存储路径与 hash 编码留给后续契约设计”的部分；ADR 0011 只在末尾追加一行指向本 ADR，原文不改。
- 来源：[spec Issue #7](https://github.com/ZombieIce/PrajnaQuant/issues/7) 的 D3–D8、D10、D11 与附录 A/B，以及已合并的票据 #8–#19。

本 ADR 固定 `prajna-domain` 与 `prajna-data` 已采用的契约。每条决定后列出实现与测试位置；没有实现或测试的部分在“未实现的决定”中单列，不视为已验证。所有数据均为合成 fixture（`poc/poc0-benchmark/fixtures/dataset-v1.json`），不证明真实市场数据、PIT 或收益口径。

## 1. Instrument 身份与 ID

1. `VenueId` 指交易所本身（`XSHG`、`XSHE`、`BJSE`、`BINANCE`、`OKX`、`HYPERLIQUID`），合成数据用 `SYNTH`。格式为首字符大写字母、2–16 位大写字母/数字/下划线。
2. `InstrumentId` 的规范形式为 `{SYMBOL}.{VENUE}`，symbol 只含大写字母、数字、`-`、`_`，长度 1–64，在 venue 内唯一且稳定。例：`510300.XSHG`、`BTC-USDT.BINANCE`（spot）、`BTC-USDT-PERP.BINANCE`（线性永续）、`BTC-USD-PERP.BINANCE`（反向永续）、`BTC-USDT-20261225.BINANCE`（交割）。
3. 不得从 ID 解析语义；语义全部在 `InstrumentSpec`（`kind`、`native_symbol`、`market_segment`、各币种（`Currency`：2–12 位大写字母/数字）、`is_inverse`、`multiplier`、`expiry`、`price_increment`、`size_increment`、`lot_size`、`session_timezone`）。`InstrumentKind` 为 `Equity | Etf | Spot | Perpetual | Future`；Perpetual/Future 必须有 `settle_currency`，只有 Future 可且必须有 `expiry`，两个 increment 必须为正。
4. 同一 ID 全生命周期只指一个标的：`InstrumentSpecs::insert` 对同 ID 同 Spec 幂等，同 ID 不同 Spec 返回 `IdReused`。
5. 旧格式（`SH:600000`、`sh510300`，SH/SZ/BJ）只提供单向映射到 XSHG/XSHE/BJSE，不改旧代码。
6. `Amount` 与 `Notional` 统一为 **`Notional`**（成交额类型），`Amount` 保留为类型别名；Arrow 列名仍为 `amount`。

实现：[`VenueId`](../../crates/prajna-domain/src/instrument.rs#L10)、[`InstrumentId`](../../crates/prajna-domain/src/instrument.rs#L100)、[`InstrumentKind`](../../crates/prajna-domain/src/instrument.rs#L182)、[`InstrumentSpec::new`](../../crates/prajna-domain/src/instrument.rs#L241)、[`InstrumentSpecs`](../../crates/prajna-domain/src/instrument.rs#L346)、[`instrument_id_from_legacy`](../../crates/prajna-domain/src/instrument.rs#L385)、[`Amount = Notional`](../../crates/prajna-domain/src/lib.rs#L163)。

测试：[ID 解析且不推断 kind（含 spot/perp/交割示例）](../../crates/prajna-domain/src/instrument.rs#L461)、[拒绝非规范 ID](../../crates/prajna-domain/src/instrument.rs#L486)、[Spec 的 kind/expiry/increment 校验](../../crates/prajna-domain/src/instrument.rs#L506)、[ID 重用拒绝与幂等](../../crates/prajna-domain/src/instrument.rs#L589)、[旧格式映射](../../crates/prajna-domain/src/instrument.rs#L616)。

## 2. 定点数物理类型

1. `Price`、`Quantity`、`Notional` 为 `i128` 尾数，scale 固定 18，要求 `|mantissa| < 10^38`；Arrow/Parquet 中为 `Decimal128(38,18)`，字段 metadata `prajna.decimal_scale=18`。
2. Instrument increment 只用于 tick/size 对齐校验，不决定存储 scale。
3. 转 `f64` 只能经显式 `to_f64()`；JSON 中十进制写成字符串（serde 只用字符串）；Raw JSON 的数字按字面量解析，不经过 `f64`（`serde_json` 启用 `arbitrary_precision`）。
4. 依 [ADR 0019](0019-fast-event-ledger-and-vector-parity.md)，三种定点类型提供 `checked_mul_decimal(Self)` 与 `checked_div_decimal(Self)`：乘除结果按 half-even 舍入到 scale 18，不做货币单位舍入。使用整数宽中间值，舍入后超出上述范围返回 `FixedPointError::Overflow`，除零返回 `FixedPointError::DivisionByZero`；原有 `checked_mul(i128)` 的整数乘法行为不变。公开接口的手算、正负舍入边界与独立 Python Decimal 向量见 [乘除测试](../../crates/prajna-domain/tests/decimal_arithmetic.rs)。此数值原语不表示 Fast Event 账本已实现。

实现：[`DECIMAL_SCALE`](../../crates/prajna-domain/src/lib.rs#L21)、[定点类型宏](../../crates/prajna-domain/src/lib.rs#L55)、[`to_f64`](../../crates/prajna-domain/src/lib.rs#L85)、[`decimal()` 字段构造与 metadata](../../crates/prajna-data/src/normalized.rs#L116)、[依赖配置](../../crates/prajna-data/Cargo.toml)。

测试：[JSON 数字字面量精确解析](../../crates/prajna-domain/src/lib.rs#L335)、[拒绝非 JSON/不精确/越界](../../crates/prajna-domain/src/lib.rs#L353)、[规范文本 round-trip](../../crates/prajna-domain/src/lib.rs#L370)、[对齐/乘法/比较](../../crates/prajna-domain/src/lib.rs#L380)、[仅显式 f64 转换](../../crates/prajna-domain/src/lib.rs#L397)、[serde 为字符串](../../crates/prajna-domain/src/lib.rs#L403)、[Raw 小数字面量不经 f64](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L1266)、[含负数/null 的 Arrow-Parquet round-trip](../../crates/prajna-data/src/normalized.rs#L873)。

## 3. 日线切日规则

1. 时间戳：`ts_open`/`ts_close` 为 `Timestamp(ns,"UTC")`，`session_date` 为 Date32。`TimestampNs` 只接受带显式 offset 的 RFC 3339。
2. 有交易时段的市场（A 股、`SYNTH`、未来美股）：`bar_spec = 1d@session`，`session_date` 为交易所本地交易日，区间为该 Session 的 `[ts_open, ts_close)`。
3. 连续交易市场（Crypto）：`bar_spec = 1d@+08:00`，区间为 `[00:00+08:00, 次日 00:00+08:00)`。只提供 UTC 日线的 venue 必须由更细 bar 重采样，否则拒绝生成（见“未实现的决定”）。
4. `available_at` 可为 null，表示 Unknown，不得默认为 `ts_close`。合成 fixture 取 `available_at = ts_close`，manifest 的 `provenance.synthetic_assumptions` 声明这一假设。`observed_at` 只出现在 Raw 来源记录，合成数据为 `"synthetic"`（由调用方写入，见 [`SourceRecordInput`](../../crates/prajna-data/src/raw.rs#L30) 与 [fixture 发布示例](../../crates/prajna-data/examples/fixture_v1.rs)，并未由库强制）。

实现：[`BarSpec`/`bounds`](../../crates/prajna-domain/src/time_bar.rs#L115)、[`Session`](../../crates/prajna-domain/src/time_bar.rs#L312)、[`Bar::new`](../../crates/prajna-domain/src/time_bar.rs#L406)、[fixture 的 09:30–15:00（+08:00）Session 生成](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L843)。

测试：[显式 offset 时间戳](../../crates/prajna-domain/src/time_bar.rs#L533)、[拒绝无 offset](../../crates/prajna-domain/src/time_bar.rs#L543)、[BarSpec 语法](../../crates/prajna-domain/src/time_bar.rs#L553)、[`1d@+08:00` 手算区间](../../crates/prajna-domain/src/time_bar.rs#L583)、[`1d@session` 区间](../../crates/prajna-domain/src/time_bar.rs#L591)、[Bar 校验且 `available_at` 保持 Unknown](../../crates/prajna-domain/src/time_bar.rs#L696)、[区间与 Spec/Session 不符被拒](../../crates/prajna-domain/src/time_bar.rs#L739)、[crypto 样式 ID 的 bar 主键排序（不验证窗口）](../../crates/prajna-data/src/normalized.rs#L956)、[fixture 的 `available_at = ts_close`](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L1135)。

## 4. Raw 布局与写入语义

Lake 根由调用方指定，不与 `data-core/` 混放：

    <lake>/raw/sha256/<hex[0:2]>/<hex>
    <lake>/raw-sources/sha256/<hex[0:2]>/<hex>.jsonl
    <lake>/normalized/<table>/<dsv_hex>/part-00000.parquet
    <lake>/manifests/<dsv_hex>.json

1. hash 写作 `sha256:<小写 hex>`。
2. Raw 写入：唯一临时文件 → fsync → 原子 rename；hash 已存在时逐字节比对，相同则幂等，不同则报错；`get` 与重复 `put` 都校验内容 hash，损坏对象报 `CorruptObject`。
3. 来源记录 jsonl 只追加，字段为 `raw_sha256`、`byte_len`、`content_type`、`source_kind`（`fixture | vendor_api | file_import | generator`）、`source_id`、`request`、`observed_at`、`ingested_by`。`request` 中 token/key/secret/signature/password/auth 类 key（大小写不敏感；含 URL query/userinfo、header、JSON/表单 body）替换为 `"[REDACTED]"`。
4. normalized 与 manifest 先写 staging 再原子 rename，manifest 是提交标记：没有 manifest 的 `normalized/` 目录视为未提交，重试时清除。已有 manifest 时，core 与已发布文件的 sha256/字节数一致则幂等，否则 `ExistingOutputConflict`。同 DSV 并发由文件锁串行化。
5. 不存在可变的“当前版本”指针。

实现：[`RawStore::put/get/sources`](../../crates/prajna-data/src/raw.rs#L112)、[凭证判定与脱敏](../../crates/prajna-data/src/raw.rs#L274)、[`publish_dataset`](../../crates/prajna-data/src/publish.rs#L157)、[`write_manifest`](../../crates/prajna-data/src/manifest.rs#L183)。

测试：[重复 put 幂等并追加来源](../../crates/prajna-data/src/raw.rs#L441)、[损坏字节在 get/put 报错](../../crates/prajna-data/src/raw.rs#L463)、[并发 put 无残留临时文件](../../crates/prajna-data/src/raw.rs#L482)、[凭证脱敏](../../crates/prajna-data/src/raw.rs#L508)、[发布幂等](../../crates/prajna-data/src/publish.rs#L647)、[同 DSV 并发发布只提交一份完整数据集](../../crates/prajna-data/src/publish.rs#L819)、[manifest 同步失败保留已提交文件且重试幂等](../../crates/prajna-data/src/publish.rs#L867)、[manifest 幂等/冲突/篡改检测](../../crates/prajna-data/src/manifest.rs#L466)。

## 5. Normalized 表与 Parquet

| 表 | 主键 | schema version |
|---|---|---|
| `instruments` | `instrument_id` | 1 |
| `bars` | `(instrument_id, bar_spec, ts_open)` | 1 |
| `sessions` | `(venue_id, session_date)` | 1 |
| `execution_status` | `(instrument_id, session_date)` | 1 |

按主键存储值字节序排序，固定 row group 65,536，ZSTD level 3，不做 hive 分区；writer 参数记入 manifest provenance。Schema metadata 为 `prajna.table`、`prajna.schema_version`、`prajna.dsv`；时间列字段 metadata 为 `prajna.time_role`。不兼容的 schema 变更必须递增 `schema_version`。

`execution_status`（MVP-1 M3，[spec Issue #53](https://github.com/ZombieIce/PrajnaQuant/issues/53)）的 `session_date` 为 Date32、`available_at` 为可空 UTC 纳秒时间戳；无某个 `(instrument_id, session_date)` 记录表示 UNKNOWN 且不可交易。MVP-1 M4 的 `synthetic-etf-daily` v3 已按标的与 calendar session 生成状态行（即使该日缺 bar），用同日 override 覆盖默认值，并将本地可用时刻按 fixture timezone 转换为 UTC；可选 `opens` 驱动 OHLC high/low。该表作为可选第四表发布并纳入 manifest/rebuild；现有 v1/v2 输出保持不变。

实现：[`SCHEMA_VERSION`/`ZSTD_LEVEL`/`ROW_GROUP_SIZE`](../../crates/prajna-data/src/normalized.rs#L21)、[`instruments_schema`](../../crates/prajna-data/src/normalized.rs#L141)、[`bars_schema`](../../crates/prajna-data/src/normalized.rs#L163)、[`sessions_schema`](../../crates/prajna-data/src/normalized.rs#L182)、[`execution_status_schema`](../../crates/prajna-data/src/normalized.rs#L193)、[`execution_status_to_record_batch`](../../crates/prajna-data/src/normalized.rs#L425)、[`execution_status_from_record_batch`](../../crates/prajna-data/src/normalized.rs#L465)、[`write_parquet`](../../crates/prajna-data/src/normalized.rs#L742)、[`read_parquet`](../../crates/prajna-data/src/normalized.rs#L779)。

测试：[D7 四表 schema 快照（字段、类型、nullability、metadata；schema 与期望数量相等）](../../crates/prajna-data/src/normalized.rs#L885)、[execution_status 排序、重复主键、hash 与 Parquet round-trip](../../crates/prajna-data/src/normalized.rs#L1009)、[instruments/sessions Parquet round-trip](../../crates/prajna-data/src/normalized.rs#L1113)、[主键未排序/非法 DSV 在建文件前被拒](../../crates/prajna-data/src/normalized.rs#L1196)、[row group 上限](../../crates/prajna-data/src/normalized.rs#L1228)、[pyarrow 读取已发布三表并校验 schema、metadata、行数与抽样数值](../../crates/prajna-data/tests/python/test_pyarrow_interop.py)（Full workflow `pyarrow-interop` job，出现跳过即失败）。

## 6. Dataset Version、逻辑 hash 与 schema 指纹

1. `dsv = "dsv:sha256:" + hex(sha256(受限 JCS(core)))`，`core = { manifest_version: 1, normalizer: {id, version, config_sha256}, inputs: [{raw_sha256}] 按 hash 排序, tables: [{table, schema_version, schema_fingerprint, logical_hash, row_count}] 按表名排序 }`。manifest 文件为 `{ dsv, core, provenance }`；`provenance`（文件路径/sha256/字节数、writer 参数、`created_at`、`git_rev`/`git_dirty`、主机、合成假设、覆盖率、被忽略的输入字段）不参与 dsv。
2. 受限 JCS 遵循 RFC 8785，值只允许字符串、布尔、null、`|n| ≤ 2^53−1` 的整数、数组与对象；禁止浮点数。
3. schema 指纹 = `sha256(受限 JCS({fields:[{name,type,nullable,metadata}], metadata}))`，字段保持 schema 顺序，schema 级 metadata 排除 `prajna.dsv`；字典编码按解码后的值类型。
4. 逻辑 hash：按主键升序，字节流依次为 `b"PRAJNA-LH\x01"`、表名（u32 LE 长度 + UTF-8）、32 字节 schema 指纹、行数（u64 LE）、逐行逐列的 tag + 值（null `0x00`、Utf8 `0x01`、Int64 `0x02`、Decimal128(38,18) `0x03`、Timestamp(ns,UTC) `0x04`、Date32 `0x05`、Boolean `0x06`）；字典/分块列按解码后的值；Float64 使用独立 tag `0x07`，按 `f64::to_bits()` 的 8 字节小端编码，拒绝 NaN，接受 ±inf，且不允许作主键；表外类型与重复主键报错。对整个流做 SHA-256，由此判定“逻辑相同”，不要求 Parquet 文件逐字节相同。
5. Normalizer 注册表按 `(id, version)` 登记；修订规则产生新版本。MVP-0 `synthetic-etf-daily` v1/v2 与 MVP-1 v3 只处理合成 fixture；v2 将 `volume` 按“手”×100，v3 保持该规则并加入执行状态及可选逐标的 open，不得用于真实数据。
6. 重建：`rebuild_dataset(raw_store, registry, manifest)` 按 `core.inputs` 读取 Raw 并校验 hash，取注册表中的 Normalizer 并校验 `config_sha256`，重算后逐项比对 `core`，返回 dsv；它不发布 Parquet。

实现：[`restricted_jcs`](../../crates/prajna-data/src/lib.rs#L57)、[`schema_fingerprint`](../../crates/prajna-data/src/lib.rs#L123)、[`logical_hash`](../../crates/prajna-data/src/lib.rs#L198)、[`Manifest`/`calculate_dsv`](../../crates/prajna-data/src/manifest.rs#L23)、[`NormalizerRegistry`](../../crates/prajna-data/src/normalizer.rs#L80)、[`synthetic-etf-daily` v1/v2/v3 配置常量](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L21)、[`rebuild_dataset`](../../crates/prajna-data/src/publish.rs#L346)。

测试：[Appendix C 外部 hex 向量](../../crates/prajna-data/src/lib.rs#L522)（fixture [`sessions-logical-hash.hex`](../../crates/prajna-data/tests/fixtures/sessions-logical-hash.hex)，期望 `2c627a18…2b58c` 见 [spec Issue #7 评论](https://github.com/ZombieIce/PrajnaQuant/issues/7#issuecomment-5902283970)）、[行序/分块/字典不改变 hash](../../crates/prajna-data/src/lib.rs#L570)、[指纹排除 `prajna.dsv` 但含字段 metadata 与类型](../../crates/prajna-data/src/lib.rs#L635)、[内容/null/字段顺序改变 hash](../../crates/prajna-data/src/lib.rs#L662)、[重复主键、非支持类型被拒](../../crates/prajna-data/src/lib.rs#L739)、[JCS 排序与浮点/大整数拒绝](../../crates/prajna-data/src/lib.rs#L765)、[固定 JCS/DSV 向量](../../crates/prajna-data/src/manifest.rs#L356)、[provenance 不影响 dsv 而 core 各字段影响](../../crates/prajna-data/src/manifest.rs#L374)、[注册表精确身份](../../crates/prajna-data/src/normalizer.rs#L123)、[v1 行数与手算 bar](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L1135)、[v1/v2 volume 修订版本化](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L1235)、[Raw 小数精确解析](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L1266)、[非法 fixture 结构化错误](../../crates/prajna-data/src/normalizer/synthetic_etf_daily.rs#L1293)、[v3 execution_status、opens OHLC、错误拒绝和 rebuild](../../crates/prajna-data/tests/rebuild.rs)、[非法 fixture 在任何 normalized 文件或 manifest 出现前被拒](../../crates/prajna-data/src/publish.rs#L918)、[缺 Normalizer / config hash 不符](../../crates/prajna-data/tests/rebuild.rs#L391)、[缺失或损坏的 Raw](../../crates/prajna-data/tests/rebuild.rs#L407)、[core 中变化项被定位](../../crates/prajna-data/tests/rebuild.rs#L440)。

## 7. 与现有 schema 7 发布链的边界

schema 7 发布链（[ADR 0008](0008-published-daily-snapshot-boundary.md)）不改，仍使用随机 snapshot ID 与现有只读 resolver。逐字段对应关系见 [`schema-7-publish-manifest-mapping.md`](../contracts/schema-7-publish-manifest-mapping.md)。回归测试为 [`daily_sync_is_idempotent_tracks_revisions_and_publishes_immutable_snapshot`](../../src/lib.rs#L3283)，覆盖 resolver 对固定 ID 与 current 指针的解析、文件 hash 与中断发布。

## 未实现的决定

- **Crypto 日线重采样**：“只提供 UTC 日线的 venue 必须由更细 bar 重采样，否则拒绝生成”没有实现，也没有测试；MVP-0 没有 Crypto 数据路径。仅有 `1d@+08:00` 的区间计算与 bar 校验，以及 crypto 样式 ID 的解析和排序测试。
- **Crypto 数据路径**：spot/perp/交割的 ID 解析与 `InstrumentSpec` 校验有测试，但没有对应的 Normalizer 或真实 venue 数据。
- **既有存储与 D6 原文的差异**：已发布的 normalized 目录冲突检测比较 manifest core 与文件 sha256/字节数，而不是重算逻辑 hash（spec D6 写的是“比对逻辑 hash”）。逻辑 hash 在发布前由 Normalizer 输出计算并写入 core。是否改为比较逻辑 hash 尚未决定，需项目负责人确认。
- **provenance 填充**：`git_rev`、`git_dirty`、`host` 字段存在，但 `publish_dataset` 当前写入 `None`；覆盖率与被忽略字段由 Normalizer 输出。
- **不在本阶段**：Python 端逻辑 hash、已发布快照导入为 Raw、分区策略、quarantine、别名/改名历史、命名 tag、旧 Normalizer 版本保留/下线、停牌与无成交表示、CLI（即 spec 的 Fog 项）。

## 后果

- 下游（MVP-1 Factor Cache、MVP-2 Run 身份）可以把 `dsv:sha256:<hex>` 作为输入数据身份；它只对合成 fixture 有验证证据。
- 引入真实数据前，需要为对应来源另写 Normalizer、覆盖率与 `available_at` 依据，并更新或新增 ADR。
- 任何改变逻辑 hash 字节编码、schema 指纹规则或 schema 的修改都会改变 dsv，必须递增版本并保留旧向量测试。
