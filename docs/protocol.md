# Groove CLI protocol v1

Status: implemented development interface. Freeze v1 only after release review.

Agents using ordinary editing/terminal tools can use the [stdio MCP adapter](mcp.md).
Its durable reuse protections upgrade the ready manifest to schema 2; this version
reads manifests 1 and 2. CLI response envelopes and slot records remain v1.
The CLI has exactly three commands. `run` and `status` are deferred.

## Callers and ownership

Rust applications normally use `groove_core::Pool` and `Lease` directly. The CLI
is intended for supervisors capable of Unix descriptor passing. An ordinary
shell command that only captures JSON cannot retain a lease after `acquire`
exits. The reference supervisor is
[`crates/groove-cli/examples/client.rs`](../crates/groove-cli/examples/client.rs).
It creates a socket pair, inherits one endpoint into the CLI, receives ownership,
retains the descriptor, and passes it to `release`.

```sh
cargo build -p groove-cli
# First initialize a disposable repository or your intended pool:
target/debug/groove --repo /path/to/repo init --capacity 4
cargo run -p groove-cli --example client -- \
  "$PWD/target/debug/groove" /path/to/repo main
```

The reference client demonstrates the wire protocol; the core agent example
instead demonstrates direct library integration and participating worker lifetime.
Neither introduces another Groove command.

## Commands

```text
groove [--repo PATH] [--request-id ID] [--diagnostics]
       [--git-timeout-ms MS] init --capacity N
       [--cleanup pristine|warm] [--worktree-root PATH]

groove [global options] acquire --base REV --lease-channel-fd FD
       [--wait-ms MS]

groove [global options] release --slot SLOT --lease-id UUID --lease-fd FD
```

Global options may appear before or after a subcommand. `--repo` defaults to `.`;
`--request-id` is optional and defaults to JSON null. Request IDs correlate
responses; they do not make requests idempotent. `--git-timeout-ms` is positive,
defaults to 120000, and bounds each Git operation and administrative-lock wait.
Repository discovery on an already initialized pool has a separate 30-second
bound. `--wait-ms` defaults to 0 (one capacity scan); positive values bound
capacity waiting, not total preparation time. Each failing slot is attempted at
most once per acquisition. There is no FIFO fairness guarantee.

`init` accepts capacity 1..4096. Repeating an identical configuration succeeds
with `created: false`; capacity, cleanup policy or root changes fail with
`CONFIG_CONFLICT`. Relative external roots resolve from the caller's current
directory. A new root must be empty, owned by the user, and cannot overlap Git
administration. Initialization intent is persisted and matching interrupted
initialization resumes. Lock files are never replaced to repair a ready pool.

`pristine` (default) removes all untracked and ignored files. `warm` preserves
ignored files when reusing a healthy worktree; they are not part of the verified
baseline and may include more than caches. Rebuilding a damaged slot can discard
them. Both modes reset tracked files and the index, detach HEAD, and reject
unsupported Git operations/configurations before handoff.

## Response envelope

Each command emits one newline-terminated JSON object on stdout. No banners,
progress messages, raw Git output or human-readable errors are mixed into it.
Help/version also return this envelope, with `result.text`. Examples below are
pretty-printed; actual output is one line. Additive response fields may be ignored;
unknown major API versions must be rejected. Metadata schemas are stricter.

```json
{"api_version":1,"request_id":"req-1","ok":true,"result":{}}
```

Errors replace `result` with `error`:

```json
{"api_version":1,"request_id":null,"ok":false,"error":{"code":"POOL_EXHAUSTED","message":"No verified worktree slot is available","retryable":true,"details":{"capacity":2,"busy_slots":2,"failed_slots":{}}}}
```

Clients branch on `code`, not the wording of `message`. `retryable` indicates
whether the same capacity request may succeed when another owner finishes.
`details` provides diagnostic context, not another source of ownership.

Stderr is empty by default. `--diagnostics` enables JSON Lines error diagnostics
with `level`, `code`, `message` and `request_id`. No JSON response can be guaranteed
after SIGKILL, OOM termination, or failure of the output stream.

## Success results

| Command | Fields |
| --- | --- |
| init | `created`, `common_git_dir`, `state_dir`, `capacity`, `slots`, `cleanup_policy`, `worktree_root` |
| acquire | `slot_id`, `lease_id`, `worktree_path`, `base_oid`, `head_mode`, `recovery`, `cleanup_policy`, `lease_transport`, `descriptor_count` |
| release | `slot_id`, `lease_id`, `released`, `cleanup` |

All managed paths are absolute UTF-8 strings. Slot IDs are `wt-0001` through
`wt-4096`. Lease IDs are UUID v4 values. OIDs currently support Git SHA-1 and
SHA-256 formats. `head_mode` is `detached`. `recovery` is `created`, `sanitized`,
or `rebuilt`. Wire acquisitions use `lease_transport: "scm_rights"` and
`descriptor_count: 1`; direct library acquisition uses `"in_process"`.
Explicit release returns `released: true`, `cleanup: "deferred"`.
See [response schema](../schemas/response-v1.schema.json).

