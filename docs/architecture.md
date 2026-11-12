# Architecture

Groove is a repository-local pool of reusable Git worktrees. The development
protocol is v1; [protocol](protocol.md) defines its wire contract and
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
| `groove-core` | Discovery, allocation, locks, controlled Git, recovery, reservations, execution and snapshots |
| `groove-cli` | Parsing, human and JSON rendering, exit codes, preferences and logging |
| `groove-release`, `groove-bench` | Unpublished Rust development tooling with Bash entry points |

Core never prints or installs global logging. It uses blocking operations and
requires no async runtime.
Managed execution optionally delivers captured bytes to a nonblocking observer.
The CLI routes those bytes and filtered diagnostics through a bounded stderr
queue, keeping presentation backpressure out of worker supervision. File
diagnostic levels and terminal verbosity are independent; stdout holds results.
Core supports Linux and macOS and fails with an explicit `compile_error!` elsewhere.
`platform` isolates signal consumption, authenticated control endpoints, filesystem
qualification and reflink creation; `quiescence` has OS-specific process-inspection
backends. Descriptor ownership, durable protection and recovery policy are shared.

`groove-core` modules, by concern:

| Module | Owns |
| --- | --- |
| `pool` | `Pool::init/open/acquire`, durable release, protected-lease listing |
| `reservation` | `Pool::reserve`, request-key journal, lookup by ID or task label |
| `inspection` | Slot observations and optional Git status |
| `execution` | Managed workers, stop sockets, orphan recovery |
| `preparation` | Project setup runs and named commands |
| `finish` | Pin a snapshot and release |
| `settings` | Capacity growth and cleanup changes |
| `recovery` | Normalizing, rebuilding and verifying checkouts |
| `git` | Bounded, isolated Git subprocesses (`Invocation`) |
| `state`, `locking`, `repository` | Durable records, permanent locks, path validation |

The CLI keeps one handler per command in `src/commands/`, rendering through
`src/output.rs`; exit codes come from typed results, never from rendered JSON.

## Storage

The common Git directory identifies the pool shared by ordinary, linked and bare
repositories. An optional dedicated external checkout root changes only worktree
placement. The checkout root is immutable; capacity can grow and cleanup policy
can change through an atomic settings update.

```text
<GIT_COMMON_DIR>/groove/
  config.json                     # Ready manifest (schema 1)
  locks/admin.lock                # Administrative serialization
  locks/wt-0001.lock              # Permanent slot ownership
  locks/wt-0001.execution.lock    # Permanent execution ownership
  slots/wt-0001.json              # Current generation and phase
  slots/wt-0001.protected.json    # Durable prohibition on reuse
  slots/wt-0001.execution.json    # Active/orphan managed run
  slots/wt-0001.setup.json        # Setup readiness for the current generation
  slots/finish-<lease-id>.json    # Checkpoint/release receipt
  requests/<key-hash>.json        # Active/completed request-key journal
  requests/<key-hash>.lock        # Permanent per-key serialization
  requests/task-<label-hash>.lock # Permanent per-label serialization
  task-logs/<run-id>/             # Captured stdout/stderr and durable run.json
  worktrees/wt-0001/              # Default disposable checkout
  tmp/
```

Git's native worktree administration stays in `<GIT_COMMON_DIR>/worktrees/`.
Snapshots use separate `groove-snapshots` administration and
`refs/groove/snapshots/`; see [snapshots](snapshots.md).

Every record carries `schema_version: 1`, the only format this release reads or
writes. Manifests always contain execution-lock identities for every slot.

`init` is create-only; interrupted setup needs an explicit resume with identical
settings. Growth/settings updates hold administrative exclusion, validate all
values, preserve existing slot identities and create only empty new-slot
records/locks. New-slot ownership checks are nonblocking while holding admin
exclusion; active old slots are never acquired or cleaned. The new capacity/policy
is published only after infrastructure sync. Interrupted unpublished slots are
validated and reused on retry. Locks are never unlinked. New checkouts remain
lazy. Handles refresh the manifest for acquisition scans, reservation lookup and
inspection; in-flight preparation may use its earlier observed policy.

## Allocation and cleanup

1. Resolve the requested revision to a full commit OID.
2. Scan fixed slots using nonblocking ownership locks and a monotonic wait budget.
3. Skip live owners and durable reservations as retryable contention.
4. Validate the owned path and native worktree registration.
5. Normalize in place, or remove and re-register, under the relevant locks.
6. Verify detached HEAD, index, contents and absence of interrupted Git operations.
7. Persist a new generation before returning a handle or protected reservation.

