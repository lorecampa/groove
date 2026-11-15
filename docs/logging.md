# Diagnostics

Use `groove --help` for terminal verbosity and `groove config --help`
for preference keys, defaults, overrides and configuration repair. Effective
settings and their environment overrides are available through
`groove config show` without a repository.

Stdout carries command results; stderr carries diagnostics, including in JSON mode.
Keep the streams separate when parsing JSON. Terminal delivery is bounded and best effort.

CLI diagnostic events use JSON Lines under `$XDG_STATE_HOME/groove/logs`, falling
back to `~/.local/state/groove/logs`. File severity is independent of terminal
verbosity. Per-invocation files have bounded retention; active files are locked
and skipped, so retention targets are not a strict aggregate quota.

## Privacy and failure boundaries

Diagnostic events omit raw argument vectors, Git output, file contents and
environment dumps. Log destinations must be
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
