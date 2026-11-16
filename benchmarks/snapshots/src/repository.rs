//! Real public repositories pinned by commit and tree identity.
//!
//! `fetch` is the only networked operation in the lab. It downloads the complete
//! history reachable from two pinned commits into a local fixture cache, verifies
//! every pin and records the exact pack files. Benchmarks and checks never fetch:
//! they re-verify the cache, then clone it into independent per-block storage.
use crate::{
    profiles::{Case, Contents, Profile},
    util::{digest, hash, require, write_json, Error, Result, Runner},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const CATALOG_BYTES: &[u8] = include_bytes!("../repositories-v1.json");
/// Untracked names the shared workloads create. A pinned tree must not track them.
const RESERVED: [&str; 4] = [
    "ignored",
    "untracked-build-output",
    "untracked-binary",
    "history",
];
const MANIFEST: &str = "groove-bench-repository.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    pub(crate) schema_version: u32,
    pub(crate) repositories: Vec<Repository>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Repository {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) license: String,
    pub(crate) shape: String,
    /// Commit measured by default; HEAD of every block clone.
    pub(crate) tip: Pin,
    /// Second commit for backward/forward switches; need not be an ancestor.
    pub(crate) base: Pin,
    /// Tracked index entries in the tip tree.
    pub(crate) tracked_paths: usize,
    /// Commits reachable from tip or base; binds the complete fetched history.
    pub(crate) reachable_commits: u64,
    /// Admission upper bounds, verified against the fetched cache.
    pub(crate) budget: Budget,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pin {
    #[serde(rename = "ref")]
    pub(crate) reference: String,
    pub(crate) commit: String,
    pub(crate) tree: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Budget {
    pub(crate) object_bytes: u64,
    pub(crate) checkout_bytes: u64,
}
impl Repository {
    pub(crate) fn sha256(&self) -> Result<String> {
        Ok(hash(&serde_json::to_vec(self)?))
    }
    fn directory(&self, cache: &Path) -> PathBuf {
        cache.join(format!("{}.git", self.id))
    }
}
fn object_id(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate(repository: &Repository) -> Result<()> {
    let id_ok = !repository.id.is_empty()
        && repository
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    require(id_ok, "Invalid repository identity")?;
    require(
        repository.url.starts_with("https://")
            && !repository.license.is_empty()
            && !repository.shape.is_empty(),
        format!(
            "{}: repository needs an HTTPS URL, license and shape",
            repository.id
        ),
    )?;
    for pin in [&repository.tip, &repository.base] {
        require(
            pin.reference.starts_with("refs/tags/")
                && object_id(&pin.commit)
                && object_id(&pin.tree),
            format!(
                "{}: pins need a tag ref and full SHA-1 commit/tree",
                repository.id
            ),
        )?;
    }
    require(
        repository.tip.commit != repository.base.commit
            && repository.tip.reference != repository.base.reference,
        format!("{}: tip and base must differ", repository.id),
    )?;
    require(
        (12..=1_000_000).contains(&repository.tracked_paths)
            && repository.reachable_commits >= 2
            && repository.budget.object_bytes > 0
            && repository.budget.checkout_bytes > 0,
        format!("{}: invalid size contract", repository.id),
    )
}
pub(crate) fn catalog() -> Result<Catalog> {
    let catalog: Catalog = serde_json::from_slice(CATALOG_BYTES)?;
    require(
        catalog.schema_version == 1,
        "Unsupported repository catalog version",
    )?;
    let mut ids = BTreeSet::new();
    for repository in &catalog.repositories {
        require(ids.insert(&repository.id), "Duplicate repository identity")?;
        validate(repository)?;
    }
    Ok(catalog)
}
pub(crate) fn find(id: &str) -> Result<Repository> {
    catalog()?
        .repositories
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| Error::Invalid(format!("Unknown pinned repository '{id}'")))
}
/// Pinned repositories referenced by the selected profiles, in catalog order.
pub(crate) fn required(profiles: &[Profile]) -> Result<Vec<Repository>> {
    let ids: BTreeSet<&str> = profiles
        .iter()
        .flat_map(|p| &p.cases)
        .filter_map(|c| match &c.contents {
            Contents::Repository { repository } => Some(repository.as_str()),
            _ => None,
        })
        .collect();
    Ok(catalog()?
        .repositories
        .into_iter()
        .filter(|r| ids.contains(r.id.as_str()))
        .collect())
}
/// Repository definitions keyed by identity, as embedded in reports and profile hashes.
pub(crate) fn definitions(profile: &Profile) -> Result<BTreeMap<String, Repository>> {
    Ok(required(std::slice::from_ref(profile))?
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect())
}

/// Default cache: outside `target/` so `cargo clean` and Groove's pristine cleanup
/// of a pooled checkout do not discard multi-GiB downloads.
pub(crate) fn default_cache() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("target"))
        .join("groove-bench/repositories")
}

