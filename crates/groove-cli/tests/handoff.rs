mod support;
use groove_core::{transport, AcquireOptions, LeaseInfo, Pool, PoolOptions};
use std::{
    fs::OpenOptions,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    process::{Command, Stdio},
    time::Duration,
};
use support::Repo;

fn acquire(r: &Repo) -> (serde_json::Value, OwnedFd) {
    let (receiver, sender) = transport::channel().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_groove"));
    c.arg("--repo")
        .arg(&r.path)
        .args([
            "--request-id",
            "handoff",
            "acquire",
            "--base",
            "HEAD",
            "--lease-channel-fd",
        ])
        .arg(sender.as_raw_fd().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: sender is alive through spawn and c is not reused.
    unsafe {
        transport::inherit_fd(&mut c, sender.as_fd());
    }
    let child = support::TestChild::spawn(&mut c);
    drop(sender);
    let (record, fd) = transport::receive_record(receiver.as_fd(), Some("handoff")).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(out.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        record
    );
    let schema = serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema)
        .unwrap()
        .is_valid(&record));
    // SAFETY: fcntl reads flags of a live owned descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    (record, fd)
}
#[test]
fn transfer_survives_cli_exit_and_release_rejects_stale_descriptor() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let (record, fd) = acquire(&r);
    let info: LeaseInfo = serde_json::from_value(record["result"].clone()).unwrap();
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::PoolExhausted
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_groove"));
    c.arg("--repo")
        .arg(&r.path)
        .args(["release", "--slot"])
        .arg(info.slot_id.as_str())
        .arg("--lease-id")
        .arg(info.lease_id.to_string())
        .arg("--lease-fd")
        .arg(fd.as_raw_fd().to_string());
    // SAFETY: fd remains live through output; c is used only here.
    unsafe {
        transport::inherit_fd(&mut c, fd.as_fd());
    }
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["result"]["released"],
        true
    );
    let next = pool.acquire(Default::default()).unwrap();
    assert!(pool
        .release_fd(&info.slot_id, info.lease_id, fd.as_fd())
        .is_err());
    assert!(pool.acquire(Default::default()).is_err());
    drop(next);
}
#[test]
fn abandoned_queued_handoff_releases_kernel_reference() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let (receiver, sender) = transport::channel().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_groove"));
    c.arg("--repo")
        .arg(&r.path)
        .args(["acquire", "--base", "HEAD", "--lease-channel-fd"])
        .arg(sender.as_raw_fd().to_string());
    // SAFETY: sender remains live throughout output; c is not reused.
    unsafe {
        transport::inherit_fd(&mut c, sender.as_fd());
    }
    let out = c.output().unwrap();
    drop(sender);
    assert!(out.status.success());
    assert!(pool.acquire(Default::default()).is_err());
    drop(receiver);
    assert!(pool.acquire(Default::default()).is_ok());
}
#[test]
fn failed_send_and_malformed_receipt_do_not_leak_ownership() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let (receiver, sender) = transport::channel().unwrap();
    let (info, fd) = pool
        .acquire(Default::default())
        .unwrap()
        .into_descriptor()
        .unwrap();
    transport::send_record(
        sender.as_fd(),
        &serde_json::json!({"api_version":99,"ok":true,"request_id":null,"result":info}),
        fd.as_fd(),
    )
    .unwrap();
    drop(fd);
    assert!(transport::receive_record(receiver.as_fd(), None).is_err());
    assert!(pool.acquire(Default::default()).is_ok());
    drop(receiver);
    let (_, fd) = pool
        .acquire(Default::default())
        .unwrap()
        .into_descriptor()
        .unwrap();
    assert!(transport::send_record(sender.as_fd(), &serde_json::json!({}), fd.as_fd()).is_err());
    drop(fd);
    assert!(pool.acquire(Default::default()).is_ok());
}
#[test]
fn socket_type_validation_and_wait_timeout() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let file = OpenOptions::new()
        .read(true)
        .open(r.path.join("tracked.txt"))
        .unwrap();
    assert!(transport::validate_channel(file.as_fd()).is_err());
    let pool = Pool::open(&r.path).unwrap();
    let _lease = pool.acquire(Default::default()).unwrap();
    assert_eq!(
        pool.acquire(AcquireOptions {
            wait: Duration::from_millis(1),
            ..Default::default()
        })
        .err()
        .unwrap()
        .code,
        groove_core::ErrorCode::AcquireTimeout
    );
}

fn send_raw(socket: std::os::fd::BorrowedFd<'_>, body: &[u8], fds: &[i32]) {
    let mut control = [0usize; 256];
    let mut iov = libc::iovec {
        iov_base: body.as_ptr() as *mut _,
        iov_len: body.len(),
    };
    // SAFETY: msghdr is initialized before the syscall.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        msg.msg_control = control.as_mut_ptr().cast();
        // SAFETY: aligned control has sufficient space for the supplied descriptor array.
        unsafe {
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) as usize;
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as usize;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(c).cast::<i32>(),
                fds.len(),
            );
        }
    }
    // SAFETY: message buffers remain live during the synchronous syscall.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
    assert_eq!(
        sent,
        body.len() as isize,
        "{}",
        std::io::Error::last_os_error()
    );
}
#[test]
fn malformed_truncated_extra_and_missing_descriptors_are_closed() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    for mode in [
        "json",
        "missing",
        "extra",
        "ancillary_truncated",
        "record_truncated",
        "request_id",
        "slot_id",
        "lease_id",
        "base_oid",
        "head_mode",
        "lease_transport",
    ] {
        let (receiver, sender) = transport::channel().unwrap();
        let (mut info, fd) = pool
            .acquire(Default::default())
            .unwrap()
            .into_descriptor()
            .unwrap();
        info.lease_transport = groove_core::LeaseTransport::ScmRights;
        let mut record =
            serde_json::json!({"api_version":1,"request_id":"expected","ok":true,"result":info});
        if matches!(
            mode,
            "slot_id" | "lease_id" | "base_oid" | "head_mode" | "lease_transport"
        ) {
            record["result"][mode] = serde_json::json!("invalid");
        }
        let body = match mode {
            "json" => b"{broken".to_vec(),
            "record_truncated" => vec![b' '; transport::MAX_RECORD + 1],
            _ => serde_json::to_vec(&record).unwrap(),
        };
        let count = match mode {
            "missing" => 0,
            "extra" => 2,
            "ancillary_truncated" => 32,
            _ => 1,
        };
        let descriptors = vec![fd.as_raw_fd(); count];
        send_raw(sender.as_fd(), &body, &descriptors);
        drop(fd);
        assert!(
            transport::receive_record(
                receiver.as_fd(),
                Some(if mode == "request_id" {
                    "wrong"
                } else {
                    "expected"
                })
            )
            .is_err(),
            "{mode}"
        );
        assert!(
            pool.acquire(Default::default()).is_ok(),
            "descriptor leak for {mode}"
        );
    }
}
