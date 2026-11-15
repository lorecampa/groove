# Groove benchmark profiles

`groove-bench` is an unpublished development CLI. A **profile names a complete
benchmark scenario**: fixtures, operations, correctness assertions, sampling,
concurrency and environment requirements. There are no workload or sampling flags.
Definitions live in [`profiles-v1.json`](profiles-v1.json); the Rust runner validates
them before execution and embeds their exact definitions and hashes in the report.

## Commands

```sh
# Discover scenarios, cases, sampling, estimated time and scratch requirements.
cargo run -p groove-bench --locked -- profiles
cargo run -p groove-bench --locked -- profiles snapshot-scale

# Measure one scenario, or every profile in one invocation.
cargo run -p groove-bench --locked -- benchmark snapshot-incremental
cargo run -p groove-bench --locked -- benchmark all

# Execute each profile's cases once, asserting correctness without statistical verdicts.
cargo run -p groove-bench --locked -- check workspace-concurrency
cargo run -p groove-bench --locked -- check workspace-multi-pool workspace-deletion
cargo run -p groove-bench --locked -- check all

# Validate the report and recompute its summary from raw evidence.
cargo run -p groove-bench --locked -- inspect
```

The default scratch parent is `target/benchmark-scratch`; it must be on local disk.
The default and **single output artifact** is
`benchmarks/snapshots/results/report.json`, an ignored path. `--temp-root` and
`--output` relocate those paths. Each invocation atomically replaces its output,
including operational failures. Nothing publishes results automatically, and
benchmark results or separate results summaries are not checked in.

`all` selects the complete catalog. Multiple names select precisely those profiles:

```sh
cargo run -p groove-bench --locked -- benchmark workspace-reuse
```

The first container run prepares dependencies and builds release binaries; later
runs reuse the content-addressed image. Estimates printed by `profiles` exclude
image preparation and depend on the host. Expensive profiles remain explicit.

## Profile definitions

Use `profiles` to inspect the current cases, operations, sampling and resource
requirements from [profiles-v1.json](profiles-v1.json). The same definitions
are validated by the runner and embedded in each report; this guide does not
maintain a second catalog.

One **repetition** runs a complete case on a fresh disposable repository: prepare
the fixture, measure the declared operations, verify correctness and clean up.
The report calls this a statistical **block**; multiple commands within one
repetition are not independent repetitions. With a baseline, each block contains
both binaries' runs on separate, identical fixtures.

Sampling, warmups and checkpoint history length belong to each profile/case.
`check` executes one repetition per case without warmups and uses the same
operations/assertions; it does not silently reduce a fixture's size. Source path
counts exclude explicitly defined supporting project/artifact files.

Cases execute sequentially. Only concurrency cases intentionally launch parallel
workers. Fixtures are fresh for each side/block, their Git storage is
normalized, and all sides must produce the same fixture identity. The primitive
path generator retains its independent golden smoke tree in `workloads-v1.json`.
No developer checkout is used as a benchmark fixture. Arbitrary repositories or
bundles cannot override a profile from the command line; a new dataset requires
an explicit, versioned profile definition.

Pool profiles assert `recovery: sanitized` after released-checkout reuse, including dirty
cleanup and monorepo base switches. A fast timing alone cannot establish reuse.
They control checkout materialization with automatic spare preparation
and idle eviction disabled. Capability discovery is untimed; older baseline
CLIs without these options already used lazy pools. Default background readiness
has separate process regressions. Successful blocks must finish checked fixture
cleanup before the runner records success.

`workspace-conversions` separately measures checkout-conversion costs
with CRLF attributes, including a small repository. Its physical-edit case changes
CRLF to LF: the edited file still hashes to the committed blob after Git's
normalization, but the acquired checkout must recover the expected CRLF bytes.
This profile verifies physical restoration as well as timing. It provides a
control workload for future optimizations that must preserve checkout behavior.

### Offline build workflows

`workspace-build` compares pristine cleanup, pristine with an external compiler
cache, and warm cleanup with ordinary ignored output. Every case has the same
10k source-shaped paths plus 32 real C compilation units, a header and a Makefile.
It runs `make -j4`, executes the program and checks its output against an
independent Rust calculation. It needs `cc`, GNU make and ccache 4.7+ on the host;
the pinned Debian image installs ccache 4.7.5 (Debian package
`4.8+really4.7.5-1`). Builds need no downloads.

Version 2 interleaves the three strategies each round, using all six permutations
over six measured rounds. Each strategy occupies each order position twice; the
actual schedule is recorded and checked by `inspect`. Warmup rounds are discarded.
The grouped version-1 prototype remains distinguishable by its profile hash/version.
Existing profiles keep their sequential case order and binary AB/BA sampling.

Each repetition starts with an empty cache and measures six successive tasks:
first use, repeated input, changed header, changed compiler flags, return to the
original commit/flags, and loss of disposable cache/output. The source checkout
and refs must survive, every acquired index and tracked working tree must match
an independent oracle, cleanup must follow policy, outputs must have independent
inodes, and every reservation must be released. A warm output's existence is not
treated as a cache hit. The compiler cache must show real misses/hits in the
appropriate phases; a fast stale executable fails the case.

