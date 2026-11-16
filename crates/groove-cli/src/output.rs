//! Response rendering: the versioned JSON envelope, human-readable text and exit codes.

use groove_core::{Error, ErrorCode};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{self, Write};

/// Wire version of the JSON envelope.
pub const API_VERSION: u32 = 2;

fn cli_key(key: &str) -> &str {
    match key {
        "lease" => "reservation",
        "leases" => "reservations",
        "lease_id" => "reservation_id",
        "lease_active" => "reservation_active",
        "request_key" => "idempotency_key",
        "worktree_path" => "workspace_path",
        "worktree_root" => "workspace_root",
        _ => key,
    }
}

/// Translate only presentation values; durable core records retain their vocabulary.
fn cli_value(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (cli_key(&key).to_owned(), cli_value(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(cli_value).collect()),
        scalar => scalar,
    }
}

pub const fn cli_error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidLease => "INVALID_RESERVATION",
        ErrorCode::LeaseMismatch => "RESERVATION_MISMATCH",
        _ => code.as_str(),
    }
}
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
    /// Data printed verbatim (a patch or a script), not composed Groove text.
    pub raw: Option<Vec<u8>>,
}

/// Position within a command's filtered, ordered collection.
#[derive(Debug, Serialize)]
pub struct Pagination {
    pub total: usize,
    pub offset: u64,
    pub limit: Option<u64>,
    pub next_offset: Option<u64>,
}

