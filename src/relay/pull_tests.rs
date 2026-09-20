use super::*;
use crate::hooks::test_helpers::isolated_test_env;
use serde_json::json;
use serial_test::serial;

fn fixture_psk() -> [u8; 32] {
    [0x33; 32]
}

fn seal_for_test(payload: &serde_json::Value, topic: &str, relay_id: &str) -> Vec<u8> {
    let psk = fixture_psk();
    let bytes = serde_json::to_vec(payload).unwrap();
    let now = crate::shared::time::now_epoch_f64() as u64;
    crate::relay::crypto::seal(&psk, relay_id, topic, &bytes, now).unwrap()
}

#[test]
#[serial]
fn test_handle_state_message_drops_remote_unique_identity_fields() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();

    let payload = json!({
        "state": {
            "short_id": "ABCD",
            "reset_ts": 0.0,
            "instances": {
                "orla": {
                    "status": "active",
                    "context": "",
                    "detail": "",
                    "status_time": crate::shared::time::now_epoch_f64(),
                    "parent": serde_json::Value::Null,
                    "directory": "/tmp/demo-parent",
                    "transcript": "/tmp/demo-parent/transcript.jsonl",
                    "wait_timeout": 42,
                    "last_stop": 0.0,
                    "tcp_mode": false,
                    "tag": "demo",
                    "tool": "codex",
                    "background": false
                },
                "luna": {
                    "status": "active",
                    "context": "",
                    "detail": "",
                    "status_time": crate::shared::time::now_epoch_f64(),
                    "parent": "orla",
                    "directory": "/tmp/demo",
                    "transcript": "/tmp/demo/transcript.jsonl",
                    "wait_timeout": 42,
                    "last_stop": 0.0,
                    "tcp_mode": false,
                    "tag": "demo",
                    "tool": "codex",
                    "background": false
                }
            }
        },
        "events": []
    });

    let topic = "relay-test/device-1234";
    let envelope = seal_for_test(&payload, topic, "relay-test");
    let mut guard = ReplayGuard::default();
    let psk = fixture_psk();
    handle_state_message(
        &db,
        "device-1234",
        &envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    );

    let row = db
        .get_instance_full("luna:ABCD")
        .unwrap()
        .expect("remote row");
    assert_eq!(row.parent_name.as_deref(), Some("orla:ABCD"));
    assert_eq!(row.session_id, None);
    assert_eq!(row.parent_session_id, None);
    assert_eq!(row.agent_id, None);
    assert_eq!(row.tool, "codex");
}

#[test]
#[serial]
fn test_handle_state_message_caches_remote_capabilities() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();

    let payload = json!({
        "state": {
            "short_id": "ABCD",
            "reset_ts": 0.0,
            "capabilities": ["launch", "resume"],
            "instances": {}
        },
        "events": []
    });

    let topic = "relay-test/device-1234";
    let envelope = seal_for_test(&payload, topic, "relay-test");
    let mut guard = ReplayGuard::default();
    let psk = fixture_psk();
    assert!(!handle_state_message(
        &db,
        "device-1234",
        &envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    ));

    assert_eq!(
        safe_kv_get(&db, "relay_caps_device-1234").as_deref(),
        Some(r#"["launch","resume"]"#)
    );
}

#[test]
#[serial]
fn test_handle_state_message_caches_legacy_peer_without_capabilities() {
    // Peers that predate the `capabilities` advertisement must be cached
    // with the "null" sentinel, not "[]". The capability check in
    // relay::control reads this sentinel as `CachedCapabilities::Legacy`
    // and lets requests through optimistically so rolling upgrades don't
    // break remote actions against older peers.
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();

    let payload = json!({
        "state": {
            "short_id": "ABCD",
            "reset_ts": 0.0,
            "instances": {}
        },
        "events": []
    });

    let topic = "relay-test/device-1234";
    let envelope = seal_for_test(&payload, topic, "relay-test");
    let mut guard = ReplayGuard::default();
    let psk = fixture_psk();
    assert!(!handle_state_message(
        &db,
        "device-1234",
        &envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    ));

    assert_eq!(
        safe_kv_get(&db, "relay_caps_device-1234").as_deref(),
        Some("null"),
        "legacy peer (no capabilities field) must be cached as the \"null\" sentinel"
    );
}

