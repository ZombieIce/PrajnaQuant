"""Synthetic GitHub and temporary-process evidence; never writes a live issue."""
import argparse
import copy
import importlib.util
import json
import os
import signal
import time
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch, MagicMock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))

SPEC = importlib.util.spec_from_file_location('workflow', Path(__file__).resolve().parents[1] / 'scripts/workflow.py')
w = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(w)
HEAD = 'a' * 40
OLD = 'b' * 40


def issue(state='open', reason=None, labels=('ready-for-agent',), owners=(), body='## Blocked by\nNone'):
    return {'state': state, 'state_reason': reason, 'labels': [{'name': n} for n in labels],
            'assignees': [{'login': n} for n in owners], 'body': body, 'updated_at': 'fixed'}


class FakeGitHub:
    repo = 'synthetic/repo'

    def __init__(self):
        self.issues = {1: issue(), 2: issue('closed', 'completed')}
        self.pull = {'state': 'open', 'head': {'sha': HEAD}, 'body': 'Closes #1'}
        self.comments = [
            {'id': 1, 'body': f'**Implementer:** Ready for independent review at {HEAD}.'},
            {'id': 2, 'body': f'**Independent reviewer:** Reviewed head: {HEAD}\nSpec/Standards require human assessment.'}]
        self.dependencies = []
        self.checks = [{'name': 'lightweight', 'bucket': 'pass', 'state': 'SUCCESS'}]
        self.deletes = []
        self.calls = []
        self.issue_reads = 0
        self.on_read = None
        self.permission = False
        self.native_error = None
        self.head_reads = 0
        self.new_head = False

    def api(self, path, method='GET'):
        self.calls.append((path, method))
        if method == 'DELETE':
            if self.permission:
                raise w.EvidenceError('permission', 'synthetic denial')
            self.deletes.append(path)
            name = path.rsplit('/', 1)[1]
            self.issues[1]['labels'] = [v for v in self.issues[1]['labels'] if v['name'] != name]
            return []
        if path.startswith('pulls/'):
            self.head_reads += 1
            pull = copy.deepcopy(self.pull)
            if self.new_head and self.head_reads > 1:
                pull['head']['sha'] = OLD
            return pull
        n = int(path.split('/')[1])
        self.issue_reads += 1
        if self.on_read:
            self.on_read(self, self.issue_reads)
        return copy.deepcopy(self.issues[n])

    def pages(self, path):
        if path.endswith('/comments'):
            return self.comments
        if self.native_error:
            raise self.native_error
        return self.dependencies

    def required(self, pr):
        if isinstance(self.checks, Exception):
            raise self.checks
        return self.checks


