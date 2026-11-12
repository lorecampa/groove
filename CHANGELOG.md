# Changelog

## Unreleased

### Changed

- Consolidated documentation around installed CLI help, the portable skill,
  executable validation gates and benchmark profiles. Removed duplicate agent
  instructions, dated launch/evaluation reports and copied reference inventories.
  Configuration help now derives preference defaults from the implementation.
- Every command's `--help` now includes its JSON output contract. Help derives
  result fields/types from the bundled conformance schema and root help derives
  error exit codes from the CLI mapping.
  Removed the duplicated Markdown protocol reference; repository access is no
  longer needed to discover the JSON interface.
- Replace `groove finish` with `groove release --save`. Saving still pins a
  local snapshot before release, retains the reservation on capture failure,
  and supports durable retries by lease ID. Plain `release` keeps its existing
  behavior. `--description` and `--max-file-bytes` require `--save`; the default
  description is now `workspace saved`. Update scripts to use the new syntax;
  JSON success results retain their existing shapes.

### Fixed

- The agent skill explicitly directs agents to each command's installed JSON
  help contract before parsing responses, including nested commands.
- Agent installation instructions use the latest published CLI and let the
  current agent choose its skill installation location. The README setup prompt
  no longer requires users to choose an installation scope or pin an alpha.

- Diagnostic logging rejects unsafe directories and nonregular, multiply linked
  or writable cleanup locks without blocking commands or deleting existing logs.
  A FIFO at `.cleanup.lock` can no longer stall a CLI response.
- Execution-lock contention from log/history maintenance now reports retryable
  `INVALID_LEASE` without claiming a worker is active. Finish retries preserve
  saved snapshots and staged/working files; no protection is bypassed.
- Recovery hints keep repository/pool selection, and pool destruction explains
  exporting residual ignored artifacts instead of finishing released tasks.
- Setup validation identifies invalid/duplicate step names and invalid argv in
  their profile. Failed worker spawns include executable, checkout, lease and
  history run identifiers while retaining their I/O source.
- Unborn snapshot recovery keeps the original initial branch name. Snapshot and
  execution guidance now explicitly describes special-file exclusions and
  removal of all inherited `GIT_*`/`GROOVE_*` variables.

### Added

- Local advisory/source/license/review gates, an opt-in weekly security workflow,
  and bounded identifier/project parser fuzzing with an isolated lockfile.
  Exact-version Cargo Vet bootstrap exemptions explicitly record the review backlog.
- Patched contributor/release Rust toolchain with the Rust 1.89 minimum retained;
  unnecessary tracing attribute/formatting/ANSI dependency features are disabled.
- Target-specific CycloneDX dependency inventories in binary bundles and checksum
  assets, embedded binary dependency metadata for hosted native builds, and
  build/SBOM attestations when supported by the repository's GitHub plan.
- Optional `--limit` and `--offset` windows for history and snapshot listings;
  omitted limits preserve complete output and the existing response shapes.
- CLI version output includes its build source revision, taken from the source
  checkout, Cargo archive metadata or `GROOVE_BUILD_REVISION`; unknown provenance
  is explicit when source metadata is unavailable.

## [0.1.0] - 2026-11-12

Initial Linux/macOS alpha. These notes describe the current product; unpublished
development experiments are omitted.

### Added

- Hosted CI runs only on deliberate manual dispatch or release invocation while
  GitHub compute is unavailable; routine pushes, pull requests and schedules do
  not consume runners. Required local checks remain in place.

- Agent-readable CLI/skill installation, scope, update and removal guidance;
  a prompt-based README entry point without a custom integration installer.
  The portable skill distinguishes coordinators from assigned workers and bundles
  focused recovery/project references, with observable evaluation scenarios.
- GitHub releases independent of optional crates.io publication, draft-only
  candidate/manual rehearsals, byte-verified uploads and retries that preserve
  existing assets. Release notes include tag-pinned Cargo installation and platform
  downloads. A private-to-public launch checklist documents external prerequisites.

- Package verification builds extracted sources with fresh compiler outputs;
  earlier archives at the same version cannot supply stale library artifacts.

- Versioned `workspace-multi-pool` and `workspace-deletion` benchmark profiles,
  covering cross-pool throughput, lazy/populated/dirty/external-cache removal,
  retained results/locks and unaffected sibling reservations. Reports validate
  exact operation counts and correctness evidence before aggregating measurements.
- Named pools selected with `--pool NAME`, a `pools` catalog, independent capacity,
  cleanup, checkout roots, task labels, request keys, caches and retained history.
  The default pool keeps its layout and keys. Repository-wide coordination rejects
  overlapping checkout roots; deletion requires an explicit selection when several
  pools exist. Snapshots/branches stay shared and managed commands get `GROOVE_POOL`.
- Explicit idle-pool deletion with `destroy`, read-only previews, narrowly scoped
  `--discard-changes`, generation preconditions and crash-safe retries. Permanent
  locks, request tombstones, finish receipts, execution history/logs, branches and
  snapshots survive. Reinitialization can choose new settings while stale pool
  and snapshot handles remain invalid; history cleanup supports retired slots.
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

- Capacity growth reuses retired slots after deletion and smaller reinitialization,
  preserving permanent locks, request keys, finish receipts and history. Growth
  uses bounded descriptors and still rejects live owners, protection and unsafe
  unpublished state; crash retries preserve active reservations.
- Command help describes named-pool checkout roots and reinitialization. Agent
  guidance carries pool selection and the original lease ID through retries;
  user guides defer command options and defaults to CLI help.
- Reftable repositories, `worktree.useRelativePaths` and relative registrations
  written by `git worktree repair` work; a deleted slot with a relative
  registration is rebuilt instead of blocking every acquisition.
- `--base HEAD` (the default) resolves where the command runs, so a linked
  worktree's caller gets its own commit rather than the main checkout's.
- Release and finish detach HEAD, so branches created in a slot can be checked
  out or deleted elsewhere immediately.
- Task labels are unique among active reservations (a reuse names the existing
  lease); labels and request keys reject control characters, and human output
  escapes control characters in stored text.
- `init` rejects a worktree root inside any checkout, where `git add -A` or
  `git clean` there would capture or delete pooled work.
- A reserved checkout deleted outside Groove reports `RECOVERY_FAILED` with its
  path and an unhealthy slot; `release --force` discards it. A moved repository,
  a deleted working directory and an unborn HEAD get specific errors.
- Per-slot failure reasons include Git's own diagnostic line. Failed stdout
  writes other than a closed pipe are explained on stderr (exit 7 as before).
- Git maintenance children keep their leader unreaped until the process group is
  signalled, so a group kill can never reach a process that reused its ID.

- Development benchmarks use complete versioned profiles for incremental saves,
  recovery, path scaling, binary changes, workspace reuse, setup caching and
  concurrent workers. Profiles own all inputs, operations and sampling; `profiles`,
  `check`, `benchmark` and `inspect` expose one self-contained report. Paired
  comparisons use independent blocks and independent Git oracles. Pinned
  containers compile each CLI offline and validate runtime isolation;
  native host measurements remain explicitly exploratory. Historical benchmark
  results and unpublished performance claims have been removed.
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
