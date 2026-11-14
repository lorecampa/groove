# Architecture

Groove provides repository-local pools of reusable Git worktrees. The development
protocol is v1; each command's `--help` derives its output contract from the
bundled response schema, and
[testing](testing.md) records validation boundaries.

## Identity and ownership

A **slot ID** names a permanent reusable directory, such as `wt-0001`. A **lease
ID** names one reservation of that slot. Reacquisition changes the lease ID, so
an old request cannot release the next task's workspace. CLI users normally refer
to a reservation by its **task label** (`--task`); lease IDs remain the
unambiguous identifier underneath, and slot IDs are for inspection.

An exclusive `flock` owns a slot while a core operation or an in-process `Lease`
holds it. Durable protection additionally prohibits reuse after that owner exits.
Neither metadata, UUIDs, PIDs nor timestamps prove kernel ownership. No
reservation expires, and no daemon or heartbeat is required.

Lock files are permanent: the manifest records their device/inode identities.
Never unlink or atomically replace them. Metadata is different: write a temporary
file, sync it, rename it, then sync the parent directory. Directory/path checks
assume cooperative same-user processes; worktrees are not security sandboxes.

## Components

| Package | Responsibility |
| --- | --- |
| `groove-core` | Discovery, allocation, locks, controlled Git, recovery, reservations and snapshots |
| `groove-cli` | Parsing, human and JSON rendering, exit codes, preferences and logging |
| `groove-release`, `groove-bench` | Unpublished release and benchmark tooling |

Core never prints or installs global logging. It uses blocking operations and
requires no async runtime.
The CLI routes diagnostics through a bounded stderr queue; stdout holds results.
Core supports Linux and macOS and fails with an explicit `compile_error!` elsewhere.
`platform` isolates Git child observation and filesystem qualification; `quiescence` has OS-specific process-inspection
backends. Descriptor ownership, durable protection and recovery policy are shared.

Browse [the core source](../crates/groove-core/src/) for module ownership;
public API behavior lives in rustdoc beside its implementation.

The CLI keeps one handler per command in `src/commands/`, rendering through
`src/output.rs`; exit codes come from typed results, never from rendered JSON.

## Storage

The common Git directory identifies the namespace catalog shared by ordinary,
linked and bare checkouts. `default` keeps its original directory; named pools
use `<common>/groove/pools/<name>/`. Per-pool checkout
roots are immutable until deletion/reinitialization. Capacity and cleanup can
change through an atomic settings update.

[State paths](../crates/groove-core/src/state.rs) and
[namespace administration](../crates/groove-core/src/namespace.rs) define the
storage layout. Treat it as coordination state, never as a manual cleanup API.
Permanent locks, durable reservations, saved-release receipts and request journals
survive task exit; checkout contents and legacy artifact caches are disposable.

Every manifest references the original permanent `<common>/groove/locks/admin.lock`;
named pools do not create independent Git-administration locks. Pool-local lifecycle
admission remains separate, so a sibling's reservation or healthy normalization
does not need another pool's lifecycle gate. Init holds repository admin exclusion
while validating and publishing checkout-root intent; equal, nested and existing
filesystem-alias roots cannot be claimed by another pool. Pending initialization
and retained deleted pools keep their current claims. A successful reinit with a
different root relinquishes the previous claim. Names use lowercase ASCII to avoid
case/normalization aliases, and never contain path separators or traversal.

Labels, request keys/tombstones, slot IDs, legacy execution history, receipts and artifacts
are pool-scoped. Reusing a key/label in another namespace is independent; stale
UUIDs never authorize operations there. Config/init/destroy results identify the
selected name. Catalog observations include ready, initializing, deleting and
retained deleted pools. Implicit destruction of default refuses when several
non-deleted pools exist; the check runs under repository admin exclusion, so a
concurrent init cannot slip between ambiguity checking and deletion.

