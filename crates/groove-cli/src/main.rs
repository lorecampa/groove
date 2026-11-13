//! `groove`: reusable Git worktree pools for parallel coding tasks.

mod args;
mod commands;
mod contract;
mod logging;
mod output;
mod preferences;
mod progress;
mod ready;
mod terminal;

use args::{Args, Command};
use clap::{error::ErrorKind, FromArgMatches};
use groove_core::{Error, ErrorCode};
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
        let verbose = crate::contract::command()
            .ignore_errors(true)
            .try_get_matches_from(raw)
            .map_or(0, |m| m.get_count("verbose"));
        let quiet = crate::contract::command()
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

/// JSON only when `--json` is given.
fn requested_format(json_flag: bool) -> Format {
    if json_flag {
        Format::Json
    } else {
        Format::Human
    }
}

fn main() -> ExitCode {
    let raw: Vec<_> = std::env::args_os().collect();
    if let Some(code) = ready::internal(&raw) {
        return ExitCode::from(code);
    }
    match crate::contract::command()
        .try_get_matches_from(&raw)
        .and_then(|matches| Args::from_arg_matches(&matches))
    {
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
    output::emit_error(partial.format, request_id, &error, &terminal, None)
}

fn run(args: &Args) -> u8 {
    let terminal = terminal::Terminal::new(terminal::Verbosity {
        verbose: args.verbose,
        quiet: args.quiet,
    });
    // The preferences editor owns the terminal while it runs.
    if !matches!(
        args.command,
        Command::Config(args::ConfigCommand::Global(args::GlobalCommand::Edit))
    ) {
        terminal.start_progress(args.json, args.request_id.clone(), args.command.name());
    }
    run_command(args, &terminal)
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
            Err(error) => {
                return output::emit_error(format, request_id, &error, terminal, Some(args))
            }
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
        selected_pool: args.pool.as_ref(),
        git_timeout: Duration::from_millis(args.git_timeout_ms),
    };
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match commands::run(&context, &args.command) {
        Ok(result) => {
            ready::trigger(args);
            tracing::info!(
                event = "command.completed",
                elapsed_ms = elapsed_ms(),
                "Command completed"
            );
            output::emit_success(format, request_id, result, terminal)
        }
        Err(error) => {
            tracing::error!(event = "command.failed", file_only = true, code = %error.code, elapsed_ms = elapsed_ms(), "Command failed");
            output::emit_error(format, request_id, &error, terminal, Some(args))
        }
    }
}
