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
    if native_git "$directory/git"; then
      test_git="$directory/git"
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
