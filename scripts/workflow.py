#!/usr/bin/env python3
"""Narrow, read-only-by-default workflow evidence. Python 3.12 + gh; no daemon."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
from urllib.parse import quote

from workflow_process import OwnedRun, ProcessControlError

TRIAGE = frozenset({'needs-triage', 'needs-info', 'ready-for-agent', 'ready-for-human', 'wontfix'})
ROOT = Path(__file__).resolve().parents[1]
DISK_RESERVE = 10 * 1024**3


class EvidenceError(Exception):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


def process(args, **kwargs):
    try:
        return subprocess.run(args, capture_output=True, text=True, timeout=90, **kwargs)
    except FileNotFoundError:
        raise EvidenceError('unavailable', f'executable unavailable: {args[0]}') from None
    except subprocess.TimeoutExpired:
        raise EvidenceError('unknown', f'timeout: {args[0]}') from None
    except OSError:
        raise EvidenceError('permission', f'cannot execute: {args[0]}') from None


def repo_name(value):
    if (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_.-]+', value)
            or value.split('/')[-1] in ('.', '..')):
        raise argparse.ArgumentTypeError('explicit owner/repository required')
    return value


def positive(value):
    n = int(value)
    if n < 1:
        raise argparse.ArgumentTypeError('positive integer required')
    return n


def json_output(value):
    print(json.dumps(value, ensure_ascii=False, indent=2))


class GitHub:
    def __init__(self, repo):
        self.repo = repo

    def api(self, path, method='GET'):
        # All callers use repository-relative endpoints, never arbitrary URLs.
        result = process(['gh', 'api', f'repos/{self.repo}/{path}', '--method', method])
        if result.returncode:
            # Do not echo response bodies, credentials, issue text or research identities.
            status = re.search(r'HTTP (\d{3})', result.stderr)
            code = int(status[1]) if status else None
            kind = 'permission' if code in (401, 403) else 'unknown' if code == 404 else 'error'
            raise EvidenceError(kind, f'GitHub {method} failed ({code or "transport/auth"})')
        if not result.stdout.strip():
            return None
        try:
            return json.loads(result.stdout)
        except ValueError:
            raise EvidenceError('error', 'invalid GitHub JSON') from None

    def pages(self, path):
        rows = []
        # Bounded per explicit issue/PR; fail closed instead of silently truncating.
        for page in range(1, 101):
            batch = self.api(f'{path}{"&" if "?" in path else "?"}per_page=100&page={page}')
            if not isinstance(batch, list):
                raise EvidenceError('error', 'expected paginated GitHub array')
            rows.extend(batch)
            if len(batch) < 100:
                return rows
        raise EvidenceError('unknown', 'pagination limit reached')

    def required(self, pr):
        r = process(['gh', 'pr', 'checks', str(pr), '--repo', self.repo, '--required',
                     '--json', 'name,state,bucket'])
        if r.returncode not in (0, 1, 8) or not r.stdout.strip():
            raise EvidenceError('unknown', 'required checks unavailable; protection not inferred')
        try:
            checks = json.loads(r.stdout)
        except ValueError:
            raise EvidenceError('unknown', 'required checks unavailable') from None
        if not isinstance(checks, list) or not checks:
            raise EvidenceError('unknown', 'no required checks returned; policy requires human verification')
        # skip/neutral is deliberately not success for a required check.
        return [{'name': c['name'], 'state': c['state'], 'bucket': c['bucket']} for c in checks]


def blockers(body):
    """Only references in the Blocked by heading/inline field, not the whole issue."""
    section = []
    active = False
    found = False
    for line in body.splitlines():
        if re.match(r'^\s*(?:#{1,6}\s*)?(?:\*\*)?Blocked by\b', line, re.I):
            active = found = True
            section.append(line)
        elif re.match(r'^\s*#{1,6}\s', line):
            active = False
        elif active:
            section.append(line)
    ids = set()
    ambiguous = False
    declared = False
    for line in section:
        # Only the leading dependency list; PRs/specs in parenthetical rationale are not blockers.
        line = re.sub(r'^\s*(?:#{1,6}\s*)?(?:\*\*)?Blocked by\s*:?(?:\*\*)?\s*:?\s*', '', line, flags=re.I)
        line = re.sub(r'^\s*[-*]\s*', '', line).strip()
        match = re.match(r'#\d+(?:\s*[,，、]\s*#\d+)*', line)
        if match:
            ids.update(map(int, re.findall(r'#(\d+)', match[0])))
            declared = True
        elif re.match(r'^(?:None\b|无)', line, re.I):
            declared = True
        elif '#' in line or 'http' in line or (line and not declared):
            ambiguous = True
    return ids, found, ambiguous or (found and not declared)


def issue_gaps(issue, actor):
    gaps = []
    if issue['state'] != 'open':
        gaps.append('issue is not open')
    triage = TRIAGE & {x['name'] for x in issue.get('labels', [])}
    if triage != {'ready-for-agent'}:
        gaps.append('expected exactly one triage: ready-for-agent')
    owners = {x['login'] for x in issue.get('assignees', [])}
    if owners - {actor}:
        gaps.append('claim conflict: assigned to another actor')
    return gaps


def declarations(comments, head):
    # Markers only index claims, not independence, resolution, acceptance or validity.
    result = {}
    for role, prefix in [('ready', '**Implementer:**'), ('review', '**Independent reviewer:**')]:
        candidates = []
        for c in comments:
            body = c.get('body') or ''
            if not body.startswith(prefix):
                continue
            if role == 'ready' and not re.search(r'\bready\b', body, re.I):
                continue
            if role == 'review' and not re.search(r'reviewed (?:head|sha)|reviewed head/base|审查.*(?:head|SHA)', body, re.I):
                continue
            # Inspect only the declaration line, not arbitrary SHAs in a finding.
            line = next((s for s in body.splitlines() if re.search(
                r'\bready\b|reviewed (?:head|sha)|reviewed head/base|审查.*(?:head|SHA)', s, re.I)), '')
            pattern = (r"\bready\b[^\n]*?\bat\s+`?([0-9a-f]{7,40})\b" if role == 'ready'
                       else r"reviewed (?:head(?:/base)?|sha)\s*[:=]?\s*`?([0-9a-f]{7,40})\b")
            match = re.search(pattern, line, re.I)
            candidates.append({'id': c.get('id'), 'current': bool(match and head.startswith(match[1])),
                               'has_sha': bool(match)})
        result[role] = candidates[-1] if candidates else {'current': False, 'has_sha': False}
    return result


def inspect(api, number, actor, pr=None):
    issue = api.api(f'issues/{number}')
    gaps = issue_gaps(issue, actor)
    unknown = []
    body_ids, has_field, ambiguous_body = blockers(issue.get('body') or '')
    if not has_field:
        unknown.append('Blocked by field absent')
    elif ambiguous_body:
        unknown.append('Blocked by syntax ambiguous or empty; manual verification required')
    native_ids = None
    try:
        native = api.pages(f'issues/{number}/dependencies/blocked_by')
        # Cross-repository dependencies need explicit handling, never silently use local numbers.
        prefix = f'https://api.github.com/repos/{api.repo}/issues/'
        if any(not d.get('url', '').startswith(prefix) for d in native):
            unknown.append('cross-repository native dependency; manual verification required')
        else:
            native_ids = {d['number'] for d in native}
            if body_ids != native_ids:
                gaps.append(f'body/native dependency mismatch: body={sorted(body_ids)}, native={sorted(native_ids)}')
    except EvidenceError as e:
        unknown.append(f'native dependencies: {e.kind}')
    for n in sorted(body_ids | (native_ids or set())):
        dep = api.api(f'issues/{n}')
        if dep['state'] != 'closed' or dep.get('state_reason') != 'completed':
            gaps.append(f'blocker #{n} is not closed/completed')
    report = {'issue': number, 'gaps': gaps, 'unknown': unknown}
    if pr:
        pull = api.api(f'pulls/{pr}')
        head = pull['head']['sha']
        if not re.search(rf'\b(?:closes|fixes|resolves)\s+(?:{re.escape(api.repo)})?#{number}\b', pull.get('body') or '', re.I):
            gaps.append('PR closing reference to issue missing; verify ticket/PR association')
        if pull['state'] != 'open':
            gaps.append('PR is not open')
        comments = api.pages(f'issues/{pr}/comments')
        claims = declarations(comments, head)
        for role, claim in claims.items():
            if not claim['current']:
                gaps.append(f'{role} declaration absent, ambiguous or stale')
        report.update({'pr': pr, 'head': head, 'base': pull.get('base', {}).get('sha', 'unknown'), 'declarations': claims,
                       'claim_limit': 'markers do not establish independent review validity or resolved findings'})
        try:
            report['required_checks'] = api.required(pr)
            report['checks_state'] = ('success' if all(c['bucket'] == 'pass' for c in report['required_checks'])
                                      else 'pending_or_failed')
        except EvidenceError as e:
            unknown.append(f'required checks: {e.kind}')
        if api.api(f'pulls/{pr}')['head']['sha'] != head:
            unknown.append('PR head changed during inspection; refresh required')
    refreshed = api.api(f'issues/{number}')
    if any(refreshed.get(k) != issue.get(k) for k in ('updated_at', 'state', 'labels', 'assignees', 'body')):
        unknown.append('issue changed during inspection; refresh required')
    report['result'] = 'gaps' if gaps else 'unknown' if unknown else 'mechanical_evidence_only'
    report['handoff'] = 'human must confirm local verification, review validity and acceptance; never auto merge-ready'
    return report


def cleanup(api, number, apply=False):
    before = api.api(f'issues/{number}')
    labels = sorted(TRIAGE & {x['name'] for x in before.get('labels', [])})
    if before['state'] != 'closed':
        return {'result': 'refused', 'reason': 'issue is open', 'removed': []}
    if not apply:
        return {'result': 'dry_run', 'would_remove': labels}
    removed = []
    try:
        for label in labels:
            # Refresh before each narrow DELETE. No PATCH of labels/body/state/closing reason.
            current = api.api(f'issues/{number}')
            if current['state'] != 'closed' or current.get('state_reason') != before.get('state_reason'):
                return {'result': 'race', 'removed': removed, 'reason': 'state/closing reason changed'}
            if label not in {x['name'] for x in current.get('labels', [])}:
                continue
            api.api(f'issues/{number}/labels/{quote(label, safe="")}', method='DELETE')
            removed.append(label)
        after = api.api(f'issues/{number}')
        if after['state'] != 'closed' or after.get('state_reason') != before.get('state_reason'):
            return {'result': 'race', 'removed': removed, 'reason': 'post-write state changed; manual reconciliation'}
    except EvidenceError as e:
        return {'result': e.kind, 'reason': str(e), 'removed': removed, 'recheck_required': True}
    return {'result': 'applied', 'removed': removed}


def git(*args):
    r = process(['git', *args], cwd=ROOT)
    if r.returncode:
        raise EvidenceError('error', 'git operation failed; verify explicit base/head')
    return r.stdout


def commit(ref):
    if ref.startswith('-'):
        raise EvidenceError('error', 'invalid revision')
    return git('rev-parse', '--verify', f'{ref}^{{commit}}').strip()


def classify(path, content, primary_docs=False):
    p = path.lower()
    if p.endswith(('package-lock.json', 'cargo.lock')):
        return 'generated'
    if '/vendor/' in p:
        return 'unknown'  # Vendored files may contain hand-maintained production patches.
    if p.endswith(('.md', '.rst')) or p.startswith('.agents/'):
        return 'implementation' if primary_docs else 'documentation'
    if p.endswith('.rs') and (re.search(r'#\s*\[\s*cfg(?:_attr)?\s*\([^\n]*\btest\b', content) or '#[test]' in content):
        return 'unknown'  # Whole file requires content-level review; never guess hunk ownership.
    if (p.startswith(('tests/', 'python/tests/', 'apps/web/e2e/')) or '/tests/' in p
            or Path(p).name.startswith('test_') or p.endswith(('.spec.ts', '.test.ts'))):
        return 'tests_candidate'
    return 'implementation'


def size(base, head, primary_docs=False, estimate_upper=None, hard_budget=None):
    base, head = commit(base), commit(head)
    totals = dict.fromkeys(['implementation', 'tests_candidate', 'documentation', 'generated', 'unknown'], 0)
    files = []
    records = git('diff', '--numstat', '-z', '--no-renames', base, head, '--').split('\0')
    for row in filter(None, records):
        added, deleted, path = row.split('\t', 2)
        if added == '-' or deleted == '-':
            files.append({'path': path, 'category': 'unknown', 'lines': None})
            continue
        count = int(added) + int(deleted)
        # Only tracked changed source at pinned revisions; no recursive research-directory scan.
        content = ''
        if path.endswith('.rs'):
            for rev in (base, head):
                r = process(['git', 'show', f'{rev}:{path}'], cwd=ROOT)
                if r.returncode == 0:
                    content += r.stdout
        category = classify(path, content, primary_docs)
        totals[category] += count
        files.append({'path': path, 'category': category, 'lines': count})
    return {'base': base, 'head': head, 'totals': totals, 'total': sum(totals.values()), 'files': files,
            'binary_count': sum(f['lines'] is None for f in files),
            'classification_limit': 'test paths are candidates, not exemptions; unknown requires content review',
            'review_800': totals['implementation'] + totals['unknown'] >= 800,
            'review_2000': sum(totals.values()) >= 2000,
            'first_estimate_upper': estimate_upper,
            'review_estimate_x2': None if estimate_upper is None else sum(totals.values()) >= 2 * estimate_upper,
            'explicit_hard_budget': hard_budget,
            'hard_budget_exceeded': None if hard_budget is None or any(f['lines'] is None for f in files) else sum(totals.values()) > hard_budget}


def affected(paths):
    """Conservative browser applicability; unknown code paths run rather than silently skip."""
    exact = {'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain'}
    prefixes = ('apps/web/', 'crates/', 'src/', '.cargo/', '.github/workflows/', 'scripts/',
                'poc/vendor/', 'python/', 'tests/')
    return any(p in exact or p.startswith(prefixes) or p.endswith(('.rs', '.toml')) for p in paths)


def gate(results, web_required):
    expected = {'core'} | ({'web'} if web_required else set())
    failures = [name for name in sorted(expected) if results.get(name) != 'success']
    # Only an explicit false applicability decision authorizes web skip.
    if not web_required and results.get('web') not in ('success', 'skipped'):
        failures.append('web unexpected result')
    return {'result': 'failed' if failures else 'success', 'gaps': failures,
            'web_applicability': 'required' if web_required else 'not_applicable_by_paths'}


def counts(text, kind):
    if kind == 'cargo':
        rows = re.findall(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)
        if not rows:
            return None
        passed, failed, ignored = map(sum, zip(*(map(int, r) for r in rows)))
        return {'executed': passed + failed, 'passed': passed, 'failed': failed, 'skipped': ignored}
    if kind == 'unittest':
        rows = re.findall(r'^Ran (\d+) tests? in ', text, re.M)
        if len(rows) != 1:
            return None
        ran = int(rows[0])
        skipped = re.search(r'^(?:OK|FAILED).*?skipped=(\d+)', text, re.M)
        failures = re.findall(r'(?:failures|errors)=(\d+)', text)
        skipped = int(skipped[1]) if skipped else 0
        failed = sum(map(int, failures))
        return {'executed': ran - skipped, 'passed': ran - skipped - failed, 'failed': failed, 'skipped': skipped}
    passed = re.findall(r'^\s*(\d+) passed(?:\s|$)', text, re.M)
    failed = re.findall(r'^\s*(\d+) failed(?:\s|$)', text, re.M)
    skipped = re.findall(r'^\s*(\d+) skipped(?:\s|$)', text, re.M)
    if not (passed or failed or skipped):
        return None
    p, f, s = (sum(map(int, x)) for x in (passed, failed, skipped))
    return {'executed': p + f, 'passed': p, 'failed': f, 'skipped': s}


def runtime_version(command):
    try:
        result = process(command)
        return result.stdout.splitlines()[0] if result.returncode == 0 and result.stdout.strip() else 'unknown'
    except EvidenceError:
        return 'unavailable'


def record(args):
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        raise EvidenceError('error', 'explicit test command required')
    log = Path(args.log).resolve()
    if log.is_relative_to(ROOT):
        raise EvidenceError('error', 'raw test logs must be outside the repository')
    source_sha = commit('HEAD')
    dirty_at_start = bool(git('status', '--porcelain', '--untracked-files=normal').strip())
    start = time.monotonic()
    stopped_reason = None
    minimum_free = shutil.disk_usage(ROOT).free
    # Exclusive creation preserves prior evidence; child output stays local.
    with log.open('x') as stream:
        if minimum_free < DISK_RESERVE:
            exit_code, stopped_reason = 125, 'disk_reserve_before_start'
        else:
            try:
                owned = OwnedRun(command, ROOT,
                                 {**os.environ, 'NO_COLOR': '1', 'CARGO_TERM_COLOR': 'never'}, stream)
                child = owned.child
                try:
                    deadline = start + args.timeout
                    while child.poll() is None:
                        minimum_free = min(minimum_free, shutil.disk_usage(ROOT).free)
                        if minimum_free < DISK_RESERVE:
                            stopped_reason = 'disk_reserve_during_run'
                            break
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            stopped_reason = 'timeout'
                            break
                        try:
                            child.wait(timeout=min(1, remaining))
                        except subprocess.TimeoutExpired:
                            pass
                    exit_code = (124 if stopped_reason == 'timeout' else 125) if stopped_reason else child.returncode
                finally:
                    # Cleanup is independent of leader exit and covers nested recorders/sessions.
                    owned.close()
            except (OSError, ProcessControlError):
                exit_code, stopped_reason = 126, 'ownership_or_cleanup_unverified'
    parsed = counts(log.read_text(errors='replace'), args.kind)
    valid = bool(parsed) and parsed['failed'] == 0 and (parsed['executed'] > 0 or bool(args.allow_empty))
    if parsed and args.kind != 'cargo' and parsed['skipped'] and not args.allow_skips:
        valid = False
    ending_sha = commit('HEAD')
    dirty_at_end = bool(git('status', '--porcelain', '--untracked-files=normal').strip())
    result = 'passed' if valid and exit_code == 0 else 'failed'
    if source_sha != ending_sha:
        result = 'source_changed'
    report = {'result': result, 'command': command,
              'sha': source_sha, 'ending_sha': ending_sha, 'executor': args.executor, 'selection': args.selection,
              'working_tree_dirty': dirty_at_start, 'dirty_at_end': dirty_at_end,
              'source_verification': 'clean_stable' if source_sha == ending_sha and not (dirty_at_start or dirty_at_end) else 'manual_mapping_required',
              'environment': {'platform': sys.platform, 'recorder_python': sys.version.split()[0],
                              'node': runtime_version(['node', '--version']),
                              'rust': runtime_version(['rustc', '--version']),
                              'browser_executable': os.environ.get('PRAJNA_CHROME_EXECUTABLE', 'unverified')},
              'counts': parsed, 'exit_code': exit_code, 'seconds': round(time.monotonic()-start, 3),
              'stopped_reason': stopped_reason, 'minimum_free_gib': round(minimum_free/1024**3, 2),
              'allow_empty_reason': args.allow_empty, 'allow_skips_reason': args.allow_skips,
              'ignored_policy': 'cargo ignored are recorded; positive executed count still required'}
    if args.output:
        with Path(args.output).open('x') as stream:
            json.dump(report, stream, indent=2)
    return report


def process_visibility():
    child = None
    try:
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(10)'],
                                 start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        r = process(['ps', '-axo', 'pid=,pgid='])
        pairs = {int(pid): int(pgid) for pid, pgid in re.findall(r'^\s*(\d+)\s+(\d+)\s*$', r.stdout, re.M)}
        return r.returncode == 0 and os.getpid() in pairs and pairs.get(child.pid) == child.pid
    except (OSError, EvidenceError):
        return False
    finally:
        if child is not None:
            child.terminate()
            child.wait(timeout=5)


def preflight(port, browser):
    checks = {}
    for name, command in [('node', ['node', '--version']), ('python', ['python3.12', '--version']),
                          ('rust', ['rustc', '--version']), ('npm', ['npm', '--version']), ('gh', ['gh', '--version'])]:
        try:
            r = process(command)
            value = r.stdout.splitlines()[0] if r.returncode == 0 and r.stdout.strip() else 'unavailable'
            checks[name] = value
        except EvidenceError as e:
            checks[name] = e.kind
    m = re.search(r'v(\d+)\.', checks['node'])
    checks['node_20_plus'] = bool(m and int(m[1]) >= 20)
    checks['rust_available'] = bool(re.match(r'^rustc \d+\.', checks['rust']))
    checks['npm_available'] = bool(re.match(r'^\d+\.\d+', checks['npm']))
    checks['gh_scope'] = 'informational; required by inspect/cleanup, not local browser validation'
    checks['python_3_12'] = checks['python'].startswith('Python 3.12.')
    checks['python_stdlib'] = runtime_version(['python3.12', '-c', 'import pyexpat, ssl, venv; print("ok")']) == 'ok'
    checks['process_groups'] = process_visibility()
    try:
        with socket.socket() as s:
            s.bind(('127.0.0.1', port))
        checks['loopback_port'] = 'available_at_check_time'
    except OSError:
        checks['loopback_port'] = 'unavailable_or_denied'
    checks['free_gib'] = round(shutil.disk_usage(ROOT).free / (1024**3), 1)
    checks['disk_reserve_10gib'] = checks['free_gib'] >= 10
    executable = browser or os.environ.get('PRAJNA_CHROME_EXECUTABLE')
    checks['browser'] = 'explicit_executable_found' if executable and os.access(executable, os.X_OK) else 'unverified'
    checks['coverage_warning'] = 'no-default-features excludes quant-research app reading tests; map CI features explicitly'
    checks['result'] = 'capable_pending_behavior_test' if all((checks['rust_available'], checks['npm_available'], checks['node_20_plus'], checks['python_3_12'], checks['python_stdlib'],
        checks['process_groups'], checks['disk_reserve_10gib'], checks['browser'] != 'unverified',
        checks['loopback_port'] == 'available_at_check_time')) else 'gaps'
    return checks


def timings(data):
    def seconds(start, end):
        if not start or not end:
            return None
        return (dt.datetime.fromisoformat(end.replace('Z', '+00:00'))-
                dt.datetime.fromisoformat(start.replace('Z', '+00:00'))).total_seconds()
    return [{'job': j['name'], 'wall_seconds': seconds(j.get('started_at'), j.get('completed_at')),
             'steps': [{'number': s['number'], 'name': s['name'], 'conclusion': s.get('conclusion'),
                        'seconds': seconds(s.get('started_at'), s.get('completed_at'))} for s in j.get('steps', [])]}
            for j in data['jobs']]


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest='mode', required=True)
    for name in ('inspect', 'cleanup'):
        q = sub.add_parser(name)
        q.add_argument('--repo', type=repo_name, required=True)
        q.add_argument('--issue', type=positive, required=True)
        if name == 'inspect':
            q.add_argument('--actor', required=True)
            q.add_argument('--pr', type=positive)
        else:
            q.add_argument('--apply', action='store_true')
    for name in ('size', 'paths'):
        q = sub.add_parser(name)
        q.add_argument('--base', required=True)
        q.add_argument('--head', required=True)
        if name == 'size':
            q.add_argument('--primary-docs', action='store_true')
            q.add_argument('--estimate-upper', type=positive, help='first approved upper; never a rolling estimate')
            q.add_argument('--hard-budget', type=positive, help='only an explicitly authorized fixed-scope hard budget')
    q = sub.add_parser('gate')
    q.add_argument('--core', required=True)
    q.add_argument('--web', required=True)
    q.add_argument('--web-required', choices=['true', 'false'], required=True)
    q = sub.add_parser('preflight')
    q.add_argument('--port', type=positive, default=18789)
    q.add_argument('--browser')
    q = sub.add_parser('run')
    q.add_argument('--kind', choices=['cargo', 'unittest', 'playwright'], required=True)
    q.add_argument('--executor', required=True)
    q.add_argument('--selection', required=True)
    q.add_argument('--log', required=True)
    q.add_argument('--output')
    q.add_argument('--timeout', type=positive, default=1200)
    q.add_argument('--allow-empty', metavar='REASON')
    q.add_argument('--allow-skips', metavar='REASON')
    q.add_argument('command', nargs=argparse.REMAINDER)
    q = sub.add_parser('timings')
    q.add_argument('--jobs-json', required=True)
    return p


def main():
    args = parser().parse_args()
    try:
        if args.mode == 'inspect':
            report = inspect(GitHub(args.repo), args.issue, args.actor, args.pr)
        elif args.mode == 'cleanup':
            report = cleanup(GitHub(args.repo), args.issue, args.apply)
        elif args.mode == 'size':
            report = size(args.base, args.head, args.primary_docs, args.estimate_upper, args.hard_budget)
        elif args.mode == 'paths':
            paths = git('diff', '--name-only', '-z', '--no-renames', commit(args.base), commit(args.head), '--').split('\0')
            report = {'web_required': affected(list(filter(None, paths))), 'files': len(list(filter(None, paths)))}
        elif args.mode == 'gate':
            report = gate({'core': args.core, 'web': args.web}, args.web_required == 'true')
        elif args.mode == 'run':
            report = record(args)
        elif args.mode == 'preflight':
            report = preflight(args.port, args.browser)
        else:
            report = {'jobs': timings(json.loads(Path(args.jobs_json).read_text()))}
        json_output(report)
        if report.get('result') in ('unknown', 'permission', 'error', 'unavailable', 'source_changed'):
            return 2
        return 0 if report.get('result') in (None, 'success', 'passed', 'dry_run', 'applied',
                                             'mechanical_evidence_only', 'capable_pending_behavior_test') else 1
    except EvidenceError as e:
        json_output({'result': e.kind, 'reason': str(e)})
        return 2
    except (OSError, ValueError, KeyError, TypeError) as e:
        json_output({'result': 'error', 'reason': type(e).__name__})
        return 2


if __name__ == '__main__':
    sys.exit(main())
