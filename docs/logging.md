# Diagnostics and captured output

Use `groove --help` for terminal verbosity and `groove config global --help`
for preference keys, defaults, overrides and configuration repair. Effective
settings and their environment overrides are available through
`groove config global show` without a repository.

Stdout carries command results; stderr carries diagnostics and live worker
output, including in JSON mode. Keep the streams separate when parsing JSON.
`--quiet` suppresses live worker output without changing captured files or the
execution result. Worker streams contain arbitrary bytes, and ordering between
stdout and stderr is unspecified. Terminal delivery is bounded and best effort;
a slow consumer cannot block worker supervision. Captured worker files are the
source for complete output within the configured per-stream limit.

## Two independent log lifetimes

CLI diagnostic events use JSON Lines under `$XDG_STATE_HOME/groove/logs`, falling
back to `~/.local/state/groove/logs`. File severity is independent of terminal
verbosity. Per-invocation files have bounded retention; active files are locked
and skipped, so retention targets are not a strict aggregate quota.

Managed-worker stdout/stderr instead lives under the selected pool's
`task-logs/`, outside reusable checkouts. Diagnostic retention does not remove
these files. Use history/storage commands for their explicit cleanup; see
[execution](execution.md#history-and-explicit-log-cleanup).

## Privacy and failure boundaries

Diagnostic events omit raw argument vectors, Git output, file contents and
environment dumps. Worker output may contain secrets. Log destinations must be
owned by the current user and cannot traverse symlinks or be writable by others;
existing directory permissions are not changed.

Diagnostic file I/O is best effort. An unsafe or unwritable destination, full disk
or cleanup contention does not change a lease operation's result. Writes are not
fsynced, so abrupt termination can lose final events. Invalid configuration is
validated separately before an operational command starts; use configuration
repair commands rather than deleting pool state. Logs are troubleshooting data,
not proof of ownership or an audit journal.

For event fields, bounds, file permissions and retention implementation, consult
[the logging subscriber](../crates/groove-cli/src/logging.rs) and
[preferences](../crates/groove-cli/src/preferences.rs).
