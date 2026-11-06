#!/usr/bin/env python3
"""Build a local macOS arm64 RC; never create a tag or publish a release."""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request

from rc_runtime import DEMO_CONFIGS, DEMO_PREFIX, REQUIRED_BINS, RcError, digest, git_env


ROOT = Path(__file__).resolve().parents[1]
TARGET = 'aarch64-apple-darwin'
GITHUB_REPOSITORY = 'ZombieIce/PrajnaQuant'
SOURCE_PATTERNS = (
    '/.gitignore', '/Cargo.toml', '/Cargo.lock', '/src/',
    '/crates/*/Cargo.toml', '/crates/*/build.rs', '/crates/*/src/',
    '/crates/*/tests/', '/crates/*/examples/', '/poc/vendor/polars-io/',
    '/apps/web/src/', '/apps/web/index.html', '/apps/web/package.json',
    '/apps/web/package-lock.json', '/apps/web/tsconfig*.json', '/apps/web/vite.config.*',
    '/scripts/rc_*.py', '/scripts/workflow_process.py',
)
CONTRACTS = (
    'dataset-preview-read.md', 'shanghai-shenzhen-session-calendar.md',
    'shanghai-shenzhen-daily-normalizer.md', 'adjusted-research-series.md',
    'research-pools.md', 'pool-updates.md', 'evaluation-window.md',
    'momentum-research.md', 'single-security-selection.md', 'experiment-result-read.md',
)
DECISIONS = (
    '0030-supplier-adjusted-research-series.md', '0031-common-evaluation-window-and-adjusted-binding.md',
    '0032-cn-corporate-action-fact-versions.md', '0033-static-corporate-action-ledger.md',
    '0034-holding-benchmark-execution.md', '0035-strategy-benchmark-comparison.md',
    '0036-versioned-complete-performance-summary.md', '0037-authoritative-calendar-report-and-performance-page.md',
    '0038-fixed-pool-production-update.md',
)
DATA_BINS = (
    'prajna-preview-dataset', 'prajna-publish-daily', 'prajna-import-adjusted',
    'prajna-import-daily-price', 'prajna-corporate-facts', 'prajna-collect-daily-raw',
    'prajna-session-calendar', 'prajna-select-security-evidence',
)


def command(args, cwd=ROOT, env=None, log=None, *, stdout_only=False):
    if log is not None:
        log.write(f'\n$ {shlex.join(str(arg) for arg in args)}\n')
        log.flush()
    try:
        result = subprocess.run(args, cwd=cwd, env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE if stdout_only else subprocess.STDOUT, text=True)
    except OSError as error:
        raise RcError(f'Cannot run {args[0]}: {error}') from error
    if log is not None:
        log.write(result.stdout)
        if result.stderr:
            log.write(result.stderr)
        log.flush()
    if result.returncode:
        diagnostic = 'see build log' if log is not None else (result.stderr or result.stdout).strip()
        raise RcError(f'{args[0]} exited {result.returncode}; {diagnostic or "no diagnostic output"}')
    return result.stdout


def tool_version(args, log):
    value = command(args, log=log, stdout_only=True).strip()
    if not value or len(value.splitlines()) != 1:
        raise RcError(f'{args[0]} returned an invalid version')
    return value


def build_versions(log):
    return {
        'rustc_version': tool_version(['rustc', '--version'], log),
        'cargo_version': tool_version(['cargo', '--version'], log),
        'git_version': tool_version(['git', '--version'], log),
        'node_version': tool_version(['node', '--version'], log),
        'npm_version': tool_version(['npm', '--version'], log),
        'python_version': platform.python_version(),
        'macos_version': tool_version(['sw_vers', '-productVersion'], log),
    }


def git(repo, *args):
    return subprocess.check_output(['git', *args], cwd=repo, env=git_env())


