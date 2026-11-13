//! `groove config`: pool settings and preferences.

use super::Context;
use crate::{
    args::{ConfigCommand, GlobalCommand},
    output::{fields, Output},
    preferences::{self, Change},
};
use groove_core::{PoolUpdate, Result};
use serde_json::json;

pub fn run(context: &Context<'_>, command: &ConfigCommand) -> Result<Output> {
    match command {
        ConfigCommand::Show => {
            let settings = context.pool()?.settings()?;
            Output::fields(json!({ "pool": settings }))
        }
        ConfigCommand::Set { capacity, cleanup } => {
            let result = context.pool()?.configure(PoolUpdate {
                capacity: *capacity,
                cleanup_policy: cleanup.map(Into::into),
                timeout: context.git_timeout,
            })?;
            let headline = if result.updated {
                "Pool settings updated."
            } else {
                "Pool settings unchanged."
            };
            let text = format!(
                "{headline}\n{}",
                fields(&serde_json::to_value(&result.pool)?, 1)
            );
            Output::text(result, text)
        }
        ConfigCommand::Global(command) => {
            let result = match command {
                GlobalCommand::Show => preferences::show()?,
                GlobalCommand::Path => json!({ "config_path": preferences::path()? }),
                GlobalCommand::Edit => preferences::change(Change::Edit)?,
                GlobalCommand::Set { settings } => preferences::change(Change::Update(
                    settings
                        .iter()
                        .map(|(key, value)| (key.clone(), Some(value.clone())))
                        .collect(),
                ))?,
                GlobalCommand::Unset { keys } => preferences::change(Change::Update(
                    keys.iter().map(|key| (key.clone(), None)).collect(),
                ))?,
            };
            Output::fields(result)
        }
    }
}
