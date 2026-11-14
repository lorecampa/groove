# Operating Groove

Use `groove --help` and `groove COMMAND --help` for command options, defaults and
examples. This guide covers persistent state, ownership and operator recovery.

## Requirements

Linux or macOS 13+ (Apple Silicon/Intel), Git 2.40+, local storage, and cooperative processes running as the
repository owner. Building needs Rust 1.89+. The base Linux target is kernel 5.4;
minimum-kernel VM coverage is not established. Native macOS CI targets macOS 15;
minimum-version macOS VM coverage is not established. Other platforms fail to compile
with an explicit error.

Both platforms use flock and close-on-exec descriptors.
On Linux, recognized types are ext2/3/4, XFS, Btrfs, F2FS, ZFS, tmpfs and overlayfs.
Overlay backing storage must also be local. Other types, including NFS, CIFS/SMB,
FUSE/virtiofs, Coda, AFS, 9p and Ceph, are rejected rather than assumed local.
macOS requires local APFS, including case-sensitive APFS; HFS+, network and FUSE
filesystems are unsupported. Passing
that check does not certify flock/fsync or power-loss durability. Managed state
must be owned by you and not writable by group/others.
For a local filesystem outside the recognized list, such as bcachefs, JFS or
exFAT, explicitly set `GROOVE_ALLOW_UNSUPPORTED_FILESYSTEM=1` for Groove commands
after checking that the storage supplies local flock, atomic rename and fsync
semantics. This bypasses only filesystem-type qualification; ownership, mount,
locking and path checks still apply. Other values do not enable the override.
The override does not establish network-filesystem correctness or durability.
APFS filenames must be valid UTF-8. Names that differ only by case or Unicode
normalization may collide on the destination filesystem; repositories must use
names that the selected volume can represent distinctly.

Linux cleanup checks the visible mount table and rejects mounts within a slot,
including bind mounts with the same device ID. macOS acquisition has no mount-table
safeguard. Removal checks device boundaries, which still miss same-device mounts.
On macOS, unmount every mounted directory beneath a slot before acquisition,
background preparation, eviction or destruction. Mount changes concurrent with
cleanup remain outside the cooperative-writer guarantee on both platforms.

SHA-1/SHA-256 and ordinary, linked and bare repositories are supported. Pool
checkouts reject submodules, sparse checkout, worktree-specific configuration and
repository-configured content filters such as LFS. User/system filter definitions
are rejected when the requested commit's paths select a filter; merely installing
an unused global filter does not block an ordinary checkout.
Filter validation uses the requested commit's attribute files in a scratch tree,
so the caller's subdirectory and background-worker directory do not alter it.
Its private index lives outside that tree on repository storage, avoiding tracked
path collisions and system temporary-directory capacity limits.
Driver names must contain only ASCII letters, digits, dots, underscores or hyphens.
Maintenance clears repository-selection overrides and disables hooks, fsmonitor, automatic maintenance, prompts and
system/global Git configuration. Baselines follow controlled Git checkout
semantics, including normal attributes/line-ending conversion.

## Setup and storage

