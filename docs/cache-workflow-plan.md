# Pristine workspaces and external caches

This proposal covers guidance, missing lifecycle coverage and a benchmark of the
complete acquire/build/release workflow. It preserves the existing cleanup
contract and keeps project execution and cache management in the caller's runner
or build tools.

## Current behavior and scope

Pristine acquisition removes tracked edits, untracked files and ignored residue.
Warm acquisition retains ignored residue only in healthy reused checkouts, without
validating it for the next task. Rebuilding a checkout may discard that residue.
Tracked paths still receive a full rewrite under both policies.

Managed execution, setup profiles and artifact-cache APIs were removed in PR #32.
This plan uses external build-tool caches and direct commands in a reservation;
it does not restore those APIs, introduce `.groove.toml`, or add implicit execution.
See [operations](operations.md#cleanup-and-shared-state) and the
[architecture](architecture.md#allocation-and-cleanup) for the existing boundaries.

## 1. Explain the workflow and its responsibilities

Expand the operations guide and relevant installed help with a concrete choice:
use pristine cleanup for independent tasks, and retain dependencies through an
external cache supported by the build tool. Keep warm available for callers that
accept unmanaged ignored residue and validate outputs themselves. No default or
existing configuration value changes.

Distinguish a dependency/download cache from compiled outputs. The build tool owns
compatibility, invalidation, concurrent access and validation. A lockfile alone
cannot establish correctness of arbitrary generated or executable output. Prefer
a tool's documented content-addressed cache where available; do not suggest
sharing mutable output directories or hardlinking workspace files across slots.

Show one acquire/direct-build/release workflow using an external cache outside
all pool roots. The runner owns cancellation, logs, timeouts and stopped-writer
teardown. Explain that ignored outputs are excluded from snapshots and required
results must be exported before release. Keep configuration syntax in command
help rather than introducing another reference table.

Acceptance: a reader can identify who validates each artifact, choose a cleanup
policy, and recover a saved task without mistaking retained files for readiness.
Update the changelog when the user-facing guidance ships.

## 2. Add only missing lifecycle regressions

Review existing reset-equivalence, settings and stress coverage before adding
fixtures. Full rewrites, external hardlinks, permission restoration, orphan HEAD,
backdated tracked files and warm-to-pristine transitions already have regressions;
keep them as the baseline instead of duplicating them.

Add observable scenarios for uncovered combinations:

- Switch A to B to A while ignore rules change and an ignored directory becomes a
  tracked path. Verify tracked bytes, HEAD/index and the policy-specific residue
  against an independent checkout. Include a protected sibling and source repo.
- Exercise a direct offline build in successive reservations with an external
  cache: repeat an input, change source/build configuration, then return to the
  original input. Check the executed program's output, independent writable
  workspaces and safe cache sharing through the selected tool's own interface.
- Remove or invalidate that external cache and rerun the same workflow. The build
  must produce the expected output without requiring Groove metadata repair.

Use disposable repositories and caches. Keep build-tool integration separate
from core cleanup tests, and opt in explicitly if additional host tools are
required. Use readiness handshakes for concurrent writers and stop them before
release. A rejected operation must retain protection and user results.

Acceptance: tests verify actual bytes or program output and source/sibling
preservation, with default scheduling and bounded waits. They establish the
selected build tool's behavior, not a general cache-validity guarantee in Groove.

## 3. Measure total time until usable output

Add a new versioned benchmark profile using the same offline fixture and build
command across three strategies:

1. Pristine acquisition and build without a persistent external cache.
2. Pristine acquisition and build with the selected tool's external cache.
3. Warm acquisition and build with its ordinary workspace residue.

Pin the build tool, cache backend and offline population method in the profile
identity. Report dependency-download caching separately from compiled-artifact
reuse; they avoid different work.

Measure acquisition, the direct build/validation, release and total task time
separately. Include first use, repeated input, changed source/build configuration
and a return to an earlier input. Use identical source history and toolchains;
report cache state and logical/allocated bytes, including external storage.
Verify output before recording a successful observation. Cache population and
validation that users wait for belong inside task timing.

Use a self-contained compilable fixture for build correctness. Retain existing
heterogeneous monorepo profiles for checkout and snapshot scaling; their
source-shaped text does not represent a compilable application. Add controlled
many-small-file and large-file cache payloads only where they answer a distinct
copy/hash cost question. Do not change workloads behind existing profile names.

Acceptance: retain raw paired samples, execution order, warmups and uncertainty,
including regressions and inconclusive results. State OS-cache/storage conditions;
do not claim cold-start performance without measuring it. Keep fixture generation
and independent oracles outside operation timing, and reports in PR/CI artifacts.

## 4. Validate and deliver incrementally

Resolve test-harness failures in a separate fix before relying on a full gate.
Reproduce timeout mechanisms with bounded fault injection; a later passing run
alone does not explain an intermittent failure. Preserve failed-run evidence.
Do not bypass hooks, raise deadlines, serialize the suite, or substitute RAM-backed
fixtures for failing native disk-backed tests.

Implement guidance, focused lifecycle coverage and the benchmark in separate
reviewable commits after this proposal is reviewed. Run the repository's
[required checks](testing.md#required-checks) on native disk-backed fixtures with
default scheduling, then the benchmark's correctness mode before paired timings.
Report Linux and macOS coverage separately; unrun platforms remain unvalidated.

No new config keys, experimental flags, storage formats or compatibility migration
are needed. Index refresh, changes to reset guarantees, removal of warm cleanup,
a Groove-managed cache engine and cache eviction are outside this proposal.
