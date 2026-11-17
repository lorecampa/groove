# Install Groove

Install the CLI and the Groove skill for the agent you're running in. Use an
existing working installation unless the user asked to update it.

## Install the CLI

Groove runs on Linux and macOS 13+ with local storage (APFS on macOS). It needs
Git 2.40+ and Rust/Cargo 1.89+ to build; use maintained, patched toolchains.

```sh
cargo install --locked groove-cli
```

This installs the latest release visible to the configured registry. A corporate
mirror can lag crates.io and may offer an older version than the one already
installed. Check `groove --version` before and after installation; avoid replacing
a newer working binary with an older mirrored version.

When the registry lags and GitHub is reachable, install the reviewed release tag:

```sh
cargo install --locked --git https://github.com/lorecampa/groove --tag vX.Y.Z groove-cli
```

Replace `vX.Y.Z` with the intended published tag from the repository's releases.
Git-tag installations retain their source; `groove upgrade --check` identifies
them and gives manual update guidance without querying an unrelated registry.

Only select a specific version if
the user requests one. The package is `groove-cli`; the command is `groove`.
If it isn't on PATH, locate Cargo's installation directory and make it available
to your terminal.

## Install shell completions (optional)

Run `groove completions --help` for shell setup and verification, including
Oh My Zsh. Inspect existing startup files before editing and verify in the user's
interactive shell. Regenerate the completion script after upgrading Groove.

## Choose your agent

Install the skill once, then tell your harness when to use it:

- **One session:** load the skill and ask the agent to use Groove throughout that
  session. There is no need to repeat the instruction for every task.
- **Project default:** add an instruction through your harness's existing
  personal or project configuration. For Omnigent, a private custom agent can
  carry the workflow and be selected as the project's default agent.

Installing the skill makes it available; it does not enable a project policy.
Groove ships skill plugins only. It does not install lifecycle hooks or read
`groove.agent.enabled` / `groove.agent.instructionsFile` settings.

| Agent | Skill setup | Persistent instructions |
| --- | --- | --- |
| Claude Code | Native `groove` plugin | Claude instructions, imports or local project instructions |
| Codex | Native `groove` plugin | Agent instructions or supported Codex configuration |
| Pi | Git/local Pi package or portable skill | Pi's existing personal/project instructions |
| Hermes | Skills Hub GitHub install or portable skill | Hermes' project context or session preload |
| OpenCode | Portable skill in its discovery directory | OpenCode instruction files/configuration |
| Gemini CLI | Native skill installer | Gemini's `GEMINI.md` context |

## Install the agent plugins

For Claude Code or Codex, register this repository's plugin catalog once, then
install the `groove` skill plugin. These are two separate native operations;
no public directory listing is needed.

### Claude Code

```sh
claude plugin marketplace add lorecampa/groove
claude plugin install groove@groove
```

In a fresh session, invoke `/groove:groove`. To update, run
`claude plugin marketplace update groove` and `claude plugin update groove@groove`,
then restart the session.

### Codex

```sh
codex plugin marketplace add lorecampa/groove
codex plugin add groove@groove
```

Start a fresh session and ask Codex to load the Groove skill. To update, run
`codex plugin marketplace upgrade groove` and `codex plugin add groove@groove`,
then restart. The skill plugin has no hooks to approve.

## Pi

Install the authored skill as a Git package:

```sh
pi install git:github.com/lorecampa/groove
pi list
```

The repository's private `package.json` selects only `skills/groove/`, with no
extensions or npm dependencies. In a fresh session invoke `/skill:groove`.
For a local checkout, use `pi install /absolute/path/to/groove`, or load it for
one launch with `pi --skill /absolute/path/to/groove/skills/groove`.
Keep installed local paths outside reusable pool slots.

Use Pi's existing personal/project instructions for persistent defaults. Update
with `pi update git:github.com/lorecampa/groove` and remove with
`pi remove git:github.com/lorecampa/groove`. Follow your version's `--help` if
its package commands differ. See [Pi packages](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/packages.md)
and [Pi skills](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/skills.md).

