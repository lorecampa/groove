//! Help's output reference comes from the schema exercised by conformance tests.
use crate::{args::Args, output, preferences};
use clap::CommandFactory;
use groove_core::ErrorCode;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};

const SCHEMA: &str = include_str!("../schemas/response-v2.schema.json");

fn schema() -> Value {
    // The bundled source is checked by JSON Schema and CLI conformance tests.
    serde_json::from_str(SCHEMA).expect("bundled response schema must be valid JSON")
}

/// Build the actual parser, including schema-derived output help on every command.
pub fn command() -> clap::Command {
    let command = Args::command();
    let context: Vec<_> = command
        .get_arguments()
        .filter(|argument| matches!(argument.get_id().as_str(), "pool" | "git_timeout_ms"))
        .cloned()
        .collect();
    scope_context_help(decorate(command, ""), "", &context)
}

// Transitional context aliases belong to their commands, never the global set.
// Native command arguments take precedence and advertise the canonical spelling.
fn scope_context_help(
    mut command: clap::Command,
    path: &str,
    context: &[clap::Arg],
) -> clap::Command {
    for argument in context {
        let pool = argument.get_id() == "pool";
        let visible = if pool {
            matches!(
                path,
                "acquire"
                    | "release"
                    | "show"
                    | "list"
                    | "pool create"
                    | "pool show"
                    | "pool get"
                    | "pool set"
                    | "pool delete"
            )
        } else {
            matches!(
                path.split(' ').next(),
                Some("acquire" | "release" | "show" | "list" | "pool" | "snapshot")
            )
        };
        if command.get_arguments().any(|native| {
            native.get_id() != argument.get_id()
                && (native.get_long() == argument.get_long()
                    || native.get_all_aliases().is_some_and(|aliases| {
                        argument
                            .get_long()
                            .is_some_and(|long| aliases.contains(&long))
                    }))
        }) {
            continue;
        }
        let argument = argument.clone().hide(true).global(false);
        if command
            .get_arguments()
            .any(|existing| existing.get_id() == argument.get_id())
        {
            let id = argument.get_id().clone();
            command = command.mut_arg(id, |_| argument);
        } else if visible {
            command = command.arg(argument);
        }
    }
    let children: Vec<_> = command.get_subcommands().cloned().collect();
    for child in children {
        let name = child.get_name().to_owned();
        let child_path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path} {name}")
        };
        command = command.mut_subcommand(name, |_| scope_context_help(child, &child_path, context));
    }
    command
}

fn decorate(mut command: clap::Command, path: &str) -> clap::Command {
    let text = format!(
        "{}\nHelp topics: groove help json|exit-codes|durations|recovery|environment\n",
        concise_help(path)
    );
    command = command.after_long_help(text.clone()).after_help(text);
    let children: Vec<_> = command
        .get_subcommands()
        .map(|child| {
            let child_path = if path.is_empty() {
                child.get_name().to_owned()
            } else {
                format!("{path} {}", child.get_name())
            };
            decorate(child.clone(), &child_path)
        })
        .collect();
    for child in children {
        let name = child.get_name().to_owned();
        command = command.mut_subcommand(name, |_| child);
    }
    command
}

fn concise_help(path: &str) -> &'static str {
    match path {
        "help" => "EXAMPLES:\n  groove help json acquire\n  groove help recovery release\n  groove help pool create\n",
        "path" => "EXAMPLES:\n  groove path fix-nav\n  groove path  # infer from the current workspace\n",
        "upgrade" => "EXAMPLES:\n  groove upgrade --check\n  groove upgrade --dry-run --with-plugins\n  groove upgrade --to 0.6.0\n",
        "" => include_str!("../help/short/overview.txt"),
        "pool" => include_str!("../help/short/pool.txt"),
        "pool create" => include_str!("../help/short/pool-init.txt"),
        "pool show" => include_str!("../help/short/pool-show.txt"),
        "pool set" => include_str!("../help/short/pool-set.txt"),
        "acquire" => include_str!("../help/short/acquire.txt"),
        "release" => include_str!("../help/short/release.txt"),
        "pool delete" => include_str!("../help/short/pool-destroy.txt"),
        "pool list" => include_str!("../help/short/pool-list.txt"),
        "list" => include_str!("../help/short/list.txt"),
        "show" => include_str!("../help/short/show.txt"),
        "completions" => include_str!("../help/short/completions.txt"),
        "snapshot" => include_str!("../help/short/snapshot.txt"),
        "snapshot save" => include_str!("../help/short/snapshot-save.txt"),
        "snapshot list" => include_str!("../help/short/snapshot-list.txt"),
        "snapshot show" => include_str!("../help/short/snapshot-show.txt"),
        "snapshot storage" => include_str!("../help/short/snapshot-storage.txt"),
        "snapshot prune" => include_str!("../help/short/snapshot-prune.txt"),
        "snapshot diff" => include_str!("../help/short/snapshot-diff.txt"),
        "snapshot restore" => include_str!("../help/short/snapshot-restore.txt"),
        "snapshot delete" => include_str!("../help/short/snapshot-delete.txt"),
        "config" => include_str!("../help/short/config.txt"),
        "config show" => include_str!("../help/short/config-show.txt"),
        "config get" => include_str!("../help/short/config-get.txt"),
        "pool get" => include_str!("../help/short/pool-get.txt"),
        "config set" => include_str!("../help/short/config-set.txt"),
        "config path" => include_str!("../help/short/config-path.txt"),
        "config edit" => include_str!("../help/short/config-edit.txt"),
        "config unset" => include_str!("../help/short/config-unset.txt"),
        _ => "",
    }
}

