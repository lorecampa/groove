mod support;

use groove_core::{ErrorCode, Pool, PoolOptions};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, net::UnixStream, process::CommandExt},
    },
    panic::{catch_unwind, AssertUnwindSafe},
    process::{Command, Stdio},
};

#[test]
fn unrelated_child_keeps_ownership_until_exec_closes_inherited_descriptors() {
    for execution_lock in [false, true] {
        let repo = support::Repo::new();
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&repo.path).unwrap();
        let lease = pool.acquire(Default::default()).unwrap();
        lease.protect_from_reuse().unwrap();
        let info = lease.info().clone();
        let execution = if execution_lock {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(repo.state().join("locks/wt-0001.execution.lock"))
                .unwrap();
            // SAFETY: file owns this disposable pool's permanent execution-lock
            // descriptor. The test retains it until the child inherits it at fork.
            let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            assert_eq!(locked, 0);
            Some(file)
        } else {
            None
        };
        let slot_owner = if execution_lock {
            drop(lease);
            None
        } else {
            Some(lease)
        };
        let result_path = info.worktree_path.join("tracked.txt");
        fs::write(&result_path, "protected result").unwrap();
        let lock = repo.state().join(if execution_lock {
            "locks/wt-0001.execution.lock"
        } else {
            "locks/wt-0001.lock"
        });
        let inode = fs::metadata(&lock).unwrap().ino();

        let (mut parent, inherited) = UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(support::PROCESS_TIMEOUT))
            .unwrap();
        parent
            .set_write_timeout(Some(support::PROCESS_TIMEOUT))
            .unwrap();
        inherited
            .set_read_timeout(Some(support::PROCESS_TIMEOUT))
            .unwrap();
        inherited
            .set_write_timeout(Some(support::PROCESS_TIMEOUT))
            .unwrap();
        let mut command = Command::new("bash");
        command.args(["-c", "read -r line"]).stdin(Stdio::piped());
        // SAFETY: the child closure only uses async-signal-safe read/write and errno
        // access with a valid owned socket and one-byte stack buffer. The captured
        // UnixStream keeps its descriptor alive through spawn. Socket timeouts bound
        // the handshake even if the parent fails; no Rust locks/allocations run here.
        unsafe {
            command.pre_exec(move || {
                let mut byte = [1u8];
                if libc::write(inherited.as_raw_fd(), byte.as_ptr().cast(), 1) != 1
                    || libc::read(inherited.as_raw_fd(), byte.as_mut_ptr().cast(), 1) != 1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawning = std::thread::spawn(move || support::TestChild::spawn(&mut command));
        let assertions = catch_unwind(AssertUnwindSafe(|| {
            parent.read_exact(&mut [0]).unwrap(); // The child is forked but has not exec'd.
            drop(slot_owner);
            drop(execution);
            assert_eq!(
                pool.release_lease(info.lease_id).unwrap_err().code,
                ErrorCode::InvalidLease
            );
            assert_eq!(
                pool.finish(info.lease_id, Default::default())
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidLease
            );
            assert_eq!(
                pool.acquire(Default::default()).unwrap_err().code,
                ErrorCode::PoolExhausted
            );
            assert_eq!(pool.protected_leases().unwrap()[0].lease_id, info.lease_id);
            assert_eq!(fs::read(&result_path).unwrap(), b"protected result");
            assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
        }));
        // Always unblock spawn and join, including when an assertion above fails.
        let _ = parent.write_all(&[1]);
        let mut child = spawning.join().unwrap();
        if let Err(panic) = assertions {
            std::panic::resume_unwind(panic);
        }
        // spawn returned only after exec. The unrelated process is still alive, but
        // CLOEXEC has closed its inherited lease: ownership depends on fds, not PID.
        assert!(child.try_wait().unwrap().is_none());
        let finished = pool.finish(info.lease_id, Default::default()).unwrap();
        assert!(finished.snapshot.pinned);
        assert_eq!(
            repo.git(&[
                "show",
                &format!("{}:tracked.txt", finished.snapshot.work_tree)
            ]),
            "protected result"
        );
        pool.release_lease(info.lease_id).unwrap();
        assert!(pool.protected_leases().unwrap().is_empty());
        assert_eq!(fs::read(&result_path).unwrap(), b"protected result");
        let next = pool.acquire(Default::default()).unwrap();
        assert_ne!(next.info().lease_id, info.lease_id);
        assert_eq!(fs::read(result_path).unwrap(), b"baseline\n");
        assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
    }
}
