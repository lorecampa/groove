# Local snapshots

`groove snapshot save` saves checkpoints without moving HEAD or changing working
files or the real Git index. No pool initialization, remote, or first commit is
needed. A working tree is required; a bare repository cannot be captured.
Each snapshot command's `--help` explains its JSON envelope and result fields.

Use `groove snapshot --help` and each subcommand's `--help` for command options,
defaults and examples. This guide explains capture, recovery and retention.

For a pooled task, `groove release --save --task NAME` saves and pins a snapshot before
releasing its checkout. It retains protection on capture failure and a durable
retry receipt on success; see [reservation retries and recovery](../skills/groove/references/recovery.md).

```sh
groove snapshot save "before refactor"
groove snapshot list
groove snapshot show ID
groove snapshot diff ID            # versus current files, including untracked
groove snapshot diff ID OTHER_ID
groove snapshot diff ID --max-file-size 0  # unlimited current-file capture
groove snapshot restore ID --to ../recovery
groove snapshot restore ID         # automatic safety checkpoint first
groove snapshot restore ID --max-file-size 0  # unlimited safety capture
```

Save includes staged state, unstaged changes, deletions, executable modes,
symlinks, and untracked non-ignored files. Ignored files and empty directories
are excluded, including custom user/system Git ignore rules. Special files such as
FIFOs, sockets and devices outside ignored paths are refused: Git does not
preserve them as ordinary untracked content. Ignored special files are excluded.
Export or remove non-ignored special files before saving; export
needed ignored artifacts separately before release --save. Root paths must be UTF-8;
Linux filenames may contain arbitrary non-NUL bytes. macOS APFS requires valid UTF-8
filenames. A description and optional `--session ID` associate work with a task.

Snapshots preserve actual working-file bytes, executable bits and symlinks even
when Git preferences enable line-ending conversion or ignore permissions/links.
Groove overrides `core.autocrlf`, `core.eol`, `core.filemode` and `core.symlinks` only for its
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

## Comparing changes

Save a checkpoint before a task to use its current files as the baseline, including
edits and eligible untracked files already present. Later, `groove snapshot diff ID`
shows changes since that checkpoint. A second snapshot ID compares two saved states.
This is useful when the task starts with uncommitted work that should stay outside
its patch.

Diff exports an uncolored, binary-capable Git patch with fixed `a/` and `b/`
prefixes, independently of Git display preferences. Redirect human output to save
the patch, or extract the patch string from the documented JSON result. If JSON
contains `patch_encoding: "base64"`, decode `patch` before applying it; otherwise
`patch` is UTF-8 text. Human output retains arbitrary content bytes, and filenames
use Git's quoted patch syntax. Apply it
to files matching the baseline. Equal file trees produce empty output. The patch
compares working files; use restore to recover the saved staging state as well.
Export leaves HEAD, the real index and working files unchanged.

## Recovery

Recovery to a new directory requires that directory not exist and be outside
the source working tree and all pool roots, including through parent symlinks.
This prevents introducing nested repositories that block subsequent saves. For a snapshot
with a base commit, this creates a detached linked worktree. For a snapshot from
before the first commit, it creates an independent repository and fetches the
snapshot objects locally, preserving an unborn HEAD and its original branch name.
Saved files and staging state
are restored in either case.

If recovery fails after creating the destination, Groove removes its checkout
and registration before returning the original error. Cleanup failures include
the destination and a secondary error; stop writers before inspecting and
removing that partial recovery. Process death can still interrupt cleanup.

In-place restore requires HEAD to equal the saved base. It creates a safety
snapshot before modifying files; failures after that point report its ID in error
details. Files captured by the safety snapshot but absent from the target are
removed. Ignored-file collisions are rejected before checkout. Ignored files
unrelated to the restore remain untouched. Restoration is not transactional
against process death: use the safety checkpoint to recover interrupted work.

## Deletion and storage

```sh
groove snapshot delete ID                # snapshots pinned by release --save need --force
```

`release --save` pins the snapshot it saves, so deleting it needs `--force`.
Interrupted attempts remain pinned until a retry completes release. The retry
then unpins its superseded snapshots, retaining their files and refs for ordinary
recovery or deletion. A durable receipt resumes pin cleanup after interruption;
the final saved-release snapshot remains pinned.
Unreadable, corrupt or inconsistent pin metadata refuses snapshot reads and
deletion, even with `--force`. Repair the reference before retrying. Deletion
checks the observed snapshot and pin in one ref transaction; changed references
are preserved.