The cache is outside every pool root and private to a block/binary. The runner
clears inherited settings and uses ccache's content-based compiler checks,
preprocessor mode, no sloppiness, and no hardlinks or reflinks. Preprocessor mode
avoids direct-mode timestamp heuristics for newly checked-out headers. Compiler,
make and ccache versions are retained and must agree across paired sides/blocks.
This measures compiled-object caching; it does not measure dependency downloads.

Raw samples retain acquisition, build, execution/validation and release wall time,
CPU and peak RSS. Phase-specific readiness is the sum of timed acquire/build/
validation intervals; total task time also includes release. Independent oracles,
tool queries, cache counters, storage scans and fixture/cache-loss preparation are
outside timing. Output and external-cache storage report logical and allocated
regular-file bytes. These checks warm filesystem caches; no OS-cold claim is made.
With a baseline binary, normal paired AB/BA sampling and intervals compare binaries.
Cache counters remain informational rather than receiving speed verdicts. Strategy
medians are descriptive; this profile does not assign a paired strategy-level speedup
verdict or establish a result for every build system.

```sh
cargo run -p groove-bench --locked -- check workspace-build --host
cargo run -p groove-bench --locked -- benchmark workspace-build
```

### Heterogeneous monorepos

`monorepo-reuse` exercises acquisition, dirty cleanup and backward/forward commit
switches; `monorepo-snapshots` exercises mixed staged/unstaged edits, deletion,
untracked content and recovery. Use `profiles PROFILE` for their sizes, sampling
and conservative scratch/inode budgets before starting these expensive scenarios.

The generator mixes unique source-shaped text and poorly compressible binary
assets over nested package/module paths, with deeper generated paths for every
seventh file. Per 100 paths, 80 contain 1 KiB, 15 contain 16 KiB, four contain
256 KiB and one contains a 1 MiB binary. Thus 100k generated paths contain
2,424,832,000 bytes (2.26 GiB), before supporting files, Git storage and filesystem
allocation. Generation holds at most one file's payload in memory.

Each history commit edits up to one percent of paths (excluding binary assets),
adds a file and deletes a file. Objects are packed before measuring. The fixture
identity binds the complete deterministic history, not just the tip tree. Reuse
switches to the initial commit and back, checking full index and working trees
through the independent Git oracle. Warm cleanup must preserve every ignored
output file byte-for-byte; pristine cleanup must remove them. Fixture preparation
and correctness checks are outside operation timings and can warm OS caches.

To avoid repeatedly generating and packing multi-GiB history, each monorepo case
builds one immutable seed. Every side/block gets a local clone with independent
objects (`--no-hardlinks`, no alternates) and a fresh checkout/index. The seed's
commit identity is checked against every clone. Other profiles retain their
per-block generation. Seed construction, copying and checkout are outside measured
operations; this is warm-cache characterization, not a cold-start claim.

These are controlled synthetic workloads, not copies of company repositories.
They complement the million-path fixture, whose tiny repeated files isolate
path/index scaling. They do not model years of history, Git LFS, sparse checkout
or cold OS caches. Runs need no fixture downloads. Paired sides execute
sequentially, so peak scratch requirements are not doubled.

```sh
cargo run -p groove-bench --locked -- benchmark monorepo-reuse \
  --baseline-source /absolute/baseline-checkout
cargo run -p groove-bench --locked -- check monorepo-snapshots
```

## Correctness evidence and timing

Every successful block requires its scenario's specific assertions. Saves preserve
HEAD/index/working files, match independent full-tree Git staging, avoid new blobs
on unchanged saves and recover the captured staging/working trees. Restore cases
verify both trees and base HEAD, preservation of existing ignored residue, and
absence of ignored data in new checkouts. The Git oracle writes only into separate
object storage, so it cannot repair a broken snapshot or change object-growth data.

Workspace cases inspect actual files, Git state and cleanup policy. Concurrent workers must have distinct lease IDs and
paths, succeed, reject excess capacity with the typed CLI error/exit category, and
release every reservation. Expected failures are asserted rather than discarded.
Multi-pool workers reuse task labels/request keys across namespaces, verify distinct
IDs/paths, and exhaust each pool independently.
Their first registration measurements are separate from warm batch throughput.
Deletion cases verify every configured path/native registration is removed and
permanent lock inodes, snapshot recovery, saved-release receipts, request-key tombstones,
root neighbors and a protected sibling survive. The external-cache case creates
legacy artifact fixtures directly; dirty cases add tracked, untracked and ignored
files using the profile's fixture definition. Fixture
setup, result preservation and recovery verification are outside deletion timings.
Reinit uses a new checkout root, retaining the intentionally preserved root neighbor.
Worker start phases use bounded readiness handshakes; startup failures disconnect
waiting peers, and a worker panic retains the fixture for explicit recovery.

