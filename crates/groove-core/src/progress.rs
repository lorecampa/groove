//! Best-effort process-wide phase observation; never an ownership signal.
use std::sync::{OnceLock, RwLock};

static PHASE: OnceLock<RwLock<Option<&'static str>>> = OnceLock::new();

pub(crate) fn set_phase(name: &'static str) {
    // Diagnostics must not turn a poisoned observer into an operation failure.
    if let Ok(mut phase) = PHASE.get_or_init(|| RwLock::new(None)).write() {
        *phase = Some(name);
    }
}

/// Most recently reported operation phase in this process, if available.
///
/// Concurrent operations may replace one another's phase. This observation is
/// advisory: it implies neither ownership, progress percentage nor completion.
pub fn current_phase() -> Option<&'static str> {
    PHASE
        .get()
        .and_then(|phase| phase.read().ok().and_then(|phase| *phase))
}
