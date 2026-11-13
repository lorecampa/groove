# Security policy

Groove worktrees are for cooperative agents and CI. They share Git objects,
configuration and refs and are not sandboxes for untrusted code. Supported scope
and destructive cleanup boundaries are documented in [operations](docs/operations.md).
Only the current version is maintained during the alpha.

See [security maintenance](docs/security.md) for dependency policy, advisory
checks, parser fuzzing, build-tool updates and release provenance verification.

For a vulnerability, use
[GitHub's private report form](https://github.com/lorecampa/groove/security/advisories/new)
when private vulnerability reporting is enabled. From the repository's Security
tab, choose **Report a vulnerability**.

If that form is unavailable, [open a contact request](https://github.com/lorecampa/groove/issues/new?title=Private%20security%20contact%20request)
asking only for a private reporting channel. Do not include vulnerability details,
proofs of concept, secrets or affected private data in that public request.

In the private report, include the Groove version, operating system,
Git/Rust/kernel versions, filesystem, affected command and a reproduction using
a disposable repository.
