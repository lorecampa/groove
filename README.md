# <img src="assets/groove-icon.svg" alt="" width="40" align="top"> Groove

Reusable Git worktrees for coding agents. Give each task an exclusive checkout,
then release it for reuse when the work is saved and every writer has stopped.

## Install

Development preview: Linux, Git 2.40+, a local filesystem, and Rust 1.89+.
Until the package is published, install from source:

```sh
cargo install --git https://github.com/lorecampa/groove --locked groove-cli
```

Ensure `~/.cargo/bin` is on your `PATH`. Install once; use it across repositories.

## Instruct your agent

Add this to your project's `AGENTS.md` or your agent's persistent instructions:

> Use Groove for task workspaces. Read `groove --help` and the relevant command's
> `--help` before use. Initialize a pool if needed, acquire a checkout, and work
> only in its returned path. Create a unique task branch and save results before
> releasing. Stop every writer before `release --writers-stopped`. After a lost
> acquire response, inspect `groove leases` and identify ownership before retrying.
> Never release another task's reservation or bypass pool protection.

An installable [agent skill](skills/groove/SKILL.md) is also available.

## Basic workflow

From the repository you want to work on:

```sh
groove init --capacity 4
groove acquire --base HEAD
```

Commands return JSON. Check `ok`, then retain `result.slot_id`, `result.lease_id`
and `result.worktree_path`. In that checkout, create a unique branch, do your
work, and commit your results. The reservation survives command exit.

After saving results and stopping all workers, tests and watchers, run from the
original repository:

```sh
groove release --slot SLOT --lease-id UUID --writers-stopped
```

Replace `SLOT` and `UUID` with the acquired identifiers. Released files are
disposable: the next acquisition cleans the checkout, including ignored files
under the default policy.

Use `groove --help`, `groove acquire --help`, or `groove snapshot --help` for
commands, examples and recovery rules. Help is JSON too; with jq installed,
`groove --help | jq -r '.result.text'` displays the formatted text.

[Contributing](CONTRIBUTING.md) · [Optional MCP](docs/mcp.md) ·
[MIT](LICENSE-MIT) / [Apache-2.0](LICENSE-APACHE)
