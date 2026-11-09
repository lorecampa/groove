#!/bin/sh
# The same gates can be run directly, without a Git operation.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
case "${1:-}" in
  commit)
    git diff --cached --check
    cargo fmt --all -- --check
    ;;
  push)
    git diff --check
    cargo fmt --all -- --check
    python3 -m unittest discover -s tests/benchmark_infra -v
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    cargo test --workspace --all-targets --all-features --locked
    cargo test --workspace --doc --locked
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
    ;;
  *) printf '%s\n' 'Usage: scripts/check.sh commit|push' >&2; exit 2 ;;
esac
