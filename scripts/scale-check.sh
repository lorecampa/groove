#!/usr/bin/env bash
# Contract and timing checks in disposable, isolated repository storage.
set -euo pipefail
set -E
trap 'status=$?; printf "scale-check setup: command failed at line %s (exit %s)\n" "$LINENO" "$status" >&2; exit 2' ERR
binary=target/release/groove
source_repo=
files=60000
directories=20000
scratch=target/scale-check
git_timeout=30m
max_reuse_percent=60
max_error_bytes=65536
keep=false
usage() {
    cat <<'HELP'
Usage: scripts/scale-check.sh [OPTIONS]
  --binary PATH           Groove binary (default: target/release/groove)
  --repo PATH             Clone this repository; never operate on its checkout
  --files N               Synthetic source files (default: 60000)
  --directories N         Synthetic directory budget (default: 20000)
  --scratch DIR           New scratch directory (default: target/scale-check)
  --git-timeout DURATION  Cold checkout timeout (default: 30m)
  --max-reuse-percent P   Maximum fastest reuse/cold percentage (default: 60)
  --max-error-bytes N     Maximum saved-release/error response (default: 65536)
  --keep                  Retain scratch after success
  -h, --help              Show this help
Exit: 0 contracts hold; 1 contract violation; 2 usage/setup error.
HELP
}
setup() { printf 'scale-check setup: %s\n' "$*" >&2; exit 2; }
fail() { printf 'scale-check violation: %s\n' "$*" >&2; exit 1; }
while (($#)); do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --keep) keep=true; shift ;;
        --binary|--repo|--files|--directories|--scratch|--git-timeout|--max-reuse-percent|--max-error-bytes)
            (($# >= 2)) || setup "missing value for $1"
            case "$1" in
                --binary) binary=$2 ;; --repo) source_repo=$2 ;;
                --files) files=$2 ;; --directories) directories=$2 ;;
                --scratch) scratch=$2 ;; --git-timeout) git_timeout=$2 ;;
                --max-reuse-percent) max_reuse_percent=$2 ;; --max-error-bytes) max_error_bytes=$2 ;;
            esac
            shift 2 ;;
        *) setup "unknown option: $1" ;;
    esac
