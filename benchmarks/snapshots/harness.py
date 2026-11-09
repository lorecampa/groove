#!/usr/bin/env python3
"""Versioned deterministic fixture and benchmark runner (Python standard library only)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
SPEC_PATH = HERE / 'workloads-v1.json'
LOCK_PATH = HERE / 'environment.lock.json'
SPEC = json.loads(SPEC_PATH.read_text())
LOCK = json.loads(LOCK_PATH.read_text())
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('GIT_', 'GROOVE_'))}
ENV.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1', GIT_OPTIONAL_LOCKS='0',
           GIT_AUTHOR_DATE='2025-01-01T00:00:00Z', GIT_COMMITTER_DATE='2025-01-01T00:00:00Z',
           LC_ALL='C', TZ='UTC')


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def command(args, cwd=None, env=None):
    return subprocess.check_output(args, cwd=cwd, env=env or ENV, stderr=subprocess.PIPE)


def git(repo, *args, env=None):
    return command(['git', '-c', 'core.hooksPath=/dev/null', '-c', 'gc.auto=0', *args], cwd=repo, env=env)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def versions():
    return {'rust': command(['rustc', '--version']).decode().strip(),
            'git': command(['git', '--version']).decode().strip(),
            'python': platform.python_version(),
            'gnu_time': command(['/usr/bin/time', '--version']).decode().splitlines()[0],
            'gnu_time_package': command(['dpkg-query', '-W', '-f=${Version}', 'time']).decode().strip()}


def verify_environment():
    found = versions()
    require(found['rust'].split()[1] == LOCK['rust'], 'Rust does not match environment.lock.json')
    require(found['git'].split()[2] == LOCK['git'], 'Git does not match environment.lock.json')
    require(found['python'] == LOCK['python'], 'Python does not match environment.lock.json')
    require(found['gnu_time_package'].split('-')[0] == LOCK['gnu_time'], 'GNU time package does not match environment.lock.json')
    require(platform.machine() == 'x86_64', 'Canonical environment requires linux/amd64')
    return found


def path_for(index):
    return f'd{index // SPEC["files_per_directory"]:05}/f{index:07}.txt'


def content_for(index):
    return (f'fixture-v1:{SPEC["seed"]}:{index % SPEC["content_pool"]}\n' * SPEC['content_repetitions']).encode()


def changed_positions(count):
    total = min(SPEC['changed_files'], count)
    return [(SPEC['seed'] + j * (count // total)) % count for j in range(total)]


def create_fixture(repo, count):
    repo.mkdir()
    git(repo, 'init', '-q', '--object-format=sha1')
    git(repo, 'config', 'user.name', 'Snapshot Scale')
    git(repo, 'config', 'user.email', 'scale@example.invalid')
    for i in range(count):
        path = repo / path_for(i)
        if i % SPEC['files_per_directory'] == 0:
            path.parent.mkdir()
        path.write_bytes(content_for(i))
    (repo / '.gitignore').write_bytes(b'ignored/\n')
    git(repo, 'add', '.')
    git(repo, 'commit', '-qm', 'fixture-v1')
    return git(repo, 'rev-parse', 'HEAD^{tree}').decode().strip()


def fingerprint(repo, include_refs=True, env=None):
    index = Path(os.fsdecode(git(repo, 'rev-parse', '--path-format=absolute', '--git-path', 'index', env=env)).strip())
    data = [git(repo, 'rev-parse', 'HEAD', env=env), git(repo, 'status', '--porcelain=v1', '-z', env=env),
            git(repo, 'diff', '--binary', 'HEAD', env=env), index.read_bytes()]
    if include_refs:
        data.append(git(repo, 'show-ref', env=env))
    return hashlib.sha256(b'\0'.join(data)).hexdigest()


def object_sizes(repo):
    sizes = [p.stat() for p in (repo / '.git/objects').rglob('*') if p.is_file()]
    return {'logical_bytes': sum(s.st_size for s in sizes), 'allocated_bytes': sum(s.st_blocks * 512 for s in sizes)}


def invoke(binary, repo, *args):
    with tempfile.NamedTemporaryFile() as rss:
        started = time.perf_counter()
        process = subprocess.run(['/usr/bin/time', '-f', '%M', '-o', rss.name, str(binary), '-v',
                                  '--repo', str(repo), '--git-timeout-ms', '300000', 'snapshot', *args],
                                 env=ENV, capture_output=True, timeout=900)
        require(process.returncode == 0, process.stdout.decode(errors='replace') + process.stderr.decode(errors='replace'))
        elapsed = time.perf_counter() - started
        rss.seek(0)
        return json.loads(process.stdout)['result']['snapshot_result'], {
            'seconds': elapsed, 'peak_rss_kib': int(rss.read().strip()),
            'git_steps_ms': [int(n) for n in re.findall(r'Git operation finished.*?elapsed_ms=(\d+)', process.stderr.decode(errors='replace'))]}


def benchmark(binary, repo, runs, selected=None):
    names = git(repo, 'ls-files', '-z').split(b'\0')[:-1]
    if selected is None:
        selected = [os.fsdecode(p) for p in names if (repo / os.fsdecode(p)).is_file()
                    and not (repo / os.fsdecode(p)).is_symlink()][:SPEC['changed_files']]
    require(bool(selected), 'Benchmark needs at least one regular tracked file')
    before = fingerprint(repo, include_refs=False)
    size_before = object_sizes(repo)
    first, first_time = invoke(binary, repo, 'save', 'first')
    require(fingerprint(repo, include_refs=False) == before, 'First save changed working state')
    timings = []
    for iteration in range(runs):
        for name in selected:
            with (repo / name).open('ab') as stream:
                stream.write(f'\nbenchmark-v1:{iteration}\n'.encode())
        state = fingerprint(repo, include_refs=False)
        _, measured = invoke(binary, repo, 'save', f'iteration {iteration}')
        require(fingerprint(repo, include_refs=False) == state, 'Save changed working state')
        timings.append(measured)
    _, unchanged = invoke(binary, repo, 'save', 'unchanged')
    size_after = object_sizes(repo)
    recovery = repo.parent / 'recovered'
    _, external = invoke(binary, repo, 'restore', first['snapshot']['id'], '--to', str(recovery))
    require(git(recovery, 'status', '--porcelain=v1', '-z') == b'', 'External recovery differs from baseline')
    _, inplace = invoke(binary, repo, 'restore', first['snapshot']['id'])
    require(git(repo, 'status', '--porcelain=v1', '-z') == b'', 'In-place recovery differs from baseline')
    return {'tracked_files': len(names), 'changed_files': len(selected), 'first_save': first_time,
            'warm_changed_saves': timings, 'warm_median_seconds': statistics.median(t['seconds'] for t in timings),
            'unchanged_save': unchanged, 'external_restore': external, 'inplace_restore': inplace,
            'object_growth': {key: size_after[key] - size_before[key] for key in size_before},
            'recovery_verified': True}


def text_file(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def environment(root, canonical):
    vfs = os.statvfs(root)
    filesystem = command(['stat', '-f', '-c', '%T', str(root)]).decode().strip()
    quota = text_file('/sys/fs/cgroup/cpu.max')
    memory = text_file('/sys/fs/cgroup/memory.max')
    if canonical:
        verify_environment()
        require(filesystem in ('ext2/ext3', 'xfs', 'btrfs', 'zfs'), 'Canonical scratch storage requires a supported bind-mounted local disk filesystem')
        require(quota is not None and quota.split()[0] != 'max' and int(quota.split()[0]) / int(quota.split()[1]) == LOCK['cpus'], 'CPU quota differs from lock')
        require(memory == str(LOCK['memory_bytes']), 'Memory limit differs from lock')
    cpu = next((line.split(':', 1)[1].strip() for line in (text_file('/proc/cpuinfo') or '').splitlines() if line.startswith('model name')), None)
    packages = Path('/opt/benchmark-packages.txt')
    return {'canonical': canonical, 'tools': versions(), 'kernel': platform.release(), 'architecture': platform.machine(),
            'cpu_model': cpu, 'cpu_quota': quota, 'memory_limit': memory, 'filesystem': filesystem,
            'available_bytes': vfs.f_bavail * vfs.f_frsize, 'available_inodes': vfs.f_favail,
            'packages_sha256': digest(packages) if packages.exists() else None,
            'image_id': os.environ.get('GROOVE_BENCH_IMAGE_ID')}


def preflight(root, count):
    vfs = os.statvfs(root)
    require(vfs.f_favail >= count * 3 + 10000, 'Insufficient free inodes for fixture and recovery')
    require(vfs.f_bavail * vfs.f_frsize >= count * 12288 + 512 * 1024 * 1024, 'Insufficient free disk space')


def write_report(path, report):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
        json.dump(report, stream, indent=2)
        stream.write('\n')
        temporary = Path(stream.name)
    temporary.replace(path)


def run(args):
    require(args.runs > 0, '--runs must be positive')
    args.temp_root.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve()
    source_path = Path('/opt/benchmark-source.json')
    source = json.loads(source_path.read_text()) if source_path.exists() else {'kind': 'local-unpinned'}
    report = {'schema_version': 2, 'status': 'running', 'mode': args.mode, 'fixture_version': SPEC['fixture_version'],
              'workload_sha256': digest(SPEC_PATH), 'environment_lock_sha256': digest(LOCK_PATH),
              'source': source, 'binary_sha256': digest(binary), 'runs': args.runs,
              'environment': environment(args.temp_root, args.canonical), 'results': {}, 'source_unchanged': None}
    write_report(args.output, report)
    try:
        for profile in args.profile:
            count = SPEC['profiles'][profile]
            preflight(args.temp_root, count)
            with tempfile.TemporaryDirectory(prefix='groove-scale-v1-', dir=args.temp_root) as tmp:
                root = Path(tmp)
                # Isolate all user preferences, logs and temporary files.
                for key in ['HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'TMPDIR']:
                    location = root / key.lower(); location.mkdir(); ENV[key] = str(location)
                if args.mode == 'check':
                    candidates = [p for p in (ROOT / 'target/release/deps').glob('snapshot_scale-*') if p.is_file() and os.access(p, os.X_OK) and '.' not in p.name]
                    require(len(candidates) == 1, 'Build exactly one release snapshot_scale test executable')
                    test = {'smoke': 'scale_smoke', 'large': 'scale_100k', 'million': 'scale_1m'}[profile]
                    flags = [] if profile == 'smoke' else ['--ignored']
                    out = command([str(candidates[0]), *flags, '--exact', test, '--nocapture'], env=ENV).decode()
                    require('1 passed' in out, 'Expected scale test did not execute')
                    report['results'][profile] = {'verified': True, 'test': test, 'test_binary_sha256': digest(candidates[0]), 'stdout': out}
                else:
                    repo = root / 'repo'
                    tree = create_fixture(repo, count)
                    report['results'][profile] = benchmark(binary, repo, args.runs, [path_for(i) for i in changed_positions(count)])
                    report['results'][profile]['fixture_tree'] = tree
                write_report(args.output, report)
                print(json.dumps({'profile': profile, 'result': report['results'][profile]}), flush=True)
        if args.real_repo:
            require(args.mode == 'benchmark', '--real-repo is for benchmark mode')
            source_env = dict(ENV)
            before = fingerprint(args.real_repo, env=source_env)
            try:
                with tempfile.TemporaryDirectory(prefix='groove-real-v1-', dir=args.temp_root) as tmp:
                    repo = Path(tmp) / 'repo'
                    # Prior per-fixture temporary environment paths have been removed.
                    for key in ['HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'TMPDIR']:
                        location = Path(tmp) / key.lower(); location.mkdir(); ENV[key] = str(location)
                    command(['git', 'clone', '--local', '--no-hardlinks', str(args.real_repo.resolve()), str(repo)])
                    report['results']['real_repo'] = benchmark(binary, repo, args.runs)
                    report['results']['real_repo']['base_commit'] = git(repo, 'rev-parse', 'HEAD').decode().strip()
            finally:
                report['source_unchanged'] = fingerprint(args.real_repo, env=source_env) == before
                require(report['source_unchanged'], 'Real source changed during benchmark')
        report['status'] = 'passed'
    except Exception as error:
        report['status'] = 'failed'; report['error'] = str(error)
        raise
    finally:
        write_report(args.output, report)


def comparison_key(report):
    env = report['environment']
    return {key: report[key] for key in ['schema_version', 'mode', 'fixture_version', 'workload_sha256', 'environment_lock_sha256', 'runs']} | {
        'environment': {key: env[key] for key in ['canonical', 'tools', 'kernel', 'architecture', 'cpu_model', 'cpu_quota', 'memory_limit', 'filesystem', 'packages_sha256']}}


def compare(before, after):
    require(before.get('schema_version') == after.get('schema_version') == 2, 'Legacy or unsupported report schema; generate version 2 reports')
    require(before['environment']['canonical'] and after['environment']['canonical'], 'Only canonical environment reports support regression comparisons')
    require(before['status'] == after['status'] == 'passed', 'Only successful runs can be compared')
    require(before['mode'] == after['mode'] == 'benchmark', 'Correctness reports have no latency comparison')
    require(comparison_key(before) == comparison_key(after), 'Workload or environment differs; these runs are not comparable')
    require(before['results'].keys() == after['results'].keys(), 'Profile sets differ')
    result = {}
    for name in before['results']:
        a, b = before['results'][name], after['results'][name]
        require(a.get('fixture_tree', a.get('base_commit')) == b.get('fixture_tree', b.get('base_commit')), 'Fixture or real-repository revision differs')
        result[name] = {'before_seconds': a['warm_median_seconds'], 'after_seconds': b['warm_median_seconds'],
                        'ratio': b['warm_median_seconds'] / a['warm_median_seconds']}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    sub.add_parser('verify-environment')
    run_parser = sub.add_parser('run')
    run_parser.add_argument('--mode', choices=['check', 'benchmark'], required=True)
    run_parser.add_argument('--profile', choices=SPEC['profiles'], action='append', required=True)
    run_parser.add_argument('--runs', type=int, default=SPEC['runs'])
    run_parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/groove')
    run_parser.add_argument('--temp-root', type=Path, required=True)
    run_parser.add_argument('--output', type=Path, required=True)
    run_parser.add_argument('--canonical', action='store_true')
    run_parser.add_argument('--real-repo', type=Path)
    comparison = sub.add_parser('compare')
    comparison.add_argument('before', type=Path); comparison.add_argument('after', type=Path)
    args = parser.parse_args()
    if args.action == 'verify-environment': print(json.dumps(verify_environment()))
    elif args.action == 'compare': print(json.dumps(compare(json.loads(args.before.read_text()), json.loads(args.after.read_text())), indent=2))
    else: run(args)


if __name__ == '__main__':
    main()
