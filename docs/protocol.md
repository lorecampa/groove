# Groove CLI protocol v1

Status: development interface. Freeze v1 only after release review.

The CLI is the default interface for people and agents. Acquisition persists a
durable reservation before exposing its checkout; the reservation survives
command exit until explicit release. No daemon or retained shell session is
required.

All durable records (pool manifest, slot records, protections, execution and
setup records, run history, receipts) use schema version 1. There is no migration code: the
format has not been released. A future change must bump the version and add an
explicit, tested migration.

## Output modes

Output is human-readable by default: results go to stdout, errors to stderr as
`error: MESSAGE (CODE)` plus any details. Help is plain text.

With `--json`, or `GROOVE_OUTPUT=json` in the environment, every invocation
writes exactly one newline-terminated JSON object to stdout, including help,
version and argument errors. Nothing else is mixed into stdout. Stderr carries
warnings, errors and live worker output by default, including in JSON mode.
`-v` adds info, `-vv` debug, and `-vvv` trace diagnostics. `-q`/`--quiet` hides
warnings, progress and live worker output; command errors remain visible.
Quiet and verbose conflict. File logging has an independent `logging.level`
(default `debug`; `error`, `warn`, `info`, `debug`, `trace` are accepted).
See [logging](logging.md) for capture and bounded terminal-delivery behavior.
No response can be
guaranteed after SIGKILL, OOM termination or failure of the output stream.

```json
{"api_version":1,"request_id":"req-1","ok":true,"result":{}}
```

Errors replace `result` with `error`:

```json
{"api_version":1,"request_id":null,"ok":false,"error":{"code":"POOL_EXHAUSTED","message":"No verified worktree slot is available","retryable":true,"details":{"capacity":{"capacity":2,"busy_slots":2,"failed_slots":{}}}}}
```

Clients branch on `code`, not on `message`. `retryable` says whether the same
request may succeed once another owner finishes. `details` is typed (see the
`error` definition in the [response schema](../schemas/response-v1.schema.json)):
`git` (exit code and bounded output of a failed Git call), `capacity`, `path`, and
partial-progress fields such as `lease_id`, `released`, `snapshot_id`,
`safety_snapshot_id` and `preparation`. Details describe what to
recover; they never establish ownership. Help/version return `result.text`.
Additive fields may appear; unknown major API versions must be rejected.

Common failures also include an optional `error.hint` string with suggested next
steps. Human errors render the same text as `Hint: ...` on stderr. Hints are
advisory prose, not stable commands or authorization to reclaim a workspace;
clients still branch on `code` and inspect typed `details`. Commands mentioned in
hints apply to the same repository (`--repo`) and reservation selector as the
failed invocation. Codes, retryability and exit categories are unchanged.

## Commands

`groove COMMAND --help` documents examples, defaults and recovery rules.

```text
groove [global options] init --capacity N [--cleanup pristine|warm] [--worktree-root PATH] [--resume]
groove [global options] pools
groove [global options] destroy [--dry-run] [--discard-changes] [--pool-id UUID]
groove [global options] acquire [--base REV] [--task LABEL] [--request-key KEY]
       [--wait-ms MS] [--setup PROFILE [--retry] [--cache-source PATH]]
groove [global options] exec SELECTOR [limits] -- PROGRAM [ARG...]
groove [global options] exec SELECTOR [limits] --setup PROFILE [--retry] [--cache-source PATH]
groove [global options] exec SELECTOR [limits] --recipe NAME|--agent NAME [-- ARG...]
groove [global options] stop|recover|release SELECTOR
groove [global options] finish SELECTOR [--description TEXT] [--max-file-bytes N]
groove [global options] list [--slots] [--details]
groove [global options] show [SELECTOR | --slot SLOT] [--details]
groove [global options] completions bash|zsh|fish|powershell|elvish
groove [global options] history [--task LABEL] [--lease-id UUID]
groove [global options] history show|delete RUN_ID
groove [global options] storage show
groove [global options] storage clean --logs [--older-than 30d] [--dry-run]
groove [global options] snapshot save|list|show|diff|restore|delete ...
groove [global options] config show | set [--capacity N] [--cleanup pristine|warm]
groove [global options] config project [--example]
groove [global options] config global show|path|edit | set KEY=VALUE... | unset KEY...

SELECTOR = --task LABEL | --lease-id UUID
```

