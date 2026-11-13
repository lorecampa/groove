use crate::{
    fixture::lock,
    util::{digest, require, Error, Result, Runner},
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{fs, path::Path};

fn text_file(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}
pub(crate) fn versions(runner: &Runner) -> Result<Value> {
    let cwd = Path::new("/");
    #[cfg(target_os = "linux")]
    let gnu_time = Some(runner.text("/usr/bin/time", ["--version"], cwd)?);
    #[cfg(target_os = "linux")]
    let package = runner
        .text("dpkg-query", ["-W", "-f=${Version}", "time"], cwd)
        .ok();
    #[cfg(target_os = "macos")]
    let (gnu_time, package): (Option<String>, Option<String>) = (None, None);
    Ok(json!({
        "rust": runner.text("rustc", ["--version"], cwd)?,
        "git": runner.text("git", ["--version"], cwd)?,
        "gnu_time": gnu_time.as_deref().and_then(|s| s.lines().next()),
        "gnu_time_package": package,
    }))
}
pub(crate) fn verify_environment(runner: &Runner) -> Result<Value> {
    let found = versions(runner)?;
    let lock = lock();
    require(
        found["rust"]
            .as_str()
            .and_then(|s| s.split_whitespace().nth(1))
            == lock["rust"].as_str(),
        "Rust differs from environment lock",
    )?;
    require(
        found["git"]
            .as_str()
            .and_then(|s| s.split_whitespace().nth(2))
            == lock["git"].as_str(),
        "Git differs from environment lock",
    )?;
    require(
        found["gnu_time_package"]
            .as_str()
            .and_then(|s| s.split('-').next())
            == lock["gnu_time"].as_str(),
        "GNU time differs from environment lock",
    )?;
    require(
        std::env::consts::ARCH == "x86_64" && std::env::consts::OS == "linux",
        "Canonical environment requires linux/amd64",
    )?;
    Ok(found)
}
#[cfg(target_os = "linux")]
pub(crate) fn storage(runner: &Runner, root: &Path) -> Result<(String, u64, u64)> {
    let out = runner.text(
        "stat",
        [
            std::ffi::OsStr::new("-f"),
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("%T\n%a\n%S\n%d"),
            root.as_os_str(),
        ],
        root,
    )?;
    let lines: Vec<_> = out.lines().collect();
    require(lines.len() == 4, "Invalid stat output")?;
    let parse = |s: &str| s.parse::<u64>().map_err(|e| Error::Invalid(e.to_string()));
    Ok((
        lines[0].to_owned(),
        parse(lines[1])?.saturating_mul(parse(lines[2])?),
        parse(lines[3])?,
    ))
}
#[cfg(target_os = "macos")]
pub(crate) fn storage(_runner: &Runner, root: &Path) -> Result<(String, u64, u64)> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path =
        CString::new(root.as_os_str().as_bytes()).map_err(|e| Error::Invalid(e.to_string()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is NUL-terminated; stat is writable storage of the required size.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful statfs initialized stat.
    let stat = unsafe { stat.assume_init() };
    let name: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    Ok((
        String::from_utf8_lossy(&name).into_owned(),
        stat.f_bavail.saturating_mul(stat.f_bsize as u64),
        stat.f_ffree,
    ))
}
pub(crate) fn preflight(
    runner: &Runner,
    root: &Path,
    required_bytes: u64,
    required_inodes: u64,
) -> Result<()> {
    let (_, bytes, inodes) = storage(runner, root)?;
    require(
        inodes >= required_inodes,
        "Insufficient free inodes for fixture and recovery",
    )?;
    require(bytes >= required_bytes, "Insufficient free disk space")
}
pub(crate) fn environment(runner: &Runner, root: &Path, runtime: Option<&Value>) -> Result<Value> {
    let canonical = runtime.is_some();
    if let Some(contract) = runtime {
        validate_contract(contract)?;
    }
    let (filesystem, bytes, inodes) = storage(runner, root)?;
    let quota = text_file("/sys/fs/cgroup/cpu.max");
    let memory = text_file("/sys/fs/cgroup/memory.max");
    let tools = if canonical {
        verify_environment(runner)?
    } else {
        versions(runner)?
    };
    if canonical {
        require(
            text_file("/sys/fs/cgroup/memory.swap.max").as_deref() == Some("0"),
            "Swap must be disabled",
        )?;
        require(
            text_file("/sys/fs/cgroup/pids.max") == Some(lock()["pids_limit"].to_string()),
            "PID limit differs from lock",
        )?;
        require(
            Path::new("/opt/benchmark-source.json").is_file()
                && Path::new("/opt/benchmark-packages.txt").is_file()
                && Path::new("/opt/benchmark-vendor.sha256").is_file(),
            "Canonical image provenance is missing",
        )?;
        require(
            ["ext2/ext3", "xfs", "btrfs", "zfs"].contains(&filesystem.as_str()),
            "Canonical scratch requires bind-mounted local disk",
        )?;
        let parts: Vec<f64> = quota
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        require(
            parts.len() == 2
                && parts[1] > 0.0
                && parts[0] / parts[1] == lock()["cpus"].as_f64().unwrap_or(0.0),
            "CPU quota differs from lock",
        )?;
        require(
            memory.as_deref() == Some(lock()["memory_bytes"].to_string().as_str()),
            "Memory limit differs from lock",
        )?;
    }
    #[cfg(target_os = "linux")]
    let cpu = text_file("/proc/cpuinfo").and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_owned())
    });
    #[cfg(target_os = "macos")]
    let cpu = Some(runner.text("/usr/sbin/sysctl", ["-n", "machdep.cpu.brand_string"], root)?);
    let packages = Path::new("/opt/benchmark-packages.txt");
    Ok(json!({"canonical": canonical, "tools": tools,
        "kernel": runner.text("uname", ["-r"], root)?, "architecture": std::env::consts::ARCH,
        "cpu_model": cpu, "cpu_quota": quota, "memory_limit": memory, "filesystem": filesystem,
        "available_bytes": bytes, "available_inodes": inodes,
        "packages_sha256": if packages.exists() { Some(digest(packages)?) } else { None },
        "vendor_manifest":text_file("/opt/benchmark-vendor.sha256"),
        "runtime": runtime,
        "umask":text_file("/proc/self/status").and_then(|s|s.lines().find_map(|line|line.strip_prefix("Umask:").map(|v|v.trim().to_owned()))),
        "cpu_affinity": text_file("/sys/fs/cgroup/cpuset.cpus.effective"),
        "cpu_stat": text_file("/sys/fs/cgroup/cpu.stat"),
        "memory_events": text_file("/sys/fs/cgroup/memory.events"),
        "swap_limit":text_file("/sys/fs/cgroup/memory.swap.max"),
        "pids_limit":text_file("/sys/fs/cgroup/pids.max")}))
}

