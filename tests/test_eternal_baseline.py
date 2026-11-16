import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import date
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import eternal_baseline as baseline


class TotalReturnTests(unittest.TestCase):
    def test_freeze_rejects_non_iso_bar_dates(self):
        with self.assertRaisesRegex(ValueError, '日期无效'):
            baseline.ensure_ordered_bars('sh510000', [
                {'date': '2020-1-2', 'code': 'sh510000'},
            ])

    def test_tencent_bar_parser_preserves_unadjusted_fields_and_rejects_bad_dates(self):
        body = json.dumps({
            'data': {
                'sh510000': {
                    'day': [['2020-01-02', '10', '11', '11.5', '9.5', '1000']],
                },
            },
        }).encode()
        rows = baseline.parse_tencent_bars(body, 'sh510000', '2020-01-01', '2020-01-03')
        self.assertEqual(rows[0]['open'], '10')
        self.assertEqual(rows[0]['close'], '11')
        self.assertEqual(rows[0]['source'], 'tencent_response')
        with self.assertRaisesRegex(ValueError, '日期越界'):
            baseline.parse_tencent_bars(body, 'sh510000', '2020-01-03', '2020-01-04')

    def test_sina_split_dividend_factors_create_total_return_prices(self):
        bars = [
            {'date': '2020-01-02', 'code': 'sh510000', 'open': '10', 'close': '10'},
            {'date': '2020-01-03', 'code': 'sh510000', 'open': '9.9', 'close': '10.1'},
        ]
        factors = [
            {'date': '2020-01-02', 'split': '1', 'dividend': '0'},
            {'date': '2020-01-03', 'split': '1', 'dividend': '0.1'},
        ]
        prices, actions = baseline.total_return_prices(bars, factors)
        self.assertAlmostEqual(prices['2020-01-03'][0], 1)
        self.assertAlmostEqual(prices['2020-01-03'][1], 10.1 / 9.9)
        self.assertEqual(actions[0]['cash_per_old_share'], .1)

    def test_split_factor_adjusts_raw_opening_basis(self):
        bars = [
            {'date': '2020-01-02', 'code': 'sh510000', 'open': '10', 'close': '10'},
            {'date': '2020-01-03', 'code': 'sh510000', 'open': '5', 'close': '5.5'},
        ]
        factors = [
            {'date': '2020-01-02', 'split': '1', 'dividend': '0'},
            {'date': '2020-01-03', 'split': '2', 'dividend': '0'},
        ]
        prices, actions = baseline.total_return_prices(bars, factors)
        self.assertAlmostEqual(prices['2020-01-03'][0], 1)
        self.assertAlmostEqual(prices['2020-01-03'][1], 1.1)
        self.assertEqual(actions[0]['shares_per_old_share'], 2)

    def test_opening_limits_mark_exact_directional_boundary(self):
        bars = [
            {'date': '2020-01-02', 'close': '10'},
            {'date': '2020-01-03', 'open': '11', 'close': '11'},
            {'date': '2020-01-06', 'open': '9.9', 'close': '9.9'},
        ]
        flags = baseline.opening_limits(bars, [], baseline.Decimal('.10'))
        self.assertEqual(flags['2020-01-03'], (True, False))
        self.assertEqual(flags['2020-01-06'], (False, True))

    def test_factor_gap_and_duplicate_dates_fail_closed(self):
        bars = [
            {'date': '2020-01-02', 'code': 'sh510000', 'open': '10', 'close': '10'},
        ]
        with self.assertRaisesRegex(ValueError, '缺少有效因子'):
            baseline.total_return_prices(bars, [
                {'date': '2020-01-03', 'split': '1', 'dividend': '0'},
            ])
        with self.assertRaisesRegex(ValueError, '日期重复'):
            baseline.total_return_prices(bars, [
                {'date': '2020-01-01', 'split': '1', 'dividend': '0'},
                {'date': '2020-01-01', 'split': '1', 'dividend': '0'},
            ])

    def test_factor_response_requires_all_fund_rows_and_s_u_fields(self):
        body = b'var x={"total":1,"data":[{"d":"2020-01-02","f":"1","s":"1","u":"0"}]};'
        self.assertEqual(baseline.parse_sina_factors(body, 'sh510000'), [
            {'date': '2020-01-02', 'split': '1', 'dividend': '0'},
        ])
        incomplete = b'var x={"total":2,"data":[{"d":"2020-01-02","f":"1","s":"1","u":"0"}]};'
        with self.assertRaisesRegex(ValueError, '不完整'):
            baseline.parse_sina_factors(incomplete, 'sh510000')