Global options (`--repo`, `--pool`, `--json`, `--request-id`, `-v`, `--quiet`, `--git-timeout-ms`) may
appear before or after a subcommand. `--repo` defaults to `.` and discovery works
from subdirectories and linked worktrees. `--request-id` only correlates responses
and logs. `--git-timeout-ms` is positive, defaults to 120000, and bounds each Git
operation and administrative/snapshot-lock wait, including discovery and snapshots
taken by `finish`. Snapshot-lock timeout keeps the existing `INVALID_ARGUMENT`
code, exit 2 and `retryable:true`; it never releases a task reservation.

### Selecting a pool

`--pool NAME` selects one namespace. Omission always means `default`, including
from a named pool's checkout; cwd and environment never choose a different pool.
Names match `[a-z][a-z0-9_-]{0,63}`. Missing names return `NOT_INITIALIZED`; a
lease UUID or task label is never searched in other namespaces. Names and UUIDs
do not confer ownership. All linked/bare checkouts share the same catalog.

`pools` returns all recorded namespaces sorted by name, including initializing,
deleting and retained deleted pools. Each entry has `name`, nullable `pool_id`,
`phase`, `state_dir`, nullable `capacity`, `cleanup_policy` and `worktree_root`.
It is a nontransactional, read-only observation and creates no pool storage.

Capacity/settings, slots, labels, request keys/tombstones, setup/cache data,
history/logs and finish receipts are pool-scoped. The same label or key can be
used independently in different pools. The original default layout and its keys
are unchanged; named state lives in `<common>/groove/pools/<name>/`. Checkout-root
claims cannot overlap other pools, including pending and retained deleted pools.

Snapshots, Git refs/objects/configuration and global CLI preferences remain shared.
`--pool` does not filter snapshot commands or global configuration. Snapshot commands
use the checkout given by `--repo`; managed-checkout services acquire that checkout's
actual pool admission regardless of CLI selection. Recovery destinations must stay
outside every pool root. Worker environment includes verified `GROOVE_POOL`; slot
numbers and project-derived ports remain pool-local, without automatic port isolation.

### Selecting a reservation

`--task LABEL` selects the single active reservation whose task label equals
`LABEL`; zero matches fail with `LEASE_MISMATCH`, several with
`INVALID_ARGUMENT` (use `--lease-id`). `--lease-id` is passed to core unresolved,
so retries of `release` and `finish` keep working for a generation that a
successor has since superseded. Slot IDs name reusable directories; lease IDs
name one reservation of a slot and never release a later one.

Selected `show` observes only the resolved slot; `--details` runs bounded local
Git status only in that workspace. Resolving a task or lease may still scan
reservation metadata. Unselected `show` and `list --details` inspect the whole
pool. Inspection remains nontransactional and never establishes ownership.

### Shell completions

`completions SHELL` prints a generated script for commands, options and fixed
values. It does not query live task names or lease IDs, require a repository or
valid user preferences, install files, or execute the script. Human mode writes
the script directly; JSON mode returns it as `result.text` in exactly one response,
including when selected by `GROOVE_OUTPUT=json`. Regenerate scripts after upgrades.
See `groove completions --help` for installation examples.

### acquire

`--base` (default `HEAD`) is any commit, branch or tag. The checkout is clean and
its HEAD is detached there; Groove never creates, checks out or moves branches.
On any failure before the reservation is published, no slot remains reserved.

