#![no_main]
use groove_core::Project;
use libfuzzer_sys::fuzz_target;
use std::path::{Component, Path};

fuzz_target!(|input: &str| {
    // Pure parsing only: fuzzing must never execute project commands or touch
    // a real checkout. Match the production reader's 1 MiB input bound.
    if input.len() > 1024 * 1024 {
        return;
    }
    if let Ok(project) = Project::parse(input) {
        assert_eq!(project.schema_version, 1);
        for cache in project.caches.values() {
            for path in cache.paths.iter().chain(&cache.inputs) {
                assert!(!Path::new(path).is_absolute());
                assert!(path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))));
            }
        }
        for name in project.workspace.environment.keys() {
            assert!(!name.starts_with("GIT_") && !name.starts_with("GROOVE_"));
        }
    }
});