## Hermes

Install the explicit authored skill path, including its references:

```sh
hermes skills install lorecampa/groove/skills/groove
hermes skills list
hermes -s groove
```

The last command starts a session with Groove preloaded. For persistent project
rules, use Hermes' supported context, such as an opted-in `.hermes.md` or
`AGENTS.md`. Hermes uses the first matching context source; check which file
actually loads. Follow the active profile's skill/configuration paths.
Update with `hermes skills update` and remove with
`hermes skills uninstall groove`. Review the installer's scan findings.
See [Hermes skills](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/skills.md).

## OpenCode

Reuse an existing Groove skill in `~/.agents/skills/`, or copy the complete skill
from a stable checkout after inspecting the destination for local changes:

```sh
mkdir -p ~/.config/opencode/skills
cp -R /absolute/path/to/groove/skills/groove ~/.config/opencode/skills/
```

In a fresh session ask OpenCode to load Groove. Configure defaults through its
supported persistent instructions. Updates use the same reviewed copy procedure;
remove only the skill directory you installed.
See [OpenCode skills](https://opencode.ai/docs/skills/).

## Gemini CLI

```sh
gemini skills install https://github.com/lorecampa/groove.git --path skills/groove
gemini skills list --all
```

Review the install prompt. Gemini also discovers `~/.agents/skills/`; reuse an
existing skill there when appropriate. Ask Gemini to activate Groove in a fresh
session. Configure defaults through the intended `GEMINI.md` context. Update by
reviewing and reinstalling from the same source; remove a personal install with
`gemini skills uninstall groove --scope user`.
See [Gemini skills](https://geminicli.com/docs/cli/skills/).

## Other agents and existing skill installations

Install the complete [skills/groove/](../skills/groove/) directory, including
`references/`, in the discovery location supported by your current agent.
Prefer a personal installation that remains available in new worktrees. Preserve
existing customizations and avoid competing copies. Use authenticated Git access
or a local checkout if the repository is private.

## Use Groove by default in a project

Add this instruction through your harness's persistent instruction mechanism:

> Use the Groove skill to manage development workspaces for this project.

For shared team policy, put it in the project's versioned agent instructions
only when that policy is intended for contributors. For personal preferences,
use a private agent configuration or a supported local instruction file. If the
harness cannot express a private project scope, use session preload rather than
silently changing contributor policy.

### Personal workflow files and worktrees

A personal Markdown file can hold your preferred workflow, including PR and
rebase rules. Keep its canonical copy outside reusable worktrees. In Omnigent,
put its contents in a private custom agent's instructions (or a supported bundled
instruction file), then select that agent as the project default. Configure
worker agents too; choosing a default does not make unrelated agents inherit
its instructions. A referenced file in an uploaded agent bundle is a snapshot;
after changing the source file, update the agent configuration.

An optional line in a shared `AGENTS.md`, such as “If `WORKFLOW.local.md` exists,
read it for personal workflow preferences,” is another possible convention.
Ignore that file using `.git/info/exclude` for a preference private to your clone,
or `.gitignore` if the team adopts the convention. A plain Markdown link is not
a portable automatic import: the agent must follow the instruction to read it.
Git ignores prevent commits; they do not prevent an agent from reading a file.

Ignored files are not copied into new Git or Groove worktrees. That convention
therefore needs a local file in each relevant checkout, and it will not survive
slot reuse by itself. Do not rely on it for instructions that must apply before
an agent edits files in a new workspace. Codex's `AGENTS.override.md` can replace
`AGENTS.md` in the same directory, so it is not a universal additive local file.
Claude supports `@path` imports and `CLAUDE.local.md`; their loading rules are
specific to Claude, and local files still need to be available in each worktree.
See [Claude memory](https://code.claude.com/docs/en/memory) and
[Codex instruction discovery](https://developers.openai.com/codex/guides/agents-md).

Initialize a pool separately if the project does not have one:

```sh
groove --pool default pool init --capacity 4
```

Choose the capacity and reuse existing settings. Installing a skill does not
initialize or resize a pool. Assigned workers reuse the reservation supplied by
their coordinator; the skill owns acquisition and handoff guidance.

## Verify

```sh
groove --version
groove --json --help
```

Start a fresh session and check both skill discovery and the instructions loaded
at startup. Ask it to describe the project workflow without opening files or
using tools; then separately ask it to load the skill and confirm the source.
An installed skill alone does not prove a project preference was loaded.
Check for competing skill copies and bundled references. Verify the instructions
from a reserved pool worktree too. Report CLI/skill versions, loaded sources and
whether a restart or updated worker configuration is needed.

## Remove the plugins

For Claude Code:

```sh
claude plugin uninstall groove@groove
claude plugin marketplace remove groove
```

For Codex:

```sh
codex plugin remove groove@groove
codex plugin marketplace remove groove
```

Plugin removal does not release reservations, remove snapshots or change your
personal instruction files. Remove manual skill copies only when no longer needed.

## Develop the plugins locally

Edit the canonical skill in `skills/groove/`, then run
`bash scripts/sync-plugins.sh --write`; `--check` verifies the real-file mirror.
Plugin installers may omit symlinks, so the package includes regular files.
Both marketplace commands accept a stable local checkout path. Export the
catalogs and `plugins/` outside reusable slots before registering a local source.
For Pi, also export `package.json` and `skills/`.
With Omnigent, install Codex plugins against your normal user `CODEX_HOME`, then
verify a fresh session. No Omnigent change is required.

## Distribute the setup

The Git repository hosts the Claude/Codex catalogs through
`.claude-plugin/marketplace.json` and `.agents/plugins/marketplace.json`.
Pi installs the Git package and Hermes installs the GitHub skill path. Public
marketplace listings and npm publication are not required. Private repository
access must work through the installer or an authenticated local checkout.
Public directory submission is an optional, separate publication step.
See [Claude marketplace hosting](https://code.claude.com/docs/en/plugin-marketplaces)
and [OpenAI repository marketplaces](https://developers.openai.com/plugins/build/plugins).

## Source installations

For development, build from an existing checkout:

```sh
bash scripts/install-cli.sh
```

Run the script from this repository, or invoke its absolute path from elsewhere.
It builds the checked-out source, including uncommitted edits, using the pinned
toolchain and lockfile. It replaces an existing CLI even when its package version
is unchanged. It does not fetch or change branches; use a clean checkout at the
desired commit when you need that exact revision.

Run `bash scripts/install-cli.sh --help` for examples and Cargo options, including
`--root` for an isolated installation. Verify with `groove --version`; for an
alternate root, run that root's `bin/groove --version` explicitly.

The equivalent direct command from the repository root is
`cargo install --path crates/groove-cli --locked --force`.

If Git authenticates but Cargo's built-in Git transport fails, use the configured
Git transport for that invocation:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git ssh://git@github.com/lorecampa/groove --locked groove-cli
```

This installs the development revision using existing Git credentials. Keep
credentials out of URLs and install the skill from the matching revision.

## Upgrade an existing installation

Run `groove upgrade --check` to check for a stable CLI release, or `groove upgrade`
to update a verified Cargo crates.io installation in its existing location.
Add `--with-plugins` to update supported existing native agent plugins too.
Use `groove upgrade --help` for previews, exact versions, supported sources,
compatibility boundaries, partial failures and manual-install guidance.

Interactive pool commands can display an informational update notice for newer
minor or major releases at most daily. Patch releases stay available through
explicit checks and upgrades. Checks are best-effort and never fail a pool operation. Disable them with
`groove config set updates.check=false` or `GROOVE_NO_UPDATE_NOTIFIER=1`.
JSON, CI and quiet invocations do not perform automatic checks.
