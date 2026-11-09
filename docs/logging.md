# User configuration and logging

Groove keeps stdout exclusively for its JSON response. Debug events are saved as
JSON Lines in `$XDG_STATE_HOME/groove/logs`, falling back to
`~/.local/state/groove/logs`. `-v` / `--verbose` mirrors readable events to stderr;
`-vv` also enables trace detail in both destinations.

```sh
groove acquire
groove -v acquire
groove -vv acquire
GROOVE_LOG_DIR=/tmp/job-logs groove leases
GROOVE_LOG_ENABLED=false groove leases
```

## User preferences

Preferences live in `$XDG_CONFIG_HOME/groove/config.toml`, falling back to
`~/.config/groove/config.toml`. XDG paths must be absolute; unset, empty or relative
XDG variables use the fallback. No config file is required. Logging settings are
user-wide and separate from repository pool configuration.

```toml
[logging]
enabled = true
# directory = "/absolute/custom/log/path"
max_file_mib = 5
max_total_mib = 100
max_files = 100
max_age_days = 7
```

`GROOVE_LOG_ENABLED=true|false` and `GROOVE_LOG_DIR=/absolute/path` override file
settings. File settings override defaults. There are no `--log-dir`,
`--no-log-file` or `--diagnostics` flags. Verbosity remains a per-invocation flag.
Directories must be absolute (no shell expansion of `~` inside TOML). Limits must
be positive; file size cannot exceed the total target. Maximum accepted values
are 1048576 MiB, 100000 files and 36500 days.

```sh
groove config path
groove config show
groove config set logging.enabled false
groove config set logging.max_files 50
groove config set logging.directory '"/tmp/groove-logs"'
groove config unset logging.directory
groove config edit
```

`show` returns effective settings, the config path, and indicators of environment
overrides. `directory: null` means the automatic XDG state location. `set` accepts
TOML values and changes only the named preference; `unset` restores the default.
Comments are preserved. These commands require no Git repository and return JSON
through the normal response envelope (`config_path`, plus `config` and
`environment_overrides` for show, or `updated: true` for a successful write).

`edit` opens a temporary copy using `$VISUAL`, then `$EDITOR`, then `vi`. Editor
arguments and quotes are supported without shell evaluation. Editor UI goes to
stderr, leaving stdout available for the final JSON response. Groove validates
the edited copy, checks for concurrent file changes, and atomically replaces the
config only after success. Invalid edits or editor failures leave the original
unchanged; the temporary copy is discarded. Concurrent Groove edits fail with
`CONFIG_CONFLICT` rather than silently overwriting one another. Changes take
effect on the next invocation.

Unknown keys, invalid TOML, invalid overrides and out-of-range limits fail before
an operational command runs. `config path` and `config edit` remain available to
repair broken settings. Config commands do not create persistent logs, so they
can repair the logging destination itself; `-v` still works. Help remains
available even if settings are broken.

## Log format

Each invocation uses `groove-<invocation UUID>.jsonl`. Each line is an independent
JSON object, for example:

```json
{"log_version":1,"timestamp":"2026-11-10T14:32:18.123Z","level":"INFO","event":"command.completed","message":"Command completed","invocation_id":"8349f17c-7264-4ada-b8f0-f024c7cb1dc0","request_id":"task-42","command":"acquire","pid":1234,"target":"groove","fields":{"elapsed_ms":34}}
```

`timestamp` is UTC RFC 3339 with millisecond precision and a `Z` suffix. File
and stderr output use the same captured event time. Numeric and boolean fields retain
their JSON types. `message` is readable text; `event` identifies the event without
matching prose. Invocation, request, command and process context appears on every
record. Request IDs are optional and separate from invocation UUIDs. `log_version`
versions this diagnostic format independently of the CLI response protocol.

Verbose stderr renders the same event as readable text:

```text
2026-11-10T14:32:18.123Z INFO Command completed elapsed_ms=34
```

It announces the log path and escapes control characters in messages and fields.
Events include command stages, Git completion/exit status, slot protection,
reservation and release identifiers. Trace adds Git starts and busy-slot scans.
Core code emits events without configuring global logging. The MCP executable
does not configure this CLI logging facility.

Logs omit raw argument vectors, Git output, file contents and environment dumps.
Event strings are bounded to 2048 UTF-8 bytes, and request IDs to 2048 characters.
New log files use mode 0600 and new directories mode 0700. Existing directory
permissions are unchanged. Logs are troubleshooting data, not an ownership
journal or audit log.

## Retention and failures

Files stop accepting complete records at `max_file_mib`; stderr and command
execution continue. At startup and normal exit, oldest inactive files are removed
to meet `max_files`, `max_total_mib` and `max_age_days`. No daemon is required.

Active logs are locked and skipped, so active invocations can add at most
`max_file_mib` each above retention targets. This is not a strict global quota.
The OS releases locks after crashes; the next invocation can prune those files.
Cleanup only considers regular `groove-*.jsonl` files and skips symlinks.

File I/O is best-effort: unwritable destinations, full disks, cleanup contention
or write failures never change a lease operation's result. Verbose startup reports
when a file cannot be opened; default stderr remains empty. Writes are synchronous
but not fsynced, so abrupt termination can lose the final event. Malformed config
is validated separately, before any lease operation starts.
