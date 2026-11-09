mod support;
use groove_core::{ErrorCode, LeaseId, LeaseInfo, Pool};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
use support::Repo;

struct Server {
    child: Child,
    input: Option<ChildStdin>,
    responses: Receiver<Value>,
    next_id: u64,
}
impl Server {
    fn start(repo: &Repo) -> Self {
        Self::command(repo, |_| {})
    }
    fn command(repo: &Repo, configure: impl FnOnce(&mut Command)) -> Self {
        let binary = std::env::var_os("GROOVE_MCP_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_groove-mcp").into());
        let mut command = Command::new(binary);
        command
            .arg("--repo")
            .arg(&repo.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        configure(&mut command);
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value: Value = serde_json::from_str(&line.unwrap())
                    .expect("stdout must contain only MCP JSON");
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            input,
            responses,
            next_id: 1,
        };
        let response = server.rpc("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"groove-test","version":"1"}}));
        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(response["result"]["serverInfo"]["name"], "groove-mcp");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        server.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        server
    }
    fn send(&mut self, message: Value) {
        let input = self.input.as_mut().unwrap();
        serde_json::to_writer(&mut *input, &message).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
    }
    fn begin(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        id
    }
    fn receive(&self, id: u64) -> Value {
        let start = Instant::now();
        loop {
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(15).saturating_sub(start.elapsed()))
                .expect("bounded MCP response");
            if response["id"] == id {
                return response;
            }
        }
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.begin(method, params);
        self.receive(id)
    }
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.rpc("tools/call", json!({"name":name,"arguments":arguments}));
        assert!(response.get("error").is_none(), "{response}");
        let result = &response["result"];
        let structured = result["structuredContent"].clone();
        assert!(structured.is_object(), "{response}");
        assert_eq!(
            result["isError"].as_bool().unwrap_or(false),
            structured["ok"] == false
        );
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, structured);
        structured
    }
    fn init(&mut self, capacity: usize) {
        let result = self.tool("groove_init", json!({"capacity":capacity}));
        assert_eq!(result["ok"], true, "{result}");
    }
    fn acquire(&mut self, key: &str) -> LeaseInfo {
        let result = self.tool("groove_acquire", json!({"base":"HEAD","request_key":key}));
        assert_eq!(result["ok"], true, "{result}");
        serde_json::from_value(result["result"]["lease"].clone()).unwrap()
    }
    fn release(&mut self, info: &LeaseInfo, stopped: bool) -> Value {
        self.tool(
            "groove_release",
            json!({"slot_id":info.slot_id,"lease_id":info.lease_id,"writers_stopped":stopped}),
        )
    }
    fn stop(&mut self, kill: bool) {
        if kill {
            self.child.kill().unwrap();
        } else {
            self.input.take();
        }
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                if !kill {
                    assert!(status.success(), "{status}");
                }
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "server did not terminate"
            );
            std::thread::yield_now();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn discovery_protocol_validation_and_repository_scope() {
    let repo = Repo::new();
    let mut server = Server::start(&repo);
    let response = server.rpc("tools/list", json!({}));
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    for name in [
        "groove_init",
        "groove_acquire",
        "groove_leases",
        "groove_release",
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        jsonschema::validator_for(&tool["inputSchema"]).unwrap();
    }
    assert_eq!(server.rpc("ping", json!({}))["result"], json!({}));
    assert!(
        server.rpc("tools/call", json!({"name":"unknown","arguments":{}}))["error"].is_object()
    );
    let invalid = server.rpc(
        "tools/call",
        json!({"name":"groove_init","arguments":{"capacity":1,"repo":"/elsewhere"}}),
    );
    assert!(
        invalid["error"].is_object() || invalid["result"]["isError"] == true,
        "{invalid}"
    );
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::NotInitialized
    );
    assert_eq!(
        server.tool("groove_acquire", json!({"base":"HEAD","request_key":""}))["error"]["code"],
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        server.tool(
            "groove_acquire",
            json!({"base":"HEAD","request_key":"not-initialized"})
        )["error"]["code"],
        "NOT_INITIALIZED"
    );
    server.init(1);
    assert_eq!(
        server.tool("groove_init", json!({"capacity":1}))["result"]["created"],
        false
    );
    assert_eq!(
        server.tool("groove_init", json!({"capacity":2}))["error"]["code"],
        "CONFIG_CONFLICT"
    );
    assert_eq!(
        server.tool(
            "groove_acquire",
            json!({"base":"missing-revision","request_key":"invalid-base"})
        )["error"]["code"],
        "INVALID_REVISION"
    );
}

