#!/usr/bin/env bash
# Advisory, dependency-policy and recorded-review checks. Never install tools here.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
case "${1:-check}" in
  check)
    gitleaks git --redact --no-banner --log-opts=--all
    cargo audit --deny warnings
    cargo deny --locked check
    cargo vet --locked --no-minimize-exemptions
    cargo audit --file fuzz/Cargo.lock --deny warnings
    cargo deny --manifest-path fuzz/Cargo.toml --locked check
    ;;
  *) printf '%s\n' 'Usage: bash scripts/security.sh [check]' >&2; exit 2 ;;
esac