With `--request-key`, repeating the same key, base and task returns the
same reservation, even after a restart or a crash between protection and
journaling. Keys are scoped to the selected pool, 1..128 bytes and never expire; changed
inputs and completed keys are rejected. Without a key, inspect `list` before
retrying. Task labels are 1..256 bytes and never prove ownership.

`--wait-ms` defaults to 0 (one capacity scan); positive values bound waiting for
capacity, not preparation. Each failing slot is attempted at most once per
acquisition. There is no FIFO fairness.

### release and finish

`release` returns the slot to the pool. Save results and stop all writers first.
Unless `--force` is given, release first refuses with `UNSAVED_CHANGES` (staged,
modified or untracked files, found by one read-only `git status` without rename
detection, ignored-file scan or ahead/behind work) and with `WORKSPACE_IN_USE`
(other processes whose working directory, executable or open files are inside the
workspace, found through `/proc` on Linux or libproc on macOS; the invoking process and its ancestors are
excluded). `finish` applies only the process check. Both checks are best effort
and skipped for an inactive reservation, so retries are never blocked; `--force`
never bypasses a running managed execution. Error details carry `changes` or
`processes`. Release takes the slot and
execution locks, rechecks the generation, records the release, removes durable
protection. Cleanup is deferred to the next acquisition and may
discard workspace contents. Same-generation retries succeed until reassignment.
A reservation whose slot is held open by a live library `Lease` must be released
through that handle.

Failure to enumerate the process table returns an I/O error instead of reporting
an empty process list. Individual inaccessible/exited processes remain best-effort
observations. `control_name` in execution/setup records is an opaque endpoint name;
its transport is platform-specific, and clients request cancellation through `stop`.

`finish` saves and pins a snapshot, writes a receipt, then releases. A snapshot
failure keeps the reservation. Retrying finish by lease ID reuses the receipt
even after the slot was reused. Recover saved results with
`snapshot restore ID --to DIRECTORY`; finish and restore do not export patches.

### Pool settings

`destroy` removes an idle pool's configured checkouts, their exact native Git
registrations and disposable artifact/cache staging directories. Finish/release all
reservations and stop every writer first. Live slot/execution descriptors,
protection/execution records (including malformed ones) and observed processes
always refuse deletion. Unlike release's convenience scan, destroy also checks
inactive slots and includes the caller/ancestors. Run it from outside the pool.
Residual changes, untracked/ignored files and broken checkouts require explicit
`--discard-changes`; warm policy does not preserve ignored files during deletion.
Path, ownership and process checks cannot be overridden.
Omission of `--pool` targets default and refuses with `INVALID_ARGUMENT` when
several non-deleted pools exist; use an explicit name (including `--pool default`).
The ambiguity check is serialized with pool creation. `--pool-id` checks only the
generation within that selected namespace and never selects another pool.
Without discard, HEAD must be reachable from a repository ref (a branch, tag or
snapshot suffices); hidden index flags and interrupted Git operations also refuse
removal. Inspection is bounded and fails closed, including for oversized Git output.

`--dry-run` validates and returns the plan without removing data or changing the
lifecycle record; it can briefly exclude concurrent operations. Both preview and
execution fail when preconditions are unmet. Permanent locks, request journals and
completed-key tombstones, finish receipts, logs/history, snapshots, branches and
checkout-root directories remain. Unknown root contents are retained; unknown
artifact entries refuse removal. History/storage and snapshot commands remain
usable after deletion; explicit finish retries by lease ID can return retained
receipts. New work requires a fresh init, which may choose different capacity/root.

Deletion durably transitions `ready` to `deleting` to `deleted`. Normal pool use
and init refuse incomplete deletion; repeat destroy with the original discard
policy to resume. A completed retry returns `already_deleted: true`.
Do not edit or repurpose removal paths during incomplete deletion: the original
preflight authorized their removal, and retries may encounter partially removed Git
registrations. Ownership, paths and processes are revalidated on each retry.
Supply `--pool-id` from the preview/result to prevent a lost-response retry from deleting
a subsequently initialized pool. Omission targets the current pool at invocation.
A preview before a generation has been durably assigned (legacy pools or very
early interrupted initialization) has `pool_id: null`.
Destroy also cancels interrupted init, retaining permanent locks. Damaged/moved
paths or malformed metadata are never adopted as permission to delete foreign data.

