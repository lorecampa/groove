# Testing

Use disposable repositories for every ownership, cleanup and recovery test.
The source checkout is never a benchmark fixture. Repository tooling and test
helpers use Rust/Bash; internal tools are unpublished Cargo workspace packages.

## Writing tests: intent and observable contracts

These practices apply to contributors and coding agents. Start with the guarantee
the caller needs, then choose inputs and assertions that would expose its violation.
A test should survive a refactor that preserves that guarantee.

- Name the behavior and conditions, such as `stale_reservation_ids_never_release_a_successor`.
  Prefer one coherent scenario; table-driven cases are useful for parallel examples.
- Exercise public core APIs or the real CLI. Assert returned values, typed errors,
  exit statuses, file contents, Git state and effects on other reservations.
  Small unit tests are appropriate for pure rules; they must still describe the rule,
  rather than reproduce the function's algorithm in the expected value.
- For rejected destructive operations, assert the error **and** preserved data,
  reservation identity and continued exclusion where relevant. `is_err()` alone
  can pass because setup failed for an unrelated reason.
- For CLI tests, assert the versioned schema, expected success/error result,
  exit category and stdout/stderr rules. Schema validity alone does not prove the
  requested command worked. Worker exit codes intentionally differ from Groove errors.
- Use independent expectations: restore and inspect files, apply a result with Git,
  or compare with a small specification model. A serialize/deserialize roundtrip
  alone can miss matching defects on both sides; also check the published format.
- Avoid asserting private helper calls, incidental operation order, exact diagnostic
  prose or whole internal records. Use stable error codes. Exact formatting belongs
  in deliberate presentation tests; reviewed output snapshots must normalize only
  nondeterministic values, never meaningful behavior.
- Internal state is justified when it **is** a documented contract: permanent lock
  inode identity, persisted schemas and crash durability are examples. Corrupting
  metadata or using a checkpoint to arrange a failure is valid; assert the resulting
  public behavior and safety invariants afterwards.
- Ask before finishing: what plausible bug would make this test fail? Do not add
  assertions or snapshots solely to increase a coverage percentage. Prefer a focused
  regression for each discovered bug and retain it after the broader property test.

## Libraries and tools

| Need | Preferred approach |
| --- | --- |
| Examples and pure rules | Rust `#[test]`, ordinary assertions and table-driven cases |
| Disposable files and repositories | `tempfile` and `tests/support::Repo`; real Git |
| Machine-readable CLI contracts | `serde_json` for semantic assertions plus `jsonschema` for published schemas |
| Generated inputs and sequences | `proptest` as a dev-dependency; independent strategies, shrinking and persisted regressions |
| Process/crash/descriptor scenarios | Shared `TestChild`, `accept_child` and checkpoint helpers; real OS processes |
| Assertion-strength audits | `cargo-mutants`, scoped to relevant code and tests in a disposable copy |

Keep the current minimal `proptest` feature set (`std`); its Rust 1.88 minimum is
compatible with Groove's Rust 1.89 minimum. Review features, transitive dependencies,
MSRV and lockfile changes when adding/upgrading a library. Testing dependencies belong
in dev-dependencies, not production dependencies.

`assert_cmd` is an optional choice for simple CLI cases if it removes meaningful
boilerplate; it does not replace descriptor/process lifecycle tests. `insta` or
`snapbox` may suit a deliberate presentation contract, but do not snapshot every
JSON response or internal structure. Do not add mocking frameworks for Git/flock
behavior: mocks cannot establish those OS guarantees. Loom models instrumented
thread synchronization, not the real filesystem/flock/process behavior tested here.

Nextest can provide an additional run with per-test timeouts and reporting. Its
process-per-test scheduling differs from libtest; it must not replace the required
default-scheduled Cargo run or be used to declare an unexplained failure fixed.
Do not turn on automatic retries to conceal flaky tests.

## Process isolation and failure paths

CI explicitly disables the runner's system/global Git configuration. Snapshot
fixtures must not inherit installed content filters or automatic CRLF conversion.
Use `support::groove_in` for repository CLI fixtures, including checkpoint children;
tests of user preferences can explicitly override their configuration paths.
`checkpoint_finish_ignores_parent_git_filters` exercises the finish/recovery crash
scenario in a child test process with a global filter definition.

Use explicit readiness handshakes, not sleep-based guesses. Bound connection,
message and exit waits and report captured output on failure. `TestChild` captures
stdout/stderr to temporary files (at most 1 MiB per stream is read back), avoiding
pipe backpressure and waits for inherited pipe writers. It owns the direct child
through completion/unwind and kills/reaps that child on drop. It does not stop
arbitrary detached descendants: tests spawning writers must arrange their teardown
using the managed-execution protocol or an explicit bounded handshake.

