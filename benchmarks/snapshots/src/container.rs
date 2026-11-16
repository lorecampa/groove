use crate::{
    fixture::lock,
    util::{digest, hash, require, root, write_json, Result, Runner},
    ExecuteArgs, Mode,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

fn copy(source: &Path, target: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        symlink(fs::read_link(source)?, target)?;
    } else if meta.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if ["target", "__pycache__", "results"]
                .iter()
                .any(|name| entry.file_name() == *name)
            {
                continue;
            }
            copy(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, target)?;
    }
    Ok(())
}
fn manifest(context: &Path) -> Result<String> {
    fn paths(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                paths(&entry.path(), files)?;
            } else {
                files.push(entry.path());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    paths(context, &mut files)?;
    files.sort();
    let mut hash = Sha256::new();
    for path in files {
        hash.update(
            path.strip_prefix(context)
                .expect("context descendant")
                .as_os_str()
                .as_bytes(),
        );
        hash.update([0]);
        let meta = fs::symlink_metadata(&path)?;
        hash.update(meta.permissions().mode().to_le_bytes());
        if meta.file_type().is_symlink() {
            hash.update(b"symlink:");
            hash.update(fs::read_link(path)?.as_os_str().as_bytes());
        } else {
            hash.update(digest(&path)?.as_bytes());
        }
        hash.update([0]);
    }
    Ok(crate::util::hex(&hash.finalize()))
}

fn source_tree(source: &Path, target: &Path) -> Result<serde_json::Value> {
    fs::create_dir_all(target)?;
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "README.md",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "crates",
        "schemas",
        "benchmarks",
        "tools",
        "rust-toolchain.toml",
        ".cargo",
    ] {
        if source.join(name).exists() {
            copy(&source.join(name), &target.join(name))?;
        }
    }
    let runner = Runner::launcher();
    Ok(
        json!({"revision":runner.git_text(source,&["rev-parse","HEAD"])?,"dirty":!runner.git(source,["status","--porcelain"])?.is_empty(),"manifest_sha256":manifest(target)?}),
    )
}

