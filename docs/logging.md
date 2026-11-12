# User configuration and logging

Groove reserves stdout for command results: human-readable text by default, or
exactly one versioned response with `--json` / `GROOVE_OUTPUT=json`. Stderr carries
human diagnostics and live managed-worker output, including in JSON mode. Do not
merge stderr into stdout when parsing JSON.

| Terminal option | Groove diagnostics | Live worker output |
| --- | --- | --- |
| Default | Warnings and errors | Shown |
| `-v`, `--verbose` | Also info | Shown |
| `-vv` | Also debug | Shown |
| `-vvv` (or more) | Also trace | Shown |
| `-q`, `--quiet` | Errors only | Hidden |

Quiet and verbose conflict. Quiet preserves results, exit statuses and saved logs.
Command failures are rendered once on stderr, including argument and configuration
errors. JSON failures additionally include the structured error on stdout.
To suppress stderr completely, redirect it with `2>/dev/null`.

Worker stdout/stderr are arbitrary bytes, not severity-tagged diagnostics. Both
are mirrored to stderr without prefixes or inferred severity, and captured in
separate files. Quiet hides all worker output, including worker error text; the
execution result and exit status still report failure. Groove adds no colors or
terminal animations; worker bytes are forwarded as produced. Stdin stays closed
and workers receive files, not a pseudo-terminal, so child buffering still applies.

Terminal delivery uses a bounded queue (1 MiB of payload) and a maximum 250 ms
final drain. A slow or failed stderr consumer cannot block worker supervision.
Some live output may be omitted under backpressure; when possible, a warning
points to the captured logs. Per-stream order is preserved; ordering between
stdout and stderr is not guaranteed. Saved worker files remain authoritative,
subject to the configured per-stream output limit.

Diagnostic events are saved as JSON Lines in `$XDG_STATE_HOME/groove/logs`, falling
back to `~/.local/state/groove/logs`. File severity is controlled independently by
`logging.level`, default `debug`. Terminal flags do not change file capture.

These are CLI diagnostic logs. [Managed execution](execution.md) captures worker
output separately under the selected pool's `task-logs/` directory; the retention
settings below do not apply to those files. Use `groove history` to find them and
`groove storage clean --logs --older-than 30d --dry-run` to preview their explicit cleanup.

```sh
groove acquire
groove -v acquire
groove -vv acquire
groove --json --quiet exec --task fix-parser -- cargo test > result.json
groove config global set logging.level=trace
GROOVE_LOG_DIR=/tmp/job-logs groove list
GROOVE_LOG_ENABLED=false groove list
```

## User preferences

Preferences live in `$XDG_CONFIG_HOME/groove/config.toml`, falling back to
`~/.config/groove/config.toml`. XDG paths must be absolute; unset, empty or relative
XDG variables use the fallback. No config file is required. Logging settings are
user-wide and separate from repository pool configuration.

```toml
[logging]
enabled = true
level = "debug" # error, warn, info, debug, trace
# directory = "/absolute/custom/log/path"
max_file_mib = 5
max_total_mib = 100
max_files = 100
max_age_days = 7
```

`GROOVE_LOG_ENABLED=true|false` and `GROOVE_LOG_DIR=/absolute/path` override file
settings. File settings override defaults. Logging preferences are set with `config global set`; there are no root
`--log-dir`, `--no-log-file` or `--diagnostics` flags. Verbosity remains a per-invocation flag.
Directories must be absolute (no shell expansion of `~` inside TOML). Limits must
be positive; file size cannot exceed the total target. Maximum accepted values
are 1048576 MiB, 100000 files and 36500 days.

```sh
groove config global path
groove config global show
groove config global set logging.enabled=false
groove config global set logging.max_files=50 logging.directory=/tmp/groove-logs
groove config global unset logging.directory
groove config global edit
```

`config global show` returns effective settings, the config path, and indicators of
environment overrides. An absent `directory` means the automatic XDG state location.
`set KEY=VALUE` accepts TOML literals (`true`, `50`, `"text"`) or bare strings, and
validates every update together before one atomic save; `unset KEY` restores a
default. Comments are preserved. These commands require no Git repository; with
`--json` they use the normal response envelope (`config_path`, plus `config` and
`environment_overrides` for show, or `updated: true` for a successful write).

`edit` opens a temporary copy using `$VISUAL`, then `$EDITOR`, then `vi`. Editor
arguments and quotes are supported without shell evaluation. Editor UI goes to
stderr, leaving stdout available for the final response. Groove validates
the edited copy, checks for concurrent file changes, and atomically replaces the
config only after success. Invalid edits or editor failures leave the original
unchanged; the temporary copy is discarded. Concurrent Groove edits fail with
`CONFIG_CONFLICT` rather than silently overwriting one another. Changes take
effect on the next invocation.

Unknown keys, invalid TOML, invalid overrides and out-of-range limits fail before
an operational command runs. `config global path` and `config global edit` remain available to
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
Core code emits events without configuring global logging.

Logs omit raw argument vectors, Git output, file contents and environment dumps.
Event strings are bounded to 2048 UTF-8 bytes, and request IDs to 2048 characters.
New log files use mode 0600 and new directories mode 0700. Existing directory
permissions are unchanged; existing destinations must be owned by the current
user, must not be writable by group/others, and cannot traverse symlinks.
Cleanup locks must be regular, singly linked, owned files without group/other
write permission. Invalid destinations disable logging without blocking the
command, including when a FIFO occupies the lock pathname.
Logs are troubleshooting data, not an ownership
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
or write failures never change a lease operation's result. Startup warns when a
file cannot be opened, unless quiet. Diagnostic file writes are synchronous
but not fsynced, so abrupt termination can lose the final event. Malformed config
is validated separately, before any lease operation starts.
