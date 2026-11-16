# Security maintenance

Groove coordinates cooperative processes running as the repository owner. It is
not a sandbox: workers inherit the user's environment, credentials and normal
tool configuration, and worktrees share Git administration. Run untrusted code
in a separate sandbox or VM with restricted credentials. See
[operations](operations.md) and the [reporting policy](../SECURITY.md).

## Local security gate

Use the patched toolchain pinned in `rust-toolchain.toml` for development and
release builds. Rust 1.89 remains the compatibility minimum; its CI job is not
the release builder. Keep Rust/Cargo, Git and the operating system patched.
Git 2.40 is a feature minimum, not a security baseline. Lockfile scanners do not
establish the security of those external tools or of code without an advisory.

The recommended setup uses [mise](https://mise.jdx.dev/getting-started.html).
[mise.toml](../mise.toml) is the shared version definition for Gitleaks and the
Cargo security tools, used locally and by the security workflow. Rust stays
pinned separately in `rust-toolchain.toml`; install rustup and a native linker
before installing the Cargo tools. Mise builds those tools with
`cargo install --locked`. Optional fuzz, release and benchmark tooling is documented separately.

From the checkout, after installing mise:

```sh
mise trust
mise install
mise run doctor
mise exec -- bash scripts/security.sh
```

Activate mise in your [shell](https://mise.jdx.dev/getting-started.html#activate-mise)
so tools are available to Git hooks, or run Git through `mise exec -- git ...`.
The hooks and check scripts do not invoke mise or install tools. Manual setup
also works: install the versions declared in `mise.toml` on `PATH`, then run
`bash scripts/check-tools.sh push` and `bash scripts/security.sh`. For Gitleaks,
see the [upstream installation instructions](https://github.com/gitleaks/gitleaks#installing).
The prerequisite check reports the tool that cannot run and explains setup;
it does not enforce versions for manually installed tools.

The pre-commit hook also scans staged additions with Gitleaks, before secrets
enter local history. It does not scan unstaged or untracked content. Findings
are redacted and scan failures block the commit.

The gate is also required by `bash scripts/check.sh push`:

| Tool | Check |
| --- | --- |
| Gitleaks | Scans reachable Git history for secrets, with findings redacted. |
| `cargo audit` | Checks the workspace and isolated fuzz lockfiles for known dependency vulnerabilities and other advisories. |
| `cargo deny` | Checks both dependency graphs against source, license and dependency restrictions. |
| `cargo vet` | Checks recorded workspace dependency reviews and exact-version exemptions. |

Missing tools and failed checks fail the gate. It does not install tools, update
dependencies or record new audits. Gitleaks is a development binary; the hosted
workflow installs it through mise with download checksums pinned in `mise.lock`.
History scanning excludes untracked files and unreachable/deleted history. The initial
Cargo Vet exemptions are described below.

`deny.toml` covers Linux and both macOS targets, including test/build inputs.
Only crates.io registry sources are allowed. Advisory exceptions require a
documented applicability analysis; none are currently configured. Duplicate
versions are reported for review, not treated as vulnerabilities. License
allowances are compliance policy, not security certifications. libFuzzer's NCSA
allowance is restricted to its unpublished development harness.

Default tracing attribute and subscriber formatting/ANSI features are disabled:
Groove uses tracing events and its own subscriber and renderer. Review features,
build scripts, procedural macros and transitive changes on dependency updates.
The draft snapshot adapter adds vendored libgit2 through `git2`, with networking
features disabled and experimental SHA-256 support enabled. This moves repository
parsing into the Groove process and adds native C build inputs. The adapter uses
private configuration and refs; object writes request fsync before native Git
publishes the snapshot ref. The six new exact-version Cargo Vet exemptions are
explicit draft review debt, not completed audits; native/FFI review and capture
cancellation remain prerequisites for merging the replacement.
Compilation can execute dependency code; `--locked` fixes resolution but does
not sandbox a build. Keep secrets out of dependency-review/build environments.

## Dependency updates and review backlog

`supply-chain/` records Cargo Vet policy and minimized imports from Mozilla's
and Google's audit collections. Local Groove packages are first-party code, even when their
versions match crates.io. Current exact-version exemptions are a bootstrap
review backlog, **not completed audits**. They prevent accepting new versions
silently; do not regenerate the baseline to make an update pass.

For an update, inspect the changed manifests, lockfile, sources and features.
Run `cargo vet diff CRATE OLD NEW` or `cargo vet inspect CRATE VERSION`, review
unsafe/ambient capabilities and build-time execution, and record only audits
actually performed. Refresh imported evidence with `cargo vet` on the update
branch and review changes to `supply-chain/`. Run the security and full local
gates. An unavoidable exemption needs an exact version, scope/reason and a
follow-up review; an exemption is not evidence of safety.

Dependabot proposes weekly Cargo and Actions updates. Repository-level alerts
and automatic security fixes must also be enabled in GitHub. Scheduled security
CI is prepared but its jobs require `GROOVE_SECURITY_SCHEDULE_ENABLED=true`;
leave it unset while hosted compute is unavailable. Manual dispatch and release
invocation remain available. A workflow file is not evidence of a successful run.

## Bounded parser fuzzing

The isolated `fuzz/` package is unpublished and has its own audited lockfile.
Its target exercises identifier/deserialization path-safety contracts. It never
executes project commands or operates on a checkout.

```sh
cargo install --locked cargo-fuzz@0.13.2
rustup toolchain install nightly-2026-11-12 --profile minimal
bash scripts/fuzz.sh 60
```

The duration is seconds per target (1..3600). The script prefetches the locked
graph, builds/runs offline with AddressSanitizer, uses disposable corpus copies
and checks that the lockfile is unchanged. libFuzzer artifacts under
`fuzz/artifacts/` are ignored: inspect them, retain a minimal useful corpus input
and add an observable regression for each real bug. Short runs are smoke tests,
not exhaustive fuzzing. The manual Security workflow can opt into these runs.
Real-process ownership, cleanup and crash tests remain necessary; fuzzing pure
parsers cannot establish descriptor/process guarantees.

## Release provenance and inventory

Native release builds use pinned `cargo-auditable@0.7.7` through
`bash scripts/release.sh packages --release --auditable --target TARGET`.
The installed binary contains dependency information for later scanning with
`cargo audit bin PATH`. The ordinary package check does not require this tool.

Each bundle includes `SBOM.cdx.json` and publishes the same file as a target-named
CycloneDX 1.6 asset. It describes the locked CLI's normal and build dependency
graph for that target, excludes dev-only packages, and includes package URLs,
license expressions and registry archive SHA-256 hashes. Build inputs are not
necessarily linked into the executable. This inventory complements embedded
binary metadata; it is not a claim of byte-for-byte reproducibility.

Release builds attest provenance and the SBOM with GitHub OIDC when the
repository is public. Private repositories require a supported GitHub plan and
explicit `GROOVE_PRIVATE_ATTESTATIONS=true`. A private build without that
capability has checksums and inventory but no GitHub attestation. Verify both
archive bytes and provenance as described in [releasing](releasing.md).

Use immutable releases, restricted release-tag creation and protected `main`
when the account's plan supports them. Keep publication credentials scoped to
the publishing job; untrusted PR code must never receive them. Enable secret
scanning/push protection and verify private vulnerability reporting where
available. These are hosted settings, not properties established by checked-in
YAML. Do not change repository visibility or buy a plan to work around a denial.
See [public availability](releasing.md#public-availability) for publication review.

## References

- [RustSec cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit)
- [cargo-deny checks](https://embarkstudios.github.io/cargo-deny/checks/index.html)
- [Cargo Vet criteria and review](https://mozilla.github.io/cargo-vet/)
- [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
- [cargo-auditable](https://github.com/rust-secure-code/cargo-auditable)
- [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
- [SLSA build and source assurance](https://slsa.dev/spec/v1.2/tracks)