pub(crate) fn build(baseline: Option<&Path>) -> Result<(String, String)> {
    let context = tempfile::Builder::new()
        .prefix("groove-bench-build-")
        .tempdir()?;
    let source = root();
    let candidate = source_tree(&source, context.path())?;
    let baseline = source_tree(
        baseline.unwrap_or(&source),
        &context.path().join("baseline"),
    )?;
    let provenance = json!({"candidate":candidate,"baseline":baseline,"build_policy":"vendored dependencies; network-disabled frozen compilation; fixed paths and image toolchain"});
    write_json(&context.path().join("benchmark-source.json"), &provenance)?;
    let tag = format!(
        "groove-bench:{}",
        &hash(&serde_json::to_vec(&provenance)?)[..20]
    );
    let runner = Runner::launcher();
    if runner
        .run("docker", ["image", "inspect", &tag], &source)
        .is_err()
    {
        eprintln!("Preparing dependencies and compiling offline in {tag}");
        let mut build = runner.clone();
        build.timeout = Duration::from_secs(1800);
        build.run(
            "docker",
            vec![
                OsString::from("build"),
                "--platform".into(),
                lock()["platform"].as_str().expect("platform").into(),
                "-t".into(),
                tag.clone().into(),
                "-f".into(),
                context
                    .path()
                    .join("benchmarks/snapshots/Dockerfile")
                    .into(),
                context.path().into(),
            ],
            &source,
        )?;
    }
    let id = runner.text(
        "docker",
        ["image", "inspect", "--format={{.Id}}", &tag],
        &source,
    )?;
    Ok((tag, id))
}
fn mount(path: &Path, target: &str, readonly: bool) -> Result<OsString> {
    require(
        !path.as_os_str().as_bytes().contains(&b','),
        "Docker bind paths cannot contain commas",
    )?;
    let mut value = OsString::from("type=bind,source=");
    value.push(path);
    value.push(format!(",target={target}"));
    if readonly {
        value.push(",readonly");
    }
    Ok(value)
}
fn cpu_list(value: &str) -> Result<Vec<u32>> {
    let mut cpus = std::collections::BTreeSet::new();
    for part in value.trim().split(',') {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        let parse = |v: &str| {
            v.parse::<u32>()
                .map_err(|_| crate::util::Error::Invalid("Invalid CPU list".into()))
        };
        let (first, last) = (parse(first)?, parse(last)?);
        require(first <= last && last < 4096, "Invalid CPU range")?;
        cpus.extend(first..=last);
    }
    require(!cpus.is_empty(), "Empty CPU list")?;
    Ok(cpus.into_iter().collect())
}
struct Container {
    id: String,
    scratch: Option<tempfile::TempDir>,
    removed: bool,
}
impl Container {
    fn remove(&mut self) -> Result<()> {
        let mut runner = Runner::launcher();
        runner.interruptible = false;
        runner.timeout = Duration::from_secs(30);
        runner.run("docker", ["rm", "--force", &self.id], &root())?;
        self.removed = true;
        Ok(())
    }
}
impl Drop for Container {
    fn drop(&mut self) {
        if !self.removed {
            if let Err(error) = self.remove() {
                // A failed daemon cleanup does not establish stopped writers.
                // Keep this exact scratch directory for explicit operator recovery.
                let preserved = self.scratch.take().map(tempfile::TempDir::keep);
                eprintln!(
                    "Owned container cleanup failed: {error}; retained scratch {preserved:?}"
                );
            }
        }
    }
}
fn launch(mode: Mode, args: &ExecuteArgs) -> Result<()> {
    let plan = &args.plan;
    let output = std::path::absolute(&plan.output)?;
    let parent = output.parent().expect("absolute output parent");
    fs::create_dir_all(parent)?;
    fs::create_dir_all(&plan.temp_root)?;
    // Fail before the image build when a pinned repository has not been fetched.
    let pinned = crate::repository::required(&crate::profiles::select(&plan.profiles)?)?;
    let cache = if pinned.is_empty() {
        None
    } else {
        let cache = fs::canonicalize(plan.cache.path()).map_err(|_| {
            crate::util::Error::Invalid(format!(
                "Pinned repository cache {} is missing; run `cargo run -p groove-bench --locked -- fetch PROFILE`",
                plan.cache.path().display()
            ))
        })?;
        for repository in &pinned {
            crate::repository::verify(&Runner::new(), &cache, repository)?;
        }
        Some(cache)
    };
    let scratch = tempfile::Builder::new()
        .prefix("groove-bench-runtime-")
        .tempdir_in(&plan.temp_root)?;
    let (tag, _) = build(args.baseline_source.as_deref())?;
    let runner = Runner::launcher();
    let lock = lock();
    let available = fs::read_to_string("/proc/self/status")?;
    let available = available
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .ok_or_else(|| {
            crate::util::Error::Invalid("Canonical benchmarks require a native Linux host".into())
        })?;
    let available = cpu_list(available)?;
    let selected = if let Some(cpus) = &args.cpu_set {
        cpu_list(cpus)?
    } else {
        available
            .iter()
            .copied()
            .take(lock["cpus"].as_u64().unwrap_or(4) as usize)
            .collect()
    };
    require(
        selected.len() == lock["cpus"].as_u64().unwrap_or(4) as usize
            && selected.iter().all(|c| available.contains(c)),
        "Select exactly four available CPUs",
    )?;
    let cpus = selected
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let uid = runner.text("id", ["-u"], &root())?;
    let gid = runner.text("id", ["-g"], &root())?;
    let mut command: Vec<OsString> = [
        "create",
        "--platform",
        lock["platform"].as_str().expect("platform"),
        "--network=none",
        "--read-only",
        "--init",
        "--user",
        &format!("{uid}:{gid}"),
        "--cpus",
        &lock["cpus"].to_string(),
        "--cpuset-cpus",
        &cpus,
        "--memory",
        &lock["memory_bytes"].to_string(),
        "--memory-swap",
        &lock["memory_bytes"].to_string(),
        "--pids-limit",
        &lock["pids_limit"].to_string(),
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=268435456",
    ]
    .map(Into::into)
    .to_vec();
    command.extend([
        "--mount".into(),
        mount(&scratch.path().canonicalize()?, "/scratch", false)?,
        "--mount".into(),
        mount(&parent.canonicalize()?, "/results", false)?,
    ]);
    if let Some(cache) = &cache {
        // Read-only: measured processes can clone from the cache but never alter it.
        command.extend(["--mount".into(), mount(cache, "/fixtures", true)?]);
    }
    let mut report_path = OsString::from("/results/");
    report_path.push(output.file_name().expect("output name"));
    command.extend([
        tag.into(),
        "__run".into(),
        "--mode".into(),
        mode.name().into(),
        "--runtime-contract".into(),
        "/scratch/runtime.json".into(),
        "--temp-root".into(),
        "/scratch".into(),
        "--output".into(),
        report_path,
        "--fixture-cache".into(),
        "/fixtures".into(),
    ]);
    command.extend(plan.profiles.iter().map(OsString::from));
    if args.baseline_source.is_some() {
        command.extend([
            "--baseline".into(),
            "/baseline/target/release/groove".into(),
        ]);
    }
    let id = runner.text("docker", command, &root())?;
    require(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Docker returned an invalid owned container ID",
    )?;
    let mut container = Container {
        id,
        scratch: Some(scratch),
        removed: false,
    };
    let scratch = container.scratch.as_ref().expect("owned runtime scratch");
    let inspection: serde_json::Value =
        serde_json::from_str(&runner.text("docker", ["inspect", &container.id], &root())?)?;
    let storage = runner.text(
        "findmnt",
        [
            OsString::from("--noheadings"),
            "--target".into(),
            scratch.path().as_os_str().to_owned(),
            "--output".into(),
            "SOURCE,FSTYPE,OPTIONS,MAJ:MIN".into(),
        ],
        &root(),
    )?;
    let contract = json!({"inspection":inspection[0],"host_identity":hash(&fs::read("/etc/machine-id")?),"storage":{"mount":storage},"cpu_policy":"affinity and quota, no exclusive CPU reservation","kernel":runner.text("uname",["-r"],&root())?});
    crate::report::validate_contract(&contract)?;
    write_json(&scratch.path().join("runtime.json"), &contract)?;
    let mut runtime = runner;
    runtime.progress_path = Some(output);
    runtime.timeout = Duration::from_secs(24 * 3600);
    eprintln!(
        "Measuring on CPUs {cpus}; image {}",
        contract["inspection"]["Image"]
    );
    let out = runtime.run("docker", ["start", "--attach", &container.id], &root())?;
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    // Attached start can succeed after a failed container process; inspect its actual exit.
    let exit = runtime.text(
        "docker",
        ["inspect", "--format={{.State.ExitCode}}", &container.id],
        &root(),
    )?;
    require(
        exit == "0",
        "Benchmark container failed; inspect the single output report",
    )?;
    container.remove()?;
    Ok(())
}
pub(crate) fn run(mode: Mode, args: ExecuteArgs) -> Result<()> {
    write_json(
        &args.plan.output,
        &json!({"status":"running","phase":"build","summary":null}),
    )?;
    let result = launch(mode, &args);
    if let Err(error) = &result {
        crate::report::record_failure(&args.plan.output, error)?;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_daemon_cleanup_keeps_owned_scratch() {
        const CHILD: &str = "GROOVE_BENCH_CLEANUP_TEST";
        if let Some(path) = std::env::var_os(CHILD) {
            let scratch = tempfile::tempdir_in(path).unwrap();
            let preserved = scratch.path().to_owned();
            fs::write(preserved.join("result"), b"preserve").unwrap();
            drop(Container {
                id: "a".repeat(64),
                scratch: Some(scratch),
                removed: false,
            });
            assert_eq!(fs::read(preserved.join("result")).unwrap(), b"preserve");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let tools = temp.path().join("tools");
        fs::create_dir(&tools).unwrap();
        let docker = tools.join("docker");
        fs::write(&docker, b"#!/bin/bash\nexit 1\n").unwrap();
        fs::set_permissions(docker, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = tools.into_os_string();
        path.push(":/usr/bin:/bin");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "container::tests::failed_daemon_cleanup_keeps_owned_scratch",
            ])
            .env(CHILD, temp.path())
            .env("PATH", path)
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[test]
    fn context_hash_tracks_content_and_modes_without_following_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("file"), "first").unwrap();
        symlink("missing", src.join("link")).unwrap();
        fs::create_dir(src.join("target")).unwrap();
        fs::write(src.join("target/artifact"), "ignored").unwrap();
        let dest = temp.path().join("dest");
        copy(&src, &dest).unwrap();
        assert!(!dest.join("target").exists());
        assert!(fs::symlink_metadata(dest.join("link"))
            .unwrap()
            .file_type()
            .is_symlink());
        let before = manifest(&dest).unwrap();
        assert_eq!(before, manifest(&dest).unwrap());
        fs::write(dest.join("file"), "second").unwrap();
        assert_ne!(before, manifest(&dest).unwrap());
        let before = manifest(&dest).unwrap();
        fs::set_permissions(dest.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(before, manifest(&dest).unwrap());
        assert!(mount(Path::new("/a,b"), "/scratch", false).is_err());
        assert_eq!(
            mount(Path::new("/path with spaces"), "/real-source", true).unwrap(),
            "type=bind,source=/path with spaces,target=/real-source,readonly"
        );
    }
}