Git's native worktree administration stays in `<GIT_COMMON_DIR>/worktrees/`.
New checkout roots live outside Git administration by default. Their project
component combines a sanitized repository name and a hash of the canonical common
Git directory: linked callers agree, independent clones do not share slots, and
no remote URL is required. Global preferences choose the parent; an explicit pool
root overrides it. Only the resolved absolute root is persisted, so later preference
or HOME changes do not relocate existing pools. Legacy roots remain accepted.
Snapshots use separate `groove-snapshots` administration and
`refs/groove/snapshots/`; see [snapshots](snapshots.md).

Durable records carry versioned formats and permanent lock identities; readers
reject incompatible formats instead of guessing or replacing locks.

`init` is create-only; interrupted setup needs an explicit resume with identical
settings. Growth/settings updates hold administrative exclusion, validate all
values and preserve existing slot identities. Brand-new/unpublished slots must
have empty diagnostic records. A slot retained after completed deletion may have
old diagnostics; growth validates its archived lock identities and original
checkout path, excludes slot/execution owners and durable protection, then resets
only that diagnostic record. Receipts, journals and history stay independent.
New-slot ownership checks are nonblocking while holding admin exclusion; probes
close per slot rather than consuming descriptors proportional to capacity. The
published manifest excludes new slots until publication, and admin exclusion
serializes growth, so no cooperating allocation can claim them in between.
Active old slots are never acquired or cleaned. The new capacity/policy
is published only after infrastructure sync. Interrupted unpublished slots are
validated and reused on retry. Locks are never unlinked. New checkouts materialize
on acquisition or background preparation. Handles refresh the manifest for acquisition scans, reservation lookup and
inspection; in-flight preparation may use its earlier observed policy.

## Allocation and cleanup

Pool mutations first take shared admission on `locks/lifecycle.lock` and check
their pool generation. This guard covers the entire operation, including request
infrastructure before slot acquisition and saved-release receipts after slot release.
Nested shared admission is allowed; exclusive transitions use nonblocking admission
so no queued exclusive waiter can deadlock a nested shared operation. The guard is
not retained by an idle `Pool` handle. Normal slot/execution/admin locking and
descriptor inheritance remain authoritative for surviving children.

`init` and `destroy` take exclusive admission before admin exclusion. Destroy
probes every existing slot/execution lock nonblockingly, including unpublished
growth and retired slots, and checks that every configured identity still exists.
It never waits for slot ownership while holding admin. Live/protected reservations
and orphan execution records always refuse deletion; no discard option bypasses them.
Exclusive admission prevents new cooperating owners, so successful probes can close
individually instead of consuming two descriptors per slot for the whole deletion.
Snapshot operations originating in a managed slot take shared admission too and
pass it to mutating Git children. Independent snapshots in the main checkout
remain available after deletion. New restore destinations must be outside pool roots.

Destroy preflights all configured paths, observed processes (including the caller
and ancestors), residual changes and artifact entries before persisting `deleting`.
It removes only configured slot paths/exact registrations and recognized disposable
cache directories, syncing each removal. A crash leaves allocation disabled;
retrying with the same discard policy revalidates ownership/paths/processes and
completes removal. Already-removed paths are accepted. It then archives the manifest
and publishes `deleted`. Roots, unknown files in checkout roots, permanent locks,
request journals, receipts and logs/history remain; snapshots and branches are independent.
Completed receipts whose snapshots no longer exist are pruned after durable
deletion, unless their pin-retirement journal is still pending. Permanent
request/label locks and never-expiring key tombstones retain their identities.
Once deletion is durable, the external `.groove-owner` symlink is removed when
no active/pending pool uses the root. A crash before marker cleanup is repaired by
a completed destroy retry; a later repository's replacement claim is preserved.

Reinitialization changes the UUID generation, preserves all recorded lock identities
(including retired manifests) and retains that pool's request tombstones.
Old `Pool` and managed-slot snapshot handles reject the replacement generation.
Reinit resets slot diagnostic records while preserving independent receipts/history.
Saved-release receipt retries also work after deletion. Interrupted init can be
cancelled under the same exclusion without materializing checkouts. Legacy ready
manifests without a lifecycle record remain readable; deletion publishes their
first generation. All newly initialized pools have lifecycle records. Use one
Groove version across the repository's pools; older binaries do not understand lifecycle admission.

