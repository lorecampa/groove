mod support;

use std::{fs, os::unix::fs::PermissionsExt, process::Output};
use support::Repo;

fn bounded(repo: &Repo, args: &[&str], restriction: &str) -> Output {
    let bin = repo.temp.path().join("bounded-git");
    fs::create_dir(&bin).unwrap();
    fs::write(
        bin.join("git"),
        r#"#!/bin/sh
case " $* " in
  *' ls-tree -r --name-only '*)
    if [ "$GROOVE_RESTRICTION" = target ]; then
      echo 'unnecessary whole-target collision inventory' >&2; exit 64
    fi ;;
  *' write-tree '*)
    if [ "$GROOVE_RESTRICTION" = duplicate ]; then
      mkdir "$GROOVE_WRITE_MARKER" 2>/dev/null || {
        echo 'unchanged scratch index written twice' >&2; exit 64;
      }
    fi ;;
esac
exec "$GROOVE_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    support::groove_in(&repo.path)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env(
            "GROOVE_REAL_GIT",
            std::env::var_os("GROOVE_TEST_GIT").unwrap(),
        )
        .env("GROOVE_RESTRICTION", restriction)
        .env("GROOVE_WRITE_MARKER", repo.temp.path().join("written"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn restore_without_ignored_files_does_not_inventory_every_target_path() {
    let repo = Repo::new();
    fs::write(repo.path.join("tracked.txt"), "snapshot bytes\n").unwrap();
    let saved = support::assert_success(
        &support::groove_in(&repo.path)
            .args(["snapshot", "save"])
            .output()
            .unwrap(),
    );
    let id = saved["result"]["snapshot"]["id"].as_str().unwrap();
    repo.git(&["checkout", "--", "tracked.txt"]);
    support::assert_success(&bounded(&repo, &["snapshot", "restore", id], "target"));
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"snapshot bytes\n"
    );
    assert!(repo.git(&["diff", "--cached"]).is_empty());
}

#[test]
fn clean_snapshot_reuses_its_unchanged_index_tree() {
    let repo = Repo::new();
    let response = support::assert_success(&bounded(&repo, &["snapshot", "save"], "duplicate"));
    let snapshot = &response["result"]["snapshot"];
    assert_eq!(snapshot["index_tree"], snapshot["work_tree"]);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    assert!(repo.git(&["status", "--porcelain"]).is_empty());
}
