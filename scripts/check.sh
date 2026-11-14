#!/bin/sh
# The same gates can be run directly, without a Git operation.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
case "${1:-}" in
  commit)
    bash scripts/check-index.sh
    ;;
  push)
    git diff --check
    bash scripts/security.sh
    cargo fmt --all -- --check
    cargo fmt --manifest-path fuzz/Cargo.toml -- --check
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    cargo test --workspace --all-targets --all-features --locked
    cargo test --workspace --doc --locked
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
    ;;
  *) printf '%s\n' 'Usage: scripts/check.sh commit|push' >&2; exit 2 ;;
esac
