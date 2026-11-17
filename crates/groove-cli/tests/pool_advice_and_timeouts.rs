mod support;

use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    sync::OnceLock,
    time::{Duration, Instant},
};
use support::Repo;

fn check(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    static SCHEMA: OnceLock<jsonschema::Validator> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let schema: Value =
                serde_json::from_str(include_str!("../../../schemas/response-v2.schema.json"))
                    .unwrap();
            jsonschema::validator_for(&schema).unwrap()
        })
        .validate(&value)
        .unwrap();
    assert_eq!(value["ok"], code == 0);
    value
}

fn cli(path: &Path, args: &[&str]) -> Value {
    check(support::groove_in(path).args(args).output().unwrap(), 0)["result"].clone()
}

fn init(repo: &Repo, name: &str, capacity: &str) -> Value {
    check(
        support::groove_in(&repo.path)
            .args([
                "--pool",
                name,
                "pool",
                "init",
                "--capacity",
                capacity,
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
                "--workspace-root",
            ])
            .arg(support::worktree_root(&repo.path, name))
            .output()
            .unwrap(),
        0,
    )["result"]
        .clone()
}

fn slow_git(repo: &Repo) -> Command {
    let wrapper = repo.temp.path().join("wrapper");
    fs::create_dir_all(&wrapper).unwrap();
    let script = wrapper.join("git");
    fs::write(
        &script,
        r#"#!/bin/bash
if [[ -z "${GROOVE_SLOW_WHEN:-}" || "${!GROOVE_SLOW_WHEN:-}" == 1 ]]; then
  case " $* " in *" checkout --detach "*) sleep 3 ;; esac
fi
exec "$GROOVE_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    let real_git = Command::new("bash")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let mut command = support::groove_in(&repo.path);
    command
        .env(
            "PATH",
            format!("{}:{}", wrapper.display(), std::env::var("PATH").unwrap()),
        )
        .env(
            "GROOVE_REAL_GIT",
            String::from_utf8(real_git.stdout).unwrap().trim(),
        )
        .env_remove("GROOVE_SLOW_WHEN");
    command
}