class InspectionTests(unittest.TestCase):
    def test_mechanical_success_is_not_review_approval(self):
        report = w.inspect(FakeGitHub(), 1, 'agent', 9)
        self.assertEqual(report['result'], 'mechanical_evidence_only')
        self.assertIn('human', report['handoff'])
        self.assertIn('do not establish', report['claim_limit'])

    def test_claim_conflict_and_multiple_triage(self):
        api = FakeGitHub()
        api.issues[1] = issue(owners=['other'], labels=['ready-for-agent', 'needs-info'])
        gaps = w.inspect(api, 1, 'agent')['gaps']
        self.assertEqual(len(gaps), 2)
        self.assertTrue(any('conflict' in g for g in gaps))

    def test_own_claim_or_unclaimed_allowed(self):
        for owners in [(), ('agent',)]:
            self.assertEqual(w.issue_gaps(issue(owners=owners), 'agent'), [])

    def test_noncompleted_closed_blocker_is_not_satisfied(self):
        for state, reason in [('open', None), ('closed', 'not_planned'), ('closed', None)]:
            with self.subTest(state=state, reason=reason):
                api = FakeGitHub()
                api.issues[1]['body'] = '## Blocked by\n- #2'
                api.dependencies = [{'number': 2, 'url': 'https://api.github.com/repos/synthetic/repo/issues/2'}]
                api.issues[2] = issue(state, reason)
                self.assertIn('blocker #2 is not closed/completed', w.inspect(api, 1, 'agent')['gaps'])

    def test_completed_blocker_and_matching_native_dependency(self):
        api = FakeGitHub()
        api.issues[1]['body'] = '## Blocked by\n- #2\n## Notes\n#99'
        api.dependencies = [{'number': 2, 'url': 'https://api.github.com/repos/synthetic/repo/issues/2'}]
        self.assertEqual(w.inspect(api, 1, 'agent')['gaps'], [])

    def test_rationale_pr_reference_is_not_a_dependency(self):
        ids, found, ambiguous = w.blockers('## Blocked by\n- #370（closed/completed，PR #372 已交付）\n## Scope\n#99')
        self.assertTrue(found)
        self.assertEqual(ids, {370})
        self.assertEqual(w.blockers('Blocked by: #2, #3')[0], {2, 3})

    def test_bold_colon_dependency_declaration(self):
        self.assertEqual(w.blockers('**Blocked by:** #2, #3'), ({2, 3}, True, False))
        self.assertEqual(w.blockers('**Blocked by**: None'), (set(), True, False))

    def test_unparsed_body_dependency_is_unknown_not_empty(self):
        for body in ['## Blocked by\n', '## Blocked by\n- https://github.com/synthetic/repo/issues/2',
                     '## Blocked by\n- [#2](https://github.com/synthetic/repo/issues/2)']:
            api = FakeGitHub()
            api.issues[1]['body'] = body
            self.assertEqual(w.inspect(api, 1, 'agent')['result'], 'unknown')

    def test_body_native_disagreement(self):
        api = FakeGitHub()
        api.issues[1]['body'] = 'Blocked by: #2'
        self.assertIn('mismatch', w.inspect(api, 1, 'agent')['gaps'][0])

    def test_cross_repo_native_dependency_is_unknown(self):
        api = FakeGitHub()
        api.dependencies = [{'number': 2, 'url': 'https://api.github.com/repos/another/repo/issues/2'}]
        self.assertIn('cross-repository', w.inspect(api, 1, 'agent')['unknown'][0])

    def test_unavailable_native_dependency_is_not_empty_success(self):
        api = FakeGitHub()
        api.native_error = w.EvidenceError('permission', 'denied')
        self.assertEqual(w.inspect(api, 1, 'agent')['result'], 'unknown')

    def test_missing_blocker_field_is_unknown(self):
        api = FakeGitHub()
        api.issues[1]['body'] = 'Parent: #2'
        self.assertIn('Blocked by field absent', w.inspect(api, 1, 'agent')['unknown'])

    def test_stale_ready_and_review(self):
        api = FakeGitHub()
        api.comments = [{'body': c['body'].replace(HEAD, OLD)} for c in api.comments]
        report = w.inspect(api, 1, 'agent', 9)
        self.assertTrue(all(not c['current'] for c in report['declarations'].values()))

    def test_latest_declaration_overrides_old(self):
        api = FakeGitHub()
        api.comments.append({'body': f'**Implementer:** Ready for independent review at {OLD}.'})
        self.assertFalse(w.inspect(api, 1, 'agent', 9)['declarations']['ready']['current'])

    def test_unprefixed_comment_not_declaration(self):
        self.assertFalse(w.declarations([{'body': f'Ready for independent review at {HEAD}'}], HEAD)['ready']['current'])

    def test_base_sha_in_review_cannot_prove_current_head(self):
        comments = [{'body': f'**Independent reviewer:** Reviewed head/base: {OLD} / {HEAD}'}]
        self.assertFalse(w.declarations(comments, HEAD)['review']['current'])

    def test_pending_checks_preserve_ready_handoff_evidence(self):
        api = FakeGitHub()
        api.checks[0]['bucket'] = 'pending'
        r = w.inspect(api, 1, 'agent', 9)
        self.assertTrue(r['declarations']['ready']['current'])
        self.assertEqual(r['checks_state'], 'pending_or_failed')
        self.assertNotIn('merge_ready', r.values())

    def test_missing_required_check_metadata_unknown(self):
        api = FakeGitHub()
        api.checks = w.EvidenceError('unknown', 'no check metadata')
        self.assertIn('required checks: unknown', w.inspect(api, 1, 'agent', 9)['unknown'])

    def test_head_race_requires_refresh(self):
        api = FakeGitHub()
        api.new_head = True
        self.assertIn('PR head changed during inspection; refresh required', w.inspect(api, 1, 'agent', 9)['unknown'])

    def test_issue_race_requires_refresh(self):
        api = FakeGitHub()
        def mutate(a, n):
            if n == 2:
                a.issues[1]['state'] = 'closed'
        api.on_read = mutate
        self.assertTrue(any('issue changed' in s for s in w.inspect(api, 1, 'agent')['unknown']))

    def test_wrong_pr_association_is_gap(self):
        api = FakeGitHub()
        api.pull['body'] = 'Closes #99'
        self.assertTrue(any('closing reference' in s for s in w.inspect(api, 1, 'agent', 9)['gaps']))


