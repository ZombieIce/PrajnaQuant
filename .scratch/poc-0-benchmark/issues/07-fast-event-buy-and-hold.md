# 07: B2 Fast Event 跑通 Buy & Hold

**What to build:** 研究者可以用统一入口让最小 Fast Event 原型运行 S1 Buy & Hold，并检查单 Venue、长仓、市价单、L1 成交和固定成本下的完整交易账本。

**Blocked by:** 01 固定数据集与统一基准入口。

**Status:** resolved

- [x] T 收盘产生的决策在下一可用 open 执行；缺 bar、不可成交状态和末日未执行目标按独立预期处理。
- [x] 订单、Fill、数量、成交价、佣金、滑点、现金、持仓及逐日 NAV 可追溯，且 `NAV ≈ cash + Σ(quantity × 估值价)`。
- [x] 同一输入可以重复生成相同账本和 checksum；正确性通过后才保存独立的初始化、事件处理与端到端计时。
- [x] 报告限定原型的市场与撮合语义，不把现有 ETF 回测直接称为完整 Fast Event Engine。

正确性范围说明：基准 fixture 覆盖 B2 的下一可用 open、B 停牌拒单及重试、B 末日缺 bar 的沿用估值和账本恒等式。另有 CLI 变体移除 A 的所有后续 bars 并启用非零买入税，独立断言末日 `no_future_execution_session` 待执行目标和现金非负；由于修改了固定输入，原 S2 金标准预期失败是有意的，B2 自身检查仍需通过。

## Comments

2026-09-27：已在统一 `benchmark-poc0` 报告加入 `b2_fast_event_buy_hold`；JSON 包含账本 checksum 和分列计时。全量 workspace 测试与 CLI golden/变体检查通过，具体验证记录见 `HANDOFF.md`。

2026-09-27 验收：独立复算金标准 fixture（`poc/poc0-benchmark/fixtures/dataset-v1.json`）逐日现金/持仓/NAV，手算结果与报告一致（Jan 13 现金 39740/NAV 100640 … Jan 16 现金 9610/NAV 101710，`total_cost` 390，`final_equity` 101710），B 在 Jan 13 HALTED 拒单、Jan 14 重试成交，B 在 Jan 16 缺 bar 沿用 Jan 15 收盘估值 99.0，均与代码路径吻合。非零买入税变体手算 `tax` 1201.2、`final_equity` 99738.8 同样吻合，末日未执行目标 reason 为 `no_successful_open_fill_before_dataset_end`（此为 B2 原型自身的终态原因字符串；上一条评论提到的 `no_future_execution_session` 是既有参考回测引擎的原因字符串，与 B2 无关，纯属评论措辞误引，不影响验收）。复跑 `cargo test -p quant-research --no-default-features --locked --offline --test poc0_benchmark_cli`（10 passed）、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 均通过。四条验收项与正确性范围说明均满足，标记 `resolved`。
