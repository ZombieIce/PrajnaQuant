# MVP-3 independent full_target golden

This Python-standard-library oracle implements the synthetic `full_target`
subset of [spec #112](https://github.com/ZombieIce/PrajnaQuant/issues/112)
F8-F10/F13.4 for [ticket #117](https://github.com/ZombieIce/PrajnaQuant/issues/117).
It imports neither Rust artifacts, the MVP-1 Python oracle, POC execution
code, nor Nautilus. It is a third-party reference, not a second production
quant core and not evidence that the platform Fast Event engine exists.

## Inputs and provenance

Hashes are SHA-256 of the exact committed source bytes, also included as
`fixture_sha256` in every expected result:

| Source | Shape and use | SHA-256 |
| --- | --- | --- |
| [dataset-v1.json](../poc0-benchmark/fixtures/dataset-v1.json) | Hand-authored 3 instruments x 10 Sessions; S1/S2/S3, UNKNOWN C, HALTED B, missing B bar, nonzero costs | `9b9009b1586a6f271cd301042f97f2adf84146c870d72802682799d60352ed85` |
| [b2-ma20-60-v1.json](../poc0-benchmark/fixtures/b2-ma20-60-v1.json) | Compressed 3 x 130; S3 | `953218548395293b07148554c57ddc64440f9feb2e7cf8fc37de4dec0c3ee49d` |
| [hand-v1.json](fixtures/hand-v1.json) | New hand-authored 1 x 3; S1 fee/accounting example | `87acfaa5f180dc388b0643d9618e49332f44e1a6e35e4b49af588d7c07f94aa6` |
| [hand-retry-v1.json](fixtures/hand-retry-v1.json) | Hand-authored 2 x 4; S1 costed retry of a HALTED leg, budget cut to cash | `18124241deea15d6e99feac4cbd544c2b612fa70e71ff407128888c39867065a` |
| [hand-rebalance-v1.json](fixtures/hand-rebalance-v1.json) | Hand-authored 2 x 6; S2 `top_n=2` continuing holdings trimmed/topped up with costs | `4ea9162adbb0f8d0bbaa3eeda8f5c19d3fd79a032137884ede7195f86e13bcec` |

The compressed MA fixture expands into weekdays from `start_date`, with
60 flat, 20 high and 50 low closes for A; B/C remain flat. Opens are 100.
All execution statuses are explicitly synthetic TRADABLE, available at
08:50 +08:00; opens are at 09:30, closes at 15:00. Expansion is independent
Python, not imported from the POC harness. A is weighted at 1/3, unlike
the earlier POC S3 single-instrument Universe. No old `entry_only` golden
is overwritten or claimed to match `full_target`.

Instrument IDs are `{symbol}.SYNTH` and sort lexically. `lot_size` is read
from each instrument, falling back to the legacy source fixture's explicit
account lot size; the resolved per-instrument map is saved in parameters.
Source files are synthetic Static Universes, not historical PIT or
dividend-total-return datasets.

## Recompute every expected value

From the repository root, using Python 3.12 (the lightweight CI version):

```sh
for sizing in lot vector_parity; do
  for strategy in s1 s2 s3; do
    python3 poc/mvp3-golden/fast_event_golden.py \
      --fixture poc/poc0-benchmark/fixtures/dataset-v1.json \
      --strategy "$strategy" --sizing "$sizing" \
      --out "poc/mvp3-golden/expected/dataset-v1.$strategy.$sizing.json"
  done
  python3 poc/mvp3-golden/fast_event_golden.py \
    --fixture poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json \
    --strategy s3 --sizing "$sizing" \
    --out "poc/mvp3-golden/expected/b2-ma20-60-v1.s3.$sizing.json"
  python3 poc/mvp3-golden/fast_event_golden.py \
    --fixture poc/mvp3-golden/fixtures/hand-v1.json \
    --strategy s1 --sizing "$sizing" \
    --out "poc/mvp3-golden/expected/hand-v1.s1.$sizing.json"
  python3 poc/mvp3-golden/fast_event_golden.py \
    --fixture poc/mvp3-golden/fixtures/hand-retry-v1.json \
    --strategy s1 --sizing "$sizing" \
    --out "poc/mvp3-golden/expected/hand-retry-v1.s1.$sizing.json"
  python3 poc/mvp3-golden/fast_event_golden.py \
    --fixture poc/mvp3-golden/fixtures/hand-rebalance-v1.json \
    --strategy s2 --sizing "$sizing" \
    --out "poc/mvp3-golden/expected/hand-rebalance-v1.s2.$sizing.json"
done
python3 -m unittest tests.test_mvp3_fast_event_golden -v
```

The fourteen expected JSON files contain every open-after-execution and close
cash/quantity/valuation/equity/NAV row, decisions, execution attempts,
orders (including blocked/unaffordable attempts), Fills and fee components.
All rows and numeric values are compared with CLI recomputation in CI,
not merely final NAV. Tests use temporary paths and do not require a
specific working directory or locally built binaries.

## Time, decisions and execution

Event sequence: mark at raw open -> attempt previous pending decision ->
sell before buy (IDs ascending within each direction) -> record post-fill
open -> mark and record close -> form a close decision -> replace pending.
A last-Session decision has no execution Session and remains pending.

- **S1**: first Session close decides equal Universe weights, default
  `unfilled_entry=retry`; no subsequent decisions.
- **S2**: momentum uses closes at T and T-N; volatility uses N Session
  returns and sample standard deviation; optional trend requires current
  close at or above the N-Session mean. Score is weighted short momentum plus
  long momentum minus volatility. Even zero-weight dependencies require
  valid windows. Windows do not bridge absent bars. Every required close
  must have known availability no later than that Session's 15:00 close.
  Start at the first valid score; repeat every `rebalance_every` Sessions,
  rank descending then ID ascending, equal-weight the selected `top_n`.
  Empty rankings on a scheduled decision produce an empty target.
  Default `unfilled_entry=skip` preserves the existing S2 convention.
- **S3**: each instrument independently enters on previous MA20/60 gap
  <= 0 and current gap > 0; exits on previous gap >= 0 and current gap < 0.
  Held targets have fixed weight 1/N, never renormalized. **Owner-confirmed
  convention**: only target-set changes emit a decision. Missing, unknown
  or late-available windows clear the preceding gap, but do not liquidate
  existing strategy state. The first subsequent valid gap establishes a
  baseline, not a crossing. The 3x10 S3 results prove warmup/no-trade behavior;
  3x130 proves entry at index 60, next-open buy at 61, exit at 85 and sell at 86.

Execution requires a raw open plus TRADABLE/is_tradable=true status whose
known availability is at or before that open. No status record means
UNKNOWN. If **any held instrument** is unavailable, defer the entire
attempt, matching the current conservative `strategy::execute` convention.
Otherwise unavailable buy targets are skipped without renormalization.
`--unfilled-entry retry` keeps only blocked legs with their original weights;
filled legs are not rebalanced during retry. Cash-based lot reductions do
not create a retry. A new decision replaces even deferred/retry pending.

**Owner-confirmed valuation convention:** valid raw bars mark even
HALTED/UNKNOWN instruments; status gates trades, not an existing valid
price. Missing open/close prices carry their respective most recent raw
price, with source Session and field saved on each held position. Open
never carries a previous close, preserving Vector open-to-open comparison.
An unpriced held position is an error, never a zero-price asset.

## Sizing, fees and tolerances

`lot`: desired quantities use pre-open raw-marked equity and round down to
Instrument lots. Fill price is raw open times (1 +/- directional slippage).
Commission is max(proportional Fill notional commission, minimum commission);
tax uses that same Fill notional. Buys reduce one lot at a time until
notional plus commission and tax fits cash. Every final Order fills in
full; quantity reduction occurs before Order submission.

`vector_parity`: drift weights are q * raw open / E0. Fee bases are
E0 * abs(target - drift weight); commission, directional slippage and tax
use these bases. E1 = E0 - sum(costs); full_target quantities are
target * E1 / raw open. Fills use raw open. A cost-induced reduction can
sell a continuing holding with zero weight-delta fee basis; therefore
`fee_side` records the basis direction separately from the actual Fill side.
Minimum commission and lots do not apply. A leg whose absolute weight delta
is <= 1e-12 is a no-trade: it has no Order, Fill or fee basis, so float
residue never becomes a dust Fill. Retry leaves filled quantities unchanged
and sizes only missing legs per ADR 0019 (#116): budget is the original
target weight times pre-fee open equity, fees are paid from remaining cash,
and budgets are cut in ascending `instrument_id` order to
`cash_weight / (1 + buy_rate)` where `buy_rate` is commission + buy slippage
+ buy tax. Filled legs never change; a cash cut adds no new pending.

Each Fill saves notional, fee basis, commission, minimum-commission top-up
(already included in commission), tax, slippage, `cash_fees`, `total_cost`,
and signed `cash_change`. Lot slippage is embedded in Fill notional, so
`cash_fees` excludes it; parity slippage is a separate cash fee.
For both modes, cash_change = signed notional - cash_fees, and economic
total_cost = commission + tax + slippage. Do not double-count minimum
top-up or embedded lot slippage.

This ticket uses one VP allocated all account capital, unallocated=0:
Session cash/holdings/equity are both that VP's and the account's ledger.
Multi-VP aggregation is not implemented or claimed here. Replay checks
cash from all Fills and both valuation identities. Python float64 only
proves conservation within tolerance; Rust scale-18 accounting must still
prove exact conservation independently.

F7 comparison requires cash/equity/fees absolute error <= 1e-6 CNY and
normalized Vector NAV relative error <= 1e-9. Order/Fill identities,
directions and lot quantities must match exactly; fractional quantities
are the F8 analytical values recorded as float64, not an assertion that
Python reproduces Rust scale-18 rounding. No extra fractional-quantity
tolerance is authorized by this oracle. The same Python CLI replay checks
every saved value exactly. Existing Vector S2 fixtures are independently
checked for decisions/attempts and open NAV alignment.

## Line-by-line hand example (1 instrument x 3 Sessions)

See [hand-v1.json](fixtures/hand-v1.json) and the two commented literal
tests in [test_mvp3_fast_event_golden.py](../../tests/test_mvp3_fast_event_golden.py).
Initial cash is 1000, raw opens 10, closes 10/12/12, lot size 10,
commission 1%, minimum 2, buy slip 1%, buy tax 2%.

| Event | Lot cash / q / equity | Parity cash / q / equity | Explanation |
| --- | --- | --- | --- |
| D1 open | 1000 / 0 / 1000 | 1000 / 0 / 1000 | No prior decision |
| D1 close | 1000 / 0 / 1000 | 1000 / 0 / 1000 | S1 decides target=1; no same-close Fill |
| D2 open | 63.73 / 90 / 963.73 | 0 / 96 / 960 | Lot: initial 100 unaffordable; 90 at 10.1 -> notional 909, commission 9.09, tax 18.18, embedded slip 9. Parity: E0 basis=1000 -> commission 10, slip 10, tax 20; E1=960, q=96 at raw 10 |
| D2 close | 63.73 / 90 / 1143.73 | 0 / 96 / 1152 | Mark at 12, without another trade |
| D3 open | 63.73 / 90 / 963.73 | 0 / 96 / 960 | Raw open is again 10; S1 does not rebalance |
| D3 close | 63.73 / 90 / 1143.73 | 0 / 96 / 1152 | Mark at 12; no new fees |

L1 does not model T+1, price limits, currency-unit rounding, interest,
funding or corporate actions. These results prove synthetic semantics only.
