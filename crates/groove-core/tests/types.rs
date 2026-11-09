use groove_core::{CommitOid, Error, ErrorCode, LeaseId, SlotId};
use std::{collections::BTreeMap, error::Error as _};

#[test]
fn slot_ids_validate_paths_ranges_and_json_map_keys() {
    for (number, text) in [(1, "wt-0001"), (10, "wt-0010"), (4096, "wt-4096")] {
        let id = SlotId::new(number).unwrap();
        assert_eq!(id.as_str(), text);
        assert_eq!(text.parse::<SlotId>().unwrap(), id);
        assert_eq!(id.to_string(), text);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<SlotId>(&json).unwrap(), id);
        let map = BTreeMap::from([(id, 1u8)]);
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(
            serde_json::from_str::<BTreeMap<SlotId, u8>>(&json).unwrap(),
            map
        );
    }
    for text in [
        "",
        "admin",
        "../wt-0001",
        "wt-0001/..",
        "wt-1",
        "wt-0000",
        "wt-4097",
        "wt-9999",
        "wt-00001",
        "WT-0001",
        "wt-0001\n",
        "wt-0001\0",
        "wt-０００１",
    ] {
        assert_eq!(
            text.parse::<SlotId>().unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{text:?}"
        );
        assert!(serde_json::from_value::<SlotId>(serde_json::json!(text)).is_err());
        assert!(
            serde_json::from_value::<BTreeMap<SlotId, u8>>(serde_json::json!({text: 1})).is_err()
        );
    }
    assert!(SlotId::new(0).is_err());
    assert!(SlotId::new(4097).is_err());
}

#[test]
fn lease_ids_remain_uuid_strings_with_explicit_uuid_conversion() {
    let text = "00000000-0000-4000-8000-000000000001";
    let id: LeaseId = text.parse().unwrap();
    assert_eq!(id.to_string(), text);
    let uuid = uuid::Uuid::parse_str(text).unwrap();
    assert_eq!(id.as_uuid(), &uuid);
    assert_eq!(LeaseId::from(uuid), id);
    assert_eq!(uuid::Uuid::from(id), uuid);
    assert_eq!(serde_json::to_value(id).unwrap(), serde_json::json!(text));
    assert_eq!(
        serde_json::from_value::<LeaseId>(serde_json::json!(text)).unwrap(),
        id
    );
    assert_eq!(LeaseId::generate().as_uuid().get_version_num(), 4);
    let error = "wt-0001".parse::<LeaseId>().unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.source().unwrap().is::<uuid::Error>());
    assert!(serde_json::from_value::<LeaseId>(serde_json::json!("invalid")).is_err());
}

#[test]
fn commit_ids_validate_both_hash_formats_at_every_input_boundary() {
    for length in [40, 64] {
        let text = "a".repeat(length);
        let id: CommitOid = text.parse().unwrap();
        assert_eq!(id.as_str(), text);
        assert_eq!(id.to_string(), text);
        assert_eq!(serde_json::to_value(&id).unwrap(), serde_json::json!(text));
        assert_eq!(
            serde_json::from_value::<CommitOid>(serde_json::json!(text)).unwrap(),
            id
        );
    }
    for text in [
        "HEAD".into(),
        "a".repeat(39),
        "a".repeat(41),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(40),
        "A".repeat(40),
        format!("{}\n", "a".repeat(40)),
    ] {
        assert_eq!(
            text.parse::<CommitOid>().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert!(serde_json::from_value::<CommitOid>(serde_json::json!(text)).is_err());
    }
}

#[test]
fn error_codes_preserve_v1_wire_names_and_exit_categories() {
    use ErrorCode::*;
    for (code, text, exit) in [
        (InvalidArgument, "INVALID_ARGUMENT", 2),
        (UnsupportedConfiguration, "UNSUPPORTED_CONFIGURATION", 2),
        (PoolExhausted, "POOL_EXHAUSTED", 3),
        (AcquireTimeout, "ACQUIRE_TIMEOUT", 3),
        (NotInitialized, "NOT_INITIALIZED", 4),
        (ConfigConflict, "CONFIG_CONFLICT", 4),
        (InvalidRevision, "INVALID_REVISION", 4),
        (InvalidLease, "INVALID_LEASE", 5),
        (LeaseMismatch, "LEASE_MISMATCH", 5),
        (RecoveryFailed, "RECOVERY_FAILED", 6),
        (PoolUnhealthy, "POOL_UNHEALTHY", 6),
        (IoError, "IO_ERROR", 7),
        (GitError, "GIT_ERROR", 7),
        (ProtocolError, "PROTOCOL_ERROR", 7),
    ] {
        assert_eq!(code.as_str(), text);
        assert_eq!(code.to_string(), text);
        assert_eq!(serde_json::to_value(code).unwrap(), serde_json::json!(text));
        assert_eq!(
            serde_json::from_value::<ErrorCode>(serde_json::json!(text)).unwrap(),
            code
        );
        assert_eq!(Error::new(code, "message").exit_code(), exit);
    }
    assert!(serde_json::from_value::<ErrorCode>(serde_json::json!("TYPO")).is_err());
}

#[test]
fn errors_preserve_inspectable_sources_and_thread_safety() {
    let error = Error::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    assert_eq!(error.code, ErrorCode::IoError);
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let parsing = serde_json::from_str::<SlotId>("\"../unsafe\"").unwrap_err();
    let error = Error::from(parsing);
    assert_eq!(error.code, ErrorCode::ProtocolError);
    assert!(error.source().unwrap().is::<serde_json::Error>());
    let context = Error::new(ErrorCode::InvalidArgument, "context").with_source(error);
    assert!(context
        .source()
        .unwrap()
        .source()
        .unwrap()
        .is::<serde_json::Error>());
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Error>();
    send_sync::<SlotId>();
    send_sync::<LeaseId>();
    send_sync::<CommitOid>();
}
