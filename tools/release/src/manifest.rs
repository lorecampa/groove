use crate::{process::require, read_toml, Error, Release, Result, PACKAGES};
use semver::Version;
use std::{collections::BTreeSet, fs, path::Path};
use toml::Value;

pub(crate) fn version(text: &str) -> Result<Version> {
    let version = Version::parse(text)?;
    require(
        version.build.is_empty(),
        "Release versions must not contain build metadata",
    )?;
    Ok(version)
}

fn field<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Value> {
    keys.iter().try_fold(value, |value, key| {
        value
            .get(key)
            .ok_or_else(|| Error::Invalid(format!("Missing manifest field {}", keys.join("."))))
    })
}

pub(crate) fn text<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a str> {
    field(value, keys)?
        .as_str()
        .ok_or_else(|| Error::Invalid(format!("Expected string: {}", keys.join("."))))
}

impl Release {
    pub(crate) fn validate_manifest(
        &self,
        tag: Option<&str>,
        branch: Option<&str>,
    ) -> Result<String> {
        let manifest = read_toml(&self.root.join("Cargo.toml"))?;
        let current = text(&manifest, &["workspace", "package", "version"])?;
        version(current)?;
        let members = field(&manifest, &["workspace", "members"])?
            .as_array()
            .ok_or_else(|| Error::Invalid("Expected workspace member list".into()))?;
        let public: BTreeSet<_> = PACKAGES
            .iter()
            .map(|name| format!("crates/{name}"))
            .collect();
        let members: BTreeSet<_> = members
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::Invalid("Expected workspace member path".into()))
            })
            .collect::<Result<_>>()?;
        require(
            public
                .iter()
                .all(|member| members.contains(member.as_str())),
            "A release package is missing from the workspace",
        )?;
        for member in members.iter().filter(|member| !public.contains(**member)) {
            let private = read_toml(&self.root.join(member).join("Cargo.toml"))?;
            require(private.get("package").and_then(|p| p.get("publish")).and_then(Value::as_bool) == Some(false),
                    format!("Additional workspace member {member} must declare publish = false or join the release list"))?;
        }
        let core = field(&manifest, &["workspace", "dependencies", "groove-core"])?;
        require(
            text(core, &["path"])? == "crates/groove-core" && text(core, &["version"])? == current,
            "Workspace groove-core dependency must match the workspace version and path",
        )?;
        for name in PACKAGES {
            let package = read_toml(&self.root.join("crates").join(name).join("Cargo.toml"))?;
            require(
                text(&package, &["package", "name"])? == name,
                "Unexpected package name",
            )?;
            require(
                field(&package, &["package", "version", "workspace"])?.as_bool() == Some(true),
                format!("{name} must inherit the workspace version"),
            )?;
            if let Some(publish) = package["package"].get("publish") {
                let crates_io = publish
                    .as_array()
                    .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some("crates-io"));
                require(
                    publish.as_bool() == Some(true) || crates_io,
                    format!("{name} is not publishable to crates.io"),
                )?;
            }
            if name != "groove-core" {
                require(
                    field(&package, &["dependencies", "groove-core", "workspace"])?.as_bool()
                        == Some(true),
                    format!("{name} must inherit the workspace groove-core dependency"),
                )?;
            }
        }
        let lock = read_toml(&self.root.join("Cargo.lock"))?;
        let packages = field(&lock, &["package"])?
            .as_array()
            .ok_or_else(|| Error::Invalid("Expected lockfile packages".into()))?;
        for name in PACKAGES {
            let entries: Vec<_> = packages
                .iter()
                .filter(|p| {
                    p.get("name").and_then(Value::as_str) == Some(name) && p.get("source").is_none()
                })
                .collect();
            require(entries.len() == 1 && entries[0].get("version").and_then(Value::as_str) == Some(current),
                    format!("Cargo.lock version mismatch for {name}; run cargo update --workspace --offline"))?;
        }
        if let Some(tag) = tag {
            require(
                tag == format!("v{current}"),
                format!("Tag {tag:?} must equal v{current}"),
            )?;
            self.clean_tree()?;
            let head = self.git(args!["rev-parse", "HEAD"])?;
            let tagged = self.git(args!["rev-parse", format!("refs/tags/{tag}^{{commit}}")])?;
            require(
                head.trim() == tagged.trim(),
                "HEAD must be the exact tagged commit",
            )?;
            notes(&self.root, current)?;
        }
        if let Some(branch) = branch {
            require(tag.is_some(), "Branch validation requires a release tag")?;
            self.git(args!["merge-base", "--is-ancestor", "HEAD", branch])?;
        }
        Ok(current.into())
    }

    pub(crate) fn prepare_manifest(&self, next: &str) -> Result<()> {
        let current = self.validate(None, None)?;
        require(
            version(next)? >= version(&current)?,
            "Cannot prepare an older version",
        )?;
        self.clean_tree()?;
        let mut paths = vec![
            self.root.join("Cargo.toml"),
            self.root.join("Cargo.lock"),
            self.root.join("CHANGELOG.md"),
        ];
        let fuzz_manifest = self.root.join("fuzz/Cargo.toml");
        if fuzz_manifest.exists() {
            paths.extend([fuzz_manifest.clone(), self.root.join("fuzz/Cargo.lock")]);
        }
        let original: Vec<_> = paths.iter().map(fs::read).collect::<std::io::Result<_>>()?;
        let changelog =
            String::from_utf8(original[2].clone()).map_err(|e| Error::Invalid(e.to_string()))?;
        require(
            !changelog
                .lines()
                .any(|line| line.starts_with(&format!("## [{next}]"))),
            "That version already has a changelog entry",
        )?;
        require(
            changelog
                .lines()
                .filter(|line| *line == "## Unreleased")
                .count()
                == 1,
            "Expected one ## Unreleased changelog heading",
        )?;
        let pending = changelog
            .split_once("## Unreleased\n")
            .map(|(_, tail)| tail.split("\n## ").next().unwrap_or_default());
        require(
            pending.is_some_and(|notes| !notes.trim().is_empty()),
            "There are no unreleased notes",
        )?;
        let result = (|| {
            let mut document = fs::read_to_string(&paths[0])?.parse::<toml_edit::DocumentMut>()?;
            document["workspace"]["package"]["version"] = toml_edit::value(next);
            document["workspace"]["dependencies"]["groove-core"]["version"] =
                toml_edit::value(next);
            fs::write(&paths[0], document.to_string())?;
            if fuzz_manifest.exists() {
                let mut fuzz =
                    fs::read_to_string(&fuzz_manifest)?.parse::<toml_edit::DocumentMut>()?;
                fuzz["dependencies"]["groove-core"]["version"] = toml_edit::value(next);
                fs::write(&fuzz_manifest, fuzz.to_string())?;
            }
            self.runner.run(
                &self.root,
                "cargo",
                args!["update", "--workspace", "--offline"],
            )?;
            if fuzz_manifest.exists() {
                self.runner.run(
                    &self.root,
                    "cargo",
                    args![
                        "update",
                        "--workspace",
                        "--offline",
                        "--manifest-path",
                        fuzz_manifest.as_os_str()
                    ],
                )?;
            }
            self.validate(None, None)?;
            let today = chrono::Utc::now().date_naive();
            fs::write(
                &paths[2],
                changelog.replacen(
                    "## Unreleased\n",
                    &format!("## Unreleased\n\n## [{next}] - {today}\n"),
                    1,
                ),
            )?;
            notes(&self.root, next)?;
            Ok(())
        })();
        if let Err(error) = result {
            for (path, bytes) in paths.iter().zip(&original) {
                fs::write(path, bytes)?;
            }
            return Err(error);
        }
        eprintln!(
            "Prepared {next}. Review the diff and open a release PR. No commit or tag was created."
        );
        Ok(())
    }
}

pub(crate) fn notes(root: &Path, version: &str) -> Result<String> {
    let changelog = fs::read_to_string(root.join("CHANGELOG.md"))?;
    let prefix = format!("## [{version}] - ");
    let mut matches = 0;
    let mut collecting = false;
    let mut lines = Vec::new();
    for line in changelog.lines() {
        if line.starts_with("## ") {
            collecting = false;
            if let Some(date) = line.strip_prefix(&prefix) {
                let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .map_err(|e| Error::Invalid(format!("Invalid release date: {e}")))?;
                require(
                    parsed.to_string() == date,
                    "Release date must use YYYY-MM-DD",
                )?;
                matches += 1;
                collecting = true;
            }
        } else if collecting {
            lines.push(line);
        }
    }
    require(
        matches == 1,
        format!("Expected one changelog heading: ## [{version}] - YYYY-MM-DD"),
    )?;
    let notes = lines.join("\n");
    require(!notes.trim().is_empty(), "Release notes must not be empty")?;
    Ok(format!("{}\n", notes.trim()))
}
