# <img src="assets/groove-icon.svg" alt="" width="40" align="top"> Groove

Reusable Git worktrees for parallel coding tasks.

**Linux/macOS alpha.** Interfaces and storage formats are still evolving.

Groove gives each task its own checkout from a bounded pool. Reserve a workspace,
run commands, and save the result before returning it. Reservations survive shell
exits and crashes until you explicitly finish or release them. No daemon is required.

## Install

Requires Linux or macOS 13+, Git 2.40+ and local storage (APFS on macOS).
Building from source requires Rust 1.89+.

```sh
cargo install --locked groove-cli
```

The package is `groove-cli`; the installed command is `groove`.
Cargo builds the latest published release locally.

## Set up your coding agent

Give your agent this prompt:

> Install Groove for this agent by following
> https://raw.githubusercontent.com/lorecampa/groove/main/docs/install.md

Then ask: “Use Groove for this task.” See [agent workflows](docs/agents.md)
for assignments and handoff.

## Use a workspace

From a Git repository with at least one commit:

```sh
groove init --capacity 4
groove acquire --task fix-parser
groove exec --task fix-parser -- cargo test
groove finish --task fix-parser
```

Acquisition returns a checkout path with detached HEAD. Work in that path, stop
all writers, then finish. `finish` pins a local snapshot and frees the slot;
recover its files and staging state with:

```sh
groove snapshot restore SNAPSHOT_ID --to ../recovered-parser
```

Ignored files are excluded from snapshots. `release` frees a slot without saving
a snapshot; the next acquisition can discard its contents. Nothing is merged or
pushed automatically.

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
groove exec --help
groove destroy --help
```

For scripts, add `--json` or set `GROOVE_OUTPUT=json`. Every response, including
errors and help, is one versioned JSON object. Check `ok` and the exit status
before using `result.worktree_path`. Use a request key to retry acquisition and
retain the lease ID for subsequent retries; see the [agent workflow](docs/agents.md).

## Guides

- [Agents](docs/agents.md): workspace selection, retries and handoff, with a
  [portable skill](skills/groove/SKILL.md).
- [Project setup](docs/project.md): explicit setup steps, caches and named commands
  in `.groove.toml`.
- [Execution and recovery](docs/execution.md): worker ownership, cancellation and logs.
- [Operations](docs/operations.md): pools, deletion, shared state and supported repositories.
- [Snapshots](docs/snapshots.md): capture, recovery and retention.
- `groove COMMAND --help`: inputs, JSON output contracts and recovery for integrations.
- [Contributing](CONTRIBUTING.md): development and validation.

Licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
