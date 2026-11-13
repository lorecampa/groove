# Changelog

## Unreleased

### Removed

- Remove managed task execution: `exec`, `stop`, `recover`, setup profiles,
  recipes, agent launchers, artifact seeding, `.groove.toml` configuration,
  run history and log cleanup. Run commands directly in a reserved checkout.
  The corresponding core APIs and JSON result types are removed while the
  development protocol remains v1. Legacy execution locks and records still
  block reuse; stop/recover outstanding runs with the previous Groove version
  before upgrading. Existing logs, history and artifacts remain on disk;
  read-only sizes remain available through `pools/show --disk-usage`.

### Added

- Long-running commands show delayed spinners on interactive stderr, or versioned
  JSON liveness events on stderr with `--json`. `--quiet` suppresses progress;
  redirected human output and the single JSON stdout response stay unchanged.
- Named experimental-feature configuration for user preferences,
  with explicit opt-in, retained graduated names, and errors for unknown/removed
  capabilities. The initial registry is empty. User preferences now have a
  published TOML data-model schema.
- `pools --disk-usage` compares logical workspace, cache and log/history sizes.
  `show --disk-usage` adds a storage breakdown to the selected pool's view.
  Slow scans show an interactive spinner.
- Pi can install the portable skill as a Git/local package. Agent setup separates
  installation, session use and project defaults, with Hermes instructions and
  repository-catalog distribution guidance.
- Native Claude Code and Codex plugins package the portable skill. Installation
  guidance covers project defaults through existing harness instructions, private
  workflow files, updates, removal and verification in fresh sessions/worktrees.

### Changed

- Global output rules and exit codes live in root help; command help retains
  its result fields. Trim repeated flag explanations from help and guides.
- Explain contributor Git hooks, name the checks in the security gate, and
  clarify the license choice. Keep vulnerability reporting separate from
  maintainer setup instructions.
- Updating a global preference preserves the existing value's inline comment
  and spacing. Historical configuration goldens protect defaults and file edits.
- The development benchmark lab adds heterogeneous multi-GiB monorepo profiles
  with packed history, commit switching and full-tree recovery checks. Native
  runs resolve relative scratch paths before invoking fixture subprocesses.
  A separate CRLF profile measures checkout-conversion costs and verifies
  restoration of physical line endings that normalize to the same Git blob.
- `pools` hides deleted pools by default in text and JSON output. Use
  `pools --all` to include retained deleted pools; interrupted pools stay visible.
- New pools default to `~/.worktrees/<project>-<repo-id>/<pool>` on Linux and
  macOS. Set the global `worktrees.root` preference to change the parent or use
  `init --worktree-root` for an exact pool path. Existing pools keep their roots.
- Avoid redundant reservation metadata clones, defer unused error summaries,
  and reduce allocations in benchmark hashes and terminal log fields.
- Mark pure query methods and error builders as `must_use`, and avoid per-byte
  allocations when encoding request keys and release checksums.
- Agent onboarding configures persistent harness instructions once, so users
  do not need to request Groove separately for each task. The setup prompt uses
  personal configuration unless shared project policy is requested.
- Document snapshot and release API errors, including partial failure and
  recovery behavior, in Rust API documentation.

### Fixed

- Warm reuse removes previously tracked files when switching to a commit that
  deletes and ignores those paths, while preserving unrelated ignored caches.

## [0.2.1] - 2026-11-13

### Changed

- Explain bounded workspace pools for parallel agents and the runner's
  scheduling, communication and integration responsibilities. Agent onboarding
  includes a concrete prompt for pool reuse, task assignments and preserving
  results before release.
- JSON output now requires `--json` on each command; `GROOVE_OUTPUT` is no
  longer read. Replace exported `GROOVE_OUTPUT=json` settings with explicit
  flags in scripts and agent commands. JSON response schemas are unchanged.
- Installation and completion help include shell setup, Oh My Zsh ordering,
  verification and upgrade instructions for humans and agents.

- The README opens with a compact badge bar linking documentation, licensing,
  Rust requirements and supported platforms, and drops the vague alpha warning.

### Fixed

- Snapshot reads and deletion refuse unreadable, corrupt or inconsistent pins.
  Deletion verifies pin and snapshot references together, preserving snapshots
  when either reference changes before removal.
- Snapshot diffs export applicable binary patches independently of Git color
  and prefix preferences, preserving patch bytes in human and JSON output.
- Setup checks every approved cache root before clearing any, refusing nested
  Git repositories, worktrees and unsafe paths while retaining the reservation.
- Acquisition and destruction preserve recognizable foreign repositories or
  moved worktrees occupying a slot and reject duplicate registration claims.
- Execution observed as cancelled before launch records cancellation without
  starting the worker, retaining logs, history and reservation protection.
- Isolated process tests require evidence that the selected fixture entered;
  a missing or stale exact test selector can no longer silently pass.

## [0.2.0] - 2026-11-12

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

- The agent skill explicitly directs agents to each command's installed help
  for its JSON output contract before parsing responses, including nested commands.
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

- Contributor coding, commit and review conventions, with PR prompts for concrete
  validation evidence and compatibility or recovery effects.
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
