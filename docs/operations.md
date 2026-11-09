# Operating Groove

## Requirements and scope

Groove supports Linux, Rust 1.89 or newer, installed Git 2.40 or newer, local
filesystems, and cooperative processes running as the repository owner. The
Linux kernel baseline is 5.4; kernel compatibility is not separately exercised
on that minimum in CI. The implementation uses flock, close-on-exec descriptors,
Unix seqpacket sockets and descriptor passing, without pidfd or openat2.

NFS, CIFS/SMB, FUSE, SMB2, Coda and AFS mounts are rejected by filesystem type.
Passing that check is not certification of filesystem locking or power-loss
durability. Managed directories must be owned by the current user and not be
writable by group or others. Ordinary, linked and bare Git repositories share
the pool resolved through the common Git directory.

Git object formats SHA-1 and SHA-256 are supported. Submodules, sparse checkout,
worktree-specific configuration and repository-configured checkout filters
(including configured LFS filters) are unsupported. System/global Git config is
not used for maintenance. Repository-selection environment overrides are cleared;
Git hooks, fsmonitor, automatic maintenance and prompts are disabled. A baseline
means a clean Git checkout under that controlled configuration, including normal
Git attributes and line-ending conversions; it does not promise raw blob bytes.

## Installation and first use

```sh
cargo build --release --locked
cargo install --path crates/groove-cli --locked
cargo install --path crates/groove-mcp --locked  # Optional MCP adapter

groove --repo /path/to/repo init --capacity 4
# Opt-in alternatives on a new pool:
groove --repo /other/repo init --capacity 4 --cleanup warm \
  --worktree-root /absolute/dedicated/groove-worktrees
```

The CLI defaults to durable reservations: `groove acquire` persists protection
before returning a checkout, `groove leases` lists reservations, and
`groove release --slot SLOT --lease-id UUID --writers-stopped` explicitly releases
after results are saved and all writers stop. Use the [agent guide](agents.md).
MCP is optional. Descriptor-aware supervisors may opt into descriptor transport.
See [protocol](protocol.md) and its Rust
reference client. Rust programs can use `Pool` and `Lease` directly; the
[agent example](../crates/groove-core/examples/agent.rs) supervises one worker.

All state except an explicitly selected external worktree root lives in
`<GIT_COMMON_DIR>/groove/`. Default worktrees inside that directory are convenient
but enlarge `.git` backups, and some editor/watch configurations may exclude them.
Cargo and npm smoke tests cover that default. Choose an external root if your
tools require it. The dedicated root must not be repurposed while the pool exists.

## Writer lifetime and background processes

Keep the lease until every process that can write the slot has stopped. Direct
workers launched with `Lease::run_worker` deliberately inherit the descriptor;
unrelated execs do not. The helper waits for its direct worker, including failure
handling. It cannot prove that detached grandchildren stopped.

A compiler cache daemon, language server, watcher or dev server that inherits a
descriptor can keep a slot busy indefinitely. A descendant that closes it but
continues writing creates the opposite risk: premature reuse after the other
owners exit. Start participating tasks in a dedicated process group and stop
known writers before explicit release. Process groups help supervision but are
not containment: descendants can create new sessions or close inherited FDs.
Use an external supervisor with stronger lifecycle control when required.

Do not explicitly unlock while any writer remains. For unprotected leases, normal descriptor closure
preserves locks retained by participating children. A daemon retaining the last
copy produces genuine occupancy, not a lease that can safely be expired.

## Results, cleanup and shared Git state

Groove does not execute tasks, commit changes or publish results for you. Preserve
work before release, for example by committing to a task-owned branch or exporting
a patch elsewhere. Detached commits without a durable ref are not durable task
results and can become unreachable when a slot is reset or rebuilt.

Branches and ordinary shared refs created by agents remain in the repository;
Groove neither deletes nor manages them. Worktrees also share objects and Git
configuration. They are not security sandboxes. Use unique branch names and
coordinate changes to shared repository state outside Groove.

`pristine` removes ignored and untracked files on every acquisition. `warm`
retains ignored files in healthy slots; ignored files are arbitrary residual
state, not a validated cache. Recovery may rebuild and discard them. Capacity
bounds slots and checkout directories, not build artifacts or object-database
size. External caches can be used with pristine slots.

## Recovery and diagnostics

Acquisition obtains the slot lock, validates paths/registration, normalizes and
verifies the baseline. Interrupted operations or damaged registrations trigger a
rebuild under both slot and administrative locks. Each slot is repaired at most
once per acquisition. Failed slots get a quarantine record; another slot may
still succeed. A later request can retry recovery.

`POOL_EXHAUSTED`/`ACQUIRE_TIMEOUT` details include capacity, busy count and per-slot
failure messages. `POOL_UNHEALTHY` means no usable/busy candidate remains after the
bounded attempts. `slots/wt-NNNN.json` provides advisory diagnostic metadata only:
PIDs, phase and UUID never prove current ownership. Metadata can be stale after a
crash. Never delete/replace lock files or sweep shared Git lock files to free slots.

A read-only `/proc/*/fd` ownership investigation can help an operator find leaked
descriptors, but permissions, races and independently opened FDs make it incomplete.
Automatic process enumeration and `status` are deferred. Diagnose surviving owners
with operating-system tools; never infer availability from the absence of a PID.

Native worktree registrations are created with `git worktree add --lock` to
prevent external pruning. This Git administrative lock is distinct from the
flock lease. Recovery removes only registrations whose exact backlink identifies
the owned slot; it never runs repository-wide prune or sweeps unrelated metadata.

Managed-path symlinks and unsafe registration pointers are rejected. These checks
assume cooperating writers respect the lease; they do not defend against a
malicious same-user process racing directory replacement or mutating shared Git
administration. Stop all owners before moving/restoring a pool. Restoring lock
files onto new inodes invalidates the manifest identities and requires deliberate
operator intervention rather than automatic lock replacement.

## Validation limits

Process-kill tests establish recovery after SIGKILL, not behavior under actual
power loss, broken storage, hostile same-user writers or every editor/build tool.
See [testing](testing.md) for coverage, commands and remaining validation work.

## Durable CLI and MCP workspaces

Default CLI reservations and MCP leases remain protected after their owner exits.
CLI callers inspect `groove leases` and release with the exact IDs and
`--writers-stopped` after saving results and stopping every writer. Use
`groove_leases` after restart and `groove_release` only after stopping all writers
and saving results. A pool of abandoned protected slots reports `POOL_UNHEALTHY`;
it does not automatically expire or clean them. Never remove protection files to
free capacity. The first protection upgrades the manifest to schema 2, readable
by this version; older binaries must be stopped first. See [MCP recovery](mcp.md).
