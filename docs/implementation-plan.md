# Groove implementation plan and delivery record

Status: v1 development implementation present. Library, CLI, examples, schemas and
local regression suites are implemented. This is not a declaration of a stable
public release; release validation limits are listed below and in [testing](testing.md).

## Deliverable and scope

The workspace is entirely Rust: `groove-core` provides `Pool` and `Lease`;
`groove-cli` ships the `groove` binary with `init`, `acquire`, and `release`.
CLI callers are descriptor-aware supervisors; Rust applications normally use the
library directly. The reference CLI client is Rust too.

Supported: Linux, local filesystems, fixed capacity, detached worktrees, SHA-1 and
SHA-256, cleanup before every acquisition, crash recovery, quarantine, optional
external checkout roots and explicit pristine/warm policy.

Deferred: `run`, `status`, process enumeration, resizing, Windows, network/FUSE
filesystems, submodules, sparse checkout, configured filters/LFS, worktree-specific
configuration, fetching, automatic task-result publication, a daemon and a TUI.
Callers preserve results and stop writers before release. No crash-critical
behavior depends on Rust destructors running after SIGKILL.

## Repository layout

```text
Cargo.toml / Cargo.lock
crates/
  groove-core/
    src/
      lib.rs                  Public facade
      error.rs / types.rs     Typed errors, options and results
      repository.rs           Discovery, filesystem and path validation
      state.rs                Versioned durable metadata; opt-in test checkpoints
      locking.rs              Permanent lock identity and descriptor ownership
      pool.rs                 Initialization, allocation and bounded waits
      lease.rs                Lease lifetime, explicit release and worker helper
      git.rs                  Controlled Git subprocesses and execution timeouts
      recovery.rs             Normalize, verify, rebuild or quarantine
      transport.rs            Shared Unix descriptor wire transport
    examples/agent.rs         Direct library integration
    tests/
      support/mod.rs          Temporary repositories and direct-child RAII
      initialization.rs / ownership.rs / recovery.rs
      concurrency.rs / compatibility.rs / stress.rs
  groove-cli/
    src/
      main.rs / args.rs       Entrypoint and argument parsing
      protocol.rs             JSON envelope and error/exit translation
      transport.rs            Validate/duplicate inherited descriptors
    examples/client.rs        Complete descriptor-aware CLI caller
    tests/
      protocol.rs / handoff.rs / crashes.rs / git_child.rs
schemas/
  response-v1.schema.json / config-v1.schema.json / slot-v1.schema.json
docs/
  architecture.md / implementation-plan.md / protocol.md / operations.md
  testing.md / plan-review.md
.github/workflows/ci.yml
```

Two crates keep the public reuse boundary small. Transport lives in the core
crate so the CLI and reference client exercise the same syscall implementation.
The core never prints, configures global logging or requires an async runtime.

## 1. Workspace and ownership foundations

- [x] Workspace, committed lockfile, typed options/results/errors and real-Git fixtures.
- [x] Rust 1.89 minimum selected from dependency requirements and exercised locally;
  Git 2.40 minimum, current tools and minimum tools included in CI.
- [x] Stable slot locks with validated device/inode identity, no replacement or unlink.
- [x] Nonblocking contention, bounded waits and no ownership based on metadata/PIDs.
- [x] Duplicate-descriptor and independent-open release checks with real kernel locks.

Acceptance: one owner per slot; the last descriptor closing frees it; retained
copies keep it busy. Stale UUIDs cannot release a successor.

## 2. End-to-end library acquisition

- [x] Ordinary, linked-worktree and bare discovery through the common Git directory.
- [x] Idempotent fixed-capacity init; conflicting options fail; durable initialization
  intent and ready manifest allow matching interrupted initialization to resume.
- [x] Pool-wide cleanup policy and fixed owned checkout root in the manifest.
- [x] Detached worktree creation, normalization and verification before `Lease` return.
- [x] Concurrent-process acquisition tests with more callers than capacity.

A shell script can combine flock and Git worktrees, but must separately implement
lease transfer, verified baselines, bounded capacity, durable initialization,
recovery and quarantine. Those are Groove's library contracts; retained build
caches are an optional policy, not the sole reason for a pool.

## 3. Recovery and quarantine

- [x] Reconstruct the index to discard flags/stat assumptions; forced detached
  checkout and policy-specific untracked cleanup; verify HEAD/index/status/registration.
- [x] Detect interrupted operations and invalid registrations; rebuild only the
  owned slot under slot and administrative locks; no repository-wide prune.
- [x] Preserve permanent locks, unrelated trees and shared refs during recovery.
- [x] Inherit both protective descriptors into registration-mutating Git children.
- [x] Bound repairs per acquisition; quarantine failures and continue to usable slots.
- [x] Deterministic crash barriers around writes, normalization, rebuilding and release.

Warm mode verifies tracked baseline and absence of nonignored untracked files;
pristine additionally requires ignored artifacts to be absent. Neither cached
status nor a prior success record authorizes skipping normalization.

