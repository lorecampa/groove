# Changelog

## Unreleased

- Concise installation and agent workflow README; expanded command help covers
  reservations, recovery, snapshots, configuration and JSON/exit contracts.
- Contributor `AGENTS.md`, pinned Rust 1.89.0 tooling and MSRV-aware dependency
  resolution. CLI help coverage requires descriptions for every command/argument.

Initial Linux development implementation; no public release is recorded here.

- Replace `--diagnostics` with `-v`/`--verbose` and `-vv`. Add bounded per-invocation
  typed, versioned JSONL logs and readable stderr with matching UTC RFC 3339 timestamps; preserve JSON-only stdout and
  keep logging I/O failures independent of lease outcomes.
- User TOML preferences with environment overrides and `config path/show/set/unset/edit`;
  validated, atomic edits preserve comments and reject concurrent changes.

- CLI-first agent workflow: default `acquire` persists a durable reservation across
  shell calls, `leases` lists protected workspaces, and `release --writers-stopped`
  requires explicit confirmation before reuse. Repository defaults to the current
  directory; acquisition base defaults to HEAD. No daemon or MCP setup is required.
- Optional `--lease-channel-fd` / `--lease-fd` transport retains descriptor-based
  supervisor compatibility. JSON adds `durable_protection` with zero descriptors.
- `Lease::into_protected` exports a durable reservation. Protected release supports
  same-generation retries after lost responses without unlocking newer work.
- Portable agent skill, source-linked setup prompt and CLI-first documentation;
  MCP remains optional. Regression tests cover independent/concurrent shell calls,
  SIGKILL, lost output, saved branches, cleanup, confirmation and stale releases.

- Rust pool and lease API, plus JSON CLI initialization, acquisition and release.
- Fixed-capacity ownership using file descriptors, crash recovery and verified cleanup.
- Stream large tree validation without imposing the diagnostic output limit;
  detect submodules even beyond the retained diagnostic buffer.
- Retry partially recorded releases for the same lease generation while rejecting
  stale generations and descriptors that conflict with an active owner.
- Regression coverage for large trees and release retries after process crashes.
- Public Rust API documentation and compile-checked usage example.
- Breaking development Rust API change: `ErrorCode` replaces string error codes;
  validated `SlotId`, `LeaseId` and `CommitOid` replace aliases/raw strings.
  Release requires typed IDs. Explicit parsing/conversion is required at callers.
- Typed HEAD mode, lease transport, cleanup timing and internal slot phases;
  errors retain underlying I/O and parsing sources for Rust callers.
- Valid v1 JSON encodings stay unchanged. Slot identifiers now consistently
  enforce `wt-0001` through `wt-4096`, including CLI parsing and JSON schemas.
- Local stdio MCP adapter using the official Rust SDK, with initialization,
  acquisition, lease listing and explicit release tools, scoped to one repository.
- Durable reuse protection for MCP workspaces across cancellation, EOF and crashes;
  explicit stopped-writer recovery and manifest schema 2 to reject older binaries.
- MCP end-to-end tests for negotiation, schemas, errors, retries, cancellation,
  separate sessions and surviving external writers.

### Local snapshots

- Added `groove snapshot` save/list/show/diff/restore, pin/unpin, delete/prune,
  opt-in automatic retention and normal Git compaction.
- Checkpoints preserve staged and working-file trees without moving HEAD or
  modifying the real index, including repositories with no commits.
- Added recovery, process-crash and CLI tests plus a disposable real-repository
  and 100k/1M-file benchmark runner. See `docs/snapshots.md`.

### Reproducible validation

- Added deterministic large snapshot regressions, shared versioned workload
  fixtures, a pinned container environment, provenance reports and scale CI.
- Added installable pre-commit/pre-push hooks and fail-closed hook regression tests.
- Isolated protection-test process fixtures to prevent unrelated forks from
  retaining transient descriptor owners during default parallel scheduling.
