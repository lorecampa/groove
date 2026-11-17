---
name: groove
description: Use Groove's reusable Git worktrees when the user requests Groove or repository instructions require pooled task workspaces. Acquire or reuse the assigned reservation, preserve results, and hand off or release safely. Also use for Groove reservation retries and recovery; ordinary Git work does not require Groove.
license: MIT OR Apache-2.0
---

# Groove task workspaces

Requires the installed CLI, Git 2.40+ and Linux or macOS with supported local
storage. Use the terminal; this skill does not provide an MCP server.

Read `groove --json --help` for global output rules and `groove help exit-codes` for exit categories. Use
`groove --json` for machine-readable responses and keep stderr separate from
stdout. Before first using a command, read
`groove --json COMMAND --help`, including any subcommand (for example
`groove --json snapshot restore --help`). The JSON response stores the help
screen in `result.text`. Command help defines syntax, result fields/types and
errors. Use the installed contract to parse JSON responses.
Check both the exit status and `ok` before reading `result`.

Installing this skill does not require Groove for every repository or authorize
pushing, merging, discarding work or changing repository policy.

Use your runner's existing delegation and messaging tools when available. Sharing
a pool does not connect separately launched agents' conversations. The runner or
coordinator supplies task scheduling, communication, command execution and result integration.

## Establish your assignment first

- **Coordinator:** you are responsible for acquiring and eventually finishing
  the task's reservation, including stopping delegated workers.
- **Assigned worker:** a coordinator already supplied a checkout, reservation ID and
  pool. Work in that assignment; do not acquire another slot or release
  it. Report results and stop your writers before returning to the coordinator.
  Separately assigned tasks can have their own reservations.

Assigned workers use the checkout, reservation ID and pool supplied by their coordinator.
Match that assignment with `groove show ID` before using it. In JSON
output, require `result.active` to be true; a successful show can otherwise describe
a released reservation. Inspection, directory location, PIDs and metadata do not
grant ownership. If the assignment is
ambiguous, inspect and report it rather than guessing.

Choose the pool at acquisition with `-p NAME`, retaining that choice for acquisition
retries. Release, show and path locate the reservation across pools. Prefer the
retained reservation UUID for retries: labels may already belong to a successor.
Labels are repository-wide; idempotency keys remain pool-local. Without a name,
acquisition and pool show/get/set select the owning workspace's pool, then the sole
pool, otherwise report ambiguity. There is no hidden default or `GROOVE_POOL`
selection. Do not switch pools to bypass contention.

## Acquire and work (coordinator)

1. Inspect the selected pool with `groove --json pool show NAME`.
   Initialize it with `groove pool create NAME --capacity N` when repository
   setup is part of the assignment; otherwise report an uninitialized pool as
   a prerequisite. Do not silently change capacity or existing configuration.
2. Choose a task label and a new idempotency key for this logical acquisition:

   ```sh
   groove --json acquire fix-parser -p default --key fix-parser-attempt-1
   ```

   Retain the pool, key, original base argument, `result.reservation_id` and
   `result.workspace_path` outside the checkout. After a lost response, repeat
   the same acquisition inputs and key. Do not generate a second key to retry.
3. Direct **all** edits, Git commands and terminal work to the returned path.
   Set tool working directories explicitly. If the harness cannot move its
   editing context safely, hand off/relaunch in that checkout before editing.
   Read the checkout's project instructions. HEAD starts detached; Groove does
   not create branches. Git refs/configuration are shared across worktrees.
4. Work and run commands directly in the returned checkout using your runner or
   terminal. Keep the reservation until every writer has stopped and results are preserved.

On `POOL_EXHAUSTED` or `ACQUIRE_TIMEOUT`, wait/retry with the same acquisition
identity (for example `--wait 5s`). Never create overflow worktrees, enlarge
capacity or remove protection to make progress. If a bounded wait still cannot
make progress, report contention with the retained retry inputs instead of
spinning indefinitely.

## Preserve and hand off

An assigned worker reports its changes, checks and remaining processes, then
returns without releasing the reservation. The coordinator waits for all assigned work to stop before release.

A coordinator finishing the task must:

1. Stop and wait for every worker, test, watcher and dev server, including
   external writers. Move coordinator terminals outside the checkout before
   finishing. Use your runner or terminal to stop commands; Groove does not supervise them.
2. Export any required ignored artifacts; snapshots exclude ignored files.
3. Preserve the result and release:

   ```sh
   groove --json release RESERVATION_ID --save -m 'parser fix'
   ```

   Retain the pinned snapshot ID from `result.snapshot.id`. Recover files and staging state with
   `groove snapshot restore SNAPSHOT_ID --to /new/recovery-directory`.

If review requires a live checkout, keep the reservation and report it instead
of finishing. Once released, never use the old path: the next task can erase it.
Without `--save`, `release` does not save a snapshot. Nothing is merged or pushed automatically.

`WORKSPACE_IN_USE` means inspect/stop writers, not use `--force`. Failed saving
or writer shutdown means **leave protection intact** and report the recovery needed.
Never delete locks, protection or execution records. Reservations never expire.

## Retry, recover and report

For a lost response, conflicting label or blocked `release --save`,
read [references/recovery.md](references/recovery.md). Retry finalization with
its original **reservation ID**, never a reusable task label. Stale IDs cannot
release a successor; saved-release receipts survive slot reuse.

Report the task/pool, verification results and one explicit final state:
reservation retained with path/reservation ID, handed back to its coordinator, or
finished with snapshot ID and restore command. Include unresolved writer or
recovery problems. A local snapshot is not a remote backup.
