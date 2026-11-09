# Security

Groove worktrees are for cooperative agents and CI. They share Git objects,
configuration and refs and are not sandboxes for untrusted code. Supported scope
and destructive cleanup boundaries are documented in [operations](docs/operations.md).
Only the current development version is maintained before the first release.

For a vulnerability, use the repository's private vulnerability reporting feature
if available. Otherwise ask the maintainer for a private reporting channel without
posting exploit details in a public issue. Include the Git/Rust/kernel versions,
filesystem, affected command and a reproduction using a disposable repository.
