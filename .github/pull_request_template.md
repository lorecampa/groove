<!--
Use a short imperative title, for example "Preserve reservations on save failure".
See CONTRIBUTING.md for coding, commit and review conventions.
Keep the description proportional to the change; use "None" where applicable.
Report vulnerabilities through SECURITY.md.
-->

## Change

<!--
Describe the concrete problem and resulting behavior for a reviewer without prior
context. Include a before/after example if helpful and explain significant
tradeoffs. Link related issues; use "Closes #123" only if this resolves the issue.
Review prose and comments using CONTRIBUTING.md#writing-and-comment-review;
preserve technical terms, safety conditions and useful reference structure.
-->

## Validation

<!--
List exact commands run, observed results and relevant regression coverage.
Run `bash scripts/check.sh push` for code changes. Hosted CI is not automatic.
State failures, skipped checks and platform limitations; serial-only success
does not establish default/concurrent scheduling. For docs-only changes, explain
what you checked and why runtime tests were unnecessary.
For ownership/recovery changes, describe descriptor lifetime, writer teardown,
and the regression that checks failure behavior and preservation of results.
-->

## Compatibility

<!--
Note Rust API, CLI/JSON, schema/storage, supported-environment or cleanup changes,
with migration or recovery steps where needed. Update help, schemas, conformance
tests and CHANGELOG.md where applicable. Write "None" if there is no impact.
-->
