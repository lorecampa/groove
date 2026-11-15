//! `groove config`: user preferences.

use super::Context;
use crate::{
    args::ConfigCommand,
    output::Output,
    preferences::{self, Change},
};
use groove_core::Result;
use serde_json::json;

pub fn run(_context: &Context<'_>, command: &ConfigCommand) -> Result<Output> {
    let result = match command {
        ConfigCommand::Get { key } => return setting_output(key, preferences::get(key)?),
        ConfigCommand::Show => preferences::show()?,
        ConfigCommand::Path => json!({ "config_path": preferences::path()? }),
        ConfigCommand::Edit => preferences::change(Change::Edit)?,
        ConfigCommand::Set { settings } => preferences::change(Change::Update(
            settings
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect(),
        ))?,
        ConfigCommand::Unset { keys } => preferences::change(Change::Update(
            keys.iter().map(|key| (key.clone(), None)).collect(),
        ))?,
    };
    Output::fields(result)
}

pub(super) fn setting_output(key: &str, value: serde_json::Value) -> Result<Output> {
    let text = if value.is_null() {
        String::new()
    } else {
        crate::output::scalar_text(&value)
    };
    Output::text(json!({"key": key, "value": value}), format!("{text}\n"))
}
