//! Bounded Git worktree pools. A [`Lease`] owns a kernel lock, not a timeout.
//! Callers must preserve results and stop writers before explicit release.
//!
//! Requires Linux, Git 2.40+, and a local filesystem. Worktrees share repository
//! objects, refs and configuration; they are not security sandboxes. Pool clones
//! share configuration, while each non-cloneable lease owns one slot descriptor.
//!
//! ```no_run
//! use groove_core::{Pool, PoolOptions};
//! use std::process::Command;
//!
//! # fn main() -> groove_core::Result<()> {
//! Pool::init("/path/to/repo", PoolOptions::default())?;
//! let pool = Pool::open("/path/to/repo")?;
//! let mut lease = pool.acquire(Default::default())?;
//! let mut worker = Command::new("git");
//! worker.args(["status", "--short"]);
//! let status = lease.run_worker(worker)?;
//! // Inspect status, preserve results, and stop all writers before releasing.
//! lease.release()?;
//! # Ok(())
//! # }
//! ```
//!
//! Dropping a lease closes its descriptor without cleanup. A participating child
//! can retain ownership after its parent exits. Cleanup happens on acquisition.
//! CLI supervisors use [`transport`] to retain ownership across processes.
#![cfg(target_os = "linux")]
#![deny(missing_docs, missing_debug_implementations)]
mod error;
mod git;
mod identifiers;
mod lease;
mod locking;
mod pool;
mod recovery;
mod repository;
mod state;
pub mod transport;
mod types;

pub use error::{Error, ErrorCode, Result};
pub use identifiers::{CommitOid, LeaseId, SlotId};
pub use lease::Lease;
pub use pool::Pool;
pub use types::*;