done
for number in "$files" "$directories" "$max_reuse_percent" "$max_error_bytes"; do
    [[ $number =~ ^[1-9][0-9]*$ && ${#number} -le 9 ]] || setup 'counts and limits must be positive integers (at most nine digits)'
done
[[ $git_timeout =~ ^[0-9]+(ms|s|m|h)$ ]] || setup 'timeout requires a unit, e.g. 30m'
[[ -x $binary ]] || setup "binary is not executable: $binary"
binary=$(cd "$(dirname "$binary")" && printf '%s/%s' "$PWD" "$(basename "$binary")")
if [[ -n $source_repo ]]; then source_repo=$(cd "$source_repo" && pwd -P) || setup 'repository does not exist'; fi
[[ ! -e $scratch && ! -L $scratch ]] || setup 'scratch already exists; choose a new directory'
mkdir -p "$(dirname "$scratch")"
scratch_parent=$(cd "$(dirname "$scratch")" && pwd -P)
scratch=$scratch_parent/$(basename "$scratch")
mkdir "$scratch"
export HOME=$scratch/home XDG_CONFIG_HOME=$scratch/config XDG_STATE_HOME=$scratch/state
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_CONFIG_COUNT
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
repo=$scratch/repo
if [[ -n $source_repo ]]; then
    git clone --shared --no-checkout -- "$source_repo" "$repo" > "$scratch/setup.log" 2>&1 || setup 'clone failed'
    git -C "$repo" checkout --detach HEAD >> "$scratch/setup.log" 2>&1 || setup 'clone checkout failed'
else
    mkdir "$repo"
    git -C "$repo" init -q
fi
git -C "$repo" config user.name 'Groove Scale Check'
git -C "$repo" config user.email scale@example.invalid
git -C "$repo" config core.hooksPath /dev/null
if [[ -z $source_repo ]]; then
    components=$(( (directories + 3) / 4 ))
    for ((n=0; n<components; n++)); do
        printf -v directory '%s/services/team-%03d/component-%05d/src/main/generated' "$repo" "$((n / 100))" "$n"
        mkdir -p "$directory"
    done
    for ((n=0; n<files; n++)); do
        component=$((n % components))
        printf -v path '%s/services/team-%03d/component-%05d/src/main/generated/Source%d.src' "$repo" "$((component / 100))" "$component" "$n"
        printf 'source %s\n' "$n" > "$path"
    done
    mkdir -p "$repo/tools" "$repo/legacy"
    printf '*.src text eol=lf\n*.bat text eol=crlf\nlegacy/** text=auto\n' > "$repo/.gitattributes"
    printf 'echo install\r\n' > "$repo/tools/install.bat"
    printf 'legacy notes\r\n' > "$repo/legacy/notes.txt"
    git -C "$repo" add .
    legacy=$(git -C "$repo" hash-object -w --no-filters legacy/notes.txt)
    git -C "$repo" update-index --add --cacheinfo "100644,$legacy,legacy/notes.txt"
    git -C "$repo" commit -qm 'Synthetic scale baseline'
    printf 'second commit\n' > "$repo/history.src"
    git -C "$repo" add history.src
    git -C "$repo" commit -qm 'Scale history'
fi
git -C "$repo" rev-parse --verify HEAD~1 > /dev/null || setup 'fixture requires HEAD~1'
# Pick a regular source path without modifying conversion rules themselves.
untouched=
git -C "$repo" ls-files -z > "$scratch/tracked-paths"
while IFS= read -r -d '' path; do
    case "$path" in .gitattributes|*/.gitattributes|*.bat) continue ;; esac
    if [[ -f $repo/$path && ! -L $repo/$path ]]; then untouched=$path; break; fi
done < "$scratch/tracked-paths"
[[ -n $untouched ]] || setup 'fixture has no regular tracked file'
field() { sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p" "$1"; }
now() {
    if [[ -n ${EPOCHREALTIME:-} ]]; then
        local stamp=$EPOCHREALTIME fraction
        fraction=${stamp#*.}000
        printf '%s\n' "$(( ${stamp%.*} * 1000 + 10#${fraction:0:3} ))"
    else printf '%s\n' "$(( $(date +%s) * 1000 ))"; fi
}
identity() {
    if stat -c '%i:%y' "$1" 2>/dev/null; then return; fi
    stat -f '%i:%m:%Fm' "$1"
}
# The checker can diagnose the reuse regression on historical v1 binaries too.
"$binary" --json --help > "$scratch/help.json" 2> "$scratch/help.err" || setup 'cannot run binary'
legacy_cli=false
if [[ $(sed -n 's/.*"api_version":\([0-9]*\).*/\1/p' "$scratch/help.json") == 1 ]]; then legacy_cli=true; fi
invoke() {
    local args=() arg duration destroying=false
    if $legacy_cli; then
        while (($#)); do
            arg=$1; shift
            case "$arg" in
                pool) continue ;;
                --workspace-root) arg=--worktree-root ;;
                --reservation-id) arg=--lease-id ;;
                destroy) destroying=true ;;
                --force) if $destroying; then arg=--discard-changes; fi ;;
                --git-timeout)
                    duration=$1; shift
                    case "$duration" in *ms) duration=${duration%ms} ;; *s) duration=$(( ${duration%s} * 1000 )) ;; *m) duration=$(( ${duration%m} * 60000 )) ;; *h) duration=$(( ${duration%h} * 3600000 )) ;; esac
                    args+=(--git-timeout-ms "$duration"); continue ;;
            esac
            args+=("$arg")
        done
    else args=("$@"); fi
    "$binary" --json --repo "$repo" ${args[@]+"${args[@]}"}
}
call() {
    local name=$1; shift
    invoke "$@" > "$scratch/$name.json" 2> "$scratch/$name.err" || fail "$name failed (see $scratch/$name.json)"
}
lease_fields() {
    slot=$(field "$scratch/acquire.json" slot_id)
    id=$(field "$scratch/acquire.json" reservation_id)
    workspace=$(field "$scratch/acquire.json" workspace_path)
    if $legacy_cli; then id=$(field "$scratch/acquire.json" lease_id); workspace=$(field "$scratch/acquire.json" worktree_path); fi
    [[ -n $id && -d $workspace && -n $slot ]] || fail 'invalid acquisition response'
}
acquire() {
    local start
    start=$(now)
    call acquire --pool scale-check "$@" acquire
    elapsed=$(( $(now) - start ))
    lease_fields
}
release() { call release --pool scale-check release --reservation-id "$id" "$@"; }
check_reuse() {
    [[ $(field "$scratch/acquire.json" recovery) == sanitized ]] || fail 'a released slot was not reused in place'
    [[ $slot == "$first_slot" ]] || fail 'sequential tasks changed slots'
    [[ $(identity "$workspace/$untouched") == "$original_identity" ]] || fail 'untouched tracked file changed identity'
    if ((elapsed < fastest)); then fastest=$elapsed; fi
}
call init --pool scale-check pool init --capacity 2 --ready-target 0 --idle-timeout never --workspace-root "$scratch/worktrees"
acquire --git-timeout "$git_timeout"
cold=$elapsed
[[ $(field "$scratch/acquire.json" recovery) == created ]] || fail 'cold acquisition was not created'
first_slot=$slot
original_identity=$(identity "$workspace/$untouched")
release
acquire
fastest=$elapsed
check_reuse
warm=$elapsed
git -C "$workspace" config branch.scale.remote origin
git -C "$workspace" config branch.scale.merge refs/heads/main
release
acquire
check_reuse
config_reuse=$elapsed
release
# Base is resolved in the clone, never in the source repository.
old=$(git -C "$repo" rev-parse HEAD~1)
call acquire --pool scale-check acquire --base HEAD~1
lease_fields
[[ $(field "$scratch/acquire.json" recovery) == sanitized ]] || fail 'base switch rebuilt the checkout'
[[ $(git -C "$workspace" rev-parse HEAD) == "$old" ]] || fail 'base switch selected the wrong commit'
[[ -z $(git -C "$workspace" status --porcelain) ]] || fail 'base switch left changes'
release
acquire
[[ $(field "$scratch/acquire.json" recovery) == sanitized ]] || fail 'return to tip rebuilt checkout'
original_identity=$(identity "$workspace/$untouched")
printf 'scale edit\n' >> "$workspace/$untouched"
printf 'new text\n' > "$workspace/scale-new.txt"
printf '\211PNG\r\n\032\n\000\000data' > "$workspace/scale-new.bin"
cp "$workspace/$untouched" "$scratch/expected-tracked"
cp "$workspace/scale-new.txt" "$scratch/expected-text"
cp "$workspace/scale-new.bin" "$scratch/expected-binary"
status=0
invoke --pool scale-check release --reservation-id "$id" > "$scratch/refused.json" 2> "$scratch/refused.err" || status=$?
[[ $status == 5 ]] || fail 'plain release did not refuse unsaved work with exit 5'
start=$(now)
release --save
save_time=$(( $(now) - start ))
(( $(wc -c < "$scratch/release.json") <= max_error_bytes )) || fail 'saved release response exceeds size limit'
snapshot=$(field "$scratch/release.json" id)
[[ -n $snapshot ]] || fail 'saved release has no snapshot'
call restore --git-timeout "$git_timeout" snapshot restore "$snapshot" --to "$scratch/restored"
compare() {
    cmp "$scratch/expected-tracked" "$1/$untouched" || fail 'tracked bytes changed on restore'
    cmp "$scratch/expected-text" "$1/scale-new.txt" || fail 'text bytes changed on restore'
    cmp "$scratch/expected-binary" "$1/scale-new.bin" || fail 'binary bytes changed on restore'
}
compare "$scratch/restored"
acquire
(cd "$workspace"; repo=$workspace; invoke snapshot restore "$snapshot") > "$scratch/in-place.json" 2> "$scratch/in-place.err" || fail 'in-place restore failed'
# --repo must select the workspace when restoring in place.
compare "$workspace"
release --force
if ((cold >= 5000 && fastest * 100 > cold * max_reuse_percent)); then fail 'fastest reuse exceeds cold acquisition percentage'; fi
count=0
for path in "$scratch"/worktrees/wt-*; do if [[ -d $path ]]; then count=$((count + 1)); fi; done
[[ $count == 1 ]] || fail 'sequential tasks materialised more than one checkout'
status=0
invoke --pool scale-check acquire --base refs/heads/does-not-exist > "$scratch/error.json" 2> "$scratch/error.err" || status=$?
[[ $status != 0 ]] || fail 'invalid revision unexpectedly acquired a workspace'
(( $(wc -c < "$scratch/error.json") <= max_error_bytes )) || fail 'error response exceeds size limit'
call destroy --pool scale-check pool destroy --force
for path in "$scratch"/worktrees/wt-*; do [[ ! -d $path ]] || fail 'destroy left a checkout'; done
{
    printf '%-24s %12s\n' Operation Milliseconds
    printf '%-24s %12s\n' cold-acquire "$cold" warm-acquire "$warm" config-reuse "$config_reuse" fastest-reuse "$fastest" release-save "$save_time"
    printf 'All scale contracts hold.\n'
} | tee "$scratch/report.txt"
cp "$scratch/report.txt" "$scratch_parent/last-report.txt"
if ! $keep; then rm -rf -- "$scratch"; fi
