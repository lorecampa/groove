# Dependency review baseline

This store was bootstrapped from the existing lockfile on 2026-11-12. Exact-version
exemptions are an explicit review backlog, not a claim that these dependencies
have been audited. Do not run `cargo vet init` or bulk-add exemptions to accept
an update. Review version diffs and record only work actually performed.

Mozilla's audit collection is an explicit trusted import. `imports.lock` records
the relevant evidence; review changes when refreshing it. First-party Groove
packages are not treated as crates.io dependencies. The locked check uses these
records without refreshing imports. See [security maintenance](../docs/security.md)
for update/review procedures and the limits of advisory scanning.
