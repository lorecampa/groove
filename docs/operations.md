# Operating Groove

## Requirements

Linux or macOS 13+ (Apple Silicon/Intel), Git 2.40+, local storage, and cooperative processes running as the
repository owner. Building needs Rust 1.89+. The base Linux target is kernel 5.4;
minimum-kernel VM coverage is not established. Native macOS CI targets macOS 15;
minimum-version macOS VM coverage is not established. Other platforms fail to compile
with an explicit error.

Both platforms use flock and close-on-exec descriptors. Linux uses signalfd and
abstract Unix sockets; macOS consumes blocked signals with sigwait and uses
filesystem Unix sockets authenticated with getpeereid.
On Linux, NFS, CIFS/SMB, FUSE, SMB2, Coda and AFS are rejected by filesystem type.
macOS requires local APFS, including case-sensitive APFS; HFS+, network and FUSE
filesystems are unsupported. Passing
that check does not certify flock/fsync or power-loss durability. Managed state
must be owned by you and not writable by group/others.
APFS filenames must be valid UTF-8. Names that differ only by case or Unicode
normalization may collide on the destination filesystem; repositories must use
names that the selected volume can represent distinctly.

On macOS, control sockets live in the private mode-0700 directory
`/private/tmp/groove-UID/`, with short names derived from each run's random endpoint.
This avoids Unix socket path limits for deeply nested repositories and long TMPDIRs.
Normal exit removes the owned endpoint. Explicit execution recovery or setup retry
cleans the corresponding stale socket under execution exclusion; an unexpected
file is preserved and causes an error. Socket files never establish ownership.

SHA-1/SHA-256 and ordinary, linked and bare repositories are supported. Pool
checkouts reject submodules, sparse checkout, worktree-specific configuration and
configured content filters such as LFS. Maintenance clears repository-selection
overrides and disables hooks, fsmonitor, automatic maintenance, prompts and
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
A ready pool rejects every repeated init. The checkout root cannot change.
Use `groove config set --capacity 8 --cleanup warm` to grow/change future cleanup.
Updates validate all values before publication; shrinking is unsupported. Active
reservations and permanent locks survive. Retry matching interrupted init with
`--resume`; use `config show` after a lost response if initialization already completed. A custom root must be dedicated, initially empty, owned by you
and outside Git administration. Relative paths resolve from the caller's directory.


Default state/worktrees live in `<GIT_COMMON_DIR>/groove/`. This enlarges `.git`
backups; editors may exclude it. Choose an external root when needed and do not
repurpose it while the pool exists. Stop all owners before moving/restoring a pool.
Restoring permanent locks onto new inodes invalidates manifest identities;
Groove refuses automatic replacement.

Use one Groove version per live pool. Every record uses schema 1; there is no
migration between development versions, so recreate pools after upgrading a
pre-release build. Current handles and acquisition waiters refresh settings, while
already-started preparation may use its observed policy.

## Task lifecycle

[Agent workflow](agents.md) covers acquire, retry, work and finish. Normal commands
select a reservation with `--task` (its label) or `--lease-id`; slot IDs
identify reusable directories for diagnostics.
Reservations survive process exit and restart, with no TTL or automatic reclamation.

```sh
groove list
groove list --slots
groove show --slot wt-0001
```

`list` lists durable reservations. `list --slots` includes available, reserved,
locked, quarantined and unhealthy slots, plus task labels, acquisition times and
execution records. These are nontransactional observations; PIDs/age never prove
ownership or stopped writers. A live in-process library lease need not appear
in `list`.

Use [managed execution](execution.md) for deadlines, cancellation, stop and captured
logs. External editors, watchers and commands remain your responsibility.
`Lease::run_worker` waits only for its direct child; retained descriptors can keep
a slot busy indefinitely. Closing a descriptor while continuing to write creates
the opposite risk. Never explicitly unlock until all writers stop.

`finish` saves a pinned local checkout snapshot before releasing. Otherwise save
results outside all slots (for example on a branch you create), then `release`.
Neither publishes results. The next acquisition may discard all files in the slot.

## Cleanup and shared state

`pristine` removes tracked edits, untracked files and ignored files on acquisition.
`warm` retains ignored files only in healthy reused checkouts; they are unverified
residue and may be lost during rebuild. External caches can be used with pristine
slots. Capacity bounds reservations, not process count, artifacts or disk usage.

Worktrees share Git objects, refs and configuration. Branches created by agents
remain in the repository; Groove does not manage/delete them. Coordinate shared
state changes and avoid branch-name collisions. Worktrees are not security sandboxes.
Snapshots survive slot reuse but remain local; ordinary branch pushes do not back
them up. Task logs, request journals and finish receipts have no automatic expiry.
Use `groove storage show` for logical usage and `groove storage clean --logs
--older-than 30d --dry-run` to preview explicit captured-log removal. Cleanup
retains history; `history delete RUN_ID` separately forgets a terminal run after
its logs are cleaned. Neither operation removes workspaces, caches, snapshots,
request journals or finish receipts. See [execution](execution.md#history-and-explicit-log-cleanup).

## Recovery

| Situation | Action |
| --- | --- |
| `POOL_EXHAUSTED` / `ACQUIRE_TIMEOUT` | Wait or inspect tasks. Protected reservations count as busy and are retryable. |
| `POOL_UNHEALTHY` | Inspect per-slot failures; malformed state or failed preparation prevents safe reuse. |
| Lost acquire response | Retry the same request key/base/label; without a key, identify ownership in `list`. |
| Active managed run | Request `stop`; wait for execution to clear. |
| Orphan execution record | Follow [execution recovery](execution.md#crashes-and-recovery); stop all writers before `recover`. |
| Live library `Lease` owner | Release through that handle after stopping writers; another process cannot unlock it. |
| Failed preparation | Groove quarantines the slot; a later acquisition retries bounded repair. |

Recovery normalizes or rebuilds under slot/admin locks, then verifies the baseline.
Native registrations use `git worktree add --lock` against external pruning,
independently of flock. Only registrations with exact owned backlinks are removed;
Groove never sweeps shared Git locks or unrelated metadata.

Never delete/replace lock or protection records to force capacity. Read-only
`/proc/*/fd` investigation on Linux, or process inspection on macOS, may help
locate owners but is incomplete because of
permissions and races. Missing PIDs/descriptors do not authorize cleanup. If
ownership is uncertain, leave protection intact.

Managed-path symlink and registration checks protect cooperating workflows, not
malicious same-user directory races. SIGKILL tests establish process-crash
recovery, not storage/power-loss guarantees. See [testing](testing.md).

## Prepared environments

Use [project configuration](project.md) for explicit setup, compatible independent
artifact copies, stable environment/ports, named recipes and agent argv adapters.
`config project --example` prints a starter without writing it. `show --details`
and `list --details` opt into local Git facts; ordinary inspection shows recorded
setup state, reasons and explicit next actions. `show --task LABEL` requires one
active match and shows the workspace path; inspecting never reserves it.

Sparse/LFS/submodule support remains
staged as documented in the project guide; existing unsupported checks remain.