#[test]
#[serial]
fn test_handle_state_message_accepts_sender_clock_skew_in_both_directions() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();
    let mut guard = ReplayGuard::default();
    let psk = fixture_psk();
    let now = crate::shared::time::now_epoch_f64() as i64;

    for (device_id, short_id, offset) in [
        ("device-past", "PAST", -61_i64),
        ("device-future", "FUTR", 61_i64),
    ] {
        let payload = json!({
            "state": {
                "short_id": short_id,
                "reset_ts": 0.0,
                "instances": {
                    "luna": {
                        "status": "active",
                        "context": "",
                        "detail": "",
                        "status_time": crate::shared::time::now_epoch_f64(),
                        "parent": serde_json::Value::Null,
                        "directory": "/tmp/demo",
                        "transcript": "/tmp/demo/transcript.jsonl",
                        "wait_timeout": 42,
                        "last_stop": 0.0,
                        "tcp_mode": false,
                        "tag": serde_json::Value::Null,
                        "tool": "codex",
                        "background": false
                    }
                }
            },
            "events": []
        });
        let topic = format!("relay-test/{device_id}");
        let bytes = serde_json::to_vec(&payload).unwrap();
        let envelope =
            crate::relay::crypto::seal(&psk, "relay-test", &topic, &bytes, (now + offset) as u64)
                .unwrap();

        assert!(!handle_state_message(
            &db,
            device_id,
            &envelope,
            "own-device-5678",
            &mut InboundContext {
                psk: &psk,
                relay_id: "relay-test",
                topic: &topic,
                replay_guard: &mut guard,
            },
        ));
        assert!(
            db.get_instance_full(&format!("luna:{short_id}"))
                .unwrap()
                .is_some()
        );
    }
}

#[test]
#[serial]
fn test_handle_state_message_rejects_rollback_behind_watermark() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();
    safe_kv_set(&db, "relay_state_ts_device-1234", Some("1500"));

    let payload = json!({
        "state": {
            "short_id": "ABCD",
            "reset_ts": 0.0,
            "instances": {
                "luna": {
                    "status": "active",
                    "context": "",
                    "detail": "",
                    "status_time": crate::shared::time::now_epoch_f64(),
                    "parent": serde_json::Value::Null,
                    "directory": "/tmp/demo",
                    "transcript": "/tmp/demo/transcript.jsonl",
                    "wait_timeout": 42,
                    "last_stop": 0.0,
                    "tcp_mode": false,
                    "tag": serde_json::Value::Null,
                    "tool": "codex",
                    "background": false
                }
            }
        },
        "events": []
    });

    let topic = "relay-test/device-1234";
    let bytes = serde_json::to_vec(&payload).unwrap();
    let envelope =
        crate::relay::crypto::seal(&fixture_psk(), "relay-test", topic, &bytes, 1000).unwrap();
    let mut guard = ReplayGuard::default();
    let psk = fixture_psk();

    assert!(!handle_state_message(
        &db,
        "device-1234",
        &envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    ));

    assert!(db.get_instance_full("luna:ABCD").unwrap().is_none());
}

#[test]
#[serial]
fn test_decrypt_failure_does_not_consume_replay_slot() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();
    let topic = "relay-test/device-1234";
    let payload = json!({
        "state": {
            "short_id": "ABCD",
            "reset_ts": 0.0,
            "instances": {}
        },
        "events": []
    });

    let good_envelope = seal_for_test(&payload, topic, "relay-test");
    let mut bad_psk = fixture_psk();
    bad_psk[0] ^= 0x55;
    let bad_bytes = serde_json::to_vec(&payload).unwrap();
    let bad_envelope =
        crate::relay::crypto::seal(&bad_psk, "relay-test", topic, &bad_bytes, 1234).unwrap();

    let mut guard = ReplayGuard::new(1, 600, crate::relay::replay::MAX_SKEW_SECS);
    let psk = fixture_psk();

    assert!(!handle_state_message(
        &db,
        "device-1234",
        &bad_envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    ));
    assert_eq!(
        guard.len(),
        0,
        "failed decrypt must not record replay nonce"
    );

    assert!(!handle_state_message(
        &db,
        "device-1234",
        &good_envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    ));
    assert_eq!(guard.len(), 1);
}

#[test]
#[serial]
fn test_handle_state_message_authenticated_null_state_cleans_up_device_and_watermark() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, origin_device_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna:ABCD", "device-1234", 1.0],
        )
        .unwrap();
    safe_kv_set(&db, "relay_state_ts_device-1234", Some("1500"));

    let payload = json!({
        "state": serde_json::Value::Null,
        "events": [],
    });
    let topic = "relay-test/device-1234";
    let bytes = serde_json::to_vec(&payload).unwrap();
    let envelope =
        crate::relay::crypto::seal(&fixture_psk(), "relay-test", topic, &bytes, 2000).unwrap();
    let mut guard = ReplayGuard::default();
    let psk = fixture_psk();

    assert!(!handle_state_message(
        &db,
        "device-1234",
        &envelope,
        "own-device-5678",
        &mut InboundContext {
            psk: &psk,
            relay_id: "relay-test",
            topic,
            replay_guard: &mut guard,
        },
    ));

    assert!(db.get_instance_full("luna:ABCD").unwrap().is_none());
    assert_eq!(safe_kv_get(&db, "relay_state_ts_device-1234"), None);
}
