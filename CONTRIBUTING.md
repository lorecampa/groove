# Contributing

Coding agents must also follow [AGENTS.md](AGENTS.md). The checked-in
`rust-toolchain.toml` pins the contributor toolchain; CI tests both the minimum
supported Rust version and current stable.

Groove is a Linux/macOS Rust workspace. Install Rust 1.89+ and Git 2.40+; Node/npm are
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
Protocol changes must update documentation, schemas and conformance tests together.
Do not introduce implicit task execution, a system-wide daemon, or automatic
result publishing into v1. All tests must use disposable repositories.

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
protection survives handle closure and requires stopped-writer release.
Document user-visible changes in [CHANGELOG.md](CHANGELOG.md). Use the
[release checklist](docs/releasing.md) before publishing.

Be respectful and focus review on code and behavior. Harassment, discrimination,
and disclosure of private information are not acceptable in project spaces.

## Repository hooks

Run `./scripts/install-hooks.sh` once per clone. The installer sets the local
`core.hooksPath` to the versioned `.githooks` directory and refuses to replace an
existing custom path. Pre-commit checks staged whitespace and Rust formatting;
pre-push runs formatting, Clippy, the full Rust suite
(including release, benchmark and hook tests)
with default scheduling, doctests and rustdoc. Hooks do not edit or stage files.
Run the same gates directly with `./scripts/check.sh commit` or
`./scripts/check.sh push`. Benchmarks and large pinned-container regressions are
local-only opt-ins; see [the snapshot lab](benchmarks/snapshots/README.md).

## Releases

PR merges run checks only. A maintainer publishes by pushing a `vVERSION` tag for
a reviewed release commit on `main`. Prepare the version, internal dependency,
lockfile and dated changelog with `bash scripts/release.sh prepare VERSION`
from a clean checkout, then submit those changes as a release PR. This command
does not commit, tag or publish. Release tooling is an unpublished Rust workspace
package with a Bash entry point; registry checks also require `curl`.

Run `cargo test --package groove-release --locked` for the release
regressions and `bash scripts/release.sh packages --allow-dirty` to build the
extracted package contents while developing. CI requires a clean checkout.
See [the release guide](docs/releasing.md) for account setup, first publication,
tagging, binary downloads and recovery from a partial release.
