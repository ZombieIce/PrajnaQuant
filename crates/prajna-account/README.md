# prajna-account

MVP-3 的单 Venue、单记账币种、长仓、无杠杆、全额成交账本。只依赖
`prajna-domain` 与 Serde，不读取行情、不生成信号、不决定可交易性、数量或费率。
这不是已接通的 Fast Event Engine；Run Spec、ResultLevel 和调仓由后续票据实现。

## 使用契约

```text
TradingAccount::new(currency, venue, initial_cash, VP allocations)
  submit_order(Order::new(id, VP, instrument, side, quantity, submitted_at))
  apply_fill(Fill::new(order, executed_at, raw_price, pricing, costs))
  value(explicit valuation prices) -> AccountValuation
  AccountValuation::validate() -> exact reconciliation
```

- 资本仅在构造时分配，允许零资本 VP；无 VP 时全为 Unallocated Capital。
  超额、负分配和重复 VP ID 均报错。VP ID 和 Order ID 在一个账户内唯一。
- Order 数量严格为正，方向为 Buy/Sell；一个 Order 只能全额成交一次。
  Fill 复制其已登记 Order，拥有唯一 VP；不同 VP 对同一 Instrument 的买卖不会净额化。
  Fill 不得早于提交时间。引擎负责 Session、执行状态、先卖后买与买腿顺序。
- 初始资金、每个 VP 现金、聚合账户现金、持仓均不得为负。拒绝的 Fill 不改变
  现金、持仓、成交列表或已成交标记；预先登记的 Order 保留，可重新尝试。
  现金削减与整手由引擎决定，账本不会自动借用其他 VP / Unallocated 的资金。
- 全部数值使用 Domain i128 / scale 18。乘法使用 half-even；加减经过范围检查；
  无 f64、容差检查或货币单位舍入。溢出返回 `LedgerError::Arithmetic`。
- `value` 为持仓要求显式正估值价；缺失、零价、负价均报错。缺 bar / 停牌的
  carried price 及其来源由引擎提供；估值价不能作为可成交证明。未持仓标的不需估值价。
  该 crate 不校验 Instrument 的报价币种，调用方必须先确认与账户记账币种一致；
  不支持 FX、乘数、衍生品结算或杠杆。
- 账本余额只读；快照字段可读写并可 `validate`，修改快照不回写账本。
  Order / Fill / VP 和估值快照可 Serialize；不提供绕过校验的 Deserialize。

## Fill 现金与费用

`FillCosts.commission` 是比例佣金，`minimum_commission_top_up` 是额外最低佣金补足额，
二者相加才是总佣金。税另列，所有成本分项必须非负；费率和 minimum 的计算由引擎负责。

```text
raw_notional   = round_half_even(raw_price * quantity, 18)
trade_notional = round_half_even(fill_price * quantity, 18)
total_fees     = commission + minimum_commission_top_up + tax + slippage
cash_delta     = (Buy: -raw_notional; Sell: +raw_notional) - total_fees
```

- `SeparateSlippage { slippage }`（vector_parity）：Fill 价为 raw open，成交额为
  raw_notional；slippage 由引擎按权重换手成本提供，单独扣现金。
- `EmbeddedSlippage { execution_price }`（lot）：成交价必须为不利方向（买价 ≥ raw，
  卖价 ≤ raw），滑点等于两个**已舍入成交额之差**。买单为 trade − raw，卖单为 raw − trade。
  该精确差额避免 scale-18 边界上重复乘法造成结算差异。
  滑点披露但不在含滑点成交额上重复扣款：现金变动等价于有向 trade_notional − 佣金 − 补足 − 税。

例：买 100 股，raw 10、成交价 10.1、比例佣金 1、补足 4、税 2：
raw_notional=1000、trade_notional=1010、slippage=10、total_fees=17、cash_delta=-1017。

## 两层精确估值

每个 VP 与账户分别逐标的做一次 scale-18 half-even 估值。
账户数量是各 VP 数量之和；账户市值由聚合数量独立乘价计算，
必须同时等于该标的的 VP 市值之和，否则返回 `LedgerError::Invariant`。
所有 VP 与账户使用同一标的的同一估值价。

```text
VP mark            = round_half_even(VP quantity * price, 18)
Account mark       = round_half_even(sum(VP quantity) * price, 18)
                   = sum(VP marks)  // checked, failure is an error
VP equity          = VP cash + sum(VP marks)
Account cash       = sum(VP cash) + Unallocated Capital
Account quantities = sum(VP quantities), per instrument
Account equity     = Account cash + sum(Account marks)
                   = sum(VP equity) + Unallocated Capital
```

half-even 不满足分配律：两个 VP 各持有 `1e-18` 股、估值价 `0.5`，
两个 VP 市值各为 0；聚合数量 `2e-18` 的账户市值却为 `1e-18`。
本 spec 要求两层恒等式精确成立，因此 `value` 拒绝该估值；不以容差放行，
不改变 VP 市值或账户乘价口径。后续 Run 必须把该错误作为恒等式失败报告；
若未来需要分摊舍入差额，须先作明确的 spec/ADR 决策。
`AccountValuation::validate` 检查 VP 与账户数量/价格/市值/权益及两层聚合恒等式，
偏差即报错。这一边界供后续 Engine 和独立 review 核查。

## 验证

`cargo test -p prajna-account --locked --offline`：手算两 VP 同 ETF、open/close、
买卖与费用、最低佣金补足、两种滑点口径、拒绝/回滚、精确误差注入、half-even 与溢出。
CI lightweight 增加本 crate 的测试、Clippy 与依赖图守卫。
仅证明合成账本契约；不证明真实市场、PIT、执行状态或总回报。
