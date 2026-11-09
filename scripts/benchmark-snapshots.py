#!/usr/bin/env python3
"""Compatibility entry point; delegates to the versioned snapshot harness.

For canonical measurements use benchmarks/snapshots/run.py (pinned container).
This entry point records local tool versions and marks results non-canonical.
"""
import argparse
from pathlib import Path
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'benchmarks/snapshots'))
from harness import SPEC, run  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/groove')
    parser.add_argument('--real-repo', type=Path)
    parser.add_argument('--sizes', type=int, nargs='*', default=[100000, 1000000])
    parser.add_argument('--temp-root', type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument('--runs', type=int, default=SPEC['runs'])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    reverse = {count: name for name, count in SPEC['profiles'].items()}
    if any(count not in reverse for count in args.sizes):
        parser.error('Use versioned fixture sizes: ' + ', '.join(map(str, reverse)))
    args.profile = [reverse[count] for count in args.sizes]
    args.mode = 'benchmark'
    args.canonical = False
    run(args)


if __name__ == '__main__': main()
