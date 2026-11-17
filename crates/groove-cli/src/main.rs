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
mod updates;

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
        let options: Vec<_> = raw.iter().skip(1).take_while(|arg| *arg != "--").collect();
        let request_id = options.iter().enumerate().find_map(|(i, arg)| {
            let arg = arg.to_str()?;
            arg.strip_prefix("--request-id=")
                .or_else(|| {
                    (arg == "--request-id")
                        .then(|| options.get(i + 1).and_then(|value| value.to_str()))
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
            format: requested_format(options.iter().any(|arg| *arg == "--json")),
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
    match std::panic::catch_unwind(|| dispatch(&raw)) {
        Ok(code) => code,
        Err(_) => {
            let partial = Partial::recover(&raw);
            let terminal = terminal::Terminal::new(terminal::Verbosity {
                verbose: partial.verbose,
                quiet: partial.quiet,
            });
            let error = Error::new(
                ErrorCode::ProtocolError,
                "An internal panic interrupted the command; inspect pool state before retrying",
            );
            ExitCode::from(output::emit_error(
                partial.format,
                partial.request_id.as_deref(),
                &error,
                &terminal,
                None,
                None,
            ))
        }
    }
}

fn dispatch(raw: &[std::ffi::OsString]) -> ExitCode {
    match crate::contract::command()
        .color(requested_color(raw))
        .try_get_matches_from(raw)
        .and_then(|matches| {
            let mut args = Args::from_arg_matches(&matches)?;
            args.git_timeout_explicit = matches.value_source("git_timeout_ms")
                == Some(clap::parser::ValueSource::CommandLine);
            let mut command_matches = &matches;
            while let Some((_, child)) = command_matches.subcommand() {
                if let Ok(Some(pool)) = child.try_get_one::<groove_core::PoolName>("pool") {
                    args.pool = Some(pool.clone());
                }
                if child.try_get_one::<u64>("git_timeout_ms").is_ok()
                    && child.value_source("git_timeout_ms")
                        == Some(clap::parser::ValueSource::CommandLine)
                {
                    args.git_timeout_ms = *child
                        .get_one::<u64>("git_timeout_ms")
                        .expect("explicit timeout has a parsed value");
                    args.git_timeout_explicit = true;
                }
                command_matches = child;
            }
            Ok(args)
        })
        .and_then(|args| {
            args.normalize()
                .map_err(|message| clap::Error::raw(ErrorKind::InvalidValue, message))
        }) {
        Ok(mut args) => {
            if matches!(args.command, Command::Upgrade(_))
                && raw.iter().any(|arg| {
                    arg == "--target-version"
                        || arg
                            .to_str()
                            .is_some_and(|arg| arg.starts_with("--target-version="))
                })
            {
                args.deprecations
                    .push("--target-version is deprecated; use --to");
            }
            if raw.iter().any(|arg| {
                arg == "--workspace-root"
                    || arg
                        .to_str()
                        .is_some_and(|arg| arg.starts_with("--workspace-root="))
            }) {
                args.deprecations
                    .push("--workspace-root is deprecated; use --root");
            }
            if matches!(
                args.command,
                Command::Snapshot(args::SnapshotCommand::Prune { apply: true, .. })
            ) {
                args.deprecations.push("--apply is deprecated; use --yes");
            }
            if matches!(
                args.command,
                Command::Snapshot(args::SnapshotCommand::Delete { .. })
            ) && raw.iter().any(|arg| arg == "--force")
            {
                args.deprecations
                    .push("snapshot delete --force is deprecated; use --pinned");
            }
            for words in raw.windows(2) {
                if words[0] == "pool" && words[1] == "init" {
                    args.deprecations
                        .push("pool init is deprecated; use pool create");
                }
                if words[0] == "pool" && words[1] == "destroy" {
                    args.deprecations
                        .push("pool destroy is deprecated; use pool delete");
                }
            }
            ExitCode::from(run(&mut args))
        }
        Err(error) => ExitCode::from(reject(raw, &error)),
    }
}

fn requested_color(raw: &[std::ffi::OsString]) -> clap::ColorChoice {
    use std::io::IsTerminal;
    let mut choice = "auto";
    let mut arguments = raw.iter().skip(1).take_while(|arg| *arg != "--");
    while let Some(arg) = arguments.next() {
        if arg == "--color" {
            choice = arguments
                .next()
                .and_then(|arg| arg.to_str())
                .unwrap_or("auto");
        } else if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("--color=")) {
            choice = value;
        }
    }
    match choice {
        "always" => clap::ColorChoice::Always,
        "never" => clap::ColorChoice::Never,
        _ if std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal() => {
            clap::ColorChoice::Never
        }
        _ => clap::ColorChoice::Auto,
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
        if matches!(partial.format, Format::Human)
            && requested_color(raw) != clap::ColorChoice::Never
        {
            return output::emit_styled_help(&error.render().ansi().to_string());
        }
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
    output::emit_error(partial.format, request_id, &error, &terminal, None, None)
}

fn run(args: &mut Args) -> u8 {
    let terminal = terminal::Terminal::new(terminal::Verbosity {
        verbose: args.verbose,
        quiet: args.quiet,
    });
    // The preferences editor owns the terminal while it runs.
    if !matches!(args.command, Command::Config(args::ConfigCommand::Edit)) {
        terminal.start_progress(args.json, args.request_id.clone(), args.command.name());
    }
    run_command(args, &terminal)
}

fn run_command(args: &mut Args, terminal: &terminal::Terminal) -> u8 {
    let format = requested_format(args.json);
    let request_id = args.request_id.as_deref();
    if args.pool.is_some()
        && matches!(
            args.command,
            Command::Snapshot(_)
                | Command::Help { .. }
                | Command::Config(_)
                | Command::Completions { .. }
                | Command::Upgrade(_)
                | Command::Pool(args::PoolCommand::List { .. })
        )
    {
        return output::emit_error(
            format,
            request_id,
            &Error::new(
                ErrorCode::InvalidArgument,
                "--pool is not used by this command; omit it",
            ),
            terminal,
            Some(args),
            None,
        );
    }
    // Configuration and completion commands work even with broken preferences.
    let logging = if matches!(
        args.command,
        Command::Config(_)
            | Command::Completions { .. }
            | Command::Upgrade(_)
            | Command::Help { .. }
    ) {
        preferences::Logging::disabled()
    } else {
        match preferences::load() {
            Ok(settings) => settings.logging,
            Err(error) => {
                return output::emit_error(format, request_id, &error, terminal, Some(args), None)
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
    for warning in &args.deprecations {
        tracing::warn!(event = "arguments.deprecated", "{warning}");
    }
    tracing::debug!(event = "command.started", "Command started");
    let positional_pool = match &args.command {
        Command::Pool(command) => command.name(),
        _ => None,
    };
    if args
        .pool
        .as_ref()
        .zip(positional_pool)
        .is_some_and(|(flag, positional)| flag != positional)
    {
        return output::emit_error(
            format,
            request_id,
            &Error::new(
                ErrorCode::InvalidArgument,
                "The positional pool name conflicts with --pool",
            ),
            terminal,
            Some(args),
            None,
        );
    }
    let explicit_pool = positional_pool.or(args.pool.as_ref());
    if matches!(
        args.command,
        Command::Pool(args::PoolCommand::Destroy { .. })
    ) && explicit_pool.is_none()
    {
        return output::emit_error(
            format,
            request_id,
            &Error::new(
                ErrorCode::InvalidArgument,
                "pool delete requires an explicit NAME",
            ),
            terminal,
            Some(args),
            None,
        );
    }
    let reservation_selector = match &args.command {
        Command::Release {
            lease: selector,
            save,
            ..
        } if selector.task.is_some() || selector.lease_id.is_some() => {
            Some((selector.clone(), *save))
        }
        Command::Show { selector, .. }
            if selector.task.is_some() || selector.lease_id.is_some() =>
        {
            Some((
                args::LeaseSelector {
                    reservation: None,
                    task: selector.task.clone(),
                    lease_id: selector.lease_id,
                },
                false,
            ))
        }
        Command::Path { selector } if selector.task.is_some() || selector.lease_id.is_some() => {
            Some((selector.clone(), false))
        }
        _ => None,
    };
    let inferred_pool = if let Some((selector, saved)) = reservation_selector {
        match commands::reservation_pool(
            &args.repo,
            explicit_pool,
            &selector,
            saved,
            Duration::from_millis(args.git_timeout_ms),
        ) {
            Ok(name) => Some(name),
            Err(error) => {
                return output::emit_error(
                    format,
                    request_id,
                    &error,
                    terminal,
                    Some(args),
                    explicit_pool,
                )
            }
        }
    } else if explicit_pool.is_none()
        && !matches!(
            args.command,
            Command::Config(_)
                | Command::Help { .. }
                | Command::Completions { .. }
                | Command::Upgrade(_)
                | Command::Snapshot(_)
                | Command::Pool(args::PoolCommand::List { .. })
                | Command::Pool(args::PoolCommand::Init { .. })
                | Command::List {
                    slots: false,
                    details: false,
                    ..
                }
        )
    {
        let timeout = Duration::from_millis(args.git_timeout_ms);
        let inferred = if matches!(
            args.command,
            Command::Acquire { .. }
                | Command::Pool(
                    args::PoolCommand::Get { .. }
                        | args::PoolCommand::Set { .. }
                        | args::PoolCommand::Show { .. }
                )
        ) {
            commands::resolve_pool(&args.repo, timeout).map(Some)
        } else {
            groove_core::Pool::owning_pool(&args.repo, timeout)
        };
        match inferred {
            Ok(pool) => pool,
            Err(error) => {
                tracing::error!(event = "command.failed", file_only = true, code = %error.code,
                    elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "Command failed");
                return output::emit_error(format, request_id, &error, terminal, Some(args), None);
            }
        }
    } else {
        None
    };
    let selected_pool = explicit_pool.or(inferred_pool.as_ref());
    tracing::info!(event = "pool.selected", pool = %selected_pool.cloned().unwrap_or_default(), "Selected pool");
    let effective_timeout = if args.git_timeout_explicit {
        Ok(Duration::from_millis(args.git_timeout_ms))
    } else if matches!(args.command, Command::Pool(args::PoolCommand::Init { .. })) {
        preferences::load().map(|preferences| preferences.pool_defaults.git_timeout.duration())
    } else if let Some(name) = selected_pool.filter(|_| {
        !matches!(
            args.command,
            Command::Pool(args::PoolCommand::Destroy { .. })
        )
    }) {
        groove_core::Pool::open_retained_named(
            &args.repo,
            name,
            Duration::from_millis(args.git_timeout_ms),
        )
        .and_then(|pool| pool.settings())
        .map(|settings| settings.git_timeout.duration())
    } else {
        Ok(Duration::from_millis(args.git_timeout_ms))
    };
    let effective_timeout = match effective_timeout {
        Ok(timeout) => timeout,
        Err(error) => {
            return output::emit_error(
                format,
                request_id,
                &error,
                terminal,
                Some(args),
                selected_pool,
            )
        }
    };
    args.git_timeout_ms = effective_timeout.as_millis() as u64;
    let context = commands::Context {
        repo: &args.repo,
        selected_pool,
        git_timeout: effective_timeout,
    };
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let update_check = updates::start(args);
    match commands::run(&context, &args.command) {
        Ok(result) => {
            updates::finish(update_check, terminal);
            ready::trigger(args, selected_pool);
            tracing::info!(
                event = "command.completed",
                elapsed_ms = elapsed_ms(),
                "Command completed"
            );
            output::emit_success(format, request_id, result, terminal)
        }
        Err(error) => {
            updates::finish(update_check, terminal);
            if matches!(
                args.command,
                Command::Pool(args::PoolCommand::Destroy { dry_run: false, .. })
            ) {
                ready::restart(args, selected_pool);
            }
            tracing::error!(event = "command.failed", file_only = true, code = %error.code, elapsed_ms = elapsed_ms(), "Command failed");
            output::emit_error(
                format,
                request_id,
                &error,
                terminal,
                Some(args),
                selected_pool,
            )
        }
    }
}