/// Installed reference topics and ordinary command help, without repository I/O.
pub fn help_topic(parts: &[String]) -> groove_core::Result<String> {
    let invalid = |message| groove_core::Error::new(ErrorCode::InvalidArgument, message);
    let topic = parts.first().map(String::as_str).unwrap_or("");
    match topic {
        "json" => {
            let schema = schema();
            let filter = parts[1..].join(" ");
            let mut text = include_str!("../help/json.txt").to_owned();
            let mut definitions = BTreeSet::new();
            for (name, types) in schema["x-groove-command-results"]
                .as_object()
                .expect("validated command schema")
            {
                if filter.is_empty() || name == &filter || name.starts_with(&format!("{filter} ")) {
                    let types: Vec<_> = types
                        .as_array()
                        .expect("validated result list")
                        .iter()
                        .filter_map(Value::as_str)
                        .collect();
                    text.push_str(&format!("\n{name}: {}\n", types.join(" | ")));
                    definitions.extend(types.into_iter().map(str::to_owned));
                }
            }
            if definitions.is_empty() {
                return Err(invalid(format!("Unknown command contract: {filter}")));
            }
            append_definitions(&mut text, definitions, &schema);
            Ok(text)
        }
        "exit-codes" => {
            let mut text = "Exit codes are stable categories; branch on error.code for the specific failure.\n".to_owned();
            for value in schema()["$defs"]["error"]["properties"]["code"]["enum"]
                .as_array()
                .expect("validated error codes")
            {
                let library_name = match value.as_str() {
                    Some("INVALID_RESERVATION") => Value::String("INVALID_LEASE".into()),
                    Some("RESERVATION_MISMATCH") => Value::String("LEASE_MISMATCH".into()),
                    _ => value.clone(),
                };
                if let Ok(code) = serde_json::from_value::<ErrorCode>(library_name) {
                    text.push_str(&format!(
                        "{}: {}\n",
                        output::error_status(code),
                        output::cli_error_code(code)
                    ));
                }
            }
            text.push_str("0: success\n7: stdout could not be written\nSignals may terminate a process before it emits JSON.\n");
            Ok(text)
        }
        "durations" => Ok(include_str!("../help/durations.txt").into()),
        "environment" => Ok(format!(
            "{}\nPreference defaults (TOML storage):\n{}",
            include_str!("../help/environment.txt"),
            toml::to_string_pretty(&preferences::Config::default())
                .expect("default preferences serialize")
        )),
        "recovery" if parts.len() == 1 => Ok(include_str!("../help/recovery.txt").into()),
        _ => {
            let recovery = topic == "recovery";
            let mut command = if recovery { Args::command() } else { command() };
            command.build();
            let path = if recovery { &parts[1..] } else { parts };
            for component in path {
                command = command.find_subcommand(component).cloned().ok_or_else(|| {
                    invalid(format!(
                        "Unknown command or help topic: {}",
                        parts.join(" ")
                    ))
                })?;
            }
            if recovery {
                Ok(command
                    .get_after_long_help()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| include_str!("../help/recovery.txt").into()))
            } else {
                Ok(command.render_long_help().to_string())
            }
        }
    }
}

fn references(value: &Value, pending: &mut VecDeque<String>) {
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|name| name.strip_prefix("#/$defs/"))
            {
                pending.push_back(name.to_owned());
            }
            for value in object.values() {
                references(value, pending);
            }
        }
        Value::Array(values) => {
            for value in values {
                references(value, pending);
            }
        }
        _ => {}
    }
}