#[test]
fn lease_survives_tool_calls_retry_and_release_never_unlocks_successor() {
    let repo = Repo::new();
    let mut server = Server::start(&repo);
    server.init(1);
    let info = server.acquire("task-1");
    let pool = Pool::open(&repo.path).unwrap();
    assert_eq!(server.acquire("task-1").lease_id, info.lease_id);
    assert_eq!(
        server.tool(
            "groove_acquire",
            json!({"base":"HEAD~1","request_key":"task-1"})
        )["error"]["code"],
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    assert_eq!(
        server.tool(
            "groove_acquire",
            json!({"base":"HEAD","request_key":"task-2"})
        )["error"]["code"],
        "POOL_EXHAUSTED"
    );
    std::fs::write(info.worktree_path.join("tracked.txt"), "task result").unwrap();
    assert_eq!(
        server.release(&info, false)["error"]["code"],
        "INVALID_ARGUMENT"
    );
    let list = server.tool("groove_leases", json!({}));
    assert_eq!(list["result"]["leases"][0]["owned_by_this_server"], true);
    assert_eq!(list["result"]["leases"][0]["request_key"], "task-1");
    assert_eq!(server.release(&info, true)["ok"], true);
    assert!(pool.protected_leases().unwrap().is_empty());
    let successor = server.acquire("task-2");
    assert_ne!(successor.lease_id, info.lease_id);
    assert_eq!(
        std::fs::read_to_string(info.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert_eq!(server.release(&info, true)["ok"], true); // Lost-response retry has no side effects.
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    assert_eq!(
        server.tool(
            "groove_acquire",
            json!({"base":"HEAD","request_key":"task-1"})
        )["error"]["code"],
        "INVALID_LEASE"
    );
    assert_eq!(server.release(&successor, true)["ok"], true);
}

#[test]
fn disconnect_and_sigkill_preserve_work_until_explicit_recovery() {
    for kill in [false, true] {
        let repo = Repo::new();
        let mut owner = Server::start(&repo);
        owner.init(1);
        let info = owner.acquire("task");
        let pool = Pool::open(&repo.path).unwrap();
        let mut other = Server::start(&repo);
        assert_eq!(other.release(&info, true)["error"]["code"], "INVALID_LEASE");
        // A worker started outside MCP does not inherit its descriptor. It keeps
        // writing after the server dies, exactly like an agent's separate shell tool.
        let mut command = Command::new("sh");
        command
            .current_dir(&info.worktree_path)
            .args(["-c", "read value; printf '%s' \"$value\" > tracked.txt"])
            .stdin(Stdio::piped());
        let mut worker = support::TestChild::spawn(&mut command);
        owner.stop(kill);
        assert_eq!(
            pool.acquire(Default::default()).err().unwrap().code,
            ErrorCode::PoolUnhealthy
        );
        worker
            .stdin
            .take()
            .unwrap()
            .write_all(b"surviving writer\n")
            .unwrap();
        assert!(worker.wait_with_output().unwrap().status.success());
        assert_eq!(
            std::fs::read_to_string(info.worktree_path.join("tracked.txt")).unwrap(),
            "surviving writer"
        );
        let list = other.tool("groove_leases", json!({}));
        assert_eq!(list["result"]["leases"][0]["owned_by_this_server"], false);
        let mut wrong = info.clone();
        wrong.lease_id = LeaseId::generate();
        assert_eq!(
            other.release(&wrong, true)["error"]["code"],
            "LEASE_MISMATCH"
        );
        assert_eq!(
            other.release(&info, false)["error"]["code"],
            "INVALID_ARGUMENT"
        );
        assert_eq!(other.release(&info, true)["ok"], true);
        let next = pool.acquire(Default::default()).unwrap();
        assert_ne!(next.info().lease_id, info.lease_id);
        assert_eq!(
            std::fs::read_to_string(info.worktree_path.join("tracked.txt")).unwrap(),
            "baseline\n"
        );
    }
}

#[cfg(feature = "test-hooks")]
#[test]
fn cancelled_acquire_remains_discoverable_and_protected() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    let socket = repo.temp.path().join("checkpoint.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut server = Server::command(&repo, |command| {
        command
            .env("GROOVE_TEST_CHECKPOINT", "lease-recorded")
            .env("GROOVE_TEST_SOCKET", &socket);
    });
    server.init(1);
    let request = server.begin(
        "tools/call",
        json!({"name":"groove_acquire","arguments":{"base":"HEAD","request_key":"cancelled-task"}}),
    );
    let start = Instant::now();
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(start.elapsed() < Duration::from_secs(15));
                std::thread::yield_now();
            }
            Err(error) => panic!("{error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut checkpoint = [0; 14];
    stream.read_exact(&mut checkpoint).unwrap();
    assert_eq!(&checkpoint, b"lease-recorded");
    server.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":request,"reason":"test cancellation"}}));
    stream.write_all(b"x").unwrap();
    let list = server.tool("groove_leases", json!({}));
    assert_eq!(list["result"]["leases"].as_array().unwrap().len(), 1);
    let info: LeaseInfo =
        serde_json::from_value(list["result"]["leases"][0]["lease"].clone()).unwrap();
    assert_eq!(server.acquire("cancelled-task").lease_id, info.lease_id);
    assert_eq!(server.release(&info, true)["ok"], true);
}

/// Opt-in test against a real local repository, always cloned into disposable storage.
#[test]
#[ignore = "set GROOVE_TEST_LOCAL_REPO and run with --ignored to test a real repository clone"]
fn real_local_repository_workspace_lifecycle() {
    use std::path::PathBuf;
    let source = PathBuf::from(
        std::env::var_os("GROOVE_TEST_LOCAL_REPO").expect("set GROOVE_TEST_LOCAL_REPO"),
    );
    let source = std::fs::canonicalize(source).unwrap();
    let source_status = || {
        let output = support::command(&source, "git")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args([
                "-c",
                "core.fsmonitor=false",
                "status",
                "--porcelain=v1",
                "-z",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    };
    let original_status = source_status();
    let original_pool_existed = source.join(".git/groove").exists();
    let original_refs = support::git(
        &source,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let original_diff = support::git(
        &source,
        &["diff", "--binary", "--no-ext-diff", "--no-textconv", "HEAD"],
    );
    let original_head = support::git(&source, &["rev-parse", "HEAD"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("real repository clone");
    let output = support::command(temp.path(), "git")
        .args(["clone", "--local", "--no-hardlinks", "--no-checkout", "--"])
        .arg(&source)
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "local clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo = Repo { temp, path };
    repo.git(&["config", "user.name", "Groove Local Test"]);
    repo.git(&["config", "user.email", "groove-test@example.invalid"]);
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), original_head);
    let mut server = Server::start(&repo);
    server.init(2);
    let a = server.acquire("real-task-a");
    let b = server.acquire("real-task-b");
    assert_ne!(a.slot_id, b.slot_id);
    assert_ne!(a.worktree_path, b.worktree_path);
    for info in [&a, &b] {
        assert_eq!(info.base_oid.as_str(), original_head);
        assert_eq!(
            support::git(&info.worktree_path, &["rev-parse", "HEAD"]),
            original_head
        );
        assert!(support::git(&info.worktree_path, &["status", "--porcelain"]).is_empty());
    }
    assert_eq!(
        server.tool(
            "groove_acquire",
            json!({"base":"HEAD","request_key":"overflow"})
        )["error"]["code"],
        "POOL_EXHAUSTED"
    );
    let files = support::git(&a.worktree_path, &["ls-tree", "-r", "-z", "HEAD"]);
    let tracked = files
        .split('\0')
        .filter_map(|entry| {
            let (metadata, name) = entry.split_once('\t')?;
            if !metadata.starts_with("100644 ") && !metadata.starts_with("100755 ") {
                return None;
            }
            let path = a.worktree_path.join(name);
            (std::fs::metadata(path).ok()?.len() < 1024 * 1024).then(|| PathBuf::from(name))
        })
        .next()
        .expect("repository needs a regular file smaller than 1 MiB");
    let original = std::fs::read(a.worktree_path.join(&tracked)).unwrap();
    let mut changed = original.clone();
    changed.extend_from_slice(b"\nGroove local MCP lifecycle test\n");
    std::fs::write(a.worktree_path.join(&tracked), &changed).unwrap();
    let artifact = format!(".groove-local-test-{}", a.lease_id);
    assert!(!a.worktree_path.join(&artifact).exists());
    std::fs::write(a.worktree_path.join(&artifact), "disposable artifact").unwrap();
    let branch = format!("groove-local-test/{}", a.lease_id);
    support::git(&a.worktree_path, &["checkout", "-b", &branch]);
    let tracked = tracked.to_str().expect("test selects a UTF-8 filename");
    support::git(&a.worktree_path, &["add", "--", tracked]);
    support::git(
        &a.worktree_path,
        &["commit", "-qm", "Preserve disposable MCP test result"],
    );
    let result_commit = support::git(&a.worktree_path, &["rev-parse", "HEAD"]);
    assert_ne!(result_commit, original_head);
    assert_eq!(
        std::fs::read(b.worktree_path.join(tracked)).unwrap(),
        original
    );
    assert_eq!(server.release(&a, true)["ok"], true);
    assert_eq!(server.release(&b, true)["ok"], true);
    let c = server.acquire("reused-task-c");
    let d = server.acquire("reused-task-d");
    assert_eq!(
        std::fs::read(a.worktree_path.join(tracked)).unwrap(),
        original
    );
    assert!(!a.worktree_path.join(&artifact).exists());
    assert_eq!(repo.git(&["rev-parse", &branch]), result_commit);
    let abandoned = c
        .worktree_path
        .join(format!(".groove-abandoned-{}", c.lease_id));
    std::fs::write(&abandoned, "preserve across MCP crash").unwrap();
    server.stop(true);
    let pool = Pool::open(&repo.path).unwrap();
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolUnhealthy
    );
    assert_eq!(
        std::fs::read_to_string(&abandoned).unwrap(),
        "preserve across MCP crash"
    );
    let mut restarted = Server::start(&repo);
    let list = restarted.tool("groove_leases", json!({}));
    assert_eq!(list["result"]["leases"].as_array().unwrap().len(), 2);
    for info in [&c, &d] {
        assert_eq!(restarted.release(info, true)["ok"], true);
    }
    let e = restarted.acquire("after-restart-e");
    let f = restarted.acquire("after-restart-f");
    assert!(!abandoned.exists());
    assert_eq!(restarted.release(&e, true)["ok"], true);
    assert_eq!(restarted.release(&f, true)["ok"], true);
    restarted.stop(false);
    assert_eq!(
        source_status(),
        original_status,
        "source status changed during local test"
    );
    assert!(
        support::git(
            &source,
            &["diff", "--binary", "--no-ext-diff", "--no-textconv", "HEAD"]
        ) == original_diff,
        "source tracked content changed during local test"
    );
    assert_eq!(support::git(&source, &["rev-parse", "HEAD"]), original_head);
    assert_eq!(
        source.join(".git/groove").exists(),
        original_pool_existed,
        "source pool state changed during local test"
    );
    assert_eq!(
        support::git(
            &source,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        original_refs,
        "source refs changed during local test"
    );
    eprintln!("Local repository smoke passed: two slots, capacity, independent edits, retained result branch, cleanup, crash protection and restart recovery");
}
