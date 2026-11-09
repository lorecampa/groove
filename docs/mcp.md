# MCP agent setup

Groove provides a local stdio MCP server built with the official Rust MCP SDK.
It holds leases between tool calls so agents can use their ordinary file and
terminal tools without implementing descriptor passing. Linux and a local
filesystem are required; the server and editing tools must see the same paths.

## Install and connect

From this checkout:

```sh
cargo install --path crates/groove-cli --locked
cargo install --path crates/groove-mcp --locked
```

Configure a **local stdio MCP server** in your agent application, including
Omnigent if it supports custom MCP servers. Set the executable to the absolute
path returned by `command -v groove-mcp` and arguments to:

```text
--repo /absolute/path/to/your/repository
```

For clients that accept the common `mcpServers` JSON configuration shape:

```json
{
  "mcpServers": {
    "groove": {
      "command": "/home/YOUR_USER/.cargo/bin/groove-mcp",
      "args": ["--repo", "/absolute/path/to/your/repository"]
    }
  }
}
```

The client launches and maintains the server process. Configuration interfaces
vary between applications; this is not a command to paste into an agent prompt.
Use one server configuration per repository. The server cannot be redirected to
another repository through tool arguments. It exposes no HTTP port or shell tool.
`--git-timeout-ms` bounds each Git operation (default 30000; maximum 120000).

## Agent instructions

Add this to the session's instructions after connecting the server:

> Use Groove for disposable task workspaces. Initialize the pool with our chosen
> capacity and cleanup policy if needed. Before editing, call groove_acquire with
> the requested base and a unique request_key for this task. Perform every file,
> Git and terminal operation in result.lease.worktree_path. Keep the returned
> slot_id and lease_id. Save results to a task-owned branch or export them outside
> the workspace. Stop all workers, tests, watchers and dev servers before calling
> groove_release with writers_stopped=true. Never edit Groove metadata or delete
> lock/protection files. After a lost response, retry the same request_key/base on
> the same server or inspect groove_leases. If capacity is full, retry later;
> do not create an overflow worktree. Never use a released path until acquired again.

A task-owned branch keeps result commits reachable. An unreferenced detached
commit or a file left in the workspace can be lost during the next cleanup.
Artifact content and durable task metadata belong outside the reusable checkout.

## Tools

| Tool | Arguments | Behavior |
| --- | --- | --- |
| `groove_init` | `capacity`, optional `cleanup` (`pristine`/`warm`), optional `worktree_root` | Initializes this repository's pool; matching repetition is idempotent. |
| `groove_acquire` | `base`, `request_key` | Acquires, verifies and durably protects a workspace; returns `lease` metadata and `reuse_protected: true`. Capacity contention fails immediately. |
| `groove_leases` | none | Lists protected leases, their task keys when known to this server, and `owned_by_this_server`. This is diagnostic, not proof that writers have stopped. |
| `groove_release` | `slot_id`, `lease_id`, `writers_stopped` | Releases a held or abandoned protected lease. Requires explicit confirmation that all writers have stopped and results are saved. |

Successful tool results contain `{"ok":true,"result":...}` in both MCP text and
structured content. Operation failures set MCP `isError: true` and return
`{"ok":false,"error":{"code":...,"message":...,"retryable":...,"details":...}}`.
Unknown tools/invalid MCP requests follow SDK protocol handling. Input decoding
errors can be SDK tool errors with text only. stdout contains MCP messages only.

A request key is 1..128 bytes and scoped to this server process. Retrying the
same key/base returns the same active lease, even if the branch moved. Changing
its base or reusing it after release is rejected. Completed release retries on
this process return the recorded result and never unlock a successor. Request
and release histories are bounded to 4096 entries each. After restarting, inspect
protected leases instead of assuming request-key history survived.

Git operations run on blocking workers and mutations are serialized within one
server. Cancelling a request can cancel its response while the in-flight mutation
finishes: it remains protected and discoverable through `groove_leases`.

## Disconnects, crashes and recovery

MCP clients may close a session while external tools still write into its folder.
For that reason, closing stdin, cancellation, SIGTERM or SIGKILL **does not make an
exposed workspace reusable**. Before exposing a path, the server writes a durable
reuse prohibition under `slots/<slot>.protected.json`. Normal acquisition checks
this under the slot lock before touching contents. A fully protected but unlocked
pool reports `POOL_UNHEALTHY`, with instructions in `failed_slots`.

After restarting the server:

1. Call `groove_leases` to find the abandoned lease.
2. Stop its external writers and preserve its results. The server cannot inspect
   or terminate arbitrary processes started through another application's tools.
3. Call `groove_release` with its slot and lease IDs and `writers_stopped: true`.
   A different live lock owner prevents recovery through this server.
4. Acquire a new lease; Groove then cleans and verifies the checkout.

Protection is a prohibition on cleanup, not a replacement for kernel ownership.
Ordinary unprotected Rust/descriptor leases still end when their last descriptor
closes. Explicit release removes protection before unlocking, and interrupted
release can be retried. Corrupt protection records block reuse until repaired;
never delete them to bypass a still-running writer.

The first protection upgrades the ready manifest to schema 2. New Groove reads
schemas 1 and 2; earlier binaries reject schema 2 instead of ignoring protection.
Stop all older Groove processes before enabling MCP: an already-open old library
handle cannot be upgraded in place. Do not mix versions or manually downgrade the
manifest. Pool configuration remains immutable, and the CLI response stays v1.

This is cooperative workspace management. An agent falsely confirming that its
writers stopped can still cause premature cleanup. Git worktrees share objects,
refs and configuration; they are not security sandboxes.

## Protocol references

The official SDK handles JSON-RPC, MCP initialization/version negotiation, tool
schemas, cancellation and newline-delimited stdio transport. The regression suite
exercises a real client exchange using MCP 2025-11-25 and validates advertised
schemas. See the [MCP transport specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports),
[tool specification](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
and [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk).
