# Private rehearsal and public launch

The repository can stay private throughout implementation and release rehearsal.
GitHub releases inherit repository access; creating a release does not change
visibility. Crates.io publishing is separate and exposes packaged source even
when the GitHub repository is private. It is disabled by default here.

## Before pushing the candidate

- Review tracked files and all history intended for publication for credentials,
  private documents, internal URLs, accidental artifacts and license ownership.
  A clean current tree alone is not a history review. Rotate exposed credentials;
  deleting their latest copy is insufficient.
- Review licenses, packaged source/archive contents and third-party notices.
- Run `bash scripts/check.sh push` and
  `bash scripts/release.sh packages --allow-dirty` while developing. All ownership
  regressions use disposable repositories and default test scheduling.
- Review Actions token permissions, external-PR execution, branch/tag protections
  available on the account's plan, and who can push release tags. No privileged
  workflow should execute untrusted fork code with secrets.
- Review historical Actions logs/artifacts before visibility changes: those logs
  can become public too. Inspect locally without publishing secret findings.
- Confirm Actions billing/budget permits the private Linux/macOS matrix. Account
  settings are external prerequisites; checked-in YAML does not enable runners.

## Rehearse privately

1. Push reviewed changes to a private branch/PR after the required local checks.
   Routine hosted CI is disabled while compute is unavailable; this does not
   block ordinary merges. Obtain remote evidence when deliberately rehearsing
   a release after hosted compute becomes available.
2. Prepare a candidate prerelease such as `0.1.0-rc.1` on a reviewed commit using
   the release guide. Push its tag when ready: candidate tags leave verified
   drafts and skip crates.io, while repository visibility stays unchanged.
3. Use the manual Release workflow with an existing tag to rehearse again.
   Manual dispatch is always draft-only. Stable tag pushes automatically publish
   the verified GitHub release; do not use a stable tag for a draft rehearsal.
4. Verify the exact commit's Linux/macOS checks, downloaded asset checksums,
   version, a disposable acquire/finish/restore cycle, and retry behavior.
5. Install the matching complete skill and run/record the harness evaluations.
   Authenticated success does not establish anonymous installation.

Do not manufacture production v0.1.0 tags for experimentation or move a tag after
publication. Use distinct candidate versions and keep an explicit record of any
unrun platform/harness checks. See [releases](releasing.md) for exact commands.

## Public launch (explicit maintainer action)

- Approve the source/history/log review and the candidate's validation results.
- Change repository visibility only when ready; this is never done by a script.
- Enable GitHub private vulnerability reporting and verify the form linked from
  SECURITY.md. This feature is for public repositories; it cannot be verified as
  an anonymous reporting path while the repository is private.
- Check README links, release downloads, Cargo Git installation and skill fetching
  without authentication from a fresh environment.
- Publish the reviewed launch release, check its release notes/assets, and verify
  that existing private rehearsal releases do not confuse the public release list.
- Leave crates.io publication disabled until its separate setup is complete.

Background: [GitHub visibility changes](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/managing-repository-settings/setting-repository-visibility),
[release access](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases),
[private reporting](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/report-privately),
and [Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions).

## Local validation recorded 2026-11-12

The implementation was checked on Linux with pinned Rust 1.89.0. These checks
passed with default Cargo test scheduling:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
git diff --check
cargo test -p groove-release --locked
bash scripts/release.sh packages --allow-dirty
```

Workflow validation passed with actionlint 1.7.12. The skill-creator structural
validator passed using an isolated external validation environment, without a new
repository runtime dependency. A debug-profile Linux bundle passed the release
smoke protocol, archive checksum checks and inspection of the complete skill
and new guides. This does not establish optimized release builds, macOS, current
stable Rust, minimum Git or a live GitHub release rehearsal for this candidate.
Documented opt-in reflink/large benchmark tests remain skipped on this host.
Agent trial evidence and discovery limitations are in [skill evaluations](skill-evaluations.md).

Gitleaks 8.30.1 found no likely credentials in all available Git history, the
staged implementation or downloaded historical Actions logs (42 runs, 376 text
log files). No Actions artifacts were retained by GitHub at review time. The
scanner was obtained from its checksummed upstream release; it is not a new
repository dependency. This is automated credential screening, not a complete
manual confidentiality, license or security audit. Keep that review as a launch
gate, and do not commit downloaded logs or scan reports.

GitHub access was verified while the repository remained private. The existing
[CI run](https://github.com/lorecampa/groove/actions/runs/43233105388) did not start
jobs: GitHub reported failed recent payments or an insufficient spending limit.
The branch-protection API also returned HTTP 403 requiring GitHub Pro or public
visibility. Resolve billing for private remote validation; configure protection
when supported, without changing visibility merely to get a green check.

Automatic push, pull-request and scheduled CI triggers were subsequently disabled
at the maintainer's request. Local checks remain required; manual/release checks
remain available when compute returns. Remote validation is not a merge gate
while hosted compute is unavailable.

These external limitations prevent claiming remote release readiness. No crate
publication, release tag, release announcement or repository visibility change
is part of the implementation verification.
