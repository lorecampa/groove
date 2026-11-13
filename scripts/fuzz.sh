#!/usr/bin/env bash
# Bounded parser fuzzing with disposable corpora; no repository command execution.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
duration=${1:-60}
if [[ $# -gt 1 || ! $duration =~ ^[1-9][0-9]{0,3}$ ]] || (( duration > 3600 )); then
  printf '%s\n' 'Usage: bash scripts/fuzz.sh [SECONDS_PER_TARGET (1..3600; default 60)]' >&2
  exit 2
fi
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
cp fuzz/Cargo.lock "$scratch/Cargo.lock"
# cargo-fuzz does not expose --locked. Prefetch the locked graph, build offline,
# and reject any lockfile change. Corpus mutations stay outside the checkout.
cargo fetch --locked --manifest-path fuzz/Cargo.toml
for target in identifiers; do
  cp -R "fuzz/corpus/$target" "$scratch/$target"
  CARGO_NET_OFFLINE=true cargo +nightly-2026-11-12 fuzz run "$target" "$scratch/$target" -- \
    "-max_total_time=$duration" -max_len=65536 -timeout=10
  cmp fuzz/Cargo.lock "$scratch/Cargo.lock"
done
