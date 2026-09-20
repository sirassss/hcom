use super::*;

#[cfg(unix)]
#[test]
fn test_background_push_is_disabled_in_unit_tests() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("spawned");
    let shim = temp.path().join("hcom-shim");
    std::fs::write(&shim, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();

    spawn_background_push_with(&[shim.to_string_lossy().into_owned()]);
    std::thread::sleep(std::time::Duration::from_millis(100));

    assert!(
        !marker.exists(),
        "unit tests must never spawn a real hcom relay push subprocess"
    );
}

#[test]
fn test_parse_broker_url_mqtts() {
    let (host, port, tls) = parse_broker_url("mqtts://broker.emqx.io:8883").unwrap();
    assert_eq!(host, "broker.emqx.io");
    assert_eq!(port, 8883);
    assert!(tls);
}

#[test]
fn test_parse_broker_url_mqtt() {
    let (host, port, tls) = parse_broker_url("mqtt://localhost:1883").unwrap();
    assert_eq!(host, "localhost");
    assert_eq!(port, 1883);
    assert!(!tls);
}

#[test]
fn test_parse_broker_url_default_port() {
    let (host, port, tls) = parse_broker_url("mqtts://broker.emqx.io").unwrap();
    assert_eq!(host, "broker.emqx.io");
    assert_eq!(port, 8883);
    assert!(tls);
}

#[test]
fn test_parse_broker_url_empty() {
    assert!(parse_broker_url("").is_none());
}

#[test]
fn test_topics() {
    assert_eq!(
        state_topic("relay-123", "device-abc"),
        "relay-123/device-abc"
    );
    assert_eq!(control_topic("relay-123"), "relay-123/control");
    assert_eq!(wildcard_topic("relay-123"), "relay-123/+");
}

fn is_cvcv_upper(s: &str) -> bool {
    const C: &[u8] = b"BDFGHKLMNPRSTVZ";
    const V: &[u8] = b"AEIOU";
    let b = s.as_bytes();
    b.len() == 4 && C.contains(&b[0]) && V.contains(&b[1]) && C.contains(&b[2]) && V.contains(&b[3])
}

#[test]
fn test_device_short_id() {
    // FNV-1a → CVCV word, uppercased. Deterministic, valid format, and
    // different inputs typically produce different outputs.
    for uuid in ["abcd-1234-efgh", "12345678", "device-123"] {
        let s = device_short_id(uuid);
        assert!(is_cvcv_upper(&s), "{s} is not 4-letter uppercase CVCV");
        assert_eq!(device_short_id(uuid), s, "not deterministic");
    }
}

#[test]
fn test_device_short_id_for_db_persists_natural_hash() {
    let db = test_db();
    let natural = device_short_id("device-123");
    let short_id = device_short_id_for_db(&db, "device-123");

    assert_eq!(short_id, natural);
    assert_eq!(
        safe_kv_get(&db, "relay_uuid_short_device-123").as_deref(),
        Some(natural.as_str())
    );
    assert_eq!(
        safe_kv_get(&db, &format!("relay_short_{natural}")).as_deref(),
        Some("device-123")
    );
}

#[test]
fn test_device_short_id_for_db_probes_on_collision() {
    let db = test_db();
    let mut collision = None;
    let mut seen = std::collections::HashMap::new();
    for i in 0..20_000 {
        let uuid = format!("device-{i}");
        let short = device_short_id(&uuid);
        if let Some(first) = seen.insert(short.clone(), uuid.clone()) {
            collision = Some((first, uuid, short));
            break;
        }
    }
    let (first, second, legacy_short) = collision.expect("expected a legacy short-id collision");

    assert_eq!(device_short_id_for_db(&db, &first), legacy_short);
    let second_short = device_short_id_for_db(&db, &second);

    assert_ne!(second_short, legacy_short);
    assert_eq!(
        safe_kv_get(&db, &format!("relay_uuid_short_{second}")).as_deref(),
        Some(second_short.as_str())
    );
    assert_eq!(
        safe_kv_get(&db, &format!("relay_short_{second_short}")).as_deref(),
        Some(second.as_str())
    );
    assert_eq!(device_short_id_for_db(&db, &first), legacy_short);
}

#[test]
fn test_is_relay_enabled() {
    let mut config = HcomConfig::default();
    // Default: relay_id empty, relay_enabled true → not enabled
    assert!(!is_relay_enabled(&config));

    config.relay_id = "some-id".to_string();
    assert!(is_relay_enabled(&config));

    config.relay_enabled = false;
    assert!(!is_relay_enabled(&config));
}

#[test]
fn test_read_device_uuid_creates_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(".tmp").join("device_id");
    let uuid = read_or_create_device_uuid_at(&path).expect("should create");
    assert!(!uuid.is_empty());
    // Subsequent call must return the SAME persisted UUID.
    let again = read_or_create_device_uuid_at(&path).expect("should read");
    assert_eq!(uuid, again);
}

