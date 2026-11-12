# Contributing

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
Do not introduce implicit task execution, a system-wide daemon, or automatic
result publishing into v1. All tests must use disposable repositories.

The workspace uses semantic versioning. Until 1.0, Rust API and wire-contract
changes are reviewed together and documented; protocol v1 has not yet been
formally frozen. Unknown protocol major versions must be rejected. Submit a
focused pull request describing the behavior and the checks you ran.

Before submitting, run:

```sh
bash scripts/check.sh push
```

Public APIs require rustdoc, including ownership and error behavior. Keep descriptor leases
alive explicitly; dropping an unprotected lease closes its descriptor. Durable
protection survives handle closure and requires stopped-writer release.
CLI help owns command syntax, options, defaults and command-specific recovery.
Keep the API contract in installed help and the bundled schema, not duplicated
Markdown command/response tables. Keep the README focused on installation and the
basic workflow; guides explain concepts and operational boundaries. Update help,
schemas and conformance tests with behavior changes, and link to them instead of
copying their reference tables.
Use `config project --example` for the project configuration example and the
benchmark CLI's `profiles` command for its catalog. Keep workflow instructions
in the portable skill and validation commands in `scripts/check.sh`. Do not add
checked-in run reports, readiness claims, test inventories or copies of generated
examples/defaults. Put validation evidence in the relevant PR or CI run. Keep
design rationale, safety boundaries, contributor policy and release history here;
link to executable definitions for reference details.
Document user-visible changes in [CHANGELOG.md](CHANGELOG.md). Use the
[release checklist](docs/releasing.md) before publishing.

Be respectful and focus review on code and behavior. Harassment, discrimination,
and disclosure of private information are not acceptable in project spaces.

## Repository hooks

Run `./scripts/install-hooks.sh` once per clone. The installer sets the local
`core.hooksPath` to the versioned `.githooks` directory and refuses to replace an
existing custom path. Pre-commit checks staged whitespace and Rust formatting;
pre-push runs the dependency security gate, formatting, Clippy, the full Rust suite
(including release, benchmark and hook tests)
with default scheduling, doctests and rustdoc. Hooks do not edit or stage files.
Run the same gates directly with `./scripts/check.sh commit` or
`./scripts/check.sh push`. Benchmarks and large pinned-container regressions are
local-only opt-ins; see [the snapshot lab](benchmarks/snapshots/README.md).

## Releases

See [the release guide](docs/releasing.md) for preparation, account setup,
publication, binary downloads and recovery from a partial release. The
unpublished release tool exposes its commands through `bash scripts/release.sh --help`.