Snapshots live under `refs/groove/snapshots/<UUID>`, pins under
`refs/groove/snapshot-pins/<UUID>`, and the lock under
`<GIT_COMMON_DIR>/groove-snapshots/`. All linked worktrees share this namespace.
The manifest records the originating Git directory. This deliberately uses a
repository-wide snapshot lock to coordinate save, delete and restore.
Concurrent operations wait up to `--git-timeout` for this
lock, including snapshots taken by `release --save`. A lock timeout returns a retryable
error and keeps task reservations and files protected; retry the same operation.
Git mutation children inherit the lock so killing the parent cannot release it
while a child is still writing. Scratch indexes left by a killed process are
removed by the next lock owner, keeping interrupted captures from accumulating
large temporary files.
An interrupted or timed-out Git mutation can leave native Git lockfiles behind.
Inspect the interrupted operation and stop its writers before repairing those
locks. Groove does not sweep Git locks; never replace its permanent lock files.

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
repository's Git directory loses them. `groove pool destroy` preserves snapshots and
pins; recover with restore from the main or another surviving checkout. Restore
destinations must be outside the pool checkout root. Snapshot handles opened in a
managed slot reject a subsequently reinitialized pool. All named pools share the
snapshot namespace; `--pool` does not filter snapshot commands. Services opened in
named slots retain that slot's actual admission, and recovery destinations are
checked against every pool root under repository-wide administration. Remote export/backup is
not implemented.

## Supported boundaries

Unfinished merges (including conflict-free merges), rebases, am, cherry-picks,
reverts, bisects, conflicts, submodules
(including clean gitlinks), nested untracked repositories,
assume-unchanged or manual skip-worktree flags, intent-to-add entries,
and active filters, ident, working-tree-encoding and eol=crlf are rejected. Capture
checks conversion attributes on saved paths. Explicit text, legacy crlf and eol=lf rules
require LF working bytes and byte-preserving staging. Physical EOL checks also
reject CRLF text hidden by Git's clean status; lone carriage returns in explicit text
are refused before publication. Saving a .gitattributes file widens validation to the whole tree
and rejects active text/conversion rules there. Restore rejects carriage returns
in saved text blobs under saved or effective destination rules.
Automatic text rules (`text=auto`) preserve binary data, lone carriage returns
and CRLF already stored in the index. Checkouts that write LF-index text as CRLF
(`core.autocrlf=true` or `core.eol=crlf`) cannot be snapshotted.
Installing Git LFS or defining an unused filter does not block snapshots. Restore
checks paths differing from the snapshot base against both saved and effective destination attributes
before materializing files, including recovery with `--to`. This prevents silent loss of
conflict stages, external object content or byte-exact working-file state.
Sparse checkout entries outside the working directory remain in captured trees;
restoration follows the destination's sparse checkout configuration. The pool's
more restrictive supported-configuration rules remain unchanged.
Unchanged base paths can use converting rules, such as `eol=crlf`, and follow
ordinary checkout behavior. Attribute and ignored-collision preflight failures
leave no safety checkpoint. Foreign refs without snapshot UUID names are ignored
by listing; malformed snapshots with valid IDs still fail explicitly.

No hooks or external diff/textconv commands are executed. The operation timeout
applies to each Git subprocess and each snapshot lock wait. Restore tree inventories
stream through temporary files on the snapshot-state filesystem and bounded
attribute batches, avoiding large inventories in a small RAM-backed /tmp. Other large Git
responses have a bounded 128 MiB limit;
exceeding it fails explicitly. This is intended for supported local Linux filesystems
or local APFS on macOS and
Git 2.40+, matching Groove's platform contract.

## Testing and benchmarks

See [testing](testing.md) for validation gates. Discover benchmark workloads with
`cargo run -p groove-bench --locked -- profiles`.

Snapshot storage is guarded deterministically by `groove-core`'s
`snapshot_objects` tests, which run in every required check. For SHA-1 and SHA-256
repositories they compare the complete object set before and after clean,
unchanged, edited, reverted, deleted, staged, untracked and in-place-restore
captures. Each save may write only new file contents, the trees on changed paths,
its wrapper tree and one commit. Unchanged and reverted states write only the
commit; ignored files and already-stored blobs are never written. Benchmark
object-growth metrics characterize size on large repositories but do not gate.

The lab uses complete versioned profiles with disposable deterministic repositories,
independent Git oracles, repeated restore/save measurements and paired baseline
comparisons. Each invocation writes one self-contained JSON report; benchmark
results are not checked in or published. For workload choices, hermetic builds,
host requirements, native macOS limitations and scale regressions, use the
[benchmark lab](../benchmarks/snapshots/README.md).

## Storage reporting

`groove snapshot storage` reports repository-wide snapshot and pin counts,
oldest/newest creation times, and loose/packed Git object storage. Git's rounded KiB
measurements are converted to bytes; they include ordinary history and shared
objects, exclude alternate object stores, and do not measure exact snapshot
attribution or reclaimable space. Concurrent Git maintenance can change totals.

Use `groove snapshot storage --largest 10` to inspect the largest unique blobs
in saved index and working trees, including unchanged base files. The CLI accepts at most 1000 entries. The default,
`--largest 0`, skips tree enumeration. The optional scan streams inventories to
temporary files on the snapshot-state filesystem, retaining only the requested
largest blobs and never reading their contents. Logical sizes are uncompressed
content lengths, independent of packing and compression. Shared objects appear
once, sorted by descending size then ascending object ID, with one representative
snapshot/path rather than exhaustive provenance. Representatives use lexical
snapshot ID order, index before work, and Git tree path order. Non-UTF-8 paths
are parsed byte-for-byte but displayed with replacement characters, which can
make distinct paths look alike. See `groove snapshot storage --help` for details.
