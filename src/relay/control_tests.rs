use super::*;
use crate::hooks::test_helpers::isolated_test_env;
use serde_json::json;

fn test_db() -> HcomDb {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    std::mem::forget(dir);
    db
}

fn latest_rpc_result(db: &HcomDb) -> serde_json::Value {
    let payload: String = db
        .conn()
        .query_row(
            "SELECT data FROM events WHERE type = 'rpc_result' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&payload).unwrap()
}

#[test]
fn test_split_device_suffix_valid() {
    assert_eq!(split_device_suffix("luna:ABCD"), Some(("luna", "ABCD")));
    assert_eq!(
        split_device_suffix("myagent:XY12"),
        Some(("myagent", "XY12"))
    );
    assert_eq!(split_device_suffix("a:B3C4"), Some(("a", "B3C4")));
    // All digits
    assert_eq!(split_device_suffix("foo:1234"), Some(("foo", "1234")));
}

#[test]
fn test_split_device_suffix_invalid() {
    // Lowercase in suffix
    assert_eq!(split_device_suffix("luna:abcd"), None);
    // Mixed case
    assert_eq!(split_device_suffix("luna:ABCd"), None);
    // Too short
    assert_eq!(split_device_suffix("luna:ABC"), None);
    // Too long
    assert_eq!(split_device_suffix("luna:ABCDE"), None);
    // No colon
    assert_eq!(split_device_suffix("luna"), None);
    // tag:value pattern should not match
    assert_eq!(split_device_suffix("tag:something"), None);
    // Hyphen in suffix
    assert_eq!(split_device_suffix("foo:AB-D"), None);
    // Empty base
    assert_eq!(split_device_suffix(":ABCD"), Some(("", "ABCD")));
}

#[test]
fn test_handle_control_events_filters_by_target() {
    // Control events targeting a different device should be ignored
    let events = vec![json!({
        "type": "control",
        "ts": 1000.0,
        "data": {
            "action": "kill",
            "target_device": "ABCD",
            "from": "_:EFGH",
            "request_id": "req-other-device",
            "params": {
                "target": "luna",
            }
        }
    })];

    // own_short_id is "WXYZ" — event targets "ABCD", so nothing should happen
    let db = test_db();
    assert!(!handle_control_events(&db, &events, "WXYZ", "device-123"));

    // No crash, no panic — event was filtered
}

#[test]
fn test_resolve_remote_cwd_rejects_missing_requested_directory() {
    let err = resolve_remote_cwd(Some("/definitely/missing")).unwrap_err();
    assert!(err.contains("requested cwd does not exist or is not a directory"));
}

#[test]
fn test_resolve_remote_cwd_rejects_empty() {
    let err = resolve_remote_cwd(None).unwrap_err();
    assert!(err.contains("--dir"));
    let err = resolve_remote_cwd(Some("")).unwrap_err();
    assert!(err.contains("--dir"));
}

#[test]
#[serial_test::serial]
fn test_build_rpc_control_payload_includes_request_id_and_params() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let psk = [0x55u8; 32];
    let config = HcomConfig {
        relay_id: "relay-1".to_string(),
        relay_psk: super::super::encode_psk(&psk),
        ..Default::default()
    };
    let db = test_db();
    let (topic, sealed) = build_rpc_control_payload(
        &db,
        &config,
        "launch",
        "WXYZ",
        "req-launch",
        &json!({"tool": "claude", "count": 1}),
    )
    .expect("payload");
    // Build path now produces a sealed envelope; opening with the same PSK
    // returns the original JSON.
    let plaintext =
        super::super::crypto::open(&psk, &config.relay_id, &topic, &sealed).expect("open");
    let parsed: Value = serde_json::from_slice(&plaintext).unwrap();
    let data = &parsed["events"][0]["data"];
    assert_eq!(data["action"], "launch");
    assert_eq!(data["target_device"], "WXYZ");
    assert_eq!(data["request_id"], "req-launch");
    assert_eq!(data["params"]["tool"], "claude");
}

#[test]
fn test_remote_launch_request_from_params_defaults_optional_fields() {
    let request = RemoteLaunchRequest::from_params(&json!({"tool": "claude", "count": 2})).unwrap();
    assert_eq!(request.tool, "claude");
    assert_eq!(request.count, 2);
    assert!(request.args.is_empty());
    assert_eq!(request.tag, None);
    assert_eq!(request.launcher, None);
    assert_eq!(request.system_prompt, None);
    assert_eq!(request.initial_prompt, None);
    assert!(!request.background);
    assert_eq!(request.terminal, None);
    assert_eq!(request.cwd, None);
}