pub(crate) fn validate_contract(contract: &Value) -> Result<()> {
    let host = &contract["inspection"]["HostConfig"];
    let lock = lock();
    for (key, expected) in [
        ("ReadonlyRootfs", json!(true)),
        ("NetworkMode", json!("none")),
        ("Memory", lock["memory_bytes"].clone()),
        ("MemorySwap", lock["memory_bytes"].clone()),
        (
            "NanoCpus",
            json!(lock["cpus"].as_u64().unwrap_or(0) * 1_000_000_000),
        ),
        ("PidsLimit", lock["pids_limit"].clone()),
    ] {
        require(
            host[key] == expected,
            format!("Runtime contract differs: {key}"),
        )?;
    }
    require(
        host["CpusetCpus"].as_str().is_some_and(|s| !s.is_empty()),
        "Missing CPU affinity",
    )?;
    for value in [
        &contract["inspection"]["Image"],
        &contract["host_identity"],
        &contract["storage"]["mount"],
    ] {
        require(
            value.as_str().is_some_and(|s| !s.is_empty()),
            "Missing runtime provenance",
        )?;
    }
    let mounts = contract["inspection"]["Mounts"]
        .as_array()
        .ok_or_else(|| Error::Invalid("Missing runtime mounts".into()))?;
    for (destination, writable) in [("/scratch", true), ("/results", true)] {
        require(
            mounts.iter().any(|m| {
                m["Destination"] == destination && m["RW"] == writable && m["Type"] == "bind"
            }),
            format!("Missing runtime bind: {destination}"),
        )?;
    }
    for mount in mounts {
        require(
            match mount["Destination"].as_str() {
                Some("/scratch" | "/results") => mount["Type"] == "bind" && mount["RW"] == true,
                Some("/tmp") => mount["Type"] == "tmpfs",
                _ => false,
            },
            "Unexpected runtime mount or writable fixture",
        )?;
    }
    Ok(())
}

