#!/usr/bin/env bash
# Build the CLI from this checkout, using its pinned toolchain and Cargo.lock.
set -euo pipefail
case "${1:-}" in
  -h|--help)
    cat <<'HELP'
Usage: scripts/install-cli.sh [CARGO_INSTALL_OPTIONS...]

Build and install Groove's CLI from this checkout's source, including local edits.
Runs cargo install --path crates/groove-cli --locked --force from the repository
root. Cargo installs to its normal destination and replaces an existing Groove
binary, even when the package version has not changed. The repository is not
fetched or switched to another branch; Cargo can download missing dependencies.

Additional options are passed literally to cargo install. Relative paths resolve
from the checkout root. For example:
  bash scripts/install-cli.sh
  bash scripts/install-cli.sh --offline --root /absolute/preview-install

Requires Rust/Cargo. Verify the resulting binary with groove --version (or use
PATH_TO_PREVIEW/bin/groove --version for an alternate --root). The version output
includes the source revision. Git/path installations use this script or their
original installer for updates; groove upgrade only installs crates.io updates.
HELP
    exit 0
    ;;
esac
checkout_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd -- "$checkout_root"
exec cargo install --path "$checkout_root/crates/groove-cli" --locked --force "$@"
