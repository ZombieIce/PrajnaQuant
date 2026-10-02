# MVP-1 independent factor golden

This is the independent Python-standard-library factor and Vector-result
golden for the 3×10 fixture in M13. It reads the hand-authored JSON fixture
directly; it does not read D7 Parquet or reuse Rust or `prajna-research` code.

Regenerate the committed 3×10 factors and Vector result from the repository
root:

```sh
python3 poc/mvp1-golden/vector_golden.py \
  --fixture poc/poc0-benchmark/fixtures/dataset-v1.json \
  --out poc/mvp1-golden/expected/dataset-v1.json
```

Run the focused tests:

```sh
python3 -m unittest tests.test_mvp1_vector_golden -v
```

An optional `--trend N` emits `trend_filter(N)` and enables it as a
`rotation_score` dependency. Without it, the fixture's `trend_filter: false`
means the score has no trend dependency. The fixture's strategy supplies the
short and long momentum windows, volatility window, and all three score
weights.

Output factor keys are `momentum(N)`, `volatility(N)`, optional
`trend_filter(N)`, and `rotation_score`. Each factor has one row per instrument
and calendar session, sorted by instrument ID and session date. Windows count
the fixture's Venue Sessions: momentum needs the close at `t` and `t−N`,
volatility uses `N` consecutive daily returns and sample standard deviation
(denominator `N−1`), and trend compares the current close with the trailing
`N`-session mean. A missing bar is never skipped or bridged.

Statuses follow spec M6: `insufficient_window`, `missing_input`,
`unknown_availability`, `ok`, and `filtered`. Composite dependency errors
propagate in the order `insufficient_window > missing_input >
unknown_availability`; an otherwise valid score is `filtered` when its
enabled trend factor is zero. Non-`ok` values are null. A filtered score keeps
the known availability timestamp of its inputs.

Availability follows spec M7. For this synthetic fixture only, each bar is
assumed available at its session close, constructed from the session date and
`bar_defaults.close_available_at`, then converted to an RFC 3339 UTC string.
The result records the SHA-256 of the exact fixture bytes and the factor
absolute tolerance (`1e-12`) and NAV absolute tolerance (`1e-10`). This
synthetic assumption is not evidence of real-market point-in-time
availability.

## Vector result

The first decision is the first Venue Session with at least one `ok`
`rotation_score`; subsequent decisions follow `rebalance_every` Venue
Sessions. Scores rank descending with `instrument_id` as the ascending
tie-breaker, and the first `top_n` names receive equal target weights. Sparse
weight maps omit zero-weight instruments; omitted instruments are cash or
zero-weight holdings.

A decision made after a session close is attempted at the next session open.
On a later decision, any still-pending target is replaced. An execution is
deferred when any currently held instrument is not executable; otherwise
unexecutable buy legs are skipped without redistributing their target weight.
An instrument is executable only when it has an open bar, is marked tradable,
and its execution-status availability is no later than the session open.
Missing execution-status records are not executable.

Session returns are measured from the previous Venue Session open to the
current open using the previous session's post-execution weights. Missing
open prices carry the last known valuation forward and are listed in
`valuation_carried`; their return is recognized when a later open is observed.
Target weights on each session row reflect the latest decision made by that
session's close, while `weights_after_execution` reflect the open execution.
NAV starts at `1.0` on the first session open; session transaction costs are
applied after the open-to-open gross return. Cost is proportional:
`buys * (commission_rate + buy_slippage_bps / 1e4 + buy_tax_rate) +
sells * (commission_rate + sell_slippage_bps / 1e4 + sell_tax_rate)`.
Minimum commission and lots are not modeled.