class StitchTests(unittest.TestCase):
    def test_stitch_scales_after_first_overlap_and_uses_legacy_limits(self):
        prices = {
            'legacy': {
                '2020-01-02': (1.0, 2.0),
                '2020-01-03': (2.0, 4.0),
                '2020-01-06': (4.0, 8.0),
            },
            'current': {
                '2020-01-03': (5.0, 10.0),
                '2020-01-06': (6.0, 12.0),
                '2020-01-07': (8.0, 16.0),
            },
        }
        limits = {
            'legacy': {'2020-01-03': (True, False)},
            'current': {'2020-01-06': (False, True), '2020-01-07': (True, False)},
        }
        anchor, switch = baseline.splice_etf(prices, limits, 'legacy', 'current')
        self.assertEqual((anchor, switch), ('2020-01-03', '2020-01-06'))
        self.assertEqual(prices['current']['2020-01-02'], (1.0, 2.0))
        self.assertEqual(prices['current']['2020-01-03'], (2.0, 4.0))
        self.assertAlmostEqual(prices['current']['2020-01-06'][0], 2.4)
        self.assertAlmostEqual(prices['current']['2020-01-06'][1], 4.8)
        self.assertEqual(limits['current']['2020-01-03'], (True, False))
        self.assertEqual(limits['current']['2020-01-06'], (False, True))

    def test_missing_overlap_and_empty_post_overlap_fail(self):
        with self.assertRaisesRegex(ValueError, '缺少共同收盘日'):
            baseline.splice_etf(
                {'legacy': {'2020-01-02': (1, 1)}, 'current': {'2020-01-03': (1, 1)}},
                {'legacy': {}, 'current': {}}, 'legacy', 'current',
            )
        with self.assertRaisesRegex(ValueError, '没有可用交易日'):
            baseline.splice_etf(
                {'legacy': {'2020-01-02': (1, 1)}, 'current': {'2020-01-02': (1, 1)}},
                {'legacy': {}, 'current': {}}, 'legacy', 'current',
            )


