# <img src="assets/groove-icon.svg" alt="" width="40" align="top"> Groove

Reusable Git worktrees for parallel coding tasks.

**Linux/macOS alpha.** The CLI and recovery workflow are tested, but APIs and storage
formats are still evolving. Review the [supported scope](docs/operations.md)
before using Groove for important work.

Give each task, or each agent, its own clean checkout from a fixed pool, with no
branch bookkeeping. Reservations survive shell calls and crashes until you finish
or release them.

- **Name it, use it.** `acquire --task foo` reserves a clean checkout; later
  commands find it by that name.
- **Bounded capacity.** Tasks wait for a free slot instead of piling up worktrees.
- **Crash-safe ownership.** Kernel locks, not timeouts or PIDs. Retry a lost
  response with the same request key.
- **Managed commands.** Run, time out and stop workers; logs stay outside the checkout.
- **Recoverable results.** `finish` pins a local snapshot before releasing the slot.

Use `--pool NAME` for workflows needing separate capacity, cleanup and caches;
omitting it selects `default`. Run `groove pools --help` for selection and deletion
rules. Pools share Git branches, objects and local snapshots.

## Install

Linux or macOS 13+ (Apple Silicon and Intel), Git 2.40+ and local storage
(APFS on macOS). Building requires Rust 1.89+. Pools need full
checkouts without submodules, content filters (such as Git LFS) or sparse checkout.

```sh
cargo install --git https://github.com/lorecampa/groove --locked groove-cli
```

Once a version is published on [crates.io](https://crates.io/crates/groove-cli),
install that release with:

```sh
cargo install --locked groove-cli
```

The package is `groove-cli`; the installed command is `groove`. Cargo builds it
from source. Tagged releases also provide prebuilt Linux and macOS binaries; see
[releases](docs/releasing.md) for installation and publishing instructions.

## Try it

From a Git repository with at least one commit:

```sh
groove init --capacity 4
groove acquire --task fix-parser                # prints reservation details, including the checkout path
groove exec --task fix-parser -- cargo test
groove finish --task fix-parser                 # snapshot the work, free the slot
```

The checkout starts at `HEAD` (or `--base main`) with HEAD detached; Groove never
creates or switches branches. `finish` pins a snapshot of everything you changed,
so you can get it back with `groove snapshot restore ID --to DIR`.
If you want to keep commits on a branch, create one yourself before releasing;
release detaches the checkout, so that branch is immediately usable elsewhere.
`release --task NAME` frees a slot without a snapshot, and refuses if there are
unsaved changes or other processes still using the workspace (`--force` overrides). The next acquisition
cleans the slot, ignored files included by default, so stop every writer first.
Snapshots are local; nothing is pushed.

Other things to try:

- `groove list` shows active reservations; `groove list --slots` shows every slot
  with its status and next step.
- `groove snapshot save 'before refactor'` checkpoints any checkout.
- `groove config set --capacity 8` grows the pool.
- `groove history --task fix-parser` finds past executions after release.
- `groove storage clean --logs --older-than 30d --dry-run` previews log cleanup.

## Scripts and agents

Output is human-readable by default. Add `--json`, or set `GROOVE_OUTPUT=json`,
to get exactly one versioned JSON object on stdout for every outcome, including
errors and help. Exit codes are stable categories; see `groove --help`.
Live worker output and diagnostics go to stderr; `--quiet` hides worker output
and warnings while preserving errors, results and saved logs.

```sh
groove --json acquire --task issue-42 --request-key issue-42-1
# {"api_version":1,"ok":true,"result":{"worktree_path":"/.../wt-0001",...}}
```

Agents read `result.worktree_path` from that response; `groove show --task NAME`
prints it again later.

Install the [agent skill](skills/groove/SKILL.md), or add this to your `AGENTS.md`:

> Use Groove for task workspaces. Acquire with `--task NAME` and a unique
> `--request-key`; after a lost response, retry the same command. Work only in the
> returned path. Stop all writers, then `groove finish --task NAME`. Never bypass
> another task's reservation.

A [`.groove.toml`](docs/project.md) can declare setup steps, artifact caches and
named commands (`acquire --setup dev`, `exec --recipe test`). Nothing runs implicitly.
No daemon is required.

## Learn more

[Agent workflow](docs/agents.md) · [Execution and recovery](docs/execution.md) ·
[Operations](docs/operations.md) · [Snapshots](docs/snapshots.md) ·
[Protocol](docs/protocol.md) · [Contributing](CONTRIBUTING.md)

Run `groove COMMAND --help` for examples, defaults and recovery rules.
Worktrees share Git objects, refs and configuration. Groove coordinates cooperative
workspace ownership; it is not a security sandbox or a CPU/memory quota.

Licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