fn count_objects(runner: &Runner, repo: &Path) -> Result<BTreeMap<String, u64>> {
    let text = runner.git_text(repo, &["count-objects", "-v"])?;
    let mut counts = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once(": ")
            .ok_or_else(|| Error::Invalid("Unexpected count-objects output".into()))?;
        let value = value
            .parse()
            .map_err(|_| Error::Invalid("Unexpected count-objects value".into()))?;
        counts.insert(key.to_owned(), value);
    }
    Ok(counts)
}
fn packs(repo: &Path) -> Result<Vec<Value>> {
    let mut packs = Vec::new();
    for entry in fs::read_dir(repo.join("objects/pack"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "pack") {
            packs.push(json!({
                "name": path.file_name().expect("pack file name").to_string_lossy(),
                "bytes": fs::metadata(&path)?.len(),
                "sha256": digest(&path)?,
            }));
        }
    }
    packs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(packs)
}
fn tree_entries(
    runner: &Runner,
    repo: &Path,
    revision: &str,
) -> Result<Vec<(String, String, u64)>> {
    let raw = runner.git(repo, ["ls-tree", "-r", "-z", "-l", "--full-tree", revision])?;
    raw.split(|b| *b == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let record = std::str::from_utf8(record)
                .map_err(|_| Error::Invalid("Pinned tree has a non-UTF-8 path".into()))?;
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| Error::Invalid("Unexpected ls-tree record".into()))?;
            let fields: Vec<_> = meta.split_whitespace().collect();
            require(fields.len() == 4, "Unexpected ls-tree metadata")?;
            let size = fields[3].parse().unwrap_or(0);
            Ok((fields[0].to_owned(), path.to_owned(), size))
        })
        .collect()
}

/// Check a cache directory against its pinned definition, including pack bytes.
/// Returns the verified manifest. Never writes to the cache.
pub(crate) fn verify(runner: &Runner, cache: &Path, repository: &Repository) -> Result<Value> {
    let repo = repository.directory(cache);
    let missing = || {
        Error::Invalid(format!(
            "Pinned repository '{}' is missing or stale in {}; run `cargo run -p groove-bench --locked -- fetch PROFILE`",
            repository.id,
            cache.display()
        ))
    };
    let manifest: Value =
        serde_json::from_slice(&fs::read(repo.join(MANIFEST)).map_err(|_| missing())?)?;
    require(
        manifest["definition_sha256"] == repository.sha256()?,
        missing().to_string(),
    )?;
    let id = &repository.id;
    for (branch, pin) in [
        ("bench-tip", &repository.tip),
        ("bench-base", &repository.base),
    ] {
        require(
            runner.git_text(
                &repo,
                &["rev-parse", &format!("refs/heads/{branch}^{{commit}}")],
            )? == pin.commit
                && runner.git_text(
                    &repo,
                    &["rev-parse", &format!("refs/heads/{branch}^{{tree}}")],
                )? == pin.tree,
            format!("{id}: {branch} differs from its pinned commit/tree"),
        )?;
    }
    require(
        runner.git_text(&repo, &["symbolic-ref", "HEAD"])? == "refs/heads/bench-tip"
            && runner.git_text(&repo, &["for-each-ref", "--format=%(refname)"])?
                == "refs/heads/bench-base\nrefs/heads/bench-tip",
        format!("{id}: cache must contain exactly the two pinned branches"),
    )?;
    let commits: u64 = runner
        .git_text(&repo, &["rev-list", "--count", "bench-tip", "bench-base"])?
        .parse()
        .map_err(|_| Error::Invalid("Invalid commit count".into()))?;
    require(
        commits == repository.reachable_commits,
        format!(
            "{id}: {commits} reachable commits, pinned {}",
            repository.reachable_commits
        ),
    )?;
    let mut checkout = 0;
    for (branch, expected) in [
        ("bench-tip", Some(repository.tracked_paths)),
        ("bench-base", None),
    ] {
        let entries = tree_entries(runner, &repo, branch)?;
        // Groove rejects submodules; a pinned fixture must be one it supports.
        require(
            entries.iter().all(|(mode, _, _)| mode != "160000"),
            format!("{id}: {branch} contains a submodule"),
        )?;
        require(
            entries.iter().all(|(_, path, _)| {
                let top = path.split('/').next().unwrap_or_default();
                !RESERVED.contains(&top)
            }),
            format!("{id}: {branch} tracks a workload-reserved top-level name"),
        )?;
        if let Some(expected) = expected {
            require(
                entries.len() == expected,
                format!("{id}: {} tracked paths, pinned {expected}", entries.len()),
            )?;
            checkout = entries.iter().map(|(_, _, size)| size).sum::<u64>();
        }
    }
    let counts = count_objects(runner, &repo)?;
    let objects = (counts["size"] + counts["size-pack"]) * 1024;
    require(
        objects <= repository.budget.object_bytes && checkout <= repository.budget.checkout_bytes,
        format!("{id}: fetched {objects} object bytes / {checkout} checkout bytes exceed the pinned budget"),
    )?;
    require(
        manifest["packs"] == json!(packs(&repo)?) && manifest["objects"] == json!(counts),
        format!("{id}: cached objects differ from the fetched manifest"),
    )?;
    Ok(manifest)
}

