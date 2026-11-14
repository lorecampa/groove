#!/bin/sh
# Check exactly the index without stashing, editing or staging the working tree.
set -eu
cd "$(git rev-parse --show-toplevel)"
git diff --cached --check
bash scripts/check-tools.sh commit
gitleaks git --pre-commit --staged --redact --no-banner

index_tree=$(mktemp -d "${TMPDIR:-/tmp}/groove-index-check.XXXXXX")
trap 'rm -rf -- "$index_tree"' EXIT
trap 'exit 1' HUP INT TERM
git checkout-index --all --prefix="$index_tree/"
(
  unset $(git rev-parse --local-env-vars)
  cd "$index_tree"
  cargo fmt --all -- --check
  cargo fmt --manifest-path fuzz/Cargo.toml -- --check
)
