mod support;
use groove_core::{AcquireOptions, Pool, PoolOptions};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Stdio},
    time::Duration,
};
use support::Repo;

fn now() -> u128 {
    let mut ts = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: ts is writable storage; CLOCK_MONOTONIC is supported on Linux and macOS.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, ts.as_mut_ptr()) };
    assert_eq!(status, 0);
    // SAFETY: successful clock_gettime initialized ts.
    let ts = unsafe { ts.assume_init() };
    ts.tv_sec as u128 * 1_000_000_000 + ts.tv_nsec as u128
}
fn read_line(stream: &mut BufReader<UnixStream>) -> String {
    let mut s = String::new();
    assert!(stream.read_line(&mut s).unwrap() > 0);
    s.trim_end().into()
}
fn spawn(r: &Repo, socket: &std::path::Path, mode: &str) -> support::TestChild {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "process_helper", "--nocapture"])
        .env("GROOVE_HELPER_REPO", &r.path)
        .env("GROOVE_HELPER_SOCKET", socket)
        .env("GROOVE_HELPER_MODE", mode)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    support::TestChild::spawn(&mut command)
}
#[test]
fn process_helper() {
    let Ok(repo) = std::env::var("GROOVE_HELPER_REPO") else {
        return;
    };
    let socket = std::env::var("GROOVE_HELPER_SOCKET").unwrap();
    let mode = std::env::var("GROOVE_HELPER_MODE").unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    if mode == "worker" {
        stream.write_all(b"worker-ready\n").unwrap();
        let mut b = [0];
        stream.read_exact(&mut b).unwrap();
        return;
    }
    stream.write_all(b"ready\n").unwrap();
    let mut b = [0];
    stream.read_exact(&mut b).unwrap();
    let pool = Pool::open(&repo).unwrap();
    let mut lease = pool
        .acquire(AcquireOptions {
            wait: Duration::from_secs(30),
            ..Default::default()
        })
        .unwrap();
    if mode == "owner_worker" {
        stream.write_all(b"owner-ready\n").unwrap();
        let mut worker = Command::new(std::env::current_exe().unwrap());
        worker
            .args(["--exact", "process_helper", "--nocapture"])
            .env("GROOVE_HELPER_MODE", "worker")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        lease.run_worker(worker).unwrap();
        return;
    }
    let started = now();
    writeln!(stream, "acquired {} {started}", lease.info().slot_id).unwrap();
    stream.read_exact(&mut b).unwrap();
    let ended = now();
    // Drop is also valid release; metadata is deliberately left stale.
    drop(lease);
    writeln!(stream, "released {ended}").unwrap();
}
#[test]
fn many_processes_never_overlap_one_slot_or_exceed_capacity() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 2,
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let socket = r.temp.path().join("concurrency.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(spawn(&r, &socket, "normal"));
    }
    let mut streams = Vec::new();
    for _ in 0..8 {
        let stream =
            support::accept_child(&listener, &mut children, support::PROCESS_TIMEOUT).unwrap();
        let mut reader = BufReader::new(stream);
        assert_eq!(read_line(&mut reader), "ready");
        streams.push(reader);
    }
    let mut threads = Vec::new();
    for mut reader in streams {
        reader.get_mut().write_all(b"g").unwrap();
        threads.push(std::thread::spawn(move || {
            let acquired = read_line(&mut reader);
            reader.get_mut().write_all(b"g").unwrap();
            let released = read_line(&mut reader);
            (acquired, released)
        }));
    }
    let mut intervals: BTreeMap<String, Vec<(u128, u128)>> = BTreeMap::new();
    for thread in threads {
        let (start, end) = thread.join().unwrap();
        let start: Vec<_> = start.split_whitespace().collect();
        let end: Vec<_> = end.split_whitespace().collect();
        intervals
            .entry(start[1].into())
            .or_default()
            .push((start[2].parse().unwrap(), end[1].parse().unwrap()));
    }
    for intervals in intervals.values_mut() {
        intervals.sort_unstable();
        for pair in intervals.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "Overlapping leases: {pair:?}");
        }
    }
    let mut events = Vec::new();
    for intervals in intervals.values() {
        for (start, end) in intervals {
            events.push((*start, 1));
            events.push((*end, -1));
        }
    }
    events.sort_unstable();
    let mut active = 0;
    for (_, delta) in events {
        active += delta;
        assert!((0..=2).contains(&active));
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(intervals.values().map(Vec::len).sum::<usize>(), 8);
}
#[test]
fn sigkill_owner_reclaims_but_surviving_worker_keeps_ownership() {
    for with_worker in [false, true] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(support::worktree_root(&r.path, "default")),
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let socket = r.temp.path().join("kill.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut child = spawn(
            &r,
            &socket,
            if with_worker {
                "owner_worker"
            } else {
                "normal"
            },
        );
        let stream = support::accept_child(
            &listener,
            std::slice::from_mut(&mut child),
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
        let mut stream = BufReader::new(stream);
        assert_eq!(read_line(&mut stream), "ready");
        stream.get_mut().write_all(b"g").unwrap();
        let acquired = read_line(&mut stream);
        let mut worker = if with_worker {
            assert_eq!(acquired, "owner-ready");
            let stream = support::accept_child(
                &listener,
                std::slice::from_mut(&mut child),
                support::PROCESS_TIMEOUT,
            )
            .unwrap();
            let mut stream = BufReader::new(stream);
            assert_eq!(read_line(&mut stream), "worker-ready");
            Some(stream)
        } else {
            assert!(acquired.starts_with("acquired"));
            None
        };
        child.kill().unwrap();
        child.wait().unwrap();
        if let Some(worker) = worker.as_mut() {
            assert_eq!(
                pool.acquire(Default::default()).err().unwrap().code,
                groove_core::ErrorCode::PoolExhausted
            );
            worker.get_mut().write_all(b"g").unwrap();
            let mut end = Vec::new();
            worker.read_to_end(&mut end).unwrap();
        }
        assert!(pool
            .acquire(AcquireOptions {
                wait: Duration::from_secs(5),
                ..Default::default()
            })
            .is_ok());
    }
}
