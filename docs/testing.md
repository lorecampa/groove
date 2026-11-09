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
| CLI protocol | JSON help/version/errors, exit categories, request IDs, stderr JSON Lines, concurrent init, real responses and metadata validated against published schemas |
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
