# <img src="assets/groove-icon.svg" alt="" width="40" align="top"> Groove

[![Docs](https://img.shields.io/badge/docs-guides-4c8eda?style=flat)](#guides)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat)](#license)
[![Rust: 1.89+](https://img.shields.io/badge/rust-1.89%2B-orange?style=flat)](CONTRIBUTING.md)
[![Platforms: Linux / macOS](https://img.shields.io/badge/platforms-Linux%20%2F%20macOS-lightgrey?style=flat)](docs/operations.md)

Reusable Git worktrees for parallel coding tasks.

Groove gives each task its own checkout from a bounded pool. Reserve a workspace,
run commands, and save the result before returning it. Reservations survive shell
exits and crashes until you explicitly release them. No daemon is required.

For example, a coordinator handling 20 independent fixes can use a pool of four
workspaces. It assigns tasks to available slots, waits or retries when the pool
is full, and saves each result before returning the slot for another task.

Your agent runner handles task scheduling, agent communication and result
integration. Groove handles workspace allocation, reuse and recovery. Capacity
bounds reserved workspaces; the runner controls agent concurrency and resource use.

[![Groove demo: reserve workspaces, replenish ready spares, and save results](assets/groove-demo.gif)](assets/groove-demo.mp4)

A pool of 10 slots keeps a target of two spare worktrees ready in the background.
The demo reserves two tasks, shows the replenished spares, then saves and releases
both workspaces. [Watch the video](assets/groove-demo.mp4).

## Install

Requires Linux or macOS 13+, Git 2.40+ and local storage (APFS on macOS).
Building from source requires Rust 1.89+.

```sh
cargo install --locked groove-cli
```

The package is `groove-cli`; the installed command is `groove`.
Cargo builds the latest published release locally.

## Use Groove with your coding agent

Open your project in your coding agent and paste:

```text
Set up Groove in this project using:
https://github.com/lorecampa/groove/blob/main/docs/install.md

Install anything missing and configure this agent to use Groove
for coding tasks in this project by default. Keep this preference
personal to me. Reuse existing pool settings; if a pool needs to be
created, ask me how many workspaces it should have.
Tell me if I need to restart the agent.
```

After setup, give your agent tasks normally. You don't need to mention Groove in
every request.

[Manual setup and supported agents](docs/install.md#choose-your-agent).

## Use a workspace

From a Git repository with at least one commit:

```sh
groove init --capacity 4
groove acquire --task fix-parser
# Work and run tests directly in the printed checkout path.
groove release --save --task fix-parser
```

New pools put checkouts in `~/.worktrees/<project>-<repo-id>/<pool>` on Linux and
macOS. Set a different parent with
`groove config global set worktrees.root=/absolute/worktrees`, or choose an exact
pool location with `init --worktree-root`. Existing pools keep their locations.

Acquisition returns a checkout path with detached HEAD. Work in that path, stop
all writers, then run `groove release --save` to pin a local snapshot and free the
slot. Recover its files and staging state with:

```sh
groove snapshot restore SNAPSHOT_ID --to ../recovered-parser
```

Ignored files are excluded from snapshots. `release` frees a slot without saving
a snapshot; background preparation or the next acquisition can discard its
contents. Nothing is merged or pushed automatically.

Pools can separate workflow capacity, cleanup and caches. Use `groove pools` to
list them and pass the same `--pool NAME` on each pool command. Omitting it selects
`default`. Pools share Git branches, objects and configuration; they are not
security sandboxes. See [operations](docs/operations.md) for repository compatibility
and ownership boundaries.

## Command reference

CLI help is the reference for commands, options, defaults and command-specific
recovery instructions:

```sh
groove --help
groove acquire --help
groove destroy --help
```

For scripts, add `--json` to each command. Every response, including
errors and help, is one versioned JSON object. Check `ok` and the exit status
before using `result.worktree_path`. Use a request key to retry acquisition and
retain the lease ID for subsequent retries; see the [portable skill](skills/groove/SKILL.md).

## Guides

- [Agent skill](skills/groove/SKILL.md): workspace selection, retries and handoff.
- [Operations](docs/operations.md): pools, deletion, shared state and supported repositories.
- [Snapshots](docs/snapshots.md): capture, recovery and retention.
- `groove COMMAND --help`: inputs, JSON output contracts and recovery for integrations.
- [Contributing](CONTRIBUTING.md): development and validation.
- [Security](SECURITY.md): supported versions and private vulnerability reporting.

<a id="license"></a>
Licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
