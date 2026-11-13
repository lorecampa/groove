# Reservation retries and recovery

Read the installed command's help for current details; use JSON and preserve the
original pool selection. Never infer ownership from a listing or PID.

| Situation | Action |
| --- | --- |
| Lost acquisition response | Repeat the same request key, task, pool and base argument. Groove returns the original reservation even if the branch moved. |
| Acquisition used no key | Inspect `list` and establish which reservation belongs to the task before attempting another acquisition. |
| Active task label already exists | Inspect the lease/path in the error; determine whether it is this task's lost response or another task. Do not take over another reservation. |
| Changed inputs or completed key | Do not repurpose that key. A genuinely new task needs a new key; finishing a prior task does not free its key. |
| Lost release response | Retry the original lease ID, pool and `--save` setting. Labels can already refer to a newer task. `release --save` has a durable receipt; a plain release retry can become stale after reassignment. |
| `WORKSPACE_IN_USE` | Inspect `error.details.processes`, stop the task's writers and wait for them to exit. Detection is best effort and covers your own processes only. |
| `UNSAVED_CHANGES` on release | Use `release --save` to preserve the task's result, or retain the reservation. Discard only when explicitly requested. |
| Snapshot failure or lock timeout | Keep the reservation, resolve the reported cause, and retry release --save with the same lease ID. |

Legacy execution records or locks from older Groove versions still block reuse.
Before upgrading, use the previous version to stop managed commands and resolve
orphaned runs after stopping all external writers. Never delete their records or
locks to force reuse; retain the reservation if ownership is uncertain.

Never use `--force`, delete metadata, expire reservations or remove permanent
lock files as an automatic retry strategy. `--force` can discard results and
skip best-effort process checks; it never bypasses descriptor ownership or legacy execution protection.
