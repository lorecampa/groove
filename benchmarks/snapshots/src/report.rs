use crate::{
    fixture::lock,
    util::{digest, require, Error, Result, Runner},
};
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
    let package = Some(runner.text("dpkg-query", ["-W", "-f=${Version}", "time"], cwd)?);
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
pub(crate) fn preflight(runner: &Runner, root: &Path, count: usize) -> Result<()> {
    let (_, bytes, inodes) = storage(runner, root)?;
    require(
        inodes >= count as u64 * 3 + 10000,
        "Insufficient free inodes for fixture and recovery",
    )?;
    require(
        bytes >= count as u64 * 12288 + 512 * 1024 * 1024,
        "Insufficient free disk space",
    )
}
pub(crate) fn environment(runner: &Runner, root: &Path, canonical: bool) -> Result<Value> {
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
    let cpu = text_file("/proc/cpuinfo").and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_owned())
    });
    let packages = Path::new("/opt/benchmark-packages.txt");
    Ok(json!({"canonical": canonical, "tools": tools,
        "kernel": runner.text("uname", ["-r"], root)?, "architecture": std::env::consts::ARCH,
        "cpu_model": cpu, "cpu_quota": quota, "memory_limit": memory, "filesystem": filesystem,
        "available_bytes": bytes, "available_inodes": inodes,
        "packages_sha256": if packages.exists() { Some(digest(packages)?) } else { None },
        "image_id": std::env::var("GROOVE_BENCH_IMAGE_ID").ok()}))
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| Error::Invalid(format!("Missing report field: {key}")))
}
pub(crate) fn compare(a: &Value, b: &Value) -> Result<Value> {
    require(
        a["schema_version"] == 2 && b["schema_version"] == 2,
        "Legacy or unsupported report schema; generate version 2 reports",
    )?;
    require(
        a["environment"]["canonical"] == true && b["environment"]["canonical"] == true,
        "Only canonical reports support regression comparisons",
    )?;
    require(
        a["status"] == "passed" && b["status"] == "passed",
        "Only successful runs can be compared",
    )?;
    require(
        a["mode"] == "benchmark" && b["mode"] == "benchmark",
        "Correctness reports have no latency comparison",
    )?;
    for key in [
        "schema_version",
        "measurement_version",
        "mode",
        "fixture_version",
        "workload_sha256",
        "environment_lock_sha256",
        "runs",
    ] {
        require(
            required(a, key)? == required(b, key)?,
            format!("Workload differs: {key}"),
        )?;
    }
    for key in [
        "canonical",
        "tools",
        "kernel",
        "architecture",
        "cpu_model",
        "cpu_quota",
        "memory_limit",
        "filesystem",
        "packages_sha256",
    ] {
        require(
            required(&a["environment"], key)? == required(&b["environment"], key)?,
            format!("Environment differs: {key}"),
        )?;
    }
    let results = |v: &Value| -> Result<Map<String, Value>> {
        v["results"]
            .as_object()
            .cloned()
            .ok_or_else(|| Error::Invalid("Missing results".into()))
    };
    let (a, b) = (results(a)?, results(b)?);
    require(
        !a.is_empty() && a.keys().eq(b.keys()),
        "Profile sets differ or are empty",
    )?;
    let mut output = Map::new();
    for (name, a) in a {
        let b = &b[&name];
        let identity = |v: &Value| {
            v.get("fixture_tree")
                .or_else(|| v.get("base_commit"))
                .cloned()
        };
        require(
            identity(&a).is_some() && identity(&a) == identity(b),
            "Fixture or real-repository revision differs",
        )?;
        require(
            a["recovery_verified"] == true && b["recovery_verified"] == true,
            "Recovery must be verified",
        )?;
        require(
            a["dirty_recovery_verified"] == true && b["dirty_recovery_verified"] == true,
            "Dirty recovery must be verified; regenerate older reports",
        )?;
        let before = a["warm_median_seconds"].as_f64().unwrap_or(0.0);
        let after = b["warm_median_seconds"].as_f64().unwrap_or(0.0);
        require(before > 0.0 && after > 0.0, "Invalid median timing")?;
        output.insert(
            name,
            json!({"before_seconds": before, "after_seconds": after, "ratio": after / before}),
        );
    }
    Ok(output.into())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Value {
        json!({"schema_version":2,"measurement_version":2,"status":"passed","mode":"benchmark","fixture_version":1,
        "workload_sha256":"work","environment_lock_sha256":"lock","runs":5,
        "source":{"revision":"old"},"binary_sha256":"old",
        "environment":{"canonical":true,"tools":{"git":"2.40.0"},"kernel":"kernel",
        "architecture":"x86_64","cpu_model":"cpu","cpu_quota":"400000 100000",
        "memory_limit":"8589934592","filesystem":"ext2/ext3","packages_sha256":"packages","image_id":"old"},
        "results":{"smoke":{"warm_median_seconds":2,"fixture_tree":"same","recovery_verified":true,"dirty_recovery_verified":true}}})
    }
    #[test]
    fn comparisons_allow_new_code_but_reject_environment_or_workload_drift() {
        let a = sample();
        let mut b = sample();
        b["source"]["revision"] = json!("new");
        b["binary_sha256"] = json!("new");
        b["environment"]["image_id"] = json!("new");
        b["results"]["smoke"]["warm_median_seconds"] = json!(1);
        assert_eq!(compare(&a, &b).unwrap()["smoke"]["ratio"], 0.5);
        for key in [
            "tools",
            "kernel",
            "cpu_quota",
            "memory_limit",
            "filesystem",
            "packages_sha256",
            "canonical",
        ] {
            let mut drift = b.clone();
            drift["environment"][key] = json!("different");
            assert!(compare(&a, &drift).is_err(), "{key}");
        }
        for key in [
            "status",
            "schema_version",
            "measurement_version",
            "workload_sha256",
            "runs",
            "mode",
        ] {
            let mut drift = b.clone();
            drift[key] = json!("different");
            assert!(compare(&a, &drift).is_err(), "{key}");
        }
        for (key, value) in [
            ("fixture_tree", json!("different")),
            ("warm_median_seconds", json!(0)),
            ("recovery_verified", json!(false)),
            ("dirty_recovery_verified", json!(false)),
        ] {
            let mut drift = b.clone();
            drift["results"]["smoke"][key] = value;
            assert!(compare(&a, &drift).is_err());
        }
        assert!(compare(&json!({}), &json!({})).is_err());
        let mut legacy = b.clone();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("measurement_version");
        assert!(compare(&legacy, &legacy).is_err());
        let mut unverified = b.clone();
        unverified["results"]["smoke"]
            .as_object_mut()
            .unwrap()
            .remove("dirty_recovery_verified");
        assert!(compare(&unverified, &unverified).is_err());
        let mut missing = b.clone();
        missing["environment"]
            .as_object_mut()
            .unwrap()
            .remove("kernel");
        assert!(compare(&missing, &missing).is_err());
        b["results"].as_object_mut().unwrap().remove("smoke");
        assert!(compare(&a, &b).is_err());
    }
}