#[test]
fn test_remote_launch_request_from_params_collects_terminal() {
    let request = RemoteLaunchRequest::from_params(&json!({
        "tool": "codex",
        "count": 1,
        "terminal": "kitty-tab",
        "launcher": "rega",
    }))
    .unwrap();
    assert_eq!(request.terminal.as_deref(), Some("kitty-tab"));
    assert_eq!(request.launcher.as_deref(), Some("rega"));
}

#[test]
fn test_prepare_remote_launch_supports_interactive() {
    let request = RemoteLaunchRequest::from_params(&json!({
        "tool": "codex",
        "count": 1,
        "args": ["--model", "gpt-5.4"],
        "background": false,
    }))
    .unwrap();
    let prepared = prepare_remote_launch(&request, &HcomConfig::default());
    assert!(!prepared.background);
    assert_eq!(prepared.args, vec!["--model", "gpt-5.4"]);
}

#[test]
fn test_prepare_remote_launch_keeps_background_detection() {
    let request = RemoteLaunchRequest::from_params(&json!({
        "tool": "claude",
        "count": 1,
        "args": ["-p"],
        "background": false,
    }))
    .unwrap();
    let prepared = prepare_remote_launch(&request, &HcomConfig::default());
    assert!(prepared.background);
}

#[test]
fn test_prepare_remote_launch_claude_print_normalizes() {
    // Remote `claude -p` (explicit print mode) must go through the same
    // print-mode normalization as the local path.
    let request = RemoteLaunchRequest::from_params(&json!({
        "tool": "claude",
        "count": 1,
        "args": ["-p"],
        "background": true,
        "initial_prompt": "say hi in hcom",
    }))
    .unwrap();
    let prepared = prepare_remote_launch(&request, &HcomConfig::default());
    assert!(prepared.background);
    assert!(prepared.args.iter().any(|arg| arg == "-p"));
    assert!(
        prepared
            .args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"])
    );
    assert!(prepared.args.iter().any(|arg| arg == "--verbose"));
}

#[test]
fn test_prepare_remote_launch_claude_headless_stays_pty() {
    // Bare remote `claude --headless` (no -p) is the live PTY session — no -p
    // is injected, no print-mode defaults, matching the local path.
    let request = RemoteLaunchRequest::from_params(&json!({
        "tool": "claude",
        "count": 1,
        "args": [],
        "background": true,
        "initial_prompt": "say hi in hcom",
    }))
    .unwrap();
    let prepared = prepare_remote_launch(&request, &HcomConfig::default());
    assert!(prepared.background);
    assert!(
        !prepared
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-p" | "--print"))
    );
}

#[test]
fn test_remote_launch_defers_claude_print_prompt_validation() {
    let request = RemoteLaunchRequest::from_params(&json!({
        "tool": "claude",
        "count": 1,
        "args": ["-p"],
        "background": true,
    }))
    .unwrap();
    let prepared = prepare_remote_launch(&request, &HcomConfig::default());
    assert!(prepared.background);
    assert!(
        crate::commands::launch::validate_claude_headless_launch(
            &request.tool,
            prepared.background,
            &prepared.args,
            request.initial_prompt.as_deref(),
        )
        .is_ok()
    );
}

#[test]
fn test_remote_resume_request_from_params_collects_extra_args() {
    let request = RemoteResumeRequest::from_params(&json!({
        "target": "luna",
        "fork": true,
        "extra_args": ["--terminal", "kitty", "--model", "opus"],
        "launcher": "rega",
    }))
    .unwrap();
    assert_eq!(request.target, "luna");
    assert!(request.fork);
    assert_eq!(request.launcher.as_deref(), Some("rega"));
    assert_eq!(
        request.extra_args,
        vec!["--terminal", "kitty", "--model", "opus"]
    );
}

#[test]
fn test_handle_control_events_kill_emits_rpc_result() {
    let db = test_db();
    let events = vec![json!({
        "type": "control",
        "ts": 1002.0,
        "data": {
            "action": "kill",
            "target_device": "WXYZ",
            "from": "_:EFGH",
            "request_id": "req-kill",
            "params": {
                "target": "missing-agent",
            }
        }
    })];

    assert!(handle_control_events(&db, &events, "WXYZ", "device-123"));
    let result = latest_rpc_result(&db);
    assert_eq!(result["request_id"], "req-kill");
    assert_eq!(result["action"], "kill");
    assert_eq!(result["ok"], false);
}

