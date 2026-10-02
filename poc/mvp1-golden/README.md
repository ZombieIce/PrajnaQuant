# MVP-1 independent factor golden

This is the independent Python-standard-library factor-value slice of M13. It
reads the hand-authored JSON fixture directly; it does not read D7 Parquet or
reuse Rust or `prajna-research` code.

Regenerate the committed 3×10 expected output from the repository root:

```sh
python3 poc/mvp1-golden/vector_golden.py \
  --fixture poc/poc0-benchmark/fixtures/dataset-v1.json \
  --out poc/mvp1-golden/expected/dataset-v1.factors.json
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
absolute tolerance (`1e-12`). This synthetic assumption is not evidence of
real-market point-in-time availability.

The M6/M7 definitions and M13 independent-golden requirement are in
[spec issue #53](https://github.com/ZombieIce/PrajnaQuant/issues/53). This
ticket covers the 3×10 factor values and statuses only; it does not implement
the larger M13 ranking or vector-result goldens.
