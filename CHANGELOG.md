# Changelog

## Unreleased

Initial Linux/macOS alpha. These notes describe the current product; unpublished
development experiments are omitted.

### Added

- macOS 13+ backends for Apple Silicon and Intel on local APFS: cancellation
  signals, authenticated stop/setup sockets, workspace process inspection and
  strict reflink caches. Descriptor ownership, durable protection and explicit
  orphan recovery retain the same conservative rules on both platforms.
- Fixed-capacity pools of reusable Git worktrees, with detached checkouts, named
  tasks, explicit capacity growth and pristine or warm cleanup policies.
- Descriptor-owned leases and durable reservations that survive process exit.
  Request keys make acquisition retries safe; stale lease IDs cannot release a
  later task. Reservations never expire automatically.
- Managed command execution with literal arguments, captured logs, live stderr
  output, timeouts, cancellation and explicit orphan recovery. Active workers
  retain exclusion after supervisor death.
- Local snapshots that preserve staging state, working-file bytes, deletions,
  executable bits, symlinks and non-ignored untracked files. `finish` pins a
  checkpoint before releasing; receipts make retries safe after slot reuse.
- Explicit project setup, compatible independent artifact caches, named recipes
  and agent command adapters through a reviewed `.groove.toml`. Failed setup
  retains its reservation; retries are explicit.
- Human-readable output and versioned JSON responses, typed errors, actionable
  hints, command help, shell completions and configurable diagnostic logging.
- Slot inspection, execution history and explicit captured-log cleanup with a
  dry-run mode. Inspection never grants ownership or authorizes recovery.
- Public `groove-core` Rust library and `groove-cli` executable package, with
  crates.io release tooling and checksummed GNU/Linux and Darwin binary archives.
  Target-specific manifests/checksums prevent parallel builds from overwriting
  another platform's metadata.

### Correctness and validation

- Snapshot benchmarks record Git step diagnostics, independently verify changed
  saves and restore dirty checkpoints in both modes. Report comparisons reject
  older measurement policies or missing dirty-recovery verification.
- Process-table enumeration failures return an error rather than an empty
  workspace-user list. Native macOS CI covers both CPU architectures, Rust 1.89
  and stable, case-sensitive APFS, strict reflinks and extracted packages/bundles.
- Snapshots preserve physical bytes, executable-bit changes and symlinks despite
  Git autocrlf/filemode/symlinks preferences, including preferences changed after
  capture. Restore validates active conversion attributes before changing files.
  Unused global filter definitions do not prevent ordinary checkpoints.
- Concurrent snapshots and finishes honor the configured Git timeout for shared
  snapshot-lock waits. Failure keeps results and reservations protected.
- Release refuses unsaved changes, and release/finish check for other processes
  using the workspace. These best-effort checks do not replace stopping writers;
  `--force` never bypasses a managed execution.
- Real-process contention, descriptor-inheritance and crash regressions; JSON
  conformance tests; property-based cleanup coverage; extracted-package checks;
  pinned Rust, minimum-Git/current-Git and stable-Rust CI coverage.

### Supported scope

- Linux or macOS 13+, Git 2.40+, local filesystems (APFS on macOS) and cooperative
  processes. Worktrees share
  Git objects, refs and configuration; they are not security sandboxes.
- Pools require full checkouts without submodules, sparse checkout or configured
  content filters. Snapshots reject active content-converting attributes and
  unsupported index states. Ignored files are excluded from checkpoints.
- Snapshots are local, with no remote backup or automatic publishing. Ordinary
  correctness tests run in CI; APFS reflinks run separately in native macOS CI.
  Linux reflink and large-scale benchmarks remain explicit local opt-ins.
