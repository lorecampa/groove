# Working on Groove

Read [CONTRIBUTING.md](CONTRIBUTING.md) before editing. Use
[architecture](docs/architecture.md) for ownership and locking changes,
command `--help`, CLI help sources and [schemas](schemas/) for CLI changes,
[snapshots](docs/snapshots.md) for checkpoints, and
[testing](docs/testing.md) for regression coverage and known limitations.
Follow its test-writing practices: assert intent and observable contracts, choose
the smallest suitable test layer, and use the documented libraries and process harness.
Use [execution](docs/execution.md) for managed-worker teardown and orphan recovery.
[The portable skill](skills/groove/SKILL.md) describes using the product;
this file describes developing it.

## Boundaries and invariants

- Keep Git, filesystem, locking, snapshots and recovery in `groove-core`.
  Keep argument parsing, rendering and subscriber setup in `groove-cli`.
  Core code never prints or installs global logging.
- A lease owns an OS descriptor. Never infer ownership from a PID, timestamp,
  metadata record or lease ID. Never unlink or replace permanent lock files.
- Durable protection survives handle/process exit. Preserve results and stop all
  writers before explicit release. Never expire reservations or bypass protection
  to make a test pass. Validate Groove-owned paths before destructive cleanup.
- Use owned/borrowed descriptor types, small unsafe boundaries and a local
  `SAFETY` argument for every unsafe block. Account for descriptor inheritance,
  surviving children, interruption and error paths.
- Run Git with argument arrays, controlled environment, bounded execution and
  drained/captured output. Do not introduce shell interpolation or interactive Git.
- Use typed identifiers and errors; propagate expected failures with `?` and retain
  underlying error sources. Reserve `expect` for explained internal invariants.
  Public APIs need rustdoc, error/safety behavior and useful examples.
- Stdout is human-readable by default. With `--json` it
  carries exactly one versioned JSON response, including help and errors. Human
  errors go to stderr; diagnostics go through tracing. Preserve exit categories
  and schema compatibility. Compute exit codes from typed results, never by
  inspecting rendered JSON.
- Every CLI command and option needs useful help. Include examples, defaults,
  destructive effects and recovery rules where relevant. Keep README concise;
  teach detailed usage through `--help` at each command level.
- Update schemas, installed help and conformance tests together when wire behavior
  changes. Record user-visible changes in `CHANGELOG.md`.
- Use disposable repositories in tests. Ownership/recovery changes require real
  process regressions, including failure paths. Never benchmark or destructively
  test against a developer's working repository.
- Respect the declared minimum Rust version. Review new dependencies, feature
  flags and lockfile changes; do not weaken lints or checks to hide a failure.
- Use Rust or Bash for repository tooling and test helpers. Internal workspace
  tools must declare `publish = false`; do not add a Python runtime dependency.
- Preserve unrelated working-tree changes. Do not add implicit task execution,
  a global daemon, automatic publishing or unrelated abstractions.

Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
for public APIs and the [Rust CLI book](https://rust-cli.github.io/book/) for CLI
conventions, preserving Groove's documented JSON response contract under `--json`.

## Validation

The pinned toolchain includes rustfmt and Clippy. Before completing code changes:

```sh
bash scripts/check.sh push
```

Run focused checks while iterating. CI also tests current stable and minimum Git.
Report the exact checks run and any failures or skipped coverage; never describe
serial-only results as evidence that concurrent/default scheduling passes.
