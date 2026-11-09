#!/bin/sh
set -eu
cd "$(git rev-parse --show-toplevel)"
existing=$(git config --local --get core.hooksPath || true)
case "$existing" in
  ''|.githooks) git config --local core.hooksPath .githooks ;;
  *) printf 'Refusing to replace existing core.hooksPath: %s\n' "$existing" >&2; exit 1 ;;
esac
printf '%s\n' 'Installed repository hooks: pre-commit (format/whitespace), pre-push (full checks).'
