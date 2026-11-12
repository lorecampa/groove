# Install Groove

Install the CLI and the Groove skill for the agent you're running in. Use an
existing working installation unless the user asked to update it.

## Install the CLI

Groove runs on Linux and macOS 13+ with local storage (APFS on macOS). It needs
Git 2.40+ and Rust/Cargo 1.89+ to build; use maintained, patched toolchains.

```sh
cargo install --locked groove-cli
```

This installs the latest published release. Only select a specific version if
the user requests one. The package is `groove-cli`; the command is `groove`.
If it isn't on PATH, locate Cargo's installation directory and make it available
to your terminal.

## Install the skill

Install [skills/groove/](https://github.com/lorecampa/groove/tree/main/skills/groove)
from this repository for your current agent. Use the skill installer and discovery
location supported by that agent; inspect its existing configuration if needed.
Install only for the current agent, not every agent found on the machine.

By default, make the skill available across the user's projects so it also loads
in newly acquired worktrees. Install into a particular repository only when the
user asks for that. There is no need to ask the user to choose a directory.

If copying manually, copy the whole `skills/groove/` directory, including
`references/`. Compare an existing copy before replacing it and preserve local
customizations. While the repository is private, use an authenticated checkout
or the user's existing Git access to obtain the skill.

## Verify

```sh
groove --version
groove --json --help
```

Confirm that the `groove` skill is discoverable by your agent. If discovery needs
a new session, tell the user to restart rather than claiming it is already loaded.
Report the CLI version and installed skill location. Do not initialize a project
pool or start a task as part of installation.

## Project policy

The skill teaches the workflow; project instructions decide when to require it.
For repositories that use Groove, add a short policy to the agent's project
instruction file, for example:

> Use the Groove skill for coding task workspaces in this repository. Workers
> already assigned a reservation reuse it and hand results back; the coordinator
> owns acquisition and finalization.

Commit project instructions before acquiring if workers need them in their
checkouts. Uncommitted files do not propagate to a new checkout.

## Source installations

For development, build from an existing checkout:

```sh
cargo install --path /path/to/groove/crates/groove-cli --locked
```

If Git authenticates but Cargo's built-in Git transport fails, use the configured
Git transport for that invocation:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git ssh://git@github.com/lorecampa/groove --locked groove-cli
```

This installs the development revision using existing Git credentials. Keep
credentials out of URLs and install the skill from the matching revision.
