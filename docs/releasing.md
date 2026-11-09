# Public repository and release readiness

Groove is a development preview. Local tests and checked-in workflows do not
establish that remote CI, repository protections, or publishing credentials work.

## Rust practice assessment

The implementation uses owned descriptors and RAII, a non-cloneable lease,
explicit mutable release, small documented unsafe syscall boundaries, a thin CLI,
and integration tests with real Git and processes. These are appropriate choices
for a synchronous Linux library; an async runtime is not required for correctness.
The library now requires public documentation and Debug implementations, includes
a compile-checked usage example, and warns when a lease is discarded unused.

Error codes and protocol modes now use enums. Slot, lease and commit identifiers
are distinct types; parsing and deserialization validate their representation.
The core uses typed slot keys and lifecycle phases. Errors preserve underlying
I/O and parsing sources. Error codes and the new public protocol-mode enums are
non-exhaustive to permit future additions without requiring exhaustive downstream
matches. Before freezing the Rust API, still review the evolution strategy for
public option/result structs and future protocol changes.

Use the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
and [Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html)
as review references. No single checklist certifies a project as production-ready.

## Before making the repository public

- Confirm the MIT/Apache license choice and ownership of all contributed code.
  Both license texts are included in each crate package.
- Enable private vulnerability reporting and verify the reporting channel in
  SECURITY.md. Set a maintainer contact for community conduct reports.
- Enable branch protection/rulesets requiring review and successful CI; enable
  Dependabot alerts and secret scanning where available. Review the complete Git
  history for credentials and private material before changing visibility.
- Run the checked-in CI on GitHub. It covers minimum/current Rust and Git,
  formatting, Clippy, API documentation, regression tests and dependency auditing.
- Confirm maintainer ownership, review availability, and supported-platform policy.
  A formal code of conduct and enforcement contact should be selected before
  inviting a broader contributor community.

CI actions are pinned to full commits, permissions are read-only, checkout does
not persist credentials, and the downloaded minimum Git archive has a pinned
checksum. See [GitHub's workflow security guidance](https://docs.github.com/en/actions/reference/security/secure-use).
The audit job checks the lockfile against [RustSec](https://rustsec.org/); it does
not replace a code review or prove that dependencies have no vulnerabilities.

## Before the first tagged release

1. Resolve the public API decisions above and explicitly approve the v1 wire
   contract. Keep schemas, protocol documentation and conformance tests aligned.
2. Run all checks in CONTRIBUTING.md with both supported compiler bounds. Exercise
   the installed production CLI and examples without `test-hooks`. Review the
   validation limits in testing.md, particularly power-loss and filesystem behavior.
3. Inspect `cargo package -p groove-core --list` and
   `cargo package -p groove-cli --list`; check licenses, README and unwanted files.
   Run `cargo publish -p groove-core --dry-run --locked` from a clean checkout.
   Verify crate-name availability and registry ownership before any upload.
4. If publishing to crates.io, publish/verify the core dependency first, then
   dry-run and publish the CLI. Its registry dependency cannot be validated before
   the matching core version exists. Actual publishing requires maintainer action.
5. Set the release version and dated changelog, tag the reviewed commit, and
   publish installation instructions for that exact version. If distributing
   binaries, define target coverage, checksums and artifact provenance first.

No crates.io publication, GitHub settings changes, tag or release upload is
performed by the local readiness work.

## Agent interfaces

The Rust API, CLI-first durable reservations, optional descriptor transport and
repository-scoped stdio MCP server are implemented. The portable agent skill and
[CLI setup guide](agents.md) do not require MCP registration. MCP holds leases between tool calls and protects exposed workspaces
from automatic reuse after a server disconnect or crash. See [MCP setup](mcp.md)
for installation, configuration, request retry scope and explicit recovery.
Both CLI and MCP registry packages depend on the core package; publish/verify
that dependency first. Additional adapters should follow concrete user needs.