Snapshot services opened inside managed workspaces resolve the actual namespace
from validated checkout roots, regardless of CLI pool selection. Main-checkout
snapshots stay repository-wide. Restore into a new checkout takes repository admin
exclusion, validates its destination against every published/pending root and passes
that descriptor to mutation children; it never waits for sibling lifecycle admission
while holding admin. Parent aliases are resolved before validation and the same
resolved path is passed to Git. This prevents reinit from changing root ownership
between destination validation and materialization.

1. Resolve the requested revision to a full commit OID.
2. Scan fixed slots using nonblocking ownership locks and a monotonic wait budget.
3. Skip live owners and durable reservations as retryable contention.
4. Validate the owned path and native worktree registration.
5. Normalize in place, or remove and re-register, under the relevant locks.
6. Verify detached HEAD, index, contents and absence of interrupted Git operations.
7. Persist a new generation before returning a handle or protected reservation.

Capacity bounds reservable slots, not process count, artifacts, CPU, memory or Git
object storage. Waiting has no FIFO guarantee; Git preparation has its own timeout.

Normalization removes the owned index, discarding its flags and stat cache. If
the previous HEAD is readable and differs from the destination, `read-tree HEAD`
reconstructs only its tracked-path inventory so checkout can remove paths the
destination deletes and ignores.
The index is not refreshed: forced detached checkout rewrites tracked files,
breaking shared hardlinks and recreating file metadata. If the previous HEAD is
unreadable or unborn, any partial index is discarded and checkout proceeds
without it, preserving in-place recovery and warm caches. Cleanup uses
`git clean -ffdx` (`pristine`) or `-ffd` (`warm`). Warm ignored files are unverified
residue and may be discarded during recovery. Branches created by tasks survive
cleanup; detached commits without a durable ref may become unreachable.

