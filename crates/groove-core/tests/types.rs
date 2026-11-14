use groove_core::{CommitOid, Error, ErrorCode, LeaseId, PoolId, SlotId};
use std::{collections::BTreeMap, error::Error as _};

#[test]
fn pool_names_are_portable_and_cannot_escape_their_namespace() {
    use groove_core::PoolName;
    for name in [
        "default",
        "frontend",
        "ci-12",
        "backend_warm",
        &"a".repeat(64),
    ] {
        let parsed: PoolName = name.parse().unwrap();
        assert_eq!(parsed.as_str(), name);
        assert_eq!(parsed.to_string(), name);
        assert_eq!(String::from(parsed.clone()), name);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), name);
    }
    for name in [
        "",
        "../other",
        ".",
        "..",
        "/tmp/pool",
        "a/b",
        "a\\b",
        "FRONTEND",
        "1ci",
        "a b",
        "a\n",
        "cafè",
        &"a".repeat(65),
    ] {
        assert_eq!(
            name.parse::<PoolName>().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert!(
            serde_json::from_value::<PoolName>(serde_json::json!(name)).is_err(),
            "{name:?}"
        );
    }
    assert_eq!(PoolName::default().as_str(), "default");
}

#[test]
fn pool_ids_accept_only_uuid_v4_and_preserve_their_wire_identity() {
    let text = "00000000-0000-4000-8000-000000000001";
    let id: PoolId = text.parse().unwrap();
    assert_eq!(id.to_string(), text);
    assert_eq!(String::from(id), text);
    assert_eq!(PoolId::try_from(text.to_owned()).unwrap(), id);
    assert_eq!(serde_json::to_value(id).unwrap(), serde_json::json!(text));
    assert_eq!(
        serde_json::from_value::<PoolId>(serde_json::json!(text)).unwrap(),
        id
    );
    for invalid in [
        "00000000-0000-0000-0000-000000000000",
        "00000000-0000-1000-8000-000000000001",
        "00000000-0000-3000-8000-000000000001",
        "00000000-0000-5000-8000-000000000001",
        "00000000-0000-7000-8000-000000000001",
        "not-a-uuid",
    ] {
        assert_eq!(
            invalid.parse::<PoolId>().unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{invalid}"
        );
        assert_eq!(
            PoolId::try_from(invalid.to_owned()).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{invalid}"
        );
        assert!(
            serde_json::from_value::<PoolId>(serde_json::json!(invalid)).is_err(),
            "{invalid}"
        );
    }
    let malformed = "not-a-uuid".parse::<PoolId>().unwrap_err();
    assert!(malformed.source().unwrap().is::<uuid::Error>());
}

#[test]
fn historical_unnamed_results_are_read_as_the_default_namespace() {
    use groove_core::{DestroyResult, InitResult, PoolName, PoolSettings};
    let settings = serde_json::json!({"capacity":1, "cleanup_policy":"pristine", "state_dir":"/repo/.git/groove", "worktree_root":"/repo/.git/groove/worktrees"});
    let decoded: PoolSettings = serde_json::from_value(settings.clone()).unwrap();
    assert_eq!(decoded.pool_name, PoolName::default());
    let mut init = settings.clone();
    init["created"] = true.into();
    init["common_git_dir"] = "/repo/.git".into();
    init["slots"] = serde_json::json!(["wt-0001"]);
    assert_eq!(
        serde_json::from_value::<InitResult>(init)
            .unwrap()
            .pool_name,
        PoolName::default()
    );
    let destroy = serde_json::json!({"pool_id":null,"dry_run":true,"deleted":false,"already_deleted":false,"resumed":false,"state_dir":"/repo/.git/groove","worktree_root":"/repo/.git/groove/worktrees","slots":["wt-0001"],"artifacts":[]});
    assert_eq!(
        serde_json::from_value::<DestroyResult>(destroy)
            .unwrap()
            .pool_name,
        PoolName::default()
    );
    let mut named = settings;
    named["pool_name"] = "frontend".into();
    assert_eq!(
        serde_json::from_value::<PoolSettings>(named)
            .unwrap()
            .pool_name
            .as_str(),
        "frontend"
    );
}

#[test]
fn slot_ids_validate_paths_ranges_and_json_map_keys() {
    for (number, text) in [(1, "wt-0001"), (10, "wt-0010"), (4096, "wt-4096")] {
        let id = SlotId::new(number).unwrap();
        assert_eq!(id.as_str(), text);
        assert_eq!(usize::from(id.number()), number);
        assert_eq!(id.as_ref(), text);
        assert_eq!(String::from(id.clone()), text);
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
    assert_eq!(SlotId::new(0).unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(
        SlotId::new(4097).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
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
        let text = "0123456789abcdef".repeat(4)[..length].to_owned();
        let id: CommitOid = text.parse().unwrap();
        assert_eq!(id.as_str(), text);
        assert_eq!(id.as_ref(), text);
        assert_eq!(String::from(id.clone()), text);
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
        "0".repeat(40).replacen('0', "/", 1),
        "0".repeat(40).replacen('0', ":", 1),
        "a".repeat(40).replacen('a', "`", 1),
        "a".repeat(40).replacen('a', "g", 1),
    ] {
        assert_eq!(
            text.parse::<CommitOid>().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert!(serde_json::from_value::<CommitOid>(serde_json::json!(text)).is_err());
    }
}

#[test]
fn error_codes_preserve_v1_wire_names() {
    use ErrorCode::*;
    for (code, text) in [
        (InvalidArgument, "INVALID_ARGUMENT"),
        (UnsupportedConfiguration, "UNSUPPORTED_CONFIGURATION"),
        (PoolExhausted, "POOL_EXHAUSTED"),
        (AcquireTimeout, "ACQUIRE_TIMEOUT"),
        (NotInitialized, "NOT_INITIALIZED"),
        (ConfigConflict, "CONFIG_CONFLICT"),
        (InvalidRevision, "INVALID_REVISION"),
        (InvalidLease, "INVALID_LEASE"),
        (LeaseMismatch, "LEASE_MISMATCH"),
        (RecoveryFailed, "RECOVERY_FAILED"),
        (PoolUnhealthy, "POOL_UNHEALTHY"),
        (IoError, "IO_ERROR"),
        (GitError, "GIT_ERROR"),
        (ProtocolError, "PROTOCOL_ERROR"),
    ] {
        assert_eq!(code.as_str(), text);
        assert_eq!(code.to_string(), text);
        assert_eq!(serde_json::to_value(code).unwrap(), serde_json::json!(text));
        assert_eq!(
            serde_json::from_value::<ErrorCode>(serde_json::json!(text)).unwrap(),
            code
        );
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