#[test]
fn test_read_device_uuid_repairs_empty_file() {
    // Regression: prior implementation used create_new which refused to
    // replace an existing-but-empty file, so a 0-byte device_id (left by
    // an aborted write) caused permanent None.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".tmp");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("device_id");
    std::fs::write(&path, "").unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

    let uuid = read_or_create_device_uuid_at(&path).expect("should repair");
    assert!(!uuid.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), uuid);
}

#[test]
fn test_read_device_uuid_repairs_whitespace_only_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".tmp");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("device_id");
    std::fs::write(&path, "   \n\t  ").unwrap();

    let uuid = read_or_create_device_uuid_at(&path).expect("should repair");
    assert!(!uuid.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), uuid);
}

#[test]
fn test_read_device_uuid_concurrent_first_callers_agree() {
    // Regression: concurrent first callers used to each generate their own
    // UUID, return their own in-memory copy, and disagree on disk. With
    // flock + read-under-lock, all callers must observe the SAME UUID.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(".tmp").join("device_id");

    let n = 8;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(n));
    let path_arc = std::sync::Arc::new(path.clone());

    let handles: Vec<_> = (0..n)
        .map(|_| {
            let b = std::sync::Arc::clone(&barrier);
            let p = std::sync::Arc::clone(&path_arc);
            std::thread::spawn(move || {
                b.wait();
                read_or_create_device_uuid_at(&p)
            })
        })
        .collect();

    let results: Vec<Option<String>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let first = results[0].as_ref().expect("at least one must succeed");
    for (i, r) in results.iter().enumerate() {
        let r = r
            .as_ref()
            .unwrap_or_else(|| panic!("thread {i} returned None"));
        assert_eq!(
            r, first,
            "thread {i} got divergent UUID — race not serialized"
        );
    }
    // And the persisted file matches.
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), first);
}

// ── RelayHealth derivation matrix ────────────────────────────────
//
// Each test pins one cell of the 10-row precedence matrix agreed with
// dinu. Keep names aligned to precedence rules for grep-friendly failure
// output. The `obs()` helper builds an Enabled observation; individual
// tests tweak only the fields under test.

fn obs() -> RelayObservation {
    RelayObservation {
        configured: true,
        enabled: true,
        raw_status: None,
        raw_error: None,
        heartbeat_age_s: None,
        pidfile: None,
        last_push: 0.0,
        broker: None,
    }
}

#[test]
fn derive_01_not_configured() {
    let o = RelayObservation {
        configured: false,
        enabled: true, // shouldn't matter
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::NotConfigured);
}

#[test]
fn derive_02_disabled_takes_precedence_over_runtime_state() {
    // If disable didn't clear runtime KV we'd still see raw_status=ok here;
    // Disabled must short-circuit regardless.
    let o = RelayObservation {
        enabled: false,
        raw_status: Some("ok".into()),
        heartbeat_age_s: Some(1.0),
        pidfile: Some((12345, true)),
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Disabled);
}

#[test]
fn derive_03_reported_error_wins_over_pid_and_heartbeat() {
    // Worker self-report is authoritative — fresh heartbeat doesn't rescue us.
    let o = RelayObservation {
        raw_status: Some("error".into()),
        raw_error: Some("not authorized".into()),
        heartbeat_age_s: Some(0.5),
        pidfile: Some((4242, true)),
        ..obs()
    };
    match derive_relay_health(&o) {
        RelayHealth::Error {
            reason,
            detail,
            pid,
        } => {
            assert_eq!(reason, RelayErrorReason::Reported);
            assert_eq!(detail.as_deref(), Some("not authorized"));
            assert_eq!(pid, Some(4242));
        }
        other => panic!("expected Error(Reported), got {other:?}"),
    }
}

#[test]
fn derive_04_pidfile_present_pid_dead_is_stale_pidfile_error() {
    let o = RelayObservation {
        pidfile: Some((9999, false)),
        ..obs()
    };
    match derive_relay_health(&o) {
        RelayHealth::Error { reason, pid, .. } => {
            assert_eq!(reason, RelayErrorReason::StalePidfile);
            assert_eq!(pid, Some(9999));
        }
        other => panic!("expected Error(StalePidfile), got {other:?}"),
    }
}

#[test]
fn derive_05_pid_alive_heartbeat_missing_is_starting() {
    let o = RelayObservation {
        pidfile: Some((111, true)),
        heartbeat_age_s: None,
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Starting { pid: 111 });
}

#[test]
fn derive_06_pid_alive_heartbeat_stale_is_stale() {
    let o = RelayObservation {
        pidfile: Some((222, true)),
        heartbeat_age_s: Some(HEARTBEAT_STALE_SECS + 5.0),
        ..obs()
    };
    match derive_relay_health(&o) {
        RelayHealth::Stale { age_s, pid } => {
            assert!(age_s >= HEARTBEAT_STALE_SECS);
            assert_eq!(pid, 222);
        }
        other => panic!("expected Stale, got {other:?}"),
    }
}