Verification asks Git whether HEAD is symbolic rather than reading the HEAD file,
so reftable repositories work. Registrations are always written with absolute
paths (`worktree.useRelativePaths=false` for Groove's own `worktree add`);
relative links written later by Git are resolved lexically and must reach the
same file through the kernel before they are trusted or removed.

New slots register with `--no-checkout` before normalization, avoiding duplicate
materialization. Administrative exclusion ends after registration; slot exclusion
continues through checkout and verification and is inherited by mutating Git children.

Recovery rebuilds only validated owned paths and exact native registrations,
under slot then administrative locks. Failed slots are quarantined and attempted
at most once per acquisition; later requests may retry. Never run shared worktree
prune or sweep unrelated Git lock files. Never hand out an unverified checkout.
Before checkout mutation or disposal, recognizable foreign occupants and duplicate
registration claims refuse preparation. Destruction preflights this identity check
before publishing `deleting` and rechecks at removal. Damaged owned registrations
remain repairable; a forged pointer never authorizes deletion of its target.

## Reservation and retry ordering

`Pool::acquire` returns a non-Clone descriptor-owning `Lease`. Dropping it closes
the descriptor; reuse becomes possible after the last copy closes, including any
inherited by a participating child. `Lease::release` retains the handle on
failure and invalidates it on success.

`Pool::reserve` acquires a slot (HEAD detached at the base; Groove never manages
branches), saves task metadata and durable protection, then records an optional
request key before closing the descriptor. Retries serialize
per key and inspect protection to recover a crash before journal publication. An
existing journal never authorizes allocating a replacement lease. Completed keys
remain tombstoned. The same key, base and label return the original active
reservation, even if the base branch moved.

Within one pool, at most one active reservation carries each task label. A permanent per-label
lock (taken after any request-key lock) serializes the duplicate check with
protection publication. Labels and keys reject control characters, and human
output escapes them in any stored text.

Release takes the slot lock, rechecks the generation and independent execution
state, detaches HEAD in a still-registered checkout (best effort, so a branch
created there is no longer held), records a completed key, writes release
metadata, removes protection and unlocks.
Same-generation retries complete interrupted release; a successor rejects stale
requests. A process holding a live `Lease` keeps its slot locked, so another
process cannot release that reservation.

## Saved release and legacy compatibility

`release --save` ensures durable reuse protection, saves and pins a snapshot,
persists a receipt, then releases. Retries of an active reservation capture its
current state again; older checkpoints remain pinned. Completed retries validate
and return their saved snapshot without touching a successor checkout, including
after pool deletion. Snapshot failure retains the reservation; capture requires
stopped writers. Release never merges, pushes or publishes task results.

Groove does not launch or supervise task commands. The runner owns their lifetime.
Permanent execution locks and records from previous versions remain authoritative
compatibility barriers: release, saved release and deletion refuse them. Use the
previous version to stop and recover outstanding executions before upgrading.
Existing logs, history and artifact data are not discarded on upgrade.

New checkout roots retain an immutable `.groove-owner` symlink naming the owning
repository's canonical namespace directory. Atomic symlink creation prevents independent
repositories from claiming the same empty root. The marker is read without
following it, survives destruction, and never confers lease ownership. Legacy
roots remain supported; do not remove owner markers to transfer storage.

## Git subprocesses


Git runs with argv arrays, a controlled repository-selection environment,
disabled hooks/prompts/fsmonitor/automatic maintenance, bounded time and captured
output. `git::Invocation` makes each policy explicit: `ConfigScope::Isolated`
(maintenance; system/global configuration disabled) or `ConfigScope::User`
(snapshots honor user ignore and attribute rules while preserving physical bytes,
executable modes and symlinks independently of checkout preferences), the stdout bound, inherited
descriptors, and whether `ls-tree` output is streamed for gitlink detection.
Ordinary output is bounded to 1 MiB per stream while both streams are drained.
Index audits stream `ls-files -v --stage -z` headers, retaining conflicts,
gitlinks, hidden-edit flags and potential intent-to-add entries without storing
the complete tracked-path list. Malformed or incomplete records fail closed.
Failure diagnostics retain at most 16 KiB per stream, including JSON escaping,
independently of the larger successful-plumbing output budget. Truncation is
reported explicitly; warnings can fail inventories even when Git exits zero.

Mutating children inherit relevant ownership/admin descriptors. Timeouts terminate
the mutation process group and reap the direct child before relinquishing protection.
`Lease::run_worker` waits only for its direct child; callers must stop detached descendants.

Platform/configuration exclusions and operator recovery are in
[operations](operations.md).

## Ready checkouts and idle eviction

The CLI copies global defaults into explicit pool policy at initialization and
starts a short-lived worker after successful pool mutations. Core library callers
choose a policy and their own execution of `request_ready`/`run_ready`. Existing
manifests without policy retain lazy allocation. Enabling policy adds fields that
older strict readers reject; mixed-version writers must not share enabled pools.

A permanent per-pool ready lock serializes reconciliation. A versioned request
nonce and the worker's final request check/descriptor closure are coordinated by
repository admin exclusion. Requests arriving during work cause another pass;
a request after closure starts a new owner. The CLI publishes requests in the
child, so foreground success does not wait behind background administration.
Workers use generation-checked lifecycle admission throughout their lifetime.
Slot and execution locks, durable protection and path validation remain authoritative.
No timestamp expires a reservation. Descriptor use is bounded independently of
capacity; candidate counts are observations, so the ready target is best effort.

Preparation prefers explicitly released or never-used slots. Dropped leases and
quarantined records are not background disposal candidates. Before changing an
existing checkout, the worker checks all observable external users and unsaved
files. Saved-release contents must match the pinned snapshot's HEAD, index tree,
working tree and untracked inventory. Checks remain best effort against external
writers, which must stop before release. Ignored caches are disposable.

Per-slot intent records bind preparation/eviction to the pool and lease generation.
They permit retry after interruption, including removal of native registration
before checkout contents. A subsequent acquisition changes the lease generation,
invalidating the previous worker intent. Exact owned paths/registrations are
revalidated; permanent locks, capacity, branches, snapshots and receipts survive.
Missing idle timestamps begin a fresh observation period. Future timestamps do not
age out; time is only an eligibility signal. Ready hints influence allocation order
but never bypass normal acquisition verification.

Workers exit after immediate work. Expiration is checked on later pool operations,
not by a timer or daemon. Failures are observable through pool show; repeated work
uses bounded retry backoff, while changed policy permits an immediate retry.
