# Agent setup: CLI first

Install Groove once per machine, teach your agent the workflow once, then use
it in any local repository. A project needs its own pool, not its own binary
installation or MCP registration. Linux, a local filesystem and Git 2.40+ are
required; building from source needs Rust 1.89+.

## Install

```sh
cargo install --git https://github.com/lorecampa/groove --locked groove-cli
groove --version
```

Ensure `~/.cargo/bin` is on `PATH`. This builds the current development source;
there is no published binary installer or crates.io release assumed here. For a
reproducible installation, add `--rev COMMIT` with a reviewed full commit ID.
Private repository access requires your existing Git authentication.

To install from a checkout instead:

```sh
git clone https://github.com/lorecampa/groove.git
cd groove
cargo install --path crates/groove-cli --locked
```

## Install the agent skill

The portable skill is [skills/groove/SKILL.md](../skills/groove/SKILL.md). From a
Groove source checkout, copy it to your agent’s supported user skill directory.
For Codex:

```sh
mkdir -p ~/.codex/skills/groove
cp skills/groove/SKILL.md ~/.codex/skills/groove/SKILL.md
```

If your environment uses a custom Codex home, use its `skills/groove` directory.
For other clients, use their documented skill installation mechanism; do not
assume every agent discovers the same directory. Start a new session if needed
for discovery. No MCP server configuration is required.

You can also give the skill contents to an agent as instructions without
installing it. A short task prompt is: “Use Groove to work on this task in a
pooled worktree. Save the results on a task branch before releasing it.”

## Task workflow

Read `groove --help` and the relevant command's `--help` before use. The command
help includes examples, defaults, ownership and recovery rules. Help is returned
as JSON `result.text`; `groove --help | jq -r '.result.text'` renders it with jq.

From the repository to manage:

```sh
groove init --capacity 4
groove acquire --base HEAD
```

`--repo` defaults to the current directory; Git discovery handles repository
subdirectories and linked worktrees. The pool is shared through the common Git
directory. Use an absolute `--repo` when calling from somewhere else. No global
“current project” is stored, so independent agent sessions can use different
repositories concurrently.

Read the successful JSON `result`, retaining its `slot_id`, `lease_id` and
`worktree_path`. Each response has `api_version: 1` and `ok`; failures have `error`
and a nonzero exit code. A normal acquisition returns
`lease_transport: "durable_protection"` and `descriptor_count: 0`. That reservation
survives the command’s exit and machine restart. It does not need a background
server, retained shell session or descriptor passing.

Work only in the returned checkout. HEAD starts detached: create a unique
task-owned branch before committing results. Export untracked artifacts and
patches outside all disposable workspaces if they must survive. Stop every writer
before explicitly releasing:

```sh
groove --repo /absolute/path/to/repository release --slot SLOT --lease-id UUID --writers-stopped
```

Replace SLOT and UUID with the acquired identifiers. Cleanup is deferred until
next acquisition. Do not access a released path until it is acquired again.
Never release a reservation merely because its originating CLI command exited.

The default `pristine` cleanup removes ignored and untracked contents. Pool
configuration is immutable; repeat initialization only with the same settings.
Capacity conflicts or protected workspaces are reasons to wait or investigate,
not to create overflow worktrees or delete metadata.

## Recovery and retries

```sh
groove leases
```

This lists durable reservations, including abandoned MCP reservations. It does
not prove that writers have stopped or that a reservation belongs to this task.
After losing an acquire response, inspect the list before retrying; `--request-id`
is correlation metadata, not an idempotency key. When ownership is ambiguous,
identify the task before using or releasing its checkout.

Preserve results and stop all external writers before releasing an interrupted
task. A live MCP lock owner must release through that server; CLI release will
reject it. A failed or lost release can be repeated with the same IDs after
confirming stopped writers. Same-generation retries succeed until a new
acquisition supersedes the receipt; stale releases cannot unlock a successor.
No reservation expires with time. If stopping writers or saving results fails,
leave the workspace protected and report what needs recovery.

Protections use manifest schema 2. Stop all older Groove processes before using
durable CLI reservations; do not mix binary versions against one live pool.
Descriptor transport remains opt-in for custom supervisors; see
[protocol](protocol.md). [MCP](mcp.md) remains available when your client prefers
structured tools or lacks terminal access.
