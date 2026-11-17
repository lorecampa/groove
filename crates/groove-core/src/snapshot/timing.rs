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
        let reported = match phase {
            "validation" | "preflight" => "validating",
            "special_file_scan" => "scanning special files",
            "scratch_index_tree" | "staging" | "staged_sizes" | "work_tree" => "staging",
            "stability" => "checking stability",
            "manifest" | "publish" => "publishing",
            "lock_wait" => "waiting for snapshot lock",
            "recovery_creation" => "creating recovery checkout",
            "apply" => "restoring files",
            "safety_pin" => "pinning safety snapshot",
            "safety_unpin" => "finishing restore",
            _ => operation,
        };
        crate::progress::set_phase(reported);
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
