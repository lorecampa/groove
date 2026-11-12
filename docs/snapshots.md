# Local snapshots

`groove snapshot save` saves checkpoints without moving HEAD or changing working
files or the real Git index. No pool initialization, remote, or first commit is
needed. With `--json`, results use the normal envelope (see [protocol](protocol.md)).

Use `groove snapshot --help` and each subcommand's `--help` for command options,
defaults and examples. This guide explains capture, recovery and retention.

For a pooled task, `groove finish --task NAME` saves and pins a snapshot before
releasing its checkout. It retains protection on capture failure and a durable
retry receipt on success; see [finish and recovery](execution.md#checkpoint-and-release).

```sh
groove snapshot save "before refactor"
groove snapshot list
groove snapshot show ID
groove snapshot diff ID            # versus current files, including untracked
groove snapshot diff ID OTHER_ID
groove snapshot restore ID --to ../recovery
groove snapshot restore ID         # automatic safety checkpoint first
```

Save includes staged state, unstaged changes, deletions, executable modes,
symlinks, and untracked non-ignored files. Ignored files and empty directories
are excluded, including custom user/system Git ignore rules. Special files such as
FIFOs, sockets and devices are also excluded: Git does not enumerate them as
ordinary untracked content. Export needed special/ignored artifacts separately
before finish; a successful snapshot does not preserve them. Root paths must be UTF-8;
Linux filenames may contain arbitrary non-NUL bytes. macOS APFS requires valid UTF-8
filenames. A description and optional `--session ID` associate work with a task.

Snapshots preserve actual working-file bytes, executable bits and symlinks even
when Git preferences enable line-ending conversion or ignore permissions/links.
Groove overrides `core.autocrlf`, `core.filemode` and `core.symlinks` only for its
snapshot subprocesses; it does not change your Git configuration or real index.

Stop external writers before save/restore. Capture checks HEAD, index bytes,
changed-path lists, untracked-path lists and changed-file metadata for concurrent
modification. This catches common races, but is not an atomic filesystem snapshot
and cannot prove consistency against arbitrary writers. A detected race fails;
the caller can retry after stopping writers.

Changed files exceeding the capture limit fail before publication. See
`groove snapshot save --help` for the limit and its override.
Large binary edits can consume substantial space: Git initially writes whole new
blobs, with possible delta compression during subsequent packing.

## Recovery

Recovery to a new directory requires that directory not exist. For a snapshot
with a base commit, this creates a detached linked worktree. For a snapshot from
before the first commit, it creates an independent repository and fetches the
snapshot objects locally, preserving an unborn HEAD and its original branch name.
Saved files and staging state
are restored in either case.

In-place restore requires HEAD to equal the saved base. It creates a safety
snapshot before modifying files; failures after that point report its ID in error
details. Files captured by the safety snapshot but absent from the target are
removed. Ignored-file collisions are rejected before checkout. Ignored files
unrelated to the restore remain untouched. Restoration is not transactional
against process death: use the safety checkpoint to recover interrupted work.

## Deletion and storage

```sh
groove snapshot delete ID                # snapshots pinned by finish need --force
```

`finish` pins the snapshot it saves, so deleting it needs `--force`.

Snapshots live under `refs/groove/snapshots/<UUID>`, pins under
`refs/groove/snapshot-pins/<UUID>`, and the lock under
`<GIT_COMMON_DIR>/groove-snapshots/`. All linked worktrees share this namespace.
The manifest records the originating Git directory. This deliberately uses a
repository-wide snapshot lock to coordinate save, delete and restore.
Concurrent operations wait up to `--git-timeout-ms` for this
lock, including snapshots taken by `finish`. A lock timeout returns a retryable
error and keeps task reservations and files protected; retry the same operation.
Git mutation children inherit the lock so killing the parent cannot release it
while a child is still writing. Scratch indexes left by a killed process are
removed by the next lock owner, keeping interrupted captures from accumulating
large temporary files.

A snapshot commit has the original HEAD as its parent (or no parent when unborn).
Its tree contains `index` and `work` subtrees and an `untracked` manifest blob. Its
commit message contains versioned JSON. There is no parent link between snapshots;
keeping a recent snapshot does not keep older deleted ones alive. Repeated saves
reuse file blobs and unchanged trees, but create small new metadata commits.

Deleting refs makes exclusive objects eligible for collection. It does not
immediately release space; Git's normal garbage collection (`git gc`) reclaims it
with its usual expiration safeguards. Branches, reflogs and other refs can keep
objects alive. There is no byte quota; the maximum-file limit bounds ordinary
growth. Groove never runs GC itself.

Snapshots are local and ordinary branch pushes do not publish them. Deleting the
repository's Git directory loses them. `groove destroy` preserves snapshots and
pins; recover with restore from the main or another surviving checkout. Restore
destinations must be outside the pool checkout root. Snapshot handles opened in a
managed slot reject a subsequently reinitialized pool. All named pools share the
snapshot namespace; `--pool` does not filter snapshot commands. Services opened in
named slots retain that slot's actual admission, and recovery destinations are
checked against every pool root under repository-wide administration. Remote export/backup is
not implemented.

## Supported boundaries

Conflicts, submodules (including clean gitlinks), nested untracked repositories,
assume-unchanged or manual skip-worktree flags, intent-to-add entries,
and active content-converting attributes (including filters) are rejected. Merely
installing Git LFS or defining an unused filter does not block snapshots. Restore
checks the saved paths against both saved and effective destination attributes
before materializing files, including recovery with `--to`. This prevents silent loss of
conflict stages, external object content or byte-exact working-file state.
Sparse checkout entries outside the working directory remain in captured trees;
restoration follows the destination's sparse checkout configuration. The pool's
more restrictive supported-configuration rules remain unchanged.

No hooks or external diff/textconv commands are executed. The operation timeout
applies to each Git subprocess and each snapshot lock wait. Large Git responses have a bounded 128 MiB limit;
exceeding it fails explicitly. This is intended for supported local Linux filesystems
or local APFS on macOS and
Git 2.40+, matching Groove's platform contract.

## Testing and benchmarks

```sh
cargo test -p groove-core --test snapshots --locked
cargo test -p groove-cli --test snapshots --all-features --locked
cargo build -p groove-cli --release --locked
cargo run -p groove-bench --locked -- profiles
cargo run -p groove-bench --locked -- benchmark snapshot-incremental
cargo run -p groove-bench --locked -- check snapshot-scale
```

The lab uses complete versioned profiles with disposable deterministic repositories,
independent Git oracles, repeated restore/save measurements and paired baseline
comparisons. Each invocation writes one self-contained JSON report; benchmark
results are not checked in or published. For workload choices, hermetic builds,
host requirements, native macOS limitations and scale regressions, use the
[benchmark lab](../benchmarks/snapshots/README.md).
