# Using Groove with an agent

Install the [CLI](../README.md#install) once per machine. Each repository has its
own pool catalog, shared by linked worktrees through Git's common directory. No daemon
or extra registration is required.

## Teach the workflow

Ask your agent to follow the [installation guide](install.md). It installs the
CLI and the entire [portable skill](../skills/groove/SKILL.md), including its
references, where the current agent discovers skills. By default, the skill is
available across projects, including newly acquired worktrees.

The skill teaches the procedure; project instructions decide when it is required.
For a repository that uses Groove, add a short policy to the harness's project
instruction file (for example AGENTS.md, or CLAUDE.md for Claude Code):

> Use the Groove skill for coding task workspaces in this repository. Workers
> already assigned a Groove reservation use that assignment and hand results
> back; the coordinator owns acquisition and finalization.

Keep the detailed lifecycle in the skill instead of maintaining copies in every
instruction file. Review and commit project instructions/skills before acquiring
from that commit if workers need them; uncommitted files do not automatically
propagate into acquired checkouts. Installing a user skill does not require its
use in every repository.

A task prompt can simply say: “Use Groove for this task; preserve the result
before returning the workspace.” Set `GROOVE_OUTPUT=json` in the agent's
environment so every response is one JSON object (help included, in
`result.text`). Without it, output is human-readable text.

## Acquire and work

First distinguish the coordinator from an assigned worker. The coordinator
acquires, delegates, waits for writers to stop and eventually releases the
reservation. A worker with a supplied pool/lease/path stays in that reservation and returns results
without acquiring another slot or releasing the coordinator's reservation.
Managed workers receive `GROOVE_LEASE_ID`, `GROOVE_POOL`, `GROOVE_WORKTREE` and
`GROOVE_TASK` as context; these values alone do not grant ownership. Confirm the
assignment and inspect it before working. If the harness cannot safely direct
all edits to the returned checkout, relaunch/handoff there before making changes.

Omitting `--pool` always selects `default`. If a task names a workflow pool,
carry `--pool NAME` alongside its lease ID/task label on every pool command,
including retries, stop/recover, history and release --save. Inspect `groove pools` when
needed; never select another pool to bypass contention. Labels/keys are pool-local,
but snapshots, branches and Git configuration remain shared. The managed worker
environment exposes `GROOVE_POOL`; the CLI does not infer selection from it or cwd.

```sh
export GROOVE_OUTPUT=json
groove init --capacity 4
groove acquire --task issue-42 --request-key issue-42-attempt-1
```

Check `ok` before using `result.worktree_path`. Keep `result.lease_id` and the pool
selection outside the checkout for retries. The checkout is clean, with HEAD
detached at the base; Groove never creates or switches branches. Follow-up
commands use `--task issue-42`. Only one active reservation may carry a label; a
second `acquire` with it fails with `INVALID_ARGUMENT` whose details give the
existing `lease_id` and `worktree_path`, so a lost response without a request key
is recoverable. Work only in the returned checkout. Shared refs
and configuration affect other worktrees.

Use `groove exec --task issue-42 -- cargo test` for managed commands. It
captures logs, supports deadlines and cancellation, and rejects release while
running. A worker already inside managed execution runs commands directly rather
than nesting another `exec` on the same reservation. Ordinary terminal commands
also work, but their writers remain your
responsibility. See [execution](execution.md) for descendant and crash boundaries.

## Preserve and release

Assigned workers stop their writers and hand results back without finalizing.
The coordinator waits for their managed execution to end before releasing.
Keep the reservation instead if review needs a live checkout, and report that
state. Move coordinator terminals outside the slot before finalization.

After all workers, tests, watchers and dev servers stop:

```sh
groove release --save --task issue-42 --description 'task complete'
```

This saves and pins a local checkpoint, then releases the slot. It refuses while
other processes still use the workspace (`WORKSPACE_IN_USE`, with the processes in
`error.details.processes`); `release` additionally refuses unsaved changes
(`UNSAVED_CHANGES`). These checks are best effort: they only see your own
processes, so still stop writers yourself. `--force` skips them. Snapshot failure keeps the reservation protected.
Recover a checkpoint with `groove snapshot restore ID --to PATH`. Ignored files
are excluded; export required artifacts separately. Snapshots are local, not
remote backups.

`groove release --task issue-42` releases without a checkpoint. Nothing is pushed. The next acquisition may delete all
workspace contents. Never use a released path.

## Retry and recover

Use a new request key for each logical task. After a lost acquire response, retry
the **same key, base and task**, including after a restart; Groove
returns the original reservation even if the base branch moved. Changed inputs and
completed keys are rejected. Keys do not expire and are scoped to the selected pool.
`--request-id` is separate correlation metadata.

Without a key, inspect `groove list` and establish ownership before acquiring
again. `groove list --slots` reports all slots, task context, lock observations,
executions and failures. Neither listing proves that writers have stopped.

Capacity errors are retryable; use `acquire --wait-ms 5000` or retry later. Never
create overflow worktrees or remove metadata. `init` rejects an existing pool; use
`config show` to inspect it and `config set --capacity N` to grow it explicitly.
Interrupted initialization needs `init --resume`.

Reservations survive crashes and never expire. Stop managed execution with `stop`;
for an orphan, follow [operator recovery](execution.md#crashes-and-recovery).
Retry lost release responses with the original `--lease-id` and pool
selection. A task label can identify a newer reservation after release, so do not
retry by label. Release retries work until the slot is reassigned; stale IDs cannot
release a successor. `release --save` keeps a durable receipt that survives reassignment.

If saving results or stopping writers fails, keep the reservation and report what
needs recovery. Use one Groove version across the repository's pools.

## Project readiness and agent adapters

When instructed to use a project's reviewed `.groove.toml`, acquire with
`--setup PROFILE` or run `exec --task NAME --setup PROFILE`. Check the preparation
status before starting work. Keep the returned identifiers even when setup fails;
never silently acquire another slot. Retry failed or interrupted setup only with
`--retry` after stopping or recovering writers. Recipes and agents with
`requires_setup` check readiness but never run setup themselves.

`exec --task NAME --recipe RECIPE` and `--agent AGENT -- ARGS` share the execution
lifecycle and append literal argv. Built-in agent names only choose an executable;
configure or pass noninteractive arguments for your installed tool. Do not enable
agent worktree hooks that delete or recreate permanent Groove slots.
`groove show --task NAME` shows the path but grants no ownership.

`release --save` returns the pinned snapshot ID. Recover its files and staging state with
`groove snapshot restore ID --to /new/recovery-directory`, even after slot reuse.
Retry release --save by lease ID after a lost response. See [project workspaces](project.md)
for caches, environment and setup limits.

## Source installations

For development, build from an existing checkout:

```sh
cargo install --path /path/to/groove/crates/groove-cli --locked
```

If Git authenticates but Cargo's built-in Git transport fails, use the configured
Git transport for that invocation:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git ssh://git@github.com/lorecampa/groove --locked groove-cli
```

This installs the development revision. It uses existing Git credentials and
does not grant access to a private repository. Keep credentials out of URLs.
