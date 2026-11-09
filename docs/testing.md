# Test infrastructure

Implemented infrastructure for testing Groove with real Git, kernel locks, and
processes. All repositories are temporary and local; tests need no network,
credentials, or developer repository.

## Disposable repository fixture

A reusable `Repo` helper creates a repository inside a `tempfile::TempDir`
and removes it after the test. It supports ordinary repositories, bare
repositories, and linked worktrees, with helpers to:

- Create commits and record their OIDs.
- Initialize pools and inspect managed worktrees.
- Introduce staged, unstaged, untracked, ignored, and nested-repository contents.
- Construct interrupted operations, damaged indexes, and broken registrations.
- Inspect HEAD, index, files, refs, registration, and lock inode identities.

Use the installed Git executable with explicit arguments and working directories.
Isolate home and Git configuration, set a local test identity, disable hooks and
prompts, and clear inherited repository-selection variables. Never change the
test runner's global environment or working directory.

Assertions inspect actual Git and filesystem state, rather than relying solely
on Groove's success response. Cleanup must preserve the primary checkout,
unrelated worktrees, and shared refs.

## Controllable process harness

A reusable harness launches helper clients or the freshly built Groove binary.
It captures output, sends commands, waits for named checkpoints over pipes or
sockets, and can terminate a process at a precise boundary. It also implements
the CLI's socket and descriptor transport so clients can retain lease ownership.

Example scenario:

1. Client A acquires the only slot and signals `lease_acquired`.
2. Client B attempts acquisition and receives `POOL_EXHAUSTED`.
3. The harness kills A and waits for its exit.
4. Client C acquires the slot; the test verifies the restored baseline.

Add checkpoints around Git mutations, metadata publication, and descriptor
handoff. Use barriers for ordering and bounded timeouts for hang detection;
never use sleeps to establish correctness. Test-only controls must not be
enabled in production builds.

The harness owns the temporary directories and closes its unused descriptor
copies. Direct helper children use an RAII guard that kills/reaps them on unwind;
intentional orphan-worker tests wait for socket closure and subsequent acquisition.
Crash diagnostics identify the named checkpoint. CI also bounds each job, since a
failed FIFO rendezvous or checkpoint listener cannot always detect a missing peer.

## Test layers and tools

| Layer | Coverage | Tools |
| --- | --- | --- |
| Validation/seeded | Parsing, error mapping and repeatable contamination/recovery sequences | Rust `#[test]`; explicit deterministic seed |
| OS component | Lock contention, descriptor duplication, inheritance and transfer | Process harness; production syscall wrappers |
| Git integration | Discovery, sanitation, recovery, quarantine and path safety | Repository fixture; real Git |
| CLI end-to-end | JSON, exit codes, acquisition handoff and release | `std::process::Command`, `serde_json`, descriptor-aware harness |
| Stress/compatibility | Repeated contention, crash sequences and supported Git versions | Same fixtures and harness; extended CI jobs |

Use JSON Schema validation for protocol and metadata conformance. Assert
identity relationships explicitly. Mock only narrow failure boundaries, such as
failed writes or Git commands; successful integration paths use real resources.

## CI and acceptance

Run unit, OS, Git, CLI, and bounded deterministic crash tests on every PR.
Run larger randomized workloads and broader compatibility checks nightly, with
reproducible seeds. CI exercises the installed binary and both reference examples.
Retries must not hide flaky ownership tests. Property-test frameworks, snapshot
libraries and a nextest runner remain optional future tooling, not runtime needs.

Across these suites, verify exclusive ownership, capacity bounds, verified
baselines, stable lock identities, safe recovery, and rejection of stale releases.
Include surviving children and abandoned descriptor handoffs. SIGKILL tests
process-crash recovery; power-loss durability requires separate storage testing.

## Running the implemented suites

Requirements: Linux, Rust 1.89+, Git 2.40+. Node/npm enable the optional npm
compatibility smoke test; that one test returns early when npm is unavailable.
CI supplies Node/npm. Downloading Rust dependencies requires network initially;
the actual tests use only local repositories and resources.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --locked
cargo build --workspace --examples --locked

# Reproduce a larger deterministic contamination sequence:
GROOVE_STRESS_SEED=2026 GROOVE_STRESS_STEPS=100 \
  cargo test -p groove-core --test stress --locked