#[test]
fn test_handle_control_events_ignores_unknown_actions() {
    let db = test_db();
    let events = vec![json!({
        "type": "control",
        "ts": 1004.0,
        "data": {
            "action": "mystery",
            "target_device": "WXYZ",
            "from": "_:EFGH",
            "request_id": "req-unknown",
            "params": {},
        }
    })];

    assert!(!handle_control_events(&db, &events, "WXYZ", "device-123"));
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'rpc_result'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
#[serial_test::serial]
fn test_handle_control_events_relay_off_disables_local_relay() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let config = HcomConfig {
        relay: "mqtts://broker.emqx.io:8883".to_string(),
        relay_id: "relay-1".to_string(),
        relay_psk: super::super::encode_psk(&[0x22; 32]),
        relay_enabled: true,
        ..Default::default()
    };
    crate::config::save_toml_config(&config, None).unwrap();

    let db = test_db();
    let loaded = HcomConfig::load(None).unwrap();
    let result = handle_remote_relay_off(&db, &json!({}), "_:EFGH", &loaded).unwrap();
    assert_eq!(result["disabled"], true);
    let updated = HcomConfig::load(None).unwrap();
    assert!(!updated.relay_enabled);
}

#[test]
fn test_require_successful_rpc_result_returns_error_for_failed_response() {
    let err = require_successful_rpc_result(json!({
        "action": "resume",
        "ok": false,
        "result": { "error": "boom" }
    }))
    .unwrap_err();
    assert_eq!(err, "resume failed: boom");
}

