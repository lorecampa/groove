#!/usr/bin/env bash
# Run the unpublished Rust release tool using the repository's pinned toolchain.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
exec cargo run --quiet --locked --package groove-release -- "$@"
