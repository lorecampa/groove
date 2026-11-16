//! Explicit snapshot retention, with preview as the default.
use crate::output::{table, Output};
use groove_core::{
    snapshot::{PruneOptions, PruneRetentionReason, Snapshots},
    Result,
};
use serde_json::json;
use std::time::Duration;

pub(super) fn run(
    service: &Snapshots,
    older_than_ms: u64,
    keep_last: usize,
    apply: bool,
) -> Result<Output> {
    let result = service.prune(PruneOptions {
        older_than: Duration::from_millis(older_than_ms),
        keep_last,
        apply,
    })?;
    let action = if apply { "deleted" } else { "would delete" };
    let mut rows: Vec<Vec<String>> = result
        .selected
        .iter()
        .map(|id| vec![id.clone(), action.into()])
        .collect();
    rows.extend(result.retained.iter().map(|retained| {
        let reason = match retained.reason {
            PruneRetentionReason::Pinned => "retained: pinned",
            PruneRetentionReason::ProtectedWorkspace => "retained: protected workspace",
            PruneRetentionReason::KeepLast => "retained: among newest in originating workspace",
            PruneRetentionReason::TooRecent => "retained: too recent",
        };
        vec![retained.id.clone(), reason.into()]
    }));
    let mut text = format!(
        "{} snapshots {action}; keeping the newest {keep_last} per originating workspace.\n",
        result.selected.len(),
    );
    if !rows.is_empty() {
        text.push_str(&table(&["SNAPSHOT", "ACTION"], &rows));
        text.push('\n');
    }
    if !apply {
        text.push_str(
            "Preview only. Repeat with --apply to recompute and delete eligible snapshots.\n",
        );
    }
    text.push_str("Deletion removes references. Git garbage collection may reclaim unused objects later; shared objects remain.\n");
    Output::text(json!({ "prune": result }), text)
}
