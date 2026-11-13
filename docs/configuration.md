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

When an experiment graduates, make its implementation unconditional and retain
the registry entry as stabilized. Old opt-ins remain valid. An abandoned
experiment keeps a removed entry with a reason and replacement guidance; a
request fails explicitly. Never reuse a name. Experimental behavior can change
before graduation, but those changes still require release notes and review of
compatibility expectations.

## Implementing an experimental feature

The snippets below use a hypothetical `fast-scan` feature. It is not registered
or available in the current binary. Add the real feature's registry entry,
runtime gate and regression coverage in the same PR.

### Register and connect the capability

1. In [the core registry](../crates/groove-core/src/experimental.rs), define a
   named constant and add it to `FEATURES`, keeping all existing entries:

   ```rust
   /// Opt into the new scan strategy.
   pub const FAST_SCAN: Feature = Feature {
       name: "fast-scan",
       introduced_in: "0.3.0",
       status: FeatureStatus::Experimental,
   };

   pub const FEATURES: &[Feature] = &[FAST_SCAN];
   ```

   Use the actual first release version. Choose a permanent name accepted by
   the resolver; never repurpose it for another feature.

2. At the command implementing the feature, resolve the applicable sources
   before starting the operation. With user preferences and the relevant project
   definition already loaded:

   ```rust
   let features = user_preferences
       .experimental
       .resolve_with(&project.experimental)?;
   ```

   Use `resolve()` when only one source applies. Keep preference loading in the
   CLI and pass the resolved `FeatureSet` explicitly into the core operation.
   Registration validates a name; it does not activate an implementation by
   itself. The initial infrastructure does not automatically pass resolved
   features through CLI handlers, so the first real feature must add that
   plumbing. Preserve access to help and configuration repair when settings are
   invalid. Reading project configuration must not execute project commands.

3. Gate the experimental behavior using the registered definition:

   ```rust
   if features.is_enabled(&FAST_SCAN) {
       scan_with_new_strategy()?;
   } else {
       scan_with_existing_strategy()?;
   }
   ```

   These scan functions stand for the feature's actual implementation. Keep
   existing behavior when the experiment is absent. A feature that changes
   durable storage also needs the independent compatibility design described
   [below](#storage-and-ownership-boundaries).

4. Document the feature in installed command help and `CHANGELOG.md`. Replace
   applicable statements that no experiments are registered, including module
   documentation and schema descriptions. Update schema structure and
   conformance tests if the feature adds configuration or changes wire behavior.
   Follow `config global --help` for user opt-in syntax; user and project
   requests are additive, and setting the feature array replaces that source's
   previous array.

### Test before releasing the experiment

Exercise the actual operation with the feature absent and explicitly enabled.
Assert the existing behavior in the first case and the intended new behavior in
the second; accepting the configuration or inspecting a `FeatureSet` alone does
not establish that the runtime gate works. Cover failure paths and preservation
of user data where the feature can change it. Ownership/recovery changes still
require real-process regressions.

Check user-only and project-only opt-ins through the relevant CLI path, along
with their additive union and the same name in both sources. Unsupported,
malformed and duplicate requests must retain their documented error categories
and leave configuration unchanged. Adapt the existing resolver/CLI contract
tests when registration changes their assumptions, keeping fixture-only names
out of the production registry.

Add a saved TOML input and an independently reviewed effective-value expectation
for the new opt-in, following [the golden-test rules](testing.md). Preserve
existing historical inputs and default expectations unless a behavior change is
intentional. Validate sparse input against the published schema as well as
loading it through the real parser. Keep exact file-edit expectations when
configuration editing is affected.

Run focused checks for the feature, then the
[required validation](../CONTRIBUTING.md#validation) with default scheduling.
Report actual results and skipped coverage in the PR.

## Stabilization and cleanup

After reviewing user feedback and the feature's interactions, stabilize it in a
PR that makes the following changes together:

1. Change the retained registry entry to
   `FeatureStatus::Stabilized { since: "0.4.0" }`, using the actual graduation
   version. The resolver enables stabilized entries without an opt-in. `since`
   records release history; it does not select behavior by comparing versions.
2. Delete the runtime `if`, its old implementation and feature-specific plumbing
   that no other experiment needs. Leave the new implementation unconditional:

   ```rust
   scan_with_new_strategy()?;
   ```

   Remove unused imports and fallback-only helpers. Keep the feature definition
   and its `FEATURES` entry: the parser still uses them to accept old saved
   opt-ins, even though the operation no longer checks the flag.
3. Keep the original opt-in TOML fixture unchanged. Verify that both an absent
   flag and that legacy file now reach the stable behavior. Retain regression
   coverage of the feature itself, updating it to exercise the default path.
   Review expected-value changes caused by graduation explicitly; do not
   regenerate historical inputs or retain assertions that the old fallback
   still runs. Keep the resolver's graduation compatibility coverage.
4. Update help and release notes to describe the stable behavior. Users may
   remove the obsolete opt-in, but leaving it must remain valid. Do not rewrite
   their files automatically. Run focused regressions and required validation
   again before merging.

If an experiment is abandoned instead, remove its implementation and keep a
`FeatureStatus::Removed { since, reason }` entry with an actionable replacement
or explanation. Test that requesting the old name yields
`UNSUPPORTED_CONFIGURATION`, without silently falling back or changing user
data. Removing an already stable behavior requires a separate compatibility
review; an experimental tombstone does not waive stable configuration promises.

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