#[test]
fn ready_git_timeout_is_optional_normalized_and_pool_local() {
    let repo = Repo::new();
    assert!(init(&repo, "default", "1")
        .get("ready_git_timeout")
        .is_none());
    assert!(cli(&repo.path, &["pool", "get", "ready-git-timeout"])["value"].is_null());
    let set = cli(&repo.path, &["pool", "set", "--ready-git-timeout", "10m"]);
    assert_eq!(set["updated"], true);
    assert_eq!(set["pool"]["ready_git_timeout"], "10m");
    assert_eq!(
        cli(&repo.path, &["pool", "get", "ready-git-timeout"])["value"],
        "10m"
    );
    assert_eq!(
        cli(&repo.path, &["pool", "show"])["pool"]["ready_git_timeout"],
        "10m"
    );
    assert_eq!(
        cli(&repo.path, &["pool", "set", "--ready-git-timeout", "120s"])["pool"]
            ["ready_git_timeout"],
        "2m"
    );
    for value in ["0s", "5", "-1m", "never"] {
        let error = check(
            support::groove_in(&repo.path)
                .args(["pool", "set", &format!("--ready-git-timeout={value}")])
                .output()
                .unwrap(),
            2,
        );
        assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_eq!(
        cli(&repo.path, &["pool", "set", "--ready-git-timeout", "2m"])["updated"],
        false
    );
    init(&repo, "other", "1");
    assert!(cli(
        &repo.path,
        &["--pool", "other", "pool", "get", "ready-git-timeout"]
    )["value"]
        .is_null());
}

#[test]
fn worker_uses_the_persisted_git_timeout_and_reports_the_budget() {
    let repo = Repo::new();
    init(&repo, "default", "2");
    cli(&repo.path, &["pool", "set", "--ready-git-timeout", "300ms"]);
    check(
        slow_git(&repo)
            .env("GROOVE_SLOW_WHEN", "GROOVE_INTERNAL_READY_WORKER")
            .args(["pool", "set", "--ready-target", "1"])
            .output()
            .unwrap(),
        0,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = loop {
        let shown = cli(&repo.path, &["pool", "show"]);
        let ready = &shown["ready"];
        if ready["running"] == false && ready["attempted_at_unix_ms"].as_u64().unwrap() > 0 {
            break ready.clone();
        }
        assert!(Instant::now() < deadline, "{shown}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(ready["ready"], 0);
    assert!(!ready["failures"].as_object().unwrap().is_empty());
    let list = cli(&repo.path, &["list", "--slots"]);
    for (id, failure) in ready["failures"].as_object().unwrap() {
        let shown = cli(&repo.path, &["show", "--slot", id]);
        assert_eq!(shown["slot"]["status"], "available");
        assert_eq!(shown["slot"]["failure"], *failure);
        let slot = list["inspection"]["slots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|slot| slot["slot_id"] == *id)
            .unwrap();
        assert_eq!(slot["status"], "available");
        assert_eq!(slot["failure"], *failure);
        let failure = failure.as_str().unwrap();
        for expected in ["timed out", "after 300ms", "--ready-git-timeout"] {
            assert!(failure.contains(expected), "{failure}");
        }
    }
    cli(&repo.path, &["pool", "set", "--ready-target", "0"]);
    let acquired = cli(&repo.path, &["acquire"]);
    assert!(cli(
        &repo.path,
        &["show", "--slot", acquired["slot_id"].as_str().unwrap()]
    )["slot"]["failure"]
        .is_null());
}

#[test]
fn a_prepared_spare_clears_a_transient_failure_after_successful_verification() {
    let repo = Repo::new();
    init(&repo, "default", "1");
    let wait = |after| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let shown = cli(&repo.path, &["pool", "show"]);
            if shown["ready"]["running"] == false
                && shown["ready"]["attempted_at_unix_ms"].as_u64().unwrap() > after
            {
                break shown;
            }
            assert!(Instant::now() < deadline, "{shown}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    cli(&repo.path, &["pool", "set", "--ready-target", "1"]);
    let ready = wait(0);
    assert_eq!(ready["ready"]["ready"], 1);
    fs::write(repo.slot(1).join("tracked.txt"), "transient edit\n").unwrap();
    cli(&repo.path, &["pool", "set", "--ready-git-timeout", "2m"]);
    let failed = wait(ready["ready"]["attempted_at_unix_ms"].as_u64().unwrap());
    assert!(!failed["ready"]["failures"].as_object().unwrap().is_empty());
    assert!(failed["inspection"]["slots"][0]["failure"].is_string());
    fs::write(repo.slot(1).join("tracked.txt"), "baseline\n").unwrap();
    cli(&repo.path, &["pool", "set", "--ready-git-timeout", "3m"]);
    let recovered = wait(failed["ready"]["attempted_at_unix_ms"].as_u64().unwrap());
    assert_eq!(recovered["ready"]["ready"], 1);
    assert!(recovered["ready"]["failures"]
        .as_object()
        .unwrap()
        .is_empty());
    assert!(recovered["inspection"]["slots"][0]["failure"].is_null());
}

#[test]
fn foreground_timeout_keeps_the_slot_available_and_retry_clears_failure() {
    let repo = Repo::new();
    init(&repo, "default", "1");
    let error = check(
        slow_git(&repo)
            .args(["--git-timeout", "300ms", "acquire"])
            .output()
            .unwrap(),
        6,
    );
    assert_eq!(error["error"]["code"], "POOL_UNHEALTHY");
    let hint = error["error"]["hint"].as_str().unwrap();
    assert!(
        hint.contains("--git-timeout") && hint.contains("300ms"),
        "{hint}"
    );
    let list = cli(&repo.path, &["list", "--slots"]);
    let slot = &list["inspection"]["slots"][0];
    assert_eq!(slot["status"], "available");
    assert!(slot["failure"].as_str().unwrap().contains("timed out"));
    let record: Value =
        serde_json::from_slice(&fs::read(repo.state().join("slots/wt-0001.json")).unwrap())
            .unwrap();
    assert_eq!(record["phase"], "available");
    assert_eq!(cli(&repo.path, &["acquire"])["slot_id"], "wt-0001");
    assert!(cli(&repo.path, &["show", "--slot", "wt-0001"])["slot"]["failure"].is_null());
}

#[test]
fn copied_release_advice_keeps_the_inferred_pool_selection() {
    let repo = Repo::new();
    init(&repo, "review", "1");
    let acquired = cli(
        &repo.path,
        &["--pool", "review", "acquire", "--task", "edit"],
    );
    let slot = Path::new(acquired["workspace_path"].as_str().unwrap());
    fs::write(slot.join("tracked.txt"), "saved by copied advice\n").unwrap();
    let error = check(
        support::groove_in(slot)
            .arg("--repo")
            .arg(slot)
            .args(["release", "--task", "edit"])
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(error["error"]["code"], "UNSAVED_CHANGES");
    let hint = error["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("--pool review"), "{hint}");
    assert!(!hint.contains("--pool default"), "{hint}");
    let command = hint
        .split('`')
        .find(|part| part.starts_with("groove release --save"))
        .unwrap();
    let words = shell_words::split(command).unwrap();
    let saved = check(
        support::groove_in(slot)
            .args(&words[1..])
            .args(["--task", "edit"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(saved["result"]["snapshot"]["pinned"], true);
}

#[test]
fn destruction_accepts_saved_release_files_but_refuses_later_edits_and_ignored_files() {
    let repo = Repo::new();
    init(&repo, "default", "1");
    let acquired = cli(&repo.path, &["acquire", "--task", "work"]);
    let slot = Path::new(acquired["workspace_path"].as_str().unwrap());
    fs::write(slot.join("tracked.txt"), "saved edit\n").unwrap();
    fs::write(slot.join("new.txt"), "saved staged file\n").unwrap();
    support::git(slot, &["add", "new.txt"]);
    let saved = cli(&repo.path, &["release", "--save", "--task", "work"]);
    assert_eq!(saved["snapshot"]["pinned"], true);
    cli(&repo.path, &["pool", "destroy", "--dry-run"]);
    fs::write(slot.join("tracked.txt"), "later edit\n").unwrap();
    let error = check(
        support::groove_in(&repo.path)
            .args(["pool", "destroy", "--dry-run"])
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(error["error"]["code"], "UNSAVED_CHANGES");
    assert_eq!(
        fs::read_to_string(slot.join("tracked.txt")).unwrap(),
        "later edit\n"
    );
    fs::write(slot.join("tracked.txt"), "saved edit\n").unwrap();
    fs::create_dir(slot.join("ignored")).unwrap();
    fs::write(slot.join("ignored/cache"), "excluded\n").unwrap();
    let error = check(
        support::groove_in(&repo.path)
            .args(["pool", "destroy", "--dry-run"])
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(error["error"]["code"], "UNSAVED_CHANGES");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ignored"));
    assert_eq!(
        fs::read_to_string(slot.join("ignored/cache")).unwrap(),
        "excluded\n"
    );
    fs::remove_dir_all(slot.join("ignored")).unwrap();
    assert_eq!(
        cli(&repo.path, &["pool", "destroy"])["destruction"]["deleted"],
        true
    );
    assert!(!slot.exists());
    let recovered = repo.temp.path().join("recovered");
    check(
        support::groove_in(&repo.path)
            .args([
                "snapshot",
                "restore",
                saved["snapshot"]["id"].as_str().unwrap(),
                "--to",
            ])
            .arg(&recovered)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        fs::read_to_string(recovered.join("tracked.txt")).unwrap(),
        "saved edit\n"
    );
    assert_eq!(
        fs::read_to_string(recovered.join("new.txt")).unwrap(),
        "saved staged file\n"
    );
}

#[test]
fn empty_worktree_configuration_is_compatible_with_init_and_acquire() {
    let repo = Repo::new();
    support::git(&repo.path, &["config", "extensions.worktreeConfig", "true"]);
    fs::write(repo.path.join(".git/config.worktree"), "").unwrap();
    init(&repo, "default", "1");
    assert_eq!(cli(&repo.path, &["acquire"])["slot_id"], "wt-0001");
}

#[test]
fn incompatible_worktree_configuration_names_its_key_and_origin_before_init() {
    let repo = Repo::new();
    support::git(&repo.path, &["config", "extensions.worktreeConfig", "true"]);
    support::git(
        &repo.path,
        &["config", "--worktree", "core.sparseCheckout", "true"],
    );
    let error = check(
        support::groove_in(&repo.path)
            .args(["pool", "init", "--capacity", "1"])
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(error["error"]["code"], "UNSUPPORTED_CONFIGURATION");
    assert_eq!(
        error["error"]["details"]["setting"]["key"],
        "core.sparsecheckout"
    );
    assert_eq!(error["error"]["details"]["setting"]["value"], "true");
    assert!(error["error"]["details"]["setting"]["origin"]
        .as_str()
        .unwrap()
        .ends_with("config.worktree"));
    assert!(!repo.state().join("config.json").exists());
}

#[test]
fn configured_filters_name_their_origin_and_remain_rejected_after_init() {
    let repo = Repo::new();
    support::git(
        &repo.path,
        &["config", "filter.lfs.clean", "untrusted-filter"],
    );
    let error = check(
        support::groove_in(&repo.path)
            .args(["pool", "init", "--capacity", "1"])
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(error["error"]["code"], "UNSUPPORTED_CONFIGURATION");
    let setting = &error["error"]["details"]["setting"];
    assert_eq!(setting["key"], "filter.lfs.clean");
    assert_eq!(setting["value"], "untrusted-filter");
    assert!(setting["origin"]
        .as_str()
        .unwrap()
        .contains(repo.path.join(".git/config").to_str().unwrap()));
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains(setting["origin"].as_str().unwrap()));
    assert!(!repo.state().join("config.json").exists());
    support::git(&repo.path, &["config", "--unset", "filter.lfs.clean"]);
    init(&repo, "default", "1");
    support::git(
        &repo.path,
        &["config", "filter.lfs.clean", "untrusted-filter"],
    );
    let error = check(
        support::groove_in(&repo.path)
            .arg("acquire")
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(
        error["error"]["details"]["setting"]["key"],
        "filter.lfs.clean"
    );
    assert!(!repo.slot(1).exists());
}

#[test]
fn slow_checkout_progress_reports_the_current_phase() {
    let repo = Repo::new();
    init(&repo, "default", "1");
    let output = slow_git(&repo).arg("acquire").output().unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/progress-v2.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let events: Vec<Value> = std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .map(|line| {
            let event: Value = serde_json::from_str(line).unwrap();
            validator.validate(&event).unwrap();
            event
        })
        .collect();
    check(output, 0);
    assert!(
        events
            .iter()
            .any(|event| event["event"] == "progress" && event["phase"] == "checking out"),
        "{events:?}"
    );
}

#[test]
fn detailed_inspection_traces_the_git_program_and_only_the_directory_basename() {
    let repo = Repo::new();
    init(&repo, "default", "1");
    cli(&repo.path, &["acquire"]);
    cli(&repo.path, &["config", "set", "logging.level=trace"]);
    let logs = repo.temp.path().join("trace-logs");
    check(
        support::groove_in(&repo.path)
            .env("GROOVE_LOG_ENABLED", "true")
            .env("GROOVE_LOG_DIR", &logs)
            .args(["list", "--details"])
            .output()
            .unwrap(),
        0,
    );
    let events: Vec<Value> = fs::read_dir(logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .flat_map(|path| {
            fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect();
    assert!(
        events.iter().any(|event| event["event"] == "git.completed"
            && event["fields"]["program"] == "status"
            && event["fields"]["cwd"] == "wt-0001"),
        "{events:?}"
    );
}

#[test]
fn destruction_repairs_nested_read_only_directories() {
    let repo = Repo::new();
    init(&repo, "default", "1");
    let acquired = cli(&repo.path, &["acquire", "--task", "readonly"]);
    let slot = Path::new(acquired["workspace_path"].as_str().unwrap());
    let nested = slot.join("ignored/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("cache"), "disposable\n").unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o500)).unwrap();
    fs::set_permissions(slot.join("ignored"), fs::Permissions::from_mode(0o500)).unwrap();
    cli(&repo.path, &["release", "--task", "readonly", "--force"]);
    assert_eq!(
        cli(&repo.path, &["pool", "destroy", "--force"])["destruction"]["deleted"],
        true
    );
    assert!(!slot.exists());
}

#[test]
fn destruction_removes_all_three_checkouts_and_exact_registrations() {
    let repo = Repo::new();
    init(&repo, "default", "3");
    let mut paths = Vec::new();
    for task in ["one", "two", "three"] {
        let acquired = cli(&repo.path, &["acquire", "--task", task]);
        let slot = Path::new(acquired["workspace_path"].as_str().unwrap()).to_owned();
        let admin = support::git(&slot, &["rev-parse", "--absolute-git-dir"]);
        paths.push((slot, std::path::PathBuf::from(admin.trim())));
    }
    for task in ["one", "two", "three"] {
        cli(&repo.path, &["release", "--task", task]);
    }
    let result = cli(&repo.path, &["pool", "destroy"]);
    assert_eq!(result["destruction"]["deleted"], true);
    assert_eq!(result["destruction"]["slots"].as_array().unwrap().len(), 3);
    for (slot, admin) in paths {
        assert!(!slot.exists(), "checkout remained: {}", slot.display());
        assert!(
            !admin.exists(),
            "registration remained: {}",
            admin.display()
        );
    }
}
