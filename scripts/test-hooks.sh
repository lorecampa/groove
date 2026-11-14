#!/usr/bin/env bash
# Exercise contributor hooks in disposable repositories with the real scanner.
set -euo pipefail
source_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/groove-hook-tests.XXXXXX")
trap 'rm -rf -- "$fixture"' EXIT
trap 'exit 1' HUP INT TERM
# A calling pre-commit hook may select an alternate index.
unset $(git rev-parse --local-env-vars)
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export GIT_TERMINAL_PROMPT=0
mkdir -p "$fixture/repo/src" "$fixture/repo/fuzz/src" "$fixture/bin"
cd "$fixture/repo"
git init -q
git config user.name 'Hook tests'
git config user.email 'hooks@example.invalid'
cp -R "$source_root/.githooks" .
mkdir scripts
cp "$source_root"/scripts/{check.sh,check-index.sh,check-tools.sh,security.sh} scripts/
git config core.hooksPath .githooks
cat > Cargo.toml <<'MANIFEST'
[package]
name = "hook-fixture"
version = "0.0.0"
edition = "2021"
[workspace]
exclude = ["fuzz"]
MANIFEST
cat > fuzz/Cargo.toml <<'MANIFEST'
[package]
name = "hook-fixture-fuzz"
version = "0.0.0"
edition = "2021"
[workspace]
MANIFEST
printf 'fn main() {}\n' > src/main.rs
printf 'fn main() {}\n' > fuzz/src/main.rs
printf 'safe contents\n' > notes.txt
git add .
git commit -qm 'Create disposable hook fixture'

expect_failure() {
  if "$@" > "$fixture/failure.log" 2>&1; then
    printf 'Expected rejection: %s\n' "$*" >&2
    exit 1
  fi
}

# Scanner errors must block commits, too.
cat > "$fixture/bin/gitleaks" <<'STUB'
#!/bin/sh
printf '%s\n' 'scanner unavailable' >&2
exit 127
STUB
chmod +x "$fixture/bin/gitleaks"
expect_failure env PATH="$fixture/bin:$PATH" git commit --allow-empty -qm 'Must reject unavailable scanner'
grep -q 'scanner unavailable' "$fixture/failure.log"
grep -q 'mise install' "$fixture/failure.log"
rm "$fixture/bin/gitleaks"

# A secret staged then hidden by a clean working copy must still be rejected.
# Assemble a synthetic scanner fixture without storing a token in source history.
printf 'api_key = "%s%s"\n' 'AKIA' 'J7MN2Q9RX4VT6Z8P' > notes.txt
git add notes.txt
printf 'safe contents\n' > notes.txt
expect_failure git commit -qm 'Must reject staged secret'
grep -q 'leaks found' "$fixture/failure.log"
git show :notes.txt | cmp - <(printf 'api_key = "%s%s"\n' 'AKIA' 'J7MN2Q9RX4VT6Z8P')
cmp notes.txt <(printf 'safe contents\n')
git add notes.txt

# Formatting checks must see the staged version, preserving unstaged edits.
printf 'fn main(){println!("unstaged");}\n' > src/main.rs
git commit --allow-empty -qm 'Formatted index passes despite unstaged formatting'
cmp src/main.rs <(printf 'fn main(){println!("unstaged");}\n')
printf 'fn main(){println!("staged");}\n' > src/main.rs
git add src/main.rs
printf 'fn main() {}\n' > src/main.rs
expect_failure git commit -qm 'Must reject staged formatting'
grep -q 'Diff in' "$fixture/failure.log"
git add src/main.rs
printf 'fn main(){println!("fuzz");}\n' > fuzz/src/main.rs
git add fuzz/src/main.rs
expect_failure git commit -qm 'Must reject staged fuzz formatting'
grep -q 'Diff in' "$fixture/failure.log"
printf 'fn main() {}\n' > fuzz/src/main.rs
git add fuzz/src/main.rs

# Stub expensive build/dependency commands only for push-hook control flow.
# Full source validation is run separately by scripts/check.sh push.
cat > "$fixture/bin/cargo" <<'STUB'
#!/bin/sh
printf '%s\n' "$*" >> "$HOOK_CARGO_LOG"
if [ "${HOOK_DIRTY_DURING_CHECK:-}" = true ]; then
  printf 'changed during checks\n' >> notes.txt
fi
STUB
chmod +x "$fixture/bin/cargo"
export PATH="$fixture/bin:$PATH" HOOK_CARGO_LOG="$fixture/cargo.log"
head=$(git rev-parse HEAD)
zero=0000000000000000000000000000000000000000
push_update() {
  printf 'refs/heads/test %s refs/heads/test %s\n' "$1" "$zero" | .githooks/pre-push
}
: > "$HOOK_CARGO_LOG"
printf 'untracked\n' > pending.txt
expect_failure push_update "$head"
grep -q 'clean checkout' "$fixture/failure.log"
test ! -s "$HOOK_CARGO_LOG"
rm pending.txt
printf 'unstaged\n' >> notes.txt
expect_failure push_update "$head"
grep -q 'clean checkout' "$fixture/failure.log"
git add notes.txt
expect_failure push_update "$head"
grep -q 'clean checkout' "$fixture/failure.log"
git -c core.hooksPath=/dev/null commit -qm 'Advance fixture HEAD'
expect_failure push_update "$head"
grep -q 'check out its commit' "$fixture/failure.log"
test ! -s "$HOOK_CARGO_LOG"
head=$(git rev-parse HEAD)
push_update "$head"
# Annotated release tags at HEAD also validate successfully.
git -c core.hooksPath=/dev/null tag -am 'Disposable release' hook-release
push_update "$(git rev-parse hook-release)"
grep -q '^test --workspace' "$HOOK_CARGO_LOG"
export HOOK_DIRTY_DURING_CHECK=true
expect_failure push_update "$head"
grep -q 'clean checkout' "$fixture/failure.log"
unset HOOK_DIRTY_DURING_CHECK
# Deletion-only pushes remain possible even from a dirty checkout.
: > "$HOOK_CARGO_LOG"
push_update "$zero"
test ! -s "$HOOK_CARGO_LOG"
printf '%s\n' 'Contributor hook regressions passed.'
