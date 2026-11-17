//! The release pipeline must validate and build the same immutable revision.
#[test]
fn publication_requires_the_complete_matrix_on_the_validated_commit() {
    let publish = include_str!("../../../.github/workflows/publish.yml");
    let ci = include_str!("../../../.github/workflows/ci.yml");
    // Job bodies contain more-indented lines; split only another two-space key.
    let body = |name: &str| {
        let start = format!("\n  {name}:\n");
        publish
            .split_once(&start)
            .unwrap()
            .1
            .lines()
            .take_while(|line| !line.starts_with("  ") || line.starts_with("   "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(body("validate").contains("commit=$(git rev-parse HEAD)"));
    for name in ["checks", "binaries", "publish", "announce"] {
        assert!(
            body(name).contains("needs.validate.outputs.commit"),
            "{name} does not use the validated commit"
        );
    }
    assert!(body("publish").contains("needs: [validate, checks, binaries]"));
    assert!(body("announce").contains("needs.checks.result == 'success'"));
    assert!(body("announce").contains("needs.binaries.result == 'success'"));
    assert!(ci.contains("  pull_request:\n"));
    assert!(ci.contains("    branches: [main]"));
    assert!(ci.contains("os: [macos-15, macos-15-intel]"));
    assert!(ci.contains("rust: ['1.89.0', stable]"));
}

#[test]
fn case_sensitive_checks_keep_compiler_and_capture_scratch_off_the_test_volume() {
    let ci = include_str!("../../../.github/workflows/ci.yml");
    let fixture_commands: Vec<_> = ci
        .lines()
        .filter(|line| line.contains("GROOVE_TEST_ROOT=") && line.contains("cargo test"))
        .collect();
    assert_eq!(fixture_commands.len(), 2);
    for command in fixture_commands {
        assert!(
            command.contains("GROOVE_TEST_ROOT=\"$mount/scratch\""),
            "{command}"
        );
        assert!(!command.contains("TMPDIR="), "Compiler and harness scratch must not compete with managed APFS fixture data: {command}");
    }
}
