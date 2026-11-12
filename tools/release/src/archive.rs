use crate::{manifest, process::require, read_toml, Release, Result, BINARIES, PACKAGES};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub(crate) fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn unpack(
    archive: &Path,
    destination: &Path,
    name: &str,
    version: &str,
) -> Result<PathBuf> {
    let prefix = format!("{name}-{version}");
    let mut source = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut names = BTreeSet::new();
    for entry in source.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        require(
            entry.header().entry_type().is_file(),
            "Unexpected non-file entry in crate archive",
        )?;
        require(
            path.components().all(|p| matches!(p, Component::Normal(_))),
            "Unsafe crate archive path",
        )?;
        let relative = path
            .strip_prefix(&prefix)
            .map_err(|_| crate::Error::Invalid("Unexpected crate archive prefix".into()))?;
        require(
            relative.components().all(|p| {
                let p = p.as_os_str().to_string_lossy();
                !p.starts_with(".env") && p != ".git"
            }),
            "Private environment or Git files found in crate archive",
        )?;
        require(
            names.insert(relative.to_path_buf()),
            "Duplicate crate archive entry",
        )?;
    }
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "README.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
    ] {
        require(
            names.contains(Path::new(name)),
            format!("Missing release metadata in crate archive: {name}"),
        )?;
    }
    let mut source = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    source.unpack(destination)?;
    let package = destination.join(prefix);
    let manifest = read_toml(&package.join("Cargo.toml"))?;
    require(
        manifest::text(&manifest, &["package", "version"])? == version,
        "Packaged version mismatch",
    )?;
    require(
        manifest.get("workspace").is_none(),
        "Packaged manifest still depends on a workspace",
    )?;
    Ok(package)
}

impl Release {
    pub(crate) fn check_packages(
        &self,
        allow_dirty: bool,
        target: Option<&str>,
        release: bool,
    ) -> Result<()> {
        let version = self.validate(None, None)?;
        let scratch = tempfile::Builder::new()
            .prefix("groove-packages-")
            .tempdir()?;
        // Each extracted source tree needs a fresh target directory. Cargo can
        // otherwise reuse artifacts from a prior temporary tree at the same
        // version (including libraries built from older package contents).
        let build = scratch.path().join("build");
        let mut package_patch = Vec::new();
        let mut build_patch = Vec::new();
        for name in PACKAGES {
            let mut command = args![
                "package",
                "-p",
                name,
                "--locked",
                "--no-verify",
                "--target-dir",
                scratch.path().join("target")
            ];
            command.extend(package_patch.clone());
            if allow_dirty {
                command.push("--allow-dirty".into());
            }
            self.runner.run(&self.root, "cargo", command)?;
            let archive = scratch
                .path()
                .join("target/package")
                .join(format!("{name}-{version}.crate"));
            let package = unpack(&archive, &scratch.path().join("source"), name, &version)?;
            let mut command = args![
                "build",
                "--all-targets",
                "--locked",
                "--manifest-path",
                package.join("Cargo.toml"),
                "--target-dir",
                build
            ];
            if let Some(target) = target {
                command.extend(args!["--target", target]);
            }
            if release {
                command.push("--release".into());
            }
            command.extend(build_patch.clone());
            self.runner.run(&self.root, "cargo", command)?;
            if name == "groove-core" {
                // Packaging resolves the local metadata; compilation uses ONLY the extracted core.
                // Real crates.io publication below never uses these overrides.
                let patch = |path: &Path| {
                    format!(
                        "patch.crates-io.groove-core.path={}",
                        toml::Value::String(path.to_string_lossy().into_owned())
                    )
                };
                package_patch = args!["--config", patch(&self.root.join("crates/groove-core"))];
                build_patch = args!["--config", patch(&package)];
            }
        }
        let profile = if release { "release" } else { "debug" };
        let binaries = build.join(target.unwrap_or_default()).join(profile);
        self.smoke(&binaries, &version)?;
        // Expose only smoke-tested executables at the documented bundle path;
        // all compiler outputs and extracted sources remain invocation-local.
        let output = self
            .root
            .join("target/package-check")
            .join(target.unwrap_or_default())
            .join(profile);
        fs::create_dir_all(&output)?;
        for name in BINARIES {
            fs::copy(binaries.join(name), output.join(name))?;
        }
        Ok(())
    }

