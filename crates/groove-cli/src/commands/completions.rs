//! Shell scripts generated from the same grammar used to parse commands.

use crate::{args::Args, output::Output};
use clap::CommandFactory;
use groove_core::{Error, ErrorCode, Result};

pub fn run(shell: clap_complete::Shell) -> Result<Output> {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Args::command(), "groove", &mut script);
    let text = String::from_utf8(script).map_err(|error| {
        Error::new(ErrorCode::ProtocolError, "Completion script is not UTF-8").with_source(error)
    })?;
    Output::text(serde_json::json!({ "text": text }), text)
}