class SimulationTests(unittest.TestCase):
    def setUp(self):
        self.days = [date(2020, 1, day) for day in (2, 3, 6, 7)]
        self.opens = [[1.0, 1.0], [1.0, 1.0], [1.2, 1.0], [1.15, 1.0]]
        self.closes = [[1.0, 1.0], [1.11, 1.0], [1.2, 1.0], [1.15, 1.0]]
        self.portfolio = {'sh510000': .5, 'sh520000': .5}
        self.unblocked = [[(False, False), (False, False)] for _ in self.days]

    def test_insufficient_common_sessions_raise_a_validation_error(self):
        for days in ([], self.days[:1]):
            with self.subTest(session_count=len(days)):
                with self.assertRaisesRegex(ValueError, '共同交易日少于两个'):
                    baseline.simulate(
                        days, self.opens[:len(days)], self.closes[:len(days)],
                        self.unblocked[:len(days)], .10, .0005, self.portfolio,
                    )

    def test_close_signal_executes_whole_portfolio_next_common_open(self):
        navs, signals, trades, stats, daily = baseline.simulate(
            self.days, self.opens, self.closes, self.unblocked,
            .10, .0005, self.portfolio,
        )
        self.assertEqual(len(signals), 1)
        self.assertEqual(signals[0]['signal_date'], self.days[1])
        self.assertEqual(len(trades), 1)
        self.assertEqual(trades[0]['signal_date'], self.days[1])
        self.assertEqual(trades[0]['intended_date'], self.days[2])
        self.assertEqual(trades[0]['trade_date'], self.days[2])
        self.assertEqual(stats['rebalances'], 1)
        self.assertEqual(len(navs), len(daily))
        self.assertGreater(daily[0]['fees'], 0)
        self.assertGreater(trades[0]['fees_in_initial_nav'], 0)
        for nav, row in zip(navs, daily):
            self.assertAlmostEqual(nav, row['cash'] + sum(row['holdings'].values()))
        self.assertGreater(trades[0]['net_trade_520000'], 0)
        self.assertLess(trades[0]['net_trade_510000'], 0)

    def test_directional_limit_defers_entire_rebalance_until_later_open(self):
        cases = (
            ('buy at upper limit', [(False, False), (True, False)]),
            ('sell at lower limit', [(False, True), (False, False)]),
        )
        for label, blocked_flags in cases:
            with self.subTest(label=label):
                limits = [list(row) for row in self.unblocked]
                limits[2] = blocked_flags
                _, signals, trades, stats, _ = baseline.simulate(
                    self.days, self.opens, self.closes, limits,
                    .10, .0005, self.portfolio,
                )
                self.assertEqual(len(signals), 1)
                self.assertEqual(stats['blocked_open_days'], 1)
                self.assertEqual(len(trades), 1)
                self.assertEqual(trades[0]['intended_date'], self.days[2])
                self.assertEqual(trades[0]['trade_date'], self.days[3])

    def test_opposite_direction_limits_do_not_block(self):
        limits = [list(row) for row in self.unblocked]
        limits[2] = [(True, False), (False, True)]
        _, _, trades, stats, _ = baseline.simulate(
            self.days, self.opens, self.closes, limits,
            .10, .0005, self.portfolio,
        )
        self.assertEqual(stats['blocked_open_days'], 0)
        self.assertEqual(trades[0]['trade_date'], self.days[2])

    def test_final_close_signal_is_recorded_without_future_execution_day(self):
        closes = [[1.0, 1.0], [1.0, 1.0], [1.11, 1.0]]
        _, signals, trades, stats, _ = baseline.simulate(
            self.days[:3], self.opens[:3], closes, self.unblocked[:3],
            .10, .0005, self.portfolio,
        )
        self.assertEqual(len(signals), 1)
        self.assertIsNone(signals[0]['intended_date'])
        self.assertEqual(trades, [])
        self.assertTrue(stats['pending_at_end'])

    def test_reference_gaps_are_explicit(self):
        dates = self.days[:2]
        stats = {
            'start': self.days[0],
            'end': self.days[1],
            'cagr': 0.0,
            'max_drawdown': 0.0,
            'rebalances': 0,
        }
        reference = {
            'history_etf': [[day.isoformat(), 1.0] for day in dates],
            'stats_etf': {
                'start': dates[0].isoformat(),
                'end': dates[1].isoformat(),
                'cagr': 0.0,
                'max_drawdown': 0.0,
                'rebalances': 0,
            },
        }
        result = baseline.compare_reference(reference, dates, [1.0, 1.0], stats, [
            ('2020-01-01', '2020-01-02'),
            ('2020-01-01', '2020-01-02'),
        ])
        self.assertEqual(result['overall_status'], 'match')
        self.assertTrue(all(
            status == 'unavailable_in_reference'
            for status in result['execution_detail_comparison'].values()
        ))


