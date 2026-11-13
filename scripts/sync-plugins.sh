#!/usr/bin/env bash
# Codex installers omit symlinks: ship a checked, real-file skill mirror.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
case "${1:---check}" in
  --check)
    diff -r skills/groove plugins/groove/skills/groove
    ;;
  --write)
    mkdir -p plugins/groove/skills/groove/references
    cp skills/groove/SKILL.md plugins/groove/skills/groove/SKILL.md
    cp skills/groove/references/*.md plugins/groove/skills/groove/references/
    diff -r skills/groove plugins/groove/skills/groove
    ;;
  *) printf '%s\n' 'Usage: scripts/sync-plugins.sh [--check|--write]' >&2; exit 2 ;;
esac