def remote_tag_exists(repo, version):
    remotes = git(repo, 'remote').decode().splitlines()
    if 'origin' not in remotes:
        raise RcError('cannot verify candidate version: Git origin remote is not configured')
    try:
        result = subprocess.run(
            ['git', 'ls-remote', '--exit-code', '--refs', '--tags', 'origin', f'refs/tags/{version}'],
            cwd=repo, env=git_env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RcError(f'cannot verify candidate version against origin tags: {error}') from error
    if result.returncode == 0:
        return bool(result.stdout.strip())
    if result.returncode == 2:
        return False
    detail = result.stderr.strip() or f'git ls-remote exited {result.returncode}'
    raise RcError(f'cannot verify candidate version against origin tags: {detail}')


def remote_release_exists(version):
    url = f'https://api.github.com/repos/{GITHUB_REPOSITORY}/releases/tags/{version}'
    request = urllib.request.Request(url, headers={
        'Accept': 'application/vnd.github+json',
        'User-Agent': 'PrajnaQuant-RC-builder',
    })
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            release = json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise RcError(f'cannot verify candidate version against GitHub releases: HTTP {error.code}') from error
    except (OSError, TimeoutError, json.JSONDecodeError) as error:
        raise RcError(f'cannot verify candidate version against GitHub releases: {error}') from error
    if not isinstance(release, dict) or release.get('tag_name') != version:
        raise RcError('GitHub release lookup returned an unexpected response')
    return True


def source_snapshot(repo, destination, temporary, revision):
    bare = temporary / 'source.git'
    environment = git_env()
    command(['git', 'clone', '--quiet', '--bare', '--depth=1', '--no-local',
             repo.as_uri(), str(bare)], env=environment)
    command(['git', f'--git-dir={bare}', 'config', 'uploadpack.allowFilter', 'true'], env=environment)
    command(['git', 'clone', '--quiet', '--filter=blob:none', '--depth=1', '--no-checkout',
             '--no-local', bare.as_uri(), str(destination)], env=environment)
    command(['git', 'sparse-checkout', 'init', '--no-cone'], cwd=destination, env=environment)
    command(['git', 'sparse-checkout', 'set', '--no-cone', *SOURCE_PATTERNS],
            cwd=destination, env=environment)
    command(['git', 'checkout', '--quiet', '--detach'], cwd=destination, env=environment)
    command(['git', 'remote', 'remove', 'origin'], cwd=destination, env=environment)
    if git(destination, 'rev-parse', 'HEAD').decode().strip() != revision:
        raise RcError('source snapshot does not match the candidate commit')
    if git(destination, 'status', '--porcelain=v1', '--untracked-files=all'):
        raise RcError('source snapshot is not clean')

    selected = {
        entry[2:] for entry in git(destination, 'ls-files', '-t', '-z').split(b'\0')
        if entry and not entry.startswith(b'S ')
    }
    allowed = set()
    for entry in git(destination, 'ls-tree', '-r', '-z', 'HEAD').split(b'\0'):
        if not entry:
            continue
        header, path = entry.split(b'\t', 1)
        _mode, kind, object_id = header.split(b' ')
        if kind == b'blob' and path in selected:
            allowed.add(object_id)
    objects = git(destination, 'cat-file', '--batch-all-objects', '--batch-check=%(objectname) %(objecttype)')
    present = {line.split()[0] for line in objects.splitlines() if line.endswith(b' blob')}
    if present - allowed:
        raise RcError('source snapshot contains excluded Git blobs')


def portable_values(value, root, preview_input=None):
    if isinstance(value, dict):
        return {key: portable_values(item, root, preview_input) for key, item in value.items()}
    if isinstance(value, list):
        return [portable_values(item, root, preview_input) for item in value]
    if isinstance(value, str) and value.startswith('/'):
        path = Path(value).resolve()
        if preview_input is not None and (path == preview_input or path.is_relative_to(preview_input)):
            relative = Path('synthetic-input/preview') / path.relative_to(preview_input)
        elif path != root and path.is_relative_to(root):
            relative = path.relative_to(root)
        else:
            raise RcError('synthetic configuration references a path outside its fixture')
        return DEMO_PREFIX + relative.as_posix()
    return value


def portable_demo(root, preview_input=None, build_root=None):
    build_path = build_root or root
    needles = {str(build_path).encode(), str(build_path.resolve()).encode()}
    root = root.resolve()
    if preview_input is not None:
        preview_input = preview_input.resolve()
        shutil.copytree(preview_input, root / 'synthetic-input/preview')
    for name in DEMO_CONFIGS:
        path = root / name
        if not path.is_file():
            raise RcError(f'missing synthetic configuration: {name}')
        value = portable_values(json.loads(path.read_text()), root, preview_input)
        path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    for path in root.rglob('*'):
        if path.is_symlink():
            raise RcError('synthetic fixture contains a symbolic link')
        if path.is_file():
            contents = path.read_bytes()
            if any(needle in contents for needle in needles):
                raise RcError(f'synthetic fixture retains its build path: {path.relative_to(root)}')


def export_test(package, test, name, variable, root, log, environment):
    environment = {**environment, variable: str(root)}
    args = ['cargo', 'test', '-p', package, '--test', test, '--release', '--target', TARGET, '--locked', '--offline',
            name, '--', '--ignored', '--exact']
    result = command(args, env=environment, log=log)
    if not re.search(r'test result: ok\. 1 passed; 0 failed;', result):
        raise RcError(f'{name} did not execute exactly one fixture test')


def prepare_demo(temporary, release, log, environment):
    fixture = temporary / 'fixture'
    lake = fixture / 'lake'
    export_test('prajna-data', 'daily_price_import', 'generate_e2e_input',
                'PRAJNA_DAILY_PRICE_FIXTURE_ROOT', lake / 'synthetic-input/daily-price',
                log, environment)
    export_test('prajna-data', 'dataset_preview_read', 'export_synthetic_preview_fixture',
                'PRAJNA_PREVIEW_FIXTURE_ROOT', fixture, log, environment)
    command([str(release / 'examples/selection_fixture'), str(lake)], log=log)
    export_test('prajna-experiment', 'pools', 'export_synthetic_pool_fixture',
                'PRAJNA_POOL_FIXTURE_ROOT', lake, log, environment)
    export_test('prajna-experiment', 'momentum', 'export_momentum_fixture',
                'PRAJNA_MOMENTUM_FIXTURE_ROOT', lake, log, environment)
    export_test('prajna-experiment', 'pool_update', 'export_pool_update_fixture',
                'PRAJNA_POOL_UPDATE_FIXTURE_ROOT', lake, log, environment)
    facts = temporary / 'corporate-actions'
    export_test('prajna-data', 'corporate_facts', 'export_synthetic_corporate_facts_fixture',
                'PRAJNA_CORPORATE_FACTS_FIXTURE_ROOT', facts, log, environment)
    for name in ('corporate-actions.json', 'corporate-actions-revision.json'):
        command([str(release / 'prajna-corporate-facts'), 'import',
                 '--root', str(lake), '--input', str(facts / name)], log=log)
    portable_demo(lake, fixture / 'input', temporary)
    return lake


def inventory(root, prefixes=None):
    result = {}
    for path in sorted(root.rglob('*')):
        if path.is_file():
            relative = path.relative_to(root).as_posix()
            if prefixes is None or any(relative.startswith(prefix) for prefix in prefixes):
                result[relative] = {'sha256': digest(path), 'bytes': path.stat().st_size}
    return result


def build(output, version):
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RcError('this candidate is limited to macOS arm64')
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+', version):
        raise RcError('version must have the form vN.N.N-rc.N')
    if not output.is_absolute() or output == ROOT or output.is_relative_to(ROOT):
        raise RcError('output must be an absolute directory outside the checkout')
    if output.exists() and (not output.is_dir() or any(output.iterdir())):
        raise RcError('output directory must be empty')
    output.mkdir(parents=True, exist_ok=True)
    log_path = output / 'build.log'
    with log_path.open('x') as log:
        status = git(ROOT, 'status', '--porcelain=v1', '--untracked-files=all')
        if status:
            raise RcError('candidate source worktree must be clean')
        revision = git(ROOT, 'rev-parse', 'HEAD').decode().strip()
        if git(ROOT, 'tag', '--list', version).strip():
            raise RcError('candidate version is already a local Git tag')
        if remote_tag_exists(ROOT, version):
            raise RcError('candidate version is already an origin Git tag')
        if remote_release_exists(version):
            raise RcError('candidate version already has a GitHub release')
        versions = build_versions(log)
        rustc_path = shutil.which('rustc')
        if not rustc_path:
            raise RcError('rustc is not available on PATH')
        build_env = {**os.environ, 'RUSTC': rustc_path, 'RUSTC_WRAPPER': '', 'RUSTC_WORKSPACE_WRAPPER': ''}
        log.write(f'\nUsing RUSTC={rustc_path}; compiler wrappers disabled; target={TARGET}\n')
        target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).resolve()
        release = target / TARGET / 'release'
        with tempfile.TemporaryDirectory(prefix='.prajna-rc-build-', dir=output) as temp:
            temporary = Path(temp)
            bundle = temporary / f'prajna-quant-{version}-macos-arm64'
            bundle.mkdir()
            print('Building production frontend and Rust release binaries...', flush=True)
            command(['npm', 'run', 'build'], cwd=ROOT / 'apps/web', log=log)
            command(['cargo', 'build', '-p', 'quant-research', '--bin', 'quant-research',
                     '--release', '--target', TARGET, '--locked', '--offline'], log=log, env=build_env)
            command(['cargo', 'build', '-p', 'prajna-experiment', '--bins', '--example',
                     'selection_fixture', '--release', '--target', TARGET, '--locked', '--offline'],
                    log=log, env=build_env)
            if not set(DATA_BINS) <= set(REQUIRED_BINS):
                raise RcError('a production data CLI is not in the release inventory')
            args = ['cargo', 'build', '-p', 'prajna-data']
            for name in DATA_BINS:
                args.extend(['--bin', name])
            command([*args, '--release', '--target', TARGET, '--locked', '--offline'], log=log, env=build_env)
            print('Exporting and checking synthetic inputs...', flush=True)
            lake = prepare_demo(temporary, release, log, build_env)
            (bundle / 'bin').mkdir()
            for name in REQUIRED_BINS:
                shutil.copy2(release / name, bundle / 'bin' / name)
                architecture = command(['lipo', '-archs', str(bundle / 'bin' / name)], log=log).split()
                if architecture != ['arm64']:
                    raise RcError(f'{name} is not an arm64-only executable')
            shutil.copytree(ROOT / 'apps/web/dist', bundle / 'web')
            shutil.copytree(lake, bundle / 'demo/lake-template')
            (bundle / 'lib').mkdir()
            for name in ('rc_runtime.py', 'workflow_process.py'):
                shutil.copy2(ROOT / 'scripts' / name, bundle / 'lib' / name)
            (bundle / 'docs').mkdir()
            for name in ('user-guide.md', 'release-notes-rc.md'):
                shutil.copy2(ROOT / 'docs' / name, bundle / 'docs' / name)
            (bundle / 'docs/contracts').mkdir()
            for name in CONTRACTS:
                shutil.copy2(ROOT / 'docs/contracts' / name, bundle / 'docs/contracts' / name)
            (bundle / 'docs/decisions').mkdir()
            for name in DECISIONS:
                shutil.copy2(ROOT / 'docs/decisions' / name, bundle / 'docs/decisions' / name)
            (bundle / 'docs/licenses').mkdir()
            shutil.copy2(ROOT / 'LICENSE', bundle / 'LICENSE')
            shutil.copy2(ROOT / 'poc/vendor/polars-io/LICENSE', bundle / 'docs/licenses/polars-io')
            launcher = bundle / 'prajna'
            launcher.write_text('#!/bin/sh\nset -eu\nhere=$(CDPATH= cd -P "$(dirname "$0")" && pwd)\n'
                                'exec python3 "$here/lib/rc_runtime.py" "$@"\n')
            launcher.chmod(0o755)
            source_snapshot(ROOT, bundle / 'source', temporary, revision)
            files = inventory(bundle, ('bin/', 'web/', 'lib/', 'demo/', 'docs/', 'prajna', 'LICENSE'))
            metadata = {
                'version': version, 'source_commit': revision, 'target': TARGET,
                **versions,
                'files': {name: item['sha256'] for name, item in files.items()},
            }
            (bundle / 'release.json').write_text(json.dumps(metadata, indent=2) + '\n')
            listing = inventory(bundle)
            (bundle / 'release-files.json').write_text(json.dumps(listing, indent=2) + '\n')
            command(['otool', '-L', *(str(bundle / 'bin' / name) for name in REQUIRED_BINS)], log=log)
            if git(ROOT, 'rev-parse', 'HEAD').decode().strip() != revision or git(
                    ROOT, 'status', '--porcelain=v1', '--untracked-files=all'):
                raise RcError('source changed during the release build')
            archive = output / f'{bundle.name}.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                def normalized(info):
                    info.uid = info.gid = 0
                    info.uname = info.gname = ''
                    return info
                tar.add(bundle, arcname=bundle.name, filter=normalized)
            checksum = digest(archive)
            (output / f'{archive.name}.sha256').write_text(f'{checksum}  {archive.name}\n')
            print(f'Candidate archive: {archive}\nSHA-256: {checksum}\nBuild log: {log_path}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--version', default='v0.1.0-rc.1')
    options = parser.parse_args()
    try:
        build(options.output.resolve(), options.version)
    except (RcError, OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f'RC build failed: {error}', file=sys.stderr)
        sys.exit(1)
