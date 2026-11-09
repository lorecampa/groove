# Snapshot benchmark results

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

[Raw measurements](snapshot-benchmark-results.json) include each sample, Git step durations, environment and CPU information. Reproduce with:

```sh
cargo build -p groove-cli --release --locked
python3 scripts/benchmark-snapshots.py \
  --real-repo /path/to/repository --sizes 100000 1000000 \
  --temp-root /path/on/a/large/local/disk --runs 5 \
  --output /tmp/snapshot-benchmark.json
```

The source is read-only. Disposable clones copy objects without hardlinks, and generated repositories are removed afterward. Several million available inodes are needed for the largest fixture and recovery checkout.

## Reproducible infrastructure

These measurements predate the pinned lab and remain historical, not a canonical
regression baseline. New runs use [versioned fixtures and environment](../benchmarks/snapshots/README.md),
automated large correctness tests and v2 provenance reports. The former script
now delegates to that harness; it no longer maintains a separate implementation.