/// Download one repository's pinned history into the cache, resumably.
/// Partial downloads stay in `<id>.partial.git`; a verified cache is renamed into place.
pub(crate) fn fetch(cache: &Path, repository: &Repository) -> Result<Value> {
    let mut runner = Runner::new();
    runner.timeout = Duration::from_secs(6 * 3600);
    // Only this networked operation sees proxy settings; measured runs never do.
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        if let Some(value) = std::env::var_os(key) {
            runner.env.insert(key.into(), value);
        }
    }
    fs::create_dir_all(cache)?;
    let runner = runner.isolated(&cache.join(".runner"))?;
    if let Ok(manifest) = verify(&runner, cache, repository) {
        eprintln!("{}: verified cached pins", repository.id);
        return Ok(manifest);
    }
    let complete = repository.directory(cache);
    let partial = cache.join(format!("{}.partial.git", repository.id));
    if complete.exists() {
        // A stale or damaged cache keeps its objects for an incremental fetch.
        require(
            !partial.exists(),
            format!(
                "Both {} and {} exist; remove one after inspection",
                complete.display(),
                partial.display()
            ),
        )?;
        fs::rename(&complete, &partial)?;
    }
    if !partial.exists() {
        runner.git(
            cache,
            [
                OsString::from("init"),
                "-q".into(),
                "--bare".into(),
                "--object-format=sha1".into(),
                partial.clone().into(),
            ],
        )?;
    }
    eprintln!(
        "{}: fetching complete history of {} and {} from {} (up to {} MiB)",
        repository.id,
        repository.tip.reference,
        repository.base.reference,
        repository.url,
        repository.budget.object_bytes.div_ceil(1048576)
    );
    let mut args: Vec<OsString> = [
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-write-fetch-head",
        "--",
    ]
    .map(Into::into)
    .to_vec();
    args.push(repository.url.clone().into());
    for pin in [&repository.tip, &repository.base] {
        args.push(format!("+{0}:{0}", pin.reference).into());
    }
    runner.git(&partial, args)?;
    for (branch, pin) in [
        ("bench-tip", &repository.tip),
        ("bench-base", &repository.base),
    ] {
        let commit = runner.git_text(
            &partial,
            &["rev-parse", &format!("{}^{{commit}}", pin.reference)],
        )?;
        require(
            commit == pin.commit,
            format!(
                "{}: upstream {} is {commit}, pinned {}; the tag moved",
                repository.id, pin.reference, pin.commit
            ),
        )?;
        runner.git(
            &partial,
            ["update-ref", &format!("refs/heads/{branch}"), &commit],
        )?;
    }
    for pin in [&repository.tip, &repository.base] {
        runner.git(&partial, ["update-ref", "-d", &pin.reference])?;
    }
    runner.git(&partial, ["symbolic-ref", "HEAD", "refs/heads/bench-tip"])?;
    let manifest = json!({
        "repository": repository.id,
        "definition_sha256": repository.sha256()?,
        "fetch_git": runner.git_text(&partial, &["--version"])?,
        "objects": count_objects(&runner, &partial)?,
        "packs": packs(&partial)?,
    });
    write_json(&partial.join(MANIFEST), &manifest)?;
    fs::rename(&partial, &complete)?;
    verify(&runner, cache, repository).inspect_err(|_| {
        eprintln!(
            "{}: fetched cache failed verification; retained {}",
            repository.id,
            complete.display()
        )
    })
}

/// Clone the verified, read-only cache into independent block storage.
pub(crate) fn seed(cache: &Path, repository: &Repository) -> PathBuf {
    repository.directory(cache)
}

