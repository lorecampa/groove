# Managed execution and recovery

A reservation controls reuse of a checkout. A managed execution controls one
explicitly launched command and its participating descendants. Ending execution
never releases the reservation.

## Run and stop

```sh
groove exec --task fix-parser --timeout-ms 60000 -- cargo test
groove stop --task fix-parser
```

Run `stop` from another terminal. It requests cancellation; inspect
until the execution record disappears before finishing or releasing. Only one
managed command runs per reservation. Other reservations can execute concurrently.
`stop` waits for a bounded acknowledgement from the authenticated supervisor.
An interrupted or unacknowledged request is not proof that execution stopped;
inspect the reservation before taking another action. Disconnected/unverifiable
control clients cannot clear protection or interrupt worker supervision.

Programs receive direct argv, the checkout as their working directory,
`GROOVE_LEASE_ID` and `GROOVE_WORKTREE`. Stdin is closed. Shell syntax requires an
explicit `bash -c` command. Repository-selection `GIT_*` overrides are removed.
The worker uses your environment and normal tool configuration.

stdout/stderr go to private files under
`<GIT_COMMON_DIR>/groove/task-logs/<run-id>/`, outside the reusable checkout.
Both streams are also mirrored live to Groove's stderr in human and JSON modes;
`--quiet` suppresses mirroring without changing captured files or execution status.
Worker bytes are not classified by log level. Child buffering can delay output;
cross-stream ordering is unspecified. Terminal delivery is bounded and best-effort
so a slow or failed reader cannot prevent timeout or cancellation. Consult the
saved files for complete output within `--max-log-bytes`.
The result (text or `--json`) contains both paths. Arguments are not persisted in execution
metadata; worker output may contain sensitive data. Task logs are separate from
[CLI diagnostic logs](logging.md) and have no automatic retention policy.

| Control | Default | Behavior |
| --- | --- | --- |
| `--timeout-ms` | `0` | No runtime deadline; positive values trigger termination. |
| `--grace-ms` | `500` | SIGTERM grace before SIGKILL; maximum 60000 ms. |
| `--max-log-bytes` | `8388608` | Per stream; exceeding it stops the worker and truncates retained output. |

Output monitoring is periodic: temporary disk usage can exceed the retained-byte
limit. There is no aggregate disk quota across executions.

The CLI handles SIGINT/SIGTERM as cancellation. `stop` works independently of
the running command. The supervisor terminates its process
group even when the direct child exits first, then checks teardown before clearing
the execution record. A failed teardown keeps both prohibitions in place.

An execution response can have `ok:true` and a nonzero worker result. CLI status
matches the worker exit code, or `128 + signal`; timeout returns 124 and output
limit returns 1. `result.execution.termination` (`exited`, `timed_out`,
`cancelled`, `output_limited`) distinguishes these cases from a Groove error. The reservation remains protected after every outcome.

## Descendant tracking

Default execution uses a dedicated process group, suitable for cooperative
workers. Descendants that create another session/group can escape it, and a
writer can close inherited descriptors. Stop such external writers yourself
before release. Neither a PID lookup nor missing descriptors prove quiescence.
Groove deliberately does not manage cgroups or containers; run work that needs
stronger isolation inside your own container or systemd scope.

Linux CLI supervisors enable child-subreaper behavior to reap adopted group members.
On macOS, orphan adoption/reaping belongs to the system reaper. Both implementations
keep the leader unreaped through group signalling and require the process group to
disappear within the teardown deadline. A remaining group, including an unreaped
member owned by another parent, causes an error and retains durable protection.

## Crashes and recovery

Execution has a separate permanent flock and a durable execution record written
before spawn. A live worker inherits that lock. Release/finish reject either a
live execution lock or a remaining record, including after supervisor death.
Protection is never cleared by a timeout, PID disappearance or age.

1. Run `groove list --slots` and identify the task and run.
2. Try `groove stop --task NAME`. A live supervisor stops its group.
3. For an orphaned run, independently stop **every** writer. Then run
   `groove recover --task NAME` to clear the execution record. A live execution
   lock rejects recovery.
4. Preserve results and `finish`, or explicitly `release`.

`recover` is an operator assertion that external writers stopped; it does not
search for or signal a possibly reused PID. It clears the execution prohibition
after recording recovery in history; it keeps the reservation.
If ownership is uncertain, leave the checkout reserved. Never remove lock files,
protection or execution records by hand to force reuse.

## History and explicit log cleanup

Every managed execution, including setup steps, recipes and agents, writes
`task-logs/<run-id>/run.json` before spawning. A successful teardown syncs logs
and publishes the outcome before clearing execution protection. History survives
release, finish and slot reuse. Setup steps skipped on a cache hit create no run.
Only the program name is recorded, not arguments or environment values.

```sh
groove history --task fix-parser
groove history show RUN_ID
groove storage show
groove storage clean --logs --older-than 30d --dry-run
groove storage clean --logs --older-than 30d
groove history delete RUN_ID
```

History filters refer to historical labels; use `--lease-id` to distinguish reused
task labels. `unresolved` means no terminal outcome was durably published. It may
be active or interrupted. Explicit recovery marks it `recovered`, with unknown
exit status; an already-recorded completed outcome is preserved.

Cleanup permanently removes stdout/stderr of terminal runs older than the requested
age (default 30 days), retaining the small history record. `--older-than 0s` selects
all terminal runs. It rechecks eligibility under the slot execution lock and skips
slots with live locks or orphaned execution records, even for an older run on the
same slot. A `removing` state makes interrupted deletion retryable. Inspect every
reported entry; partial failures produce exit 7. `history delete` explicitly
forgets one terminal record only after its logs have been cleaned.

Unknown/legacy directories without run history are reported as untracked and
never automatically deleted. Cache eviction, workspace contents, snapshots,
reservation records, finish receipts and CLI diagnostic logs are outside this
cleanup command. Storage sizes count logical lengths; reflinks and hardlinks can
share blocks, so reported sizes do not predict actual disk space reclaimed.

## Checkpoint and release

`groove finish --task NAME` saves and pins a snapshot, persists its receipt,
then releases. A failed snapshot leaves the reservation protected; a release error
reports the saved snapshot ID in `error.details`. Retry the same command after
resolving the error. Once a checkpoint receipt exists, retries reuse it, even if
the description changes; do not edit more files between a partial finish and retry.
The receipt survives a crash and later slot reuse without touching the successor.

Snapshots preserve staged, working and non-ignored untracked files, with the
[documented exclusions](snapshots.md). All writers must stop before capture;
this is not an atomic filesystem snapshot. Explicitly unpin/delete finished
snapshots when no longer needed. Request-key journals and finish receipts do not
expire and should remain with the pool.

## Project commands

`exec --setup NAME`, `--recipe NAME` and `--agent NAME` share managed execution.
Their worker output, and `acquire --setup` output, follows the same stderr/quiet rules.
Setup is explicitly requested, ordered, cancellable and durable; named commands
append trailing argv literally and verify declared readiness. Recipes default to
600000 ms per command, or their configured `timeout_ms`; a positive CLI runtime
limit overrides it. Built-in agent prefixes have no runtime limit. Setup failure
retains the reservation and stops later steps. `stop` also covers cache phases;
interrupted setup requires `--retry` after writer recovery.

Commands additionally receive stable slot identity, task label, project environment
and optional derived port. Inherited `GROOVE_*` is cleared before verified values
are assigned. No setup is triggered by acquire without `--setup`, inspection or
navigation. See [configuration and cache semantics](project.md).
