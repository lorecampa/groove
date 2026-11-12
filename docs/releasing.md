# Releases

Groove is a Linux/macOS alpha. Keep one version across both public packages and
GitHub assets. Routine pushes and PRs do not run hosted CI while compute is
unavailable. Reviewed `vVERSION` tags explicitly invoke the release pipeline,
including its CI matrix, and therefore still require hosted compute.
The repository can remain private throughout preparation and rehearsal.

GitHub Releases work independently of crates.io. Registry publication is **off by
default**; it requires the repository variable `GROOVE_PUBLISH_CRATES_IO=true`
and the separate account setup below. Do not enable it merely to test releases.
Cargo is the only package-manager route for now; there are no Homebrew/npm jobs.

## Prepare a release

From a clean checkout, collect user-visible changes under `## Unreleased`, then:

```sh
git switch -c release/0.1.0
bash scripts/release.sh prepare 0.1.0
git diff
bash scripts/check.sh push
bash scripts/release.sh packages --allow-dirty
```

Preparation updates the workspace version, internal dependency, Cargo.lock and
dated changelog. It does not commit, tag, push or upload. It supports the current
version for the first release and rejects downgrades or duplicate release entries.
Review the date if the release is delayed. Commit the changes, submit a release
PR and merge after review and the required local checks. When hosted compute is
available, run the optional CI matrix as well. The actual release pipeline still
requires its remote checks. Version, notes, source and binaries must agree.

Then select the reviewed commit on main and explicitly push its annotated tag:

```sh
git switch main
git pull --ff-only
git tag -a v0.1.0 -m 'Groove 0.1.0'
bash scripts/release.sh validate --tag v0.1.0 --branch origin/main
git push origin refs/tags/v0.1.0
```

Use actual reviewed versions, not these example tags, for rehearsals. Never move
or reuse a released tag to fix code; prepare a new version.

## GitHub release pipeline

The tag-triggered [Release workflow](../.github/workflows/publish.yml):

1. Validates version, dated notes, clean tagged commit and main ancestry.
2. Runs the reusable CI matrix against the exact tag, including package and audit
   jobs, while building native production archives without `test-hooks`.
3. Optionally publishes crates when explicitly enabled. With the default disabled
   setting, no registry credentials or `crates-io` environment are required.
4. Assembles release notes, platform links and a tag-pinned Cargo installation
   command. Checks downloaded build artifacts and creates a GitHub **draft**.
5. Uploads missing assets, downloads them again and compares bytes. Only verified
   stable tag-push releases are published automatically. Candidate tags containing
   a prerelease suffix stay as verified drafts marked as prereleases.

The helper [github-release.sh](../scripts/github-release.sh) uses Bash, GitHub CLI,
`jq` and `cmp`. The workflow validates tags and checksums before invoking it.
Authentication, API and download failures stop the run rather than being treated
as an absent release. Permissions are job-scoped: only the final job can write
GitHub releases, and only optional registry publication can request an OIDC token.
Release runs are serialized without cancelling an active run. Submit one tag at
a time: GitHub concurrency can replace an older pending run.

## Private rehearsal

Prepare a distinct candidate, such as `0.1.0-rc.1`, through the same release PR
process. Push that candidate tag: the full pipeline verifies it and leaves a
**draft**, even in a public repository. Repository visibility is never changed.

The Release workflow also supports manual dispatch with an existing tag:

```sh
gh workflow run publish.yml --ref main -f tag=v0.1.0-rc.1
```

Manual dispatch always leaves a draft and skips crates.io publication, regardless
of the repository variable. It reruns the complete checks and builds against the
selected tag. Do not dispatch another tag while a release run is pending.

A rehearsal is successful only after remote checks pass and you download, verify
and exercise the archives. Local success is not evidence that GitHub runners,
permissions or account billing work. Private Actions uses your account's usage
allowance; resolve runner/billing failures before claiming remote validation.

To publish an already verified draft, review its notes/assets and use GitHub's
release UI. This is an explicit maintainer action. Normal stable releases use the
stable tag-push path. A draft's visibility is separate from repository visibility:
releases in a private repository are visible only to authorized readers.
See [the launch checklist](launch.md) before making the repository public.

## Retry and recover

Rerun failed jobs for the **same tag**, retaining the original successful binary
job artifacts where possible. Existing assets are never replaced automatically.

- An incomplete draft resumes by uploading missing assets only when existing
  assets match the local build artifacts byte for byte.
- A published release is read-only to the helper. An identical complete asset set
  passes verification without editing notes, assets or publication state.
- A mismatch or unexpected asset stops the run, preserving the release. Rebuilt
  archives/build metadata are not guaranteed to be byte-reproducible across runs.
  Investigate instead of treating different bytes as a harmless retry.