/// Make the workload-owned `ignored/` directory ignored without changing tracked
/// files, then require a clean checkout so every oracle starts from the pinned tree.
pub(crate) fn prepare(runner: &Runner, repo: &Path, repository: &str) -> Result<()> {
    let info = PathBuf::from(runner.git_text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .join("info");
    fs::create_dir_all(&info)?;
    fs::write(info.join("exclude"), "/ignored/\n")?;
    let unignored = runner.capture(
        "git",
        [
            "check-ignore",
            "--no-index",
            "untracked-build-output",
            "untracked-binary",
        ],
        repo,
    )?;
    // Exit 1 with no output means neither name is ignored; 128 is a Git failure.
    require(
        unignored.status.code() == Some(1) && unignored.stdout.is_empty(),
        format!("{repository}: workload residue names are ignored by the pinned tree"),
    )?;
    require(
        runner
            .capture("git", ["check-ignore", "-q", "ignored/probe"], repo)?
            .status
            .success(),
        format!("{repository}: ignored residue is not ignored"),
    )?;
    require(
        runner
            .git(repo, ["status", "--porcelain=v1", "-z"])?
            .is_empty(),
        format!("{repository}: pinned checkout is not clean after materialization"),
    )
}

/// Deterministic edit targets and an independent deletion target among regular,
/// non-executable tracked files of the checkout's HEAD tree.
pub(crate) fn targets(runner: &Runner, repo: &Path) -> Result<(Vec<PathBuf>, PathBuf)> {
    let candidates: Vec<String> = tree_entries(runner, repo, "HEAD")?
        .into_iter()
        .filter(|(mode, _, _)| mode == "100644")
        .map(|(_, path, _)| path)
        .collect();
    require(
        candidates.len() >= 12,
        "Pinned tree has too few regular files",
    )?;
    let positions = crate::fixture::spec().positions(candidates.len());
    let deleted = (0..candidates.len())
        .rev()
        .find(|i| !positions.contains(i))
        .expect("more candidates than edit positions");
    Ok((
        positions
            .iter()
            .map(|&i| PathBuf::from(&candidates[i]))
            .collect(),
        PathBuf::from(&candidates[deleted]),
    ))
}
/// First edit target and deletion target for any profile contents.
pub(crate) fn edited(runner: &Runner, repo: &Path, case: &Case) -> Result<PathBuf> {
    Ok(match case.contents {
        Contents::Repository { .. } => targets(runner, repo)?.0.remove(0),
        _ => PathBuf::from(case.path(0)),
    })
}
pub(crate) fn deleted(runner: &Runner, repo: &Path, case: &Case) -> Result<PathBuf> {
    Ok(match case.contents {
        Contents::Repository { .. } => targets(runner, repo)?.1,
        _ => PathBuf::from(case.path(case.files - 1)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_repository(runner: &Runner, root: &Path) -> (PathBuf, Repository) {
        let source = root.join("upstream");
        fs::create_dir(&source).unwrap();
        runner
            .git(&source, ["init", "-q", "--object-format=sha1"])
            .unwrap();
        for (name, value) in [
            ("user.name", "Pinned"),
            ("user.email", "pinned@example.invalid"),
        ] {
            runner.git(&source, ["config", name, value]).unwrap();
        }
        for i in 0..20 {
            fs::write(source.join(format!("f{i:02}.txt")), format!("{i}\n")).unwrap();
        }
        fs::write(source.join(".gitattributes"), "*.bat text eol=crlf\n").unwrap();
        fs::write(source.join("run.bat"), "echo base\n").unwrap();
        runner.git(&source, ["add", "-A"]).unwrap();
        runner.git(&source, ["commit", "-qm", "base"]).unwrap();
        runner
            .git(&source, ["tag", "-a", "-m", "base", "v1"])
            .unwrap();
        fs::write(source.join("f00.txt"), "changed\n").unwrap();
        runner.git(&source, ["commit", "-qam", "tip"]).unwrap();
        runner
            .git(&source, ["tag", "-a", "-m", "tip", "v2"])
            .unwrap();
        let rev = |r: &str| runner.git_text(&source, &["rev-parse", r]).unwrap();
        let repository = Repository {
            id: "local".into(),
            url: "https://example.invalid/local.git".into(),
            license: "test".into(),
            shape: "local fixture".into(),
            tip: Pin {
                reference: "refs/tags/v2".into(),
                commit: rev("v2^{commit}"),
                tree: rev("v2^{tree}"),
            },
            base: Pin {
                reference: "refs/tags/v1".into(),
                commit: rev("v1^{commit}"),
                tree: rev("v1^{tree}"),
            },
            tracked_paths: 22,
            reachable_commits: 2,
            budget: Budget {
                object_bytes: 1 << 20,
                checkout_bytes: 1 << 20,
            },
        };
        (source, repository)
    }

    #[test]
    fn pinned_catalog_is_valid_and_supported_by_groove() {
        let catalog = catalog().unwrap();
        assert!(catalog.repositories.len() >= 4);
        let mut bad = catalog.repositories[0].clone();
        bad.tip.commit = "HEAD".into();
        assert!(validate(&bad).is_err());
        let mut bad = catalog.repositories[0].clone();
        bad.base = bad.tip.clone();
        assert!(validate(&bad).is_err());
        let mut bad = catalog.repositories[0].clone();
        bad.url = "http://example.invalid/a.git".into();
        assert!(validate(&bad).is_err());
        let changed = Repository {
            reachable_commits: catalog.repositories[0].reachable_commits + 1,
            ..catalog.repositories[0].clone()
        };
        assert_ne!(
            changed.sha256().unwrap(),
            catalog.repositories[0].sha256().unwrap()
        );
    }

    #[test]
    fn fetch_verifies_pins_resumes_and_rejects_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let runner = Runner::new().isolated(temp.path()).unwrap();
        let (source, mut repository) = local_repository(&runner, temp.path());
        let cache = temp.path().join("cache");
        // file:// keeps the test offline while exercising the real fetch path.
        let mut local = repository.clone();
        local.url = format!("file://{}", source.display());
        // Validation requires HTTPS; the fetch path itself accepts any Git URL.
        assert!(validate(&local).is_err());
        let manifest = fetch(&cache, &local).unwrap();
        assert_eq!(manifest["repository"], "local");
        assert!(!cache.join("local.partial.git").exists());
        // A second fetch is a verified no-op.
        assert_eq!(fetch(&cache, &local).unwrap(), manifest);
        // Pin drift is reported as a stale cache, never silently accepted.
        repository.url = local.url.clone();
        repository.reachable_commits = 3;
        let error = verify(&runner, &cache, &repository)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing or stale"), "{error}");
        // Extra refs are rejected even when the manifest matches.
        let repo = cache.join("local.git");
        let tip = runner.git_text(&repo, &["rev-parse", "bench-tip"]).unwrap();
        runner
            .git(&repo, ["update-ref", "refs/heads/extra", &tip])
            .unwrap();
        assert!(verify(&runner, &cache, &local).is_err());
        runner
            .git(&repo, ["update-ref", "-d", "refs/heads/extra"])
            .unwrap();
        verify(&runner, &cache, &local).unwrap();
        // A moved upstream tag fails fetch instead of rewriting the pin.
        runner
            .git(&source, ["tag", "-f", "-a", "-m", "moved", "v2", "v1"])
            .unwrap();
        fs::remove_dir_all(&repo).unwrap();
        let error = fetch(&cache, &local).unwrap_err().to_string();
        assert!(error.contains("tag moved"), "{error}");
    }

    #[test]
    fn clones_are_clean_independent_and_select_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let runner = Runner::new().isolated(temp.path()).unwrap();
        let (source, mut repository) = local_repository(&runner, temp.path());
        repository.url = format!("file://{}", source.display());
        let cache = temp.path().join("cache");
        fetch(&cache, &repository).unwrap();
        let block = temp.path().join("block");
        fs::create_dir(&block).unwrap();
        let repo = block.join("repo");
        crate::monorepo::clone_seed(&runner, &seed(&cache, &repository), &repo).unwrap();
        prepare(&runner, &repo, "local").unwrap();
        assert_eq!(
            runner.git_text(&repo, &["rev-parse", "HEAD"]).unwrap(),
            repository.tip.commit
        );
        assert_eq!(
            runner
                .git_text(&repo, &["rev-parse", "bench-base"])
                .unwrap(),
            repository.base.commit
        );
        assert!(fs::read(repo.join("run.bat")).unwrap().ends_with(b"\r\n"));
        assert!(!repo.join(".git/objects/info/alternates").exists());
        let (edits, deletion) = targets(&runner, &repo).unwrap();
        assert_eq!(edits.len(), 12);
        assert!(!edits.contains(&deletion));
        assert_eq!(targets(&runner, &repo).unwrap(), (edits, deletion));
        fs::create_dir(repo.join("ignored")).unwrap();
        fs::write(repo.join("ignored/residue"), "x").unwrap();
        assert!(runner
            .git(&repo, ["status", "--porcelain"])
            .unwrap()
            .is_empty());
        // The cache is never modified by cloning or preparation.
        verify(&runner, &cache, &repository).unwrap();
    }
}
