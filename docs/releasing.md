# Releases

PR merges run CI only. Publishing is triggered
by explicitly pushing a `vVERSION` tag for a reviewed release commit on `main`.
Local checks do not establish that remote CI or publishing credentials work.

## How Rust releases work

[crates.io](https://crates.io/) is Rust's standard public package registry. Cargo
uploads a versioned source archive; users download and compile it with
`cargo install`. Publishing a library also lets other Rust projects depend on it.
See the [Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html).

Groove has two public packages:

| Package | Purpose | Used by |
| --- | --- | --- |
| `groove-core` | Rust library for pools, ownership and recovery | The CLI and Rust applications |
| `groove-cli` | CLI package that builds the `groove` executable | `cargo install --locked groove-cli` |

Publish `groove-core` first, then `groove-cli`, because the CLI depends on the
library. Users installing the CLI get that dependency automatically. The package
name does not need to match the executable name. `groove-release` and
`groove-bench` are internal tools with `publish = false` and are never uploaded.

GitHub Releases complement crates.io with prebuilt binaries for users who do not
want to compile Rust. The same version tag drives both forms of distribution.
Publishing source to crates.io does not change GitHub repository visibility.

## Before the first release

The code and local package checks can be ready before account setup is complete.
Before publishing the first version:

1. Check that the intended source commit's full CI matrix, audit and package jobs
   have passed. Local tests alone do not establish remote CI success.
2. Log into [crates.io](https://crates.io/) with GitHub and verify your email in
   [account settings](https://crates.io/settings/profile).
3. Check that `groove-core` and `groove-cli` are available or owned by you. A missing
   crate page suggests availability, but checking does not reserve its name.
4. Review the source archives, licenses and public documentation. Enable a working
   private security-reporting channel before making the repository public.
5. Prepare the version and dated release notes, commit those changes, and wait for
   that exact release commit's CI. `## Unreleased` is not a dated release entry.
6. Create the `crates-io` GitHub environment for later automated releases. After
   the first manual publication, configure both crates' Trusted Publishers before
   pushing the release tag.

Account verification, tokens, environment settings and Trusted Publishers are not
created by committing this repository. The commands below keep preparation,
publication and tag pushing explicit.

## Release infrastructure

The two public crates share one version and one Git tag. Additional internal
workspace tools must declare `publish = false` and are excluded from uploads.
The workspace dependency on
`groove-core` is centralized in the root Cargo.toml. The
[`release.sh`](../scripts/release.sh) entry point runs the unpublished Rust
[`groove-release`](../tools/release/Cargo.toml) tool, which checks package versions, dependency
requirements, lockfile entries, dated release notes, tag identity and ancestry.
It uses the repository's pinned Rust toolchain and Bash. Registry availability
checks use `curl`; JSON, TOML, semantic versions, archives and checksums are
handled in Rust. Release tests run with `cargo test --package groove-release --locked`.

The tag-triggered [`publish.yml`](../.github/workflows/publish.yml) workflow:

1. Validates the tag and ensures the tagged commit is on `main`.
2. Runs the existing CI matrix, audit and package checks through a reusable CI
   workflow. Builds production binaries separately, with no `test-hooks` feature.
3. Obtains a short-lived crates.io token through Trusted Publishing. Publishes
   `groove-core`, waits for it to appear in the registry, then publishes
   `groove-cli`.
4. Installs the exact published CLI version from crates.io in a fresh
   installation root. Checks its version and exercises CLI acquisition and
   release in a disposable repository.
5. Creates the GitHub Release with the changelog section and binary assets only
   after publishing and installation checks pass. Prerelease versions such as
   `0.2.0-rc.1` produce GitHub prereleases; they are still publicly downloadable.

Active release runs are serialized and are not cancelled by a newer tag. Push
one release tag at a time; GitHub may replace an older pending run if multiple
versions are queued. Actions are
pinned to commit hashes. Only the publishing job receives an OIDC permission;
only the GitHub Release job receives repository write permission.

CI packages each crate, extracts it, and builds all included targets. Until the
new core version is published, package checks use a Cargo configuration patch
to resolve core locally. Packaging resolves its metadata from the checkout;
**compilation resolves core exclusively from its extracted `.crate` contents**.
The real publication path has no patches and performs Cargo's normal package
build verification against crates.io. CI package checks therefore verify source
completeness without claiming to verify an unpublished registry dependency.

## One-time account setup

These settings are external to the repository and are not created by the scripts:

- Create a crates.io account and verify its email address. Check availability and ownership of
  `groove-core` and `groove-cli`; availability is not reserved by
  this repository. Add any intended maintainers as crate owners after publication.
- Enable GitHub Actions. Create a `crates-io` environment. The publishing workflow
  must be allowed to deploy version tags to it. Optional required reviewers can
  provide an additional release approval if your GitHub plan supports them.
- Protect `main` with required checks and protect `v*` tags against unauthorized
  creation, updates and deletion. Ensure the final release job can create GitHub
  Releases using its job-scoped `contents: write` permission.
- After bootstrapping each crate, configure its crates.io Trusted Publisher:
  owner `lorecampa`, repository `groove`, workflow filename **`publish.yml`**,
  environment **`crates-io`**. Update these identities if the repository moves.

See the [crates.io Trusted Publishing documentation](https://crates.io/docs/trusted-publishing)
and the [official authentication action](https://github.com/rust-lang/crates-io-auth-action).
No long-lived crates.io token is needed in GitHub repository secrets.

The GitHub repository can remain private, but publishing a crate exposes its
packaged source publicly. Binary releases in a private GitHub repository remain
subject to repository access permissions.

## Prepare a release PR

Start from a clean, current checkout. Record user-visible changes under
`## Unreleased`, then prepare the intended version:

```sh
git switch -c release/0.1.0
bash scripts/release.sh prepare 0.1.0
git diff
```

Preparation updates the shared version, internal dependency requirement and
Cargo.lock, then moves unreleased notes under `## [0.1.0] - YYYY-MM-DD`.
It permits the current version for the initial release, rejects downgrades and
existing changelog entries, and rolls back its edits if Cargo fails. It never
commits, tags, pushes or uploads. Review the date and notes before merging;
the preparation date may need changing if the actual release happens later.

Run the contributor checks and package verification:

```sh
./scripts/check.sh push
bash scripts/release.sh packages --allow-dirty
```

`--allow-dirty` is only for development and pre-commit verification. Publishing
requires a clean tree. Commit the version, lockfile and changelog changes, open
a release PR, and merge after review and successful CI.

## First publication

Bootstrap the first version of each crate manually before relying on Trusted
Publishing. This step makes the packages public. Use the same pinned toolchain,
clean release commit and tag that the workflow will use:

```sh
git switch main
git pull --ff-only
git tag -a v0.1.0 -m 'Groove 0.1.0'
bash scripts/release.sh validate --tag v0.1.0 --branch origin/main
cargo login
bash scripts/release.sh publish --tag v0.1.0 --execute
cargo logout
```

Create a short-lived [crates.io API token](https://crates.io/settings/tokens) that
permits creating/publishing these two crates. Enter it directly at the local
`cargo login` prompt; do not put it in chat, command history or the repository.
The release script calls standard `cargo publish` in dependency order, checks
registry propagation/checksums, then installs and smoke-tests the published CLI.
`cargo logout` removes the locally stored credential; revoke the bootstrap token
in crates.io when done.

Configure Trusted Publishing for both crates as described above
after they exist. Then push the existing tag to run CI, verify the already
published checksums, and create the binary release:

```sh
git push origin refs/tags/v0.1.0
```

Pushing the first tag before configuring Trusted Publishing causes the publishing
job to fail authentication. Configure the publishers first, then push the tag; if
needed, fix the external configuration and rerun that tag's failed workflow.

After registry publication, users can install the exact release with:

```sh
cargo install --locked --version '=0.1.0' groove-cli
groove --version
```

They need Linux, Git 2.40+ and a supported Rust compiler to build from source.
For installation without Rust, use the prebuilt binary described below.

Publishing a version is permanent: its contents cannot be overwritten. Fixes
need a new version. Even the initial alpha upload exposes its packaged source
publicly; use a reviewable release commit rather than publishing to reserve names.

## Subsequent releases

After merging the release PR, check out its exact commit on `main`, create the
matching annotated tag and push only that tag:

```sh
git switch main
git pull --ff-only
git tag -a v0.2.0 -m 'Groove 0.2.0'
bash scripts/release.sh validate --tag v0.2.0 --branch origin/main
git push origin refs/tags/v0.2.0
```

Pushing `main` or merging the release PR alone does not publish anything. Wait
for the Release workflow to complete, then verify its crates.io links and GitHub
assets. Users install an exact release with:

```sh
cargo install --locked --version '=0.2.0' groove-cli
```

## Binary downloads

Native binary targets are **x86_64 GNU/Linux** (Ubuntu 24.04, glibc 2.39+),
**Apple Silicon macOS** (`aarch64-apple-darwin`) and **Intel macOS**
(`x86_64-apple-darwin`). Darwin builds set `MACOSX_DEPLOYMENT_TARGET=13.0`
and are built/tested on macOS 15; minimum-version runtime coverage is not established.
Each archive contains
`groove`, licenses, documentation and the portable agent skill.
It is built from the extracted packages and smoke-tested on the release runner.
Git 2.40+ and the storage requirements in [operations](operations.md) still apply.
Other CPU architectures, operating systems and musl-based distributions are not
covered by this binary job. Musl builds are untested; adding that target requires
separate validation. Users on older glibc systems can build from source with
Cargo on their own supported Linux environment.

Each target contributes `groove-VERSION-TARGET.tar.gz`,
`groove-VERSION-TARGET.manifest.json` and `SHA256SUMS-TARGET`. The announcement
job combines their checksums into `SHA256SUMS`. Distinct filenames prevent
parallel jobs from overwriting another platform's metadata. The manifest records
the source commit, Rust compiler, target, build OS/libc, dirty-tree status and
archive checksum; it is build metadata, not a signed attestation. On macOS the
`libSystem` version field identifies the host macOS release.

Download the archive, manifest and `SHA256SUMS-TARGET` for your target. Before
extracting, run `sha256sum --check SHA256SUMS-TARGET` on Linux or
`shasum -a 256 --check SHA256SUMS-TARGET` on macOS. Place `groove` on your PATH.
Darwin binaries are not notarized release applications; signing/notarization
is outside the current binary pipeline.

## Recovering a partial release

Crate uploads are separate operations and cannot be rolled back as a group.
If a run fails after uploading core or CLI, fix an external condition such as
publisher configuration or a registry outage, then **rerun the workflow for the
same tag**. It rebuilds packages and skips an existing version only when the
registry checksum exactly matches the locally packaged archive. A yanked version
or checksum mismatch stops the release. Use the pinned toolchain for bootstrap
and retries so package generation remains consistent.

If uploading returns an ambiguous failure, rerun after checking crates.io; an
upload may have succeeded before the connection failed. The propagation wait is
bounded to five minutes per crate. The final installation smoke test and GitHub
announcement are also rerunnable. Existing GitHub release assets are replaced
from the successful run's verified build artifacts.

Do not move or reuse a release tag to fix source code. Prepare a new version if
code needs changing. Published versions are immutable; `cargo yank` only prevents
new dependency resolutions and does not erase existing downloads. See the
[Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html).

## Release review

Before the first publication, review public API/wire compatibility, supported
platforms, package contents, license ownership and repository history. Confirm
crate ownership, maintainer contacts, vulnerability reporting and required CI
checks. Account settings and release approval remain maintainer responsibilities.
See [contributing](../CONTRIBUTING.md), [security](../SECURITY.md) and
[validation limits](testing.md#limits).