    pub(crate) fn bundle_archive(
        &self,
        binaries: &Path,
        target: &str,
        output: &Path,
    ) -> Result<()> {
        require(
            !target.is_empty()
                && target
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid binary target",
        )?;
        let version = self.validate(None, None)?;
        self.smoke(binaries, &version)?;
        fs::create_dir_all(output)?;
        let prefix = format!("groove-{version}-{target}");
        let archive = output.join(format!("{prefix}.tar.gz"));
        let mut tar = tar::Builder::new(GzEncoder::new(
            File::create(&archive)?,
            Compression::default(),
        ));
        tar.follow_symlinks(false);
        for name in BINARIES {
            tar.append_path_with_name(binaries.join(name), Path::new(&prefix).join(name))?;
        }
        for name in [
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "README.md",
            "CHANGELOG.md",
            "CONTRIBUTING.md",
            "assets",
            "docs",
            "skills/groove",
        ] {
            let source = self.root.join(name);
            let path = Path::new(&prefix).join(name);
            if source.is_dir() {
                tar.append_dir_all(path, source)?;
            } else {
                tar.append_path_with_name(source, path)?;
            }
        }
        tar.into_inner()?.finish()?;
        // Distinct names let all native targets share one release without
        // overwriting another target's metadata or checksums.
        let manifest = output.join(format!("{prefix}.manifest.json"));
        fs::write(
            &manifest,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&self.build_metadata(target, &archive)?)?
            ),
        )?;
        let mut sums = String::new();
        for path in [&archive, &manifest] {
            sums.push_str(&format!(
                "{}  {}\n",
                digest(path)?,
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        fs::write(output.join(format!("SHA256SUMS-{target}")), sums)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn archive(path: &Path, extra: Option<(&str, tar::EntryType)>, metadata: bool) {
        let mut tar = tar::Builder::new(GzEncoder::new(
            File::create(path).unwrap(),
            Compression::default(),
        ));
        if metadata {
            for name in [
                "Cargo.toml",
                "Cargo.lock",
                "README.md",
                "LICENSE-MIT",
                "LICENSE-APACHE",
            ] {
                let body = if name == "Cargo.toml" {
                    "[package]\nversion = \"0.1.0\"\n"
                } else {
                    "fixture\n"
                };
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_size(body.len() as u64);
                header.set_cksum();
                tar.append_data(
                    &mut header,
                    format!("groove-core-0.1.0/{name}"),
                    Cursor::new(body),
                )
                .unwrap();
            }
        }
        if let Some((name, entry_type)) = extra {
            let mut header = tar::Header::new_gnu();
            // Construct the raw path so the fixture can include paths rejected by Builder::set_path.
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_entry_type(entry_type);
            header.set_mode(0o644);
            header.set_size(0);
            if entry_type.is_symlink() {
                header.set_link_name("../../escaped").unwrap();
            }
            header.set_cksum();
            tar.append(&header, Cursor::new([])).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extracted_package_is_independent_and_includes_release_metadata() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source.crate");
        archive(&source, None, true);
        let extracted = unpack(
            &source,
            &scratch.path().join("output"),
            "groove-core",
            "0.1.0",
        )
        .unwrap();
        assert!(extracted.join("LICENSE-MIT").is_file());
        assert!(extracted.join("Cargo.lock").is_file());
        assert_eq!(
            manifest::text(
                &read_toml(&extracted.join("Cargo.toml")).unwrap(),
                &["package", "version"]
            )
            .unwrap(),
            "0.1.0"
        );
    }

    #[test]
    fn missing_metadata_traversal_links_and_environment_files_are_rejected() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source.crate");
        let destination = scratch.path().join("output");
        archive(&source, None, false);
        assert!(unpack(&source, &destination, "groove-core", "0.1.0").is_err());
        for (path, kind) in [
            ("groove-core-0.1.0/../../escaped", tar::EntryType::Regular),
            ("groove-core-0.1.0/link", tar::EntryType::Symlink),
            ("groove-core-0.1.0/.env", tar::EntryType::Regular),
            ("other-0.1.0/file", tar::EntryType::Regular),
            ("groove-core-0.1.0/README.md", tar::EntryType::Regular),
        ] {
            archive(&source, Some((path, kind)), true);
            assert!(
                unpack(&source, &destination, "groove-core", "0.1.0").is_err(),
                "{path}"
            );
        }
        assert!(!scratch.path().join("escaped").exists());
        assert!(!destination.exists());
    }
}
