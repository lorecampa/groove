# Dependency review records

Cargo Vet uses this directory to check dependency review coverage. The TOML files
are the source of truth:

- [config.toml](config.toml) defines policy, trusted audit imports and exemptions
  for specific dependency versions.
- [audits.toml](audits.toml) records dependency reviews performed for this project.
- [imports.lock](imports.lock) records imported review evidence used by the locked
  check.

An exemption accepts a version without requiring sufficient audit evidence; it
does not certify safety or imply a known vulnerability. Treat exemptions as a
review backlog. Do not reinitialize the store or bulk-add exemptions to make an
update pass. Record only reviews actually performed, and review changes to
imported evidence when refreshing it.

The [security gate](../scripts/security.sh) checks these records without refreshing
imports. Passing means the recorded policy is satisfied, including any exemptions;
it does not mean every dependency has been audited. Groove does not read these
files at runtime.

See [security maintenance](../docs/security.md) for dependency update and review
procedures, required checks and the limits of advisory scanning.