#[test]
fn derive_07_pid_alive_heartbeat_fresh_status_ok_is_connected() {
    let o = RelayObservation {
        pidfile: Some((333, true)),
        heartbeat_age_s: Some(0.5),
        raw_status: Some(RAW_STATUS_OK.into()),
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Connected);
}

#[test]
fn derive_connected_is_stable_across_heartbeat_ticks() {
    // Render diffing relies on PartialEq short-circuiting when health hasn't
    // meaningfully changed. If Connected carried the heartbeat age, every
    // 1Hz tick would re-render the relay indicator for nothing.
    let mk = |age: f64| RelayObservation {
        pidfile: Some((1, true)),
        heartbeat_age_s: Some(age),
        raw_status: Some(RAW_STATUS_OK.into()),
        ..obs()
    };
    assert_eq!(derive_relay_health(&mk(0.1)), derive_relay_health(&mk(8.5)));
}

#[test]
fn derive_08_pid_alive_heartbeat_fresh_status_not_ok_is_starting() {
    // Covers the startup window: worker is ticking but hasn't received ConnAck.
    // raw_status is empty or "disconnected" — anything except "ok" or "error".
    let o = RelayObservation {
        pidfile: Some((444, true)),
        heartbeat_age_s: Some(0.2),
        raw_status: None,
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Starting { pid: 444 });

    // Also cover explicit "disconnected" sentinel in case any code path writes it.
    let o = RelayObservation {
        pidfile: Some((445, true)),
        heartbeat_age_s: Some(0.2),
        raw_status: Some("disconnected".into()),
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Starting { pid: 445 });
}

#[test]
fn derive_09_no_pid_status_ok_is_ghost_error() {
    // The pone bug: worker reaped, status KV never flipped to error.
    let o = RelayObservation {
        pidfile: None,
        heartbeat_age_s: None,
        raw_status: Some("ok".into()),
        ..obs()
    };
    match derive_relay_health(&o) {
        RelayHealth::Error { reason, pid, .. } => {
            assert_eq!(reason, RelayErrorReason::Ghost);
            assert_eq!(pid, None);
        }
        other => panic!("expected Error(Ghost), got {other:?}"),
    }
}

#[test]
fn derive_09b_no_pid_fresh_heartbeat_is_ghost() {
    // Heartbeat without pidfile is anomalous — treat as ghost.
    let o = RelayObservation {
        pidfile: None,
        heartbeat_age_s: Some(0.5),
        raw_status: None,
        ..obs()
    };
    match derive_relay_health(&o) {
        RelayHealth::Error { reason, .. } => {
            assert_eq!(reason, RelayErrorReason::Ghost);
        }
        other => panic!("expected Error(Ghost), got {other:?}"),
    }
}

#[test]
fn derive_10_no_pid_no_heartbeat_no_status_is_waiting() {
    // Cold start or clean post-shutdown idle.
    let o = RelayObservation {
        pidfile: None,
        heartbeat_age_s: None,
        raw_status: None,
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Waiting);

    // "disconnected" (or any non-ok, non-error status) with no pid also Waiting.
    let o = RelayObservation {
        pidfile: None,
        heartbeat_age_s: None,
        raw_status: Some("disconnected".into()),
        ..obs()
    };
    assert_eq!(derive_relay_health(&o), RelayHealth::Waiting);
}

// ── disable clears runtime KV, preserves activity watermarks ────

fn test_db() -> HcomDb {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_raw(&dir.path().join("test.db")).unwrap();
    db.init_db().unwrap();
    std::mem::forget(dir);
    db
}

#[test]
fn clear_runtime_relay_kv_nukes_runtime_health_fields() {
    let db = test_db();
    for key in RUNTIME_HEALTH_KV_KEYS {
        safe_kv_set(&db, key, Some("present"));
    }
    clear_runtime_relay_kv(&db);
    for key in RUNTIME_HEALTH_KV_KEYS {
        assert!(
            safe_kv_get(&db, key).is_none(),
            "{key} should be cleared after disable"
        );
    }
}

#[test]
fn clear_runtime_relay_kv_preserves_activity_watermarks() {
    // Regression guard: relay_last_push_id is a broker watermark. Clearing
    // it on disable would cause re-push of already-synced events when the
    // user toggles relay back on inside the same group.
    let db = test_db();
    safe_kv_set(&db, "relay_last_push_id", Some("12345"));
    safe_kv_set(&db, "relay_last_push", Some("1700000000.0"));
    safe_kv_set(&db, "relay_last_sync", Some("1700000001.0"));
    safe_kv_set(&db, "relay_status", Some("ok"));

    clear_runtime_relay_kv(&db);

    assert_eq!(
        safe_kv_get(&db, "relay_last_push_id").as_deref(),
        Some("12345")
    );
    assert_eq!(
        safe_kv_get(&db, "relay_last_push").as_deref(),
        Some("1700000000.0")
    );
    assert_eq!(
        safe_kv_get(&db, "relay_last_sync").as_deref(),
        Some("1700000001.0")
    );
    assert!(safe_kv_get(&db, "relay_status").is_none());
}
