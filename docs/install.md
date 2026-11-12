# Install Groove and its agent skill

This guide is intended for people and coding agents. Install the CLI once, then
install the complete portable skill for the requested harness and scope. No
Groove integration command, daemon, MCP server or native agent plugin is needed.
Installing instructions does not initialize pools or run repository setup.

## Choose a source

Requires Git 2.40+, Rust 1.89+, and Linux or macOS 13+ with supported local storage
(APFS on macOS). See [operations](operations.md) for repository limitations.
These are compatibility minimums. Use patched Rust/Cargo and Git releases;
the contributor/release toolchain is pinned separately in `rust-toolchain.toml`.

Install the alpha from crates.io; Cargo compiles the executable locally:

```sh
cargo install --locked groove-cli
groove --version
```

To select the first alpha exactly:

```sh
cargo install --locked --version '=0.1.0' groove-cli
```

For a development revision, or to install from a reviewed Git tag, use
authenticated Git access while the repository remains private:

```sh
cargo install --git https://github.com/lorecampa/groove --tag v0.1.0 --locked groove-cli
```

Omit `--tag` only when intentionally installing the development revision.
Use the same source revision for CLI and skill. Cargo is the only supported
package-manager route for now. Native GitHub binary archives are deferred for
the first alpha; their pipeline requires available hosted compute. See
[binary downloads](releasing.md#binary-downloads) for the supported targets.

Crates.io installation does not require access to the GitHub repository.
For the skill and repository guides while they remain private, use existing
authenticated Git access or a local checkout. Do not put credentials into prompts,
URLs or committed files. An unauthenticated agent cannot fetch a private guide.

If ordinary Git authenticates but Cargo's built-in Git transport does not, use
Git's configured transport for that invocation:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git ssh://git@github.com/lorecampa/groove --locked groove-cli
```

Use existing SSH/credential setup; do not add tokens to the URL or change global
authentication configuration. This addresses Cargo transport compatibility, not
missing permission to access the repository.

## Install the skill for your harness

Obtain the source with authenticated Git, at the selected CLI tag/revision, or
use `skills/groove/` from the matching binary archive. Copy the **entire folder**,
including `references/`, not just `SKILL.md`.

| Harness | User scope | Project scope |
| --- | --- | --- |
| Codex | `~/.agents/skills/groove/` | `.agents/skills/groove/` |
| Claude Code | `~/.claude/skills/groove/` | `.claude/skills/groove/` |
| Other Agent Skills harnesses | Use the harness's documented discovery directory | Use the harness's documented discovery directory |

These locations follow [Codex skill discovery](https://learn.chatgpt.com/docs/build-skills#where-codex-loads-local-skills)
and [Claude Code skill discovery](https://code.claude.com/docs/en/skills#choose-where-skills-load).
Respect managed configuration and any documented harness overrides. Skill format
compatibility is not a claim that every harness supports the same hooks/features.

User scope is a convenient default for agents launched across multiple Groove
checkouts. For team-wide project scope, install in the target repository and
review/commit the skill files so acquisitions from that commit include them.
Uncommitted files in the original checkout do not automatically reach slots.
Do not commit anything unless that is part of the requested installation.

For example, **from the matching Groove source directory**, install for Codex at
user scope using Bash:

```sh
skill_destination="$HOME/.agents/skills/groove"
if [[ -e "$skill_destination" || -L "$skill_destination" ]]; then
  printf '%s\n' 'Groove skill already exists; compare it before updating.' >&2
else
  mkdir -p "$(dirname -- "$skill_destination")"
  cp -R skills/groove "$skill_destination"
  diff -r skills/groove "$skill_destination"
fi
```

For Claude Code, use `$HOME/.claude/skills/groove`. For project scope, resolve the
target repository root first and use the appropriate relative directory there.
Avoid installing duplicate copies at several scopes for the same harness.

As an optional alternative, the existing
[Skills CLI](https://github.com/vercel-labs/skills) can discover and install the
skill across harnesses (requires Node/npm and access to the source):

```sh
npx skills add lorecampa/groove --skill groove --agent claude-code --agent codex
```

That defaults to project scope; add `--global` for user scope. This unpinned
example follows the repository's default branch, so use the documented ref
syntax of your installer when matching a particular Groove release. Use that
installer's check/update/remove commands for installations it manages.

## Verify and report

1. Run `groove --version` and `groove --json --help`; the latter must return a
   successful versioned JSON response with help in `result.text`.
2. Compare the installed skill folder with the selected source, including both
   reference files. Record the source tag/commit and destination in the setup
   report so updates can be matched to the CLI version.
3. Restart/reload the harness as its documentation requires, then check that
   `groove` appears in its available skills. Do not claim discovery is verified
   merely because the files exist. If this requires a new session, report that
   remaining step.
4. In a disposable Git repository with an initial commit, ask the agent to use
   Groove for a small change and preserve its result. Confirm the snapshot can
   be restored. See [skill evaluations](skill-evaluations.md) for role/failure
   scenarios; never run cleanup experiments against a developer's repository.

Report the CLI version, skill source, scope/destination, checks performed and
anything still needed. Do not initialize the user's actual project, change its
policy, enable hooks or launch workers as a side effect of installation.

## Updates and removal

For manually copied skills, compare the existing folder with a fresh copy from
the chosen release. Preserve customizations, review changes and replace only the
Groove skill files; never recursively overwrite a harness configuration directory.
Update the CLI and skill together, and use one Groove version across a
repository's pools. Recheck discovery after updating.

To remove a manual installation, inspect the exact `groove` skill directory and
remove only the files installed for Groove, preserving user additions. For a
symlink, remove the link itself without following it. Removing the skill does
not uninstall the CLI, stop workers or release reservations. Use the same
installer for removal if it originally installed the skill.
