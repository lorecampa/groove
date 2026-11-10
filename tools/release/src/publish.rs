use crate::{archive::digest, process::require, Release, Result, PACKAGES};
use serde::Deserialize;
use std::{
    fs,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize)]
struct RegistryEntry {
    vers: String,
    cksum: String,
    #[serde(default)]
    yanked: bool,
}

fn registry_response(status: &str, body: &[u8], version: &str) -> Result<Option<RegistryEntry>> {
    if status.trim() == "404" {
        return Ok(None);
    }
    require(
        status.trim() == "200",
        format!("Registry HTTP error {}", status.trim()),
    )?;
    for line in body.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let entry: RegistryEntry = serde_json::from_slice(line)?;
        if entry.vers == version {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

fn verify(entry: &RegistryEntry, checksum: &str, name: &str) -> Result<()> {
    require(
        !entry.yanked,
        format!("{name} {} is yanked; prepare a new release", entry.vers),
    )?;
    require(
        entry.cksum == checksum,
        format!(
            "{name} {} already exists with different contents; do not reuse or move this tag",
            entry.vers
        ),
    )
}

trait Publication {
    fn package(&mut self, name: &str) -> Result<String>;
    fn entry(&mut self, name: &str) -> Result<Option<RegistryEntry>>;
    fn upload(&mut self, name: &str) -> Result<()>;
    fn wait(&mut self, name: &str, checksum: &str) -> Result<()>;
    fn install(&mut self) -> Result<()>;
}

fn publish_in_order(backend: &mut impl Publication) -> Result<()> {
    for name in PACKAGES {
        let checksum = backend.package(name)?;
        if let Some(entry) = backend.entry(name)? {
            verify(&entry, &checksum, name)?;
            eprintln!(
                "Already published with identical contents: {name} {}",
                entry.vers
            );
        } else {
            backend.upload(name)?;
        }
        backend.wait(name, &checksum)?;
    }
    backend.install()
}

struct Publisher<'a> {
    release: &'a Release,
    version: String,
}

impl Release {
    pub(crate) fn publish_registry(&self, tag: &str, execute: bool) -> Result<()> {
        require(
            execute,
            "Publishing requires --execute; use packages to check without uploading",
        )?;
        let version = self.validate(Some(tag), Some("origin/main"))?;
        publish_in_order(&mut Publisher {
            release: self,
            version,
        })
    }
}

impl Publication for Publisher<'_> {
    fn package(&mut self, name: &str) -> Result<String> {
        let target = self.release.root.join("target/registry-release");
        self.release.runner.run(
            &self.release.root,
            "cargo",
            args!["package", "--locked", "-p", name, "--target-dir", target],
        )?;
        digest(
            &target
                .join("package")
                .join(format!("{name}-{}.crate", self.version)),
        )
    }
    fn entry(&mut self, name: &str) -> Result<Option<RegistryEntry>> {
        let response = tempfile::NamedTempFile::new()?;
        let url = format!(
            "https://index.crates.io/{}/{}/{name}",
            &name[..2],
            &name[2..4]
        );
        let status = self.release.runner.text(
            &self.release.root,
            "curl",
            args![
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--max-time",
                "30",
                "--user-agent",
                "groove-release-infrastructure",
                "--header",
                "Cache-Control: no-cache",
                "--output",
                response.path(),
                "--write-out",
                "%{http_code}",
                url,
            ],
        )?;
        registry_response(&status, &fs::read(response.path())?, &self.version)
    }
    fn upload(&mut self, name: &str) -> Result<()> {
        let target = self.release.root.join("target/registry-release");
        self.release.runner.run(
            &self.release.root,
            "cargo",
            args![
                "publish",
                "--locked",
                "--registry",
                "crates-io",
                "-p",
                name,
                "--target-dir",
                target
            ],
        )?;
        Ok(())
    }
    fn wait(&mut self, name: &str, checksum: &str) -> Result<()> {
        wait_for_publication(
            || self.entry(name),
            name,
            checksum,
            Duration::from_secs(300),
            Duration::from_secs(5),
        )
    }
    fn install(&mut self) -> Result<()> {
        let installation = tempfile::Builder::new()
            .prefix("groove-registry-install-")
            .tempdir()?;
        for name in &PACKAGES[1..] {
            self.release.runner.run(
                &self.release.root,
                "cargo",
                args![
                    "install",
                    "--locked",
                    "--registry",
                    "crates-io",
                    "--version",
                    format!("={}", self.version),
                    "--root",
                    installation.path(),
                    name
                ],
            )?;
        }
        self.release
            .smoke(&installation.path().join("bin"), &self.version)
    }
}

