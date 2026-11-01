"""Synthetic checks for release source provenance and relocatable demo inputs."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import rc_build as build
from rc_runtime import DEMO_CONFIGS, RcError, install_demo


def git(repo, *args):
    return subprocess.run(['git', *args], cwd=repo, check=True, capture_output=True, text=True)


class ReleaseBuildTests(unittest.TestCase):
    def test_source_snapshot_keeps_commit_without_bundling_excluded_blobs(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / 'source repo'
            repo.mkdir()
            git(repo, 'init', '-q')
            git(repo, 'config', 'user.name', 'Synthetic Fixture')
            git(repo, 'config', 'user.email', 'synthetic@example.invalid')
            (repo / 'Cargo.toml').write_text('[workspace]\n')
            code = repo / 'crates/example/src/lib.rs'
            code.parent.mkdir(parents=True)
            code.write_text('pub fn example() {}\n')
            private = repo / 'docs/private-example.txt'
            private.parent.mkdir()
            private.write_text('excluded synthetic evidence')
            git(repo, 'add', '.')
            git(repo, '-c', 'commit.gpgsign=false', 'commit', '-qm', 'synthetic')
            revision = git(repo, 'rev-parse', 'HEAD').stdout.strip()
            snapshot = root / 'bundle source'
            build.source_snapshot(repo, snapshot, root, revision)
            self.assertEqual(git(snapshot, 'rev-parse', 'HEAD').stdout.strip(), revision)
            self.assertEqual(git(snapshot, 'status', '--porcelain').stdout, '')
            self.assertEqual((snapshot / 'crates/example/src/lib.rs').read_text(), code.read_text())
            self.assertFalse((snapshot / 'docs/private-example.txt').exists())
            environment = {**os.environ, 'GIT_NO_LAZY_FETCH': '1'}
            missing = subprocess.run(['git', 'cat-file', '-e', 'HEAD:docs/private-example.txt'],
                                     cwd=snapshot, env=environment, capture_output=True)
            self.assertNotEqual(missing.returncode, 0)
            self.assertEqual(git(snapshot, 'remote').stdout, '')

    def test_only_owned_synthetic_paths_are_made_portable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name in DEMO_CONFIGS:
                config = root / name
                config.parent.mkdir(parents=True, exist_ok=True)
                config.write_text(json.dumps({'input': str(root / 'input/example.json')}))
            immutable = root / 'raw/synthetic'
            immutable.parent.mkdir()
            immutable.write_bytes(b'unchanged')
            build.portable_demo(root)
            for name in DEMO_CONFIGS:
                value = json.loads((root / name).read_text())
                self.assertEqual(value['input'], '@DEMO_ROOT@/input/example.json')
            self.assertEqual(immutable.read_bytes(), b'unchanged')
            (root / DEMO_CONFIGS[0]).write_text(json.dumps({'input': '/outside/example.json'}))
            with self.assertRaisesRegex(RcError, 'outside'):
                build.portable_demo(root)

    def test_immutable_fixture_with_build_path_fails_instead_of_rewriting(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name in DEMO_CONFIGS:
                config = root / name
                config.parent.mkdir(parents=True, exist_ok=True)
                config.write_text('{}')
            (root / 'immutable.json').write_text(json.dumps({'path': str(root / 'source')}))
            with self.assertRaisesRegex(RcError, 'retains its build path'):
                build.portable_demo(root)

    def test_preview_input_is_copied_and_rebound_without_build_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            lake = root / 'build/fixture/lake'
            preview = root / 'build/fixture/input'
            preview.mkdir(parents=True)
            (preview / 'report.json').write_text('synthetic report')
            alias = root / 'input-alias'
            alias.symlink_to(preview, target_is_directory=True)
            for name in DEMO_CONFIGS:
                config = lake / name
                config.parent.mkdir(parents=True, exist_ok=True)
                config.write_text(json.dumps({'raw_root': str(preview), 'report': str(alias / 'report.json')}))
            build.portable_demo(lake, preview, root / 'build')
            packaged = json.loads((lake / 'etf.json-selection.json').read_text())
            self.assertEqual(packaged['raw_root'], '@DEMO_ROOT@/synthetic-input/preview')
            self.assertEqual(packaged['report'], '@DEMO_ROOT@/synthetic-input/preview/report.json')
            destination = root / 'relocated lake'
            install_demo(lake, destination)
            installed = json.loads((destination / 'etf.json-selection.json').read_text())
            self.assertEqual(Path(installed['raw_root']), destination / 'synthetic-input/preview')
            self.assertEqual(Path(installed['report']).read_text(), 'synthetic report')


if __name__ == '__main__':
    unittest.main()
