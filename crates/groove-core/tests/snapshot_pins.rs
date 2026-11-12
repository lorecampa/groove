mod support;

use groove_core::{
    snapshot::{SaveOptions, Snapshots},
    ErrorCode,
};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use support::Repo;

fn service(repo: &Repo) -> Snapshots {
    Snapshots::open(&repo.path, Duration::from_secs(10)).unwrap()
}

#[test]
fn verified_pins_require_force_and_packed_refs_remain_supported() {
    for format in ["sha1", "sha256"] {
        let repo = Repo::format(format);
        let snapshots = service(&repo);
        let a = snapshots.save(SaveOptions::default()).unwrap();
        let b = snapshots.save(SaveOptions::default()).unwrap();
        let reference = format!("refs/groove/snapshots/{}", a.id);
        let pin = format!("refs/groove/snapshot-pins/{}", a.id);
        let oid = repo.git(&["rev-parse", &reference]);
        assert!(!snapshots.show(&a.id).unwrap().pinned);
        repo.git(&["update-ref", &pin, &oid]);
        repo.git(&["pack-refs", "--all"]);
        assert!(snapshots.show(&a.id).unwrap().pinned);
        let listed = snapshots.list().unwrap();
        assert!(listed.iter().find(|s| s.id == a.id).unwrap().pinned);
        assert!(!listed.iter().find(|s| s.id == b.id).unwrap().pinned);
        assert_eq!(
            snapshots.delete(&a.id, false).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(repo.git(&["rev-parse", &reference]), oid);
        assert_eq!(repo.git(&["rev-parse", &pin]), oid);
        snapshots.delete(&a.id, true).unwrap();
        assert!(repo
            .git(&["for-each-ref", "--format=%(refname)", &reference, &pin])
            .is_empty());
        snapshots.delete(&b.id, false).unwrap();
    }
}

#[test]
fn corrupt_or_inconsistent_pins_never_authorize_deletion() {
    for format in ["sha1", "sha256"] {
        for mode in [
            "malformed",
            "missing-object",
            "zero",
            "dangling",
            "symbolic",
            "different-snapshot",
        ] {
            let repo = Repo::format(format);
            let snapshots = service(&repo);
            fs::write(repo.path.join("tracked.txt"), "saved result\n").unwrap();
            let snapshot = snapshots.save(SaveOptions::default()).unwrap();
            let reference = format!("refs/groove/snapshots/{}", snapshot.id);
            let pin = format!("refs/groove/snapshot-pins/{}", snapshot.id);
            let oid = repo.git(&["rev-parse", &reference]);
            let contents = match mode {
                "malformed" => "broken\n".into(),
                "missing-object" => format!("{}1\n", "0".repeat(oid.len() - 1)),
                "zero" => format!("{}\n", "0".repeat(oid.len())),
                "dangling" => "ref: refs/heads/missing-pin-target\n".into(),
                "symbolic" => format!("ref: {reference}\n"),
                "different-snapshot" => {
                    let other = snapshots.save(SaveOptions::default()).unwrap();
                    format!(
                        "{}\n",
                        repo.git(&["rev-parse", &format!("refs/groove/snapshots/{}", other.id)])
                    )
                }
                _ => unreachable!(),
            };
            let path = repo.path.join(".git").join(&pin);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &contents).unwrap();
            assert_eq!(
                snapshots.show(&snapshot.id).unwrap_err().code,
                ErrorCode::GitError,
                "{format}/{mode}"
            );
            assert_eq!(
                snapshots.list().unwrap_err().code,
                ErrorCode::GitError,
                "{format}/{mode}"
            );
            for force in [false, true] {
                assert_eq!(
                    snapshots.delete(&snapshot.id, force).unwrap_err().code,
                    ErrorCode::GitError,
                    "{format}/{mode}/{force}"
                );
                assert_eq!(fs::read_to_string(&path).unwrap(), contents);
                assert_eq!(repo.git(&["rev-parse", &reference]), oid);
                assert_eq!(
                    repo.git(&["show", &format!("{}:tracked.txt", snapshot.work_tree)]),
                    "saved result"
                );
            }
        }
    }
}