class CleanupTests(unittest.TestCase):
    def api(self):
        a = FakeGitHub()
        a.issues[1] = issue('closed', 'not_planned', ['ready-for-agent', 'business', 'needs-info'])
        return a

    def test_default_dry_run(self):
        a = self.api()
        self.assertEqual(w.cleanup(a, 1)['would_remove'], ['needs-info', 'ready-for-agent'])
        self.assertEqual(a.deletes, [])

    def test_only_triage_delete_preserves_business_reason_body(self):
        a = self.api()
        old = copy.deepcopy(a.issues[1])
        self.assertEqual(w.cleanup(a, 1, True)['result'], 'applied')
        self.assertEqual(a.issues[1]['labels'], [{'name': 'business'}])
        for key in ['body', 'state', 'state_reason', 'assignees']:
            self.assertEqual(old[key], a.issues[1][key])
        self.assertTrue(all(method in ('GET', 'DELETE') for _, method in a.calls))
        self.assertTrue(all('/labels/' in path for path in a.deletes))

    def test_open_issue_refused_without_writes(self):
        a = FakeGitHub()
        self.assertEqual(w.cleanup(a, 1, True)['result'], 'refused')
        self.assertEqual(a.deletes, [])

    def test_reopened_before_write_refuses(self):
        a = self.api()
        a.on_read = lambda a, n: a.issues[1].update(state='open') if n == 2 else None
        self.assertEqual(w.cleanup(a, 1, True)['result'], 'race')
        self.assertEqual(a.deletes, [])

    def test_reopen_after_first_delete_stops_remaining_deletes(self):
        a = self.api()
        def mutate(a, n):
            if n == 3:
                a.issues[1]['state'] = 'open'
        a.on_read = mutate
        result = w.cleanup(a, 1, True)
        self.assertEqual(result['result'], 'race')
        self.assertEqual(result['removed'], ['needs-info'])
        self.assertIn({'name': 'ready-for-agent'}, a.issues[1]['labels'])

    def test_reason_change_before_write_refuses(self):
        a = self.api()
        a.on_read = lambda a, n: a.issues[1].update(state_reason='completed') if n == 2 else None
        self.assertEqual(w.cleanup(a, 1, True)['result'], 'race')
        self.assertEqual(a.deletes, [])

    def test_permission_failure_explicit(self):
        a = self.api()
        a.permission = True
        report = w.cleanup(a, 1, True)
        self.assertEqual(report['result'], 'permission')
        self.assertEqual(report['removed'], [])
        self.assertTrue(report['recheck_required'])

    def test_partial_permission_failure_reports_completed_writes(self):
        a = self.api()
        def mutate(a, n):
            if n == 3:
                a.permission = True
        a.on_read = mutate
        report = w.cleanup(a, 1, True)
        self.assertEqual(report['result'], 'permission')
        self.assertEqual(report['removed'], ['needs-info'])
        self.assertIn({'name': 'ready-for-agent'}, a.issues[1]['labels'])

    def test_partial_refresh_failure_reports_completed_writes(self):
        a = self.api()
        def mutate(a, n):
            if n == 3:
                raise w.EvidenceError('unknown', 'synthetic unavailable read')
        a.on_read = mutate
        report = w.cleanup(a, 1, True)
        self.assertEqual(report['result'], 'unknown')
        self.assertEqual(report['removed'], ['needs-info'])

    def test_concurrent_label_removal_is_idempotent(self):
        a = self.api()
        def mutate(a, n):
            if n == 2:
                a.issues[1]['labels'] = [{'name': 'business'}, {'name': 'ready-for-agent'}]
        a.on_read = mutate
        self.assertEqual(w.cleanup(a, 1, True)['removed'], ['ready-for-agent'])


