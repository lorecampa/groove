# Using Groove with an agent

Install the [CLI](../README.md#install) once per machine. Each repository has its
own pool, shared by linked worktrees through Git's common directory. No daemon
or extra registration is required.

## Teach the workflow

Copy the [portable skill](../skills/groove/SKILL.md) into your agent's supported
skill directory, or use its contents as project instructions. For example, a
Codex installation using the default home can load it from
`~/.codex/skills/groove/SKILL.md`. Restart the agent if needed for discovery.

A task prompt can simply say: “Use Groove for this task; preserve the result
before returning the workspace.” Set `GROOVE_OUTPUT=json` in the agent's
environment so every response is one JSON object (help included, in
`result.text`). Without it, output is human-readable text.

## Acquire and work

```sh
export GROOVE_OUTPUT=json
groove init --capacity 4
groove acquire --task issue-42 --request-key issue-42-attempt-1
```

Check `ok` before using `result.worktree_path`. The checkout is clean, with HEAD
detached at the base; Groove never creates or switches branches. Follow-up
commands use `--task issue-42`. Only one active reservation may carry a label; a
second `acquire` with it fails with `INVALID_ARGUMENT` whose details give the
existing `lease_id` and `worktree_path`, so a lost response without a request key
is recoverable. Work only in the returned checkout. Shared refs
and configuration affect other worktrees.

Use `groove exec --task issue-42 -- cargo test` for managed commands. It
captures logs, supports deadlines and cancellation, and rejects release while
running. Ordinary terminal commands also work, but their writers remain your
responsibility. See [execution](execution.md) for descendant and crash boundaries.

## Preserve and finish

After all workers, tests, watchers and dev servers stop:

```sh
groove finish --task issue-42 --description 'task complete'
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
completed keys are rejected. Keys do not expire and are repository-scoped.
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
Lost
release responses can be retried until the slot is reassigned; stale IDs cannot
release a successor. `finish` keeps a durable receipt that survives reassignment.

If saving results or stopping writers fails, keep the reservation and report what
needs recovery. Use one Groove version per live pool.

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

Finish returns the pinned snapshot ID. Recover its files and staging state with
`groove snapshot restore ID --to /new/recovery-directory`, even after slot reuse.
Retry finish by lease ID after a lost response. See [project workspaces](project.md)
for caches, environment and setup limits.
