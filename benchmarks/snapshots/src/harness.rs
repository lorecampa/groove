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
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, PermissionsExt},
    },
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
fn fingerprint(runner: &Runner, repo: &Path, refs: bool) -> Result<String> {
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
fn invoke(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    args: &[OsString],
) -> Result<(Value, Value)> {
    #[cfg(target_os = "linux")]
    let rss = tempfile::NamedTempFile::new()?;
    #[cfg(target_os = "linux")]
    let mut all: Vec<OsString> = [
        OsStr::new("-f"),
        OsStr::new("%M"),
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
            OsStr::new("-vv"),
            OsStr::new("--repo"),
            repo.as_os_str(),
            OsStr::new("--git-timeout-ms"),
            OsStr::new("300000"),
            OsStr::new("--json"),
            OsStr::new("snapshot"),
        ]
        .into_iter()
        .map(OsString::from),
    );
    all.extend_from_slice(args);
    let start = Instant::now();
    let out = runner.run("/usr/bin/time", all, repo)?;
    let seconds = start.elapsed().as_secs_f64();
    let response: Value = serde_json::from_slice(&out.stdout)?;
    if response["ok"] != true {
        return Err(Error::Invalid(format!(
            "Snapshot command failed: {response}"
        )));
    }
    let result = response["result"].clone();
    #[cfg(target_os = "linux")]
    let peak_rss: u64 = fs::read_to_string(rss.path())?
        .trim()
        .parse()
        .map_err(|e| Error::Invalid(format!("Invalid RSS: {e}")))?;
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
    require(!steps.is_empty(), "Missing Git timing diagnostics")?;
    Ok((
        result,
        json!({"seconds":seconds,"peak_rss_kib":peak_rss,"git_steps_ms":steps}),
    ))
}
fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(Into::into).collect()
}
// Ordinary full-tree staging in a separate index is independent of Groove's
// incremental snapshot implementation and never changes the checkout's index.
fn trees(runner: &Runner, repo: &Path) -> Result<(String, String)> {
    let scratch = tempfile::tempdir()?;
    let index = scratch.path().join("index");
    // Even write-tree can update the index's cache-tree extension. Copy the
    // actual index so validation preserves bytes as well as staging semantics.
    fs::copy(index_path(runner, repo)?, &index)?;
    let mut oracle = runner.clone();
    oracle
        .env
        .insert("GIT_INDEX_FILE".into(), index.into_os_string());
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
fn benchmark(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    runs: usize,
    selected: Option<Vec<PathBuf>>,
) -> Result<Value> {
    let files = runner.git(repo, ["ls-files", "-z"])?;
    let names: Vec<_> = files
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_vec(s.to_vec())))
        .collect();
    let selected = selected.unwrap_or_else(|| {
        names
            .iter()
            .filter(|p| fs::symlink_metadata(repo.join(p)).is_ok_and(|m| m.is_file()))
            .take(spec().changed_files)
            .cloned()
            .collect()
    });
    require(
        !selected.is_empty(),
        "Benchmark needs at least one regular tracked file",
    )?;
    let baseline_tree = runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?;
    let before = fingerprint(runner, repo, false)?;
    let sizes = object_sizes(repo)?;
    let (first, first_time) = invoke(runner, binary, repo, &strings(&["save", "first"]))?;
    require(
        fingerprint(runner, repo, false)? == before,
        "First save changed working state",
    )?;
    let mut timings = Vec::new();
    let mut latest = first.clone();
    for iteration in 0..runs {
        for name in &selected {
            writeln!(
                OpenOptions::new().append(true).open(repo.join(name))?,
                "\nbenchmark-v1:{iteration}"
            )?;
        }
        let state = fingerprint(runner, repo, false)?;
        let (saved, measured) = invoke(
            runner,
            binary,
            repo,
            &strings(&["save", &format!("iteration {iteration}")]),
        )?;
        require(
            fingerprint(runner, repo, false)? == state,
            "Save changed working state",
        )?;
        let (index, work) = trees(runner, repo)?;
        require(
            saved["snapshot"]["index_tree"] == index && saved["snapshot"]["work_tree"] == work,
            "Changed save differs from independent Git staging",
        )?;
        latest = saved;
        timings.push(measured);
    }
    let state = fingerprint(runner, repo, false)?;
    let (expected_index, expected_work) = trees(runner, repo)?;
    let (repeated, unchanged) = invoke(runner, binary, repo, &strings(&["save", "unchanged"]))?;
    require(
        fingerprint(runner, repo, false)? == state,
        "Unchanged save changed working state",
    )?;
    require(
        repeated["snapshot"]["index_tree"] == expected_index
            && repeated["snapshot"]["work_tree"] == expected_work,
        "Saved trees differ from independent Git staging",
    )?;
    let after = object_sizes(repo)?;
    let recovery = repo.parent().expect("scratch parent").join("recovered");
    let id = first["snapshot"]["id"]
        .as_str()
        .ok_or_else(|| Error::Invalid("Missing snapshot ID".into()))?;
    let (_, external) = invoke(
        runner,
        binary,
        repo,
        &[
            "restore".into(),
            id.into(),
            "--to".into(),
            recovery.into_os_string(),
        ],
    )?;
    let recovery = repo.parent().expect("scratch parent").join("recovered");
    require(
        runner
            .git(&recovery, ["status", "--porcelain=v1", "-z"])?
            .is_empty(),
        "External recovery differs from baseline",
    )?;
    require(
        runner.git_text(&recovery, &["write-tree"])? == baseline_tree,
        "External recovery tree differs",
    )?;
    let (_, inplace) = invoke(runner, binary, repo, &strings(&["restore", id]))?;
    require(
        runner
            .git(repo, ["status", "--porcelain=v1", "-z"])?
            .is_empty(),
        "In-place recovery differs from baseline",
    )?;
    require(
        runner.git_text(repo, &["write-tree"])? == baseline_tree,
        "In-place recovery tree differs",
    )?;
    // Keep clean-baseline restore timings comparable. Separately prove the dirty
    // checkpoint actually contains the edits that the measured saves captured.
    let dirty_recovery = repo
        .parent()
        .expect("scratch parent")
        .join("recovered-dirty");
    let latest_id = latest["snapshot"]["id"]
        .as_str()
        .ok_or_else(|| Error::Invalid("Missing snapshot ID".into()))?;
    invoke(
        runner,
        binary,
        repo,
        &[
            "restore".into(),
            latest_id.into(),
            "--to".into(),
            dirty_recovery.as_os_str().to_owned(),
        ],
    )?;
    verify_trees(runner, &dirty_recovery, &expected_index, &expected_work)?;
    invoke(runner, binary, repo, &strings(&["restore", latest_id]))?;
    verify_trees(runner, repo, &expected_index, &expected_work)?;
    let mut sorted: Vec<_> = timings
        .iter()
        .map(|t| t["seconds"].as_f64().expect("measurement is numeric"))
        .collect();
    sorted.sort_by(f64::total_cmp);
    let median = (sorted[(runs - 1) / 2] + sorted[runs / 2]) / 2.0;
    Ok(
        json!({"tracked_files":names.len(), "changed_files":selected.len(), "first_save":first_time,
        "warm_changed_saves":timings,"warm_median_seconds":median,"unchanged_save":unchanged,
        "external_restore":external,"inplace_restore":inplace,
        "object_growth":{"logical_bytes":i128::from(after.0)-i128::from(sizes.0),"allocated_bytes":i128::from(after.1)-i128::from(sizes.1)},
        "recovery_verified":true,"dirty_recovery_verified":true}),
    )
}
fn test_binary(args: &RunArgs) -> Result<PathBuf> {
    if let Some(path) = &args.scale_binary {
        return Ok(path.canonicalize()?);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(root().join("target/release/deps"))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("snapshot_scale-")
            && !name.contains('.')
            && entry.file_type()?.is_file()
            && entry.metadata()?.permissions().mode() & 0o111 != 0
        {
            candidates.push(entry.path());
        }
    }
    require(
        candidates.len() == 1,
        "Build exactly one release snapshot_scale executable or pass --scale-binary",
    )?;
    Ok(candidates.remove(0))
}
pub(crate) fn run(args: RunArgs) -> Result<()> {
    require(args.runs > 0, "--runs must be positive")?;
    require(
        args.real_repo.is_none() || args.mode == "benchmark",
        "--real-repo is for benchmark mode",
    )?;
    fs::create_dir_all(&args.temp_root)?;
    let temp_root = args.temp_root.canonicalize()?;
    let output = std::path::absolute(&args.output)?;
    let binary = args
        .binary
        .clone()
        .unwrap_or_else(|| root().join("target/release/groove"))
        .canonicalize()?;
    let source = Path::new("/opt/benchmark-source.json");
    let source: Value = if source.exists() {
        serde_json::from_slice(&fs::read(source)?)?
    } else {
        json!({"kind":"local-unpinned"})
    };
    let runner = Runner::new();
    let mut report = json!({"schema_version":2,"measurement_version":2,"status":"running","mode":args.mode,"fixture_version":spec().fixture_version,
        "workload_sha256":hash(SPEC_BYTES),"environment_lock_sha256":hash(LOCK_BYTES),
        "source":source,"binary_sha256":digest(&binary)?,"runner_sha256":digest(&std::env::current_exe()?)?,"runs":args.runs,
        "environment":null,"results":{},"source_unchanged":null});
    write_json(&output, &report)?;
    let result = (|| -> Result<()> {
        report["environment"] = report::environment(&runner, &temp_root, args.canonical)?;
        let spec = spec();
        for profile in &args.profile {
            let count = *spec
                .profiles
                .get(profile)
                .ok_or_else(|| Error::Invalid(format!("Unknown profile: {profile}")))?;
            report::preflight(&runner, &temp_root, count)?;
            let scratch = tempfile::Builder::new()
                .prefix("groove-scale-v1-")
                .tempdir_in(&temp_root)?;
            let local = runner.isolated(scratch.path())?;
            let value = if args.mode == "check" {
                let executable = test_binary(&args)?;
                let test = match profile.as_str() {
                    "smoke" => "scale_smoke",
                    "large" => "scale_100k",
                    "million" => "scale_1m",
                    _ => unreachable!(),
                };
                let mut flags = vec!["--exact", test, "--nocapture"];
                if profile != "smoke" {
                    flags.push("--ignored");
                }
                let stdout = local.text(&executable, flags, &root())?;
                require(
                    stdout.contains("1 passed"),
                    "Expected scale test did not execute",
                )?;
                json!({"verified":true,"test":test,"test_binary_sha256":digest(&executable)?,"stdout":stdout})
            } else {
                let repo = scratch.path().join("repo");
                let tree = spec.create(&local, &repo, count)?;
                let selected = spec
                    .positions(count)
                    .into_iter()
                    .map(|i| PathBuf::from(spec.path(i)))
                    .collect();
                let mut value = benchmark(&local, &binary, &repo, args.runs, Some(selected))?;
                value["fixture_tree"] = tree.into();
                value
            };
            report["results"][profile] = value;
            write_json(&output, &report)?;
            eprintln!("Completed {profile}");
        }
        if let Some(source) = &args.real_repo {
            let source = source.canonicalize()?;
            let before = fingerprint(&runner, &source, true)?;
            let measured = (|| -> Result<Value> {
                let scratch = tempfile::Builder::new()
                    .prefix("groove-real-v1-")
                    .tempdir_in(&temp_root)?;
                let local = runner.isolated(scratch.path())?;
                let repo = scratch.path().join("repo");
                local.git(
                    scratch.path(),
                    [
                        OsStr::new("clone"),
                        OsStr::new("--local"),
                        OsStr::new("--no-hardlinks"),
                        source.as_os_str(),
                        repo.as_os_str(),
                    ],
                )?;
                let mut value = benchmark(&local, &binary, &repo, args.runs, None)?;
                value["base_commit"] = local.git_text(&repo, &["rev-parse", "HEAD"])?.into();
                Ok(value)
            })();
            let unchanged = fingerprint(&runner, &source, true)? == before;
            report["source_unchanged"] = unchanged.into();
            require(unchanged, "Real source changed during benchmark")?;
            report["results"]["real_repo"] = measured?;
        }
        Ok(())
    })();
    match &result {
        Ok(()) => report["status"] = "passed".into(),
        Err(e) => {
            report["status"] = "failed".into();
            report["error"] = e.to_string().into();
        }
    }
    write_json(&output, &report)?;
    result
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
        let (index, work) = trees(&runner, &repo).unwrap();
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
