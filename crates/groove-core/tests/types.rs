use groove_core::{CommitOid, Error, ErrorCode, LeaseId, SlotId};
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

#[test]
fn shell_status_is_bounded_for_public_execution_result_values() {
    use groove_core::{ExecResult, ExecutionInfo, Termination};
    let lease_id = LeaseId::generate();
    let mut result = ExecResult {
        lease_id,
        run: ExecutionInfo {
            schema_version: 1,
            lease_id,
            run_id: "00000000-0000-4000-8000-000000000001".into(),
            program: "fixture".into(),
            started_at_unix_ms: 0,
            leader_pid: None,
            control_name: String::new(),
            stdout_path: Default::default(),
            stderr_path: Default::default(),
        },
        termination: Termination::Exited,
        exit_code: None,
        signal: None,
        cancel_signal: None,
    };
    // Public/deserialized result fields need not come from a real OS status.
    for (termination, code, signal, cancel, expected) in [
        (Termination::Exited, Some(0), None, None, 0),
        (Termination::Exited, Some(23), None, None, 23),
        (Termination::Exited, Some(i32::MIN), None, None, 0),
        (Termination::Exited, Some(i32::MAX), None, None, 255),
        (Termination::Exited, None, Some(libc::SIGTERM), None, 143),
        (Termination::Exited, None, Some(i32::MAX), None, 255),
        (Termination::Exited, None, Some(i32::MIN), None, 0),
        (Termination::Exited, None, None, None, 1),
        (Termination::Cancelled, None, None, Some(i32::MAX), 255),
        (Termination::Cancelled, None, None, Some(i32::MIN), 0),
        (Termination::Cancelled, None, None, None, 143),
        (Termination::TimedOut, None, None, None, 124),
        (Termination::OutputLimited, None, None, None, 1),
    ] {
        result.termination = termination;
        result.exit_code = code;
        result.signal = signal;
        result.cancel_signal = cancel;
        assert_eq!(result.shell_status(), expected, "{result:?}");
    }
}