#[test]
fn test_check_remote_action_for_db_accepts_advertised_action() {
    let db = test_db();
    safe_kv_set(&db, "relay_short_WXYZ", Some("device-123"));
    safe_kv_set(
        &db,
        "relay_caps_device-123",
        Some(r#"["launch","resume","config_get"]"#),
    );

    assert!(check_remote_action_for_db(&db, "WXYZ", "resume", None).is_ok());
}

#[test]
fn test_check_remote_action_for_db_rejects_unadvertised_action() {
    let db = test_db();
    safe_kv_set(&db, "relay_short_WXYZ", Some("device-123"));
    safe_kv_set(&db, "relay_caps_device-123", Some(r#"["launch"]"#));

    let err = check_remote_action_for_db(&db, "WXYZ", "resume", None).unwrap_err();
    assert!(
        err.starts_with("device WXYZ does not advertise remote action 'resume'"),
        "unexpected err: {err}"
    );
    assert!(
        err.contains("hcom relay off"),
        "missing restart hint: {err}"
    );
}

#[test]
fn test_check_remote_action_for_db_rejects_unadvertised_action_with_target_name() {
    let db = test_db();
    safe_kv_set(&db, "relay_short_WXYZ", Some("device-123"));
    safe_kv_set(&db, "relay_caps_device-123", Some(r#"["launch"]"#));

    let err = check_remote_action_for_db(&db, "WXYZ", "kill", Some("luna:WXYZ")).unwrap_err();
    assert!(
        err.starts_with("device WXYZ (target luna:WXYZ) does not advertise remote action 'kill'"),
        "unexpected err: {err}"
    );
}

#[test]
fn test_check_remote_action_for_db_rejects_missing_capabilities() {
    let db = test_db();
    safe_kv_set(&db, "relay_short_WXYZ", Some("device-123"));

    let err = check_remote_action_for_db(&db, "WXYZ", "resume", None).unwrap_err();
    assert!(
        err.contains("has not yet synced remote capabilities"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_check_remote_action_for_db_accepts_legacy_peer() {
    // Pre-capability peers (pull.rs stores "null" when the state message
    // carries no `capabilities` field) must be allowed through. Blocking
    // them here would break rolling upgrades: the user would see
    // "does not advertise remote action 'launch'" against a peer that
    // simply predates the capability vocabulary.
    let db = test_db();
    safe_kv_set(&db, "relay_short_WXYZ", Some("device-123"));
    safe_kv_set(&db, "relay_caps_device-123", Some("null"));

    assert_eq!(
        read_remote_capabilities(&db, "WXYZ").unwrap(),
        CachedCapabilities::Legacy
    );
    assert!(check_remote_action_for_db(&db, "WXYZ", "launch", None).is_ok());
    assert!(check_remote_action_for_db(&db, "WXYZ", "kill", None).is_ok());
}

#[test]
fn test_check_remote_action_for_db_rejects_explicit_empty_capabilities() {
    // A modern peer that explicitly advertises an empty list should still
    // be hard-blocked. The legacy carve-out only applies when the field is
    // MISSING.
    let db = test_db();
    safe_kv_set(&db, "relay_short_WXYZ", Some("device-123"));
    safe_kv_set(&db, "relay_caps_device-123", Some("[]"));

    assert_eq!(
        read_remote_capabilities(&db, "WXYZ").unwrap(),
        CachedCapabilities::Advertised(Vec::new())
    );
    let err = check_remote_action_for_db(&db, "WXYZ", "launch", None).unwrap_err();
    assert!(
        err.contains("does not advertise remote action"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_advertised_remote_capabilities_lists_all_handlers() {
    assert_eq!(
        advertised_remote_capabilities(),
        vec![
            "launch",
            "kill",
            "resume",
            "config_get",
            "config_set",
            "relay_off",
            "term_screen",
            "term_inject",
            "transcript",
            "events",
            "sub_create",
            "sub_list",
            "sub_unsub",
        ]
    );
}

#[test]
fn test_handle_remote_config_get_blocks_relay_psk() {
    let db = test_db();
    let err = handle_remote_config_get(
        &db,
        &json!({"fields": ["relay_psk"]}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap_err();
    assert_eq!(err, "relay_psk is not remotely queryable");
}

#[test]
fn test_handle_remote_config_get_blocks_relay_psk_for_instance_mode() {
    let db = test_db();
    let err = handle_remote_config_get(
        &db,
        &json!({"instance": "luna", "field": "relay_psk"}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap_err();
    assert_eq!(err, "relay_psk is not remotely queryable");
}

#[test]
fn test_handle_remote_config_set_blocks_relay_psk() {
    let db = test_db();
    let err = handle_remote_config_set(
        &db,
        &json!({"field": "relay_psk", "value": "secret"}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap_err();
    assert_eq!(err, "relay_psk is not remotely queryable");
}

#[test]
fn test_handle_remote_config_set_blocks_relay_psk_for_instance_mode() {
    let db = test_db();
    let err = handle_remote_config_set(
        &db,
        &json!({"instance": "luna", "field": "relay_psk", "value": "secret"}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap_err();
    assert_eq!(err, "relay_psk is not remotely queryable");
}

fn seed_events(db: &HcomDb, count: usize) {
    for i in 0..count {
        let etype = if i % 2 == 0 { "message" } else { "status" };
        db.log_event(etype, "luna", &json!({"i": i})).unwrap();
    }
}

#[test]
fn test_handle_remote_events_empty_filters_returns_all() {
    let db = test_db();
    seed_events(&db, 5);
    let out = handle_remote_events(&db, &json!({}), "initiator", &HcomConfig::default()).unwrap();
    assert_eq!(out["count"].as_u64().unwrap(), 5);
    let events = out["events"].as_array().unwrap();
    assert_eq!(events.len(), 5);
}

#[test]
fn test_handle_remote_events_type_message_filter() {
    let db = test_db();
    seed_events(&db, 6);
    let out = handle_remote_events(
        &db,
        &json!({"filters": {"type": ["message"]}, "last": 50}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap();
    let events = out["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    for e in events {
        assert_eq!(e["type"].as_str().unwrap(), "message");
    }
}

#[test]
fn test_handle_remote_events_hard_cap() {
    let db = test_db();
    seed_events(&db, 10);
    let out = handle_remote_events(
        &db,
        &json!({"last": 9999}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap();
    assert_eq!(out["count"].as_u64().unwrap(), 10);
    let out = handle_remote_events(
        &db,
        &json!({"last": 3}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap();
    assert_eq!(out["count"].as_u64().unwrap(), 3);
}

#[test]
fn test_handle_remote_events_missing_filters_ok() {
    let db = test_db();
    seed_events(&db, 2);
    let out = handle_remote_events(
        &db,
        &json!({"last": 10}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap();
    assert_eq!(out["count"].as_u64().unwrap(), 2);
}

#[test]
fn test_handle_remote_events_truncates_when_envelope_exceeds_cap() {
    let db = test_db();
    // Big payload per event so a few rows blow past the 96 KiB cap.
    let big = "x".repeat(8_000);
    for _ in 0..20 {
        db.log_event("message", "luna", &json!({"blob": big}))
            .unwrap();
    }
    let input_count = 20usize;
    let out = handle_remote_events(
        &db,
        &json!({"last": input_count}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap();
    assert_eq!(out["truncated"].as_bool(), Some(true));
    let returned = out["events"].as_array().unwrap().len();
    assert!(
        returned < input_count,
        "expected truncation, got {returned}"
    );
    let envelope_len = serde_json::to_string(&out).unwrap().len();
    assert!(
        envelope_len <= REMOTE_EVENTS_BYTE_CAP,
        "envelope {envelope_len} bytes exceeds cap"
    );
}

#[test]
fn test_handle_remote_events_invalid_sql_returns_err() {
    let db = test_db();
    seed_events(&db, 2);
    let err = handle_remote_events(
        &db,
        &json!({"sql": "not a real column = 1"}),
        "initiator",
        &HcomConfig::default(),
    )
    .unwrap_err();
    assert!(err.contains("sql error"), "unexpected err: {err}");
}