`lifecycle.json` uses [lifecycle schema 1](../schemas/lifecycle-v1.schema.json),
including UUID v4 `pool_id`, `phase`, permanent `lock_identity` and the persisted
`discard_changes` policy. Retired manifests retain the existing config schema.
Lifecycle additions do not change existing manifest/slot formats or exit categories.

`init` creates capacity 1..4096 and rejects a ready pool with `CONFIG_CONFLICT`,
even for identical settings. Interrupted initialization must be resumed with
`--resume` and identical settings. A custom checkout root must be empty, owned by
the user and outside Git administration. The root stays fixed until that pool is
deleted and reinitialized.

`config set` grows capacity and/or changes cleanup for future acquisitions, in
one atomic, validated update. Shrinking is unsupported. Growth preserves existing
reservations, contents and lock identities and is retryable after a crash.
Unchanged values report `updated: false`.

`pristine` (default) removes all untracked and ignored files on reuse. `warm`
keeps ignored files when a healthy worktree is normalized in place; they are not
part of the verified baseline and a rebuild can discard them.

## Success results

| Command | `result` |
| --- | --- |
| init | `pool_name`, `created`, `common_git_dir`, `state_dir`, `capacity`, `slots`, `cleanup_policy`, `worktree_root` |
| pools | `pools` (catalog entries described above) |
| destroy | `destruction` (`pool_name`, `pool_id`, `dry_run`, `deleted`, `already_deleted`, `resumed`, `state_dir`, `worktree_root`, `slots`, `artifacts`) |
| acquire | lease fields (below); plus `preparation` with `--setup` |
| exec | `execution` (below), or `preparation` with `--setup` |
| stop | `stop_requested` (execution record) or `preparation_stop_requested` |
| recover | `execution_recovered` |
| finish | `release`, `snapshot` (pinned) |
| release | `slot_id`, `lease_id`, `released: true`, `cleanup: "deferred"` |
| list | `leases`; with `--slots`/`--details`, `inspection` |
| show | `pool` and `inspection`; with a task/lease selector, `lease` and `slot`; with `--slot`, `slot` |
| completions | `text` (shell script) |
| history | `runs` (newest first, historical task/lease filters combined with AND) |
| history show / delete | `run` / `deleted_run_id` |
| storage show | `storage` (logical byte counts, run counts) |
| storage clean | `cleanup` (`dry_run`, per-run `entries`) |
| snapshot save/show | `snapshot` |
| snapshot list / diff | `snapshots` / `patch` |
| snapshot restore | `path`, `snapshot_id`, `safety_snapshot_id` |
| snapshot delete | `deleted` |
| config show / set | `pool` (including `pool_name`); `set` adds `updated` |
| config project | `project_path`, `project`, `fingerprint`; `--example` returns `text` |
| config global | `config_path`; `show` adds `config`, `environment_overrides`; writes add `updated` |

Lease fields: `slot_id`, `lease_id`, `worktree_path`, `base_oid`, `head_mode`
(always `detached`), `recovery` (`created`, `sanitized`,
`rebuilt`), `cleanup_policy`, `ownership` (`durable` for CLI reservations,
`in_process` for library leases), and optional `task`, `request_key`,
`requested_base` and diagnostic `acquired_at_unix_ms`.

Execution fields: `lease_id`, `run` (program, run ID, log paths, diagnostic leader
PID), `termination` (`exited`, `timed_out`, `cancelled`, `output_limited`),
`exit_code`, `signal` and `cancel_signal`. `ok: true` means the managed run
completed, even if the worker failed.

