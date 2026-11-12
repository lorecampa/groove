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
