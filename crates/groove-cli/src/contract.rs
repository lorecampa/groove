//! Help's output reference comes from the schema exercised by conformance tests.
use crate::{args::Args, output, preferences};
use clap::CommandFactory;
use groove_core::ErrorCode;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};

const SCHEMA: &str = include_str!("../schemas/response-v1.schema.json");

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
        .get_after_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    if path == "config" || path == "config global" || path.starts_with("config global ") {
        text.push_str(include_str!("../help/config-global.txt"));
        text.push_str("\nPREFERENCE DEFAULTS (TOML, before environment overrides):\n");
        text.push_str(
            &toml::to_string_pretty(&preferences::Config::default())
                .expect("built-in preferences must serialize as TOML"),
        );
    }
    text.push_str(include_str!("../help/json.txt"));
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
    text.push_str("\nERROR EXIT CODES:\n");
    for value in schema["$defs"]["error"]["properties"]["code"]["enum"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Ok(code) = serde_json::from_value::<ErrorCode>(value.clone()) {
            text.push_str(&format!("  {}: {code}\n", output::error_status(code)));
        }
    }
    text.push_str("  7: stdout could not be written\n");
    command = command.after_help(text);
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
            if command.get_subcommands().next().is_none() || name == "history" {
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