Capacity bounds reservable slots, not process count, artifacts, CPU, memory or Git
object storage. Waiting has no FIFO guarantee; Git preparation has its own timeout.

Normalization removes the owned index and uses forced detached checkout plus
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

At most one active reservation carries each task label. A permanent per-label
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

## Managed execution and finish

Managed execution uses a **separate** per-slot flock: it can run while a library caller retains
the lease descriptor, and release must obtain both before succeeding. The durable
execution record is published before spawn. The worker inherits the execution
descriptor, so a killed supervisor cannot free the live execution lock. Even if
all descriptors close, the record continues blocking release until explicit
recovery. The reservation itself remains protected throughout.

The supervisor owns an unreaped process-group leader until signalling completes,
then reaps and verifies teardown. The CLI enables Linux subreaper
behavior; embeddings opt in explicitly. On macOS the system reaper adopts orphaned
descendants; teardown still waits for the group to disappear, retaining protection
on failure. Stop requests address the live supervisor over a same-user Unix socket
named by lease/run UUID (abstract on Linux; a short hashed pathname in a validated
private directory on macOS); orphan recovery never
signals a recorded PID. See [execution](execution.md) for the cooperative-process
boundary and failure cases.

`finish` obtains the same execution exclusion, ensures durable reuse protection,
saves and pins a snapshot, persists a receipt, then releases.
A saved receipt makes retries stable across crashes and slot reuse.
Snapshot failure retains the reservation; capture still requires stopped writers.
Neither execution nor finish merges, pushes or publishes task results.

`history` persists diagnostic run records beside captured streams. Execution writes
an unresolved record before spawning and a terminal outcome after teardown/log sync,
before clearing the slot execution record. Recovery publishes an unknown recovered
outcome only if the run is still unresolved. History never authorizes slot reuse.

`storage` reuses the permanent execution lock for log removal and history deletion,
then rereads both execution protection and run state. No additional lock order or
replaceable lock files are introduced. Cleanup validates owned directories and
regular singly linked stream files, records `removing`, unlinks only the two known
stream paths, syncs their directory and records `removed`. Retry completes a partial
removal. History deletion requires removed logs and refuses unexpected directory
contents. Neither operation modifies protections, receipts, workspaces or artifacts.

## Git subprocesses

Git runs with argv arrays, a controlled repository-selection environment,
disabled hooks/prompts/fsmonitor/automatic maintenance, bounded time and captured
output. `git::Invocation` makes each policy explicit: `ConfigScope::Isolated`
(maintenance; system/global configuration disabled) or `ConfigScope::User`
(snapshots honor user ignore and attribute rules while preserving physical bytes,
executable modes and symlinks independently of checkout preferences), the stdout bound, inherited
descriptors, and whether `ls-tree` output is streamed for gitlink detection.
Ordinary output is bounded to 1 MiB per stream while both streams are drained.

Mutating children inherit relevant ownership/admin descriptors. Timeouts terminate
the mutation process group and reap the direct child before relinquishing protection.
`Lease::run_worker` is the lower-level direct-child helper; it does not provide
all managed-execution guarantees for detached descendants.

Platform/configuration exclusions and operator recovery are in
[operations](operations.md).

## Project readiness and artifact ownership

`project` parses/validates the versioned checkout-local definition. `preparation`
coordinates explicit setup while retaining the existing execution descriptor for
cache operations, gaps between commands and publication. Managed workers retain
that descriptor; no extra PID/time ownership rule is introduced. A separate
same-user authenticated random local socket accepts setup cancellation.

`slots/<slot>.setup.json` is durable diagnostic state scoped by lease UUID. It is
never ownership authority. Preparing is published before effects; ready follows
successful steps and definition/input verification. Interrupted records require
explicit retry. `exec_named` checks readiness while holding execution exclusion.
Acquisition without an explicit `--setup` does not run project code or seed artifacts.

`cache` owns allowlist validation, copying, hashing and immutable publication.
Artifacts have a permanent per-key lock independent of replaceable cache entries.
Lock order is reservation execution exclusion, then artifact key, then short Git
operations; another setup never needs the first slot's execution lock. Waits are
bounded/cancellable. Files are independent copies or reflink clones, never shared
mutable hardlinks. Publication uses private temporary storage, verification, sync
and rename. Crashes may leave private staging directories; no background eviction runs.

Saved task results are recovered with snapshot restore into a new checkout.
See [project workspaces](project.md).
