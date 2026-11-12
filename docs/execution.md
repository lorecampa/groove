# Managed execution and recovery

A reservation controls reuse of a checkout. A managed execution controls one
explicitly launched command and its participating descendants. Ending execution
never releases the reservation.

Use `groove exec --help`, `groove stop --help` and `groove recover --help` for
command options and defaults. This guide explains ownership and recovery.

## Run and stop

```sh
groove exec --task fix-parser --timeout-ms 60000 -- cargo test
groove stop --task fix-parser
```

Run `stop` from another terminal. It requests cancellation; inspect
until the execution record disappears before releasing. Only one
managed command runs per reservation. Other reservations can execute concurrently.
`stop` waits for a bounded acknowledgement from the authenticated supervisor.
An interrupted or unacknowledged request is not proof that execution stopped;
inspect the reservation before taking another action. Disconnected/unverifiable
control clients cannot clear protection or interrupt worker supervision.

Programs receive direct argv, the checkout as their working directory,
`GROOVE_LEASE_ID` and `GROOVE_WORKTREE`. Stdin is closed. Shell syntax requires an
explicit `bash -c` command. **All inherited `GIT_*` and `GROOVE_*` variables are
removed**, including `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_SSH_COMMAND` and
repository-selection overrides. Verified `GROOVE_*` context is then assigned.
Other inherited variables and ordinary tool configuration remain available.
Set reviewed Git variables explicitly inside command argv when needed (for
example `-- env GIT_AUTHOR_NAME=Example git ...`); project environment entries
cannot set these prefixes.

Execution locks are also used briefly by history/log maintenance. Contention
returns retryable `INVALID_LEASE` (exit 5); it does not prove a worker is active.
Retry the same operation and lease ID after contention clears. `release --save` reuses any
saved `error.details.snapshot_id` receipt. A recorded execution still requires
normal stop/recovery; never remove protection or locks.

stdout/stderr go to private files under
the selected pool's `task-logs/<run-id>/` directory, outside the reusable checkout.
The pool state directory is `<GIT_COMMON_DIR>/groove/` for `default` and
`<GIT_COMMON_DIR>/groove/pools/<name>/` for a named pool.
Both streams are also mirrored live to Groove's stderr in human and JSON modes;
`--quiet` suppresses mirroring without changing captured files or execution status.
Worker bytes are not classified by log level. Child buffering can delay output;
cross-stream ordering is unspecified. Terminal delivery is bounded and best-effort
so a slow or failed reader cannot prevent timeout or cancellation. Consult the
saved files for complete output within `--max-log-bytes`.
The result (text or `--json`) contains both paths. Arguments are not persisted in execution
metadata; worker output may contain sensitive data. Task logs are separate from
[CLI diagnostic logs](logging.md) and have no automatic retention policy.

Output monitoring is periodic: temporary disk usage can exceed the retained-byte
limit. There is no aggregate disk quota across executions.

The CLI handles SIGINT/SIGTERM as cancellation. `stop` works independently of
the running command. The supervisor terminates its process
group even when the direct child exits first, then checks teardown before clearing
the execution record. A failed teardown keeps both prohibitions in place.

An execution response can have `ok:true` and a nonzero worker result. The
generated output contract in `groove exec --help` explains worker exit statuses
and termination outcomes. The reservation remains protected after every outcome.
Cancellation observed before launch records a cancelled run with no leader PID;
no worker starts. Its empty captured logs and terminal history remain available.

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
before spawn. A live worker inherits that lock. Release rejects either a
live execution lock or a remaining record, including after supervisor death.
Protection is never cleared by a timeout, PID disappearance or age.

1. Run `groove list --slots` and identify the task and run.
2. Try `groove stop --task NAME`. A live supervisor stops its group.
3. For an orphaned run, independently stop **every** writer. Then run
   `groove recover --task NAME` to clear the execution record. A live execution
   lock rejects recovery.
4. Preserve results and `release --save`, or explicitly `release`.

`recover` is an operator assertion that external writers stopped; it does not
search for or signal a possibly reused PID. It clears the execution prohibition
after recording recovery in history; it keeps the reservation.
If ownership is uncertain, leave the checkout reserved. Never remove lock files,
protection or execution records by hand to force reuse.

## History and explicit log cleanup

Every managed execution, including setup steps, recipes and agents, writes
`task-logs/<run-id>/run.json` before spawning. A successful teardown syncs logs
and publishes the outcome before clearing execution protection. History survives
release and slot reuse. Setup steps skipped on a cache hit create no run.
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
age, retaining the small history record. `--older-than 0s` selects
all terminal runs. It rechecks eligibility under the slot execution lock and skips
slots with live locks or orphaned execution records, even for an older run on the
same slot. A `removing` state makes interrupted deletion retryable. Inspect every
reported entry; partial failures produce exit 7. `history delete` explicitly
forgets one terminal record only after its logs have been cleaned.

Unknown/legacy directories without run history are reported as untracked and
never automatically deleted. Cache eviction, workspace contents, snapshots,
reservation records, saved-release receipts and CLI diagnostic logs are outside this
cleanup command. Storage sizes count logical lengths; reflinks and hardlinks can
share blocks, so reported sizes do not predict actual disk space reclaimed.

## Checkpoint and release

`groove release --save --task NAME` saves and pins a snapshot, persists its receipt,
then releases. A failed snapshot leaves the reservation protected; a release error
reports the saved snapshot ID in `error.details`. Retry the same command after
resolving the error. Once a checkpoint receipt exists, retries reuse it, even if
the description changes; do not edit more files between a partial release --save and retry.
The receipt survives a crash and later slot reuse without touching the successor.

Snapshots preserve staged, working and non-ignored untracked files, with the
[documented exclusions](snapshots.md). All writers must stop before capture;
this is not an atomic filesystem snapshot. Explicitly unpin/delete finished
snapshots when no longer needed. Request-key journals and saved-release receipts do not
expire and survive explicit pool deletion, together with execution history/logs.
After `destroy`, history/storage and snapshot commands remain available; completed
saved-release receipts can still be retrieved by lease ID. A replacement pool with fewer
slots retains the execution-lock identities needed to clean older logs safely.

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