impl Output {
    /// Attach machine-readable continuation information and a human summary.
    pub fn paginated(mut self, pagination: Pagination) -> Result<Self, Error> {
        if !self.text.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push_str(&format!(
            "Total: {} entries; offset: {}.\n",
            pagination.total, pagination.offset
        ));
        if let Some(next) = pagination.next_offset {
            self.text.push_str(&format!(
                "More entries: repeat with --offset {next} (or --unlimited for everything).\n"
            ));
        }
        self.result["pagination"] = serde_json::to_value(pagination)?;
        Ok(self)
    }
    /// A result whose text rendering is produced by `render`.
    pub fn new(
        result: impl Serialize,
        render: impl FnOnce(&Value) -> String,
    ) -> Result<Self, Error> {
        let result = cli_value(serde_json::to_value(result)?);
        let text = render(&result);
        Ok(Self {
            result,
            text,
            status: 0,
            raw: None,
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
    /// A result whose human rendering is data that must reach stdout byte for byte,
    /// such as a patch (which may contain CRLF) or a shell script. Like `git diff`,
    /// file content is not escaped; Groove-composed text always is.
    pub fn raw(result: impl Serialize, text: impl Into<String>) -> Result<Self, Error> {
        Self::raw_bytes(result, text.into().into_bytes())
    }
    /// A byte-exact human response; JSON uses the supplied encoded representation.
    pub fn raw_bytes(result: impl Serialize, bytes: Vec<u8>) -> Result<Self, Error> {
        let mut output = Self::text(result, "")?;
        output.raw = Some(bytes);
        Ok(output)
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
        Format::Human if output.raw.is_some() => {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(output.raw.as_deref().unwrap_or_default())
                .and_then(|()| stdout.flush())
        }
        Format::Human => write_text(&printable(&output.text, true)),
    };
    output_status(written, output.status)
}

/// Exit status after writing a response. Any failed write exits 7, so a caller
/// learns its response was lost and can retry with the same request key. A reader
/// that closed the pipe (`groove list | head -1`) is not told again, matching
/// SIGPIPE in common Unix tools; other failures, such as a full disk, are reported.
///
/// The bounded stderr queue has already been drained by then, so the failure is
/// written to stderr directly.
fn output_status(written: io::Result<()>, status: u8) -> u8 {
    match written {
        Ok(()) => status,
        Err(e) => {
            if e.kind() != io::ErrorKind::BrokenPipe {
                let _ = writeln!(
                    io::stderr(),
                    "error: could not write the response to stdout: {e}"
                );
            }
            EXIT_OUTPUT_FAILED
        }
    }
}

/// Write a failure and return the process exit status.
pub fn emit_error(
    format: Format,
    request_id: Option<&str>,
    error: &Error,
    terminal: &crate::terminal::Terminal,
    args: Option<&crate::args::Args>,
    selected_pool: Option<&groove_core::PoolName>,
) -> u8 {
    let hint = error_hint(error, args, selected_pool);
    let (summary, body) = error
        .message
        .split_once('\n')
        .unwrap_or((&error.message, ""));
    let mut message = format!("error: {summary} ({})\n", cli_error_code(error.code));
    if !body.trim_end().is_empty() {
        message.push_str(body.trim_end());
        message.push('\n');
    }
    let details = cli_value(serde_json::to_value(&error.details).unwrap_or_default());
    if details.as_object().is_some_and(|d| !d.is_empty()) {
        message.push_str(&fields(&details, 1));
    }
    if error.retryable {
        message.push_str("  This error is retryable.\n");
    }
    if let Some(hint) = &hint {
        message.push_str(&format!("  Hint: {hint}\n"));
    }
    terminal.write(printable(&message, true).as_bytes());
    terminal.finish();
    if format == Format::Json {
        let mut body = json!({"ok": false, "error": {
            "code": cli_error_code(error.code),
            "message": error.message,
            "retryable": error.retryable,
            "details": details,
        }});
        if let Some(hint) = hint {
            body["error"]["hint"] = json!(hint);
        }
        return output_status(
            write_json(&envelope(request_id, body)),
            error_status(error.code),
        );
    }
    error_status(error.code)
}

/// Advisory recovery text selected from typed failures, never message matching.
fn error_hint(
    error: &Error,
    args: Option<&crate::args::Args>,
    selected_pool: Option<&groove_core::PoolName>,
) -> Option<String> {
    let hint = match error.code {
        ErrorCode::UnsavedChanges if args.is_some_and(|args| matches!(args.command, crate::args::Command::Pool(crate::args::PoolCommand::Destroy { .. }))) => Some(
            "Export any wanted residual files from the reported path before deletion, including ignored caches (snapshots exclude them). Then preview `groove pool destroy --force --dry-run` and repeat without --dry-run to acknowledge their loss. Released tasks cannot save new files through a release retry.",
        ),
        ErrorCode::InvalidLease if error.retryable => Some(
            "Another operation holds a reservation lock. Retry with the original reservation ID after contention clears. Never remove locks or protection.",
        ),
        ErrorCode::PoolExhausted | ErrorCode::AcquireTimeout => Some(
            "Run `groove list --slots` to inspect reservations. Wait for an owner to finish, or retry acquire with `--wait 30s`. Reservations never expire automatically.",
        ),
        ErrorCode::NotInitialized => Some(
            "Check `--repo`. For a new pool, run `groove pool init --capacity 4` in that repository; for interrupted initialization, see `groove pool init --help` before using --resume.",
        ),
        ErrorCode::InvalidRevision => Some(
            "Check that the requested revision exists locally and resolves to a commit. Retry with `--base` naming an existing commit, branch or tag.",
        ),
        ErrorCode::LeaseMismatch => Some(
            "Run `groove list` to find the active task and reservation ID, then check your selector. A released or reassigned reservation cannot select its successor.",
        ),
        ErrorCode::InvalidLease => Some(
            "Inspect `groove list --slots` and `groove show` with your selector. Legacy execution records must be resolved using the previous Groove version before upgrading. Never delete lock or protection files.",
        ),
        ErrorCode::UnsavedChanges => Some(
            "Preserve the changes before releasing. Stop all writers, then use `groove release --save` with the same --task or --reservation-id to save a snapshot and release the workspace.",
        ),
        ErrorCode::WorkspaceInUse => Some(
            "Stop the reported processes and all other workspace writers, then retry.",
        ),
        ErrorCode::PoolUnhealthy | ErrorCode::RecoveryFailed => Some(
            "Run `groove list --slots` and inspect the reported slot failures. Resolve the underlying filesystem or Git problem before retrying; for a slow Git operation, increase --git-timeout. Never remove lock or protection files to force reuse.",
        ),
        _ => None,
    }?;
    // Keep global selection flags inside each suggested command so copying it
    // cannot silently initialize or inspect a different pool/repository.
    let Some(args) = args else {
        return Some(hint.to_owned());
    };
    let pool = selected_pool
        .or(args.pool.as_ref())
        .cloned()
        .unwrap_or_default();
    let selection = format!(
        " --pool {} --repo {}",
        pool.as_str(),
        shell_words::quote(&args.repo.to_string_lossy())
    );
    Some(
        hint.split('`')
            .enumerate()
            .map(|(index, part)| {
                let part = if index % 2 == 1 && part.starts_with("groove ") {
                    format!("{part}{selection}")
                } else {
                    part.to_owned()
                };
                if index == 0 {
                    part
                } else {
                    format!("`{part}")
                }
            })
            .collect(),
    )
}

/// Print help or version: JSON `{text}` in JSON mode, plain text otherwise.
pub fn emit_help(format: Format, request_id: Option<&str>, text: &str) -> u8 {
    let written = match format {
        Format::Json => write_json(&envelope(
            request_id,
            json!({"ok": true, "result": {"text": text}}),
        )),
        Format::Human => write_text(&printable(text, true)),
    };
    output_status(written, 0)
}

fn write_json(response: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// Escape control characters so untrusted text (task labels, Git output, paths)
/// cannot inject terminal escape sequences. With `keep_layout`, newlines and tabs
/// pass through; table cells escape them too so one value stays on one row.
pub fn printable(text: &str, keep_layout: bool) -> std::borrow::Cow<'_, str> {
    let escape = |c: char| c.is_control() && !(keep_layout && matches!(c, '\n' | '\t'));
    if !text.chars().any(escape) {
        return text.into();
    }
    text.chars()
        .map(|c| {
            if escape(c) {
                c.escape_default().collect()
            } else {
                c.to_string()
            }
        })
        .collect::<String>()
        .into()
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
                let key = cli_key(key);
                match value {
                    Value::Null => {}
                    Value::Object(inner) if inner.is_empty() => {}
                    Value::Array(items) if items.is_empty() => {}
                    Value::Object(_) | Value::Array(_) => {
                        let contents = fields(value, depth + 1);
                        if !contents.is_empty() {
                            out.push_str(&format!("{indent}{key}:\n{contents}"));
                        }
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
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| printable(cell, false).into_owned())
                .collect()
        })
        .collect();
    let rows = rows.as_slice();
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
    fn untrusted_text_cannot_emit_terminal_controls() {
        let label = "\u{1b}[31mred\u{1b}]0;title\u{7}";
        let shown = printable(label, true);
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(shown.contains("red"));
        assert_eq!(printable("a\tb\nc", true), "a\tb\nc");
        let text = table(&["TASK"], &[vec!["a\nb".into()]]);
        assert_eq!(
            text.lines().count(),
            2,
            "a newline in a cell must not add a row: {text:?}"
        );
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
