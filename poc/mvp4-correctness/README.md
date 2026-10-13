# MVP-4 real Accurate correctness

**Conclusion:** PASS for the fixed synthetic #133 A15 / #141 subset on
macOS ARM64, Python 3.12.2, Rust 1.98.1, `nautilus_trader==2.0.0rc6`.
This is implementer execution evidence, not independent PR approval,
production/PIT/total-return evidence or a complete MVP-4 performance result.

## Boundary and independent references

The public `run_adapter` boundary drives a real, newly created Nautilus worker
per Run. Rust plans each Order from the preceding actual Fill and owns the
Trading Account / Virtual Portfolio ledger. No fake Backend, Python execution
rewrite, pre-generated Fast Fill replay or 64x252 workload is used as evidence.
The independent [MVP-3 oracle](../mvp3-golden/README.md) is recomputed via its CLI
and must match every committed expected value before each case runs. Its
Decision tapes are the input to this **execution** acceptance; this does not
independently retest production factor/strategy generation.

The suite directly compares Accurate against the oracle's submitted Orders,
every Fill quantity, raw/actual price, notional, fee basis, commission, minimum
top-up, tax, slippage, cash change and total cost. Both open-after-execution and
close cash, holdings, valuation prices/sources, equity and NAV are checked each
Session and VP. Quantity and table shapes are exact; monetary absolute error
is at most 1e-6 CNY, normalized NAV relative error at most 1e-9 (#112 F7).
Fast Event is a second comparison, exact at scale 18 for submitted Orders,
Fills, execution-state attempts, both account/VP marks and carried-price sources.
Both valuation points validate the two-layer exact accounting identities.

`prepare_fixture.py` only expands the existing compressed MA fixture using the
independent oracle's expansion, and annotates normalizer-required metadata.
`synthetic-etf-daily@3` reads its lot from `account.lot_size`, so preparation
copies the explicit common Instrument lot there (10 for the hand fee fixture,
1 for the other hand fixtures). Mixed/missing lots fail rather than silently
substituting 100. No prices, costs, availability or decisions are changed.
The report stores both original fixture SHA-256 and normalized DSV.

## Verified matrix

All cases use `lot`. Entries in the last column are native Fills, never
platform blocked attempts.

| Fixture | Strategy / rebalance | Sessions | Fills |
| --- | --- | ---: | ---: |
| hand-v1 | S1 full_target | 3 | 1 |
| hand-retry-v1 | S1 full_target | 4 | 2 |
| hand-rebalance-v1 | S2 full_target | 6 | 5 |
| hand-fee-only-v1 | S2 full_target | 5 | 2 |
| dataset-v1 | S1 / S2 / S3 full_target | 10 each | 3 / 3 / 0 |
| b2-ma20-60-v1 | S3 full_target | 130 | 2 |
| dataset-v1 | S1 entry_only, first-Session decision | 10 | 3 |
| dataset-v1 | S1 entry_only, explicit POC decision timing | 10 | 3 |
| dataset-v1 | S2 entry_only | 10 | 3 |
| b2-ma20-60-v1 | S3 entry_only, Universe `{A}` | 130 | 2 |
| hand-v1 | Two independent S1 VPs, full_target | 3 | 2 |

The three POC variants also directly compare against the committed
[POC-0 B2 preflight](../poc0-benchmark/results/b2-matched-decision-loads-reset-selected-2026-09-28.preflight.json):
every Fill and daily close cash, holdings, prices and equity. Existing POC
rc5 evidence/pins are not regenerated or modified. The 10-Session S3 result
correctly has no MA20/60 trade; the nonempty 130-Session fixture covers warmup,
entry and exit. The two-VP hand case has initial account cash 2500, capital
1000 per VP and 500 Unallocated: two same-open A Fills of 90 at 10.1, distinct
Orders/VPs, aggregate quantity 180, cash 627.46 and close equity 2787.46.
Nautilus balances/positions are compared only to the aggregate Trading Account.

## Difference classification and evidence

The archived [full report](nautilus-rc6-correctness.json.gz) retains every
non-equal compared field with actual/expected values, exactly one classification
and evidence. Equal fields are counted. Adapter constructions and native events
are separate arrays (ADR 0012), with per-Session account and per-Fill fee records.
Source-code hashes identify the executed harness, oracle and adapter/worker;
the report is generated from the implementer's pre-commit worktree.

- **Adapter defect:** no remaining differences. Wrong shape/quantity, exact
  Fast mismatch, out-of-F7 values or unexplained native account reconciliation
  fail the test. Missing diagnostics, wrong pin, native Reject/unmapped events
  and unsupported timestamp/fee mappings also fail; no skip/fallback path.
- **Backend native semantic difference:** each native Fill callback is at
  Session open +1 ms, not Domain requested open. Every such row includes the
  actual native Fill timestamp plus the registered `lockstep_barrier` offset
  and mapping evidence. The mapped Domain Fill remains at requested open.
  Native fees must equal CNY precision-2 half-even `Money`; any cash difference
  must reconcile from actual native Fill fees and currency-rounded notionals.
  These fixed cases have **no native fee, account cash or position difference**.
  This does not claim native Money supports scale 18.
- **Within tolerance:** float64 oracle monetary/NAV drift is recorded per
  leaf, not concealed by rounding. Quantities never receive monetary tolerance.

Platform gates and synthetic bid/ask/status construction are explicitly
reported as Adapter events, **not native rejects or native market data**.
They are not treated as mismatched native lifecycle tables. All actual native
events are retained; an unexpected event aborts acceptance for attribution.
There are zero unclassified differences.

## Reproduce and CI

Install [the separate rc6 requirements](../../backends/nautilus/requirements.txt)
in a dedicated Python 3.12 environment, leaving the POC rc5 environment intact:

```sh
python -m pip install -r backends/nautilus/requirements.txt
python -m unittest tests.test_mvp4_fixture_prepare tests.test_mvp3_fast_event_golden -v
PRAJNA_NAUTILUS_PYTHON=python \
PRAJNA_CORRECTNESS_REPORT=accurate-correctness.json \
cargo test -p prajna-experiment --features nautilus-integration \
  --test accurate_correctness --locked --offline
gzip -dc poc/mvp4-correctness/nautilus-rc6-correctness.json.gz \
  | jq '{conclusion,adapter_defects,unclassified_differences}'
```

The feature gates only the integration test target, not Engine behavior.
Ordinary lightweight CI never launches Nautilus. `Full`'s existing macOS
`nautilus-worker` job enables this target, requires the exact pin and uploads
the JSON report and test log as `accurate-correctness`; nonzero ignored/skipped
counts fail. The archived local run has 2 passing Rust tests (13 real Runs),
zero ignored tests. Linux runtime remains Unknown. Worker process-group cleanup
logs `Operation not permitted` on this host despite successful worker completion;
this existing transport diagnostic is disclosed, not suppressed or fixed here.
Conversion cost / throughput / RSS acceptance belongs to #142, not this report.
