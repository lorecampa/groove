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
