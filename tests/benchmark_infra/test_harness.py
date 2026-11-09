import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('harness', ROOT / 'benchmarks/snapshots/harness.py')
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)


def report():
    return {'schema_version': 2, 'status': 'passed', 'mode': 'benchmark', 'fixture_version': 1,
            'workload_sha256': 'work', 'environment_lock_sha256': 'lock', 'runs': 5,
            'source': {'revision': 'old'}, 'binary_sha256': 'old',
            'environment': {'canonical': True, 'tools': {'git': '2.40.0'}, 'kernel': 'kernel',
                            'architecture': 'x86_64', 'cpu_model': 'cpu', 'cpu_quota': '400000 100000',
                            'memory_limit': '8589934592', 'filesystem': 'ext2/ext3',
                            'packages_sha256': 'packages', 'image_id': 'old-image'},
            'results': {'smoke': {'warm_median_seconds': 2, 'fixture_tree': 'same-tree'}}}


class HarnessTests(unittest.TestCase):
    def test_generator_reproduces_versioned_golden_tree(self):
        with tempfile.TemporaryDirectory() as temp:
            a = h.create_fixture(Path(temp) / 'a', h.SPEC['profiles']['smoke'])
            b = h.create_fixture(Path(temp) / 'b', h.SPEC['profiles']['smoke'])
            self.assertEqual(a, b)
            self.assertEqual(a, h.SPEC['expected_smoke_tree_sha1'])

    def test_edits_cover_repository_and_are_repeatable(self):
        for count in h.SPEC['profiles'].values():
            selected = h.changed_positions(count)
            self.assertEqual(len(selected), len(set(selected)))
            self.assertEqual(selected, h.changed_positions(count))
            self.assertGreater(max(selected) - min(selected), count * .8)

    def test_environment_and_fixture_contracts_are_shared(self):
        dockerfile = (h.HERE / 'Dockerfile').read_text()
        for key in ['base_image', 'debian_snapshot', 'git_source_sha256']:
            self.assertIn(h.LOCK[key], dockerfile)
        self.assertEqual(json.loads((ROOT / 'crates/groove-core/tests/fixtures/snapshot-workloads-v1.json').read_text()), h.SPEC)

    def test_compare_allows_code_changes_but_not_environment_drift(self):
        a, b = report(), report()
        b['source']['revision'] = 'new'; b['binary_sha256'] = 'new'; b['environment']['image_id'] = 'new-image'
        b['results']['smoke']['warm_median_seconds'] = 1
        self.assertEqual(h.compare(a, b)['smoke']['ratio'], .5)
        for key in ['tools', 'kernel', 'cpu_quota', 'memory_limit', 'filesystem', 'packages_sha256', 'canonical']:
            drift = copy.deepcopy(b); drift['environment'][key] = 'different'
            with self.subTest(key=key), self.assertRaises(RuntimeError): h.compare(a, drift)

    def test_compare_rejects_failed_old_or_different_workloads(self):
        for key, value in [('status', 'failed'), ('schema_version', 0), ('workload_sha256', 'other'), ('runs', 10), ('mode', 'check')]:
            a, b = report(), report(); b[key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError): h.compare(a, b)
        a, b = report(), report()
        a['environment']['canonical'] = b['environment']['canonical'] = False
        with self.assertRaises(RuntimeError): h.compare(a, b)
        a, b = report(), report(); b['results']['smoke']['fixture_tree'] = 'changed'
        with self.assertRaises(RuntimeError): h.compare(a, b)

    def test_atomic_report_roundtrip_and_explicit_checks(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'report.json'
            h.write_report(path, report())
            self.assertEqual(json.loads(path.read_text()), report())
            self.assertEqual(list(Path(temp).iterdir()), [path])
        with self.assertRaises(RuntimeError): h.require(False, 'checks remain active under python -O')


if __name__ == '__main__': unittest.main()
