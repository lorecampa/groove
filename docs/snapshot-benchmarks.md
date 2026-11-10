# Snapshot benchmark results

## Pinned local rerun, 2026-11-11

All current snapshot benchmark profiles and the separate 1k/100k/1M correctness
checks passed. Real-repository measurements used independent clones of Iurio,
Venacenter and Groove; each source's HEAD, index bytes, status, tracked diff and
refs matched before and after. All disposable scratch directories were removed.

The environment used Rust 1.89.0, Git 2.40.0, release binaries, Linux amd64, ext4,
an Intel i5-6600K, four CPU-equivalents and an 8 GiB memory limit with no swap.
Measurements ran sequentially, with no concurrent builds or test jobs from this
audit. The host was shared and OS caches were not flushed.

| Fixture or cloned repository | Tracked files | Median changed save | Unchanged save | Clean restore to new checkout | Clean restore in place |
| --- | ---: | ---: | ---: | ---: | ---: |
| Smoke fixture | 1,001 | 0.23 s | 0.10 s | 0.20 s | 0.23 s |
| Large fixture | 100,001 | 1.11 s | 0.68 s | 7.55 s | 3.12 s |
| Million fixture | 1,000,001 | 7.81 s | 7.28 s | 85.81 s | 69.32 s |
| Iurio | 1,621 | 0.54 s | 0.40 s | 0.57 s | 0.87 s |
| Venacenter | 224 | 0.29 s | 0.15 s | 0.52 s | 0.42 s |
| Groove | 166 | 0.25 s | 0.09 s | 0.17 s | 0.19 s |

Each median contains five warm saves changing twelve tracked files. Synthetic
counts include `.gitignore`. Both clean restore times are single observations;
additional checks restored the final changed checkpoint in both modes and
independently verified its staging and working trees. These full-tree checks make
million-file runs take minutes even when individual saves take seconds.

This run missed the earlier aspirational changed-save targets of one second for
100k files and three seconds for 1M files. Those targets are not correctness gates.
Million-file save and recovery latency remain performance limitations. Small
repeated synthetic contents measure path/index scale; large unique binaries,
pool acquisition, setup and worker throughput are outside this suite.

The runner now uses `measurement_version: 2`: Git debug timings must be present,
each changed save must match independent Git staging, unchanged saves must
preserve the checkout, and dirty recovery must pass in both modes. Older reports
are rejected for comparison. These changes improve verification; they do not
implement a CLI performance optimization. Two fresh smoke reports compared
successfully, with medians of 0.238 and 0.229 seconds; this variation is not evidence
of a code improvement.

Unmodified JSON reports retain source, runner, binary, environment and workload
hashes, every sample, RSS, object growth and recovery results:

- [Synthetic profiles and Iurio](benchmark-results/2026-11-11/snapshots-and-iurio.json)
- [Venacenter](benchmark-results/2026-11-11/venacenter.json)
- [Groove](benchmark-results/2026-11-11/groove.json)
- [Scale correctness](benchmark-results/2026-11-11/correctness.json)
- [Smoke reference](benchmark-results/2026-11-11/smoke-reference.json),
  [repeat](benchmark-results/2026-11-11/smoke-repeat.json) and
  [comparison](benchmark-results/2026-11-11/comparison.json)

Validation also passed benchmark/hook tests, the opt-in real release-CLI roundtrip,
and the required workspace gates under default scheduling: formatting, Clippy,
all-target/all-feature tests, doctests, rustdoc with warnings denied and whitespace
checks. The gate reported 242 passed tests/doctests and four opt-in tests ignored,
with the roundtrip and both large scale cases also covered explicitly. The
remaining reflink-cache test could not run on this ext4 filesystem: an explicit
reflink probe returned `Operation not supported`. No macOS performance run was
performed.

The workspace gate runs these exact checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
git diff --check
```

## Historical local measurements, 2026-11-10

Local release-build measurements, 2026-11-10. Git 2.53.0, Linux, disk-backed disposable repositories. Five warm saves each changed ten tracked files. Timings include CLI startup, safety checks, and durable publication.

| Repository | Tracked files | Median changed save | Unchanged save | Peak RSS reported | Object growth, seven saves |
| --- | ---: | ---: | ---: | ---: | ---: |
| Iurio clone | 1,621 | 0.53 s | 0.36 s | 44.9 MiB | 259.1 KiB |
| Synthetic | 100,000 | 0.80 s | 0.64 s | 26.1 MiB | 28.6 KiB |
| Synthetic | 1,000,000 | 8.02 s | 7.56 s | 218.4 MiB | 135.4 KiB |

**The 100k-file target of under one second was met. The 1M-file target of under three seconds was not met.** The million-file median improved from an earlier 10.84 s run to 8.02 s after consolidating index validation and using parallel Git change discovery and explicit-path index updates. Full index scans, tree construction and untracked discovery remain substantial costs.

The implementation deliberately retains consistency checks. It does not yet have a persistent validated index cache or a filesystem-monitor integration; those are follow-up performance work, not delivered capabilities.

Every fixture passed recovery into a separate checkout and in-place recovery. The source Iurio repository’s HEAD, index bytes, status, tracked diff and refs matched before and after. Its uncommitted edits were not copied or modified.

These are local observations on a shared machine, not latency guarantees. OS caches were not flushed. Synthetic files share small baseline content; they exercise path/index scale rather than unique-content or large-binary storage. Object growth counts logical object-file bytes before recovery, excluding filesystem allocation overhead. Reported RSS is GNU time’s maximum, not aggregate memory across concurrent processes. Recovery latency is not separately measured.

[Raw measurements](snapshot-benchmark-results.json) include each sample, Git step durations, environment and CPU information. Run the current versioned workload with:

```sh
cargo build -p groove-cli --release --locked
cargo run -p groove-bench --locked -- run --mode benchmark \
  --real-repo /path/to/repository --profile large --profile million \
  --temp-root /path/on/a/large/local/disk --runs 5 \
  --output /tmp/snapshot-benchmark.json
```

The source is read-only. Disposable clones copy objects without hardlinks, and generated repositories are removed afterward. Several million available inodes are needed for the largest fixture and recovery checkout.

## Reproducible infrastructure

These measurements predate the pinned lab and remain historical, not a canonical
regression baseline. New runs use [versioned fixtures and environment](../benchmarks/snapshots/README.md),
automated large correctness tests and v2 provenance reports. The former Python scripts have been replaced by the Rust `groove-bench` workspace
package.
