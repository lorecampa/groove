//! Bounded phase diagnostics, independent of snapshot state and correctness.
use std::time::Instant;

/// A scope starts failed: early returns and unwinding must never report success.
/// Disabled DEBUG diagnostics avoid even reading the monotonic clock.
pub(super) struct Phase {
    started: Option<Instant>,
    operation: &'static str,
    phase: &'static str,
    outcome: &'static str,
}

impl Phase {
    pub(super) fn new(operation: &'static str, phase: &'static str) -> Self {
        Self {
            started: tracing::enabled!(tracing::Level::DEBUG).then(Instant::now),
            operation,
            phase,
            outcome: "error",
        }
    }

    /// Finish only after the entire phase has succeeded.
    pub(super) fn success(mut self) {
        self.outcome = "success";
    }

    /// Retain the original result and its error source without changing control flow.
    pub(super) fn result<T, E>(mut self, result: Result<T, E>) -> Result<T, E> {
        if result.is_ok() {
            self.outcome = "success";
        }
        result
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            // Saturation keeps the structured field a portable JSON integer.
            let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            tracing::debug!(
                event = "snapshot_timing",
                operation = self.operation,
                phase = self.phase,
                elapsed_us,
                outcome = self.outcome,
                "Snapshot phase completed"
            );
        }
    }
}