# Isolate ownership and transport failures:
cargo test -p groove-core --test concurrency --locked
cargo test -p groove-cli --test handoff --locked
cargo test -p groove-cli --features test-hooks --test crashes --locked
cargo test -p groove-cli --test git_child --locked

# Minimum supported compiler:
cargo +1.89.0 test --workspace --all-targets --all-features --locked
```

`test-hooks` is a compile-time opt-in, disabled by default. Never enable it for
production packaging. It connects to `GROOVE_TEST_SOCKET` only when the named
`GROOVE_TEST_CHECKPOINT` is reached; the harness controls continuation or SIGKILL.
Normal binaries do not include this behavior. Production backoff/polling may
sleep; tests never use sleep to establish ownership or crash ordering.

## Coverage map

| Suite | Checks |
| --- | --- |
| core initialization | Matching/concurrent init, conflicts, linked/bare discovery, custom roots, schema rejection, symlink and UTF-8 constraints |
| core ownership | Exhaustion, final closure, duplicates, explicit release, wrong generations, independent opens, unwind and release-failure retention |
| core recovery | Staged/unstaged/deleted/ignored contents, nested repositories, arbitrary filename bytes, index flags, wrong commits, interrupted rebase, malformed indexes, symlink quarantine, surviving branches, empty trees, tracked symlinks, SHA-256, unsupported gitlinks/sparse checkout, 18,000-file trees and gitlinks beyond the diagnostic limit |
| core concurrency | Eight processes/two slots with monotonic ownership intervals, SIGKILL of sole owner, surviving participating worker retaining its lease |
| core compatibility | Cargo build and Git commands inside `.git/groove/worktrees`; optional offline npm script |
| core stress | Seeded sequences of contamination, index flags, interrupted operations, explicit and implicit release; seed and step reported on failure |
| CLI protocol | JSON help/version/errors, exit categories, request IDs, verbose stderr, bounded JSONL file logs, fresh matching UTC timestamps across successful lifecycles and startup notices, ENOSPC isolation, concurrent cleanup and crashed log owners, concurrent init, real responses and metadata validated against published schemas |
| CLI handoff | Ownership survives sender exit, atomic CLOEXEC, stale release rejection, queued abandonment, failed send, malformed/truncated JSON/ancillary data, missing/extra descriptors, mismatched request IDs, capacity timeout |
| CLI crash checkpoints | Init metadata sync/rename and ready publication; existing-tree cleanup, old-tree removal, new-tree creation, verification, lease persistence; release metadata/unlock, lost response, same-generation retries and stale-generation rejection |
| CLI Git child | Parent SIGKILL while mutation child owns both locks; Git execution timeout terminates blocked mutation group before recovery |

The Git-child shim blocks on FIFOs immediately before exec of the installed real
Git. This deterministically checks descriptor lifetime across Groove's death;
it is not an assertion that every instruction inside Git has been fault-injected.
The receiver-close tests also exercise queued kernel references, not just local
Rust ownership.

## Remaining validation limits

SIGKILL does not simulate power failure, storage-controller caches, or filesystem
corruption. Supported filesystem checks are conservative rejection checks, not a
proof that arbitrary storage implements flock/fsync correctly. Minimum-kernel
VM testing, malicious directory-replacement races, exhaustive Git configuration
combinations, arbitrary detached grandchildren and broad editor/watch compatibility
are not covered. Worktrees remain a cooperative isolation mechanism.

The CI workflow defines minimum/current compiler and Git combinations, packaged
examples and scheduled extended seeds. A checked-in workflow is not evidence that
remote CI has run; inspect the actual run before declaring a release. Local
verification results are reported with the implementation change.

## Local verification record — 2026-11-09

Executed on Linux `7.0.0-42-generic`, using disposable local repositories:

- Full suite: 36 Rust test entries, including the process-helper entry; no failures.
- Current toolchain: Rust 1.99.1 and Git 2.53.0.
- Minimum compatibility: Rust 1.89.0 and Git 2.40.0; the full suite passed with
  that pair, and Git 2.40.0 was also exercised with the current compiler.
- Formatting and Clippy with warnings denied were checked on minimum/current Rust.
- Published response and metadata schemas were checked against actual outputs.
- Cargo/npm compatibility cases executed; npm was available on this machine.
- Both Rust examples ran against an installed CLI and an empty-commit fixture.
- Extended workload: seed 2026, 100 contamination/recovery cycles passed.
- Production CLI installation without test hooks succeeded.

This records local checks. Remote GitHub Actions and power-loss/kernel-VM coverage
are not implied by these results.

## Follow-up regression verification — 2026-11-09

- All 38 test entries passed on the current compiler and Rust 1.89.0.
- The new large-tree test acquires an 18,000-file commit whose tree listing exceeds
  1 MiB, then rejects a gitlink after that limit.
- The new release test kills the CLI after metadata rename, release recording,
  and unlock; retries finish the same generation and cannot release its successor.
- Clippy passed with warnings denied on both compilers. The public usage doctest
  and rustdoc with warnings denied passed on the current compiler.
- The core crate packaged and compiled successfully. Both package file lists
  include licenses and README; the CLI also packages its shared test fixtures and
  schemas. Registry verification of the CLI still requires publishing its core
  dependency first, as documented in releasing.md.
- Production-feature builds and both reference examples ran successfully.
- `cargo audit --deny warnings` passed against 1,429 RustSec advisories for
  the 127 dependencies in Cargo.lock. This is a point-in-time dependency check.

## Structured API verification — 2026-11-09

- The suite now contains 45 test entries, validated on current Rust and Rust 1.89.
- Identifier tests cover slot range/path validation, map-key deserialization,
  UUID conversion and SHA-1/SHA-256 representation checks.
- Error tests verify all v1 wire names, exit categories and inspectable sources.
- CLI tests reject malformed IDs before descriptor access and compare the slot
  schema with Rust validation across all four-digit slot numbers.
- Malformed handoff coverage includes invalid slot/lease/commit IDs and protocol
  modes, asserting that rejection does not leak the transferred lease descriptor.
- Doctests include compile-fail examples preventing raw strings and unrelated UUIDs
  from being used directly as typed IDs. Clippy and rustdoc pass with warnings denied.

## MCP adapter verification — 2026-11-09

- All 51 workspace test entries pass on current Rust and Rust 1.89.0, including
  real stdio MCP negotiation using protocol 2025-11-25 and generated-schema checks.
- MCP tests cover repository scoping, initialization conflicts, invalid revisions,
  retained ownership between calls, capacity exhaustion, same-key retries,
  duplicate release after successor acquisition, cancellation, EOF and SIGKILL.
- A separate external writer survives server death and completes its write while
  CLI/core acquisition remains blocked by durable protection. A new server lists
  and explicitly releases the abandoned generation only after writers stop.
- Core tests cover manifest schema 2, live-owner rejection, wrong generations,
  malformed protections, failed-release ownership retention and restored baselines.
  Release crash retries now also exercise protected leases.
- Formatting, Clippy on current/minimum Rust, rustdoc with warnings denied and
  four doctests pass. The updated 181-dependency lockfile passes cargo-audit against
  1,429 RustSec advisories. These are point-in-time local checks.
- Both production binaries are installed locally without test hooks. CI additionally
  runs the three default-feature stdio tests against the installed MCP executable
  using GROOVE_MCP_BINARY. The MCP package includes its shared test fixture.
- The three production MCP stdio tests also passed against the locally installed
  executable, and both installed binaries reported version 0.1.0.

## Opt-in real-repository smoke test

To exercise the installed production MCP server with an existing local project's
committed history and files:

```sh
GROOVE_TEST_LOCAL_REPO=/absolute/path/to/project \
GROOVE_MCP_BINARY="$HOME/.cargo/bin/groove-mcp" \
  cargo test -p groove-mcp --test stdio --locked -- --ignored --nocapture
