"""Synthetic PIT boundaries through the real Python API and CLI."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import prajna
from test_experiment import CLI, TARGET

PREPARE = TARGET / 'debug/examples/pit_fixture'


def rotation(view, params, previous):
    rows = [r for r in view.factors['momentum'].to_pylist()
            if str(r['session_date']) == view.session_date and r['value'] is not None
            and r['instrument_id'] in view.eligible
            and r['instrument_id'] not in view.risk_excluded]
    rows.sort(key=lambda r: (-r['value'], r['instrument_id']))
    selected = rows[:params['top_k']]
    return prajna.Decision(
        {r['instrument_id']: 1 / len(selected) for r in selected},
        [(r['instrument_id'], r['value'], i + 1) for i, r in enumerate(rows)],
    )


def flat_rotation(view, params, previous):
    # All synthetic close-to-close momentum scores are zero.
    members = sorted(set(view.eligible) - set(view.risk_excluded))
    selected = members[:params['top_k']]
    return prajna.Decision(
        {instrument: 1 / len(selected) for instrument in selected},
        [(instrument, 0.0, i + 1) for i, instrument in enumerate(members)],
    )


FIXED_UNIVERSE = {
    'version': 'synthetic-fixed-etfs@1',
    'codes': ['159002', '159003'],
    'minimum_code_count': 2,
}


class PitTests(unittest.TestCase):
    def setUp(self):
        if not PREPARE.is_file():
            raise RuntimeError('Build cargo example pit_fixture first')
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.lake = self.base / 'lake'
        self.repo = self.base / 'repository'
        self.repo.mkdir()
        self.git_env = os.environ.copy()
        for key in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR'):
            self.git_env.pop(key, None)
        self.git_env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
        subprocess.run(['git', 'init', '--quiet'], cwd=self.repo, env=self.git_env, check=True)
        subprocess.run(['git', '-c', 'user.name=PIT Test', '-c', 'user.email=pit@example.invalid',
                        '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '--quiet',
                        '-m', 'fixture'], cwd=self.repo, env=self.git_env, check=True)
        self.data = json.loads(subprocess.check_output([PREPARE, self.lake], text=True))
        self.ids = self.data.pop('ids')
        self.params = self.data['parameter_space']['list'][0]
        self.strategy = prajna.Strategy('pit_rotation', '1', bar_columns=(), factors=(
            prajna.FactorDependency('momentum', 'etf_tri_momentum', {'lookback': 1}),
        ))

    def experiment(self, engine='vector', python=False, pit=None):
        return prajna.Experiment(
            dataset_version=prajna.DatasetVersion(self.data['dsv']),
            universe=prajna.Universe(self.data['universe']), venue=prajna.Venue('SZSE'),
            strategy=(prajna.PythonStrategy('pit_rotation', '1') if python
                      else prajna.RustStrategy.S2_PIT_ROTATION),
            engine=(prajna.Engine(prajna.EngineKind.VECTOR, initial_capital='10000')
                    if engine == 'vector' else prajna.Engine(prajna.EngineKind.FAST_EVENT,
                    settings=prajna.FastEventSettings(currency='CNY', initial_cash='10000',
                    sizing='lot', rebalance='full_target', minimum_commission='5',
                    portfolios=[prajna.Capital(1, '10000')], lot_size_source=prajna.LotSizeSource()))),
            costs=prajna.Costs(0.0003, 2, 2), parameter_space=prajna.List([self.params]),
            pit=self.data['pit'] if pit is None else pit,
        )

    def generate(self, decide, pit=None, params=None, strategy=None):
        return prajna.generate_decisions(
            lake_root=str(self.lake), dsv=self.data['dsv'], venue='SZSE', members=[],
            strategy=strategy or self.strategy, decide=decide, params=params or self.params,
            pit=self.data['pit'] if pit is None else pit,
        )

    def view_strategy(self):
        # A factor-free declaration on every Session exposes one View per
        # Session, so plan inputs can be compared without a cadence offset.
        return prajna.Strategy('pit_rotation', '1', bar_columns=(), rebalance_every=1)

    def fixed_pit(self):
        pit = copy.deepcopy(self.data['pit'])
        pit['fixed_universe'] = copy.deepcopy(FIXED_UNIVERSE)
        return pit

    def generate_views(self, pit, strategy=None):
        """Collect the public As-of Views and flat Decisions for one PIT config."""
        seen, decisions = [], []
        def decide(view, params, previous):
            seen.append(view)
            decision = flat_rotation(view, params, previous)
            decisions.append(decision)
            return decision
        returned = self.generate(decide, pit=pit, strategy=strategy or self.view_strategy())
        self.assertEqual(len(returned), len(decisions))
        return seen, decisions

    def run_cli(self, pit, level='full'):
        experiment = self.experiment(pit=pit)
        definition = self.base / 'pit-definition.json'
        definition.write_text(experiment.to_json())
        output = json.loads(subprocess.check_output([
            CLI, 'run', definition, '--lake', self.lake, '--level', level, '--threads', '1',
        ], cwd=self.repo, env=self.git_env, text=True))
        run = json.loads((self.execution_path(output) / 'execution.json').read_text())['runs'][0]
        return output, run

    def load_fixture(self, name, *flags):
        self.lake = self.base / name
        self.data = json.loads(subprocess.check_output(
            [str(PREPARE), str(self.lake), *flags], text=True))
        self.ids = self.data.pop('ids')

    def run_cli_failure(self, pit):
        experiment = self.experiment(pit=pit)
        definition = self.base / 'pit-definition.json'
        definition.write_text(experiment.to_json())
        return subprocess.run([
            CLI, 'run', definition, '--lake', self.lake, '--level', 'summary',
        ], cwd=self.repo, env=self.git_env, text=True, capture_output=True)

    def execution_path(self, cli):
        def identity_hex(identity):
            return identity.split(':sha256:')[1]
        return (self.lake / 'experiments' / identity_hex(cli['exp'])
                / 'executions' / identity_hex(cli['exe']))

    def assert_plan_matches_views(self, run_spec, views):
        inputs = {entry['eligible']['universe']['run_spec']['decision_session']: entry
                  for entry in run_spec['session_inputs']}
        self.assertEqual(sorted(inputs), [f'2023-01-{day:02}' for day in range(1, 11)])
        self.assertEqual([view.session_date for view in views], sorted(inputs))
        for view in views:
            entry = inputs[view.session_date]
            self.assertEqual(view.universe, entry['eligible']['universe']['members'])
            self.assertEqual(view.eligible, entry['eligible']['members'])
            self.assertEqual(view.risk_excluded, entry['risk_excluded'])

    def assert_decisions_match_cli(self, cli, run, views, decisions):
        rows = prajna.read_run(str(self.lake), cli['exp'], cli['exe'], run['run_id'],
                               table='decisions', result_level='full').to_pylist()
        cli_dates = {row['decision_session'] for row in rows}
        actual = sorted((views[index].session_date, instrument, rank,
                         decision.targets.get(instrument), score)
                        for index, decision in enumerate(decisions)
                        if views[index].session_date in cli_dates
                        for instrument, score, rank in decision.ranked)
        expected = sorted((row['decision_session'], row['instrument_id'], row['rank'],
                           row['target_weight'], row['score']) for row in rows)
        self.assertEqual(actual, expected)

    def test_pit_preserves_declared_factor_warmup_window_and_cadence(self):
        strategy = prajna.Strategy(
            'pit_schedule', '1', bar_columns=(), lookback_sessions=2, rebalance_every=3,
            factors=(
                prajna.FactorDependency('momentum', 'etf_tri_momentum', {'lookback': 1}),
                prajna.FactorDependency('slow', 'etf_tri_momentum', {'lookback': 2}),
            ),
        )
        seen = []
        def inspect(view, params, previous):
            seen.append((view.session_date, view.sessions.num_rows,
                         None if previous is None else previous.session_date))
            return prajna.Decision({})
        decisions = self.generate(inspect, strategy=strategy)
        self.assertEqual(seen, [
            ('2023-01-03', 2, None),
            ('2023-01-06', 2, '2023-01-03'),
            ('2023-01-09', 2, '2023-01-06'),
        ])
        self.assertEqual([d.session_date for d in decisions], [day for day, _, _ in seen])

    def test_removed_members_cannot_complete_factor_warmup(self):
        strategy = prajna.Strategy('pit_warmup', '1', bar_columns=(), factors=(
            prajna.FactorDependency('momentum', 'etf_tri_momentum', {'lookback': 7}),
        ))
        seen = []
        def inspect(view, params, previous):
            seen.append(view.session_date)
            return prajna.Decision({})
        self.assertEqual(self.generate(inspect, strategy=strategy), [])
        self.assertEqual(seen, [])

    def test_session_members_eligibility_and_safe_buffers(self):
        seen = []
        def inspect(view, params, previous):
            seen.append((view.session_date, view.universe, view.eligible))
            for table in [view.bars, view.execution_status, *view.factors.values()]:
                for row in table.to_pylist():
                    self.assertIn(row['instrument_id'], view.universe)
                    self.assertLessEqual(str(row['session_date']), view.session_date)
                    if row['available_at'] is not None:
                        self.assertLessEqual(int(row['available_at'].timestamp()) * 1_000_000_000, view.ts_close)
                # Compact StringArray payload contains no future lifecycle ID.
                if view.session_date < '2023-01-04':
                    array = table['instrument_id'].combine_chunks()
                    data = array.buffers()[-1]
                    if data is not None:
                        self.assertNotIn(self.ids[2].encode(), data.to_pybytes())
            return rotation(view, params, previous)
        decisions = self.generate(inspect)
        self.assertEqual([d.session_date for d in decisions],
                         [f'2023-01-{day:02}' for day in range(2, 11)])
        self.assertNotIn(self.ids[2], seen[0][1])
        self.assertEqual(seen[0][2], self.ids[:2])
        self.assertIn(self.ids[2], seen[2][1])
        self.assertNotIn(self.ids[2], seen[2][2])
        self.assertNotIn(self.ids[0], seen[3][1])
        self.assertEqual(seen[5][1], [self.ids[2]])

    def test_invalid_buys_fail_and_explicit_exit_remains_valid(self):
        cases = [('future listing', self.ids[2], '2023-01-02'),
                 ('not eligible', self.ids[2], '2023-01-04'),
                 ('removed member', self.ids[0], '2023-01-05')]
        for name, instrument, day in cases:
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'Eligible Set'):
                self.generate(lambda v, p, prev: prajna.Decision({instrument: 1.0})
                              if v.session_date == day else rotation(v, p, prev))
        self.generate(lambda v, p, prev: prajna.Decision({self.ids[0]: 0.0})
                      if v.session_date >= '2023-01-05' else rotation(v, p, prev))

    def test_terminated_lifecycle_cannot_be_bought_but_can_request_exit(self):
        self.lake = self.base / 'terminated-lake'
        self.data = json.loads(subprocess.check_output([PREPARE, self.lake, '--terminated'], text=True))
        self.ids = self.data.pop('ids')
        self.data['pit']['risk_notices'] = []
        seen = []
        def inspect(view, params, previous):
            seen.append((view.session_date, view.universe))
            return prajna.Decision({self.ids[0]: 0.0})
        self.generate(inspect)
        self.assertIn(self.ids[0], dict(seen)['2023-01-07'])
        self.assertNotIn(self.ids[0], dict(seen)['2023-01-09'])
        with self.assertRaisesRegex(ValueError, 'Eligible Set'):
            self.generate(lambda v,p,prev: prajna.Decision({self.ids[0]: 1.0})
                          if v.session_date == '2023-01-09' else prajna.Decision({}))

    def test_future_status_does_not_enter_view_or_buy_set(self):
        pit = copy.deepcopy(self.data['pit'])
        for cell in pit['market']['openings']:
            if cell['instrument_id'] == self.ids[0] and cell['session_date'] == '2023-01-03':
                cell['market_available_at'] = '2023-01-04T15:00:00+08:00'
        def inspect(view, params, previous):
            if view.session_date == '2023-01-03':
                self.assertNotIn(self.ids[0], view.eligible)
                self.assertFalse(any(r['instrument_id'] == self.ids[0]
                    and str(r['session_date']) == view.session_date
                    for r in view.execution_status.to_pylist()))
            return rotation(view, params, previous)
        self.generate(inspect, pit=pit)
        with self.assertRaisesRegex(ValueError, 'Eligible Set'):
            self.generate(lambda v,p,prev: prajna.Decision({self.ids[0]: 1.0})
                          if v.session_date == '2023-01-03' else rotation(v,p,prev), pit=pit)

    def test_typed_pit_matches_cli_identity_and_every_saved_hash(self):
        for engine in ('vector', 'fast_event'):
            with self.subTest(engine=engine):
                experiment = self.experiment(engine)
                definition = self.base / f'{engine}.json'
                definition.write_text(experiment.to_json())
                cli = json.loads(subprocess.check_output([
                    CLI, 'run', definition, '--lake', self.lake, '--level', 'full', '--threads', '1',
                ], cwd=self.repo, env=self.git_env, text=True))
                result = experiment.run(self.lake, repository=self.repo, level=prajna.ResultLevel.FULL, threads=2)
                self.assertEqual(result.action, 'replayed')
                self.assertEqual(experiment.describe(self.lake).experiment_id, cli['exp'])
                self.assertEqual(result.manifest['execution']['id'], cli['exe'])
                self.assertEqual(result.manifest['runs'][0]['status'], 'completed')
                original = json.loads((result.path / 'execution.json').read_text())
                self.assertEqual(result.manifest['runs'], original['runs'])

    def test_ineligible_callback_fails_run_without_success_tables(self):
        experiment = self.experiment(python=True)
        result = experiment.run(self.lake, repository=self.repo, strategy=self.strategy,
            decide=lambda v,p,prev: prajna.Decision({self.ids[2]: 1.0}),
            level=prajna.ResultLevel.FULL)
        run = result.manifest['runs'][0]
        self.assertEqual(run['status'], 'failed')
        self.assertEqual(run['table_hashes'], {})
        summary = prajna.read_run(str(self.lake), experiment.describe(self.lake).experiment_id,
            result.manifest['execution']['id'], run['run_id'], table='summary')
        self.assertIn('Eligible Set', summary.to_pylist()[0]['error'])

    def test_python_rotation_uses_shared_engine_risk_exits_and_replays(self):
        strategy = prajna.Strategy('pit_rotation', '1', bar_columns=(),
                                   rebalance_every=self.params['rebalance_every'])
        for engine in ('vector', 'fast_event'):
            with self.subTest(engine=engine):
                builtin = self.experiment(engine).run(self.lake, repository=self.repo, level=prajna.ResultLevel.FULL)
                experiment = self.experiment(engine, python=True)
                result = experiment.run(self.lake, repository=self.repo, strategy=strategy,
                    decide=flat_rotation, level=prajna.ResultLevel.FULL)
                self.assertEqual(result.manifest['runs'][0]['status'], 'completed')
                a, b = builtin.manifest['runs'][0], result.manifest['runs'][0]
                # Strategies retain distinct IDs. Every Engine-produced data value agrees.
                self.assertEqual(a['table_hashes'], b['table_hashes'])
                replay = experiment.run(self.lake, repository=self.repo, strategy=strategy,
                    decide=flat_rotation, level=prajna.ResultLevel.FULL, threads=2)
                self.assertEqual(replay.action, 'replayed')
                self.assertEqual(result.manifest['execution']['id'], replay.manifest['execution']['id'])
                self.assertEqual(result.manifest['runs'], replay.manifest['runs'])

    def test_fixed_universe_without_broad_index_matches_cli(self):
        self.load_fixture('no-broad-lake', '--no-broad-index')
        self.assertIsNone(self.data['pit']['catalog']['broad_indices'])
        pit = self.fixed_pit()
        views, decisions = self.generate_views(pit)

        # Fixed membership follows the official lifetime only; no broad-index
        # list, fund classification or Python-side inclusion is consulted.
        for day, view in enumerate(views, 1):
            self.assertEqual(view.session_date, f'2023-01-{day:02}')
            expected = [self.ids[1]] + ([self.ids[2]] if day >= 4 else [])
            self.assertEqual(view.universe, expected)
            self.assertNotIn(self.ids[0], view.eligible)
        self.assertEqual(views[3].eligible, [self.ids[1]])
        self.assertIn(self.ids[0], views[4].risk_excluded)

        cli, run = self.run_cli(pit)
        self.assertEqual(run['status'], 'completed')
        self.assertEqual(run['run_spec']['pit']['fixed_universe'], FIXED_UNIVERSE)
        universe_spec = run['run_spec']['session_inputs'][0]['eligible']['universe']['run_spec']
        self.assertEqual(universe_spec['fixed_list'], ['159002', '159003'])
        self.assertEqual(universe_spec['fixed_list_version'], FIXED_UNIVERSE['version'])
        self.assertIsNone(universe_spec.get('broad_index_version'))
        self.assert_plan_matches_views(run['run_spec'], views)
        self.assert_decisions_match_cli(cli, run, views, decisions)

        # End-to-end callback Engine values agree with the CLI built-in S2 run.
        strategy = prajna.Strategy('pit_rotation', '1', bar_columns=(),
                                   rebalance_every=self.params['rebalance_every'])
        result = self.experiment(python=True, pit=pit).run(
            self.lake, repository=self.repo, strategy=strategy, decide=flat_rotation,
            level=prajna.ResultLevel.FULL)
        self.assertEqual(result.manifest['runs'][0]['status'], 'completed')
        self.assertEqual(result.manifest['runs'][0]['table_hashes'], run['table_hashes'])

    def test_explicit_null_without_broad_index_requires_versioned_list(self):
        self.load_fixture('null-no-broad-lake', '--no-broad-index')
        pit = copy.deepcopy(self.data['pit'])
        pit['fixed_universe'] = None
        with self.assertRaisesRegex(ValueError,
                                    'broad-index PIT selection requires a versioned list'):
            self.generate(lambda view, params, previous: prajna.Decision({}), pit=pit,
                          strategy=self.view_strategy())
        failed = self.run_cli_failure(pit)
        self.assertEqual(failed.returncode, 2)
        self.assertIn('broad-index PIT selection requires a versioned list', failed.stderr)

    def test_pit_default_and_explicit_null_keep_existing_contract(self):
        # Explicit null retains the legacy broad-index selector.
        broad_pit = copy.deepcopy(self.data['pit'])
        self.assertIsNone(broad_pit['fixed_universe'])
        broad_views, broad_decisions = self.generate_views(broad_pit)
        cli, run = self.run_cli(broad_pit)
        self.assertEqual(run['status'], 'completed')
        self.assertIsNone(run['run_spec']['pit'].get('fixed_universe'))
        self.assert_plan_matches_views(run['run_spec'], broad_views)
        self.assert_decisions_match_cli(cli, run, broad_views, broad_decisions)
        self.assertIn(self.ids[0], broad_views[1].universe)
        self.assertNotIn(self.ids[0], broad_views[-1].universe)
        self.assertNotIn(self.ids[1], broad_views[-1].universe)

        # A missing field keeps the versioned MVP-6 default instead of the
        # broad-index selector; its empty included list still fails the Engine
        # minimum and is never silently widened by the binding.
        default_pit = copy.deepcopy(self.data['pit'])
        del default_pit['fixed_universe']
        self.assertEqual(self.generate(
            lambda view, params, previous: prajna.Decision({}), pit=default_pit), [])
        cli, run = self.run_cli(default_pit, level='summary')
        self.assertEqual(run['status'], 'failed')
        self.assertEqual(run['run_spec']['pit']['fixed_universe']['version'],
                         'mvp6-fixed-etf@2')
        self.assertEqual(run['run_spec']['pit']['fixed_universe']['codes'], [])
        for entry in run['run_spec']['session_inputs']:
            self.assertEqual(entry['eligible']['universe']['run_spec']['fixed_list'], [])
        summary = prajna.read_run(str(self.lake), cli['exp'], cli['exe'], run['run_id'],
                                  table='summary')
        self.assertIn('P27-19 escalation', summary.to_pylist()[0]['error'])

        # The Python strategy entry applies the same Engine threshold.
        experiment = self.experiment(python=True, pit=default_pit)
        result = experiment.run(self.lake, repository=self.repo,
                                strategy=self.view_strategy(), decide=flat_rotation,
                                level=prajna.ResultLevel.SUMMARY)
        run = result.manifest['runs'][0]
        self.assertEqual(run['status'], 'failed')
        summary = prajna.read_run(
            str(self.lake), experiment.describe(self.lake).experiment_id,
            result.manifest['execution']['id'], run['run_id'], table='summary')
        self.assertIn('P27-19 escalation', summary.to_pylist()[0]['error'])

    def test_invalid_fixed_universe_is_rejected_by_python_and_cli(self):
        pit = copy.deepcopy(self.data['pit'])
        pit['fixed_universe'] = {'version': '', 'codes': ['159002']}
        with self.assertRaisesRegex(ValueError, 'invalid fixed Universe'):
            self.generate(lambda view, params, previous: prajna.Decision({}), pit=pit)
        failed = self.run_cli_failure(pit)
        self.assertEqual(failed.returncode, 2)
        self.assertIn('invalid fixed Universe', failed.stderr)

    def test_fixed_universe_lifecycle_matches_cli_after_termination(self):
        self.load_fixture('terminated-fixed-lake', '--terminated')
        pit = self.fixed_pit()
        pit['fixed_universe'] = {'version': 'synthetic-fixed-etfs@1',
                                 'codes': ['159001', '159002', '159003'],
                                 'minimum_code_count': 3}
        views, decisions = self.generate_views(pit)
        cli, run = self.run_cli(pit)
        self.assertEqual(run['status'], 'completed')
        self.assert_plan_matches_views(run['run_spec'], views)
        self.assert_decisions_match_cli(cli, run, views, decisions)
        by_day = {view.session_date: view for view in views}
        self.assertNotIn(self.ids[2], by_day['2023-01-03'].universe)
        self.assertIn(self.ids[2], by_day['2023-01-04'].universe)
        self.assertIn(self.ids[0], by_day['2023-01-07'].universe)
        self.assertNotIn(self.ids[0], by_day['2023-01-08'].universe)
        self.assertIn(self.ids[0], by_day['2023-01-05'].risk_excluded)
