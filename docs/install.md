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

## Install shell completions (optional)

Completions let you press Tab after `groove ` to discover commands and options.
Use the instructions for the shell in which you will run Groove; an agent's
execution shell may differ from your interactive terminal. See
`groove completions --help` for all supported shells and output details.

Run the completion commands below exactly as shown, without `--json`, so the
saved files can be loaded by your shell.

### Zsh (including Oh My Zsh)

Generate the completion file:

```sh
mkdir -p ~/.zsh/completions
groove completions zsh > ~/.zsh/completions/_groove
```

Add this line to your Zsh startup file (`~/.zshrc`, or `$ZDOTDIR/.zshrc` if you
use `ZDOTDIR`), before the existing `compinit` call:

```zsh
fpath=("$HOME/.zsh/completions" $fpath)
```

With Oh My Zsh, put it before `source $ZSH/oh-my-zsh.sh`; Oh My Zsh runs
`compinit` for you. If your configuration does not initialize completions, add
these lines after the `fpath` line:

```zsh
autoload -Uz compinit
compinit
```

Keep an existing initialization; do not add a second `compinit` call. Open a
new terminal, type `groove ` and press Tab. To check registration in that new
Zsh session, run:

```zsh
print -r -- ${_comps[groove]}
```

It should print `_groove`.

### Bash

```sh
mkdir -p ~/.local/share/bash-completion/completions
groove completions bash > ~/.local/share/bash-completion/completions/groove
```

Add the following line once to `~/.bashrc`:

```bash
source "$HOME/.local/share/bash-completion/completions/groove"
```

Ensure interactive login shells also load `~/.bashrc` from your existing Bash
login startup file (usually `~/.bash_profile`). Open a new terminal, type
`groove ` and press Tab. `complete -p groove` should show its registered function.

### Fish

```fish
mkdir -p ~/.config/fish/completions
groove completions fish > ~/.config/fish/completions/groove.fish
```

If you customize `XDG_CONFIG_HOME`, use `$XDG_CONFIG_HOME/fish/completions`
instead. Fish loads this file automatically. Open a new terminal, type
`groove ` and press Tab; `complete -C 'groove '` should list suggestions.

### Upgrades and agent setup

After upgrading Groove, rerun the generation command for your shell. It
overwrites the generated file; the startup configuration only needs adding once.
Completions cover commands, options and fixed choices, not live task names or
lease IDs.

Agents performing this setup should inspect the user's existing shell
configuration, preserve customizations and avoid duplicate entries. Generate
the script, check startup-file syntax (for example, `zsh -n ~/.zshrc`), and verify
completion registration in the intended shell. A clean shell with manually set
paths only checks the generated script, not the user's startup configuration.
If verification needs a new interactive terminal, say so rather than claiming
it is already active in the user's current terminal. Report the generated file
and any startup-file changes. Do not execute commands selected by completion.

## Install the agent plugins

For Claude Code or Codex, install the native plugins using that harness's CLI.
The `groove` plugin packages the [portable skill](../skills/groove/SKILL.md).
The optional `groove-project` plugin loads project workflow instructions at
session and subagent startup. Its hooks require Bash, Git and jq 1.6+ on PATH.
Skill updates and hook updates are separate so changing the skill does not
change Codex's hook package fingerprint.

### Claude Code

```sh
claude plugin marketplace add lorecampa/groove
claude plugin install groove@groove
claude plugin install groove-project@groove
```

The skill is available as `/groove:groove`. For updates, refresh the marketplace
with `claude plugin marketplace update groove`, then run
`claude plugin update groove@groove` and
`claude plugin update groove-project@groove`. Restart the session afterward.

### Codex

```sh
codex plugin marketplace add lorecampa/groove
codex plugin add groove@groove
codex plugin add groove-project@groove
```

Restart the session and review/trust the `groove-project` hooks in Codex's
`/hooks` menu. Installing a plugin does not automatically trust its hooks.
For updates, run `codex plugin marketplace upgrade groove` and reinstall the
two plugins with `codex plugin add`. Review changed hooks again when prompted.

### Other agents and existing skill installations

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

## Use Groove by default in a project

With `groove-project` installed, opt in once from the project's Git checkout:

```sh
git config --local groove.agent.enabled true
```

This preference lives in the clone's Git config and applies in its linked
worktrees. It is not committed and imposes no requirement on other contributors.
The hooks tell agents to load the skill for code, test and documentation changes;
assigned workers reuse their coordinator's reservation. No reservation is acquired
by the hook, and ending a session never releases one. Coordinators preserve
results and release explicitly through the skill.

To include your own project instructions, point to an existing UTF-8 Markdown
file outside reusable worktrees (maximum 32 KiB):

```sh
git config --local groove.agent.instructionsFile /absolute/path/to/workflow.md
```

The hook reads that file directly; no `AGENTS.md` or `CLAUDE.md` replacement is
needed. Configured files must be readable regular files with absolute paths.
The hook rejects missing or oversized files rather than silently omitting them.
Repository lookup uses the harness's hook `cwd`, not its process directory or
inherited `GIT_*` variables. Open a fresh session and ask it to describe the
project workflow already in its instructions without reading files or using
tools. Verify the plugin source and your custom rules are present. Test from a
reserved worktree too if that is where you launch agents. Skills being listed
alone does not establish that the context hook ran.

Disable the project preference with:

```sh
git config --local groove.agent.enabled false
```

For other harnesses, use their supported persistent agent instruction mechanism.
If a team explicitly wants shared project policy, it can add a short instruction:

> Use the Groove skill for coding task workspaces in this repository. Workers
> already assigned a reservation reuse it and hand results back; the coordinator
> owns acquisition and finalization.

Commit project instructions before acquiring if workers need them in their
checkouts. Uncommitted files do not propagate to a new checkout.

## Remove the plugins

For Claude Code:

```sh
claude plugin uninstall groove-project@groove
claude plugin uninstall groove@groove
claude plugin marketplace remove groove
```

For Codex:

```sh
codex plugin remove groove-project@groove
codex plugin remove groove@groove
codex plugin marketplace remove groove
```

Removing plugins does not release reservations, delete snapshots or remove
your instruction file. A manually installed skill is separate; remove only
the copy you installed when it is no longer needed.

## Develop the plugins locally

The authored skill remains in `skills/groove/`. After editing it, run
`bash scripts/sync-plugins.sh --write`; `--check` verifies the real-file mirror
under `plugins/groove/`. Native plugin installers can omit symlinks, so the
distributed skill includes its references as regular files.

Both marketplace commands accept an absolute path to a local checkout instead
of `lorecampa/groove`. Use a stable checkout or export the marketplace manifests
and `plugins/` to a private directory first. Do not register a reusable pool
worktree as a permanent marketplace source: its contents can change after release.
With Omnigent, run Codex installation against your normal user `CODEX_HOME`,
not an existing session's private home, then verify a fresh Omnigent session.
The integration uses native plugins and requires no Omnigent changes.

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
