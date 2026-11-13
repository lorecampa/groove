# Contributing

Bug reports, documentation fixes and code contributions are welcome. Search
existing issues and pull requests before starting. For a substantial feature or
compatibility change, open an issue to discuss the problem and approach first.
Small fixes can go straight to a pull request. Report vulnerabilities through
[SECURITY.md](SECURITY.md).

Coding agents must also follow [AGENTS.md](AGENTS.md). The checked-in
`rust-toolchain.toml` pins the contributor toolchain. Automatic GitHub CI is
disabled while hosted compute is unavailable; run the required local checks.
The optional manual/release CI matrix covers minimum Rust and current stable.

Groove is a Linux/macOS Rust workspace. Rust 1.89 and Git 2.40 are compatibility
minimums; use the patched contributor toolchain and a security-maintained Git.
Install the pinned audit/policy tools from [security maintenance](docs/security.md).
Node/npm are
used only for the optional build-tool compatibility test. See
[testing](docs/testing.md) for local checks and deterministic crash tests.
macOS requires local APFS storage. OS-specific operations belong in the core's
`platform` and `quiescence` backends; ownership and recovery policy stay shared.

Repository tooling and test helpers use Rust or Bash. Keep development-only Rust
tools in workspace packages marked `publish = false`; avoid adding a Python
runtime dependency.