#[test]
fn unreadable_pin_is_an_error_and_leaves_both_refs_untouched() {
    // SAFETY: geteuid only returns the caller's effective uid.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root can read mode-000 refs");
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    let snapshot = snapshots.save(Default::default()).unwrap();
    let reference = format!("refs/groove/snapshots/{}", snapshot.id);
    let pin = format!("refs/groove/snapshot-pins/{}", snapshot.id);
    let oid = repo.git(&["rev-parse", &reference]);
    repo.git(&["update-ref", &pin, &oid]);
    let path = repo.path.join(".git").join(&pin);
    struct Readable(std::path::PathBuf);
    impl Drop for Readable {
        fn drop(&mut self) {
            fs::set_permissions(&self.0, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let readable = Readable(path.clone());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        snapshots.show(&snapshot.id).unwrap_err().code,
        ErrorCode::GitError
    );
    assert_eq!(snapshots.list().unwrap_err().code, ErrorCode::GitError);
    assert_eq!(
        snapshots.delete(&snapshot.id, false).unwrap_err().code,
        ErrorCode::GitError
    );
    assert_eq!(
        snapshots.delete(&snapshot.id, true).unwrap_err().code,
        ErrorCode::GitError
    );
    drop(readable);
    assert_eq!(repo.git(&["rev-parse", &pin]), oid);
    assert_eq!(repo.git(&["rev-parse", &reference]), oid);
}

#[test]
fn reftable_pin_absence_and_forced_deletion_use_the_git_backend() {
    let version = support::git(std::path::Path::new("."), &["--version"]);
    let parts: Vec<_> = version
        .trim_start_matches("git version ")
        .split('.')
        .take(2)
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect();
    if parts.as_slice() < [2, 45].as_slice() {
        eprintln!("skipped: reftable needs Git 2.45");
        return;
    }
    let repo = Repo::new();
    fs::remove_dir_all(&repo.path).unwrap();
    fs::create_dir(&repo.path).unwrap();
    repo.git(&["init", "-q", "--ref-format=reftable"]);
    repo.git(&["config", "user.name", "Pin test"]);
    repo.git(&["config", "user.email", "pins@example.invalid"]);
    repo.git(&["commit", "-qm", "baseline", "--allow-empty"]);
    let snapshots = service(&repo);
    let snapshot = snapshots.save(Default::default()).unwrap();
    assert!(!snapshots.show(&snapshot.id).unwrap().pinned);
    let reference = format!("refs/groove/snapshots/{}", snapshot.id);
    let pin = format!("refs/groove/snapshot-pins/{}", snapshot.id);
    let oid = repo.git(&["rev-parse", &reference]);
    repo.git(&["update-ref", &pin, &oid]);
    assert!(snapshots.list().unwrap()[0].pinned);
    assert_eq!(
        snapshots.delete(&snapshot.id, false).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    snapshots.delete(&snapshot.id, true).unwrap();
    assert!(snapshots.list().unwrap().is_empty());
    assert!(repo
        .git(&["for-each-ref", "--format=%(refname)", &reference, &pin])
        .is_empty());
}

#[test]
fn pin_absence_listing_keeps_git_descriptors_bounded() {
    if !support::in_subprocess("pin_absence_listing_keeps_git_descriptors_bounded") {
        return;
    }
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    assert_eq!(
        // SAFETY: limit points to writable rlimit storage.
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) },
        0
    );
    // SAFETY: getrlimit initialized the structure above.
    let mut limit = unsafe { limit.assume_init() };
    limit.rlim_cur = limit.rlim_cur.min(64);
    // SAFETY: a valid rlimit lowers only this isolated process's soft limit.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let repo = Repo::new();
    let snapshots = service(&repo);
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..70 {
        ids.insert(snapshots.save(Default::default()).unwrap().id);
    }
    let listed = snapshots.list().unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|s| s.id.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        ids
    );
    assert!(listed.iter().all(|s| !s.pinned));
}
