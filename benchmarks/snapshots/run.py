#!/usr/bin/env python3
"""Build and run the pinned snapshot lab. No benchmark writes to the source checkout."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
LOCK = json.loads((HERE / 'environment.lock.json').read_text())


def output(args):
    return subprocess.check_output(args, cwd=ROOT).decode().strip()


def build():
    with tempfile.TemporaryDirectory(prefix='groove-bench-build-') as directory:
        context = Path(directory)
        for name in ['Cargo.toml', 'Cargo.lock', 'README.md', 'LICENSE-APACHE', 'LICENSE-MIT']:
            shutil.copy2(ROOT / name, context / name)
        for name in ['crates', 'schemas', 'benchmarks', 'scripts']:
            shutil.copytree(ROOT / name, context / name, symlinks=True,
                            ignore=shutil.ignore_patterns('target', '__pycache__', 'results'))
        h = hashlib.sha256()
        for path in sorted(context.rglob('*')):
            if path.is_symlink(): data = ('symlink:' + os.readlink(path)).encode()
            elif path.is_file(): data = path.read_bytes()
            else: continue
            h.update(str(path.relative_to(context)).encode() + b'\0' + hashlib.sha256(data).digest())
        source = {'revision': output(['git', 'rev-parse', 'HEAD']),
                  'dirty': bool(output(['git', 'status', '--porcelain'])), 'manifest_sha256': h.hexdigest()}
        (context / 'benchmark-source.json').write_text(json.dumps(source, sort_keys=True))
        tag = 'groove-snapshot-bench:' + hashlib.sha256(json.dumps(source, sort_keys=True).encode()).hexdigest()[:16]
        if subprocess.run(['docker', 'image', 'inspect', tag], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            subprocess.run(['docker', 'build', '--platform', LOCK['platform'], '-t', tag,
                            '-f', str(context / 'benchmarks/snapshots/Dockerfile'), str(context)], check=True)
        return tag, output(['docker', 'image', 'inspect', '--format={{.Id}}', tag])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['build', 'check', 'benchmark'])
    parser.add_argument('--profile', choices=['smoke', 'large', 'million'], action='append')
    parser.add_argument('--temp-root', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--real-repo', type=Path)
    args = parser.parse_args()
    if args.mode != 'build' and (not args.temp_root or not args.output or not args.profile):
        parser.error('check/benchmark require --temp-root, --output and --profile')
    tag, image_id = build()
    if args.mode == 'build': print(image_id); return
    args.temp_root.mkdir(parents=True, exist_ok=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='groove-bench-runtime-', dir=args.temp_root) as scratch:
        command = ['docker', 'run', '--rm', '--platform', LOCK['platform'], '--network=none', '--read-only',
                   '--user', f'{os.getuid()}:{os.getgid()}', '--cpus', str(LOCK['cpus']), '--memory', str(LOCK['memory_bytes']),
                   '--memory-swap', str(LOCK['memory_bytes']), '--pids-limit', str(LOCK['pids_limit']),
                   '--tmpfs', '/tmp:rw,nosuid,nodev,size=268435456',
                   '--mount', f'type=bind,source={Path(scratch).resolve()},target=/scratch',
                   '--mount', f'type=bind,source={args.output.parent.resolve()},target=/results',
                   '--env', f'GROOVE_BENCH_IMAGE_ID={image_id}']
        if args.real_repo:
            command += ['--mount', f'type=bind,source={args.real_repo.resolve()},target=/real-source,readonly']
        command += [tag, 'run', '--canonical', '--mode', args.mode, '--runs', str(args.runs),
                    '--temp-root', '/scratch', '--output', '/results/' + args.output.name]
        for profile in args.profile: command += ['--profile', profile]
        if args.real_repo: command += ['--real-repo', '/real-source']
        subprocess.run(command, check=True)


if __name__ == '__main__': main()