Only the named operations are included in a profile's metrics. Fixture generation,
setup for measurement, Git oracles and teardown are outside timed intervals but
can warm OS caches. First save means no prior application checkpoint in that
fixture; no OS-cold claim is made. No host cache eviction occurs. Normal timings
include release CLI startup, safety/durability work, the time wrapper, capture files
and 1 ms exit polling. Ten CLI-version calibration samples are retained without
subtraction. Verbose logging is disabled in measured commands.

Raw per-command wall/CPU/RSS observations remain in the report. History and worker
observations are aggregated within an independent block. RSS is the largest
collected per-process peak, not simultaneous process-tree memory. Save profiles
also record logical/allocated Git-object growth before recovery verification.

For paired comparisons, supply only an operational baseline source path:

```sh
cargo run -p groove-bench --locked -- benchmark snapshot-incremental \
  --baseline-source /absolute/baseline-checkout
```

One fixed runner executes both release CLIs, alternating AB/BA across measured
blocks. Differences receive pointwise 95% percentile bootstrap intervals (4,000
resamples of whole paired blocks), with a 5% practical threshold. Verdicts are
`faster`, `slower`, `no_meaningful_change` or `inconclusive`. Unpaired runs report
characterization only. Intervals across many metrics are not familywise guarantees;
inspect variability/outliers (which are retained) and use a quiet host. No timing
threshold makes a correctness check pass or fail.
Baselines for the multi-pool/deletion profiles must implement those CLI features;
an older binary is reported as an operational failure, never a timing comparison.

## Pinned software and controlled runtime

[environment.lock.json](environment.lock.json) pins the platform, tool versions,
base image, archive/checksum inputs and resource policy. The acquisition phase
vendors dependencies for both lockfiles. BuildKit then disables
networking while Cargo compiles the runner and each CLI independently with
`--frozen`; tool-only dependency features cannot change the measured CLI's build.
Missing dependencies or lock drift fail. Source, binary, runner, package, vendor,
fixture, catalog, profile and environment identities are retained in the artifact.

The launcher inspects and validates the actual image, network-disabled/read-only
container, init reaper, bind mounts and the locked CPU/memory/swap/process policy
before starting it. The reaper collects detached checkout workers after their
foreground CLI exits, keeping zombies from consuming the process limit.
`--cpu-set` selects CPU IDs within that policy. Affinity is not an
exclusive reservation. Host identity, kernel,
CPU model, storage mount and cgroup CPU/memory counters are recorded. Containers
cannot isolate host storage contention, CPU frequency or kernel differences.

Measured subprocesses use a controlled environment, fixed canonical executable
paths, a fixed 0022 creation mask and disposable home/config/temp directories. Docker client settings stay in
the launcher. Native Rustup discovery does not restore user Git configuration.
Progress is retained in the same report between blocks, including on failure.
Only the exact owned container is removed; scratch removal follows successful
container teardown. Failed cleanup retains scratch. SIGKILL/host loss cannot run
cleanup: inspect that invocation's container before rerunning, never prune unrelated
containers or images. Native interrupted/timed-out fixtures likewise remain for
explicit recovery after all writers stop.

Native macOS/APFS or host exploration selects the same profiles:

```sh
cargo build -p groove-cli --release --locked
cargo run -p groove-bench --locked -- benchmark workspace-reuse --host
# Optional native binary paths are execution choices, not workload overrides.
cargo run -p groove-bench --locked -- benchmark workspace-reuse --host \
  --binary /absolute/candidate/groove --baseline-binary /absolute/baseline/groove
```

Host reports are explicitly non-canonical; they do not claim the pinned Linux
build/resource contract. There is no canonical macOS environment lock yet.

## Development validation

```sh
cargo test -p groove-bench --locked
cargo test -p groove-core --test snapshot_scale --locked
cargo build -p groove-cli --release --locked
cargo test -p groove-bench --test cli --locked -- --ignored
cargo test -p groove-bench --test container --locked -- --ignored
cargo test -p groove-bench --locked small_monorepo_reuse -- --ignored
```

Normal Cargo runs cover catalog/API/evidence/harness regressions and small core
scale correctness. Opt-in release-CLI regressions exercise routine profiles and a
complete paired profile without weakening sampling. The real Docker regression
checks interruption at a canonical profile boundary. Large core `snapshot_scale`
tests remain opt-in, separately from explicit `check snapshot-scale` / `benchmark
snapshot-scale`; selecting a profile always executes all its declared cases.
GitHub runs normal correctness checks; no performance or large-scale job is
scheduled automatically. A workflow file is not evidence that remote CI ran.

Change a scenario's semantics by incrementing its profile version and reviewing
its cases, assertions, resources and sampling together. Every definition change
changes its hash. Reports strictly validate their version, selected definitions,
case/block/operation counts, checks and aggregates against raw evidence. Old report
formats are not relabeled as comparable. This private development artifact format
does not change the product CLI's JSON protocol.
