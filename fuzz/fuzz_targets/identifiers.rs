#![no_main]
use groove_core::{PoolName, SlotId};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &str| {
    // Independent path-safety contracts, also exercised through deserialization.
    if let Ok(name) = input.parse::<PoolName>() {
        assert!((1..=64).contains(&input.len()));
        assert!(input.as_bytes()[0].is_ascii_lowercase());
        assert!(input
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'));
        assert_eq!(std::path::Path::new(name.as_str()).components().count(), 1);
    }
    if let Ok(slot) = input.parse::<SlotId>() {
        assert!(input.starts_with("wt-"));
        assert!((1..=4096).contains(&slot.number()));
        assert_eq!(std::path::Path::new(slot.as_str()).components().count(), 1);
    }
    if let Ok(value) = serde_json::from_str::<PoolName>(input) {
        assert!(value.as_str().parse::<PoolName>().is_ok());
        assert!(!value.as_str().contains(['/', '\\', '.']));
    }
    if let Ok(value) = serde_json::from_str::<SlotId>(input) {
        assert!((1..=4096).contains(&value.number()));
    }
});