Follow the [installation guide](../README.md#install), then initialize from your
repository:

```sh
groove init --capacity 4
# Alternative settings, chosen once on a new pool:
groove --repo /other/repo init --capacity 4 --cleanup warm \
  --worktree-root /absolute/dedicated/groove-worktrees
```

Initialization creates fixed slot metadata; acquisition materializes worktrees.
A pool requires a first commit, including when no ready spares are requested.
Standalone snapshots remain available before that first commit.
New pools default to zero ready spares. Enable preparation explicitly with
`--ready-target N` or `pool_defaults.ready_target`; existing pool settings are
unchanged. Idle eviction still defaults to seven days and runs opportunistically.
A ready pool rejects every repeated init. Its checkout root stays fixed until
the pool is deleted and reinitialized.
Use `groove config set --capacity 8 --cleanup warm` to grow/change future cleanup.
Updates validate all values before publication; shrinking is unsupported. Active
reservations and permanent locks survive. Retry matching interrupted init with
`--resume`; use `config show` after a lost response if initialization already completed. A custom root must be dedicated, initially empty, owned by you
and outside Git administration. Relative paths resolve from the caller's directory.


State stays in `<GIT_COMMON_DIR>/groove/`; new checkout roots default to
`~/.worktrees/<project>-<repo-id>/<pool>` on Linux and macOS. Change the parent with
`groove config global set worktrees.root=/absolute/worktrees`, or choose an exact
pool root with `init --worktree-root`. See `init --help` for naming and precedence.
Existing pools retain their recorded roots, including legacy roots in `.git`.
To relocate one, stop all writers, preserve results and release every reservation,
then destroy and reinitialize with the desired root. Never move checkout directories
or edit the manifest to relocate a pool. Do not repurpose a root while its pool exists.
Stop all owners before restoring a pool.
Restoring permanent locks onto new inodes invalidates manifest identities;
Groove refuses automatic replacement.

Use one Groove version across the repository's pools. Groove checks durable format
compatibility when opening records. Use a compatible binary for unsupported formats;
never delete or rewrite coordination metadata to force an upgrade.
Current handles and acquisition waiters refresh settings, while
already-started preparation may use its observed policy.

## Workflow pools

Use a named pool when workflows need separate capacity, cleanup or caches:

```sh
groove --pool frontend init --capacity 3 --cleanup warm
groove --pool backend init --capacity 2 --cleanup pristine
groove pools
groove --pool frontend acquire --task fix-navigation --request-key navigation-1
groove --pool frontend release --save --task fix-navigation
```

Pass the same `--pool` on later pool commands. Omission always selects `default`,
even from a named checkout; missing pools never fall back to another namespace.
Labels/keys can be reused independently across pools. Named state lives under
`<common>/groove/pools/<name>/`; default retains its original layout and keys.
All linked checkouts share the catalog. Snapshots, branches, objects and Git configuration
are shared.

Checkout roots cannot overlap another pool's published or pending root. Deleted
pools retain their current root claim until reinitialized with a different root.
Capacity is per pool, so total repository capacity is the sum of the pools; it
does not bound CPU, processes or storage.

## Deleting and recreating a pool

Release every task and stop all writers, then run from outside the pool:

```sh
groove destroy --dry-run
groove destroy
# If released workspaces still contain edits, ignored caches or broken checkouts:
groove destroy --discard-changes --dry-run
groove destroy --discard-changes
groove --pool frontend destroy --dry-run
groove --pool frontend destroy --discard-changes
```

With several non-deleted pools, destruction requires explicit `--pool NAME`, even
for default. Only that namespace is removed. `--pool-id` checks its generation;
it never searches for or selects a different pool. Healthy sibling checkouts and
workers remain usable during another pool's interrupted initialization/deletion.
Short Git registration/root updates still serialize through the repository admin lock.

The discard flag acknowledges file loss only. Descriptors, durable reservations,
orphan execution records, observed processes and unsafe paths still block deletion.
The process scan includes your shell and the command itself. Ignored files are
removed even for warm pools. Unknown files beside slots are retained, and unknown
artifact entries cause an error before removal starts.

Deletion removes configured workspaces, exact native registrations and disposable
artifact caches. Permanent locks, journals, saved-release receipts, execution history/logs,
branches and snapshots survive, except that completed saved-release receipts are
pruned when their snapshot no longer exists and no pin retirement is pending.
Damaged receipts are preserved. Request-key tombstones and permanent request/label
locks are retained to honor never-expiring retry identities and lock-inode guarantees.
The checkout root itself survives. Snapshot commands remain
available. Legacy logs and history are retained without cleanup commands.
No Git object collection is run. Keeping a small coordination area is intentional:
deleting permanent locks could create two independent owners for the same path.

After deletion is durable, Groove removes the root's `.groove-owner` claim if no
active or initializing pool still uses that root. A completed destroy retry also
finishes claim removal and never removes a later repository's claim. The marker
is a namespace symlink, not a permanent lock. Retained deleted manifests still
reserve their root within the original repository until reinitialization.

If the original repository was deleted without destroying its pools, inspect
the marker with `readlink /absolute/root/.groove-owner`. Confirm that its owning
Git directory has been permanently removed, stop all writers in the root, and
preserve any remaining results outside it. Then remove only that `.groove-owner`
symlink with `unlink /absolute/root/.groove-owner`. Never remove permanent lock
files or another live repository's marker. A new pool still requires an empty root.

An interruption leaves the pool disabled. Repeat the same destroy command, including
its discard flag, to resume. Completed retries succeed. For automation, pass
`--pool-id UUID` from the preview/result so a retry cannot target a replacement pool.
After completion, `init` can choose new capacity, cleanup and checkout-root settings;
old library/snapshot handles are rejected and completed request keys remain tombstoned.
Destroy also cancels interrupted initialization without creating worktrees.
If metadata or an original default-root location is damaged, repair/restore it
before deletion; discard never authorizes removing foreign paths.

## Task lifecycle

[The portable skill](../skills/groove/SKILL.md) covers acquire, retry, work and
release --save. Normal commands select a reservation with `--task` (its label) or `--lease-id`; slot IDs
identify reusable directories for diagnostics.
Reservations survive process exit and restart, with no TTL or automatic reclamation.

```sh
groove list
groove list --slots
groove show --slot wt-0001
```

`list` lists durable reservations. `list --slots` includes available, reserved,
locked, quarantined and unhealthy slots, plus task labels and acquisition times. These are nontransactional observations; PIDs/age never prove
ownership or stopped writers. A live in-process library lease need not appear
in `list`.

Run commands directly in the reserved checkout. Your runner or terminal owns
cancellation, timeouts and logs. Stop external editors, watchers and commands
before release.
`Lease::run_worker` waits only for its direct child; retained descriptors can keep
a slot busy indefinitely. Closing a descriptor while continuing to write creates
the opposite risk. Never explicitly unlock until all writers stop.

`release --save` saves a pinned local checkout snapshot before releasing. Otherwise save
results outside all slots (for example on a branch you create), then `release`.
Neither publishes results. The next acquisition may discard all files in the slot.

## Cleanup and shared state

A forced release without a snapshot can leave a slot available but unsuitable
for background preparation or eviction. The worker cannot distinguish residual
unsaved files from edits made after release, so it refuses both. An explicit
acquisition still normalizes the slot under its ordinary ownership checks.
Use `release --save` when background preparation should preserve a recoverable
checkpoint before discarding these files.

`pristine` removes tracked edits, untracked files and ignored files on acquisition.
`warm` retains ignored files only in healthy reused checkouts; they are unverified
residue and may be lost during rebuild. External caches can be used with pristine
slots. Capacity bounds reservations, not process count, artifacts or disk usage.

Acquisition preserves inode and mtime for untouched tracked files across leases.
It uses ctime since the last verified checkout to identify paths that need
rewriting, including chmod, xattr, hardlink, utimes and directory-swap changes.
This trusts that tasks cannot backdate ctime without root privileges or a clock
change. Missing verification/index state, hidden-edit flags, attribute-rule
changes or an oversized touched-path inventory force a complete rewrite.
Reuse also requires that the checkout and marker share one recognized filesystem;
cross-filesystem markers and override-only storage always use a full rewrite.
Warm caches may therefore benefit timestamp-based builds, but retained outputs
remain unverified. The example below uses ccache to validate content reuse.

Prefer pristine cleanup for independent tasks. Keep dependency downloads or
compiler results in an external cache supported by your build tool, outside all
pool roots, and let the tool validate compatibility. Warm cleanup is useful when
your workflow accepts and validates the previous task's ignored files. Retaining
an output does not establish that it matches the current source, toolchain or
build flags. A lockfile identifies dependencies, not arbitrary generated output.

For example, this shell script uses a C project configured for `make` and `ccache`:

```sh
set -eu
# In the repository; this pool uses the default pristine cleanup.
groove --pool builds init --capacity 2
lease=$(groove --json --pool builds acquire --task fix-parser)
workspace=$(printf '%s\n' "$lease" | jq -er 'select(.ok == true) | .result.worktree_path')
lease_id=$(printf '%s\n' "$lease" | jq -er '.result.lease_id')
# The compiler cache survives workspace cleanup; object files stay in this slot.
(cd "$workspace" && CCACHE_DIR="$HOME/.cache/ccache" CCACHE_NOHARDLINK=1 make CC='ccache cc')
# Export required ignored outputs and stop every writer before releasing.
groove --pool builds release --save --lease-id "$lease_id"
```

Check the acquisition's exit status and `ok` before using its paths in automation.
If the build fails, retain the reservation to inspect or fix it before release.
Your runner owns command cancellation, timeouts and logs. Saved snapshots exclude
ignored files, including ignored build outputs.

Use the build tool's supported cache-sharing interface. For ccache, keep
[`hard_link` disabled](https://ccache.dev/manual/4.7.5.html#config_hard_link):
modifying a hardlinked object can also modify the cached entry. Do not share
mutable workspace output directories across reservations.
When a disposable cache is missing, the build must be able to rebuild from source;
its absence requires no Groove coordination-metadata repair.

Worktrees share Git objects, refs and configuration. Branches created by agents
remain in the repository; Groove does not manage/delete them. Coordinate shared
state changes and avoid branch-name collisions. Worktrees are not security sandboxes.
Snapshots survive slot reuse but remain local; ordinary branch pushes do not back
them up. Request journals and saved-release receipts have no automatic expiry.

## Recovery

Inspect the affected reservation with `list --slots` or `show` and follow the
reported next action. Command `--help` owns error-specific recovery instructions.
Retry a lost acquisition with its original key and
inputs; retry a saved release with its original lease ID and pool. Never infer
ownership from labels, PIDs or age.

If a repository was moved, restore its recorded location: pool manifests and Git
registrations contain absolute paths. Groove does not relocate pools. Restore
missing worktrees or explicitly acknowledge lost contents through the relevant
command; never repair by deleting protection.

Recovery normalizes or rebuilds under slot/admin locks, then verifies the baseline.
Native registrations use `git worktree add --lock` against external pruning,
independently of flock. Only registrations with exact owned backlinks are removed;
Groove never sweeps shared Git locks or unrelated metadata.
Recognizable foreign repositories or moved linked worktrees occupying a slot,
and multiple registrations claiming one slot, refuse acquisition and destruction.
Safely restore the occupant's original location or move it outside the pool before
retrying. `--discard-changes` does not bypass this identity check. Normal contents
of a released Groove checkout, including task-created nested repositories, remain
disposable under its cleanup policy.

Never delete/replace lock or protection records to force capacity. Read-only
`/proc/*/fd` investigation on Linux, or process inspection on macOS, may help
locate owners but is incomplete because of
permissions and races. Missing PIDs/descriptors do not authorize cleanup. If
ownership is uncertain, leave protection intact.

The release/destruction process scan observes accessible same-user processes and
release excludes the caller, its ancestors and its process group, including
pipeline peers. Destruction excludes none of these. Stop all writers explicitly;
group exclusion does not establish quiescence. The scan
compares visible paths. Other users, mount namespaces and alternate mount paths
can hide workspace users. A successful scan does not establish that all writers
stopped: callers must stop their workers, watchers and descendants before release.

Managed-path symlink and registration checks protect cooperating workflows, not
malicious same-user directory races. SIGKILL tests establish process-crash
recovery, not storage/power-loss guarantees. See [testing](testing.md).

## Running project commands

Groove does not read `.groove.toml` or run setup profiles, recipes or agents.
Use the reserved checkout directly with your terminal or agent runner.
`show --details` and `list --details` opt into local Git facts.

## Upgrading from managed execution

Before upgrading, use the previous Groove version to stop managed commands.
For orphaned runs, independently stop every writer and recover with that version.
Legacy execution locks and records still block release, saved release and deletion;
never delete them to force reuse. Completed logs, history and artifacts remain on
disk. This version provides no run-history or log-cleanup commands.

Repository compatibility is listed under [requirements](#requirements).