fn wait_for_publication(
    mut entry: impl FnMut() -> Result<Option<RegistryEntry>>,
    name: &str,
    checksum: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<()> {
    let started = Instant::now();
    loop {
        if let Some(entry) = entry()? {
            return verify(&entry, checksum, name);
        }
        require(
            started.elapsed() < timeout,
            format!("Timed out waiting for {name}; rerun the same tag"),
        )?;
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemoryPublisher {
        entries: BTreeMap<String, RegistryEntry>,
        events: Vec<String>,
    }
    impl Publication for MemoryPublisher {
        fn package(&mut self, name: &str) -> Result<String> {
            Ok(name.into())
        }
        fn entry(&mut self, name: &str) -> Result<Option<RegistryEntry>> {
            Ok(self.entries.get(name).cloned())
        }
        fn upload(&mut self, name: &str) -> Result<()> {
            if name != "groove-core" {
                require(
                    self.events.iter().any(|event| event == "wait groove-core"),
                    "Core is not indexed",
                )?;
            }
            self.events.push(format!("upload {name}"));
            self.entries.insert(
                name.into(),
                RegistryEntry {
                    vers: "0.1.0".into(),
                    cksum: name.into(),
                    yanked: false,
                },
            );
            Ok(())
        }
        fn wait(&mut self, name: &str, checksum: &str) -> Result<()> {
            verify(&self.entries[name], checksum, name)?;
            self.events.push(format!("wait {name}"));
            Ok(())
        }
        fn install(&mut self) -> Result<()> {
            self.events.push("install".into());
            Ok(())
        }
    }

    #[test]
    fn publication_orders_dependencies_and_resumes_only_identical_uploads() {
        for partial in [false, true] {
            let mut publisher = MemoryPublisher::default();
            if partial {
                publisher.entries.insert(
                    "groove-core".into(),
                    RegistryEntry {
                        vers: "0.1.0".into(),
                        cksum: "groove-core".into(),
                        yanked: false,
                    },
                );
            }
            publish_in_order(&mut publisher).unwrap();
            let uploads: Vec<_> = publisher
                .events
                .iter()
                .filter(|event| event.starts_with("upload "))
                .collect();
            let expected: Vec<_> = PACKAGES[usize::from(partial)..]
                .iter()
                .map(|name| format!("upload {name}"))
                .collect();
            assert_eq!(uploads, expected.iter().collect::<Vec<_>>());
            assert_eq!(publisher.events.last().unwrap(), "install");
        }
    }
    #[test]
    fn conflicting_and_yanked_uploads_stop_before_publishing_dependents() {
        for (checksum, yanked) in [("different", false), ("groove-core", true)] {
            let mut publisher = MemoryPublisher::default();
            publisher.entries.insert(
                "groove-core".into(),
                RegistryEntry {
                    vers: "0.1.0".into(),
                    cksum: checksum.into(),
                    yanked,
                },
            );
            assert!(publish_in_order(&mut publisher).is_err());
            assert!(publisher.events.is_empty());
        }
    }
    #[test]
    fn registry_outages_and_invalid_responses_are_not_missing_versions() {
        assert!(registry_response("503", b"outage", "0.1.0").is_err());
        assert!(registry_response("200", b"invalid", "0.1.0").is_err());
        assert!(registry_response("404", b"missing", "0.1.0")
            .unwrap()
            .is_none());
        let body = br#"{"vers":"0.1.0","cksum":"expected","yanked":false}"#;
        assert_eq!(
            registry_response("200", body, "0.1.0")
                .unwrap()
                .unwrap()
                .cksum,
            "expected"
        );
    }
    #[test]
    fn propagation_wait_checks_checksum_and_has_a_deadline() {
        let mut attempts = 0;
        wait_for_publication(
            || {
                attempts += 1;
                Ok((attempts == 2).then(|| RegistryEntry {
                    vers: "0.1.0".into(),
                    cksum: "expected".into(),
                    yanked: false,
                }))
            },
            "groove-core",
            "expected",
            Duration::from_secs(1),
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(attempts, 2);
        assert!(wait_for_publication(
            || Ok(None),
            "groove-core",
            "expected",
            Duration::ZERO,
            Duration::ZERO
        )
        .is_err());
    }
}
