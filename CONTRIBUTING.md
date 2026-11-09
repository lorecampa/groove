# Contributing

Coding agents must also follow [AGENTS.md](AGENTS.md). The checked-in
`rust-toolchain.toml` pins the contributor toolchain; CI tests both the minimum
supported Rust version and current stable.

Groove is a Linux Rust workspace. Install Rust 1.89+ and Git 2.40+; Node/npm are
used only for the optional build-tool compatibility test. See
[testing](docs/testing.md) for local checks and deterministic crash tests.

Keep Git, locking and recovery behavior in `groove-core`; the CLI handles parsing,
JSON and transport. The repository-scoped MCP adapter holds core leases and
exposes tools through the official SDK. Keep unsafe syscall boundaries small and document each safety
argument. Changes to ownership/recovery need real-process regression tests.
Protocol changes must update documentation, schemas and conformance tests together.
Do not introduce implicit task execution, a system-wide daemon, or automatic
result publishing into v1. MCP changes require stdio protocol and crash/recovery
regression coverage. All tests must use disposable repositories.

The workspace uses semantic versioning. Until 1.0, Rust API and wire-contract
changes are reviewed together and documented; protocol v1 has not yet been
formally frozen. Unknown protocol major versions must be rejected. Submit a
focused pull request describing the behavior and the checks you ran.

Before submitting, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

Public APIs require rustdoc, including ownership and error behavior. Keep descriptor leases
alive explicitly; dropping an unprotected lease closes its descriptor. Durable
CLI/MCP protection survives handle closure and requires stopped-writer release.
Document user-visible changes in [CHANGELOG.md](CHANGELOG.md). Use the
[release checklist](docs/releasing.md) before publishing.

Be respectful and focus review on code and behavior. Harassment, discrimination,
and disclosure of private information are not acceptable in project spaces.

## Repository hooks

Run `./scripts/install-hooks.sh` once per clone. The installer sets the local
`core.hooksPath` to the versioned `.githooks` directory and refuses to replace an
existing custom path. Pre-commit checks staged whitespace and Rust formatting;
pre-push runs formatting, Python infrastructure tests, Clippy, the full Rust suite
with default scheduling, doctests and rustdoc. Hooks do not edit or stage files.
Run the same gates directly with `./scripts/check.sh commit` or
`./scripts/check.sh push`. Large pinned-container regressions run separately in
CI; see [the snapshot lab](benchmarks/snapshots/README.md).
