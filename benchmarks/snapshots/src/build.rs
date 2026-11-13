//! Offline build workflows. The caller/build tool owns execution and caching.
use crate::{
    evidence::{Evidence, Storage, Timing},
    harness::{command, fingerprint, timed_process, verify_trees},
    profiles::{BuildStrategy, Case},
    statistics::Observation,
    util::{require, Result, Runner},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(crate) const PHASES: [&str; 6] = [
    "first",
    "repeat",
    "source_change",
    "flags_change",
    "return",
    "cache_removed",
];
/// All permutations put each strategy in each position twice per six rounds.
pub(crate) fn case_order(round: u32) -> [usize; 3] {
    const ORDER: [[usize; 3]; 6] = [
        [0, 1, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
        [1, 0, 2],
        [0, 2, 1],
    ];
    ORDER[round as usize % ORDER.len()]
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildEvidence {
    pub(crate) tools: BTreeMap<String, String>,
    phases: Vec<Phase>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Phase {
    name: String,
    output: String,
    hits: u64,
    misses: u64,
    workspace_output: Storage,
    external_cache: Storage,
}

fn expected(units: usize, input: u32, factor: u32) -> String {
    let mut total = 0_u32;
    for unit in 0..units {
        let mut x = input.wrapping_mul(factor);
        for step in 0..64 {
            x = (x ^ (unit * 64 + step) as u32)
                .wrapping_mul(1664525)
                .wrapping_add(1013904223);
        }
        total = total.wrapping_add(x);
    }
    format!("{total}\n")
}
fn tooling(runner: &Runner, repo: &Path) -> Result<BTreeMap<String, String>> {
    ["cc", "make", "ccache"]
        .into_iter()
        .map(|tool| {
            let output = runner.run(tool, ["--version"], repo)?;
            Ok((
                tool.into(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            ))
        })
        .collect()
}
pub(crate) fn prepare(runner: &Runner, repo: &Path, units: usize) -> Result<()> {
    // Check prerequisites before creating the build history. All subprocesses use
    // the lab's cleared environment, bounded output and process-group teardown.
    tooling(runner, repo)?;
    let source = repo.join("buildsrc");
    fs::create_dir(&source)?;
    fs::write(source.join("config.h"), "#define INPUT 7u\n")?;
    let mut main = String::from("#include <stdint.h>\n#include <stdio.h>\n#include \"config.h\"\n");
    for unit in 0..units {
        main.push_str(&format!("uint32_t unit_{unit}(uint32_t);\n"));
        let mut body = format!(
            "#include <stdint.h>\n#include \"config.h\"\nuint32_t unit_{unit}(uint32_t x) {{\n"
        );
        for step in 0..64 {
            body.push_str(&format!(
                "x = (x ^ {}u) * 1664525u + 1013904223u;\n",
                unit * 64 + step
            ));
        }
        body.push_str("return x;\n}\n");
        fs::write(source.join(format!("unit_{unit:03}.c")), body)?;
    }
    main.push_str("int main(void) { uint32_t sum = 0;\n");
    for unit in 0..units {
        main.push_str(&format!("sum += unit_{unit}(INPUT * FACTOR);\n"));
    }
    main.push_str("printf(\"%u\\n\", (unsigned)sum); return 0; }\n");
    fs::write(source.join("main.c"), main)?;
    fs::write(
        repo.join("Makefile"),
        concat!(
            "SOURCES := $(wildcard buildsrc/*.c)\n",
            "OBJECTS := $(patsubst buildsrc/%.c,ignored/build/%.o,$(SOURCES))\n",
            "all: ignored/build/app\n",
            "ignored/build/%.o: buildsrc/%.c buildsrc/config.h Makefile\n",
            "\t@mkdir -p ignored/build\n\t$(CC) $(CFLAGS) -c $< -o $@\n",
            "ignored/build/app: $(OBJECTS)\n\t$(CC) $(OBJECTS) -o $@\n",
        ),
    )?;
    runner.git(repo, ["add", "-A"])?;
    runner.git(repo, ["commit", "-qm", "offline-build-a-v1"])?;
    runner.git(repo, ["tag", "build-a"])?;
    fs::write(source.join("config.h"), "#define INPUT 11u\n")?;
    runner.git(repo, ["add", "buildsrc/config.h"])?;
    runner.git(repo, ["commit", "-qm", "offline-build-b-v1"])?;
    runner.git(repo, ["tag", "build-b"])?;
    runner.git(repo, ["reset", "--hard", "build-a"])?;
    Ok(())
}
fn call(runner: &Runner, binary: &Path, repo: &Path, args: &[&str]) -> Result<(Value, Value)> {
    command(
        runner,
        binary,
        repo,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        false,
    )
}
fn storage(path: &Path) -> Result<Storage> {
    let mut total = Storage {
        logical_bytes: 0,
        allocated_bytes: 0,
    };
    if path.exists() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            require(
                !metadata.is_symlink(),
                "Unexpected symlink in build/cache output",
            )?;
            let size = if metadata.is_dir() {
                storage(&entry.path())?
            } else {
                Storage {
                    logical_bytes: metadata.len(),
                    allocated_bytes: metadata.blocks() * 512,
                }
            };
            total.logical_bytes += size.logical_bytes;
            total.allocated_bytes += size.allocated_bytes;
        }
    }
    Ok(total)
}
fn cached_runner(runner: &Runner, root: &Path) -> Result<Runner> {
    fs::create_dir_all(root)?;
    let config = root.join("ccache.conf");
    fs::write(
        &config,
        concat!(
            "compiler_check = content\nhard_link = false\nfile_clone = false\n",
            // Fresh checkout headers deliberately have recent mtimes. Preprocessor
            // mode avoids direct-mode timestamp heuristics without sloppiness.
            "direct_mode = false\nmax_size = 1G\n",
        ),
    )?;
    let mut cached = runner.clone();
    cached
        .env
        .insert("CCACHE_CONFIGPATH".into(), config.into_os_string());
    cached
        .env
        .insert("CCACHE_DIR".into(), root.join("cache").into_os_string());
    cached
        .env
        .insert("CCACHE_TEMPDIR".into(), root.join("tmp").into_os_string());
    Ok(cached)
}
fn counters(runner: &Runner, repo: &Path) -> Result<(u64, u64)> {
    let output = runner.run("ccache", ["--print-stats"], repo)?;
    let mut values = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        if let (Some(key), Some(value)) = (fields.next(), fields.next()) {
            if let Ok(value) = value.parse::<u64>() {
                values.insert(key.to_owned(), value);
            }
        }
    }
    let get = |key| {
        values
            .get(key)
            .copied()
            .ok_or_else(|| crate::util::Error::Invalid(format!("Missing ccache counter {key}")))
    };
    Ok((
        get("direct_cache_hit")? + get("preprocessed_cache_hit")?,
        get("cache_miss")?,
    ))
}
fn build(runner: &Runner, workspace: &Path, external: bool, factor: u32) -> Result<Value> {
    let args = [
        "-j4".into(),
        format!("CC={}", if external { "ccache cc" } else { "cc" }).into(),
        format!("CFLAGS=-O2 -std=c99 -DFACTOR={factor}").into(),
    ];
    let (output, timing) = timed_process(runner, OsStr::new("make"), workspace, &args)?;
    require(
        output.status.success(),
        format!(
            "Build failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(timing)
}
pub(crate) fn measure(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &Case,
    strategy: BuildStrategy,
    units: usize,
) -> Result<Observation> {
    let before = fingerprint(runner, repo, true)?;
    let root = repo
        .parent()
        .expect("fixture repository has a parent")
        .join("compiler-cache");
    let cached = cached_runner(runner, &root)?;
    let external = strategy == BuildStrategy::ExternalCache;
    let tools = tooling(runner, repo)?;
    let mut evidence = Evidence::default();
    let mut phases = Vec::new();
    let cleanup = if strategy == BuildStrategy::Warm {
        "warm"
    } else {
        "pristine"
    };
    crate::lifecycle::init_pool(
        runner,
        binary,
        repo,
        None,
        &["init", "--capacity", "1", "--cleanup", cleanup],
    )?;
    for (index, name) in PHASES.into_iter().enumerate() {
        if name == "cache_removed" {
            // Simulate an unavailable disposable cache outside timed operations.
            // The root was created by this block and is outside every pool root.
            if root.join("cache").exists() {
                fs::remove_dir_all(root.join("cache"))?;
            }
        }
        let changed = matches!(name, "source_change" | "flags_change");
        let input = if changed { 11 } else { 7 };
        let factor = if name == "flags_change" { 2 } else { 1 };
        let base = if changed { "build-b" } else { "build-a" };
        let (lease, acquire) = call(
            runner,
            binary,
            repo,
            &["acquire", "--base", base, "--task", "build"],
        )?;
        evidence.record("acquire", acquire)?;
        let workspace = PathBuf::from(
            lease["worktree_path"]
                .as_str()
                .ok_or_else(|| crate::util::Error::Invalid("Missing workspace".into()))?,
        );
        let output_root = workspace.join("ignored");
        require(
            output_root.join("build/app").exists()
                == (index > 0 && strategy == BuildStrategy::Warm),
            "Wrong cleanup of prior build output",
        )?;
        if name == "cache_removed" && output_root.exists() {
            fs::remove_dir_all(&output_root)?;
        }
        let tree = runner.git_text(repo, &["rev-parse", &format!("{base}^{{tree}}")])?;
        verify_trees(runner, &workspace, &tree, &tree)?;
        require(
            runner.git_text(&workspace, &["rev-parse", "HEAD"])?
                == runner.git_text(repo, &["rev-parse", base])?,
            "Wrong build baseline",
        )?;
        let symbolic = runner.capture("git", ["symbolic-ref", "-q", "HEAD"], &workspace)?;
        require(
            symbolic.status.code() == Some(1) && symbolic.stdout.is_empty(),
            "Build checkout is not detached",
        )?;
        let selected = if external { &cached } else { runner };
        let old = if external {
            counters(selected, &workspace)?
        } else {
            (0, 0)
        };
        evidence.record("build", build(selected, &workspace, external, factor)?)?;
        let (output, validation) = timed_process(
            runner,
            workspace.join("ignored/build/app").as_os_str(),
            &workspace,
            &[],
        )?;
        require(
            output.status.success() && output.stdout == expected(units, input, factor).as_bytes(),
            "Executed build output differs from independent specification",
        )?;
        evidence.record("validate", validation)?;
        let new = if external {
            counters(selected, &workspace)?
        } else {
            (0, 0)
        };
        require(
            new.0 >= old.0 && new.1 >= old.1,
            "Cache counters went backwards",
        )?;
        for entry in fs::read_dir(workspace.join("ignored/build"))? {
            require(
                entry?.metadata()?.nlink() == 1,
                "Build output has shared mutable hardlinks",
            )?;
        }
        verify_trees(runner, &workspace, &tree, &tree)?;
        phases.push(Phase {
            name: name.into(),
            output: String::from_utf8_lossy(&output.stdout).into_owned(),
            hits: new.0 - old.0,
            misses: new.1 - old.1,
            workspace_output: storage(&output_root)?,
            external_cache: storage(&root.join("cache"))?,
        });
        let id = lease["lease_id"]
            .as_str()
            .ok_or_else(|| crate::util::Error::Invalid("Missing lease ID".into()))?;
        evidence.record(
            "release",
            call(runner, binary, repo, &["release", "--lease-id", id])?.1,
        )?;
    }
    require(
        call(runner, binary, repo, &["list"])?.0["leases"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "Build workflow retained a reservation",
    )?;
    require(
        before == fingerprint(runner, repo, true)?,
        "Build workflow changed source checkout or refs",
    )?;
    for check in [
        "build_output",
        "full_checkout_trees",
        "cleanup_policy",
        "independent_outputs",
        "cache_loss",
        "released",
        "source_preserved",
    ] {
        evidence.checked(check);
    }
    evidence.build = Some(BuildEvidence { tools, phases });
    crate::evidence::observation(case, evidence)
}

/// Recompute phase-specific metrics from raw evidence, including total user cost.
pub(crate) fn metrics(
    strategy: BuildStrategy,
    units: usize,
    build: &BuildEvidence,
    timings: &BTreeMap<String, Vec<Timing>>,
    metrics: &mut BTreeMap<String, f64>,
) -> Result<()> {
    require(
        build
            .tools
            .keys()
            .map(String::as_str)
            .eq(["cc", "ccache", "make"])
            && build.tools.values().all(|v| !v.is_empty()),
        "Missing build tool provenance",
    )?;
    require(build.phases.len() == PHASES.len(), "Missing build phases")?;
    for (index, (phase, name)) in build.phases.iter().zip(PHASES).enumerate() {
        require(
            phase.workspace_output.logical_bytes > 0 && phase.workspace_output.allocated_bytes > 0,
            "Missing build output storage",
        )?;
        let input = if matches!(name, "source_change" | "flags_change") {
            11
        } else {
            7
        };
        let factor = if name == "flags_change" { 2 } else { 1 };
        require(
            phase.name == name && phase.output == expected(units, input, factor),
            "Incorrect recorded build output/phase",
        )?;
        if strategy == BuildStrategy::ExternalCache {
            require(
                phase.external_cache.logical_bytes > 0,
                "Missing external cache contents",
            )?;
            if matches!(name, "first" | "cache_removed") {
                require(
                    phase.hits == 0 && phase.misses == units as u64 + 1,
                    "Cold cache did not compile every source",
                )?;
            } else if matches!(name, "repeat" | "return") {
                require(
                    phase.hits == units as u64 + 1 && phase.misses == 0,
                    "Expected cache reuse was not observed",
                )?;
            } else {
                require(
                    phase.misses > 0,
                    "Changed source/flags reused stale build output",
                )?;
            }
        } else {
            require(
                phase.hits == 0 && phase.misses == 0 && phase.external_cache.logical_bytes == 0,
                "Uncached strategy used an external cache",
            )?;
        }
        let mut ready = 0.0;
        for stage in ["acquire", "build", "validate", "release"] {
            let timing = &timings[stage][index];
            metrics.insert(format!("{name}_{stage}_seconds"), timing.seconds);
            if stage != "release" {
                ready += timing.seconds;
            }
        }
        metrics.insert(format!("{name}_ready_seconds"), ready);
        metrics.insert(
            format!("{name}_total_seconds"),
            ready + timings["release"][index].seconds,
        );
        for (label, size) in [
            ("workspace_output", &phase.workspace_output),
            ("external_cache", &phase.external_cache),
        ] {
            metrics.insert(
                format!("{name}_{label}_logical_bytes"),
                size.logical_bytes as f64,
            );
            metrics.insert(
                format!("{name}_{label}_allocated_bytes"),
                size.allocated_bytes as f64,
            );
        }
        metrics.insert(format!("{name}_cache_hits"), phase.hits as f64);
        metrics.insert(format!("{name}_cache_misses"), phase.misses as f64);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_schedule_balances_positions_without_repeating_a_permutation() {
        let orders: std::collections::BTreeSet<_> = (0..6).map(case_order).collect();
        assert_eq!(orders.len(), 6);
        for order in &orders {
            assert_eq!(
                order
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                std::collections::BTreeSet::from([0, 1, 2])
            );
        }
        for strategy in 0..3 {
            for position in 0..3 {
                assert_eq!(
                    orders
                        .iter()
                        .filter(|order| order[position] == strategy)
                        .count(),
                    2
                );
            }
        }
    }

    #[test]
    #[ignore = "requires cc, GNU make and ccache; real concurrent offline builds"]
    fn shared_compiler_cache_preserves_independent_outputs_and_checks_backdated_headers() {
        let temp = tempfile::tempdir().unwrap();
        let mut runner = Runner::new().isolated(temp.path()).unwrap();
        runner.timeout = std::time::Duration::from_secs(30);
        let a = temp.path().join("a");
        crate::fixture::spec().create(&runner, &a, 12).unwrap();
        prepare(&runner, &a, 4).unwrap();
        let b = temp.path().join("b");
        runner
            .git(
                temp.path(),
                [
                    OsStr::new("clone"),
                    OsStr::new("--no-hardlinks"),
                    a.as_os_str(),
                    b.as_os_str(),
                ],
            )
            .unwrap();
        let cached = cached_runner(&runner, &temp.path().join("external-cache")).unwrap();
        let (ready, observed) = std::sync::mpsc::channel();
        let (start_a, wait_a) = std::sync::mpsc::channel();
        let (start_b, wait_b) = std::sync::mpsc::channel();
        let worker = |workspace: &Path, start: std::sync::mpsc::Receiver<()>| {
            ready.send(()).unwrap();
            start
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap();
            build(&cached, workspace, true, 1).unwrap();
        };
        std::thread::scope(|scope| {
            let first = scope.spawn(|| worker(&a, wait_a));
            let second = scope.spawn(|| worker(&b, wait_b));
            for _ in 0..2 {
                observed
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .unwrap();
            }
            start_a.send(()).unwrap();
            start_b.send(()).unwrap();
            first.join().unwrap();
            second.join().unwrap();
        });
        let run = |workspace: &Path, input| {
            let output = runner
                .run(
                    workspace.join("ignored/build/app"),
                    std::iter::empty::<&str>(),
                    workspace,
                )
                .unwrap();
            assert_eq!(output.stdout, expected(4, input, 1).as_bytes());
        };
        run(&a, 7);
        run(&b, 7);
        let object = "ignored/build/unit_000.o";
        assert_eq!(fs::metadata(a.join(object)).unwrap().nlink(), 1);
        assert_eq!(fs::metadata(b.join(object)).unwrap().nlink(), 1);
        let independent = fs::read(b.join(object)).unwrap();
        fs::write(a.join(object), "one task modified its object").unwrap();
        assert_eq!(fs::read(b.join(object)).unwrap(), independent);
        fs::remove_dir_all(a.join("ignored/build")).unwrap();
        build(&cached, &a, true, 1).unwrap();
        run(&a, 7); // The cache entry must survive the first task's in-place write.

        let header = b.join("buildsrc/config.h");
        fs::write(&header, "#define INPUT 11u\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&header)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(60)),
            )
            .unwrap();
        // Clean outputs deliberately: make itself is timestamp based. This
        // checks content validation by the compiler cache, not a promise that
        // arbitrary warm build outputs are valid after backdating source files.
        fs::remove_dir_all(b.join("ignored/build")).unwrap();
        build(&cached, &b, true, 1).unwrap();
        run(&b, 11);
        run(&a, 7);
    }
}