class VerificationTests(unittest.TestCase):
    def test_cargo_mixed_empty_harness_does_not_hide_positive_count(self):
        result = w.counts('test result: ok. 0 passed; 0 failed; 0 ignored;\n'
                          'test result: ok. 3 passed; 0 failed; 2 ignored;', 'cargo')
        self.assertEqual(result, {'executed': 3, 'passed': 3, 'failed': 0, 'skipped': 2})

    def test_cargo_zero_and_missing_output_distinct(self):
        self.assertEqual(w.counts('test result: ok. 0 passed; 0 failed; 12 ignored;', 'cargo')['executed'], 0)
        self.assertIsNone(w.counts('Compiling complete', 'cargo'))

    def test_unittest_skips_and_failures(self):
        r = w.counts('Ran 5 tests in 0.1s\nFAILED (failures=1, errors=1, skipped=2)', 'unittest')
        self.assertEqual(r, {'executed': 3, 'passed': 1, 'failed': 2, 'skipped': 2})

    def test_unittest_duplicate_summary_is_ambiguous(self):
        self.assertIsNone(w.counts('Ran 1 test in 0.1s\nRan 2 tests in 0.1s', 'unittest'))

    def test_playwright_counts(self):
        self.assertEqual(w.counts('  22 passed (30s)\n  1 skipped', 'playwright')['executed'], 22)
        self.assertIsNone(w.counts('Total: 22 tests in 12 files', 'playwright'))

    def args(self, root, output, **extra):
        values = dict(command=[sys.executable, '-c', f'print({output!r})'], log=str(root/'run.log'),
                      output=str(root/'record.json'), timeout=10, kind='cargo', executor='synthetic',
                      selection='explicit synthetic selection', allow_empty=None, allow_skips=None)
        values.update(extra)
        return argparse.Namespace(**values)

    def test_actual_process_positive_and_log_not_overwritten(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), 'test result: ok. 2 passed; 0 failed; 1 ignored;')
            self.assertEqual(w.record(args)['result'], 'passed')
            self.assertEqual(json.loads(Path(args.output).read_text())['counts']['executed'], 2)
            with self.assertRaises(FileExistsError):
                w.record(args)

    def test_head_change_never_attributes_execution_to_new_head(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), 'test result: ok. 1 passed; 0 failed; 0 ignored;')
            with patch.object(w, 'commit', side_effect=[HEAD, OLD]):
                report = w.record(args)
            self.assertEqual(report['sha'], HEAD)
            self.assertEqual(report['ending_sha'], OLD)
            self.assertEqual(report['result'], 'source_changed')
            self.assertEqual(report['source_verification'], 'manual_mapping_required')

    def test_zero_match_actual_process_fails_unless_explicit(self):
        for allow, expected in [(None, 'failed'), ('explicit discovery-only empty scope', 'passed')]:
            with tempfile.TemporaryDirectory() as d:
                args = self.args(Path(d), 'test result: ok. 0 passed; 0 failed; 0 ignored;', allow_empty=allow)
                self.assertEqual(w.record(args)['result'], expected)

    def test_missing_summary_fails_even_with_empty_permission(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), 'compiled', allow_empty='empty allowed')
            self.assertEqual(w.record(args)['result'], 'failed')

    def test_positive_count_does_not_hide_exit_failure(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), '', command=[sys.executable, '-c',
                "print('test result: ok. 1 passed; 0 failed; 0 ignored;'); raise SystemExit(1)"])
            self.assertEqual(w.record(args)['result'], 'failed')

    def test_unittest_explicit_skip_policy(self):
        for reason, expected in [(None, 'failed'), ('optional backend excluded from this job', 'passed')]:
            with tempfile.TemporaryDirectory() as d:
                args = self.args(Path(d), 'Ran 3 tests in 0.1s\nOK (skipped=1)', kind='unittest', allow_skips=reason)
                self.assertEqual(w.record(args)['result'], expected)

    def test_timeout_is_failure(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), '', command=[sys.executable, '-c', 'import time; time.sleep(5)'], timeout=1)
            self.assertEqual(w.record(args)['exit_code'], 124)

    def test_disk_floor_refuses_new_process(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), 'test result: ok. 1 passed; 0 failed; 0 ignored;')
            with patch.object(w.shutil, 'disk_usage', return_value=MagicMock(free=9*1024**3)), patch.object(w.subprocess, 'Popen') as child, patch.object(w, 'commit', return_value=HEAD), patch.object(w, 'git', return_value=''), patch.object(w, 'runtime_version', return_value='synthetic'):
                report = w.record(args)
            child.assert_not_called()
            self.assertEqual(report['exit_code'], 125)
            self.assertEqual(report['stopped_reason'], 'disk_reserve_before_start')

    def test_disk_floor_stops_owned_process(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), '', command=[sys.executable, '-c', 'import time; time.sleep(10)'])
            with patch.object(w.shutil, 'disk_usage', side_effect=[MagicMock(free=20*1024**3), MagicMock(free=9*1024**3)]):
                report = w.record(args)
            self.assertEqual(report['exit_code'], 125)
            self.assertEqual(report['stopped_reason'], 'disk_reserve_during_run')
            self.assertEqual(report['result'], 'failed')

    def test_source_log_path_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            args = self.args(Path(d), '', log=str(w.ROOT/'private.log'))
            with self.assertRaises(w.EvidenceError):
                w.record(args)


