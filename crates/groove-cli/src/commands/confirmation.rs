//! Shared confirmation policy for operations deleting collections of objects.
use groove_core::{Error, ErrorCode, Result};
use std::io::{self, IsTerminal, Write};

pub(super) fn confirm(yes: bool, question: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Deletion requires --yes without a terminal; use --dry-run to preview",
        ));
    }
    write!(io::stderr(), "{question} [y/N] ")?;
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InvalidArgument,
            "Deletion cancelled; no objects were removed",
        ))
    }
}
