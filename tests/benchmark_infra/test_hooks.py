"""Real Git operations in disposable repos; fake Cargo checks hook failure propagation."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class HookTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.repo = root / 'repo'; self.repo.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        self.env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
                        GIT_AUTHOR_NAME='Hooks', GIT_AUTHOR_EMAIL='hooks@example.invalid',
                        GIT_COMMITTER_NAME='Hooks', GIT_COMMITTER_EMAIL='hooks@example.invalid')
        tools = root / 'tools'; tools.mkdir()
        fake = tools / 'cargo'
        fake.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$TEST_CARGO_LOG"\n[ "${TEST_FAIL_CARGO:-}" != "$1" ]\n')
        fake.chmod(0o755)
        self.log = root / 'cargo.log'
        self.env.update(PATH=str(tools) + os.pathsep + self.env['PATH'], TEST_CARGO_LOG=str(self.log))
        self.call('git', 'init', '-q')
        shutil.copytree(ROOT / '.githooks', self.repo / '.githooks')
        (self.repo / 'scripts').mkdir()
        for name in ['check.sh', 'install-hooks.sh']:
            shutil.copy2(ROOT / 'scripts' / name, self.repo / 'scripts' / name)
        checks = self.repo / 'tests/benchmark_infra'; checks.mkdir(parents=True)
        (checks / 'test_example.py').write_text('import unittest\nclass Example(unittest.TestCase):\n def test_ok(self): self.assertTrue(True)\n')
        self.call('./scripts/install-hooks.sh')
        (self.repo / 'file').write_text('first\n')
        self.call('git', 'add', '.')

    def call(self, *args, success=True):
        result = subprocess.run(args, cwd=self.repo, env=self.env, capture_output=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout.decode() + result.stderr.decode())
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def test_precommit_blocks_failed_formatting_and_accepts_success(self):
        self.env['TEST_FAIL_CARGO'] = 'fmt'
        self.call('git', 'commit', '-qm', 'blocked', success=False)
        self.call('git', 'rev-parse', '--verify', 'HEAD', success=False)
        self.env.pop('TEST_FAIL_CARGO')
        self.call('git', 'commit', '-qm', 'accepted')
        self.assertIn('fmt --all -- --check', self.log.read_text())

    def test_precommit_blocks_staged_whitespace_before_running_cargo(self):
        (self.repo / 'file').write_text('trailing whitespace  \n')
        self.call('git', 'add', 'file')
        self.call('git', 'commit', '-qm', 'blocked', success=False)
        self.assertFalse(self.log.exists())

    def test_prepush_blocks_failed_gate_without_updating_remote(self):
        self.call('git', 'commit', '-qm', 'first')
        remote = Path(self.temp.name) / 'remote.git'
        self.call('git', 'init', '--bare', '-q', str(remote))
        self.call('git', 'remote', 'add', 'origin', str(remote))
        self.env['TEST_FAIL_CARGO'] = 'clippy'
        self.call('git', 'push', 'origin', 'HEAD:refs/heads/main', success=False)
        self.assertEqual(self.call('git', 'ls-remote', 'origin', 'refs/heads/main').stdout, b'')
        self.env.pop('TEST_FAIL_CARGO')
        self.call('git', 'push', 'origin', 'HEAD:refs/heads/main')
        log = self.log.read_text()
        self.assertIn('test --workspace --all-targets --all-features --locked', log)
        self.assertNotIn('test-threads=1', log)
        self.assertIn('doc --workspace --no-deps --locked', log)

    def test_installer_is_idempotent_and_preserves_existing_custom_hooks(self):
        self.call('./scripts/install-hooks.sh')
        self.call('git', 'config', 'core.hooksPath', 'custom-hooks')
        self.call('./scripts/install-hooks.sh', success=False)
        self.assertEqual(self.call('git', 'config', '--get', 'core.hooksPath').stdout.strip(), b'custom-hooks')