## Acquisition transport

1. The supervisor creates `socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC)`.
2. It deliberately passes one endpoint across exec and supplies its FD number.
   It closes its unused copy of that endpoint after spawning.
3. The CLI validates the connected socket before attempting acquisition.
4. It sends exactly one complete response object plus exactly one lease descriptor
   in an `SCM_RIGHTS` ancillary message. The maximum JSON payload is 64 KiB.
   Sending is nonblocking; a full or abandoned channel returns a protocol error.
5. After successful send, the CLI closes its lease copy without `LOCK_UN` and
   repeats the response on stdout. A failed stdout receipt cannot revoke a
   successful transfer. The socket message is authoritative even if the CLI dies
   before printing or returns no readable receipt.
6. The receiver uses `MSG_CMSG_CLOEXEC`, validates both the response and ancillary
   data, and retains the descriptor for the task lifetime. Truncation, wrong
   versions/request IDs and missing/extra descriptors are errors; all installed
   descriptors are closed on rejected messages.

A queued descriptor retains the lock even before `recvmsg`. A caller abandoning
a request must close both its unused endpoints and any received descriptors.
Closing the receiving socket discards queued descriptors. There is no persistent
socket service and no heartbeat. A JSON receipt never reconstructs ownership.
The reference receiver reports EOF/protocol failure when acquisition failed;
the supervisor also reads the CLI's structured stdout error and exit code.

## Release and failure semantics

The caller stops every writer and preserves results before release. It inherits
the lease FD into `release` and supplies the matching slot and UUID. The CLI
checks the descriptor's device/inode, current permanent lock identity, lease
generation and nonblocking flock ownership before writing release metadata and
explicitly unlocking. An independently opened FD cannot unlock an active lease.

Explicit release is optional: closing the **last** copy of the descriptor is also
a valid release. Closing one copy while a worker retains another does not free
the slot. Conversely, explicit unlock affects all copies of that open file
description; surviving descriptors do not protect writers after that unlock.
Release metadata is diagnostic and can lag crash-induced closure.

Before unlock, a failed release retains ownership in the caller's descriptor.
After unlock, a lost response is ambiguous: the caller must stop using the
worktree. A retry with the same descriptor and UUID can finish release after its
metadata was published, or confirm it while the slot has not been reassigned.
Both `leased` and `released` records are accepted for that generation, with the
generation checked again after obtaining the lock. Stale retries cannot unlock
a successor. The Rust `Lease::release`
method borrows the handle mutably, invalidating it only on success; a failure
leaves it active. Ordinary Drop only closes its descriptor.

| Exit | Codes |
| --- | --- |
| 0 | Success |
| 2 | `INVALID_ARGUMENT`, `UNSUPPORTED_CONFIGURATION` |
| 3 | `POOL_EXHAUSTED`, `ACQUIRE_TIMEOUT` |
| 4 | `NOT_INITIALIZED`, `CONFIG_CONFLICT`, `INVALID_REVISION` |
| 5 | `INVALID_LEASE`, `LEASE_MISMATCH` |
| 6 | `RECOVERY_FAILED`, `POOL_UNHEALTHY` |
| 7 | `IO_ERROR`, `GIT_ERROR`, `PROTOCOL_ERROR` |

## Structured Rust API

The development Rust API uses `ErrorCode` variants rather than strings. Match
`ErrorCode::PoolExhausted` in Rust; serialization still emits `"POOL_EXHAUSTED"`.
The enum is non-exhaustive, so clients should retain a fallback match arm.
Underlying I/O and parsing causes are available through `std::error::Error::source`;
the CLI response envelope continues to expose only code, message, retryable and
structured details.

`SlotId`, `LeaseId` and `CommitOid` are separate types with private representations.
Parse strings at input boundaries. Slots must be canonical `wt-0001` through
`wt-4096`; lease IDs parse as UUIDs; commit IDs must be full lowercase 40- or
64-character hexadecimal strings. Parsing an identifier proves its format, not
pool membership, commit existence, or lease ownership. Those checks remain part
of pool operations and descriptor validation.

`Pool::release_fd` takes `&SlotId` and `LeaseId`. `LeaseInfo::base_oid` is a
`CommitOid`; use `.as_str()` for textual output. `HeadMode`, `LeaseTransport` and
`CleanupTiming` replace free-form Rust strings with enums. Existing valid v1 JSON
retains its string representations. Invalid IDs and unknown mode values are
rejected during deserialization, including malformed descriptor handoffs, whose
received descriptors are closed on failure.

Existing Rust callers must migrate string comparisons to enum variants and parse
IDs before invoking release. `LeaseId::generate()` creates a fresh UUID v4;
`LeaseId::from(uuid)` is an explicit conversion for UUID interoperability.
