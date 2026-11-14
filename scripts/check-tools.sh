#!/bin/sh
# Diagnose prerequisites only; hooks never install or reconfigure tools.
set -eu
require_tool() {
  if ! "$@" >/dev/null; then
    printf 'Cannot run development tool: %s\n' "$*" >&2
    case "$1 $2" in
      'git --version')
        printf '%s\n' 'Install a maintained Git version (2.40 minimum) on PATH.' >&2 ;;
      'cargo --version')
        printf '%s\n' 'Install rustup and the toolchain pinned in rust-toolchain.toml.' >&2 ;;
      *)
        printf '%s\n' 'Run mise install and use an activated shell or mise exec -- COMMAND.' >&2
        printf '%s\n' 'Alternatively, install the versions in mise.toml on PATH.' >&2 ;;
    esac
    printf '%s\n' 'See docs/security.md for contributor setup.' >&2
    exit 1
  fi
}
case "${1:-push}" in
  commit|push) ;;
  *) printf '%s\n' 'Usage: bash scripts/check-tools.sh [commit|push]' >&2; exit 2 ;;
esac
require_tool git --version
require_tool cargo --version
require_tool gitleaks version
if [ "${1:-push}" = push ]; then
  require_tool cargo audit --version
  require_tool cargo deny --version
  require_tool cargo vet --version
fi
