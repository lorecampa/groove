//! Verify the distributed skill is complete without invoking a model.
#[path = "../../../crates/groove-cli/tests/support/mod.rs"]
mod support;

use serde_json::Value;
use std::{fs, path::Path};
use support::TestChild;

#[test]
fn plugin_install_payload_contains_the_complete_portable_skill_as_real_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let output = TestChild::spawn(
        support::command(root, "bash").args(["scripts/sync-plugins.sh", "--check"]),
    )
    .wait_with_output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    for harness in ["claude", "codex"] {
        let manifest = root.join(format!("plugins/groove/.{harness}-plugin/plugin.json"));
        let value: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
        assert_eq!(value["name"], "groove");
    }
    for path in [
        "plugins/groove/skills/groove/SKILL.md",
        "plugins/groove/skills/groove/references/project.md",
        "plugins/groove/skills/groove/references/recovery.md",
    ] {
        assert!(fs::symlink_metadata(root.join(path)).unwrap().is_file());
    }
}