pub(crate) fn summarize(report: &mut Value) -> Result<()> {
    require(
        report["schema_version"] == 4 && report["measurement_version"] == 4,
        "Unsupported benchmark report version",
    )?;
    require(
        report["status"] == "passed",
        "Only completed successful reports can be summarized",
    )?;
    let mode: crate::Mode = serde_json::from_value(report["mode"].clone())?;
    for key in [
        "fixture_sha256",
        "catalog_sha256",
        "environment_lock_sha256",
        "runner_sha256",
    ] {
        require(
            report[key]
                .as_str()
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())),
            format!("Missing report provenance: {key}"),
        )?;
    }
    let canonical = report["environment"]["canonical"] == true;
    if canonical {
        validate_contract(&report["environment"]["runtime"])?;
        require(
            report["environment"]["packages_sha256"]
                .as_str()
                .is_some_and(|s| s.len() == 64)
                && report["environment"]["vendor_manifest"].is_string(),
            "Missing package/vendor provenance",
        )?;
    }
    for side in ["candidate", "baseline"] {
        if side == "candidate" || !report["binaries"][side].is_null() {
            let binary: BinaryRecord = serde_json::from_value(report["binaries"][side].clone())?;
            require(
                binary.sha256.len() == 64
                    && binary.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                    && !binary.path.is_empty(),
                "Invalid binary provenance",
            )?;
        }
    }
    let paired = report["binaries"]["baseline"].is_object();
    require(
        mode != crate::Mode::Check || !paired,
        "Correctness reports cannot compare baselines",
    )?;
    let definitions = report["profiles"]
        .as_object()
        .ok_or_else(|| Error::Invalid("Missing profile definitions".into()))?;
    require(!definitions.is_empty(), "No selected profiles")?;
    let cases = report["cases"]
        .as_object()
        .ok_or_else(|| Error::Invalid("Missing cases".into()))?;
    let mut expected = std::collections::BTreeSet::new();
    let mut summary = Map::new();
    for (id, value) in definitions {
        let selected: SelectedProfile = serde_json::from_value(value.clone())?;
        let profile = selected.definition;
        crate::profiles::validate(&profile)?;
        require(
            profile.id == *id && profile.sha256()? == selected.sha256,
            "Profile identity/hash contradicts embedded definition",
        )?;
        for case in &profile.cases {
            let key = format!("{id}/{}", case.id);
            expected.insert(key.clone());
            let result: MeasurementCase = serde_json::from_value(
                cases
                    .get(&key)
                    .ok_or_else(|| Error::Invalid(format!("Missing profile case {key}")))?
                    .clone(),
            )?;
            require(!result.identity.is_empty(), "Missing fixture identity")?;
            let count = if mode == crate::Mode::Check {
                1
            } else {
                profile.sampling.blocks as usize
            };
            require(
                result.blocks.len() == count,
                "Raw block count differs from profile sampling",
            )?;
            let mut build_tools = None;
            for block in &result.blocks {
                for observation in std::iter::once(&block.candidate).chain(block.baseline.iter()) {
                    let evidence: crate::evidence::Evidence =
                        serde_json::from_value(observation.detail.clone())?;
                    if let Some(build) = &evidence.build {
                        require(
                            build_tools
                                .as_ref()
                                .is_none_or(|tools| tools == &build.tools),
                            "Build tools changed between binaries/blocks",
                        )?;
                        build_tools = Some(build.tools.clone());
                    }
                    let recomputed = crate::evidence::observation(case, evidence)?;
                    require(
                        recomputed.verified
                            && observation.verified
                            && recomputed.metrics == observation.metrics,
                        "Stored aggregates contradict raw profile evidence",
                    )?;
                }
            }
            let metrics = crate::statistics::summarize(&result.blocks, paired, canonical)?;
            summary.insert(
                key,
                if mode == crate::Mode::Check {
                    json!({"verified":true})
                } else {
                    metrics
                },
            );
        }
    }
    require(
        expected.iter().eq(cases.keys()),
        "Unexpected report cases outside selected profiles",
    )?;
    report["summary"] = summary.into();
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedProfile {
    sha256: String,
    definition: crate::profiles::Profile,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryRecord {
    sha256: String,
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeasurementCase {
    identity: String,
    blocks: Vec<crate::statistics::Block>,
}
pub(crate) fn record_failure(output: &Path, error: &Error) -> Result<()> {
    let mut report = fs::read(output)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or_else(|| json!({}));
    report["status"] = json!("failed");
    report["error"] = json!(error.to_string());
    crate::util::write_json(output, &report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_reports_and_incomplete_runtime_provenance_are_rejected() {
        let mut report = json!({"schema_version":4,"measurement_version":999,"status":"passed"});
        assert!(summarize(&mut report).is_err());
        assert!(validate_contract(&json!({})).is_err());
    }
    #[test]
    fn runtime_contract_requires_isolation_limits_affinity_and_owned_mounts() {
        let contract = json!({"inspection":{"Image":"sha256:image","HostConfig":{"ReadonlyRootfs":true,"NetworkMode":"none","Memory":8589934592_u64,"MemorySwap":8589934592_u64,"NanoCpus":4000000000_u64,"PidsLimit":512,"CpusetCpus":"0,1,2,3"},"Mounts":[{"Destination":"/scratch","RW":true,"Type":"bind"},{"Destination":"/results","RW":true,"Type":"bind"}]},"host_identity":"host","storage":{"mount":"ext4"}});
        validate_contract(&contract).unwrap();
        for (key, value) in [
            ("ReadonlyRootfs", json!(false)),
            ("NetworkMode", json!("bridge")),
            ("Memory", json!(0)),
            ("MemorySwap", json!(-1)),
            ("NanoCpus", json!(0)),
            ("PidsLimit", json!(0)),
            ("CpusetCpus", json!("")),
        ] {
            let mut broken = contract.clone();
            broken["inspection"]["HostConfig"][key] = value;
            assert!(
                validate_contract(&broken).is_err(),
                "Accepted changed {key}"
            );
        }
        let mut broken = contract;
        broken["inspection"]["Mounts"] = json!([]);
        assert!(validate_contract(&broken).is_err());
    }
}
