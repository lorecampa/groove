#!/usr/bin/env bash
# Cargo test support: never mutate the environment of concurrent Rust tests.
set -eu
case "$(basename "$1")" in
  *-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*|rust_out) ;;
  *) exec "$@" ;;
esac

native_git() {
  [[ -f "$1" && -x "$1" ]] || return 1
  # ELF and Mach-O executables, including universal Apple binaries. Do not run
  # shell/Python Git wrappers merely to discover the executable behind them.
  case "$(od -An -tx1 -N4 "$1" | tr -d ' \n')" in
    7f454c46|feedface|feedfacf|cefaedfe|cffaedfe|cafebabe|bebafeca|cafebabf|bfbafeca) return 0 ;;
    *) return 1 ;;
  esac
}

# A native executable can still be a wrapper (some managed hosts install compiled
# Git shims). Git's own executable lives in its exec path and reports that same
# directory, so resolve each native candidate there; skip candidates that are not
# Git or whose exec path does not contain a consistent native Git.
real_git() {
  native_git "$1" || return 1
  local exec_path
  exec_path=$(env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    "$1" --exec-path 2>/dev/null) || return 1
  [[ "$exec_path" == /* ]] && native_git "$exec_path/git" || return 1
  [[ "$(env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    "$exec_path/git" --exec-path 2>/dev/null)" == "$exec_path" ]] || return 1
  test_git="$exec_path/git"
}

test_git=${GROOVE_TEST_GIT:-}
if [[ -n "$test_git" ]]; then
  if [[ "$test_git" != /* ]] || ! native_git "$test_git"; then
    echo 'GROOVE_TEST_GIT must name an absolute native Git executable' >&2
    exit 1
  fi
else
  IFS=: read -r -a search_path <<< "$PATH"
  for directory in "${search_path[@]}" /usr/lib/git-core /usr/libexec/git-core /usr/bin; do
    [[ "$directory" == /* ]] || continue
    if real_git "$directory/git"; then
      break
    fi
  done
  if [[ -z "$test_git" ]]; then
    echo 'No native Git found; set GROOVE_TEST_GIT to the real Git executable' >&2
    exit 1
  fi
fi

umask 022
# Canonicalize the caller's chosen temporary root, including macOS /var aliases,
# before any tempfile fixture derives logging, checkout or socket paths from it.
TMPDIR=$(cd "${TMPDIR:-/tmp}" && pwd -P)
export TMPDIR
test_bin=$(mktemp -d "${TMPDIR:-/tmp}/groove-test-bin.XXXXXXXX")
trap 'rm -rf -- "$test_bin"' EXIT
ln -s "$test_git" "$test_bin/git"
export PATH="$test_bin:$PATH" GROOVE_TEST_GIT="$test_git"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
"$@"
