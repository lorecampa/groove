# Reusable project workspaces

Groove provides permanent workspace slots for successive developer and agent tasks.
Acquire protects a verified checkout; explicit setup makes it ready; managed commands
run in it; `release --save` preserves a result before the slot becomes reusable. Stable paths,
independent artifacts and recoverable task generations are the product's foundation.
Each reservation can carry a task name (`acquire --task`) that later commands use
to find it, while the slot itself stays a permanent, reusable environment.

## Project configuration

`groove config project --example > .groove.toml` writes a starter. Review it,
adapt it to your toolchain, then commit it and its compatibility inputs.
`groove config project` validates and reports the selected
checkout's configuration without running commands. Unknown fields/versions fail.
Missing configuration means no setup, recipes, agents or custom environment.

```toml
schema_version = 1

[workspace]
port_base = 8000
[workspace.environment]
WORKSPACE_NAME = "{slot}"

[caches.dependencies]
paths = ["node_modules"]
inputs = ["package-lock.json"]
key = "node-22-linux-x86_64"
mode = "copy"

[setup.dev]
cache = "dependencies"
[[setup.dev.steps]]
name = "dependencies"
command = ["npm", "ci"]
when = "cache_miss"
[[setup.dev.steps]]
name = "check"
command = ["npm", "run", "check", "--if-present"]

[recipes.test]
command = ["npm", "test"]
requires_setup = "dev"

[agents.codex]
command = ["codex", "exec", "--sandbox", "workspace-write"]
requires_setup = "dev"
timeout_ms = 0
```

Commands are direct argv, with closed stdin and captured logs. An explicit shell
such as `["bash", "-c", "..."]` opts into shell syntax. Configuration is trusted
project code; managed execution controls lifetimes, not application privileges.
Setup/recipe commands default to 600000 ms per command; `timeout_ms = 0` disables
that deadline. A positive `exec --timeout-ms` overrides the configured per-command
deadline. It is not a deadline for the entire sequence. Setup has at most 256 steps.
Readiness checks use the configuration in the **leased checkout**, which can differ
from the checkout used to inspect project configuration.

## Explicit setup and retries

```sh
groove acquire --request-key issue-42-attempt-1 --task issue-42 --setup dev
# Or acquire first, then:
groove exec --lease-id UUID --setup dev
groove exec --lease-id UUID --recipe test
groove exec --lease-id UUID --agent codex -- 'Fix the parser and run tests'
groove show --lease-id UUID --details
groove stop --lease-id UUID
groove exec --lease-id UUID --setup dev --retry
```

Acquisition never runs project commands unless `--setup` is explicitly supplied.
Setup records `preparing` before artifacts/commands, runs steps in order, and records
`ready` only after successful commands, compatibility revalidation and cache
publication. Failure stops later steps and records `failed`. Every outcome retains
the reservation. `acquire --setup` returns the lease identifiers alongside the setup
result even when a command fails; inspect `error.details` on an operational failure.
Worker failure uses the worker exit code; timeout uses 124, cancellation 128+signal,
output limit or other recorded setup failure 7.

Repeating a ready setup with the same definition returns its receipt without rerunning
commands. Readiness describes that successful attempt, not a continuous filesystem
health check. A failed/interrupted/changed attempt needs explicit `--retry`, from
the first step. Design steps for replay; completed commands may have side effects.
Named recipes/agents with `requires_setup` check the lease generation, profile,
configuration and compatibility inputs immediately before execution, without
implicitly running setup. Reacquisition never inherits an earlier task's readiness.

The setup supervisor retains the execution lock during cache operations and between
steps. Live setup blocks release/recovery. `stop` also cancels cache work and
cache-lock contention; it keeps protection. SIGKILL leaves `preparing` and may leave
an orphan execution record. Stop all writers, use `recover` if required, then retry.
Age, a missing PID, or an existing directory never imply readiness or ownership.
After orphan recovery, raw managed repair commands can run before setup is retried;
`stop` targets their live execution even while old interrupted setup remains visible.

## Compatible artifact seeding

Approved roots must be ignored by Git, contain no tracked files, and stay inside
the checkout. They cannot overlap each other or compatibility inputs. Setup clears
only those approved roots before seeding, including a cache miss and an explicit
retry. Do not approve task results or secrets as caches. Ordinary acquisition does
not copy artifacts. Warm cleanup and setup caches are independent: warm preserves
ignored files opportunistically; setup explicitly replaces its approved roots.

Compatibility includes profile definition, explicit key, OS/architecture, input
contents, selected setup recipe and workspace configuration. Put every relevant
lockfile/toolchain constraint in `inputs` or `key`; Groove cannot infer ABI or build
correctness. Inputs must be regular files of at most 16 MiB each. `when="cache_miss"`
is an explicit promise that a step can be skipped after compatible seeding; `always`
is the default. Successful setup publishes the first immutable entry for the key.
Later workspace edits cannot modify that entry or another workspace's files.