fn append_definitions(text: &mut String, definitions: BTreeSet<String>, schema: &Value) {
    let mut pending: VecDeque<_> = definitions
        .into_iter()
        .filter(|name| name != "error")
        .collect();
    pending.push_back("error".to_owned());
    let mut seen = BTreeSet::new();
    text.push_str("\nOUTPUT TYPES (required unless marked optional; named types follow):\n");
    while let Some(name) = pending.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let definition = schema
            .pointer(&format!("/$defs/{name}"))
            .expect("schema references validated by contract tests");
        text.push_str(&format!("\n{name}: "));
        render(definition, 1, text);
        references(definition, &mut pending);
    }
}

fn render(schema: &Value, depth: usize, text: &mut String) {
    let indent = "  ".repeat(depth);
    if let Some(reference) = schema["$ref"].as_str() {
        text.push_str(reference.strip_prefix("#/$defs/").unwrap_or(reference));
        text.push('\n');
        return;
    }
    for (key, label) in [
        ("anyOf", "any of"),
        ("oneOf", "exactly one of"),
        ("allOf", "all of"),
    ] {
        if let Some(variants) = schema[key].as_array() {
            text.push_str(&format!("{label}:\n"));
            for variant in variants {
                text.push_str(&format!("{indent}- "));
                render(variant, depth + 1, text);
            }
            return;
        }
    }
    if let Some(value) = schema.get("const") {
        text.push_str(&format!("{value}"));
    } else if let Some(values) = schema["enum"].as_array() {
        text.push_str(
            &values
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(" | "),
        );
    } else {
        match schema.get("type") {
            Some(Value::String(kind)) => text.push_str(kind),
            Some(Value::Array(kinds)) => text.push_str(
                &kinds
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            _ => text.push_str(if schema.get("properties").is_some() {
                "object"
            } else {
                "any JSON value"
            }),
        }
    }
    // Preserve validation constraints without separately maintained type tables.
    if let Some(object) = schema.as_object() {
        for (key, value) in object {
            if ![
                "type",
                "enum",
                "const",
                "properties",
                "required",
                "items",
                "description",
                "title",
            ]
            .contains(&key.as_str())
            {
                text.push_str(&format!("; {key}={value}"));
            }
        }
    }
    text.push('\n');
    if let Some(description) = schema["description"].as_str() {
        text.push_str(&format!("{indent}{description}\n"));
    }
    if let Some(properties) = schema["properties"].as_object() {
        for (name, value) in properties {
            let required = schema["required"]
                .as_array()
                .is_some_and(|names| names.iter().any(|required| required == name));
            text.push_str(&format!(
                "{indent}{name}{}: ",
                if required { "" } else { " (optional)" }
            ));
            render(value, depth + 1, text);
        }
    }
    if let Some(items) = schema.get("items") {
        text.push_str(&format!("{indent}items: "));
        render(items, depth + 1, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_resolvable_output_types() {
        let schema = schema();
        let mapping = schema["x-groove-command-results"].as_object().unwrap();
        fn visit(
            command: &clap::Command,
            prefix: &str,
            mapping: &serde_json::Map<String, Value>,
            paths: &mut BTreeSet<String>,
        ) {
            let name = if prefix.is_empty() {
                command.get_name().to_owned()
            } else {
                format!("{prefix} {}", command.get_name())
            };
            paths.insert(name.clone());
            for alias in command.get_all_aliases() {
                paths.insert(if prefix.is_empty() {
                    alias.to_owned()
                } else {
                    format!("{prefix} {alias}")
                });
            }
            if command.get_subcommands().next().is_none() {
                assert!(
                    mapping.contains_key(&name),
                    "missing output contract for {name}"
                );
            }
            for child in command.get_subcommands() {
                visit(child, &name, mapping, paths);
            }
        }
        let mut paths = BTreeSet::new();
        for command in Args::command().get_subcommands() {
            visit(command, "", mapping, &mut paths);
        }
        for (command, definitions) in mapping {
            assert!(
                paths.contains(command) || ["help", "version"].contains(&command.as_str()),
                "stale output contract for {command}"
            );
            assert!(!definitions.as_array().unwrap().is_empty(), "{command}");
            for name in definitions.as_array().unwrap() {
                let mut pending = VecDeque::from([name.as_str().unwrap().to_owned()]);
                let mut seen = BTreeSet::new();
                while let Some(name) = pending.pop_front() {
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    let definition = schema
                        .pointer(&format!("/$defs/{name}"))
                        .unwrap_or_else(|| panic!("{command}: unresolved {name}"));
                    references(definition, &mut pending);
                }
            }
        }
        command().debug_assert();
    }
}
