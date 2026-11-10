# Snapshot regression and benchmark lab

Correctness and timing are separate. The Rust `snapshot_scale` integration suite
uses deterministic mutations and full Git-tree assertions; elapsed time never
makes a correctness test pass or fail. The benchmark runner records timings and
resource/storage data without enforcing noisy hosted-runner latency thresholds.

## Versioned inputs

- `workloads-v1.json`: fixture version, seed, directory layout, content pool,
  changed-file count, profile sizes and a golden SHA-1 tree for the smoke fixture.
  Rust integration tests and the Rust benchmark runner consume this same file. Independent
  generators must produce the golden tree; no random seed or sleeps are involved.
- `environment.lock.json`: Rust 1.89.0, Git 2.40.0, GNU time 1.9,
  the immutable linux/amd64 base-image digest, Debian archive date, Git source
  checksum, and runtime CPU/memory limits.
- `Dockerfile`: builds that environment and release binaries with `Cargo.lock`.
  The installer verifies actual tool versions. Runtime uses four CPU-equivalents,
  8 GiB RAM, no swap, no network, a read-only root, and writable isolated scratch.
- `src/`: the non-published `groove-bench` Rust workspace package. It handles
  fixture generation, bounded subprocess execution, measurements, atomic v2 JSON
  reports, comparisons, and container orchestration. No Python runtime is needed.
  The launcher creates a source-only build context, records a content hash plus
  Git revision/dirty state, caches the image, and mounts a supplied source repository
  read-only. Tests run as part of the normal Cargo workspace suite.

Containers pin user space, not host hardware or the kernel. Reports also record
CPU model, architecture, kernel, filesystem, cgroup quotas, available storage,
package manifest hash, image ID and executable hash. The comparator refuses
workload/toolchain/hardware/resource drift, and requires canonical reports for regression comparisons. Different source revisions and binary
hashes are allowed, since comparing code changes is the purpose. CPU contention,
frequency, storage devices and cache state still introduce noise; repeat samples
on a dedicated host for performance decisions.

## Deterministic regression coverage

```sh
# Small fixture in every normal workspace test run:
cargo test -p groove-core --test snapshot_scale --locked

# Opt-in local canonical 100k-file regression:
cargo run -p groove-bench --locked -- check --profile large \
  --temp-root /path/on/local/disk --output /path/to/results/check-large.json

# Opt-in local million-file regression:
cargo run -p groove-bench --locked -- check --profile million \
  --temp-root /path/on/local/disk --output /path/to/results/check-million.json
```

Each fixture has the configured number of generated files plus `.gitignore`.
The deterministic checks cover distributed staged/unstaged edits, deletions,
binary untracked data, a thousand ignored files, an independent full-tree staging
oracle, unchanged HEAD/index, unchanged-save blob reuse, both restoration modes,
snapshot deletion and retained-object reachability after GC. UUIDs and timestamps may vary;
asserted file/index state does not. The explicit large tests are ignored by normal
Cargo runs and are selected explicitly by these local commands.

## Measurements and comparison

```sh
cargo run -p groove-bench --locked -- benchmark --profile large --profile million \
  --temp-root /path/on/local/disk --output /path/to/results/baseline.json

# Optionally include a real repository, cloned from a read-only mount:
cargo run -p groove-bench --locked -- benchmark --profile smoke \
  --real-repo /path/to/repository --temp-root /path/on/local/disk \
  --output /path/to/results/real.json

cargo run -p groove-bench --locked -- compare \
  /path/to/results/baseline.json /path/to/results/candidate.json
```

The million-file cases need several million free inodes and roughly 13 GiB of free
scratch space; preflight checks reject insufficient capacity. Canonical runs reject
tmpfs and overlay scratch storage. The launcher removes its scratch directory after
completion. Images are retained in Docker's ordinary cache for reuse; no global
Docker prune is performed.

Reports include every save sample, first/unchanged saves, both restore timings,
maximum RSS reported by GNU time, logical and allocated object-byte growth,
fixture tree IDs and source-state verification. RSS is not aggregate simultaneous
process-tree memory. Cache policy is explicit: no cache eviction, then warm samples.
Fixtures use 256 repeated small payloads; they do not simulate large unique binary
histories. Git dates and file contents are fixed for reproducible fixture trees.

Every changed save is checked against ordinary full-tree Git staging in a separate
index, outside the timed command. Both restore timings target the initial clean
checkpoint. Additional untimed checks restore the final changed checkpoint into
a separate checkout and in place, comparing staging and working trees with that
independent oracle. A successful report requires all these checks. The unchanged
save must also preserve the checkout's state. Git step timings require `-vv` debug
diagnostics; missing diagnostics fail the run rather than producing empty samples.

Reports retain JSON schema version 2 and now record `measurement_version: 2` for
the corrected diagnostics and stronger verification. Regenerate both baseline and
candidate reports: comparison rejects missing/different measurement versions and
reports without verified dirty recovery. Correctness checks are not performance
samples, and this suite does not measure pool acquisition, setup or worker throughput.

For local exploratory measurements, build `groove-cli` in release mode, then use
`cargo run -p groove-bench --locked -- run --mode benchmark --profile smoke
--temp-root /path/on/local/disk --output /path/to/local.json`. These reports are
**non-canonical** and cannot be used for regression comparisons. The former Python
entry points have been removed; use the Cargo commands above.

Environment v2 removes the Python runner dependency. Its lock/package hashes differ
from v1, so regenerate both baselines and candidates; old reports must not be
relabeled as comparable. The fixture and JSON schema remain version 1 and 2
respectively. Historical ad-hoc results in `docs/snapshot-benchmark-results.json`
also remain incompatible. Reports include the runner executable's hash.

## CI and maintenance

Benchmarks and the 100k/1M-file workloads run locally only. GitHub CI runs the
normal workspace suite, including small snapshot correctness and harness/hook
tests; it does not build benchmark containers or run performance measurements.
There is no scheduled or manually dispatched scale workflow. Use the explicit
commands above when local resources are available and retain their JSON reports.

Run harness/hook tests with:

```sh
cargo test -p groove-bench --locked
# Real release CLI, recovery and dirty-source preservation:
cargo build -p groove-cli --release --locked
cargo test -p groove-bench --test cli --locked -- --ignored
```

Change fixture semantics by introducing a new fixture version and golden tree.
Change environment tools/resources by updating the lock and Dockerfile together,
then validating `verify-environment` and regenerating baselines. Never relabel old
results as comparable. `Cargo.lock`, source/binary hashes and the package manifest
preserve the exact inputs behind a measurement, including dirty working trees.
