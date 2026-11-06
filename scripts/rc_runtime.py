#!/usr/bin/env python3
"""Run one local RC from its own bundle and an explicit, independent data root."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

from workflow_process import OwnedRun, ProcessControlError


REQUIRED_BINS = (
    'quant-research', 'prajna-experiment', 'prajna-pools',
    'prajna-freeze-selection', 'prajna-freeze-comparison', 'prajna-momentum',
    'prajna-pool-update', 'prajna-preview-dataset', 'prajna-publish-daily',
    'prajna-import-adjusted', 'prajna-import-daily-price', 'prajna-corporate-facts',
    'prajna-collect-daily-raw', 'prajna-session-calendar',
    'prajna-select-security-evidence',
)
CLI_ENV = {
    'PRAJNA_EXPERIMENT_CLI': 'prajna-experiment',
    'PRAJNA_POOLS_CLI': 'prajna-pools',
    'PRAJNA_SELECTION_CLI': 'prajna-freeze-selection',
    'PRAJNA_COMPARISON_CLI': 'prajna-freeze-comparison',
    'PRAJNA_MOMENTUM_CLI': 'prajna-momentum',
    'PRAJNA_POOL_UPDATE_CLI': 'prajna-pool-update',
    'PRAJNA_DATASET_PREVIEW_CLI': 'prajna-preview-dataset',
}
DEMO_PREFIX = '@DEMO_ROOT@/'
DEMO_CONFIGS = (
    'pool-updates/configs/synthetic.json',
    'synthetic-input/XSHG/selection.json',
    'synthetic-input/XSHE/selection.json',
    'synthetic-input/daily-price/config.json',
    'etf.json-selection.json',
    'stock.json-selection.json',
)


class RcError(Exception):
    pass


def git_env():
    return {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}


def output(command, cwd=None, env=None):
    try:
        result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RcError(f'Cannot run {command[0]}: {error}') from error
    if result.returncode:
        raise RcError(f'{command[0]} failed: {result.stderr.strip()}')
    return result.stdout.strip()


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def bundle_metadata(bundle):
    try:
        metadata = json.loads((bundle / 'release.json').read_text())
        files = metadata['files']
        if not isinstance(files, dict):
            raise ValueError('files must be an object')
        required = (
            {f'bin/{name}' for name in REQUIRED_BINS}
            | {f'demo/lake-template/{name}' for name in DEMO_CONFIGS}
            | {'web/index.html', 'lib/rc_runtime.py', 'lib/workflow_process.py', 'prajna'}
        )
        if not required <= files.keys():
            raise ValueError('required files are absent from release metadata')
        for name, expected in files.items():
            relative = PurePosixPath(name)
            if relative.is_absolute() or '..' in relative.parts:
                raise ValueError('invalid bundle file path')
            path = (bundle / relative).resolve(strict=True)
            if not path.is_relative_to(bundle) or not path.is_file():
                raise ValueError(f'file outside bundle: {name}')
            if digest(path) != expected:
                raise ValueError(f'checksum mismatch: {name}')
        for name in REQUIRED_BINS:
            if not os.access(bundle / 'bin' / name, os.X_OK):
                raise ValueError(f'not executable: {name}')
        source = (bundle / 'source').resolve(strict=True)
        if not source.is_relative_to(bundle):
            raise ValueError('source snapshot is outside bundle')
        environment = git_env()
        top = Path(output(['git', 'rev-parse', '--show-toplevel'], source, environment)).resolve()
        if top != source:
            raise ValueError('source snapshot is not its own Git worktree')
        revision = output(['git', 'rev-parse', 'HEAD'], source, environment)
        if revision != metadata['source_commit']:
            raise ValueError('source snapshot revision does not match release')
        if output(['git', 'status', '--porcelain=v1', '--untracked-files=all'], source, environment):
            raise ValueError('source snapshot is dirty')
        if output(['rustc', '--version']) != metadata['rustc_version']:
            raise ValueError('runtime rustc version does not match the release build')
        output(['ps', '-p', str(os.getpid()), '-o', 'pid='])
        return metadata
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        raise RcError(f'Invalid release bundle: {error}') from error


def data_root(value, bundle, must_exist=True):
    path = Path(value).expanduser()
    if not path.is_absolute():
        raise RcError('data root must be an absolute path')
    try:
        path = path.resolve(strict=must_exist)
    except OSError as error:
        raise RcError(f'data root is unavailable: {error}') from error
    if (path == Path('/') or path == Path.home().resolve() or path == bundle
            or path.is_relative_to(bundle) or bundle.is_relative_to(path)):
        raise RcError('data root must be independent of the release bundle and home')
    if must_exist and not path.is_dir():
        raise RcError('data root must be an existing directory')
    return path


def demo_paths(value, destination):
    if isinstance(value, dict):
        return {key: demo_paths(item, destination) for key, item in value.items()}
    if isinstance(value, list):
        return [demo_paths(item, destination) for item in value]
    if isinstance(value, str) and value.startswith(DEMO_PREFIX):
        suffix = PurePosixPath(value[len(DEMO_PREFIX):])
        if suffix.is_absolute() or not suffix.parts or '..' in suffix.parts:
            raise RcError('invalid demo path')
        return str(destination.joinpath(*suffix.parts))
    return value


def install_demo(template, destination):
    try:
        if destination.exists() and (not destination.is_dir() or any(destination.iterdir())):
            raise RcError('demo destination must be an empty directory')
        if any(path.is_symlink() for path in template.rglob('*')):
            raise RcError('synthetic demo must not contain symbolic links')
        prepared = {}
        for name in DEMO_CONFIGS:
            config = template / name
            if not config.is_file():
                raise RcError(f'synthetic configuration is missing: {name}')
            prepared[name] = demo_paths(json.loads(config.read_text()), destination)
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copytree(template, destination, dirs_exist_ok=True)
        for name, value in prepared.items():
            (destination / name).write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    except (OSError, ValueError, json.JSONDecodeError) as error:
        raise RcError(f'Cannot install synthetic demo: {error}') from error


def private_state(root):
    directory = root / '.prajna-rc'
    directory.mkdir(mode=0o700, exist_ok=True)
    info = directory.lstat()
    if not directory.is_dir() or directory.is_symlink() or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RcError('control directory must be private and owned by the current user')
    return directory


def write_state(directory, value):
    temp = directory / f'.instance-{value["nonce"]}.tmp'
    descriptor = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as stream:
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temp, directory / 'instance.json')


def read_state(root):
    try:
        state = json.loads((root / '.prajna-rc/instance.json').read_text())
        if not isinstance(state['nonce'], str) or not isinstance(state['socket'], str):
            raise ValueError('invalid control identity')
        return state
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        raise RcError(f'No readable owned service state: {error}') from error


def exchange(state, operation, timeout=15):
    try:
        with socket.socket(socket.AF_UNIX) as control:
            control.settimeout(timeout)
            control.connect(state['socket'])
            control.sendall((json.dumps({'operation': operation, 'nonce': state['nonce']}) + '\n').encode())
            reply = control.recv(4096)
        result = json.loads(reply)
        if result.get('status') == 'error':
            raise RcError(result.get('message', 'service control failed'))
        return result
    except (OSError, ValueError, json.JSONDecodeError) as error:
        raise RcError(f'Owned service control unavailable: {error}') from error


def available_port(port):
    if not 1024 <= port <= 65535:
        raise RcError('port must be between 1024 and 65535')
    try:
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', port))
    except OSError as error:
        raise RcError(f'loopback port {port} is unavailable: {error}') from error


def release_env(bundle):
    environment = git_env()
    for key, name in CLI_ENV.items():
        environment[key] = str(bundle / 'bin' / name)
    return environment


def start(bundle, root, port):
    bundle_metadata(bundle)
    available_port(port)
    directory = private_state(root)
    descriptor = os.open(directory / 'lock', os.O_RDWR | os.O_CREAT | getattr(os, 'O_NOFOLLOW', 0), 0o600)
    child = None
    ready = False
    control_dir = None
    log = None
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RcError('a service already owns this data root') from error
        nonce = secrets.token_hex(32)
        control_dir = Path(tempfile.mkdtemp(prefix='prajna-rc-'))
        log = directory / f'service-{nonce[:12]}.log'
        environment = release_env(bundle)
        environment['PRAJNA_RC_NONCE'] = nonce
        log_fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_APPEND, 0o600)
        with os.fdopen(log_fd, 'ab', buffering=0) as stream:
            child = subprocess.Popen(
                [sys.executable, str(Path(__file__).resolve()), '_supervise', '--data-root', str(root),
                 '--port', str(port), '--control-dir', str(control_dir), '--lock-fd', str(descriptor),
                 '--log', str(log)],
                env=environment, stdin=subprocess.DEVNULL, stdout=stream, stderr=stream,
                pass_fds=(descriptor,), start_new_session=True,
            )
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if child.poll() is not None:
                raise RcError(f'service failed to start; inspect {log}')
            try:
                state = read_state(root)
            except RcError:
                state = None
            if state and state['nonce'] == nonce and state.get('status') == 'ready':
                if exchange(state, 'status').get('status') != 'ready':
                    raise RcError('service readiness could not be verified')
                ready = True
                print(f'Running at http://127.0.0.1:{port} (log: {log})')
                return
            if state and state['nonce'] == nonce and state.get('status') == 'failed':
                raise RcError(f'{state.get("error", "startup failed")}; inspect {log}')
            time.sleep(0.1)
        raise RcError(f'service did not become ready; inspect {log}')
    finally:
        os.close(descriptor)
        if child is not None and not ready and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired as error:
                raise RcError(f'service startup cleanup is unverified; inspect {log}') from error
        if child is None and control_dir is not None:
            control_dir.rmdir()


def supervisor(bundle, root, port, control_dir, lock_fd, log):
    nonce = os.environ['PRAJNA_RC_NONCE']
    directory = private_state(root)
    socket_path = control_dir / 'control.sock'
    state = {'nonce': nonce, 'socket': str(socket_path), 'status': 'starting', 'port': port}
    owned = None
    connection = None
    stopped = False
    failure = None

    def request_stop(_signal, _frame):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGTERM, request_stop)
    signal.signal(signal.SIGINT, request_stop)
    try:
        with socket.socket(socket.AF_UNIX) as server, log.open('ab', buffering=0) as stream:
            server.bind(str(socket_path))
            server.listen(4)
            server.settimeout(0.2)
            write_state(directory, state)
            market = root / 'legacy-market'
            market.mkdir(exist_ok=True)
            command = [
                str(bundle / 'bin/quant-research'), 'serve', '--address', f'127.0.0.1:{port}',
                '--web-dist', str(bundle / 'web'), '--dataset-root', str(root),
                '--output', str(root), '--market-data-dir', str(market),
            ]
            owned = OwnedRun(command, bundle / 'source', release_env(bundle), stream)
            deadline = time.monotonic() + 35
            banner = f'Prajna Quant: http://127.0.0.1:{port}'.encode()
            while not stopped and time.monotonic() < deadline:
                if owned.child.poll() is not None:
                    raise RcError('service exited before readiness')
                if banner in log.read_bytes():
                    try:
                        with urllib.request.urlopen(f'http://127.0.0.1:{port}/api/v1/health', timeout=1) as reply:
                            health = json.load(reply)
                        if health == {'status': 'ok', 'service': 'prajna-quant'} and owned.child.poll() is None:
                            break
                    except (OSError, ValueError, urllib.error.URLError):
                        pass
                time.sleep(0.1)
            else:
                raise RcError('service readiness timed out or was cancelled')
            state['status'] = 'ready'
            write_state(directory, state)
            while not stopped:
                if owned.child.poll() is not None:
                    raise RcError('service exited unexpectedly')
                try:
                    client, _ = server.accept()
                except socket.timeout:
                    continue
                client.settimeout(3)
                try:
                    request = json.loads(client.recv(4096))
                    if request.get('nonce') != nonce or request.get('operation') not in ('status', 'stop'):
                        client.sendall(b'{"status":"error","message":"invalid control request"}')
                        client.close()
                    elif request['operation'] == 'status':
                        client.sendall(b'{"status":"ready"}')
                        client.close()
                    else:
                        connection = client
                        stopped = True
                except (OSError, ValueError, json.JSONDecodeError):
                    client.close()
    except (OSError, RcError, ProcessControlError, ValueError) as error:
        failure = str(error)
    finally:
        if owned is not None:
            try:
                owned.close()
            except ProcessControlError as error:
                cause = error.__cause__
                failure = f'owned process cleanup unverified: {error}' + (f' ({cause})' if cause else '')
        if failure:
            state.update(status='failed', error=failure)
            write_state(directory, state)
        else:
            current = read_state(root)
            if current['nonce'] == nonce:
                (directory / 'instance.json').unlink()
            socket_path.unlink(missing_ok=True)
            control_dir.rmdir()
        if connection is not None:
            reply = {'status': 'error', 'message': failure} if failure else {'status': 'stopped'}
            try:
                connection.sendall(json.dumps(reply).encode())
            finally:
                connection.close()
        os.close(lock_fd)
    if failure:
        raise RcError(failure)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='operation', required=True)
    for name in ('start', 'stop', 'status', 'demo', 'demo-info'):
        item = sub.add_parser(name)
        item.add_argument('--data-root', required=True)
        if name == 'start':
            item.add_argument('--port', type=int, default=7878)
    cli = sub.add_parser('cli')
    cli.add_argument('name', choices=REQUIRED_BINS)
    cli.add_argument('args', nargs=argparse.REMAINDER)
    worker = sub.add_parser('_supervise')
    worker.add_argument('--data-root', required=True)
    worker.add_argument('--port', required=True, type=int)
    worker.add_argument('--control-dir', required=True)
    worker.add_argument('--lock-fd', required=True, type=int)
    worker.add_argument('--log', required=True)
    args = parser.parse_args()
    bundle = Path(__file__).resolve().parent.parent
    if args.operation == 'cli':
        bundle_metadata(bundle)
        os.chdir(bundle / 'source')
        os.execve(bundle / 'bin' / args.name, [args.name, *args.args], release_env(bundle))
    root = data_root(args.data_root, bundle, must_exist=args.operation != 'demo')
    if args.operation == 'demo':
        bundle_metadata(bundle)
        install_demo(bundle / 'demo/lake-template', root)
        print(f'Synthetic demo installed at {root}')
    elif args.operation == 'demo-info':
        for name in ('synthetic-momentum-request.json', 'synthetic-ic-request.json'):
            print(f'{name}: {(root / name).read_text()}')
    elif args.operation == 'start':
        start(bundle, root, args.port)
    elif args.operation == '_supervise':
        supervisor(bundle, root, args.port, Path(args.control_dir), args.lock_fd, Path(args.log))
    else:
        result = exchange(read_state(root), args.operation)
        print(result['status'])


if __name__ == '__main__':
    try:
        main()
    except (RcError, OSError) as error:
        print(f'RC error: {error}', file=sys.stderr)
        sys.exit(1)
