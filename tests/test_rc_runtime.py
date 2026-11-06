"""Synthetic tests for the standalone RC launcher and demo installation."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import rc_runtime as rc


FAKE_SERVER = '''#!/usr/bin/env python3
import json, os, subprocess, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
args = sys.argv
address = args[args.index("--address") + 1]
root = args[args.index("--dataset-root") + 1]
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)"],
                         start_new_session=True)
with open(os.path.join(root, "owned-child.pid"), "w") as file:
    file.write(str(child.pid))
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"status":"ok", "service":"prajna-quant"}).encode()
        self.send_response(200)
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
print("Prajna Quant: http://" + address, file=sys.stderr, flush=True)
HTTPServer(("127.0.0.1", int(address.rsplit(":", 1)[1])), Handler).serve_forever()
'''


def command(*args, cwd=None, env=None):
    return subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True, timeout=30)


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def running(pid):
    result = command('ps', '-p', str(pid), '-o', 'stat=')
    return result.returncode == 0 and not result.stdout.strip().startswith('Z')


class DemoTests(unittest.TestCase):
    def test_demo_relocates_only_declared_configuration_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            template = root / 'template'
            config = template / 'pool-updates/configs/synthetic.json'
            config.parent.mkdir(parents=True)
            config.write_text(json.dumps({
                'members': {'synthetic': {
                    'publication': {'raw_root': '@DEMO_ROOT@/update-inputs/XSHG'},
                    'adjusted_file': '@DEMO_ROOT@/update-inputs/XSHG/adjusted.json',
                }},
                'actions_file': '@DEMO_ROOT@/update-inputs/actions.json',
            }))
            raw = template / 'raw/synthetic'
            raw.parent.mkdir()
            raw.write_bytes(b'unchanged synthetic immutable input')
            selection = template / 'synthetic-input/XSHG/selection.json'
            selection.parent.mkdir(parents=True)
            selection.write_text(json.dumps({'report': '@DEMO_ROOT@/synthetic-input/XSHG/report.json'}))
            second = template / 'synthetic-input/XSHE/selection.json'
            second.parent.mkdir(parents=True)
            second.write_text(json.dumps({'report': '@DEMO_ROOT@/synthetic-input/XSHE/report.json'}))
            for name in ('etf.json-selection.json', 'stock.json-selection.json'):
                (template / name).write_text(json.dumps({'raw_root': '@DEMO_ROOT@/synthetic-input/preview'}))
            destination = root / 'independent lake'
            rc.install_demo(template, destination)
            installed = json.loads((destination / 'pool-updates/configs/synthetic.json').read_text())
            self.assertEqual(installed['members']['synthetic']['publication']['raw_root'],
                             str(destination / 'update-inputs/XSHG'))
            self.assertEqual(installed['actions_file'], str(destination / 'update-inputs/actions.json'))
            installed_selection = json.loads((destination / 'synthetic-input/XSHG/selection.json').read_text())
            self.assertEqual(installed_selection['report'], str(destination / 'synthetic-input/XSHG/report.json'))
            installed_second = json.loads((destination / 'synthetic-input/XSHE/selection.json').read_text())
            self.assertEqual(installed_second['report'], str(destination / 'synthetic-input/XSHE/report.json'))
            self.assertEqual((destination / 'raw/synthetic').read_bytes(), raw.read_bytes())
            self.assertIn('@DEMO_ROOT@', config.read_text())

    def test_demo_never_overwrites_existing_data_or_accepts_path_escape(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            template = root / 'template'
            config = template / 'pool-updates/configs/synthetic.json'
            config.parent.mkdir(parents=True)
            config.write_text(json.dumps({'actions_file': '@DEMO_ROOT@/../outside'}))
            destination = root / 'destination'
            with self.assertRaisesRegex(rc.RcError, 'demo path'):
                rc.install_demo(template, destination)
            existing = root / 'existing'
            existing.mkdir()
            (existing / 'important').write_text('keep')
            with self.assertRaisesRegex(rc.RcError, 'empty'):
                rc.install_demo(template, existing)
            self.assertEqual((existing / 'important').read_text(), 'keep')

    def test_missing_demo_selection_does_not_create_destination(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            template = root / 'template'
            config = template / rc.DEMO_CONFIGS[0]
            config.parent.mkdir(parents=True)
            config.write_text('{}')
            destination = root / 'destination'
            with self.assertRaisesRegex(rc.RcError, 'configuration is missing'):
                rc.install_demo(template, destination)
            self.assertFalse(destination.exists())


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.bundle = self.root / 'bundle'
        self.data = self.root / 'independent data'
        self.data.mkdir()
        (self.bundle / 'lib').mkdir(parents=True)
        (self.bundle / 'bin').mkdir()
        (self.bundle / 'web').mkdir()
        shutil.copy(ROOT / 'scripts/rc_runtime.py', self.bundle / 'lib/rc_runtime.py')
        shutil.copy(ROOT / 'scripts/workflow_process.py', self.bundle / 'lib/workflow_process.py')
        (self.bundle / 'web/index.html').write_text('<html>synthetic</html>')
        (self.bundle / 'prajna').write_text('synthetic launcher')
        for name in rc.DEMO_CONFIGS:
            config = self.bundle / 'demo/lake-template' / name
            config.parent.mkdir(parents=True, exist_ok=True)
            config.write_text('{}')
        for name in rc.REQUIRED_BINS:
            path = self.bundle / 'bin' / name
            path.write_text(FAKE_SERVER)
            path.chmod(0o755)
        source = self.bundle / 'source'
        source.mkdir()
        for args in [
            ('git', 'init', '-q'),
            ('git', 'config', 'user.name', 'Synthetic Fixture'),
            ('git', 'config', 'user.email', 'synthetic@example.invalid'),
        ]:
            self.assertEqual(command(*args, cwd=source).returncode, 0)
        (source / 'tracked').write_text('synthetic source')
        self.assertEqual(command('git', 'add', 'tracked', cwd=source).returncode, 0)
        self.assertEqual(command('git', '-c', 'commit.gpgsign=false', 'commit', '-qm',
                                 'synthetic source', cwd=source).returncode, 0)
        revision = command('git', 'rev-parse', 'HEAD', cwd=source).stdout.strip()
        rustc = command('rustc', '--version').stdout.strip()
        files = {}
        for path in self.bundle.rglob('*'):
            if not path.is_file() or path.is_relative_to(source):
                continue
            files[str(path.relative_to(self.bundle))] = hashlib.sha256(path.read_bytes()).hexdigest()
        (self.bundle / 'release.json').write_text(json.dumps({
            'version': 'v0.1.0-rc.test',
            'source_commit': revision,
            'rustc_version': rustc,
            'files': files,
        }))
        self.port = free_port()
        self.started = False

    def tearDown(self):
        if self.started:
            result = self.invoke('stop')
            if result.returncode != 0:
                logs = sorted((self.data / '.prajna-rc').glob('service-*.log'),
                              key=lambda path: path.stat().st_mtime)
                detail = logs[-1].read_text() if logs else ''
                self.fail(f'owned service cleanup failed: {result.stderr}{detail}')
        self.temp.cleanup()

    def invoke(self, action, *extra, env=None):
        return command(sys.executable, str(self.bundle / 'lib/rc_runtime.py'), action,
                       '--data-root', str(self.data), *extra, env=env)

    def test_start_stop_cleans_only_owned_processes_and_preserves_data(self):
        unrelated = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(300)'],
                                     start_new_session=True)
        try:
            result = self.invoke('start', '--port', str(self.port))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.started = True
            owned_file = self.data / 'owned-child.pid'
            self.assertTrue(owned_file.is_file())
            owned_pid = int(owned_file.read_text())
            self.assertTrue(running(owned_pid))
            self.assertEqual(self.invoke('status').returncode, 0)
            (self.data / 'saved-result').write_text('keep')
            stopped = self.invoke('stop')
            logs = list((self.data / '.prajna-rc').glob('service-*.log'))
            detail = logs[0].read_text() if logs else ''
            self.assertEqual(stopped.returncode, 0, stopped.stderr + detail)
            self.started = False
            self.assertFalse(running(owned_pid))
            self.assertIsNone(unrelated.poll())
            self.assertEqual((self.data / 'saved-result').read_text(), 'keep')
            restarted = self.invoke('start', '--port', str(self.port))
            self.assertEqual(restarted.returncode, 0, restarted.stderr)
            self.started = True
            self.assertEqual((self.data / 'saved-result').read_text(), 'keep')
        finally:
            unrelated.terminate()
            unrelated.wait(timeout=5)

    def test_conflicting_port_and_dirty_source_fail_without_starting(self):
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            listener.listen()
            result = self.invoke('start', '--port', str(listener.getsockname()[1]))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('port', result.stderr.lower())
        (self.bundle / 'source/tracked').write_text('dirty')
        result = self.invoke('start', '--port', str(self.port))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('source', result.stderr.lower())
        self.assertFalse((self.data / 'owned-child.pid').exists())

    def test_missing_or_changed_bundle_binary_cannot_fall_back_to_path(self):
        executable = self.bundle / 'bin/prajna-momentum'
        executable.unlink()
        result = self.invoke('start', '--port', str(self.port))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('prajna-momentum', result.stderr)
        self.assertFalse((self.data / 'owned-child.pid').exists())


if __name__ == '__main__':
    unittest.main()