Keep Git, locking and recovery behavior in `groove-core`; the CLI handles parsing,
rendering (human text and `--json`) and exit codes, with one handler per command
under `crates/groove-cli/src/commands/` and long help in `crates/groove-cli/help/`.
Keep unsafe syscall boundaries small and document each safety
argument. Changes to ownership/recovery need real-process regression tests.
Protocol changes must update CLI help, schemas and conformance tests together.
For experimental features, follow the [implementation and testing
procedure](docs/configuration.md#implementing-an-experimental-feature) and
[stabilization cleanup](docs/configuration.md#stabilization-and-cleanup).
Do not introduce implicit task execution, a system-wide daemon, or automatic
result publishing into v1. All tests must use disposable repositories.

The workspace uses semantic versioning. Until 1.0, Rust API and wire-contract
changes are reviewed together and documented; protocol v1 has not yet been
formally frozen. Unknown protocol major versions must be rejected. Submit a
focused pull request describing the behavior and the checks you ran.

## Coding and documentation style

- Use the pinned toolchain's default rustfmt style (`cargo fmt --all`) and keep
  Clippy warning-free. Follow [.editorconfig](.editorconfig) for other files.
- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
  for public APIs and the [Rust CLI book](https://rust-cli.github.io/book/) for
  command-line conventions, preserving Groove's JSON response contract.
- Follow nearby naming and module structure: `snake_case` functions/modules,
  `UpperCamelCase` types/traits and `SCREAMING_SNAKE_CASE` constants. Prefer small,
  explicit functions and types that express ownership and valid states.
- Propagate expected failures with typed errors and preserve their underlying
  sources. Reserve `expect` for explained internal invariants. Give every unsafe
  block a local `SAFETY` comment explaining why its operations are valid.
- Explain intent and constraints in comments; document public ownership, errors
  and useful examples in rustdoc. Write documentation and diagnostics in clear
  English, using existing command and error terminology.
- Keep refactors and formatting unrelated to the behavior change separate.
  Review new dependencies, features, lockfile changes and minimum Rust support.
- Choose regression coverage by observable behavior, following
  [the testing guide](docs/testing.md#writing-tests-intent-and-observable-contracts).
  Keep ownership and recovery guarantees in real-process tests.

## Commits

Use a short, imperative English subject describing the result, without a trailing
period. Aim for 72 characters or fewer; clarity matters more than a strict count.
An area prefix such as `core:` or `cli:` is optional. For example:

```text
Preserve reservations when snapshot capture fails
cli: Explain how to retry a saved release
Document the local validation workflow
```

For a nontrivial change, leave a blank line after the subject and explain the
problem, why this approach fixes it, and any compatibility or recovery effects.
Include issue references where useful. The message should make sense without
the PR conversation. Conventional Commits (`feat:`, `fix:`, and similar type
prefixes) are not required; releases use explicit versions and reviewed changelog
entries.

Keep each commit a coherent change that builds and passes its relevant checks,
with the tests and documentation needed for that change. Separate independent
refactors from fixes. Review follow-up commits are welcome while a PR is open;
before merge, consolidate temporary `fixup`/`WIP` commits or ask a maintainer for
help. Commit wording is reviewed by people; the hooks check formatting and
validation rather than enforcing a message grammar.

## Pull requests and review

Create a topic branch from `main` and open the PR against `main`. Descriptive
branch names such as `fix/snapshot-capture` or `docs/contributing` are useful.
Use a draft PR when the approach or implementation still needs discussion.

Use the same subject style as commits for the PR title. Fill in the
[PR template](.github/pull_request_template.md) for a reviewer who has not read
the issue or prior discussion:

- Lead with the concrete problem and resulting behavior. Include a small
  before/after example when it helps, and link related issues (`Closes #123`
  only when the PR resolves the issue).
- Explain significant tradeoffs and point out areas needing careful review.
  Keep the description proportional to the change; a small fix can be brief.
- Report exact validation commands, observed results, failures and skipped
  coverage. For docs-only changes, state what was checked and why runtime tests
  were unnecessary. Never present planned checks as completed checks.
- Describe API, CLI/JSON, storage and supported-environment compatibility effects,
  including migration or recovery steps. Update help, schemas, conformance tests
  and the changelog when applicable.

Before marking a code change ready for merge, run the local pre-push gate below.
Hosted CI is not automatic; absence of a failed GitHub check is not validation.
Maintainers review correctness, safety, compatibility, tests and clarity before
merging. Reply to review feedback and keep the PR description current as the
implementation changes. Maintainers may squash a small PR or preserve a useful
commit series; contributors do not need to rewrite history merely to open a PR.

These conventions draw on
[Cargo's contribution workflow](https://doc.crates.io/contrib/process/working-on-cargo.html),
[Git's commit-message guidance](https://github.com/git/git/blob/master/Documentation/SubmittingPatches#describe-changes)
and [clap's atomic-commit guidance](https://github.com/clap-rs/clap/blob/master/CONTRIBUTING.md#preparing-the-pr),
adapted to Groove's existing history and local validation requirements.

## Validation

Before submitting code changes, run:

```sh
bash scripts/check.sh push
```

This runs whitespace, dependency security, workspace/fuzz formatting, Clippy,
the default-scheduled Rust suite, doctests and rustdoc checks. See
[testing](docs/testing.md#required-checks) for the expanded commands and optional
platform/security checks.

Public APIs require rustdoc, including ownership and error behavior. Keep descriptor leases
alive explicitly; dropping an unprotected lease closes its descriptor. Durable
protection survives handle closure and requires stopped-writer release.
CLI help owns command syntax, options, defaults and command-specific recovery.
Keep the API contract in installed help and the bundled schema, not duplicated
Markdown command/response tables. Keep the README focused on installation and the
basic workflow; guides explain concepts and operational boundaries. Update help,
schemas and conformance tests with behavior changes, and link to them instead of
copying their reference tables.
Use the benchmark CLI's `profiles` command for its catalog. Keep workflow instructions
in the portable skill and validation commands in `scripts/check.sh`. Root help
owns global-flag and output rules; command help covers command-specific behavior.
Keep prose concrete and remove filler and repetition. Do not add
checked-in run reports, readiness claims, test inventories or copies of generated
examples/defaults. Put validation evidence in the relevant PR or CI run. Keep
design rationale, safety boundaries, contributor policy and release history here;
link to executable definitions for reference details.
Document user-visible changes in [CHANGELOG.md](CHANGELOG.md). Use the
[release checklist](docs/releasing.md) before publishing.

Be respectful and focus review on code and behavior. Harassment, discrimination,
and disclosure of private information are not acceptable in project spaces.

## Git hooks

Git hooks are scripts Git runs before operations such as committing or pushing.
Groove's contributor hooks run local checks; a failed check stops the operation.
They do not edit or stage files, and installing Groove does not enable them.

Run `./scripts/install-hooks.sh` once per clone to enable the hooks. The installer
sets the local `core.hooksPath` to the checked-in `.githooks` directory and refuses
to replace an existing custom path.

- `pre-commit` checks staged whitespace and workspace Rust formatting.
- `pre-push` runs the security gate, formatting, Clippy, the full Rust suite
  (including release, benchmark and hook tests) with default scheduling,
  doctests and rustdoc.

Run the same checks directly with `./scripts/check.sh commit` or
`./scripts/check.sh push`. Benchmarks and large pinned-container regressions are
local-only opt-ins; see [the snapshot lab](benchmarks/snapshots/README.md).

## Releases

See [the release guide](docs/releasing.md) for preparation, account setup,
publication, binary downloads and recovery from a partial release. The
unpublished release tool exposes its commands through `bash scripts/release.sh --help`.
