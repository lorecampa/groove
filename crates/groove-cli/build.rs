//! Capture source provenance without requiring Git for extracted source archives.
use std::{
    env, fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn git(args: &[&str]) -> Option<String> {
    let output = Path::new(&env::var_os("OUT_DIR")?).join("git-provenance.txt");
    let file = fs::File::create(&output).ok()?;
    let mut command = Command::new("git");
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    // Only fixed, read-only rev-parse queries: no network, hooks, aliases or
    // mutation subprocesses. File capture avoids pipe backpressure; these queries
    // return one OID or filesystem path, never repository-sized output.
    let mut child = command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) if started.elapsed() < Duration::from_secs(2) => {
                thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(output)
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    Some(String::from_utf8(bytes).ok()?.trim().to_owned())
}
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GROOVE_BUILD_REVISION");
    let manifest = env::var_os("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    // Extracted packages may sit inside an unrelated Git checkout. Only use Git
    // when its top-level directory actually owns this workspace member.
    let source_checkout = git(&["rev-parse", "--show-toplevel"]).is_some_and(|root| {
        Path::new(&root)
            .join("crates/groove-cli")
            .canonicalize()
            .ok()
            == manifest.canonicalize().ok()
    });
    let vcs_path = manifest.join(".cargo_vcs_info.json");
    let vcs_revision = fs::read(&vcs_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["git"]["sha1"].as_str().map(str::to_owned));
    if vcs_path.exists() {
        println!("cargo:rerun-if-changed={}", vcs_path.display());
    }
    let revision = env::var("GROOVE_BUILD_REVISION")
        .ok()
        .or_else(|| {
            source_checkout
                .then(|| git(&["rev-parse", "--verify", "HEAD"]))
                .flatten()
        })
        .or(vcs_revision)
        .filter(|id| matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    if source_checkout {
        for file in ["HEAD", "packed-refs"] {
            if let Some(path) = git(&["rev-parse", "--git-path", file]) {
                if Path::new(&path).exists() {
                    println!("cargo:rerun-if-changed={path}");
                }
            }
        }
        if let Some(head) = git(&["rev-parse", "--symbolic-full-name", "HEAD"]) {
            if head.starts_with("refs/") {
                if let Some(path) = git(&["rev-parse", "--git-path", &head]) {
                    if Path::new(&path).exists() {
                        println!("cargo:rerun-if-changed={path}");
                    }
                }
            }
        }
        // Reftable branches have no loose ref or packed-refs file to watch.
        if let Some(common) = git(&["rev-parse", "--git-common-dir"]) {
            let tables = Path::new(&common).join("reftable/tables.list");
            if tables.exists() {
                println!("cargo:rerun-if-changed={}", tables.display());
            }
        }
    }
    println!("cargo:rustc-env=GROOVE_BUILD_REVISION={revision}");
}
