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
cargo run -p groove-bench --locked -- benchmark workspace-reuse workspace-setup
```

The first container run prepares dependencies and builds release binaries; later
runs reuse the content-addressed image. Estimates printed by `profiles` exclude
image preparation and depend on the host. Expensive profiles remain explicit.

## Catalog v1

| Profile | Fixed cases | Measured operations |
| --- | --- | --- |
| `snapshot-incremental` | 10k unique text files; distributed edits and mixed staging/deletion/untracked/ignored state | First save, first changed save, checkpoint history, unchanged save |
| `snapshot-restore` | 10k unique text files and 100k repeated small files | Clean/dirty recovery into new and existing checkouts |
| `snapshot-scale` | 100k and 1M repeated small files; twelve distributed edits | First, changed, history and unchanged saves; recovery checked outside save timings |
| `snapshot-binary` | 8 MiB and 64 MiB deterministic, poorly compressible binaries alongside 1k small files | Saves after rewriting distinct 64 KiB ranges, object growth and verified recovery |
| `workspace-reuse` | 10k unique files under pristine and warm cleanup | First acquisition, clean reuse, dirty sanitation and release |
| `workspace-setup` | 1k source files, 256 4 KiB artifacts and one 4 MiB artifact | Explicit setup/cache publication on miss and independent copy seeding on hit |
| `workspace-concurrency` | 1k source files with 1, 2, 4 and 8 warm slots/workers | Acquire/no-op managed execution/release, per-worker latency, batch throughput and full-pool rejection |
| `workspace-multi-pool` | 1k source files; 2 or 4 named pools with 2 workers each, repeated labels/keys across pools | Pool init, first/warm acquisition, managed execution/release, catalog, per-worker latency, batch throughput and per-pool exhaustion |
| `workspace-deletion` | 64 lazy slots; 4 populated slots over 10k paths; 4 dirty slots; 4 external cached slots over 1k unique paths | Ambiguous-selection refusal, preview, deletion, completed retry and reinitialization while a protected sibling remains intact |

One **repetition** runs a complete case on a fresh disposable repository: prepare
the fixture, measure the declared operations, verify correctness and clean up.
The report calls this a statistical **block**; multiple commands within one
repetition are not independent repetitions. With a baseline, each block contains
both binaries' runs on separate, identical fixtures.

The scale profile owns six measured repetitions and one discarded warmup per case
and binary; other profiles own ten repetitions and one warmup. Checkpoint history
length belongs to each case. `check` executes one repetition per case without warmups. It
uses the same operations/assertions; it does not silently reduce a fixture's size.
Profile sizes count generated source paths; `.gitignore`, binaries, project files
and artifact sources add explicitly defined supporting files.

Cases execute sequentially. Only concurrency cases intentionally launch parallel
workers. Fixtures are newly generated for each side/block, their Git storage is
normalized, and all sides must produce the same fixture identity. The primitive
path generator retains its independent golden smoke tree in `workloads-v1.json`.
No developer checkout is used as a benchmark fixture. Arbitrary repositories or
bundles cannot override a profile from the command line; a new dataset requires
an explicit, versioned profile definition.

## Correctness evidence and timing

Every successful block requires its scenario's specific assertions. Saves preserve
HEAD/index/working files, match independent full-tree Git staging, avoid new blobs
on unchanged saves and recover the captured staging/working trees. Restore cases
verify both trees and base HEAD, preservation of existing ignored residue, and
absence of ignored data in new checkouts. The Git oracle writes only into separate
object storage, so it cannot repair a broken snapshot or change object-growth data.

Workspace cases inspect actual files, Git state and cleanup policy. Setup cases
compare every artifact with its independent source and verify hit/miss, skipped
steps and seeded counters. Concurrent workers must have distinct lease IDs and
paths, succeed, reject excess capacity with the typed CLI error/exit category, and
release every reservation. Expected failures are asserted rather than discarded.
Multi-pool workers reuse task labels/request keys across namespaces, verify distinct
IDs/paths and the selected worker environment, and exhaust each pool independently.
Their first registration measurements are separate from warm batch throughput.
Deletion cases verify every configured path/native registration is removed and
permanent lock inodes, snapshot recovery, saved-release receipts, request-key tombstones,
history/log bytes, root neighbors and a protected sibling survive. The external-cache
case prepares 64 4 KiB files plus a 1 MiB artifact with the real setup/cache API;
the dirty cases add tracked/untracked edits and 128 ignored files per slot. Fixture
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

`environment.lock.json` pins Linux amd64, Rust 1.89.0, Git 2.40.0, GNU time 1.9,
the base-image digest, Debian archive date, Git checksum and resource policy. The
acquisition phase vendors dependencies for both lockfiles. BuildKit then disables
networking while Cargo compiles the runner and each CLI independently with
`--frozen`; tool-only dependency features cannot change the measured CLI's build.
Missing dependencies or lock drift fail. Source, binary, runner, package, vendor,
fixture, catalog, profile and environment identities are retained in the artifact.

The launcher inspects and validates the actual image, network-disabled/read-only
container, bind mounts, CPU affinity/quota, 8 GiB memory, disabled swap and 512-PID
limit before starting it. Four available CPUs are selected, or `--cpu-set` supplies
exactly four IDs. Affinity is not an exclusive reservation. Host identity, kernel,
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
cargo run -p groove-bench --locked -- benchmark workspace-setup --host \
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
