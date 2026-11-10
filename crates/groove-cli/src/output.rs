//! Response rendering: the versioned JSON envelope, human-readable text and exit codes.

use groove_core::{Error, ErrorCode, ExecResult, Preparation, PreparationStatus};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{self, Write};

/// Wire version of the JSON envelope.
pub const API_VERSION: u32 = 1;
/// Exit status when stdout cannot be written.
pub const EXIT_OUTPUT_FAILED: u8 = 7;

/// Requested presentation, chosen once per invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable text on stdout, errors on stderr.
    Human,
    /// Exactly one versioned JSON object on stdout.
    Json,
}

/// A successful command result: its JSON payload, its human rendering and its exit status.
#[derive(Debug)]
pub struct Output {
    pub result: Value,
    pub text: String,
    pub status: u8,
}
impl Output {
    /// A result whose text rendering is produced by `render`.
    pub fn new(
        result: impl Serialize,
        render: impl FnOnce(&Value) -> String,
    ) -> Result<Self, Error> {
        let result = serde_json::to_value(result)?;
        let text = render(&result);
        Ok(Self {
            result,
            text,
            status: 0,
        })
    }
    /// A result rendered as indented `key: value` lines.
    pub fn fields(result: impl Serialize) -> Result<Self, Error> {
        Self::new(result, |value| fields(value, 0))
    }
    /// A result whose human rendering is plain text, e.g. a path or a script.
    pub fn text(result: impl Serialize, text: impl Into<String>) -> Result<Self, Error> {
        let text = text.into();
        Self::new(result, |_| text)
    }
    pub fn with_status(mut self, status: u8) -> Self {
        self.status = status;
        self
    }
}

/// CLI exit category for an error code; see the overview help.
pub fn error_status(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::InvalidArgument | ErrorCode::UnsupportedConfiguration => 2,
        ErrorCode::PoolExhausted | ErrorCode::AcquireTimeout => 3,
        ErrorCode::NotInitialized | ErrorCode::ConfigConflict | ErrorCode::InvalidRevision => 4,
        ErrorCode::InvalidLease
        | ErrorCode::LeaseMismatch
        | ErrorCode::UnsavedChanges
        | ErrorCode::WorkspaceInUse => 5,
        ErrorCode::RecoveryFailed | ErrorCode::PoolUnhealthy => 6,
        _ => 7,
    }
}

/// Exit status for a setup attempt: zero when ready, else the failing step's status.
pub fn preparation_status(preparation: &Preparation) -> u8 {
    if preparation.status == PreparationStatus::Ready {
        return 0;
    }
    if let Some(signal) = preparation.cancelled {
        return (128 + signal).clamp(0, 255) as u8;
    }
    preparation
        .steps
        .last()
        .and_then(|step| step.execution.as_ref())
        .map(ExecResult::shell_status)
        .filter(|status| *status != 0)
        .unwrap_or(EXIT_OUTPUT_FAILED)
}

fn envelope(request_id: Option<&str>, body: Value) -> Value {
    let mut response = json!({"api_version": API_VERSION, "request_id": request_id});
    if let (Some(response), Value::Object(body)) = (response.as_object_mut(), body) {
        response.extend(body);
    }
    response
}

/// Write a success and return the process exit status.
pub fn emit_success(
    format: Format,
    request_id: Option<&str>,
    output: Output,
    terminal: &crate::terminal::Terminal,
) -> u8 {
    terminal.finish();
    let written = match format {
        Format::Json => write_json(&envelope(
            request_id,
            json!({"ok": true, "result": output.result}),
        )),
        Format::Human => write_text(&output.text),
    };
    if written.is_err() {
        EXIT_OUTPUT_FAILED
    } else {
        output.status
    }
}

/// Write a failure and return the process exit status.
pub fn emit_error(
    format: Format,
    request_id: Option<&str>,
    error: &Error,
    terminal: &crate::terminal::Terminal,
) -> u8 {
    let hint = error_hint(error);
    let mut message = format!("error: {} ({})\n", error.message, error.code);
    let details = serde_json::to_value(&error.details).unwrap_or_default();
    if details.as_object().is_some_and(|d| !d.is_empty()) {
        message.push_str(&fields(&details, 1));
    }
    if error.retryable {
        message.push_str("  This error is retryable.\n");
    }
    if let Some(hint) = hint {
        message.push_str(&format!("  Hint: {hint}\n"));
    }
    terminal.write(message.as_bytes());
    terminal.finish();
    if format == Format::Json {
        let mut body = json!({"ok": false, "error": {
            "code": error.code,
            "message": error.message,
            "retryable": error.retryable,
            "details": error.details,
        }});
        if let Some(hint) = hint {
            body["error"]["hint"] = json!(hint);
        }
        if write_json(&envelope(request_id, body)).is_err() {
            return EXIT_OUTPUT_FAILED;
        }
    }
    error_status(error.code)
}

