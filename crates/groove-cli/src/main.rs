//! `groove`: reusable Git worktree pools for parallel coding tasks.

mod args;
mod commands;
mod logging;
mod output;
mod preferences;
mod terminal;

use args::{Args, Command};
use clap::{error::ErrorKind, CommandFactory, Parser};
use groove_core::{Error, ErrorCode, ExecutionSignals};
use output::Format;
use std::{process::ExitCode, time::Duration};

/// Global flags recovered from a command line that failed to parse.
struct Partial {
    format: Format,
    request_id: Option<String>,
    verbose: u8,
    quiet: bool,
}
impl Partial {
    fn recover(raw: &[std::ffi::OsString]) -> Self {
        // Help and version short-circuit clap before later flags are recorded, so
        // scan the options (everything before `--`) for the global flags directly.
        let options: Vec<&str> = raw
            .iter()
            .skip(1)
            .map_while(|arg| arg.to_str().filter(|arg| *arg != "--"))
            .collect();
        let request_id = options.iter().enumerate().find_map(|(i, arg)| {
            arg.strip_prefix("--request-id=")
                .or_else(|| {
                    (*arg == "--request-id")
                        .then(|| options.get(i + 1).copied())
                        .flatten()
                })
                .map(str::to_owned)
        });
        let verbose = Args::command()
            .ignore_errors(true)
            .try_get_matches_from(raw)
            .map_or(0, |m| m.get_count("verbose"));
        let quiet = Args::command()
            .ignore_errors(true)
            .try_get_matches_from(raw)
            .is_ok_and(|m| m.get_flag("quiet"));
        Self {
            format: requested_format(options.contains(&"--json")),
            request_id,
            verbose,
            quiet,
        }
    }
}

/// JSON when `--json` is given or `GROOVE_OUTPUT=json` is set, so agents can opt in once.
fn requested_format(json_flag: bool) -> Format {
    if json_flag || std::env::var_os("GROOVE_OUTPUT").is_some_and(|value| value == "json") {
        Format::Json
    } else {
        Format::Human
    }
}

fn main() -> ExitCode {
    let raw: Vec<_> = std::env::args_os().collect();
    match Args::try_parse_from(&raw) {
        Ok(args) => ExitCode::from(run(&args)),
        Err(error) => ExitCode::from(reject(&raw, &error)),
    }
}

/// Handle help, version and argument errors, honoring `--json` when present.
fn reject(raw: &[std::ffi::OsString], error: &clap::Error) -> u8 {
    let partial = Partial::recover(raw);
    let request_id = partial.request_id.as_deref();
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        return output::emit_help(partial.format, request_id, &error.render().to_string());
    }
    let terminal = terminal::Terminal::new(terminal::Verbosity {
        verbose: partial.verbose,
        quiet: partial.quiet,
    });
    // Honor logging preferences for argument errors too; a broken file disables logging.
    let settings =
        preferences::load().map_or_else(|_| preferences::Logging::disabled(), |c| c.logging);
    let _logging = logging::init(
        terminal.clone(),
        settings,
        partial.request_id.clone(),
        "arguments",
    );
    tracing::error!(
        event = "arguments.invalid",
        file_only = true,
        "Invalid command arguments"
    );
    let rendered = error.render().to_string();
    let error = Error::new(
        ErrorCode::InvalidArgument,
        rendered.strip_prefix("error: ").unwrap_or(&rendered),
    );
    output::emit_error(partial.format, request_id, &error, &terminal)
}

fn run(args: &Args) -> u8 {
    // Block cancellation before starting the terminal delivery thread.
    let signals = if args.command.supervises_workers() {
        ExecutionSignals::block().and_then(|guard| {
            ExecutionSignals::enable_subreaper()?;
            Ok(Some(guard))
        })
    } else {
        Ok(None)
    };
    let terminal = terminal::Terminal::new(terminal::Verbosity {
        verbose: args.verbose,
        quiet: args.quiet,
    });
    match signals {
        Ok(_signals) => run_command(args, &terminal),
        Err(error) => output::emit_error(
            requested_format(args.json),
            args.request_id.as_deref(),
            &error,
            &terminal,
        ),
    }
}

fn run_command(args: &Args, terminal: &terminal::Terminal) -> u8 {
    let format = requested_format(args.json);
    let request_id = args.request_id.as_deref();
    // Configuration and completion commands work even with broken preferences.
    let logging = if matches!(
        args.command,
        Command::Config(_) | Command::Completions { .. }
    ) {
        preferences::Logging::disabled()
    } else {
        match preferences::load() {
            Ok(settings) => settings.logging,
            Err(error) => return output::emit_error(format, request_id, &error, terminal),
        }
    };
    let _logging = logging::init(
        terminal.clone(),
        logging,
        args.request_id.clone(),
        args.command.name(),
    );
    let started = std::time::Instant::now();
    tracing::debug!(event = "command.started", "Command started");
    let context = commands::Context {
        repo: &args.repo,
        git_timeout: Duration::from_millis(args.git_timeout_ms),
        terminal,
    };
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match commands::run(&context, &args.command) {
        Ok(result) => {
            tracing::info!(
                event = "command.completed",
                elapsed_ms = elapsed_ms(),
                "Command completed"
            );
            output::emit_success(format, request_id, result, terminal)
        }
        Err(error) => {
            tracing::error!(event = "command.failed", file_only = true, code = %error.code, elapsed_ms = elapsed_ms(), "Command failed");
            output::emit_error(format, request_id, &error, terminal)
        }
    }
}