class ClassificationAndGateTests(unittest.TestCase):
    def test_rust_inline_tests_unknown_even_in_test_directory(self):
        self.assertEqual(w.classify('crates/x/tests/a.rs', '#[test]\nfn t() {}'), 'unknown')
        self.assertEqual(w.classify('crates/x/src/lib.rs', '#[cfg(test)]'), 'unknown')

    def test_docs_as_primary_and_generated_separate(self):
        self.assertEqual(w.classify('docs/rules.md', '', True), 'implementation')
        self.assertEqual(w.classify('docs/rules.md', ''), 'documentation')
        self.assertEqual(w.classify('apps/web/package-lock.json', ''), 'generated')
        self.assertEqual(w.classify('apps/web/e2e/a.spec.ts', ''), 'tests_candidate')
        self.assertEqual(w.classify('poc/vendor/patched/src/lib.rs', ''), 'unknown')
        self.assertEqual(w.classify('crates/x/src/api.rs', '#[cfg(all(test, unix))]'), 'unknown')

    def test_renames_deletions_and_added_counts_in_temporary_git(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            def g(*args):
                return subprocess.check_output(['git', '-C', d, *args], text=True).strip()
            g('init', '-q')
            g('config', 'user.email', 'synthetic@example.invalid')
            g('config', 'user.name', 'Synthetic')
            (root/'a.py').write_text('one\ntwo\n')
            g('add', '.')
            g('commit', '-qm', 'base')
            base = g('rev-parse', 'HEAD')
            (root/'a.py').rename(root/'b.py')
            (root/'doc.md').write_text('rules\n')
            g('add', '.')
            g('commit', '-qm', 'change')
            with patch.object(w, 'ROOT', root):
                r = w.size(base, 'HEAD', estimate_upper=2, hard_budget=4)
            self.assertEqual(r['total'], 5)
            self.assertEqual(r['totals']['implementation'], 4)
            self.assertEqual(r['totals']['documentation'], 1)
            self.assertTrue(r['review_estimate_x2'])
            self.assertTrue(r['hard_budget_exceeded'])

    def test_browser_trigger_paths(self):
        for path in ['apps/web/src/a.tsx', 'crates/x/src/lib.rs', 'Cargo.lock', 'src/main.rs',
                     '.github/workflows/ci.yml', 'scripts/workflow.py', 'rust-toolchain.toml']:
            with self.subTest(path=path):
                self.assertTrue(w.affected([path]))
        self.assertFalse(w.affected(['docs/guide.md', 'AGENTS.md', '.agents/skills/a/SKILL.md']))

    def test_no_unauthorized_skips_in_gate(self):
        for result in ['failure', 'cancelled', 'skipped', 'pending', None]:
            with self.subTest(result=result):
                self.assertEqual(w.gate({'core': 'success', 'web': result}, True)['result'], 'failed')
                self.assertEqual(w.gate({'core': result, 'web': 'success'}, True)['result'], 'failed')
        self.assertEqual(w.gate({'core': 'success', 'web': 'skipped'}, False)['result'], 'success')
        self.assertEqual(w.gate({'core': 'success'}, False)['result'], 'failed')

    def test_explicit_repo_and_positive_targets(self):
        for repo in ['all', '--repo', 'https://github.com/a/b', 'a/b/c', '../repo', 'a/..', './repo']:
            with self.assertRaises(argparse.ArgumentTypeError):
                w.repo_name(repo)
        self.assertEqual(w.repo_name('synthetic/repo'), 'synthetic/repo')
        with self.assertRaises(argparse.ArgumentTypeError):
            w.positive('0')

    def test_timings_missing_is_unknown_not_zero(self):
        data = {'jobs': [{'name': 'synthetic', 'started_at': '2026-01-01T00:00:00Z',
            'completed_at': '2026-01-01T00:00:10Z', 'steps': [{'number': 1, 'name': 'test'}]}]}
        r = w.timings(data)[0]
        self.assertEqual(r['wall_seconds'], 10)
        self.assertIsNone(r['steps'][0]['seconds'])


class PreflightTests(unittest.TestCase):
    def test_global_ps_must_see_owned_child_group(self):
        for output, expected in [(f'{w.os.getpid()} 99\n12345 12345\n', True),
                                 (f'{w.os.getpid()} 99\n', False),
                                 (f'{w.os.getpid()} 99\n12345 99\n', False)]:
            child = MagicMock(pid=12345)
            with patch.object(w.subprocess, 'Popen', return_value=child), patch.object(w, 'process',
                    return_value=subprocess.CompletedProcess([], 0, output, '')):
                self.assertEqual(w.process_visibility(), expected)
            child.terminate.assert_called_once()
            child.wait.assert_called_once()

    def test_denied_global_ps_is_gap_and_child_cleaned(self):
        child = MagicMock(pid=12345)
        with patch.object(w.subprocess, 'Popen', return_value=child), patch.object(w, 'process',
                side_effect=w.EvidenceError('permission', 'denied')):
            self.assertFalse(w.process_visibility())
        child.terminate.assert_called_once()


    def test_missing_required_runtimes_are_gaps_and_gh_is_informational(self):
        versions = {'node': 'v22.0.0', 'python3.12': 'Python 3.12.15', 'rustc': 'rustc 1.98.1',
                    'npm': '10.0.0', 'gh': 'gh version synthetic'}
        for missing in [(), ('rustc',), ('npm',), ('rustc', 'npm'), ('gh',)]:
            def command(args):
                return subprocess.CompletedProcess(args, 127 if args[0] in missing else 0,
                    '' if args[0] in missing else versions[args[0]], '')
            with patch.object(w, 'process', side_effect=command), patch.object(w, 'runtime_version', return_value='ok'), \
                 patch.object(w, 'process_visibility', return_value=True), patch.object(w.socket, 'socket'), \
                 patch.object(w.shutil, 'disk_usage', return_value=MagicMock(free=20*1024**3)), \
                 patch.object(w.os, 'access', return_value=True):
                result = w.preflight(18789, '/synthetic/browser')
            expected = 'gaps' if set(missing) & {'rustc', 'npm'} else 'capable_pending_behavior_test'
            self.assertEqual(result['result'], expected, missing)

    def test_empty_successful_tool_output_is_not_available(self):
        with patch.object(w, 'process', return_value=subprocess.CompletedProcess([], 0, '', '')), \
             patch.object(w, 'runtime_version', return_value='ok'), patch.object(w, 'process_visibility', return_value=True), \
             patch.object(w.socket, 'socket'), patch.object(w.shutil, 'disk_usage', return_value=MagicMock(free=20*1024**3)):
            result = w.preflight(18789, '/synthetic/browser')
        self.assertFalse(result['rust_available'])
        self.assertFalse(result['npm_available'])
        self.assertEqual(result['result'], 'gaps')


class OwnedLifecycleTests(unittest.TestCase):
    def check_stopped(self, nested, reason):
        import workflow_process as control
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            pid_file, marker = root/'worker.pid', root/'survived'
            leaf = root/'leaf.py'
            leaf.write_text("import os,signal,time\nfrom pathlib import Path\n"
                "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                f"Path({str(pid_file)!r}).write_text(str(os.getpid()))\n"
                "time.sleep(5)\n"
                f"Path({str(marker)!r}).write_text('unexpected owned work')\n"
                "time.sleep(30)\n")
            parent = root/'parent.py'
            if nested:
                # Match Playwright's detached web-server -> fixture recorder -> command structure.
                inner = [sys.executable, str(w.ROOT/'scripts/workflow.py'), 'run', '--kind', 'cargo',
                         '--executor', 'synthetic-inner', '--selection', 'synthetic-leaf',
                         '--timeout', '30', '--log', str(root/'inner.log'), '--', sys.executable, str(leaf)]
                bridge = root/'bridge.py'
                bridge.write_text(f'import subprocess\nsubprocess.run({inner!r}, check=False)\n')
                parent.write_text('import subprocess,sys\n'
                    f'p=subprocess.Popen([sys.executable,{str(bridge)!r}],start_new_session=True)\n'
                    'p.wait()\n')
            elif reason == 'race':
                parent.write_text('import subprocess,sys,time\nfrom pathlib import Path\n'
                    f'while not Path({str(root/"release")!r}).exists(): time.sleep(0.01)\n'
                    f'subprocess.Popen([sys.executable,{str(leaf)!r}],start_new_session=True)\n'
                    'time.sleep(30)\n')
            else:
                parent.write_text('import subprocess,sys,time\nfrom pathlib import Path\n'
                    f'subprocess.Popen([sys.executable,{str(leaf)!r}])\n'
                    f'while not Path({str(pid_file)!r}).exists(): time.sleep(0.01)\n'
                    + ("print('test result: ok. 1 passed; 0 failed; 0 ignored;')\n" if reason == 'normal'
                       else 'time.sleep(30)\n'))
            outsider = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'],
                                        start_new_session=True, stdout=subprocess.DEVNULL)
            try:
                args = argparse.Namespace(command=[sys.executable, str(parent)], log=str(root/'outer.log'),
                    output=None, timeout=2 if reason in ('timeout', 'race') else 15, kind='cargo', executor='synthetic-outer',
                    selection='owned lifecycle counterexample', allow_empty=None, allow_skips=None)
                if reason == 'disk':
                    def space(_):
                        return MagicMock(free=(9 if pid_file.exists() else 20)*1024**3)
                    with patch.object(w.shutil, 'disk_usage', side_effect=space):
                        report = w.record(args)
                elif reason == 'race':
                    original_signal = control.send_group
                    released = False
                    def racing_signal(group, sig):
                        nonlocal released
                        if sig == signal.SIGSTOP and not released:
                            released = True
                            (root/'release').touch()
                            until = time.monotonic() + 2
                            while not pid_file.exists() and time.monotonic() < until:
                                time.sleep(0.01)
                        original_signal(group, sig)
                    with patch.object(control, 'send_group', side_effect=racing_signal):
                        report = w.record(args)
                else:
                    report = w.record(args)
                self.assertTrue(pid_file.exists(), 'counterexample must actually start the descendant')
                pid = int(pid_file.read_text())
                table = control.process_table()
                self.assertTrue(pid not in table or table[pid][2].startswith('Z'), 'owned descendant survived return')
                self.assertFalse(marker.exists(), 'owned descendant continued writing after cancellation')
                self.assertIsNone(outsider.poll(), 'unrelated sibling must remain alive')
                self.assertEqual(report['exit_code'], {'timeout': 124, 'race': 124, 'disk': 125, 'normal': 0}[reason],
                                 (root/'outer.log').read_text()[-2000:])
                if nested:
                    self.assertFalse((root/'inner.log').read_text(), 'terminated inner command has no false pass')
            finally:
                outsider.terminate()
                outsider.wait(timeout=5)
                # Exact synthetic leaf only, also cleans the probe if an assertion fails.
                if pid_file.exists():
                    pid = int(pid_file.read_text())
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_inherited_scope_from_unrelated_run_is_refused_without_signalling_it(self):
        import workflow_process as control
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            outsider = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
            try:
                (root/'owner.json').write_text(json.dumps({'pid': outsider.pid}))
                (root/('0'*32+'.json')).write_text(json.dumps({'parent': None, 'group': outsider.pid}))
                args = argparse.Namespace(command=[sys.executable, '-c', "print('test result: ok. 1 passed; 0 failed; 0 ignored;')"],
                    log=str(root/'refused.log'), output=None, timeout=5, kind='cargo', executor='synthetic',
                    selection='unrelated scope', allow_empty=None, allow_skips=None)
                with patch.dict(os.environ, {control.SCOPE_ENV: json.dumps([str(root), '0'*32])}):
                    result = w.record(args)
                self.assertEqual(result['exit_code'], 126)
                self.assertEqual(result['stopped_reason'], 'ownership_or_cleanup_unverified')
                self.assertIsNone(outsider.poll())
                self.assertNotIn('test result:', (root/'refused.log').read_text())
                self.assertIn('unverified inherited verification scope', (root/'refused.log').read_text())
            finally:
                outsider.terminate()
                outsider.wait(timeout=5)

    def test_reused_group_identity_is_excluded_from_failure_cleanup(self):
        import workflow_process as control
        with tempfile.TemporaryDirectory() as d:
            outsider = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
            try:
                with (Path(d)/'controlled.log').open('w') as stream:
                    owned = control.OwnedRun([sys.executable, '-c', 'import time; time.sleep(30)'],
                                             w.ROOT, dict(os.environ), stream)
                    entry = owned.root/f'{owned.node}.json'
                    original = entry.read_text()
                    row = json.loads(original)
                    row.update(group=outsider.pid, birth='different process birth')
                    entry.write_text(json.dumps(row))
                    owned.groups.add(outsider.pid)  # Model a previously leased PID reused by another group.
                    try:
                        with self.assertRaises(control.ProcessControlError):
                            owned.close()
                        self.assertIsNone(outsider.poll(), 'identity rejection must also exclude failure cleanup')
                        self.assertIsNotNone(owned.child.poll())
                    finally:
                        entry.write_text(original)
                        owned.close()
            finally:
                outsider.terminate()
                outsider.wait(timeout=5)

    def test_same_group_term_ignoring_descendant_after_leader_exit(self):
        self.check_stopped(False, 'timeout')

    def test_detached_spawn_racing_first_snapshot_is_rescanned_after_freeze(self):
        self.check_stopped(False, 'race')

    def test_successful_leader_does_not_leave_background_work(self):
        self.check_stopped(False, 'normal')

    def test_nested_recorder_in_detached_server_is_stopped(self):
        self.check_stopped(True, 'timeout')

    def test_disk_stop_covers_nested_recorder_and_preserves_unrelated_group(self):
        self.check_stopped(True, 'disk')