def make_replay_snapshot(snapshot, single_common_session=False):
    snapshot.mkdir()
    dates = ('2026-11-06', '2026-11-09', '2026-11-10', '2026-11-11')
    bars = {}
    factors = {}
    for code in baseline.HISTORY_CODES:
        asset_dates = dates
        if code in {'sh513100', 'sh513500'}:
            asset_dates = dates[:2]
        elif code in {'sh513300', 'sz159612'}:
            asset_dates = dates[1:]
        elif single_common_session and code in baseline.ETF_CODES:
            asset_dates = dates[2:3]
        bars[code] = [
            {
                'date': day, 'code': code, 'open': '1', 'close': '1',
                'high': '1', 'low': '1', 'volume': '100', 'source': 'synthetic',
            }
            for day in asset_dates
        ]
        factors[code] = [{'date': dates[0], 'split': '1', 'dividend': '0'}]
        baseline.write_csv(snapshot / f'bars/{code}.csv.gz', bars[code], baseline.RAW_BAR_FIELDS)
        baseline.write_csv(snapshot / f'factors/{code}.csv.gz', factors[code],
                           ('date', 'split', 'dividend'))

    prices, limits = {}, {}
    for code in baseline.HISTORY_CODES:
        prices[code], actions = baseline.total_return_prices(bars[code], factors[code])
        limits[code] = baseline.opening_limits(bars[code], actions, baseline.Decimal('.10'))
    anchors = [baseline.splice_etf(prices, limits, old, new)
               for old, new in baseline.LEGACY_PAIRS]
    common = set.intersection(*(set(prices[code]) for code in baseline.ETF_CODES))
    days = [date.fromisoformat(day) for day in sorted(common)]
    opens = [[prices[code][day.isoformat()][0] for code in baseline.ETF_CODES] for day in days]
    closes = [[prices[code][day.isoformat()][1] for code in baseline.ETF_CODES] for day in days]
    day_limits = [[limits[code].get(day.isoformat(), (False, False))
                   for code in baseline.ETF_CODES] for day in days]
    if len(days) < 2:
        navs = [1.0]
        stats = {
            'start': days[0],
            'end': days[-1],
            'cagr': 0.0,
            'max_drawdown': 0.0,
            'rebalances': 0,
        }
    else:
        navs, _, _, stats, _ = baseline.simulate(
            days, opens, closes, day_limits,
            baseline.THRESHOLD, baseline.FEE_RATE, baseline.WEIGHTS,
        )
    reference_stats = {
        'start': stats['start'].isoformat(),
        'end': stats['end'].isoformat(),
        'nasdaqAnchor': anchors[0][0],
        'nasdaqSwitch': anchors[0][1],
        'sp500Anchor': anchors[1][0],
        'sp500Switch': anchors[1][1],
        'cagr': stats['cagr'],
        'max_drawdown': stats['max_drawdown'],
        'rebalances': stats['rebalances'],
    }
    baseline.write_json(snapshot / 'reference.json', {
        'history_etf': [[day.isoformat(), nav] for day, nav in zip(days, navs)],
        'stats_etf': reference_stats,
    })
    source_dir = snapshot / 'source'
    source_dir.mkdir()
    shutil.copyfile(Path(baseline.__file__), source_dir / 'eternal_baseline.py')
    inventory = {}
    for path in sorted(item for item in snapshot.rglob('*') if item.is_file()):
        inventory[path.relative_to(snapshot).as_posix()] = {
            'sha256': baseline.sha256_file(path),
            'bytes': path.stat().st_size,
        }
    manifest = {
        'schema_version': 2,
        'files': inventory,
        'configuration': baseline.execution_configuration(),
        'source': {
            'captured_source_sha256': {},
            'replay_entrypoint': 'source/eternal_baseline.py',
            'legacy_source_role': 'archived for provenance; not invoked by the replay adapter',
        },
    }
    baseline.write_json(snapshot / 'manifest.json', baseline.seal_manifest(manifest))


def temporary_directory_outside_repository():
    candidates = (Path(tempfile.gettempdir()), Path('/tmp'))
    for candidate in candidates:
        root = candidate.resolve()
        if root != baseline.ROOT and baseline.ROOT not in root.parents:
            return tempfile.TemporaryDirectory(dir=root)
    raise RuntimeError('no temporary directory outside the repository is available')


