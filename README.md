# Groove

Reusable Git worktree pools for coding agents and CI. Each task gets an exclusive,
verified checkout; released slots are cleaned and reused.

**Development preview · Linux only · Git 2.40+ · Rust 1.89+ to build.**

## Install

From a checkout of this repository:

```sh
cargo install --path crates/groove-cli --locked
cargo install --path crates/groove-mcp --locked
```

Executables are installed into `~/.cargo/bin`; ensure it is on your `PATH`.

## Connect your agent

Add a **local stdio MCP server** in your agent application:

- Executable: absolute path to `groove-mcp` (run `command -v groove-mcp`).
- Arguments: `--repo /absolute/path/to/your/repository`.

Then add these agent instructions:

> Use Groove for task workspaces. Call groove_init with capacity 4, then
> groove_acquire with the requested base and a unique request_key for the task.
> Work only in result.lease.worktree_path. Save changes to a task-owned branch or
> export a patch outside the workspace, stop all workers/watchers, then call
> groove_release with slot_id, lease_id and writers_stopped=true.
> After a lost response or restart, inspect groove_leases. Never delete Groove's
> lock/protection files or create overflow worktrees when the pool is full.

The MCP server keeps workspaces reserved between tool calls. Reservations survive
server shutdown/crash until explicitly released after their writers have stopped.
See [MCP setup and recovery](docs/mcp.md) for configuration JSON and tool details.

## CLI and Rust integrations

```sh
groove --repo /path/to/repo init --capacity 4
```

The CLI's `acquire` requires a supervisor that retains a transferred descriptor;
JSON capture alone does not reserve a workspace. Use the [CLI client](crates/groove-cli/examples/client.rs),
[Rust library example](crates/groove-core/examples/agent.rs), or [protocol](docs/protocol.md).

Worktrees are created on acquisition under `<GIT_COMMON_DIR>/groove/worktrees/`.
On a new pool, `init --worktree-root /absolute/empty/path` selects another root.
Cleanup defaults to `pristine`, deleting untracked and ignored files. Opt-in
`warm` may retain ignored files. Worktrees share Git objects, refs and
configuration; they are not security sandboxes.

[Contributing](CONTRIBUTING.md) · [Operations](docs/operations.md) ·
[Testing](docs/testing.md) · [Release readiness](docs/releasing.md) ·
[Changelog](CHANGELOG.md) · [Security](SECURITY.md)

Licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
