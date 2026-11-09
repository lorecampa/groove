---
name: groove
description: Use Groove to acquire exclusive reusable Git worktrees for coding tasks, preserve results, and release workspaces after all writers stop. Apply when the user asks to use Groove or a configured task workflow requires pooled worktrees.
---

# Groove task workspaces

Use the installed `groove` CLI through the terminal. MCP configuration is optional.
Run `groove --help` for command details; all responses, including help, are JSON.
Check `ok` and exit status before using `result`. Never use a path from a failed call.

## Acquire and work

- Resolve the intended repository and record its absolute path. `--repo` is optional
  when commands run within that repository, including its linked worktrees.
- If no pool exists, run `groove init --capacity 4` in that repository, unless the
  user chose other settings. Existing pool settings are immutable; do not replace
  them or remove metadata on a configuration conflict.
- Run `groove acquire --base HEAD` (or the requested revision). Save
  `result.slot_id`, `result.lease_id`, and `result.worktree_path` outside the
  disposable checkout. A successful ordinary CLI acquire is durably reserved
  even after the command exits; no background process or inherited descriptor is needed.
- Perform all task file, Git and terminal operations in `result.worktree_path`.
  Initial HEAD is detached. Create a uniquely named task branch before committing
  results; do not check out a branch already used by another worktree.
- If capacity is full or protected, wait or inspect `groove leases`. Never create
  overflow worktrees, delete protection files, or release someone else's reservation.

## Preserve and release

Preserve results on a task-owned branch or export a patch and artifacts outside
all disposable slots. Untracked files and unreferenced detached commits may be lost
on reuse. Stop every writer, including workers, tests, watchers and dev servers.
Then, using the original repository path and returned identifiers:

```sh
groove --repo /absolute/path/to/repository release --slot SLOT --lease-id UUID --writers-stopped
```

Replace SLOT and UUID with the acquired values. Do not confirm stopped writers
unless they have actually stopped. Never use a released path until acquired again.
If preserving results or stopping writers fails, leave the reservation protected
and report what remains to be recovered.

## Interrupted calls and recovery

After a lost acquire response, run `groove leases` before acquiring again. Request
IDs correlate output; they are not idempotency keys. Do not assume an unfamiliar
listed reservation belongs to this task. Stop and identify ownership if ambiguous.
A command crash or machine restart does not expire a durable reservation.

After a lost release response, repeat release with the same identifiers and
stopped-writer confirmation. A stale generation is rejected once the slot has
been acquired again. Listing is diagnostic, not proof that writers have stopped.
Never remove locks or protection records to bypass recovery. A live MCP owner
must release through its own server; CLI recovery must not unlock it.
