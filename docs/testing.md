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
  requested command worked.
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

Configuration compatibility uses checked-in historical TOML inputs and independent
effective JSON expectations under the CLI's `tests/fixtures/config/` directory.
Keep historical inputs unchanged when the format evolves; add cases rather than
regenerating old files with the current serializer. Review any expected-value
change as a contract change, including defaults and experimental graduation.
Validate sparse input against its published schema as well as exercising the
actual loader. Exact document comparisons protect comment-preserving edits;
roundtrips and current-schema validity alone cannot prove upgrade compatibility.
For feature-specific runtime coverage and retaining old opt-in fixtures through
graduation, follow the [experimental-feature
procedure](configuration.md#test-before-releasing-the-experiment) and
[stabilization cleanup](configuration.md#stabilization-and-cleanup).

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
`checkpoint_finish_ignores_parent_git_filters` exercises the release --save/recovery crash
scenario in a child test process with a global filter definition.

Use explicit readiness handshakes, not sleep-based guesses. Bound connection,
message and exit waits and report captured output on failure. `TestChild` captures
stdout/stderr to temporary files (at most 1 MiB per stream is read back), avoiding
pipe backpressure and waits for inherited pipe writers. It owns the direct child
through completion/unwind and kills/reaps that child on drop. It does not stop
arbitrary detached descendants: tests spawning writers must arrange their teardown
using an explicit bounded handshake.

`TestChild::checkpoint` checks early exit, applies a total connection/read deadline
and validates the checkpoint name. `harness.rs` exercises early exit, missing and
incomplete readiness, large output, wait timeouts and cleanup during unwind.
When adding a new harness failure mode, test it with a real failing peer.

Give independent repository scenarios separate named tests when using
`in_subprocess`. Its exit deadline covers the entire selected fixture, including
repository setup and cleanup. A loop over unrelated crash checkpoints shares one
deadline and can time out even when each individual scenario completes within it.
Keep checkpoints that form one recovery sequence together; preserve the existing
wait bounds and assertions when separating independent scenarios.

Fork temporarily inherits **all** open descriptors, including another test thread's
CLOEXEC leases, until exec. A Rust handle going away does not prove kernel ownership
has ended. `ownership.rs` and `handoff.rs` use `in_subprocess` to run fixtures concurrently
in distinct processes, without retrying assertions. The receipt/protection fixtures
in `validation.rs` use the same isolation so unrelated fixture forks cannot retain
their short-lived reservation descriptors. `fork_inheritance.rs` deliberately
holds the pre-exec boundary and verifies rejection/preservation before exec and release
after descriptor closure while the child remains alive, for both slot and execution
descriptors, including blocked release --save and successful checkpoint preservation after
exec. Keep explicit contention and
surviving-child tests; fixture isolation is not a substitute for them. Some older
fixtures use a mutex and therefore do not establish concurrent fixture scheduling.

Use `groove_in`/`groove_human_in` with a disposable fixture path for CLI commands.
The shared helper isolates Git configuration, HOME, XDG configuration/state and
logging; override these only with other fixture-owned paths when testing settings.
Core snapshot fixtures use `in_subprocess`, which sets private Git configuration
before the test process starts. Never mutate process-wide environment variables
from a concurrently scheduled test.

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
  --cargo-arg=--locked --cargo-test-arg=--test=types \
  --cargo-test-arg=--test=named_pools
```

The named-pool suite checks the default namespace's storage routing through
public operations; the type suite checks parsing, conversion and wire identity.

Keep the unmutated baseline check enabled and use the default disposable copy,
never `--in-place` on a developer checkout. Inspect every surviving mutation: it may
be an equivalent change, a missing scenario, or an assertion gap. Improve the test
through observable behavior and rerun affected mutations. Do not add exclusions just
to raise a score. Report the scope, caught/missed/unviable/timeouts and baseline failures;
a focused successful audit is not evidence about untested modules.

## Required checks

Run the same gate as pre-push:

```sh
bash scripts/check.sh push
```

[scripts/check.sh](../scripts/check.sh) owns the exact security, formatting,
Clippy, default-scheduled tests, doctests and rustdoc commands. Install its
prerequisites using [security maintenance](security.md). Choose focused tests
from the relevant crate's `tests/` directory while iterating; do not maintain a
second inventory of test names here.

Use default test scheduling; serial-only success does not establish concurrent
correctness. Do not use automatic retries to conceal unexplained failures.
The checked-in [workflows](../.github/workflows/) define hosted triggers,
platform/toolchain matrices and opt-ins. A workflow file does not prove that a
remote run succeeded or that account permissions/compute are available. Report
which checks actually ran, their revision/environment and any skips or failures.

`test-hooks` is disabled in production. Named checkpoints use
`GROOVE_TEST_SOCKET`; the harness chooses continuation or SIGKILL. Use locks,
sockets, FIFOs or readiness markers to arrange ownership/crash boundaries.
Newer-Git layout cases may skip on older Git; record skips before claiming coverage.
GitHub release-helper tests use a local `gh` peer, Bash and `jq`; they never
contact GitHub or publish.
Native plugin regressions verify the complete skill package without calling a
model. Run `bash scripts/sync-plugins.sh --check` after skill edits.

## Agent evaluations

Use fresh harness sessions and disposable repositories with the locally built
production CLI and the complete [portable skill](../skills/groove/SKILL.md).
Supply the user's task, project policy and assignment context without prescribing
expected agent decisions. Arrange real contention/failures beforehand. Keep the
coordinator outside the checkout and track reservation identifiers independently.
Inspect actual responses, changed paths, retained reservations and restored bytes;
stop all writers before cleanup.

Evaluate both coordinators and assigned workers. Test skill discovery separately
from explicitly supplying the skill. Record the CLI/skill revision, harness/model,
prompt, observed commands, outcome and remaining reservations in the trial or PR
report. Distinguish product regressions, structural skill checks and real agent
trials; none establishes the others. Mark unrun scenarios explicitly. The Rust
suite does not invoke a paid agent service.

## Repository scale

Run `cargo test -p groove-cli --test large_repository_regressions release_waits_for_concurrent_detailed_inspection -- --ignored`
for 200 acquire/release cycles during real-process detailed inspection. It is
ignored in the fast suite because of its runtime; it needs Bash and jq.
Run `cargo test -p groove-cli --all-features --test concurrent_operations -- --ignored --nocapture`
for six independent worker processes and a concurrent administration observer
over 155 seconds. It covers plain/saved/forced release and SIGKILL during saved
release followed by retry, verifies clean and exclusive acquisitions, and parses
every completed response as one API-v2 JSON object.
The restore regression observes Linux file-access events as well as unchanged
inodes and nanosecond mtimes. macOS checks identity only; the scale checker covers
the timing contract on either supported host.

The required suite includes fast directory-inventory, untouched-file and process
regressions. Run `bash scripts/scale-check.sh` for a full lifecycle on a larger
synthetic fixture, or pass `--repo PATH` to check a shared clone without changing
the source repository. It checks reuse, base switches, snapshot bytes, response
bounds, checkout counts and destruction, and writes a timing report.

`cargo run -p groove-bench --locked -- check monorepo-reuse --host` exercises the
versioned heterogeneous benchmark profile. Workspace and monorepo reuse profiles
assert in-place reuse. Assert the reported `recovery` outcome and that untouched
files keep their identity; a timing alone does not reveal a rebuild on a small
fixture. Timing checks complement these observable contracts.

Synthetic fixtures do not exercise real attribute files, symlinks or long packed
history. After `cargo run -p groove-bench --locked -- fetch repository-small`, run
`check repository-small --host` to exercise pinned public repositories with the
same oracles; `repository-large` adds Kubernetes and Linux. Fetching is the only
networked step, and runs never fall back to the network.

## Optional host tests and limits

[The benchmark lab](../benchmarks/snapshots/README.md) owns performance profiles,
scale checks and container/production-CLI opt-ins. Use its `profiles` command to
inspect workloads and requirements. Normal correctness tests do not establish
large-repository latency or concurrent throughput.

`GROOVE_TEST_ROOT`
selects ordinary fixture storage; macOS uses short temporary paths to fit Unix
socket limits. Platform tests establish only the OS/filesystem versions actually
run, not minimum-version compatibility automatically.

SIGKILL is not power loss or storage corruption. Filesystem rejection checks do
not certify flock/fsync. Minimum-kernel VM coverage, malicious same-user path
races, every Git configuration and broad editor/watcher compatibility remain
unestablished. External writers must stop before release/checkpoint.