## 4. Versioned protocol and CLI

- [x] JSON success/error envelope, optional request ID, stable codes and exit categories.
- [x] JSON help/version/usage errors and optional stderr JSON Lines diagnostics.
- [x] Published response/config/slot schemas and real-result conformance tests.
- [x] Connected seqpacket validation, bounded atomic JSON-plus-one-FD handoff,
  close-on-exec reception, malformed/truncated descriptor rejection and cleanup.
- [x] Explicit release checks descriptor identity and current generation before metadata
  changes; success unlocks, failures before unlock retain caller ownership.
- [x] Rust reference client demonstrating the entire CLI ownership lifecycle.

The received socket record is authoritative. Stdout is a duplicate receipt; its
failure cannot revoke ownership already transferred. Closing the final descriptor
is valid implicit release, while explicit unlock affects every duplicated copy.
Release metadata is advisory and not required for eventual reclamation.

## 5. Rust integration and compatibility

- [x] Read-only lease information, explicit release with active-handle retention on
  failure, and normal Drop closing only.
- [x] Participating worker helper and direct-library example; no language adapter.
- [x] Early-return/unwinding, SIGKILL, surviving worker and lost-release-response tests.
- [x] Cargo and npm smoke tests in the default `.git/groove/worktrees` location.
- [x] Spaces/Unicode in root paths, arbitrary filename bytes and SHA-256 coverage.
- [x] Documentation for result preservation, residual warm state and background daemons.

## 6. End-to-end gate and public release preparation

- [x] Real binary, schema, concurrency, deterministic crash and seeded contamination suites.
- [x] CI matrix for minimum/current Rust and Git, examples and packaged CLI exercise.
- [x] Scheduled extended seeded workloads; no timing sleeps to establish test ordering.
- [x] Installation, protocol, operations, testing and contribution/security documents.
- [x] Dual license files and dependency-update configuration.

Before declaring a public stable release, review protocol compatibility and
release artifacts, run the configured CI in the remote repository, and record
results on intended production filesystems. Power-loss testing, minimum-kernel
VM coverage and broader tool compatibility are separate validation tasks, not
claims established by SIGKILL tests. No unattended publishing workflow is enabled.

## Crash matrix

| Failure or condition | Required observation |
| --- | --- |
| Agent SIGKILL with sole descriptor | Next client acquires and sanitizes |
| Surviving child retains descriptor | Slot stays busy until the child closes |
| Background descendant retains descriptor | Genuine busy slot; metadata alone cannot free it; operations guide explains diagnosis |
| Groove killed during a Git mutation | Mutating child retains slot and administrative locks |
| Git execution timeout | Dedicated mutation process group is terminated/reaped before recovery |
| Death before descriptor handoff | No client authorized; slot can be reclaimed |
| Death after descriptor delivery | Receiver retains ownership |
| Receiver abandons queued handoff | Closing the channel discards the queued descriptor |
| Release response lost | Caller stops writing; stale retry cannot unlock successor |
| Staged edits and ignored artifacts | Pristine removes both; warm retains ignored state only |
| Interrupted rebase or damaged index | Normalize/rebuild or quarantine; never dirty success |
| Partial initialization or metadata write | Resume/revalidate without lock replacement |
| All slots busy | Structured exhaustion/timeout; no extra checkout |
| Broken slot plus usable slot | Acquisition can succeed on usable slot |
| Repeated repair failure | One attempt per slot/request, explicit unhealthy details |

## Review decisions

- CLI target is explicit; a Rust client is included, preserving the all-Rust scope.
- Warm mode is opt-in and visible in the acquisition result; pristine stays default.
- Checkout replaces the redundant explicit-source restore; index reconstruction
  additionally removes flags that could hide residual modifications.
- External roots are explicit and dedicated; default location retains locality.
- A minimal ownership/library flow precedes protocol refinement; descriptor lifetime
  is covered by early component tests rather than deferred to release testing.
- `/proc` enumeration is deferred: incomplete observations must never determine ownership.
- Native Git worktrees use `--lock` against pruning; flock remains the lease authority.
- Nonlocal/FUSE mounts are conservatively rejected; `statfs` is not a durability proof.
- `request_id` is optional in both success and failure envelopes. Branches survive cleanup.

## Required checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --locked
cargo build --workspace --examples --locked
```

See [testing](testing.md) for exact suite mapping and reproducible stress commands.

## MCP follow-up

The repository now also includes groove-mcp, an official-SDK stdio adapter scoped
by --repo. It exposes init/acquire/leases/release tools, holds descriptors between
calls, and protects exposed slots from cleanup after server death. Manifest schema
2 prevents older newly opened clients from ignoring that protection. Real-process
MCP tests cover cancellation, retries, EOF, SIGKILL and surviving external writers.
See mcp.md for configuration and explicit recovery. This extends the original
CLI/library implementation scope without adding automatic task execution.
