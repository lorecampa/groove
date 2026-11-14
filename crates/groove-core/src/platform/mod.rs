//! OS boundaries. Ownership and recovery policy stay in the shared core.

pub(crate) mod fs;
#[cfg(target_os = "linux")]
pub(crate) mod mounts;
pub(crate) mod process;
