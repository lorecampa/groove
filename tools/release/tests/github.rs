//! Exercise the release helper against a local GitHub CLI peer; never contact GitHub.
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};
use tempfile::TempDir;

struct Github {
    root: TempDir,
}

impl Github {
    fn new() -> Self {
        let f = Self {
            root: tempfile::tempdir().unwrap(),
        };
        for dir in ["bin", "assets", "remote"] {
            fs::create_dir(f.path(dir)).unwrap();
        }
        fs::write(f.path("notes"), "Reviewed notes\n").unwrap();
        fs::write(f.path("assets/archive.tar.gz"), "release bytes").unwrap();
        fs::write(f.path("assets/SHA256SUMS"), "checksums").unwrap();
        let peer = f.path("bin/gh");
        fs::write(
            &peer,
            r#"#!/usr/bin/env bash
set -euo pipefail
cd "$RELEASE_PEER"
printf '%s\n' "$*" >> calls
if [[ $1 == api ]]; then
  [[ ! -f auth-failure ]] || exit 1
  if [[ ! -f draft ]]; then printf '[[]]\n'; exit 0; fi
  count=0
  for path in remote/*; do [[ ! -f $path ]] || count=$((count + 1)); done
  jq -n --argjson draft "$(cat draft)" --argjson count "$count" \
    '[[{tag_name:"v0.1.0",draft:$draft,assets:[range($count) | {}]}]]'
  exit 0
fi
case $2 in
  create) printf 'true' > draft ;;
  upload) [[ ! -f upload-failure ]] || exit 1; cp -- "$4" "remote/${4##*/}" ;;
  download)
    while [[ $1 != --dir ]]; do shift; done
    for path in remote/*; do [[ ! -f $path ]] || cp -- "$path" "$2/"; done
    if [[ -f corrupt-download ]]; then printf 'corrupt' > "$2/archive.tar.gz"; fi
    ;;
  edit)
    for arg in "$@"; do
      if [[ $arg == --draft=false ]]; then printf 'false' > draft; fi
    done
    ;;
  *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(peer, fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn run(&self, publish: bool, success: bool) -> String {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/github-release.sh");
        let mut command = Command::new("bash");
        command
            .arg(script)
            .args(["owner/repo", "v0.1.0"])
            .arg(self.path("assets"))
            .arg(self.path("notes"))
            .arg("--execute")
            .env("RELEASE_PEER", self.root.path())
            .env(
                "PATH",
                std::env::join_paths(
                    [self.path("bin")]
                        .into_iter()
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            );
        if publish {
            command.arg("--publish");
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn assert_assets_preserved(&self) {
        assert_eq!(
            fs::read(self.path("remote/archive.tar.gz")).unwrap(),
            b"release bytes"
        );
        assert_eq!(
            fs::read(self.path("remote/SHA256SUMS")).unwrap(),
            b"checksums"
        );
    }
}

#[test]
fn rehearsal_leaves_a_verified_draft_and_publish_retry_does_not_reupload() {
    let f = Github::new();
    f.run(false, true);
    assert_eq!(fs::read_to_string(f.path("draft")).unwrap(), "true");
    f.assert_assets_preserved();
    fs::write(f.path("calls"), "").unwrap();
    f.run(true, true);
    assert_eq!(fs::read_to_string(f.path("draft")).unwrap(), "false");
    let calls = fs::read_to_string(f.path("calls")).unwrap();
    assert!(!calls.contains("release upload"));
    fs::write(f.path("calls"), "").unwrap();
    f.run(true, true);
    let calls = fs::read_to_string(f.path("calls")).unwrap();
    assert!(!calls.contains("release edit"));
    assert!(!calls.contains("release upload"));
    f.assert_assets_preserved();
}

#[test]
fn partial_draft_resumes_only_missing_assets() {
    let f = Github::new();
    fs::write(f.path("draft"), "true").unwrap();
    fs::copy(
        f.path("assets/archive.tar.gz"),
        f.path("remote/archive.tar.gz"),
    )
    .unwrap();
    f.run(true, true);
    f.assert_assets_preserved();
    let calls = fs::read_to_string(f.path("calls")).unwrap();
    let uploads: Vec<_> = calls
        .lines()
        .filter(|l| l.starts_with("release upload"))
        .collect();
    assert_eq!(uploads.len(), 1);
    assert!(uploads[0].contains("SHA256SUMS"));
}

#[test]
fn mismatched_or_incomplete_published_assets_are_never_replaced() {
    for draft in ["true", "false"] {
        let f = Github::new();
        f.run(false, true);
        fs::write(f.path("draft"), draft).unwrap();
        fs::write(f.path("remote/archive.tar.gz"), "original different bytes").unwrap();
        assert!(f.run(true, false).contains("Existing asset differs"));
        assert_eq!(
            fs::read_to_string(f.path("remote/archive.tar.gz")).unwrap(),
            "original different bytes"
        );
    }
    let f = Github::new();
    fs::write(f.path("draft"), "false").unwrap();
    assert!(f
        .run(true, false)
        .contains("Published release is incomplete"));
    assert!(!f.path("remote/archive.tar.gz").exists());
}

#[test]
fn authentication_failure_is_not_interpreted_as_an_absent_release() {
    let f = Github::new();
    fs::write(f.path("auth-failure"), "").unwrap();
    f.run(true, false);
    assert!(!f.path("draft").exists());
}

#[test]
fn failed_upload_or_corrupt_download_cannot_publish_a_draft() {
    for failure in ["upload-failure", "corrupt-download"] {
        let f = Github::new();
        fs::write(f.path(failure), "").unwrap();
        f.run(true, false);
        assert_eq!(fs::read_to_string(f.path("draft")).unwrap(), "true");
        let calls = fs::read_to_string(f.path("calls")).unwrap();
        assert!(!calls.contains("--draft=false"));
    }
}

#[test]
fn unexpected_remote_assets_stop_before_publication() {
    let f = Github::new();
    f.run(false, true);
    fs::write(f.path("remote/unreviewed"), "keep").unwrap();
    assert!(f.run(true, false).contains("Unexpected existing asset"));
    assert_eq!(fs::read_to_string(f.path("draft")).unwrap(), "true");
    assert_eq!(
        fs::read_to_string(f.path("remote/unreviewed")).unwrap(),
        "keep"
    );
}

#[test]
fn github_mutation_requires_explicit_execute() {
    let f = Github::new();
    let output = Command::new("bash")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/github-release.sh"))
        .args(["owner/repo", "v0.1.0"])
        .arg(f.path("assets"))
        .arg(f.path("notes"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--execute"));
    assert!(!f.path("calls").exists());
}
