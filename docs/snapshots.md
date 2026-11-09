# Local snapshots

`groove snapshot` saves checkpoints without moving HEAD or changing working files
or the real Git index. No pool initialization, remote, or first commit is needed.
Commands use Groove's normal JSON envelope: `result.snapshot_result` contains the
operation result and `result.elapsed_ms` records command duration.

```sh
groove snapshot save "before refactor"
groove snapshot list
groove snapshot show UUID
groove snapshot diff UUID                 # versus current files, including untracked
groove snapshot diff UUID OTHER_UUID
groove snapshot restore UUID --to ../recovery
groove snapshot restore UUID              # automatic safety checkpoint first
```

Save includes staged state, unstaged changes, deletions, executable modes,
symlinks, and untracked non-ignored files. Ignored files and empty directories
are excluded, including custom user/system Git ignore rules. Root paths must be UTF-8; individual filenames may contain arbitrary
non-NUL bytes. A description and optional `--session ID` associate work with a task.

Stop external writers before save/restore. Capture checks HEAD, index bytes,
changed-path lists, untracked-path lists and changed-file metadata for concurrent
modification. This catches common races, but is not an atomic filesystem snapshot
and cannot prove consistency against arbitrary writers. A detected race fails;
the caller can retry after stopping writers.

Changed files exceeding 100 MiB fail before publication. Use
`--max-file-bytes BYTES` to change the threshold; `0` explicitly removes it.
Large binary edits can consume substantial space: Git initially writes whole new
blobs, with possible delta compression during subsequent packing.

## Recovery

Recovery to a new directory requires that directory not exist. For a snapshot
with a base commit, this creates a detached linked worktree. For a snapshot from
before the first commit, it creates an independent repository and fetches the
snapshot objects locally, preserving an unborn HEAD. Saved files and staging state
are restored in either case.

In-place restore requires HEAD to equal the saved base. It creates a safety
snapshot before modifying files; failures after that point report its ID in error
details. Files captured by the safety snapshot but absent from the target are
removed. Ignored-file collisions are rejected before checkout. Ignored files
unrelated to the restore remain untouched. Restoration is not transactional
against process death: use the safety checkpoint to recover interrupted work.

## Retention and storage

```sh
groove snapshot pin UUID
groove snapshot unpin UUID
groove snapshot delete UUID                # pinned snapshots require --force
groove snapshot prune --older-than 30d --dry-run
groove snapshot prune --keep-last 20
groove snapshot retention --keep-last 20   # opt in to automatic retention
groove snapshot retention                 # show settings
groove snapshot retention --disable
groove snapshot compact
```

Age and count constraints use AND semantics when combined. Count is ranked per
workspace and includes pinned entries; pins are never selected for pruning.
Automatic retention is disabled by default and runs after a successful save,
always preserving the snapshot just saved. With no command-line rules, `prune`
uses the configured policy. With neither rules nor configuration it does nothing.
A retention failure is logged without disguising a successfully saved checkpoint.

Snapshots live under `refs/groove/snapshots/<UUID>`, pins under
`refs/groove/snapshot-pins/<UUID>`, and lock/retention settings under
`<GIT_COMMON_DIR>/groove-snapshots/`. All linked worktrees share this namespace.
The manifest records the originating Git directory. This deliberately uses a
repository-wide snapshot lock to coordinate save, delete, restore and compaction.
Git mutation children inherit the lock so killing the parent cannot release it
while a child is still writing. Scratch indexes left by a killed process are
removed by the next lock owner, keeping interrupted captures from accumulating
large temporary files.

A snapshot commit has the original HEAD as its parent (or no parent when unborn).
Its tree contains `index` and `work` subtrees and an `untracked` manifest blob. Its
commit message contains versioned JSON. There is no parent link between snapshots;
retaining a recent snapshot does not retain expired older snapshots. Repeated saves
reuse file blobs and unchanged trees, but create small new metadata commits.

Deleting refs makes exclusive objects eligible for collection. It does not
immediately release space. `compact` runs normal Git GC against the shared object
database, with normal expiration safeguards. Branches, reflogs and other refs can
keep objects alive. There is no aggressive immediate prune or strict byte quota.
The maximum-file limit and opt-in retention bound ordinary growth, not every
possible repository's total object size. Routine save/delete never run GC.

Snapshots are local and ordinary branch pushes do not publish them. Deleting the
repository's Git directory loses them. Remote export/backup is not implemented.

## Supported boundaries

Conflicts, submodules (including clean gitlinks), nested untracked repositories,
assume-unchanged or manual skip-worktree flags, intent-to-add entries,
configured content filters, automatic CRLF conversion,
and content-converting attributes are rejected. This prevents silent loss of
conflict stages, external object content or byte-exact working-file state.
Sparse checkout entries outside the working directory remain in captured trees;
restoration follows the destination's sparse checkout configuration. The pool's
more restrictive supported-configuration rules remain unchanged.

No hooks or external diff/textconv commands are executed. The operation timeout
applies to each Git subprocess. Large Git responses have a bounded 128 MiB limit;
exceeding it fails explicitly. This is intended for local Linux filesystems and
Git 2.40+, matching Groove's platform contract.

## Testing and benchmarks

```sh
cargo test -p groove-core --test snapshots --locked
cargo test -p groove-cli --test snapshots --all-features --locked
cargo build -p groove-cli --release --locked
python3 scripts/benchmark-snapshots.py \
  --real-repo /path/to/your/repository --sizes 100000 1000000 \
  --runs 5 --output /tmp/snapshot-benchmark.json
```

The benchmark copies source Git objects (`clone --no-hardlinks`), verifies the
source's HEAD, index, status, tracked diff and refs before/after, and removes its
disposable clones on completion. Source uncommitted changes are not copied.
It exercises save and both restore modes on the real repository's committed
checkout. Synthetic files share small baseline contents to isolate path/index
costs; they do not model large binary histories. Timings include process startup;
first-save results do not imply cold OS caches. GNU time records peak RSS.

The 1M-file benchmark and its recovered checkout require several million inodes.
Use `--temp-root /path/on/a/large/local/disk`; a typical temporary tmpfs can run
out of inodes before the fixture is ready. Timings include per-Git-step
diagnostics for profiling. See [benchmark results](snapshot-benchmarks.md).

For deterministic high-load regressions, pinned benchmark environments and CI
artifact retention, use the [snapshot lab](../benchmarks/snapshots/README.md).
