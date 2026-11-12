# Reservation retries and recovery

Read the installed command's help for current details; use JSON and preserve the
original pool selection. Never infer ownership from a listing or PID.

| Situation | Action |
| --- | --- |
| Lost acquisition response | Repeat the same request key, task, pool and base argument. Groove returns the original reservation even if the branch moved. |
| Acquisition used no key | Inspect `list` and establish which reservation belongs to the task before attempting another acquisition. |
| Active task label already exists | Inspect the lease/path in the error; determine whether it is this task's lost response or another task. Do not take over another reservation. |
| Changed inputs or completed key | Do not repurpose that key. A genuinely new task needs a new key; finishing a prior task does not free its key. |
| Lost finish/release response | Retry the original lease ID and pool. Labels can already refer to a newer task. Finish has a durable receipt; a release retry can become stale after reassignment. |
| `WORKSPACE_IN_USE` | Inspect `error.details.processes`, stop the task's writers and wait for them to exit. Detection is best effort and covers your own processes only. |
| `UNSAVED_CHANGES` on release | Use `finish` to preserve the task's result, or retain the reservation. Discard only when explicitly requested. |
| Snapshot failure or lock timeout | Keep the reservation, resolve the reported cause, and retry finish with the same lease ID. |
| Orphaned managed execution | Keep protection. Stop external writers; operator recovery is explicit and separate from normal task completion. |

`stop` requests managed cancellation. Process groups cannot contain descendants
that create new sessions, so inspect external writers as well. For an orphan,
authorized operators can use `recover --lease-id ID` after stopping writers.
Recovery does not publish, preserve results or grant another task ownership.
If you cannot establish that recovery is appropriate, report the retained lease
and error rather than forcing reuse.

Never use `--force`, delete metadata, expire reservations or remove permanent
lock files as an automatic retry strategy. `--force` can discard results and
skip best-effort process checks; it never bypasses a running managed execution.
