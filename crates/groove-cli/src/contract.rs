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
    decorate(Args::command(), "", &schema())
}

fn decorate(mut command: clap::Command, path: &str, schema: &Value) -> clap::Command {
    let mut text = command
        .get_after_long_help()
        .map(ToString::to_string)
        .unwrap_or_else(|| concise_help(path).to_owned());
    if path == "config" {
        text.push_str("\nPREFERENCE DEFAULTS (TOML, before environment overrides):\n");
        text.push_str(
            &toml::to_string_pretty(&preferences::Config::default())
                .expect("built-in preferences must serialize as TOML"),
        );
    }
    if path.starts_with("config ") {
        text.push_str("\nSee groove config --help for preference keys and defaults.\n");
    }
    if path.is_empty() {
        text.push_str(include_str!("../help/json.txt"));
    } else {
        text.push_str(
            "\nOUTPUT CONTRACT:\nSee groove --help for global output rules and exit codes.\n",
        );
    }
    let mut definitions = BTreeSet::new();
    if !path.is_empty() {
        if let Some(commands) = schema["x-groove-command-results"].as_object() {
            for (name, names) in commands {
                if name == path || name.starts_with(&format!("{path} ")) {
                    let names: Vec<_> = names
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    text.push_str(&format!("  {name} -> result: {}\n", names.join(" | ")));
                    definitions.extend(names.into_iter().map(str::to_owned));
                }
            }
        }
        definitions.insert("error".into());
        append_definitions(&mut text, definitions, schema);
    } else {
        text.push_str("Run groove COMMAND --help for that command's result fields and types.\n");
    }
    if path.is_empty() {
        text.push_str("\nERROR EXIT CODES:\n");
        for value in schema["$defs"]["error"]["properties"]["code"]["enum"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let library_name = match value.as_str() {
                Some("INVALID_RESERVATION") => Value::String("INVALID_LEASE".into()),
                Some("RESERVATION_MISMATCH") => Value::String("LEASE_MISMATCH".into()),
                _ => value.clone(),
            };
            if let Ok(code) = serde_json::from_value::<ErrorCode>(library_name) {
                text.push_str(&format!(
                    "  {}: {}\n",
                    output::error_status(code),
                    output::cli_error_code(code)
                ));
            }
        }
        text.push_str("  7: stdout could not be written\n");
    }
    let invocation = if path.is_empty() {
        "groove".to_owned()
    } else {
        format!("groove {path}")
    };
    text.push_str(&format!("\nRun {invocation} -h for concise help.\n"));
    command = command.after_long_help(text).after_help(format!(
        "{}\nRun {invocation} --help for detailed help, recovery rules and output types.",
        concise_help(path)
    ));
    let children: Vec<_> = command
        .get_subcommands()
        .map(|child| {
            let child_path = if path.is_empty() {
                child.get_name().to_owned()
            } else {
                format!("{path} {}", child.get_name())
            };
            decorate(child.clone(), &child_path, schema)
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
        "" => include_str!("../help/short/overview.txt"),
        "pool" => include_str!("../help/short/pool.txt"),
        "pool init" => include_str!("../help/short/pool-init.txt"),
        "pool show" => include_str!("../help/short/pool-show.txt"),
        "pool set" => include_str!("../help/short/pool-set.txt"),
        "acquire" => include_str!("../help/short/acquire.txt"),
        "release" => include_str!("../help/short/release.txt"),
        "pool destroy" => include_str!("../help/short/pool-destroy.txt"),
        "pool list" => include_str!("../help/short/pool-list.txt"),
        "list" => include_str!("../help/short/list.txt"),
        "show" => include_str!("../help/short/show.txt"),
        "completions" => include_str!("../help/short/completions.txt"),
        "snapshot" => include_str!("../help/short/snapshot.txt"),
        "snapshot save" => include_str!("../help/short/snapshot-save.txt"),
        "snapshot list" => include_str!("../help/short/snapshot-list.txt"),
        "snapshot show" => include_str!("../help/short/snapshot-show.txt"),
        "snapshot storage" => include_str!("../help/short/snapshot-storage.txt"),
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