`mode="copy"` uses independent byte copies; `mode="reflink"` requires Linux
FICLONE or macOS APFS cloning across artifact/workspace storage and fails without fallback.
Neither mode uses hardlinks.
Executable bits, file modification times and safe relative internal symlinks are
preserved. Ownership, ACLs and extended attributes are not copied. Special files,
escaping symlinks and symlink roots are rejected. Defaults are 20 GiB and one million
entries per snapshot, configurable as positive `max_bytes`/`max_files`. Every read
and duplicate verifies content; seeding still traverses and hashes the artifact tree.
Artifact paths must be UTF-8 and at most 128 components deep; manifests are bounded
to 128 MiB, so very large path lists can reach that limit before the entry quota.

To import an existing environment, stop its writers, then explicitly provide
`--cache-source /absolute/prepared-checkout` with setup. Input fingerprints must match;
only approved ignored roots are read. This does not run commands in the source.
The operator is responsible for its quiescence and artifact correctness.

Entries live in the selected pool's `artifacts/<key>` directory with permanent
sibling `<key>.lock` files. Pool state is `<GIT_COMMON_DIR>/groove/` for `default`
and `<GIT_COMMON_DIR>/groove/pools/<name>/` for named pools. Publication verifies a
private staging tree and atomically renames it after syncing. A crash before
publication may leave an unused staging directory;
there is no automatic cache eviction in this release. Corruption fails closed;
select a new explicit key after investigating. Never remove or replace cache locks
while clients may run. Keep source caches, pool storage and published entries within
your own trusted filesystem; this is not isolation from malicious same-user writers.

## Stable environment, selection and adapters

### Tool managers and local configuration

Groove uses your normal tool environment. A clean checkout does not automatically
trust mise/direnv configuration, install dependencies, or copy ignored `.env` files.
Review the project's configuration and use the tool manager's explicit trust/setup
commands before running project checks. For mise, review and trust the main
checkout's `mise.toml` and the leased checkout's configuration when requested;
`mise trust PATH/TO/mise.toml` applies that decision. A reviewed Groove setup
profile can then run `mise exec -- PROGRAM ARGS` as an explicit command.

Tool managers may read ancestor-directory configuration. The default pool sits
under the main checkout's `.git`, so its parent project configuration can also
apply. To keep workspaces outside that directory hierarchy, initialize a new pool
with a dedicated external root, for example:

```sh
groove --pool external init --capacity 4 --worktree-root /absolute/workspaces/my-project
```

The root is fixed until the pool is deleted and reinitialized. Check missing
dependencies, untrusted configuration and required environment files in worker
logs when a program fails; the task remains reserved. Do not copy secrets or grant
trust automatically.

### Workspace identity

Every managed command receives `GROOVE_POOL`, `GROOVE_SLOT_ID`, one-based `GROOVE_SLOT_INDEX`,
`GROOVE_LEASE_ID`, `GROOVE_WORKTREE` and `GROOVE_TASK` (empty when absent).
`port_base + slot number` supplies `GROOVE_PORT`; overflow fails before execution.
Slot numbers are pool-local: different pools can derive the same port. Choose
separate project port ranges for workflows or use `GROOVE_POOL` explicitly in
your commands. Pools do not isolate Git refs, configuration or network ports.
This is a predictable port choice, not an OS port reservation. Project environment
values accept `{slot}`, `{slot_index}`, `{worktree}`, `{lease}`, `{task}` as literal
substitutions, never shell evaluation. `GIT_*`/`GROOVE_*` overrides are rejected.

`show --task LABEL` resolves a unique active label and shows its path; duplicates
require an explicit lease or slot. Inspection never acquires a reservation.
`list --details` and `show --details` opt into bounded local Git status flags;
errors remain explicit and no forge/network request occurs. Ordinary inspection
reports durable setup state, the last failure and a suggested next action.

Agent adapters are argv prefixes under `[agents.NAME]`; extra CLI args are
appended literally. Built-in claude/codex/opencode/gemini/aider names map only to
their executable, so pass its noninteractive arguments explicitly or configure a
project adapter. No vendor process is launched during configuration reads or tests.
There is no native agent worktree hook that could delete a reusable slot. The skill
teaches this lifecycle.
The Codex example follows its [noninteractive invocation](https://learn.chatgpt.com/docs/non-interactive-mode).

## Getting results out

```sh
groove release --save --task issue-42
groove snapshot restore SNAPSHOT_ID --to ../recovered
```

`release --save` captures and pins a snapshot before releasing the reservation. Capture
failure retains protection. Retrying release --save by lease ID returns the saved result,
even after slot reuse. Restore the snapshot into a new directory to recover its
files and staging state without changing the successor's workspace. Nothing is
pushed or merged.

## Repository compatibility

Pool checkouts require full working trees without sparse checkout, submodules or
configured content filters such as Git LFS. See [requirements](operations.md#requirements)
for repository and filesystem compatibility, and [snapshots](snapshots.md#supported-boundaries)
for checkpoint-specific boundaries.
