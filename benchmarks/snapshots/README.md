# Snapshot regression and benchmark lab

Correctness and timing are separate. The Rust `snapshot_scale` integration suite
uses deterministic mutations and full Git-tree assertions; elapsed time never
makes a correctness test pass or fail. The benchmark runner records timings and
resource/storage data without enforcing noisy hosted-runner latency thresholds.

## Versioned inputs

- `workloads-v1.json`: fixture version, seed, directory layout, content pool,
  changed-file count, profile sizes and a golden SHA-1 tree for the smoke fixture.
  Rust integration tests and Python benchmarks consume this same file. Independent
  generators must produce the golden tree; no random seed or sleeps are involved.
- `environment.lock.json`: Rust 1.89.0, Git 2.40.0, Python 3.11.2, GNU time 1.9,
  the immutable linux/amd64 base-image digest, Debian archive date, Git source
  checksum, and runtime CPU/memory limits.
- `Dockerfile`: builds that environment and release binaries with `Cargo.lock`.
  The installer verifies actual tool versions. Runtime uses four CPU-equivalents,
  8 GiB RAM, no swap, no network, a read-only root, and writable isolated scratch.
- `harness.py`: report schema version 2, explicit correctness checks (including
  under `python -O`), independent scratch directories per workload, atomic report
  publication, and comparison rules.
- `run.py`: creates a clean build context containing only project source inputs,
  records its content hash plus Git revision/dirty state, builds a content-addressed
  image, and mounts a supplied real repository read-only when requested.

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

# Canonical 100k-file regression (also runs on relevant PRs):
python3 benchmarks/snapshots/run.py check --profile large \
  --temp-root /path/on/local/disk --output /path/to/results/check-large.json

# Million-file regression (scheduled weekly and manually in CI):
python3 benchmarks/snapshots/run.py check --profile million \
  --temp-root /path/on/local/disk --output /path/to/results/check-million.json
```

Each fixture has the configured number of generated files plus `.gitignore`.
The deterministic checks cover distributed staged/unstaged edits, deletions,
binary untracked data, a thousand ignored files, an independent full-tree staging
oracle, unchanged HEAD/index, unchanged-save blob reuse, both restoration modes,
pin/prune and retained-object reachability after GC. UUIDs and timestamps may vary;
asserted file/index state does not. The explicit large tests are ignored by normal
Cargo runs, then selected by exact name in the dedicated CI workflow.

## Measurements and comparison

```sh
python3 benchmarks/snapshots/run.py benchmark --profile large --profile million \
  --temp-root /path/on/local/disk --output /path/to/results/baseline.json

# Optionally include a real repository, cloned from a read-only mount:
python3 benchmarks/snapshots/run.py benchmark --profile smoke \
  --real-repo /path/to/repository --temp-root /path/on/local/disk \
  --output /path/to/results/real.json

python3 benchmarks/snapshots/harness.py compare \
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

`python3 scripts/benchmark-snapshots.py` remains a compatibility entry point into
this same harness. It produces **non-canonical** host-environment reports and
accepts the versioned fixture sizes. The earlier ad-hoc measurements in
`docs/snapshot-benchmark-results.json` remain historical observations; they have a
different fixture and environment and cannot be compared through the v2 comparator.

## CI and maintenance

`.github/workflows/snapshot-scale.yml` runs the pinned 100k regression on relevant
pull requests and pushes to main. Weekly/manual runs additionally execute the 1M regression and both
large benchmark profiles. JSON evidence is uploaded even when a workload fails;
images and timing results are not treated as proof of remote CI execution locally.

Run harness/hook tests with:

```sh
python3 -m unittest discover -s tests/benchmark_infra -v
```

Change fixture semantics by introducing a new fixture version and golden tree.
Change environment tools/resources by updating the lock and Dockerfile together,
then validating `verify-environment` and regenerating baselines. Never relabel old
results as comparable. `Cargo.lock`, source/binary hashes and the package manifest
preserve the exact inputs behind a measurement, including dirty working trees.