class TransportTests(unittest.TestCase):
    def test_permission_and_not_found_distinct(self):
        for status, kind in [(403, 'permission'), (404, 'unknown'), (500, 'error')]:
            r = subprocess.CompletedProcess([], 1, '', f'gh: response (HTTP {status}) secret body')
            with patch.object(w, 'process', return_value=r):
                with self.assertRaises(w.EvidenceError) as e:
                    w.GitHub('synthetic/repo').api('issues/1')
            self.assertEqual(e.exception.kind, kind)
            self.assertNotIn('secret', str(e.exception))

    def test_required_checks_no_data_is_unknown(self):
        for output in ['', '[]', 'invalid']:
            with patch.object(w, 'process', return_value=subprocess.CompletedProcess([], 0, output, '')):
                with self.assertRaises(w.EvidenceError):
                    w.GitHub('synthetic/repo').required(1)

    def test_required_pending_exit_eight_parsed(self):
        value = [{'name': 'lightweight', 'state': 'PENDING', 'bucket': 'pending'}]
        with patch.object(w, 'process', return_value=subprocess.CompletedProcess([], 8, json.dumps(value), '')):
            self.assertEqual(w.GitHub('synthetic/repo').required(1), value)

    def test_pages_not_silently_truncated(self):
        api = w.GitHub('synthetic/repo')
        with patch.object(api, 'api', side_effect=[[{'id': 1}]*100, [{'id': 2}]]):
            self.assertEqual(len(api.pages('issues/1/comments')), 101)
        with patch.object(api, 'api', return_value={}):
            with self.assertRaises(w.EvidenceError):
                api.pages('issues/1/comments')


if __name__ == '__main__':
    unittest.main()
