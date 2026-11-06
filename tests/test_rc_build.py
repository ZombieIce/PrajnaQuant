"""Synthetic checks for release source provenance and relocatable demo inputs."""
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import rc_build as build
from rc_runtime import DEMO_CONFIGS, RcError, install_demo


def git(repo, *args):
    return subprocess.run(['git', *args], cwd=repo, check=True, capture_output=True, text=True)


class ReleaseBuildTests(unittest.TestCase):
    def test_daily_price_importer_and_demo_input_are_in_release_inventory(self):
        self.assertIn('prajna-import-daily-price', build.DATA_BINS)
        self.assertIn('prajna-import-daily-price', build.REQUIRED_BINS)
        self.assertIn('synthetic-input/daily-price/config.json', DEMO_CONFIGS)

    def test_tool_version_keeps_stdout_separate_from_logged_warnings(self):
        log = io.StringIO()
        version = build.tool_version([
            sys.executable, '-c',
            'import sys; print("tool 1.2"); print("warning on stderr", file=sys.stderr)',
        ], log)
        self.assertEqual(version, 'tool 1.2')
        self.assertIn('warning on stderr', log.getvalue())

    def test_tool_version_rejects_empty_multiline_and_failed_output(self):
        for script in ('pass', 'print("one\\ntwo")'):
            with self.subTest(script=script), self.assertRaisesRegex(RcError, 'version'):
                build.tool_version([sys.executable, '-c', script], io.StringIO())
        log = io.StringIO()
        with self.assertRaisesRegex(RcError, 'exited 7'):
            build.tool_version([
                sys.executable, '-c',
                'import sys; print("failure diagnostic", file=sys.stderr); sys.exit(7)',
            ], log)
        self.assertIn('failure diagnostic', log.getvalue())

    def test_build_versions_records_sw_vers_and_expected_tools(self):
        outputs = {
            ('rustc', '--version'): 'rustc 1.2',
            ('cargo', '--version'): 'cargo 1.2',
            ('git', '--version'): 'git version 1.2',
            ('node', '--version'): 'v1.2',
            ('npm', '--version'): '1.2',
            ('sw_vers', '-productVersion'): '26.2',
        }
        log = io.StringIO()
        with patch.object(build, 'tool_version', side_effect=lambda args, stream: outputs[tuple(args)]) as version:
            versions = build.build_versions(log)
        self.assertEqual(versions['macos_version'], '26.2')
        self.assertEqual(versions['cargo_version'], 'cargo 1.2')
        self.assertEqual(version.call_count, len(outputs))
        version.assert_any_call(['sw_vers', '-productVersion'], log)

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

    def test_daily_price_demo_installs_relocatable_replay_inputs(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'lake'
            daily_config = 'synthetic-input/daily-price/config.json'
            input_root = root / 'synthetic-input/daily-price'
            config = input_root / 'config.json'
            config.parent.mkdir(parents=True)
            config.write_text(json.dumps({
                'calendar_root': str(input_root),
                'calendar_version': 'sha256:synthetic-calendar',
                'evidence_root': str(input_root),
                'start': '2026-01-05',
                'end': '2026-01-20',
                'members': {'510300.XSHG': 'sha256:synthetic-snapshot'},
            }))
            replay = input_root / 'replay/sh510300-raw-2026-01-05-2026-01-20.json'
            replay.parent.mkdir()
            replay.write_bytes(b'{"synthetic":true}')
            for name in DEMO_CONFIGS:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                if path != config:
                    path.write_text('{}')

            build.portable_demo(root)
            destination = Path(temp) / 'relocated-lake'
            install_demo(root, destination)
            installed = json.loads((destination / daily_config).read_text())

            self.assertEqual(installed['calendar_root'], str(destination / 'synthetic-input/daily-price'))
            self.assertEqual(installed['evidence_root'], str(destination / 'synthetic-input/daily-price'))
            self.assertEqual(
                (destination / 'synthetic-input/daily-price/replay/sh510300-raw-2026-01-05-2026-01-20.json').read_bytes(),
                b'{"synthetic":true}',
            )


if __name__ == '__main__':
    unittest.main()
