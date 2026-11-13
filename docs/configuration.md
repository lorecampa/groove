# Configuration compatibility

User preferences are a persistent interface. New binaries must continue to read
supported older files, preserving the meaning of explicit stable settings.
This promise does not imply that older binaries understand new
options or can safely share a pool with newer writers. User preferences remain
unversioned, independently of the CLI response and durable pool/record versions.
Project execution configuration is no longer read;
see the removal notice in the changelog.

Use each `config` command's installed help for syntax and defaults. The
[user schema](../schemas/user-v1.schema.json) describes its TOML data
model; the loader also enforces semantic constraints. JSON Schema default
annotations document defaults but do not populate missing fields.

## Evolution and retirement

Adding an optional setting does not itself require a new format major. Keep old
spellings, types, units and explicit stable meanings readable. Renames need
aliases with a deliberate conflict rule. Incompatible interpretation changes
need an explicit migration/version policy; do not tie format versions to every
binary release. Review default changes as behavior changes too.

Loading configuration is read-only. Do not automatically migrate a user's file
or remove obsolete opt-ins during an unrelated update. Global updates edit the
TOML document, validate the complete candidate and publish atomically, preserving
unrelated settings and comments. Invalid candidates leave original bytes intact.

Experiments are individually named capabilities. A persistent switch that enables
all experiments would also opt the user into unknown future behavior on upgrade,
so Groove does not provide one. The shared core registry records introduction,
graduation and removal; it starts empty. Tests use fixture-only feature names,
which are never accepted by production binaries.

Implement a real experiment's gate and its regression coverage with its registry
entry. Resolve user preference requests through the shared resolver before
testing a registered feature definition. Requests are additive across sources;
omission never opts into an experimental capability. Ordinary unknown keys,
malformed names and duplicate entries remain errors. Unsupported requested names
fail explicitly rather than silently running with different semantics.

When an experiment graduates, remove its conditional implementation and retain
the registry entry as stabilized. The feature becomes the default while old
opt-ins remain valid. Removal keeps a tombstone with a reason and replacement
guidance; a request fails explicitly. Never reuse a name. Experimental behavior
can change before graduation, but those changes still require release notes and
review of compatibility expectations.

## Storage and ownership boundaries

Feature opt-in never authorizes weaker descriptor ownership, lock replacement or
destructive cleanup. An experiment that writes incompatible storage needs an
independent, tested storage-version/admission design so unsupported readers and
writers refuse before changing state. Neither accepting a config file nor
enabling the same feature makes mixed-version pool access safe; see
[architecture](architecture.md).

Historical input fixtures and independent expected results protect these
contracts; follow [the testing guide](testing.md). Retain old input files through
graduation and review expected behavior changes explicitly. Current-schema
conformance and serializer roundtrips supplement those fixtures, but cannot
replace them or establish arbitrary downgrade compatibility.