Managed paths are absolute strings. Slot IDs are `wt-0001` through `wt-4096`.
Lease IDs are UUID v4. OIDs are full SHA-1 or SHA-256 hex. Inspection statuses are
`available`, `reserved`, `locked`, `quarantined` or `unhealthy`; observations never
authorize reclamation. Setup readiness is described in [project](project.md).

History records contain `schema_version`, `run_id` (UUID v4), `slot_id`, `lease_id`,
nullable `task`, `program`, `origin`, start/end Unix milliseconds, nullable
`duration_ms`, `outcome`, nullable `termination`, `exit_code`, `signal`,
`cancel_signal`, `failure`, `logs`, `stdout_path` and `stderr_path`.
Origin is tagged by `kind`: `command`, `setup` (profile/step), `recipe` or `agent`
(name), or `unknown` when recovery reconstructs missing history. Outcome is
`unresolved`, `completed`, `spawn_failed` or `recovered`.
Only `completed` has a known termination; recovery never invents an exit code.
Log state is `present`, `removing` (interrupted cleanup) or `removed`.
Paths remain in history after removal. Arguments and environment are not recorded.
The `run_history` definition in the response schema also defines persisted `run.json`.

Storage reports `log_bytes`, `history_bytes`, `untracked_log_bytes`,
`untracked_runs`, `artifact_bytes`, `workspace_bytes`, and `runs`. Counts are
nontransactional logical file sizes, not allocated blocks or estimated reclaimable
space; symlinks are not followed and shared Git/snapshot objects are excluded.
Cleanup entries contain `run_id`, `log_bytes`, `status` (`removed`, `would_remove`,
`skipped`, `failed`), nullable `reason` and nullable typed `error_code`.
`log_bytes` is the observed size before removal, not guaranteed reclaimed bytes;
on failure the operation may have removed one stream already.
Cleanup returns `ok:true` with exit **7** if any entry failed, preserving the
complete partial-success report. Initial scan failures use the normal error
envelope and occur before deletion. Skips alone are successful (exit 0).
`--older-than` accepts a nonnegative integer and `s`, `m`, `h` or `d`, default
`30d`; `0s` includes all terminal runs. Thresholds use completion/recovery time,
never ownership inference. History deletion requires already-cleaned logs.

## Exit status

| Exit | Meaning |
| --- | --- |
| 0 | Success |
| 2 | `INVALID_ARGUMENT`, `UNSUPPORTED_CONFIGURATION` |
| 3 | `POOL_EXHAUSTED`, `ACQUIRE_TIMEOUT` |
| 4 | `NOT_INITIALIZED`, `CONFIG_CONFLICT`, `INVALID_REVISION` |
| 5 | `INVALID_LEASE`, `LEASE_MISMATCH`, `UNSAVED_CHANGES`, `WORKSPACE_IN_USE` |
| 6 | `RECOVERY_FAILED`, `POOL_UNHEALTHY` |
| 7 | `IO_ERROR`, `GIT_ERROR`, `PROTOCOL_ERROR`, or stdout could not be written |

`exec` and setup exit with the worker's status: its exit code, 124 on timeout,
`128 + signal` when cancelled or killed, and 1 when output exceeded its limit.
The CLI computes these from the typed result (`ExecResult::shell_status`). Read
`result.execution` to tell worker failures from Groove failures. Worker output is
captured under the selected pool's `task-logs/` directory. See [execution](execution.md).

## Rust API

`ErrorCode` is a non-exhaustive enum serialized as the uppercase names above;
keep a fallback match arm. Underlying causes are available through
`std::error::Error::source` but are not serialized. `Error::details` is a typed
`ErrorDetails`; `Error::git_exit_code` reads a failed Git call's status. Exit
categories are a CLI concern and live in `groove-cli`.

`SlotId`, `LeaseId` and `CommitOid` are validated types. Parsing proves format, not
pool membership or ownership. `HeadMode`, `Ownership`, `Termination` and
`CleanupTiming` are enums with the string forms used above.