/// Advisory recovery text selected from typed failures, never message matching.
fn error_hint(error: &Error) -> Option<&'static str> {
    match error.code {
        ErrorCode::PoolExhausted | ErrorCode::AcquireTimeout => Some(
            "Run `groove list --slots` to inspect reservations. Wait for an owner to finish, or retry acquire with `--wait-ms 30000`. Reservations never expire automatically.",
        ),
        ErrorCode::NotInitialized => Some(
            "Check `--repo`. For a new pool, run `groove init --capacity 4` in that repository; for interrupted initialization, see `groove init --help` before using --resume.",
        ),
        ErrorCode::InvalidRevision => Some(
            "Check that the requested revision exists locally and resolves to a commit. Retry with `--base` naming an existing commit, branch or tag.",
        ),
        ErrorCode::LeaseMismatch => Some(
            "Run `groove list` to find the active task and lease ID, then check your selector. A released or reassigned lease cannot select its successor.",
        ),
        ErrorCode::InvalidLease => Some(
            "Inspect `groove list --slots` and `groove show` with your selector. If a managed run is present, request `groove stop` with that selector; for orphan recovery, stop every writer first and follow `groove recover --help`. Never delete lock or protection files.",
        ),
        ErrorCode::UnsavedChanges => Some(
            "Preserve the changes before releasing. Stop all writers, then use `groove finish` with the same --task or --lease-id to save a snapshot and release the workspace.",
        ),
        ErrorCode::WorkspaceInUse => Some(
            "Stop the reported processes and all other workspace writers, then retry. For a managed run, use `groove stop` with the same selector and inspect until its execution record clears.",
        ),
        ErrorCode::PoolUnhealthy | ErrorCode::RecoveryFailed => Some(
            "Run `groove list --slots` and inspect the reported slot failures. Resolve the underlying filesystem or Git problem before retrying; never remove lock or protection files to force reuse.",
        ),
        _ => None,
    }
}

/// Print help or version: JSON `{text}` in JSON mode, plain text otherwise.
pub fn emit_help(format: Format, request_id: Option<&str>, text: &str) -> u8 {
    let written = match format {
        Format::Json => write_json(&envelope(
            request_id,
            json!({"ok": true, "result": {"text": text}}),
        )),
        Format::Human => write_text(text),
    };
    if written.is_err() {
        EXIT_OUTPUT_FAILED
    } else {
        0
    }
}

fn write_json(response: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn write_text(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.is_empty() && !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}

/// Render a JSON value as indented `key: value` lines, skipping nulls.
pub fn fields(value: &Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let mut out = String::new();
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match value {
                    Value::Null => {}
                    Value::Object(inner) if inner.is_empty() => {}
                    Value::Array(items) if items.is_empty() => {}
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{indent}{key}:\n{}", fields(value, depth + 1)));
                    }
                    scalar => out.push_str(&format!("{indent}{key}: {}\n", scalar_text(scalar))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{indent}-\n{}", fields(item, depth + 1)));
                    }
                    scalar => out.push_str(&format!("{indent}- {}\n", scalar_text(scalar))),
                }
            }
        }
        scalar => out.push_str(&format!("{indent}{}\n", scalar_text(scalar))),
    }
    out
}

/// A scalar without JSON string quotes.
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

/// Render rows as left-aligned columns under a header.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| -> String {
        let mut line = String::new();
        for (i, (cell, width)) in cells.iter().zip(&widths).enumerate() {
            if i + 1 == cells.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  "));
            }
        }
        line.trim_end().to_owned() + "\n"
    };
    let mut out = line(header.to_vec());
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_align_columns() {
        let text = table(
            &["A", "LONGER"],
            &[
                vec!["value".into(), "x".into()],
                vec!["v".into(), "y".into()],
            ],
        );
        assert_eq!(text, "A      LONGER\nvalue  x\nv      y\n");
    }

    #[test]
    fn fields_skip_empty_values() {
        let text = fields(&json!({"a": 1, "b": null, "c": {"d": "e"}, "f": []}), 0);
        assert_eq!(text, "a: 1\nc:\n  d: e\n");
    }

    #[test]
    fn exit_categories_are_stable() {
        assert_eq!(error_status(ErrorCode::InvalidArgument), 2);
        assert_eq!(error_status(ErrorCode::PoolExhausted), 3);
        assert_eq!(error_status(ErrorCode::ConfigConflict), 4);
        assert_eq!(error_status(ErrorCode::LeaseMismatch), 5);
        assert_eq!(error_status(ErrorCode::PoolUnhealthy), 6);
        assert_eq!(error_status(ErrorCode::GitError), 7);
    }
}
