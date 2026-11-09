# Changelog

## Unreleased

Initial Linux development implementation; no public release is recorded here.

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