M6/M7 and the independent-golden requirement are specified in
[issue #53](https://github.com/ZombieIce/PrajnaQuant/issues/53); the Vector
decision and execution rules are M11/M12. This fixture has constant opens, so
all gross returns are zero and NAV changes only through transaction costs.
It is a hand-checkable boundary fixture, not a point-in-time proof of a
real-market universe or availability.

## 64×252 v2 scale golden

Both outputs use
`poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json`, whose SHA-256 is
`c805d3ef0873171d89b4380ef593c5a032d9c302783ce620b11eb09a85d5c36e`.
The fixture configures momentum windows 20/60, volatility window 20, score
weights 1/1/1, `top_n=5`, and `rebalance_every=5`.

Regenerate the outputs from the repository root:

```sh
python3 poc/mvp1-golden/vector_golden.py \
  --fixture poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json \
  --out poc/mvp1-golden/expected/b2-s2-scale-64x252-v2.json

python3 poc/mvp1-golden/vector_golden.py \
  --fixture poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json \
  --trend 20 \
  --out poc/mvp1-golden/expected/b2-s2-scale-64x252-v2.trend20.json
```

The scale fixture includes a missing `ETF001` bar on 2025-07-01 and a HALTED
status for `ETF002` on 2025-07-02. The preceding session executes the pending
target, so no execution record is expected on the HALTED session itself. The
golden tests verify these statuses, complete factor/session coverage, non-zero
gross returns, the trend-filtered ranking difference, and byte-for-byte
reproducibility. They also assert that `valuation_carried` on 2025-07-01
contains `ETF001.SYNTH`.

## 64×252 HALTED execution golden

The v2 scale golden never reaches a HALTED execution: ETF002 is neither held
nor a pending target on 2025-07-02. This separate fixture keeps the v2 bytes
and SHA-256 unchanged and adds two HALTED execution-status records that hit a
pending rebalance. It is derived by `s2_halted_dataset` in
`poc/poc0-benchmark/generate-b2-stress-fixtures.py`
(`dataset_version` `poc0.b3.s2-scale-64x252-halted.v1`; the three earlier
fixtures regenerate byte-identically). Its SHA-256 is
`14f9b485c97bda657682eed5a4d5b2179a19123616b6c2d0a94f1d9697e23de3`.
Only the plain output (no `--trend`) is committed; the trend-filtered ranking
is already covered by the v2 golden.

```sh
python3 poc/poc0-benchmark/generate-b2-stress-fixtures.py

python3 poc/mvp1-golden/vector_golden.py \
  --fixture poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-halted-v1.json \
  --out poc/mvp1-golden/expected/b2-s2-scale-64x252-halted-v1.json
```

Hand check, with the same 20/60 momentum, 20 volatility, `top_n=5`,
`rebalance_every=5` and cost parameters as v2 (cost rate
`0.001 + 10 / 1e4 = 0.002` per unit turnover):

- **2025-07-08, `blocked`.** The 2025-07-07 decision drops held ETF029 and
  targets ETF012/016/025/033/054. ETF029 is HALTED at the 07-08 open, so the
  execution is `deferred` with `blocked: ["ETF029.SYNTH"]`: no trade, turnover
  0, cost 0, and the pending target stays. The next decision (07-14) comes
  after 07-09, so the target is not replaced.
- **2025-07-09, deferred execution lands.** All names are tradable, so the
  same target executes (`executed`, no blocked or skipped buys) with five
  weights of 0.2.
- **2025-07-15, `skipped_buys`.** The 07-14 decision targets
  ETF003/007/038/041/051. ETF041 is HALTED at the open and not held, so the
  buy is skipped and its 0.2 stays in cash; the execution is `executed` with
  `skipped_buys: ["ETF041.SYNTH"]`. The four remaining targets get 0.2 each
  and share no name with the prior book, so turnover is 0.8 + 1.0 = 1.8 and
  cost is 1.8 × 0.002 = 0.0036.
- **2025-07-01.** ETF001's bar is missing, so its valuation is carried and
  `valuation_carried` contains `ETF001.SYNTH`.

`test_scale_halted_golden_blocks_sells_and_skips_buys` asserts the above and
the byte-for-byte output. It also reruns the fixture with
`execution_status_overrides` cleared (HALTED ignored): 07-08 then executes
without a block, and 07-15 has no skipped buy and holds ETF041, so the
assertions fail if HALTED is ignored.

The fixture is synthetic: it makes no point-in-time claim and is not
validation against real-market data.
