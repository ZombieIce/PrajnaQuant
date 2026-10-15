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

    def experiment(self, engine='vector', python=False):
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
            pit=self.data['pit'],
        )

    def generate(self, decide, pit=None, params=None, strategy=None):
        return prajna.generate_decisions(
            lake_root=str(self.lake), dsv=self.data['dsv'], venue='SZSE', members=[],
            strategy=strategy or self.strategy, decide=decide, params=params or self.params,
            pit=self.data['pit'] if pit is None else pit,
        )

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
                         ['2023-01-01', '2023-01-03', '2023-01-05', '2023-01-07', '2023-01-09'])
        self.assertNotIn(self.ids[2], seen[0][1])
        self.assertEqual(seen[0][2], [])
        self.assertEqual(seen[1][2], self.ids[:2])
        self.assertNotIn(self.ids[0], seen[2][1])
        self.assertEqual(seen[3][1], [self.ids[2]])

    def test_invalid_buys_fail_and_explicit_exit_remains_valid(self):
        cases = [('future listing', self.ids[2], '2023-01-01'),
                 ('not eligible', self.ids[0], '2023-01-01'),
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
        summary = prajna.read_run(self.lake, experiment.describe(self.lake).experiment_id,
            result.manifest['execution']['id'], run['run_id'], table='summary')
        self.assertIn('Eligible Set', summary.to_pylist()[0]['error'])

    def test_python_rotation_uses_shared_engine_risk_exits_and_replays(self):
        for engine in ('vector', 'fast_event'):
            with self.subTest(engine=engine):
                builtin = self.experiment(engine).run(self.lake, repository=self.repo, level=prajna.ResultLevel.FULL)
                experiment = self.experiment(engine, python=True)
                result = experiment.run(self.lake, repository=self.repo, strategy=self.strategy,
                    decide=rotation, level=prajna.ResultLevel.FULL)
                self.assertEqual(result.manifest['runs'][0]['status'], 'completed')
                a, b = builtin.manifest['runs'][0], result.manifest['runs'][0]
                # Strategies retain distinct IDs. Every Engine-produced data value agrees.
                self.assertEqual(a['table_hashes'], b['table_hashes'])
                replay = experiment.run(self.lake, repository=self.repo, strategy=self.strategy,
                    decide=rotation, level=prajna.ResultLevel.FULL, threads=2)
                self.assertEqual(replay.action, 'replayed')
                self.assertEqual(result.manifest['execution']['id'], replay.manifest['execution']['id'])
                self.assertEqual(result.manifest['runs'], replay.manifest['runs'])
