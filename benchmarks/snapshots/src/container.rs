use crate::{
    fixture::lock,
    util::{digest, hash, require, root, write_json, Result, Runner},
    ContainerArgs,
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
pub(crate) fn build() -> Result<(String, String)> {
    let context = tempfile::Builder::new()
        .prefix("groove-bench-build-")
        .tempdir()?;
    let root = root();
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
    ] {
        copy(&root.join(name), &context.path().join(name))?;
    }
    let runner = Runner::new();
    let source = json!({"revision":runner.git_text(&root,&["rev-parse","HEAD"])?,
        "dirty":!runner.git(&root,["status","--porcelain"])?.is_empty(),"manifest_sha256":manifest(context.path())?});
    write_json(&context.path().join("benchmark-source.json"), &source)?;
    let tag = format!(
        "groove-snapshot-bench:{}",
        &hash(&serde_json::to_vec(&source)?)[..16]
    );
    if runner
        .run("docker", ["image", "inspect", &tag], &root)
        .is_err()
    {
        eprintln!("Building {tag}; this can take several minutes (Docker layer cache is reused)");
        let mut build = runner.clone();
        build.timeout = Duration::from_secs(1800);
        let args: Vec<OsString> = vec![
            "build".into(),
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
        ];
        build.run("docker", args, &root)?;
    }
    let id = runner.text(
        "docker",
        ["image", "inspect", "--format={{.Id}}", &tag],
        &root,
    )?;
    Ok((tag, id))
}
fn mount(path: &Path, target: &str, readonly: bool) -> Result<OsString> {
    // Docker's --mount parser uses commas, even when argv itself is correctly quoted.
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
pub(crate) fn run(mode: &str, args: ContainerArgs) -> Result<()> {
    require(args.runs > 0, "--runs must be positive")?;
    require(
        args.real_repo.is_none() || mode == "benchmark",
        "--real-repo is for benchmark mode",
    )?;
    let output = std::path::absolute(&args.output)?;
    let parent = output.parent().expect("absolute output parent");
    fs::create_dir_all(parent)?;
    fs::create_dir_all(&args.temp_root)?;
    let scratch = tempfile::Builder::new()
        .prefix("groove-bench-runtime-")
        .tempdir_in(&args.temp_root)?;
    let mut mounts = vec![
        "--mount".into(),
        mount(&scratch.path().canonicalize()?, "/scratch", false)?,
        "--mount".into(),
        mount(&parent.canonicalize()?, "/results", false)?,
    ];
    if let Some(repo) = &args.real_repo {
        mounts.extend([
            "--mount".into(),
            mount(&repo.canonicalize()?, "/real-source", true)?,
        ]);
    }
    let (tag, image) = build()?;
    let runner = Runner::new();
    let lock = lock();
    let uid = runner.text("id", ["-u"], &root())?;
    let gid = runner.text("id", ["-g"], &root())?;
    let mut command: Vec<OsString> = [
        "run",
        "--rm",
        "--platform",
        lock["platform"].as_str().expect("platform"),
        "--network=none",
        "--read-only",
        "--user",
        &format!("{uid}:{gid}"),
        "--cpus",
        &lock["cpus"].to_string(),
        "--memory",
        &lock["memory_bytes"].to_string(),
        "--memory-swap",
        &lock["memory_bytes"].to_string(),
        "--pids-limit",
        &lock["pids_limit"].to_string(),
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=268435456",
        "--env",
        &format!("GROOVE_BENCH_IMAGE_ID={image}"),
    ]
    .map(Into::into)
    .to_vec();
    command.append(&mut mounts);
    let mut report_path = OsString::from("/results/");
    report_path.push(output.file_name().expect("output name"));
    command.extend([
        tag.into(),
        "run".into(),
        "--canonical".into(),
        "--mode".into(),
        mode.into(),
        "--runs".into(),
        args.runs.to_string().into(),
        "--temp-root".into(),
        "/scratch".into(),
        "--output".into(),
        report_path,
    ]);
    for profile in &args.profile {
        command.extend(["--profile".into(), profile.into()]);
    }
    if args.real_repo.is_some() {
        command.extend(["--real-repo".into(), "/real-source".into()]);
    }
    let mut runtime = runner;
    runtime.timeout = Duration::from_secs(3600);
    eprintln!("Running {mode} in {image}");
    let cid_dir = tempfile::tempdir()?;
    let cid_file = cid_dir.path().join("container-id");
    command.splice(
        1..1,
        [OsString::from("--cidfile"), cid_file.as_os_str().to_owned()],
    );
    let result = runtime.run("docker", command, &root());
    if result.is_err() {
        // Killing a Docker client does not stop its daemon-owned container.
        // Remove only the container ID written for this invocation.
        if let Ok(id) = fs::read_to_string(&cid_file) {
            let id = id.trim();
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Err(cleanup) = Runner::new().run("docker", ["rm", "--force", id], &root()) {
                    eprintln!("Container cleanup for {id}: {cleanup}");
                }
            }
        }
    }
    if let Ok(out) = &result {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
    }
    result?;
    println!("{}", json!({"status":"passed","output":output}));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
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