`TestChild::checkpoint` checks early exit, applies a total connection/read deadline
and validates the checkpoint name. `harness.rs` exercises early exit, missing and
incomplete readiness, large output, wait timeouts and cleanup during unwind.
When adding a new harness failure mode, test it with a real failing peer.

Fork temporarily inherits **all** open descriptors, including another test thread's
CLOEXEC leases, until exec. A Rust handle going away does not prove kernel ownership
has ended. `ownership.rs` and `handoff.rs` use `in_subprocess` to run fixtures concurrently
in distinct processes, without retrying assertions. The receipt/protection fixtures
in `validation.rs` use the same isolation so unrelated fixture forks cannot retain
their short-lived reservation descriptors. `fork_inheritance.rs` deliberately
holds the pre-exec boundary and verifies rejection/preservation before exec and release
after descriptor closure while the child remains alive, for both slot and execution
descriptors, including blocked finish and successful checkpoint preservation after
exec. Preparation fixtures also run concurrently in separate processes: unrelated
fork/exec can otherwise keep a just-dropped execution guard alive and correctly
make an immediate setup attempt report contention. Keep explicit contention and
surviving-child tests; fixture isolation is not a substitute for them. Some older
fixtures use a mutex and therefore do not establish concurrent fixture scheduling.

Never fix a timing failure by deleting protection, explicitly unlocking an inherited
descriptor, adding an arbitrary sleep or serializing the entire suite. Establish the
cause, arrange the relevant boundary deterministically, and check failure as well as
success. Bounded polling is appropriate when closure is asynchronous; it must observe
the real condition and fail at its deadline, not treat time passing as success.

## Property and mutation testing

`stress.rs` always covers all six contaminations with both explicit release and
drop. Its property test independently generates those choices and sequences, then
checks pristine contents, HEAD/index, source preservation and the final acquisition.
Do not derive multiple choices from correlated bits of the same hand-written RNG.
Use bounded case/sequence counts for expensive repository tests; keep shrinking on.

The default is 16 generated cases with up to 8 operations. `PROPTEST_CASES` controls
case count, `GROOVE_STRESS_STEPS` the maximum sequence length (1..1000), and
`GROOVE_STRESS_SEED` supplies a reproducible seed; otherwise Proptest's normal random
seed/`PROPTEST_RNG_SEED` applies. Failures report minimized inputs. Commit meaningful
seeds from `crates/groove-core/proptest-regressions/stress.txt` and add a readable
example for the discovered invariant; never ignore or delete seeds just to get green.

Install the optional mutation tool separately from workspace dependencies:

```sh
cargo install cargo-mutants --version 27.1.0 --locked
cargo mutants -p groove-core -f crates/groove-core/src/identifiers.rs \
  --output target/mutants-identifiers --timeout 60 --build-timeout 120 \
  --cargo-arg=--locked --cargo-test-arg=--test=types
```

Keep the unmutated baseline check enabled and use the default disposable copy,
never `--in-place` on a developer checkout. Inspect every surviving mutation: it may
be an equivalent change, a missing scenario, or an assertion gap. Improve the test
through observable behavior and rerun affected mutations. Do not add exclusions just
to raise a score. Report the scope, caught/missed/unviable/timeouts and baseline failures;
a focused successful audit is not evidence about untested modules.

## Required checks

The pinned toolchain includes rustfmt and Clippy. Run the same gate as pre-push:

```sh
bash scripts/check.sh push
```

It performs:

```sh
git diff --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

Use default test scheduling; serial-only success does not establish concurrent
correctness. CI also tests current stable, minimum Git and installed production
binaries. Checked-in workflows are not evidence that remote CI has run.

The macOS matrix uses native Apple Silicon and Intel runners on macOS 15 with
Rust 1.89 and stable. It runs the same full suite with default scheduling, APFS
reflink tests separately, and packaged production smoke tests. An additional
case-sensitive APFS disk image exercises process/descriptor, snapshot and cache
regressions. `GROOVE_TEST_ROOT` selects disposable repository-fixture storage;
macOS defaults those fixtures to short paths under `/private/tmp` so test sockets
fit Darwin's path limit. This does not alter product paths or global test scheduling.
These runners do not establish behavior on the minimum macOS 13 release.

## Focused regressions

| Area | Test target |
| --- | --- |
| Allocation, sanitation and recovery | `groove-core` integration tests |
| Pool deletion, retained history, generations and path preservation | `cargo test -p groove-core --test destruction --locked` and `cargo test -p groove-cli --test destruction --locked` |
| Durable keys, contention, inspection and finish | `cargo test -p groove-core --test coordination --locked` |
| Detached workspaces and lookup by task label | `cargo test -p groove-core --test tasks --locked` |
| Fork/exec descriptor inheritance and harness failures | `cargo test -p groove-core --test fork_inheritance --test harness --locked` |
| CLI execution, signals, timeout, logs and orphan recovery | `cargo test -p groove-cli --test execution --locked` |
| Workspace cwd, executable and open-file diagnostics | `cargo test -p groove-core --test quiescence --locked` |
| Human output, `--json` envelope, help and exit codes | `cargo test -p groove-cli --test protocol --locked` |
| Single-slot inspection, optional Git status and refreshed pool growth | `cargo test -p groove-core --test inspection --locked` |
| Deterministic crash boundaries | `cargo test -p groove-cli --test crashes --features test-hooks --locked` |
| Surviving Git children keep slot and admin locks | `cargo test -p groove-cli --test git_child --locked` |
| Repository layouts (reftable, separate Git dir, relative worktree links, linked-worktree HEAD) and out-of-band deletion/moves | `cargo test -p groove-core --test environments --locked` |
| CLI contracts for deleted cwd/checkouts, label reuse/escaping and undeliverable responses | `cargo test -p groove-cli --test edge_cases --locked` |
| Package/release tooling | `cargo test -p groove-release --locked` |
| Snapshot scale and benchmark tooling | [Snapshot lab](../benchmarks/snapshots/README.md) |

Layout tests that need newer Git (reftable 2.45, relative worktree links 2.48)
print a skip note and return early on the minimum-Git CI job; the current-Git job
runs them. `verification_rejects_writes_racing_in_place_cleanup` (test-hooks)
writes into a slot between cleanup and verification, so a no-op verifier fails it.
On macOS, `a_filesystem_monitor_daemon_neither_blocks_release_nor_goes_stale`
starts Git's built-in monitor daemon in a slot (Linux Git has none) and checks
that release is not blocked and the next reservation sees fresh status.

`test-hooks` is disabled in production. At a named checkpoint, it connects to
`GROOVE_TEST_SOCKET`; the harness chooses continuation or SIGKILL. Ownership and
crash ordering use locks, sockets, FIFOs or worker readiness markers. Short sleeps
are used for bounded polling and to observe the absence of delayed writes, not as
proof that a lease was acquired.

Increase seeded contamination coverage with:

```sh
GROOVE_STRESS_SEED=2026 GROOVE_STRESS_STEPS=100 \
  cargo test -p groove-core --test stress --locked
```

## Optional host tests

The benchmark roundtrip and 100k/1M-file scale tests run locally only and must
be explicitly selected; see the lab for resources, pinned environment and commands.
GitHub runs the normal suite and its small snapshot correctness fixtures, without
performance measurements or a separate scale workflow.

## Limits

CLI execution tests verify live stderr delivery before worker completion, quiet
mode with preserved files/results, and bounded timeout handling with blocked or
failed stderr. Protocol tests cover severity thresholds, independent file levels,
single terminal error rendering and JSON responses with nonempty stderr.

SIGKILL is not power loss or storage corruption. Filesystem rejection checks do
not certify flock/fsync. Minimum-kernel VM coverage, malicious same-user path
races, every Git configuration and broad editor/watch compatibility remain
unestablished. Process groups cover cooperative descendants only. External
writers must stop before release/checkpoint.

## Project setup, artifacts and handoff

The default suite covers compatible seeding across slots, independent artifacts,
file modes/mtimes, toolchain/input invalidation, external source imports, rejected
tracked/escaping/symlink roots, readiness prerequisites and explicit retries.
CLI conformance covers setup results and worker exit categories, literal agent
arguments, unique-label selection and snapshot recovery. Removed patch-export
flags are rejected before finishing or restoring.
No real agent service is invoked.

With test-hooks, real-process SIGKILL boundaries cover setup-recorded, completed
steps, cache publication and readiness publication. Children retain exclusion after
supervisor death; release remains blocked until explicit writer recovery. Tests
preserve permanent lock inode identity. Handoff tests include SHA-1/SHA-256,
binary and untracked files, unborn HEAD, new recovery directories, and retries after
slot reuse without changing the successor's HEAD/index/files.

Reflink artifact caches (`mode="reflink"`) need reflink-capable storage. Point
`GROOVE_TEST_REFLINK_ROOT` at disposable storage on such a filesystem and run
`cargo test -p groove-core --test preparation --locked -- --ignored reflink`. The
ordinary suite leaves that test ignored; report the separate run before claiming
coverage. Large-repository latency and concurrent throughput are separate from
correctness regressions.
