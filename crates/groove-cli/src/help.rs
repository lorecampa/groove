//! Detailed command help, kept next to the argument definitions.

pub const OVERVIEW: &str = "\
WORKFLOW:
  groove init --capacity 4
  groove acquire --base HEAD
  # Read result.slot_id, result.lease_id and result.worktree_path.
  # Work in that path, create a unique task branch, and commit results.
  # Stop ALL writers, then run from the original repository:
  groove release --slot SLOT --lease-id UUID --writers-stopped

RESERVATIONS AND RECOVERY:
  Reservations survive command exit, crashes and restart; there is no expiry.
  A full pool never creates overflow worktrees. Inspect with groove leases.
  If an acquire response is lost, identify the original reservation before retrying.
  Never use or release a checkout whose ownership is uncertain.
  Save results before release; the next acquisition discards workspace contents.
  No daemon or MCP setup is required. Each repository has its own fixed pool.

OUTPUT:
  Every response, including help/version/errors, is one JSON object on stdout:
  {api_version:1, request_id:null, ok:true, result:{...}}
  Failures have ok:false and error:{code,message,retryable,details}.
  Check ok and exit status; branch on error.code, never message wording.
  Unknown API major versions must be rejected; additive fields may be ignored.
  Help text is in result.text. To read it with jq:
    groove --help | jq -r '.result.text'

EXIT STATUS:
  0 success; 2 invalid arguments/unsupported configuration; 3 capacity/timeout;
  4 uninitialized/configuration conflict/invalid revision; 5 invalid/stale lease;
  6 recovery failed/unhealthy pool; 7 I/O/Git/protocol failure.
  SIGKILL or an output-stream failure may prevent a response.

REQUIREMENTS:
  Linux, Git 2.40+, local storage. Worktrees share Git objects, refs and config;
  they are not security sandboxes. Pool checkouts reject submodules, sparse
  checkout, worktree-specific config and configured checkout filters (e.g. LFS).
  Use one compatible Groove version per live pool; stop older processes before
  enabling durable reservations (manifest schema 2).

PREFERENCES AND LOGGING:
  User config: $XDG_CONFIG_HOME/groove/config.toml or ~/.config/groove/config.toml.
  Logs: $XDG_STATE_HOME/groove/logs or ~/.local/state/groove/logs.
  XDG paths must be absolute. GROOVE_LOG_ENABLED=true|false and GROOVE_LOG_DIR
  override file settings. Logs are enabled by default; file I/O is best-effort.
  Stderr is quiet by default; -v adds debug output and -vv adds trace detail.
  Use groove config --help for settings, limits and examples.

MORE HELP:
  groove COMMAND --help (or groove help COMMAND) explains each command.
  groove snapshot --help covers local checkpoints, independent of pool setup.";

pub const INIT: &str = "\
EXAMPLES:
  groove init --capacity 4
  groove init --capacity 4 --cleanup warm --worktree-root /absolute/empty/root

Initialization creates fixed slot metadata; checkouts are materialized on acquire.
Repeating identical settings is safe and returns created:false. Capacity, cleanup
policy and root are immutable: different settings fail with CONFIG_CONFLICT.
Matching interrupted initialization resumes. Never delete locks to repair a pool.
Default root: <GIT_COMMON_DIR>/groove/worktrees/. A custom root must be empty,
owned by you, on supported local storage, and outside Git administration.
Relative custom paths resolve from the caller's current directory.

Pristine cleanup deletes untracked AND ignored files. Warm retains ignored files
only in healthy reused checkouts; they remain unverified and may be lost on rebuild.
Both policies discard tracked edits and staging and establish detached HEAD.";

pub const ACQUIRE: &str = "\
EXAMPLES:
  groove acquire
  groove --repo /absolute/repo acquire --base main --wait-ms 5000

Initialize the pool first. Success returns result.slot_id, lease_id, worktree_path,
base_oid and transport metadata. Use only the returned verified checkout. HEAD is
detached: create a unique task branch before committing. Retain the identifiers
outside the disposable workspace. Ordinary acquisition returns durable_protection
with descriptor_count:0; it stays reserved until explicit stopped-writer release.

A zero wait performs one capacity scan. Positive waits bound capacity contention,
not total checkout preparation; Git timeouts are separate. There is no FIFO
fairness guarantee. Failed slots are attempted at most once per acquisition.
On POOL_EXHAUSTED or ACQUIRE_TIMEOUT, wait or inspect leases; never bypass capacity.
If output is lost, inspect leases BEFORE retrying. Request IDs are correlation
metadata, not idempotency keys, and a listed reservation may belong to another task.

ADVANCED DESCRIPTOR TRANSPORT:
  --lease-channel-fd selects an inherited Unix SOCK_SEQPACKET socket. The response
  and exactly one lease descriptor are sent with SCM_RIGHTS. The supervisor must
  retain the descriptor for every writer's lifetime. In this mode stdout alone
  does not confer ownership; closing the last copy permits reuse. Release through
  --lease-fd only after all writers stop. Ordinary shell callers should omit it.";

pub const RELEASE: &str = "\
EXAMPLE:
  groove release --slot wt-0001 --lease-id UUID --writers-stopped

Save results on a task branch, in a snapshot or outside disposable workspaces.
Stop ALL writers, including workers, tests, watchers and dev servers. Groove does
not stop arbitrary external processes. Then supply the original slot and lease ID.
After success, do not use the path until acquired again. Cleanup is deferred to
the next acquisition. Release neither commits nor publishes your results.

