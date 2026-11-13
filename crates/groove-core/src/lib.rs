//! Bounded, crash-safe pools of reusable Git worktrees for parallel tasks.
//!
//! A [`Pool`] owns a fixed number of slots. Ownership is a kernel `flock` on a
//! permanent per-slot lock file, never a timeout, PID or metadata record:
//!
//! - [`Pool::reserve`] creates a *durable reservation*, optionally named by a task
//!   label, that survives process exit until explicitly released. Retrying the
//!   same request key returns the same reservation.
//! - [`Pool::acquire`] returns an in-process [`Lease`] whose descriptor owns the
//!   slot; dropping it closes the descriptor without cleanup.
//!
//! Callers must preserve results and stop every writer before release; the next
//! acquisition cleans the slot. [`Pool::finish`] pins a local checkpoint first.
//!
//! ```no_run
//! use groove_core::{snapshot::SaveOptions, Pool, PoolOptions, ReservationOptions};
//!
//! # fn main() -> groove_core::Result<()> {
//! Pool::init("/path/to/repo", PoolOptions::default())?;
//! let pool = Pool::open("/path/to/repo")?;
//! let lease = pool.reserve(ReservationOptions {
//!     task: Some("fix parser".into()),
//!     ..Default::default()
//! })?;
//! // Edit in lease.worktree_path, then stop all writers.
//! let finished = pool.finish(lease.lease_id, SaveOptions::default())?;
//! println!("pinned snapshot {}", finished.snapshot.id);
//! # Ok(())
//! # }
//! ```
//!
//! Requires Linux or macOS, Git 2.40+ and a local filesystem. Worktrees share repository
//! objects, refs and configuration; they are not security sandboxes.
#![deny(missing_docs, missing_debug_implementations)]
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!(
    "groove-core supports Linux and macOS: ownership requires Unix descriptor locks and process supervision"
);
mod destruction;
mod error;
mod execution;
pub mod experimental;
mod finish;
mod git;
mod identifiers;
mod inspection;
mod lease;
mod lifecycle;
mod locking;
mod namespace;
mod platform;
mod pool;
mod quiescence;
mod recovery;
mod repository;
mod reservation;
mod settings;
pub mod snapshot;
mod state;
mod storage;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
mod types;

pub use destruction::{DestroyOptions, DestroyResult};
pub use error::{CapacityFailure, Error, ErrorCode, ErrorDetails, GitFailure, Result};
pub use finish::FinishResult;
pub use identifiers::{CommitOid, LeaseId, PoolId, PoolName, SlotId};
pub use inspection::{PoolInspection, SlotInspection, SlotStatus, WorkspaceGitStatus};
pub use lease::Lease;
pub use lifecycle::Phase as PoolPhase;
pub use namespace::PoolSummary;
pub use pool::Pool;
pub use quiescence::{ProcessUse, WorkspaceProcess};
pub use reservation::ReservationOptions;
pub use storage::StorageUsage;
pub use types::*;
