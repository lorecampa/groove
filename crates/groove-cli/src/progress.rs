//! Best-effort command liveness, rendered only by the stderr delivery thread.

use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

const DELAY: Duration = Duration::from_secs(1);
const JSON_INTERVAL: Duration = Duration::from_secs(5);
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct Progress {
    json: bool,
    request_id: Option<String>,
    command: &'static str,
    started: Instant,
    last: Option<Duration>,
    frame: usize,
    visible: bool,
}

impl Progress {
    pub fn new(json: bool, request_id: Option<String>, command: &'static str) -> Self {
        Self {
            json,
            request_id,
            command,
            started: Instant::now(),
            last: None,
            frame: 0,
            visible: false,
        }
    }

    pub fn tick(&mut self, writer: &mut impl Write, line_start: bool) -> io::Result<()> {
        self.render(writer, line_start, self.started.elapsed())
    }

    fn render(
        &mut self,
        writer: &mut impl Write,
        line_start: bool,
        elapsed: Duration,
    ) -> io::Result<()> {
        let interval = if self.json {
            JSON_INTERVAL
        } else {
            FRAME_INTERVAL
        };
        // Never split a diagnostic's partial line with progress output.
        if !line_start
            || elapsed < DELAY
            || self
                .last
                .is_some_and(|last| elapsed.saturating_sub(last) < interval)
        {
            return Ok(());
        }
        if self.json {
            let event = serde_json::json!({
                "api_version": crate::output::API_VERSION,
                "event": "progress",
                "request_id": self.request_id,
                "command": self.command,
                "phase": groove_core::current_phase(),
                "elapsed_ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            });
            writeln!(writer, "{event}")?;
        } else {
            let frame = ["|", "/", "-", "\\"][self.frame % 4];
            write!(
                writer,
                "\r\x1b[2K{frame} Running groove {}{}… ({}s)",
                self.command,
                groove_core::current_phase()
                    .map(|phase| format!(": {phase}"))
                    .unwrap_or_default(),
                elapsed.as_secs()
            )?;
            self.frame = self.frame.wrapping_add(1);
            self.visible = true;
        }
        self.last = Some(elapsed);
        writer.flush()
    }

    /// Remove only our own transient line before diagnostics, diagnostic output or exit.
    pub fn clear(&mut self, writer: &mut impl Write) -> io::Result<()> {
        if self.visible {
            writer.write_all(b"\r\x1b[2K")?;
            self.visible = false;
            writer.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_commands_and_partial_diagnostic_lines_have_no_progress() {
        for json in [false, true] {
            let mut progress = Progress::new(json, None, "show");
            let mut output = Vec::new();
            progress
                .render(&mut output, true, Duration::from_millis(999))
                .unwrap();
            progress
                .render(&mut output, false, Duration::from_secs(10))
                .unwrap();
            progress.clear(&mut output).unwrap();
            assert!(output.is_empty());
        }
    }

    #[test]
    fn spinner_animates_and_clears_before_other_output() {
        let mut progress = Progress::new(false, None, "show");
        let mut output = Vec::new();
        progress.render(&mut output, true, DELAY).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("| Running groove show"));
        output.clear();
        progress
            .render(&mut output, true, DELAY + FRAME_INTERVAL)
            .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("/ Running groove show"));
        output.clear();
        progress.clear(&mut output).unwrap();
        assert_eq!(output, b"\r\x1b[2K");
        output.clear();
        progress.clear(&mut output).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn json_heartbeats_are_versioned_correlated_and_rate_limited() {
        let mut progress = Progress::new(true, Some("request\n1".into()), "show");
        let mut output = Vec::new();
        progress.render(&mut output, true, DELAY).unwrap();
        progress
            .render(&mut output, true, DELAY + FRAME_INTERVAL)
            .unwrap();
        progress
            .render(&mut output, true, DELAY + JSON_INTERVAL)
            .unwrap();
        progress.clear(&mut output).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/progress-v2.schema.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let text = String::from_utf8(output).unwrap();
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert!(validator.is_valid(event), "{event}");
            assert_eq!(event["event"], "progress");
            assert_eq!(event["command"], "show");
            assert_eq!(event["request_id"], "request\n1");
        }
        assert_eq!(events[0]["elapsed_ms"], 1000);
        assert_eq!(events[1]["elapsed_ms"], 6000);
        assert!(!text.contains('\r'));
        assert!(!text.contains('\x1b'));
    }
}
