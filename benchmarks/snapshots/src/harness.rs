use crate::{
    fixture::{spec, LOCK_BYTES, SPEC_BYTES},
    report,
    util::{digest, hash, require, root, write_json, Error, Result, Runner},
    RunArgs,
};
use serde_json::{json, Value};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
    time::Instant,
};

fn index_path(runner: &Runner, repo: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(OsString::from_vec(trim_newline(
        runner.git(
            repo,
            ["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?,
    ))))
}
pub(crate) fn fingerprint(runner: &Runner, repo: &Path, refs: bool) -> Result<String> {
    let mut data = vec![
        runner.git(repo, ["rev-parse", "HEAD"])?,
        runner.git(repo, ["status", "--porcelain=v1", "-z"])?,
        runner.git(repo, ["diff", "--binary", "HEAD"])?,
        fs::read(index_path(runner, repo)?)?,
    ];
    if refs {
        data.push(runner.git(repo, ["show-ref"])?);
    }
    Ok(hash(&data.join(&0)))
}
fn trim_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    bytes
}
fn object_sizes(repo: &Path) -> Result<(u64, u64)> {
    fn walk(path: &Path) -> Result<(u64, u64)> {
        let mut total = (0, 0);
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let size = if meta.is_dir() {
                walk(&entry.path())?
            } else {
                (meta.len(), meta.blocks() * 512)
            };
            total.0 += size.0;
            total.1 += size.1;
        }
        Ok(total)
    }
    walk(&repo.join(".git/objects"))
}
fn cli_command(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    args: &[OsString],
    diagnostics: bool,
    expected_error: Option<(&str, i32)>,
) -> Result<(Value, Value)> {
    #[cfg(target_os = "linux")]
    let rss = runner.temporary_file()?;
    #[cfg(target_os = "linux")]
    let mut all: Vec<OsString> = [
        OsStr::new("-q"),
        OsStr::new("-f"),
        OsStr::new("%M %U %S"),
        OsStr::new("-o"),
        rss.path().as_os_str(),
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    #[cfg(target_os = "macos")]
    let mut all: Vec<OsString> = vec!["-l".into()];
    all.extend(
        [
            binary.as_os_str(),
            OsStr::new("--repo"),
            repo.as_os_str(),
            OsStr::new("--git-timeout-ms"),
            OsStr::new("300000"),
            OsStr::new("--json"),
        ]
        .into_iter()
        .map(OsString::from),
    );
    if diagnostics {
        all.push("-vv".into());
    }
    all.extend_from_slice(args);
    let start = Instant::now();
    let out = runner.capture("/usr/bin/time", all, repo)?;
    let seconds = start.elapsed().as_secs_f64();
    require(
        out.status.code() == Some(expected_error.map_or(0, |(_, status)| status)),
        format!(
            "CLI exited {}: {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )?;
    let response: Value = serde_json::from_slice(&out.stdout)?;
    require(
        response["api_version"] == 1,
        "Unsupported CLI response version",
    )?;
    let result = if let Some((code, status)) = expected_error {
        require(
            out.status.code() == Some(status)
                && response["ok"] == false
                && response["error"]["code"] == code,
            "Unexpected CLI error/exit category",
        )?;
        response["error"].clone()
    } else {
        require(
            out.status.success() && response["ok"] == true,
            format!(
                "CLI exited {}: {response}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        )?;
        response["result"].clone()
    };
    #[cfg(target_os = "linux")]
    let resources: Vec<f64> = fs::read_to_string(rss.path())?
        .split_whitespace()
        .map(|s| {
            s.parse()
                .map_err(|e| Error::Invalid(format!("Invalid resource measurement: {e}")))
        })
        .collect::<Result<_>>()?;
    #[cfg(target_os = "linux")]
    let peak_rss = resources
        .first()
        .copied()
        .ok_or_else(|| Error::Invalid("Missing RSS".into()))? as u64;
    #[cfg(target_os = "linux")]
    require(
        resources.len() == 3 && resources.iter().all(|v| v.is_finite() && *v >= 0.0),
        "Incomplete resource measurement",
    )?;
    #[cfg(target_os = "macos")]
    let peak_rss = String::from_utf8_lossy(&out.stderr)
        .lines()
        .find(|line| line.contains("maximum resident set size"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|bytes| bytes.parse::<u64>().ok())
        .ok_or_else(|| Error::Invalid("Missing macOS peak RSS".into()))?
        / 1024;
    let steps: Vec<u64> = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|line| line.contains("Git operation finished"))
        .filter_map(|line| {
            line.split_once("elapsed_ms=")
                .and_then(|(_, s)| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse().ok())
        })
        .collect();
    if diagnostics {
        require(!steps.is_empty(), "Missing Git timing diagnostics")?;
    }
    Ok((
        result,
        json!({"seconds":seconds,"peak_rss_kib":peak_rss,"git_steps_ms":steps,
            "cpu": cpu_resources(&out.stderr, #[cfg(target_os="linux")] &resources)}),
    ))
}
pub(crate) fn command(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    args: &[OsString],
    diagnostics: bool,
) -> Result<(Value, Value)> {
    cli_command(runner, binary, repo, args, diagnostics, None)
}
pub(crate) fn reject_full_pool(runner: &Runner, binary: &Path, repo: &Path) -> Result<Value> {
    Ok(cli_command(
        runner,
        binary,
        repo,
        &strings(&["acquire", "--task", "overflow"]),
        false,
        Some(("POOL_EXHAUSTED", 3)),
    )?
    .1)
}
fn cpu_resources(_stderr: &[u8], #[cfg(target_os = "linux")] resources: &[f64]) -> Value {
    #[cfg(target_os = "linux")]
    {
        json!({"user_seconds":resources.get(1),"system_seconds":resources.get(2)})
    }
    #[cfg(target_os = "macos")]
    {
        let text = String::from_utf8_lossy(_stderr);
        let values = text.lines().find_map(|line| {
            let words: Vec<_> = line.split_whitespace().collect();
            if !words.contains(&"real") || !words.contains(&"user") || !words.contains(&"sys") {
                return None;
            }
            let number = |label: &str| {
                words
                    .iter()
                    .position(|w| *w == label)
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| words[i].parse::<f64>().ok())
            };
            Some(json!({"user_seconds":number("user"),"system_seconds":number("sys")}))
        });
        values.unwrap_or(Value::Null)
    }
}
fn invoke(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    args: &[OsString],
) -> Result<(Value, Value)> {
    let mut all = vec!["snapshot".into()];
    all.extend_from_slice(args);
    command(runner, binary, repo, &all, false)
}
fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(Into::into).collect()
}
// Ordinary full-tree staging in a separate index is independent of Groove's
// incremental snapshot implementation and never changes the checkout's index.
fn trees(runner: &Runner, repo: &Path) -> Result<(String, String)> {
    let scratch = tempfile::tempdir_in(
        repo.parent()
            .ok_or_else(|| Error::Invalid("Fixture has no scratch parent".into()))?,
    )?;
    let index = scratch.path().join("index");
    // Even write-tree can update the index's cache-tree extension. Copy the
    // actual index so validation preserves bytes as well as staging semantics.
    fs::copy(index_path(runner, repo)?, &index)?;
    let mut oracle = runner.clone();
    oracle
        .env
        .insert("GIT_INDEX_FILE".into(), index.into_os_string());
    // Oracle writes must not repair missing snapshot objects or affect measured
    // object growth. Read existing objects through an alternate; write only here.
    let objects = scratch.path().join("objects");
    fs::create_dir(&objects)?;
    oracle
        .env
        .insert("GIT_OBJECT_DIRECTORY".into(), objects.into_os_string());
    let source_objects = runner.git_text(
        repo,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
    )?;
    oracle.env.insert(
        "GIT_ALTERNATE_OBJECT_DIRECTORIES".into(),
        format!(
            "\"{}\"",
            source_objects
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
        .into(),
    );
    let staged = oracle.git_text(repo, &["write-tree"])?;
    oracle.git(repo, ["add", "-A"])?;
    Ok((staged, oracle.git_text(repo, &["write-tree"])?))
}
fn verify_trees(runner: &Runner, repo: &Path, index: &str, work: &str) -> Result<()> {
    let (actual_index, actual_work) = trees(runner, repo)?;
    require(
        actual_index == index,
        "Restored staging tree differs from saved state",
    )?;
    require(
        actual_work == work,
        "Restored working files differ from saved state",
    )
}

fn snapshot_id(saved: &Value) -> Result<&str> {
    saved["snapshot"]["id"]
        .as_str()
        .ok_or_else(|| Error::Invalid("Missing snapshot identifier".into()))
}
fn saved_trees(saved: &Value, index: &str, work: &str) -> Result<()> {
    require(
        saved["snapshot"]["index_tree"] == index && saved["snapshot"]["work_tree"] == work,
        "Saved trees differ from independent Git staging",
    )
}
fn blob_ids(runner: &Runner, repo: &Path) -> Result<std::collections::BTreeSet<String>> {
    Ok(runner
        .git_text(
            repo,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objecttype) %(objectname)",
            ],
        )?
        .lines()
        .filter(|l| l.starts_with("blob "))
        .map(str::to_owned)
        .collect())
}
fn mutate(
    runner: &Runner,
    repo: &Path,
    case: &crate::profiles::Case,
    selected: &[PathBuf],
    iteration: usize,
    mutation: crate::profiles::Mutation,
) -> Result<()> {
    use crate::profiles::Mutation;
    if mutation == Mutation::Binary {
        use std::io::{Seek, SeekFrom};
        let bytes = if let crate::profiles::Contents::Binary { bytes } = case.contents {
            bytes
        } else {
            return Err(Error::Invalid(
                "Binary mutation requires binary contents".into(),
            ));
        };
        let mut file = OpenOptions::new()
            .write(true)
            .open(repo.join(&selected[0]))?;
        file.seek(SeekFrom::Start(
            ((iteration * 65536) % (bytes - 65536 + 1)) as u64,
        ))?;
        let mut state = iteration as u64 + 42;
        let payload: Vec<u8> = (0..65536)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        file.write_all(&payload)?;
    } else {
        for name in selected {
            writeln!(
                OpenOptions::new().append(true).open(repo.join(name))?,
                "\nprofile-v1:{iteration}"
            )?;
        }
    }
    if mutation == Mutation::Mixed {
        let staged = &selected[0];
        runner.git(
            repo,
            [OsStr::new("add"), OsStr::new("--"), staged.as_os_str()],
        )?;
        writeln!(
            OpenOptions::new().append(true).open(repo.join(staged))?,
            "unstaged-{iteration}"
        )?;
        if iteration == 0 {
            let deleted = PathBuf::from(spec().path(case.files - 1));
            require(
                !selected.contains(&deleted),
                "Deletion must be independent of edited paths",
            )?;
            fs::remove_file(repo.join(deleted))?;
            fs::create_dir(repo.join("ignored"))?;
            for i in 0..1000 {
                fs::write(repo.join(format!("ignored/{i}")), b"cache")?;
            }
        }
        fs::write(repo.join("untracked-binary"), [0, 255, iteration as u8])?;
    }
    Ok(())
}
fn snapshot_save(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &crate::profiles::Case,
    selected: &[PathBuf],
    history_depth: usize,
    mutation: crate::profiles::Mutation,
) -> Result<crate::statistics::Observation> {
    use crate::evidence::{Evidence, Storage};
    let mut evidence = Evidence::default();
    let tree = runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?;
    let state = fingerprint(runner, repo, false)?;
    let sizes = object_sizes(repo)?;
    let (first, time) = invoke(runner, binary, repo, &strings(&["save", "first"]))?;
    saved_trees(&first, &tree, &tree)?;
    require(
        fingerprint(runner, repo, false)? == state,
        "First save changed checkout",
    )?;
    evidence.record("first_save", time)?;
    let mut latest = first;
    let (mut expected_index, mut expected_work) = (tree.clone(), tree);
    for iteration in 0..history_depth {
        mutate(runner, repo, case, selected, iteration, mutation)?;
        let state = fingerprint(runner, repo, false)?;
        let (saved, time) = invoke(runner, binary, repo, &strings(&["save", "changed"]))?;
        require(
            fingerprint(runner, repo, false)? == state,
            "Changed save modified checkout/index",
        )?;
        (expected_index, expected_work) = trees(runner, repo)?;
        saved_trees(&saved, &expected_index, &expected_work)?;
        evidence.record(
            if iteration == 0 {
                "fresh_changed"
            } else {
                "history_changed"
            },
            time,
        )?;
        latest = saved;
    }
    evidence.checked("checkout_preserved");
    evidence.checked("saved_trees");
    let before_blobs = blob_ids(runner, repo)?;
    let state = fingerprint(runner, repo, false)?;
    let (unchanged, time) = invoke(runner, binary, repo, &strings(&["save", "unchanged"]))?;
    saved_trees(&unchanged, &expected_index, &expected_work)?;
    require(
        fingerprint(runner, repo, false)? == state,
        "Unchanged save modified checkout/index",
    )?;
    require(
        blob_ids(runner, repo)? == before_blobs,
        "Unchanged save created duplicate blobs",
    )?;
    evidence.record("unchanged_save", time)?;
    evidence.checked("unchanged_blob_reuse");
    let after = object_sizes(repo)?;
    evidence.object_growth = Some(Storage {
        logical_bytes: after
            .0
            .checked_sub(sizes.0)
            .ok_or_else(|| Error::Invalid("Snapshot unexpectedly removed objects".into()))?,
        allocated_bytes: after
            .1
            .checked_sub(sizes.1)
            .ok_or_else(|| Error::Invalid("Snapshot unexpectedly removed storage".into()))?,
    });
    // Recovery is an independent correctness oracle here, outside save timings.
    let recovered = repo
        .parent()
        .expect("owned fixture parent")
        .join("recovered");
    invoke(
        runner,
        binary,
        repo,
        &[
            "restore".into(),
            snapshot_id(&latest)?.into(),
            "--to".into(),
            recovered.as_os_str().to_owned(),
        ],
    )?;
    verify_trees(runner, &recovered, &expected_index, &expected_work)?;
    evidence.checked("recovery_trees");
    crate::evidence::observation(case, evidence)
}
fn snapshot_restore(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &crate::profiles::Case,
    selected: &[PathBuf],
) -> Result<crate::statistics::Observation> {
    let mut evidence = crate::evidence::Evidence::default();
    let head = runner.git_text(repo, &["rev-parse", "HEAD"])?;
    let tree = runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?;
    let state = fingerprint(runner, repo, false)?;
    let (clean, _) = invoke(runner, binary, repo, &strings(&["save", "clean"]))?;
    saved_trees(&clean, &tree, &tree)?;
    require(
        fingerprint(runner, repo, false)? == state,
        "Clean capture changed source",
    )?;
    mutate(
        runner,
        repo,
        case,
        selected,
        0,
        crate::profiles::Mutation::Mixed,
    )?;
    let state = fingerprint(runner, repo, false)?;
    let (dirty, _) = invoke(runner, binary, repo, &strings(&["save", "dirty"]))?;
    require(
        fingerprint(runner, repo, false)? == state,
        "Dirty capture changed source",
    )?;
    let (index, work) = trees(runner, repo)?;
    saved_trees(&dirty, &index, &work)?;
    evidence.checked("source_preserved");
    for (label, saved, expected_index, expected_work) in [
        ("clean", &clean, &tree, &tree),
        ("dirty", &dirty, &index, &work),
    ] {
        let recovered = repo
            .parent()
            .expect("owned fixture parent")
            .join(format!("recovered-{label}"));
        let (_, time) = invoke(
            runner,
            binary,
            repo,
            &[
                "restore".into(),
                snapshot_id(saved)?.into(),
                "--to".into(),
                recovered.as_os_str().to_owned(),
            ],
        )?;
        evidence.record(&format!("{label}_external_restore"), time)?;
        verify_trees(runner, &recovered, expected_index, expected_work)?;
        require(
            runner.git_text(&recovered, &["rev-parse", "HEAD"])? == head
                && !recovered.join("ignored").exists(),
            "External restore changed base or copied ignored residue",
        )?;
        let (_, time) = invoke(
            runner,
            binary,
            repo,
            &strings(&["restore", snapshot_id(saved)?]),
        )?;
        evidence.record(&format!("{label}_inplace_restore"), time)?;
        verify_trees(runner, repo, expected_index, expected_work)?;
        require(
            runner.git_text(repo, &["rev-parse", "HEAD"])? == head,
            "In-place recovery changed base",
        )?;
        evidence.checked(&format!("{label}_recovery"));
    }
    require(
        fs::read(repo.join("ignored/0"))? == b"cache",
        "In-place restore destroyed ignored residue",
    )?;
    evidence.checked("ignored_preserved");
    crate::evidence::observation(case, evidence)
}
fn execute_case(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &crate::profiles::Case,
    selected: &[PathBuf],
) -> Result<crate::statistics::Observation> {
    match case.workload {
        crate::profiles::Workload::SnapshotSave {
            history_depth,
            mutation,
        } => snapshot_save(
            runner,
            binary,
            repo,
            case,
            selected,
            history_depth,
            mutation,
        ),
        crate::profiles::Workload::SnapshotRestore => {
            snapshot_restore(runner, binary, repo, case, selected)
        }
        _ => crate::lifecycle::measure(runner, binary, repo, case),
    }
}
fn collect_case(
    args: &RunArgs,
    runner: &Runner,
    candidate: &Path,
    baseline: Option<&Path>,
    profile: &crate::profiles::Profile,
    case: &crate::profiles::Case,
    report: &mut Value,
) -> Result<()> {
    let key = format!("{}/{}", profile.id, case.id);
    report["cases"][&key] = json!({"identity":null,"blocks":[]});
    let (warmup, runs) = if args.mode == crate::Mode::Check {
        (0, 1)
    } else {
        (profile.sampling.warmup_blocks, profile.sampling.blocks)
    };
    for iteration in 0..warmup + runs {
        let measured = iteration >= warmup;
        let block = iteration.saturating_sub(warmup);
        let order = if baseline.is_none() {
            vec!["candidate"]
        } else if block % 2 == 0 {
            vec!["baseline", "candidate"]
        } else {
            vec!["candidate", "baseline"]
        };
        let (mut before, mut after) = (None, None);
        for side in &order {
            let binary = if *side == "baseline" {
                baseline.expect("paired order has baseline")
            } else {
                candidate
            };
            report["progress"] = json!({"profile":profile.id,"case":case.id,"phase":if measured{"measurement"}else{"warmup"},"block":if measured{block+1}else{iteration+1},"blocks":if measured{runs}else{warmup},"binary":side});
            write_json(&args.plan.output, report)?;
            eprintln!(
                "{}/{}: {} {}/{} ({side})",
                profile.id,
                case.id,
                if measured { "repetition" } else { "warmup" },
                if measured { block + 1 } else { iteration + 1 },
                if measured { runs } else { warmup }
            );
            let scratch = tempfile::Builder::new()
                .prefix("groove-bench-block-")
                .tempdir_in(&args.plan.temp_root)?;
            let local = runner.isolated(scratch.path())?;
            let outcome = (|| -> Result<_> {
                let (bytes, inodes) = case.scratch_budget();
                crate::report::preflight(&local, scratch.path(), bytes, inodes)?;
                let repo = scratch.path().join("repo");
                let (_, selected) = spec().create_workload(&local, &repo, case)?;
                crate::lifecycle::prepare(&local, &repo, case)?;
                let identity = local.git_text(&repo, &["rev-parse", "HEAD^{tree}"])?;
                let observation = execute_case(&local, binary, &repo, case, &selected)?;
                Ok((identity, observation))
            })();
            let (identity, observation) = match outcome {
                Err(error @ (Error::Interrupted | Error::Limit { .. } | Error::WorkerPanic)) => {
                    return Err(Error::Retained {
                        path: scratch.keep(),
                        source: Box::new(error),
                    })
                }
                result => result?,
            };
            let recorded = &report["cases"][&key]["identity"];
            require(
                recorded.is_null() || recorded == &identity,
                "Fixture identity drifted between blocks/binaries",
            )?;
            report["cases"][&key]["identity"] = json!(identity);
            if *side == "baseline" {
                before = Some(observation)
            } else {
                after = Some(observation)
            };
        }
        if measured {
            let measured = crate::statistics::Block {
                order: order.into_iter().map(str::to_owned).collect(),
                candidate: after.expect("candidate measured"),
                baseline: before,
            };
            report["cases"][&key]["blocks"]
                .as_array_mut()
                .expect("case initialized with blocks")
                .push(serde_json::to_value(measured)?);
            write_json(&args.plan.output, report)?;
        }
    }
    eprintln!("Completed {key}");
    Ok(())
}
pub(crate) fn run(args: RunArgs) -> Result<()> {
    write_json(
        &args.plan.output,
        &json!({"status":"running","phase":"preflight"}),
    )?;
    let output = args.plan.output.clone();
    let result = run_inner(args);
    if let Err(error) = &result {
        report::record_failure(&output, error)?;
    }
    result
}
fn run_inner(args: RunArgs) -> Result<()> {
    let profiles = crate::profiles::select(&args.plan.profiles)?;
    require(
        args.mode != crate::Mode::Check || args.baseline.is_none(),
        "Correctness checks do not compare baselines",
    )?;
    fs::create_dir_all(&args.plan.temp_root)?;
    let candidate = args
        .binary
        .clone()
        .unwrap_or_else(|| root().join("target/release/groove"))
        .canonicalize()?;
    let baseline = args.baseline.as_ref().map(fs::canonicalize).transpose()?;
    let runtime = args
        .runtime_contract
        .as_ref()
        .map(|p| -> Result<Value> { Ok(serde_json::from_slice(&fs::read(p)?)?) })
        .transpose()?;
    let mut runner = Runner::new();
    if runtime.is_some() {
        runner.env.insert(
            "PATH".into(),
            "/opt/git/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin".into(),
        );
    }
    let source = Path::new("/opt/benchmark-source.json");
    let source = if source.is_file() {
        serde_json::from_slice::<Value>(&fs::read(source)?)?
    } else {
        json!({"kind":"local-exploration"})
    };
    let binary_record =
        |path: &Path| -> Result<Value> { Ok(json!({"sha256":digest(path)?,"path":path})) };
    let mut definitions = serde_json::Map::new();
    for profile in &profiles {
        definitions.insert(
            profile.id.clone(),
            json!({"sha256":profile.sha256()?,"definition":profile}),
        );
    }
    let mut report = json!({"schema_version":4,"measurement_version":4,"status":"running","mode":args.mode,
        "fixture_version":spec().fixture_version,"fixture_sha256":hash(SPEC_BYTES),"catalog_sha256":hash(crate::profiles::CATALOG_BYTES),"environment_lock_sha256":hash(LOCK_BYTES),
        "runner_sha256":digest(&std::env::current_exe()?)?,"source":source,"profiles":definitions,
        "methodology":{"sampling":"independent fixture blocks; AB/BA alternation; paired block bootstrap, 4000 resamples, pointwise 95% intervals","practical_change_fraction":0.05,"minimum_verdict_blocks":6,
        "cache_policy":"no host cache eviction; profile-owned warmup blocks; Git oracles warm filesystem caches; first save is application-cold, not OS-cold",
        "rss_scope":"largest collected per-process peak, not simultaneous process-tree memory","limitations":["CPU affinity is not exclusive reservation; use a quiet host","intervals are pointwise, not familywise guarantees","history and concurrent-worker samples are aggregated within blocks"]},
        "binaries":{"candidate":binary_record(&candidate)?,"baseline":baseline.as_deref().map(binary_record).transpose()?},"environment":null,"cases":{},"summary":null,"progress":null});
    write_json(&args.plan.output, &report)?;
    let outcome = (|| -> Result<()> {
        report["environment"] =
            report::environment(&runner, &args.plan.temp_root, runtime.as_ref())?;
        let calibration = tempfile::tempdir_in(&args.plan.temp_root)?;
        let local = runner.isolated(calibration.path())?;
        let mut floor = Vec::new();
        for _ in 0..10 {
            floor.push(
                command(
                    &local,
                    &candidate,
                    calibration.path(),
                    &strings(&["--version"]),
                    false,
                )?
                .1,
            );
        }
        report["calibration"] = json!({"cli_version_samples":floor,"scope":"wrapper, CLI startup, capture files and 1ms exit polling; no subtraction"});
        for profile in &profiles {
            for case in &profile.cases {
                collect_case(
                    &args,
                    &runner,
                    &candidate,
                    baseline.as_deref(),
                    profile,
                    case,
                    &mut report,
                )?;
            }
        }
        report["environment_after"] =
            report::environment(&runner, &args.plan.temp_root, runtime.as_ref())?;
        report["status"] = json!("passed");
        report["progress"] = Value::Null;
        report::summarize(&mut report)?;
        Ok(())
    })();
    if let Err(error) = &outcome {
        report["status"] = json!("failed");
        report["error"] = json!(error.to_string());
    }
    write_json(&args.plan.output, &report)?;
    outcome
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_oracle_detects_lost_edits_and_staging_without_changing_the_index() {
        let temp = tempfile::tempdir().unwrap();
        let runner = Runner::new().isolated(temp.path()).unwrap();
        let repo = temp.path().join("repo");
        let spec = spec();
        spec.create(&runner, &repo, 3).unwrap();
        let edited = spec.path(0);
        fs::write(repo.join(&edited), "staged\n").unwrap();
        runner.git(&repo, ["add", "--", &edited]).unwrap();
        fs::write(repo.join(&edited), "working\n").unwrap();
        fs::remove_file(repo.join(spec.path(1))).unwrap();
        fs::write(repo.join("untracked"), b"binary\0\xff").unwrap();
        let index_bytes = fs::read(repo.join(".git/index")).unwrap();
        let object_bytes = object_sizes(&repo).unwrap();
        let (index, work) = trees(&runner, &repo).unwrap();
        assert_eq!(
            object_sizes(&repo).unwrap(),
            object_bytes,
            "Oracle must not add objects to the measured repository"
        );
        assert_ne!(index, work);
        verify_trees(&runner, &repo, &index, &work).unwrap();
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index_bytes);

        fs::remove_file(repo.join("untracked")).unwrap();
        assert!(verify_trees(&runner, &repo, &index, &work)
            .unwrap_err()
            .to_string()
            .contains("working files"));
        fs::write(repo.join("untracked"), b"binary\0\xff").unwrap();
        runner.git(&repo, ["add", "-A"]).unwrap();
        assert!(verify_trees(&runner, &repo, &index, &work)
            .unwrap_err()
            .to_string()
            .contains("staging tree"));
    }
}