```

The test uses a disposable local clone with copied Git objects. It checks two
independent workspaces, bounded capacity, a saved result branch, sanitation,
server death, protected work and recovery through a new server. Source status,
tracked diff, HEAD and refs are compared before and after; the source pool is not
initialized. The clone is removed on completion. Uncommitted source edits are
not included in the test checkout. This test passed locally against an existing
project using the installed production executable; it requires no project build,
service credentials or changes to the agent application.

## CLI-first workflow verification — 2026-11-09

- The full workspace suite passes on current Rust and Rust 1.89.0: 56 test
  entries passed, with the opt-in real-repository smoke test left ignored.
- Ordinary CLI process tests exercise repository discovery from a subdirectory,
  reservations surviving command exit, bounded concurrent acquisition, saved
  result branches, sanitation, explicit stopped-writer confirmation, lost stdout,
  release retries and rejection of stale generations.
- SIGKILL checkpoints cover protection publication and interrupted durable release.
  The crash test performs follow-up ownership operations through independent CLI
  processes, avoiding lock inheritance between threads of the process harness.
- CLI recovery rejects live protected descriptor owners (the MCP ownership model)
  and unprotected leased records. An abandoned protected lease is releasable only
  with matching identifiers after writers stop.
- Formatting, Clippy with warnings denied on both compilers, four doctests and
  rustdoc with warnings denied pass. The agent skill validator, local documentation
  links and shell syntax checks pass. Cargo audit reports no denied advisories
  against the 181-dependency lockfile at this check.
- Production binaries installed into an isolated temporary prefix without test
  hooks pass all four CLI workflow tests and all three default-feature MCP tests.
  CI now includes those installed-CLI tests alongside its installed-MCP checks.
- Package file lists include the CLI regression tests and updated response schemas.
  These are local checks; no remote CI result is implied.

## Snapshot verification — 2026-11-10

Snapshot-specific coverage adds four pure unit tests, twenty real-Git integration
tests, and five CLI/process tests. These cover IDs and path boundaries, retention,
exact staged/unstaged/untracked recovery, file modes and symlinks, non-UTF-8 names,
file/directory transitions, SHA-1/SHA-256 including unborn repositories, split
indexes, sparse checkouts, global ignore rules, failed publication, unsupported
conflicts/filters/gitlinks/index flags, large-file limits, pins and pruning,
exclusive-object reclamation, interrupted capture/restore, concurrent writes,
lost stdout, scratch cleanup, and recovery after pool release and slot reuse.

Final local checks:

- 98 workspace tests passed on Rust 1.99.1 and Rust 1.89.0 with
  `--all-targets --all-features --locked -- --test-threads=1`.
- One pre-existing opt-in MCP real-repository smoke test was skipped. The snapshot
  CLI was separately exercised against a disposable copy of Iurio.
- Four doctests passed; Clippy and rustdoc passed with warnings denied; formatting
  and `git diff --check` passed.
- Release-build benchmarks exercised Iurio, 100k files and 1M files, including
  recovery into another checkout and in place. See
  [measured results](snapshot-benchmarks.md) and
  [raw samples](snapshot-benchmark-results.json).

The default parallel test runner intermittently failed the pre-existing
`protection_survives_drop_and_only_matching_explicit_release_clears_it` pool test
with “Protected lease still has a live lock owner.” Serial scheduling passed on
both compilers; the pool implementation was left unchanged. Explicit subprocess
concurrency tests still exercise concurrent processes under serial test scheduling.
This record does not claim the default parallel workspace run is consistently green.

Process-kill tests do not simulate power loss or storage-controller failure.
Benchmarks do not establish cold-cache guarantees or all large-binary workloads.
Remote CI has not been run for this change.

## Versioned scale regressions and hooks

The snapshot lab now includes deterministic 1k/100k/1M-file integration tests, a
shared versioned fixture contract with a golden tree, a digest-pinned container
environment and resource limits, and source/binary/environment provenance in v2
reports. Relevant PRs run the 100k regression; scheduled/manual CI adds 1M and
performance measurements. See [the lab](../benchmarks/snapshots/README.md).

The two protection tests now isolate their process fixtures with a test-only
mutex: an unrelated test's fork can correctly retain a CLOEXEC lease descriptor
until exec, invalidating the tests' assumption that dropping their own handle
removes the last owner. Product ownership rules are unchanged; deliberate child
inheritance is still exercised by concurrency and Git-child regressions. The
pre-push gate uses default scheduling rather than treating serial-only success
as sufficient evidence. Hook tests use real disposable Git commits and pushes
to verify failure blocks publication and custom hook paths are preserved.

### Pinned lab verification — 2026-11-10

The digest-pinned container was built and its Rust 1.89.0, Git 2.40.0, Python
3.11.2 and GNU time 1.9 package versions were verified. The deterministic 1k,
100k and 1M integration cases passed with four-CPU/8-GiB limits and disk-backed
scratch. The 100k and 1M correctness cases took approximately 29 and 389 seconds;
these are execution observations, not pass/fail timing thresholds.

Two canonical smoke-plus-Iurio benchmark runs completed, each verifying both
restoration modes and unchanged source state through a read-only source mount.
Their v2 reports passed compatibility comparison. Ten Python infrastructure/hook
tests passed, including golden-fixture generation, environment-drift rejection,
real Git commit/push blocking on failed gates, and custom-hook preservation.
These are local executions of the pinned lab; remote CI execution is separate.