- An upload or final comparison failure leaves a draft. Inspect it before retrying.
  If a draft must be rebuilt, explicitly review/remove that draft's obsolete assets
  before retrying; never do that to published assets. Preserve logs and do not move
  the tag. Use a new version when published code/artifacts need correction.

Enable GitHub release immutability where available after validating this draft-first
flow. Even without the setting, the helper refuses to overwrite published assets.

## Binary downloads

Current targets:

| Target | Runtime scope |
| --- | --- |
| `x86_64-unknown-linux-gnu` | Linux x86_64; built on Ubuntu 24.04, glibc 2.39+ |
| `aarch64-apple-darwin` | Apple Silicon macOS; deployment target 13.0 |
| `x86_64-apple-darwin` | Intel macOS; deployment target 13.0 |

Darwin builds/tests run on macOS 15; minimum macOS 13 runtime behavior is not
established. Local APFS is required. macOS binaries are not notarized. Other
architectures and musl builds are not covered. Older Linux libc users can build
with Cargo in their supported environment. All installations still need Git 2.40+.

Each archive includes `groove`, licenses, guides and the full portable agent skill.
Download `groove-VERSION-TARGET.tar.gz`, its `.manifest.json` and
`SHA256SUMS-TARGET` from the same release. Before extracting:

```sh
# Linux, in the download directory
sha256sum --check SHA256SUMS-TARGET
# macOS
shasum -a 256 --check SHA256SUMS-TARGET
```

Use the actual target name. Place the extracted `groove` on PATH and verify
`groove --version`. The release also provides combined `SHA256SUMS`. Manifests
record source, compiler, target and build platform; checksums detect corruption
but are not independent signatures or attestations.

## Cargo installation before crates.io

Once the corresponding GitHub tag exists:

```sh
cargo install --git https://github.com/lorecampa/groove --tag v0.1.0 --locked groove-cli
```

This compiles from the selected Git revision. Private repositories require
existing authenticated Git access. Install the skill from that same revision;
see [installation](install.md). Source archives supplied by GitHub are source,
not the native binary archives produced by this workflow.

## Deferred crates.io setup

Two packages share the workspace version: `groove-core` (library) and
`groove-cli` (installs `groove`). Internal tools declare `publish = false`.
Publish core before CLI; users installing CLI resolve core automatically.
Publishing crate source makes it public even if the GitHub repository is private.

When ready, separately:

1. Create/verify the crates.io account and intended package ownership. Checking
   name availability does not reserve it. Review source archives and licenses.
2. From the exact clean reviewed release commit, create its local tag and validate
   it. Bootstrap both packages manually using a short-lived token:

   ```sh
   cargo login
   bash scripts/release.sh publish --tag v0.1.0 --execute
   cargo logout
   ```

   Use the actual release tag. Enter the token only at the local login prompt;
   revoke it after bootstrap. The tool checks propagation/checksums and installs
   and smoke-tests the exact registry version.
3. Create the `crates-io` GitHub environment and configure each crate's Trusted
   Publisher: owner `lorecampa`, repository `groove`, workflow `publish.yml`,
   environment `crates-io`. Review tag deployment restrictions/approvals supported
   by the account. No long-lived registry token belongs in Actions secrets.
4. Set `GROOVE_PUBLISH_CRATES_IO=true` only after setup. Subsequent stable tag pushes
   publish in dependency order before the GitHub release is announced. Manual
   rehearsals and prerelease tag pushes skip registry publication.
5. Verify registry installation, then update user-facing installation instructions
   to `cargo install --locked groove-cli`.

The Rust tool supports checksum-verified partial-publication retries: it skips an
existing version only when its registry checksum exactly matches the packaged
archive. Yanked/conflicting versions stop the release. Do not move tags or change
published versions; fixes require a new version. See the
[Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html)
and [Trusted Publishing](https://crates.io/docs/trusted-publishing).

## Package checks and tooling

The unpublished Rust tool behind `scripts/release.sh` checks manifests, lockfiles,
notes, tags and ancestry. Run `cargo test -p groove-release --locked`; GitHub helper
tests use a local CLI peer with `jq` and never upload. CI extracts `.crate` contents
and compiles all included targets. Before a new core exists in the registry, a
local Cargo patch resolves it exclusively from its extracted archive. Real registry
publication uses normal Cargo verification without patches.

Each check uses fresh compiler output directories for its extracted sources, so
an earlier archive at the same version cannot supply stale library artifacts.
Only smoke-tested executables are copied to `target/package-check` for bundling.

Run `bash scripts/release.sh packages --allow-dirty` during development. Clean
release checks omit that flag. See [testing](testing.md) for full gates and limits.