class ReplayEntryTests(unittest.TestCase):
    def test_offline_cli_persists_result_and_checks_snapshot_integrity(self):
        with temporary_directory_outside_repository() as directory:
            root = Path(directory)
            snapshot = root / 'snapshot'
            output = root / 'replay.json'
            make_replay_snapshot(snapshot)
            script = Path(__file__).resolve().parents[1] / 'scripts/eternal_baseline.py'
            completed = subprocess.run(
                [sys.executable, str(script), 'replay', '--snapshot', str(snapshot),
                 '--output', str(output)],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn('comparison=match', completed.stdout)
            result = json.loads(output.read_text(encoding='utf-8'))
            run_manifest = json.loads(Path(str(output) + '.run.json').read_text(encoding='utf-8'))
            self.assertEqual(result['comparison']['overall_status'], 'match')
            self.assertEqual(len(result['daily']), 4)
            self.assertIsInstance(result['signals'], list)
            self.assertIsInstance(result['trades'], list)
            self.assertEqual(run_manifest['result_sha256'], baseline.sha256_file(output))
            self.assertTrue(run_manifest['replay_code_matches_snapshot'])
            self.assertTrue(all(
                abs(row['nav'] - row['cash'] - sum(row['holdings'].values())) < 1e-10
                for row in result['daily']
            ))
            (snapshot / 'bars/sh511260.csv.gz').write_bytes(b'corrupted')
            with self.assertRaisesRegex(ValueError, '摘要不匹配'):
                baseline.read_snapshot(snapshot)

    def test_manifest_binds_configuration_and_replay_rejects_config_drift(self):
        with temporary_directory_outside_repository() as directory:
            snapshot = Path(directory) / 'snapshot'
            make_replay_snapshot(snapshot)
            manifest_path = snapshot / 'manifest.json'
            manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
            manifest['configuration']['fee_rate_one_way'] = 999
            manifest_path.write_text(json.dumps(manifest), encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'input_version 不匹配'):
                baseline.read_snapshot(snapshot)

            baseline.seal_manifest(manifest)
            manifest_path.write_text(json.dumps(manifest), encoding='utf-8')
            output = Path(directory) / 'replay.json'
            script = Path(__file__).resolve().parents[1] / 'scripts/eternal_baseline.py'
            completed = subprocess.run(
                [sys.executable, str(script), 'replay', '--snapshot', str(snapshot),
                 '--output', str(output)],
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn('configuration does not match', completed.stderr)
            self.assertFalse(output.exists())

    def test_replay_rejects_a_runner_that_differs_from_frozen_source(self):
        with temporary_directory_outside_repository() as directory:
            snapshot = Path(directory) / 'snapshot'
            make_replay_snapshot(snapshot)
            changed_script = Path(directory) / 'changed_eternal_baseline.py'
            changed_script.write_text(
                Path(baseline.__file__).read_text(encoding='utf-8') + '\n# changed\n',
                encoding='utf-8',
            )
            manifest, _ = baseline.read_snapshot(snapshot)
            with self.assertRaisesRegex(ValueError, '回放源码与 snapshot 冻结源码不一致'):
                baseline.validate_replay_source(manifest, changed_script)
            completed = subprocess.run(
                [sys.executable, str(changed_script), 'replay', '--snapshot', str(snapshot),
                 '--output', str(Path(directory) / 'replay.json')],
                capture_output=True, text=True, check=False,
                env={
                    **os.environ,
                    'PRAJNA_ETERNAL_BASELINE_REPOSITORY_ROOT': str(baseline.ROOT),
                },
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn('回放源码与 snapshot 冻结源码不一致', completed.stderr)

    def test_replay_reports_insufficient_common_sessions_as_validation_error(self):
        with temporary_directory_outside_repository() as directory:
            snapshot = Path(directory) / 'snapshot'
            output = Path(directory) / 'replay.json'
            make_replay_snapshot(snapshot, single_common_session=True)
            script = Path(__file__).resolve().parents[1] / 'scripts/eternal_baseline.py'
            completed = subprocess.run(
                [sys.executable, str(script), 'replay', '--snapshot', str(snapshot),
                 '--output', str(output)],
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn('共同交易日少于两天', completed.stderr)
            self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
