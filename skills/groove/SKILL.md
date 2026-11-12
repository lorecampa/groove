---
name: groove
description: Use Groove to acquire exclusive reusable Git worktrees, one per task, for coding tasks; preserve results and release workspaces after all writers stop. Apply when the user asks to use Groove or a configured task workflow requires pooled worktrees.
---

# Groove task workspaces

Use the installed CLI through the terminal. Set
`GROOVE_OUTPUT=json` (or pass `--json`) so every response, including errors and
help, is one JSON object. Check `ok` and the exit status before using a result.
Read `groove --help` and the relevant command's `--help` for details.

## Acquire and work

1. If the repository has no pool, run `groove init --capacity 4` unless the user
   chose other settings. A ready pool rejects repeated init; inspect it with
   `groove config show`. Grow it explicitly with `groove config set --capacity N`;
   never enlarge a pool to bypass contention.
2. Pick a short task name and a unique request key, then run
   `groove acquire --task NAME --request-key KEY`. Later commands select the
   reservation with `--task NAME`; an active name cannot be reused (the error
   names the existing reservation). Keep the key and
   `result.worktree_path` outside the checkout. The reservation survives exits.
3. Do all file, Git and terminal work in the returned path. HEAD is detached;
   Groove never creates branches. Refs and configuration are shared with other
   worktrees.
4. Prefer `groove exec --task NAME -- PROGRAM ARGS` for workers that need
   deadlines and captured logs. On capacity errors, wait or inspect `groove list`;
   never create overflow worktrees.

## Preserve and release

Stop every writer first: workers, tests, watchers and dev servers. `release`
and `finish` refuse while other processes still use the workspace
(`WORKSPACE_IN_USE`, listed in `error.details.processes`), and `release` also
refuses unsaved changes (`UNSAVED_CHANGES`). Stop those processes or use `finish`;
use `--force` only when you mean to discard the work.
`groove stop --task NAME` asks a managed run to stop; run `groove show
--task NAME` until its execution clears. Process groups do not contain
descendants that start new sessions.

Then run `groove finish --task NAME`. It pins a local snapshot of staged and
working files, then releases the slot. Ignored
files are not captured. Recover a snapshot with
`groove snapshot restore ID --to PATH`. Snapshots are local, not backups.

`groove release --task NAME` releases without a snapshot. The next
acquisition may delete everything in the slot. Never use a released path.

If stopping writers or preserving results fails, leave the reservation in place
and report what needs recovery.

## Retry and recover

After a lost response, repeat the same acquire (same key, base and task).
Changed inputs and completed keys are rejected; use a new key for a new task.
Without a key, inspect `groove list` before acquiring again. Listings, PIDs and
timestamps never prove ownership or that writers stopped.

If two reservations share a label, `--task` refuses to guess; use `--lease-id`
from `groove list`. Lost release or finish responses can be retried with the same
selector; stale IDs cannot release a later task. Reservations never expire.

An orphaned managed execution keeps release blocked. Stop all external writers,
then run the operator-only `groove recover --task NAME`. If ownership is
uncertain, leave protection intact. Never delete locks, protections or execution
records to force reuse.

## Project setup and recovery

For reviewed project setup use `acquire --setup PROFILE` or
`exec --task NAME --setup PROFILE`, and check `preparation.status`. Setup
never runs implicitly. A failed, interrupted or changed setup needs an explicit
`--retry`, which restarts from the first step. Never keep results in approved
cache roots; setup may clear them.

`exec --recipe NAME` and `exec --agent NAME -- ARGS` append literal arguments and
check required setup. `groove show --task NAME` shows the path but grants no
ownership.

`finish --task NAME` returns a pinned snapshot ID. Recover its files and staging
state with `snapshot restore ID --to /new/recovery-directory`. Retry finish by
lease ID after a lost response; it reuses the saved snapshot even after slot reuse.
Nothing is pushed or merged automatically.
