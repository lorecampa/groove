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

For source builds and Cargo/Git authentication problems, see the
[agent guide](agents.md#source-installations).