A lost release response can be retried with the same identifiers after confirming
stopped writers. Same-generation retries succeed until reacquisition supersedes
the receipt; stale IDs cannot unlock a successor. Live MCP/descriptor owners must
release through their owning server/supervisor; CLI recovery cannot unlock them.
--lease-fd is advanced descriptor-owner release, mutually exclusive with
--writers-stopped. Both modes require that every writer has stopped.";

pub const LEASES: &str = "\
EXAMPLE:
  groove leases

Returns result.leases: durable CLI reservations and MCP protections, including
abandoned ones. This is diagnostic metadata, not proof of task ownership or that
writers have stopped. Unprotected live descriptor leases are not listed.
After an interrupted acquire, identify the task before using or releasing a path.
Preserve work and stop writers before release; age and originating process exit
never authorize cleanup. Never remove lock or protection records manually.";

pub const CONFIG: &str = "\
EXAMPLES:
  groove config show
  groove config path
  groove config set logging.enabled false
  groove config set logging.max_files 50
  groove config set logging.directory '\"/tmp/groove-logs\"'
  groove config unset logging.directory
  groove config edit

USER SETTINGS (TOML):
  logging.enabled        true by default
  logging.directory      absolute path; unset uses the XDG state log directory
  logging.max_file_mib   5 by default
  logging.max_total_mib  100 by default
  logging.max_files      100 by default
  logging.max_age_days   7 by default
  Limits must be positive; per-file size cannot exceed the total target.
  Maximums: 1048576 MiB, 100000 files, 36500 days. TOML does not expand '~'.
  Unknown keys, malformed values and invalid environment overrides are errors.

PRECEDENCE AND STORAGE:
  GROOVE_LOG_ENABLED=true|false and GROOVE_LOG_DIR=/absolute/path override file
  settings, which override defaults. Config lives at
  $XDG_CONFIG_HOME/groove/config.toml or ~/.config/groove/config.toml.
  Logs live at $XDG_STATE_HOME/groove/logs or ~/.local/state/groove/logs.
  Unset, empty or relative XDG paths use the home fallback.
  Logs omit raw arguments, file contents and Git output. Each invocation has its
  own bounded JSONL file. Retention skips active files, so targets are not strict
  global quotas. Logging I/O failures do not fail lease operations.

EDITING:
  No repository is required. Changes take effect on the next invocation.
  Set/unset preserve comments; edit uses VISUAL, then EDITOR, then vi without
  shell evaluation. Editor UI goes to stderr. A temporary copy is validated and
  atomically saved; invalid edits leave the original unchanged. Concurrent edits
  fail with CONFIG_CONFLICT. Config commands do not write persistent logs.
  Use config path/edit to repair broken settings; help always remains available.";

pub const SNAPSHOT: &str = "\
EXAMPLES:
  groove snapshot save 'before refactor' --session task-42
  groove snapshot list
  groove snapshot diff UUID
  groove snapshot restore UUID --to ../recovered-work
  groove snapshot pin UUID

Snapshots save staging and working files without moving HEAD or changing the
real index. No pool, remote or first commit is required. They include deletions,
file modes, symlinks and non-ignored untracked files; ignored files and empty
folders are excluded. Stop writers before save/restore: capture detects common
races but is not an atomic filesystem snapshot.

OUTPUT AND STORAGE:
  Results are under result.snapshot_result; result.elapsed_ms is command timing.
  IDs are full UUIDs returned by save/list. All linked worktrees share snapshots.
  Saved data lives in local refs/groove/snapshots/ and the shared Git object DB;
  settings/locks live in <GIT_COMMON_DIR>/groove-snapshots/. Snapshots survive pool
  release/reuse, but ordinary branch pushes do not back them up. Deleting the
  repository's Git directory loses them. Large binary edits can consume space.

BOUNDARIES:
  Root paths must be UTF-8; individual filenames may contain non-UTF-8 bytes.
  Rejects conflicts, submodules, nested untracked repositories, configured content
  filters, content-converting attributes/automatic CRLF, intent-to-add and manual
  assume-unchanged/skip-worktree flags. Sparse recovery follows the destination's
  sparse settings. No hooks or external diff/textconv commands run. Each Git call
  has its own timeout and a bounded 128 MiB response limit.

Use groove snapshot COMMAND --help for restoration, retention and deletion rules.";

pub const RESTORE: &str = "\
EXAMPLES:
  groove snapshot restore UUID --to ../recovered-work
  groove snapshot restore UUID

Stop writers first. With --to, the destination must not exist. Recovery creates a
detached linked worktree, or an independent repository for an unborn snapshot,
and restores saved staging and working files.

Without --to, HEAD must equal the saved base. A safety snapshot is saved before
modifying files. Captured files absent from the target are removed. Ignored-file
collisions fail; unrelated ignored files remain. Restore is not transactional
against process death. On failure after safety capture, error.details reports
the safety snapshot ID; use it to recover interrupted work.";

pub const RETENTION: &str = "\
EXAMPLES:
  groove snapshot prune --older-than 30d --keep-last 20 --dry-run
  groove snapshot retention --keep-last 20
  groove snapshot retention --disable

Age and count constraints combine with AND semantics. Count is ranked per
workspace and includes pinned entries; pinned snapshots are never pruned.
Prune with no rules uses the configured policy; without a policy it does nothing.
Automatic retention is disabled by default. Configured retention runs after save
and always preserves the just-saved snapshot; cleanup failure is logged without
turning a successful save into failure. Setting a policy replaces the previous
policy. Deleting snapshots removes refs; object-space reclamation may be delayed.
Use snapshot compact for normal shared Git GC, never aggressive immediate prune.";
