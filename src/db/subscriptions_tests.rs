use super::*;
use rusqlite::params;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn setup_full_test_db() -> (HcomDb, PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(10_000);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_subscriptions_{}_{}.db",
        std::process::id(),
        test_id
    ));

    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    (db, db_path)
}

fn cleanup_test_db(path: PathBuf) {
    let _ = std::fs::remove_file(path);
}

fn bind_probe() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn await_connect(listener: &TcpListener, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok(_) => return true,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return false,
        }
    }
}

fn count_reqwatch_without_reply_notifications(db: &HcomDb, requester: &str) -> i64 {
    let pattern = format!("%@{requester} %");
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'message'
             AND json_extract(data, '$.text') LIKE '%without responding to your request%'
             AND json_extract(data, '$.text') LIKE ?1",
            params![pattern],
            |row| row.get(0),
        )
        .unwrap_or(0)
}

fn setup_reqwatch_pair(db: &HcomDb, requester: &str, responder: &str, responder_tool: &str) -> i64 {
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, last_event_id, created_at)
             VALUES (?1, 'claude', 0, 1000.0), (?2, ?3, 0, 1000.0)",
            params![requester, responder, responder_tool],
        )
        .unwrap();
    let req_data = serde_json::json!({
        "from": requester,
        "sender_kind": "instance",
        "scope": "mentions",
        "text": "ping",
        "delivered_to": [responder],
        "intent": "request",
        "mentions": [responder],
    });
    let request_id = db.log_event("message", requester, &req_data).unwrap();
    create_request_watches(db, requester, request_id, &[responder.to_string()]);
    db.conn()
        .execute(
            "UPDATE instances SET last_event_id = ?1 WHERE name = ?2",
            params![request_id, responder],
        )
        .unwrap();
    request_id
}

#[test]
fn test_create_request_watches_records_antigravity_target_tool() {
    let (db, db_path) = setup_full_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, created_at) VALUES ('gora', 'claude', 1000.0), ('nabe', 'antigravity', 1000.0)",
            [],
        )
        .unwrap();
    create_request_watches(&db, "gora", 42, &[String::from("nabe")]);
    let sub_raw = db
        .kv_get("events_sub:reqwatch-42-nabe")
        .unwrap()
        .expect("reqwatch row");
    let sub: serde_json::Value = serde_json::from_str(&sub_raw).unwrap();
    assert_eq!(sub["filters"]["target_tool"].as_str(), Some("antigravity"));
    cleanup_test_db(db_path);
}

#[test]
fn test_agy_reqwatch_listening_defers_idle_notification() {
    let (db, db_path) = setup_full_test_db();
    let request_id = setup_reqwatch_pair(&db, "gora", "nabe", "antigravity");
    let before = count_reqwatch_without_reply_notifications(&db, "gora");

    let data = serde_json::json!({"status": "listening", "context": ""});
    db.log_event("status", "nabe", &data).unwrap();

    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before,
        "agy listening should defer reqwatch notification"
    );
    let sub_raw = db
        .kv_get(&format!("events_sub:reqwatch-{request_id}-nabe"))
        .unwrap()
        .unwrap();
    let sub: serde_json::Value = serde_json::from_str(&sub_raw).unwrap();
    assert!(
        sub.get("idle_grace_until")
            .and_then(|v| v.as_f64())
            .is_some(),
        "grace should be armed: {sub}"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_agy_reqwatch_stopped_notifies_immediately() {
    let (db, db_path) = setup_full_test_db();
    let request_id = setup_reqwatch_pair(&db, "gora", "nabe", "antigravity");
    let before = count_reqwatch_without_reply_notifications(&db, "gora");

    let data = serde_json::json!({"action": "stopped", "by": "pty"});
    db.log_event("life", "nabe", &data).unwrap();

    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before + 1,
        "agy stopped should notify without waiting for grace"
    );
    assert!(
        db.kv_get(&format!("events_sub:reqwatch-{request_id}-nabe"))
            .unwrap()
            .is_none(),
        "once sub should be removed after notify"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_gemini_reqwatch_listening_notifies_without_grace() {
    let (db, db_path) = setup_full_test_db();
    setup_reqwatch_pair(&db, "gora", "nova", "gemini");
    let before = count_reqwatch_without_reply_notifications(&db, "gora");

    let data = serde_json::json!({"status": "listening", "context": ""});
    db.log_event("status", "nova", &data).unwrap();

    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before + 1,
        "non-agy listening should notify immediately"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_pi_reqwatch_notifies_after_delivery_active_then_listening() {
    let (db, db_path) = setup_full_test_db();
    setup_reqwatch_pair(&db, "gora", "nabe", "pi");
    let before = count_reqwatch_without_reply_notifications(&db, "gora");

    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "active", "context": "deliver:gora"}),
    )
    .unwrap();
    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before,
        "delivery active edge should not notify yet"
    );

    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "listening", "context": ""}),
    )
    .unwrap();
    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before + 1,
        "pi listening after delivery should notify"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_agy_reqwatch_active_clears_idle_grace() {
    let (db, db_path) = setup_full_test_db();
    let request_id = setup_reqwatch_pair(&db, "gora", "nabe", "antigravity");
    let sub_key = format!("events_sub:reqwatch-{request_id}-nabe");

    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "listening", "context": ""}),
    )
    .unwrap();
    assert!(
        db.kv_get(&sub_key)
            .unwrap()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("idle_grace_until").cloned())
            .is_some()
    );

    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "active", "context": "tool:run_command"}),
    )
    .unwrap();

    let sub: serde_json::Value =
        serde_json::from_str(&db.kv_get(&sub_key).unwrap().unwrap()).unwrap();
    assert!(
        sub.get("idle_grace_until").is_none(),
        "active should clear agy grace: {sub}"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_agy_reqwatch_expired_grace_sweep_notifies_exactly_once() {
    let (db, db_path) = setup_full_test_db();
    let request_id = setup_reqwatch_pair(&db, "gora", "nabe", "antigravity");
    let sub_key = format!("events_sub:reqwatch-{request_id}-nabe");
    let before = count_reqwatch_without_reply_notifications(&db, "gora");

    db.set_status("nabe", "listening", "").unwrap();
    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "listening", "context": ""}),
    )
    .unwrap();

    let mut sub: serde_json::Value =
        serde_json::from_str(&db.kv_get(&sub_key).unwrap().unwrap()).unwrap();
    sub["idle_grace_until"] = serde_json::json!(1.0);
    kv_store_sub(&db, &sub_key, &sub);

    sweep_expired_reqwatch_graces(&db, 2.0);
    sweep_expired_reqwatch_graces(&db, 3.0);

    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before + 1,
        "expired grace should emit one abandoned-request notice"
    );
    assert!(
        db.kv_get(&sub_key).unwrap().is_none(),
        "one-shot reqwatch should be claimed and removed"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_agy_reqwatch_reply_during_grace_cancels_expiry() {
    let (db, db_path) = setup_full_test_db();
    let request_id = setup_reqwatch_pair(&db, "gora", "nabe", "antigravity");
    let sub_key = format!("events_sub:reqwatch-{request_id}-nabe");
    let before = count_reqwatch_without_reply_notifications(&db, "gora");

    db.set_status("nabe", "listening", "").unwrap();
    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "listening", "context": ""}),
    )
    .unwrap();
    db.log_event(
        "message",
        "nabe",
        &serde_json::json!({
            "from": "nabe",
            "sender_kind": "instance",
            "scope": "mentions",
            "text": "done",
            "mentions": ["gora"],
            "delivered_to": ["gora"],
            "reply_to_local": request_id,
        }),
    )
    .unwrap();

    sweep_expired_reqwatch_graces(&db, f64::MAX);
    assert!(db.kv_get(&sub_key).unwrap().is_none());
    assert_eq!(
        count_reqwatch_without_reply_notifications(&db, "gora"),
        before,
        "a reply during grace must suppress the idle notice"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_agy_reqwatch_later_idle_spell_arms_fresh_grace() {
    let (db, db_path) = setup_full_test_db();
    let request_id = setup_reqwatch_pair(&db, "gora", "nabe", "antigravity");
    let sub_key = format!("events_sub:reqwatch-{request_id}-nabe");

    db.set_status("nabe", "listening", "").unwrap();
    let first_idle = db
        .log_event(
            "status",
            "nabe",
            &serde_json::json!({"status": "listening", "context": ""}),
        )
        .unwrap();

    db.set_status("nabe", "blocked", "approval").unwrap();
    db.log_event(
        "status",
        "nabe",
        &serde_json::json!({"status": "blocked", "context": "approval"}),
    )
    .unwrap();
    let after_blocked: serde_json::Value =
        serde_json::from_str(&db.kv_get(&sub_key).unwrap().unwrap()).unwrap();
    assert!(after_blocked.get("idle_grace_until").is_none());
    assert!(after_blocked.get("idle_grace_event_id").is_none());

    db.set_status("nabe", "listening", "").unwrap();
    let second_idle = db
        .log_event(
            "status",
            "nabe",
            &serde_json::json!({"status": "listening", "context": ""}),
        )
        .unwrap();
    let rearmed: serde_json::Value =
        serde_json::from_str(&db.kv_get(&sub_key).unwrap().unwrap()).unwrap();
    assert_ne!(first_idle, second_idle);
    assert_eq!(
        rearmed.get("idle_grace_event_id").and_then(|v| v.as_i64()),
        Some(second_idle),
        "the later idle spell must own a fresh grace timer"
    );
    assert!(rearmed.get("idle_grace_until").is_some());
    cleanup_test_db(db_path);
}

#[test]
fn test_sha256_hash() {
    let h1 = sha256_hash("test input");
    let h2 = sha256_hash("test input");
    let h3 = sha256_hash("different input");
    assert_eq!(h1, h2);
    assert_ne!(h1, h3);
    assert_eq!(h1.len(), 64);
    assert_eq!(&h1[..8], "9dfe6f15");
}

#[test]
fn test_collision_self_relevance_matches_filter_constants() {
    let sql = collision_self_relevance_sql("luna");
    assert!(sql.contains(FILE_WRITE_CONTEXTS));
    assert!(sql.contains("< 30"));
    assert!(!sql.contains("tool:edit_file"));
    assert!(!sql.contains("< 20"));
}

#[test]
fn test_subscription_recursion_guard_sys_prefix() {
    let (db, db_path) = setup_full_test_db();

    // Create a subscription
    let sub = serde_json::json!({
        "id": "test-sub",
        "caller": "luna",
        "sql": "type = 'message'",
        "last_id": 0
    });
    db.kv_set("events_sub:test", Some(&sub.to_string()))
        .unwrap();

    // Log event from sys_ instance - should NOT trigger subscription
    let data = serde_json::json!({"from": "[hcom-events]", "text": "test"});
    db.log_event("message", "sys_[hcom-events]", &data).unwrap();

    // Sub should not be updated (last_id should still be 0)
    let sub_after = db.kv_get("events_sub:test").unwrap().unwrap();
    let sub_val: serde_json::Value = serde_json::from_str(&sub_after).unwrap();
    assert_eq!(sub_val["last_id"], 0);

    cleanup_test_db(db_path);
}

#[test]
fn test_subscription_recursion_guard_system_sender_kind() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    let sub = serde_json::json!({
        "id": "test-sub",
        "caller": "luna",
        "sql": "type = 'message'",
        "last_id": 0
    });
    db.kv_set("events_sub:test", Some(&sub.to_string()))
        .unwrap();

    // Log system message - recursion guard should skip
    let data = serde_json::json!({
        "from": "[hcom-events]",
        "sender_kind": "system",
        "text": "notification"
    });
    db.log_event("message", "ext_test", &data).unwrap();

    // Sub should not be updated
    let sub_after = db.kv_get("events_sub:test").unwrap().unwrap();
    let sub_val: serde_json::Value = serde_json::from_str(&sub_after).unwrap();
    assert_eq!(sub_val["last_id"], 0);

    cleanup_test_db(db_path);
}

#[test]
fn test_subscription_matches_and_updates_cursor() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('luna', '', 1000.0)",
            [],
        )
        .unwrap();

    // Create subscription that matches all status events
    let sub = serde_json::json!({
        "id": "test-sub",
        "caller": "luna",
        "sql": "type = 'status'",
        "last_id": 0
    });
    db.kv_set("events_sub:test", Some(&sub.to_string()))
        .unwrap();

    // Log a status event (not from sys_, not system sender_kind)
    let data = serde_json::json!({"status": "active", "context": "test"});
    let event_id = db.log_event("status", "nova", &data).unwrap();

    // Sub should be updated with new last_id
    let sub_after = db.kv_get("events_sub:test").unwrap().unwrap();
    let sub_val: serde_json::Value = serde_json::from_str(&sub_after).unwrap();
    assert_eq!(sub_val["last_id"], event_id);

    cleanup_test_db(db_path);
}

#[test]
fn test_subscription_once_removes_after_match() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('luna', '', 1000.0)",
            [],
        )
        .unwrap();

    let sub = serde_json::json!({
        "id": "once-sub",
        "caller": "luna",
        "sql": "type = 'status'",
        "once": true,
        "last_id": 0
    });
    db.kv_set("events_sub:once-test", Some(&sub.to_string()))
        .unwrap();

    // Log a matching event
    let data = serde_json::json!({"status": "active"});
    db.log_event("status", "nova", &data).unwrap();

    // Subscription should be removed
    assert!(db.kv_get("events_sub:once-test").unwrap().is_none());

    cleanup_test_db(db_path);
}

#[test]
fn test_subscription_sql_error_graceful() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('luna', '', 1000.0)",
            [],
        )
        .unwrap();

    // Bad SQL subscription
    let bad_sub = serde_json::json!({
        "id": "bad-sql",
        "caller": "luna",
        "sql": "INVALID SQL %%% BROKEN",
        "last_id": 0
    });
    db.kv_set("events_sub:bad", Some(&bad_sub.to_string()))
        .unwrap();

    // Good SQL subscription
    let good_sub = serde_json::json!({
        "id": "good-sql",
        "caller": "luna",
        "sql": "type = 'status'",
        "last_id": 0
    });
    db.kv_set("events_sub:good", Some(&good_sub.to_string()))
        .unwrap();

    // Log a matching event — should not crash despite bad SQL sub
    let data = serde_json::json!({"status": "active"});
    let event_id = db.log_event("status", "nova", &data).unwrap();

    // Bad sub should remain untouched (last_id still 0)
    let bad_after = db.kv_get("events_sub:bad").unwrap().unwrap();
    let bad_val: serde_json::Value = serde_json::from_str(&bad_after).unwrap();
    assert_eq!(bad_val["last_id"], 0, "Bad SQL sub should not advance");

    // Good sub should have fired
    let good_after = db.kv_get("events_sub:good").unwrap().unwrap();
    let good_val: serde_json::Value = serde_json::from_str(&good_after).unwrap();
    assert_eq!(good_val["last_id"], event_id, "Good sub should advance");

    cleanup_test_db(db_path);
}

#[test]
fn test_cancel_request_watches_by_flow() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('requester', '', 1000.0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('responder', '', 1000.0)",
            [],
        )
        .unwrap();

    // Create a request-watch subscription
    let reqwatch = serde_json::json!({
        "id": "reqwatch-test",
        "caller": "requester",
        "sql": "type = 'status'",
        "last_id": 0,
        "once": true,
        "filters": {
            "request_watch": true,
            "target": "responder",
            "request_id": 42
        }
    });
    db.kv_set("events_sub:reqwatch-test", Some(&reqwatch.to_string()))
        .unwrap();

    // Simulate responder replying to requester with reply_to matching request_id
    cancel_request_watches_by_flow(&db, "responder", &["requester".to_string()], Some(42));

    // Subscription should be deleted
    assert!(
        db.kv_get("events_sub:reqwatch-test").unwrap().is_none(),
        "Request-watch should be cancelled when target replies"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_cancel_request_watches_wrong_reply_id() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('requester', '', 1000.0)",
            [],
        )
        .unwrap();

    let reqwatch = serde_json::json!({
        "id": "reqwatch-test2",
        "caller": "requester",
        "sql": "type = 'status'",
        "last_id": 0,
        "once": true,
        "filters": {
            "request_watch": true,
            "target": "responder",
            "request_id": 42
        }
    });
    db.kv_set("events_sub:reqwatch-test2", Some(&reqwatch.to_string()))
        .unwrap();

    // Reply with wrong request_id — should NOT cancel
    cancel_request_watches_by_flow(&db, "responder", &["requester".to_string()], Some(99));

    assert!(
        db.kv_get("events_sub:reqwatch-test2").unwrap().is_some(),
        "Request-watch should NOT be cancelled for mismatched reply_to"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_cancel_request_watches_by_reply_id_via_log_event() {
    // End-to-end: log a broadcast message with reply_to_local → should cancel reqwatch via Path 2
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('requester', '', 1000.0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('responder', '', 1000.0)",
            [],
        )
        .unwrap();

    // First, log a request message so we have an event_id to reply to
    let req_data = serde_json::json!({
        "from": "requester",
        "sender_kind": "instance",
        "scope": "mentions",
        "text": "do the thing",
        "delivered_to": ["responder"],
        "intent": "request",
        "mentions": ["responder"]
    });
    let request_id = db.log_event("message", "requester", &req_data).unwrap();

    // Create a request-watch subscription
    let reqwatch = serde_json::json!({
        "id": format!("reqwatch-{}-responder", request_id),
        "caller": "requester",
        "sql": "(type='status' AND instance=? AND status_val='listening')",
        "params": ["responder"],
        "last_id": request_id,
        "once": true,
        "filters": {
            "request_watch": true,
            "target": "responder",
            "request_id": request_id
        }
    });
    let sub_key = format!("events_sub:reqwatch-{}-responder", request_id);
    db.kv_set(&sub_key, Some(&reqwatch.to_string())).unwrap();

    // Now log a BROADCAST ack from responder with reply_to_local = request_id
    let ack_data = serde_json::json!({
        "from": "responder",
        "sender_kind": "instance",
        "scope": "broadcast",
        "text": "done with the task",
        "delivered_to": ["requester"],
        "intent": "ack",
        "reply_to": request_id.to_string(),
        "reply_to_local": request_id
    });
    db.log_event("message", "responder", &ack_data).unwrap();

    // Reqwatch should be cancelled via Path 2
    assert!(
        db.kv_get(&sub_key).unwrap().is_none(),
        "Request-watch should be cancelled when target sends broadcast with reply_to_local matching request_id"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_subscription_recursion_guard_hcom_events_sender() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('luna', '', 1000.0)",
            [],
        )
        .unwrap();

    let sub = serde_json::json!({
        "id": "test-sub",
        "caller": "luna",
        "sql": "type = 'message'",
        "last_id": 0
    });
    db.kv_set("events_sub:test", Some(&sub.to_string()))
        .unwrap();

    // Log message from [hcom-events] (non-sys_ instance) — guard B should skip
    let data = serde_json::json!({
        "from": "[hcom-events]",
        "text": "notification from events"
    });
    db.log_event("message", "ext_notifier", &data).unwrap();

    // Sub should not be updated
    let sub_after = db.kv_get("events_sub:test").unwrap().unwrap();
    let sub_val: serde_json::Value = serde_json::from_str(&sub_after).unwrap();
    assert_eq!(
        sub_val["last_id"], 0,
        "[hcom-events] sender should be blocked by guard B"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_send_system_message_broadcast() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('nova', 1000.0)",
            [],
        )
        .unwrap();

    // No @mentions = broadcast
    let delivered = db
        .send_system_message("[hcom-test]", "hello everyone")
        .unwrap();
    assert_eq!(delivered.len(), 2);
    assert!(delivered.contains(&"luna".to_string()));
    assert!(delivered.contains(&"nova".to_string()));

    cleanup_test_db(db_path);
}

#[test]
fn test_send_system_message_targeted() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('luna', '', 1000.0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('nova', '', 1000.0)",
            [],
        )
        .unwrap();

    // With @mention = targeted
    let delivered = db
        .send_system_message("[hcom-test]", "@luna your task is done")
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert!(delivered.contains(&"luna".to_string()));

    cleanup_test_db(db_path);
}

#[test]
fn test_send_system_message_with_tag() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('luna', 'api', 1000.0)",
            [],
        )
        .unwrap();

    // Mention by full name (tag-name)
    let delivered = db
        .send_system_message("[hcom-test]", "@api-luna your task is done")
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert!(delivered.contains(&"luna".to_string()));

    cleanup_test_db(db_path);
}

#[test]
fn test_send_sub_notification_wakes_target_instance() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES ('tofu', '', 1000.0), ('rune', '', 1000.0)",
            [],
        )
        .unwrap();
    let tofu_probe = bind_probe();
    let rune_probe = bind_probe();
    db.upsert_notify_endpoint("tofu", "plugin", tofu_probe.local_addr().unwrap().port())
        .unwrap();
    db.upsert_notify_endpoint("rune", "plugin", rune_probe.local_addr().unwrap().port())
        .unwrap();

    assert!(send_sub_notification(
        &db,
        "tofu",
        "[sub:test] #42 dani status | blocked | approval"
    ));

    assert!(
        await_connect(&tofu_probe, Duration::from_millis(500)),
        "subscription notification should wake the subscribed instance"
    );
    assert!(
        !await_connect(&rune_probe, Duration::from_millis(100)),
        "subscription notification should not broadcast-wake unrelated instances"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_send_system_message_exact_name_avoids_tag_prefix_collision() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tag, created_at) VALUES
             ('giru', '', 1000.0),
             ('lasa', 'giru-test', 1000.0)",
            [],
        )
        .unwrap();

    let delivered = db
        .send_system_message("[hcom-test]", "@giru request timed out")
        .unwrap();
    assert_eq!(delivered, vec!["giru"]);

    let (mentions, delivered_to): (String, String) = db
        .conn
        .query_row(
            "SELECT json_extract(data, '$.mentions'),
                    json_extract(data, '$.delivered_to')
             FROM events
             WHERE type = 'message'
             ORDER BY id DESC
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(mentions, r#"["giru"]"#);
    assert_eq!(delivered_to, r#"["giru"]"#);

    cleanup_test_db(db_path);
}

#[test]
fn test_reqwatch_reply_requires_exact_delivered_to_member() {
    let (db, db_path) = setup_full_test_db();

    let near_match = serde_json::json!({
        "from": "lasa",
        "scope": "mentions",
        "mentions": ["giru2"],
        "delivered_to": ["giru2"],
        "text": "not for giru",
    });
    let near_id = db.log_event("message", "lasa", &near_match).unwrap();
    assert!(!reqwatch_reply_exists(&db, 0, "lasa", "giru"));

    let exact_match = serde_json::json!({
        "from": "lasa",
        "scope": "mentions",
        "mentions": ["giru"],
        "delivered_to": ["giru"],
        "text": "for giru",
    });
    db.log_event("message", "lasa", &exact_match).unwrap();
    assert!(reqwatch_reply_exists(&db, near_id, "lasa", "giru"));

    cleanup_test_db(db_path);
}

#[test]
fn test_on_hit_provenance_instance_caller() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let sub = serde_json::json!({
        "id": "sub-onhit1",
        "caller": "luna",
        "caller_kind": "instance",
        "sql": "type = 'message' AND msg_from = 'nova'",
        "created": 1000.0,
        "last_id": 0,
        "once": false,
        "on_hit_text": "starting review now",
    });
    db.kv_set("events_sub:sub-onhit1", Some(&sub.to_string()))
        .unwrap();

    db.log_event(
        "message",
        "nova",
        &serde_json::json!({
            "from": "nova",
            "sender_kind": "instance",
            "scope": "broadcast",
            "text": "heads up",
            "delivered_to": ["luna"],
        }),
    )
    .unwrap();

    // Find the on-hit event: from=luna, sender_kind=instance, text matches
    let row: Option<(String, String)> = db
        .conn
        .query_row(
            "SELECT json_extract(data, '$.sender_kind'), json_extract(data, '$.text') \
             FROM events WHERE json_extract(data, '$.from') = 'luna' \
             AND json_extract(data, '$.text') = 'starting review now' LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok();
    assert!(row.is_some(), "on-hit message should be logged");
    let (kind, text) = row.unwrap();
    assert_eq!(
        kind, "instance",
        "caller 'luna' is an instance → sender_kind=instance"
    );
    assert_eq!(
        text, "starting review now",
        "on-hit text sent verbatim, no @-prefix"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_on_hit_external_caller_and_mention_routing() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('dbadmin', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    // Caller 'bigboss' is NOT in instances → external kind.
    // on_hit_text mentions @dbadmin → normal mention routing must deliver to dbadmin only.
    let sub = serde_json::json!({
        "id": "sub-onhit2",
        "caller": "bigboss",
        "caller_kind": "external",
        "sql": "type = 'message' AND msg_from = 'nova'",
        "created": 1000.0,
        "last_id": 0,
        "once": false,
        "on_hit_text": "@dbadmin review the change",
    });
    db.kv_set("events_sub:sub-onhit2", Some(&sub.to_string()))
        .unwrap();

    db.log_event(
        "message",
        "nova",
        &serde_json::json!({
            "from": "nova",
            "sender_kind": "instance",
            "scope": "broadcast",
            "text": "trigger",
            "delivered_to": ["dbadmin"],
        }),
    )
    .unwrap();

    let row: Option<(String, String, String)> = db
        .conn
        .query_row(
            "SELECT json_extract(data, '$.sender_kind'), \
                    json_extract(data, '$.scope'), \
                    json_extract(data, '$.delivered_to') \
             FROM events WHERE json_extract(data, '$.from') = 'bigboss' LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .ok();
    assert!(
        row.is_some(),
        "on-hit message from bigboss should be logged"
    );
    let (kind, scope, delivered) = row.unwrap();
    assert_eq!(
        kind, "external",
        "non-instance caller → sender_kind=external"
    );
    assert_eq!(scope, "mentions", "text contains @mention → mentions scope");
    assert!(
        delivered.contains("dbadmin"),
        "delivered_to must include dbadmin"
    );
    assert!(
        !delivered.contains("bigboss"),
        "caller itself is not auto-mentioned"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_on_hit_caller_kind_captured_at_creation() {
    // Verify resolve_caller_kind via create_filter_subscription:
    // instance caller → caller_kind=instance
    // non-instance caller (e.g. bigboss from -b) → caller_kind=external
    use std::collections::HashMap;

    let (db, db_path) = setup_full_test_db();
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("agent".to_string(), vec!["luna".to_string()]);
    filters.insert("status".to_string(), vec!["listening".to_string()]);

    create_filter_subscription(&db, &filters, &[], "luna", false, Some("hi")).unwrap();
    create_filter_subscription(&db, &filters, &[], "bigboss", false, Some("hi")).unwrap();

    let luna_kind: String = db
        .conn
        .query_row(
            "SELECT json_extract(value, '$.caller_kind') FROM kv \
             WHERE key LIKE 'events_sub:%' \
             AND json_extract(value, '$.caller') = 'luna' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(luna_kind, "instance");

    let bb_kind: String = db
        .conn
        .query_row(
            "SELECT json_extract(value, '$.caller_kind') FROM kv \
             WHERE key LIKE 'events_sub:%' \
             AND json_extract(value, '$.caller') = 'bigboss' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(bb_kind, "external");

    cleanup_test_db(db_path);
}

#[test]
fn test_on_hit_provenance_stable_after_caller_stops() {
    // Sub created by an instance stays sender_kind=instance at fire time
    // even if that instance row has been deleted before the match.
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let sub = serde_json::json!({
        "id": "sub-stab1",
        "caller": "luna",
        "caller_kind": "instance",
        "sql": "type = 'message' AND msg_from = 'nova'",
        "created": 1000.0,
        "last_id": 0,
        "once": false,
        "on_hit_text": "still luna",
    });
    db.kv_set("events_sub:sub-stab1", Some(&sub.to_string()))
        .unwrap();

    // Caller disappears before the sub fires.
    db.conn
        .execute("DELETE FROM instances WHERE name = 'luna'", [])
        .unwrap();

    db.log_event(
        "message",
        "nova",
        &serde_json::json!({
            "from": "nova",
            "sender_kind": "instance",
            "scope": "broadcast",
            "text": "trigger",
            "delivered_to": [],
        }),
    )
    .unwrap();

    let kind: Option<String> = db
        .conn
        .query_row(
            "SELECT json_extract(data, '$.sender_kind') FROM events \
             WHERE json_extract(data, '$.from') = 'luna' \
             AND json_extract(data, '$.text') = 'still luna' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    assert_eq!(
        kind.as_deref(),
        Some("instance"),
        "provenance captured at creation must survive caller stop"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_on_hit_unmatched_mention_delivers_to_nobody() {
    // Documents current behavior: an on-hit text mentioning a nonexistent
    // agent produces a mentions-scope event with empty delivered_to.
    // This mirrors how send_system_message behaves for typos — no error,
    // no fallback to broadcast. If we ever tighten mention validation for
    // on-hit, update this test.
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let sub = serde_json::json!({
        "id": "sub-typo1",
        "caller": "luna",
        "caller_kind": "instance",
        "sql": "type = 'message' AND msg_from = 'nova'",
        "created": 1000.0,
        "last_id": 0,
        "once": false,
        "on_hit_text": "@notarealagent hello",
    });
    db.kv_set("events_sub:sub-typo1", Some(&sub.to_string()))
        .unwrap();

    db.log_event(
        "message",
        "nova",
        &serde_json::json!({
            "from": "nova",
            "sender_kind": "instance",
            "scope": "broadcast",
            "text": "trigger",
            "delivered_to": [],
        }),
    )
    .unwrap();

    let row: Option<(String, String)> = db
        .conn
        .query_row(
            "SELECT json_extract(data, '$.scope'), \
                    json_extract(data, '$.delivered_to') \
             FROM events WHERE json_extract(data, '$.from') = 'luna' \
             AND json_extract(data, '$.text') = '@notarealagent hello' LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok();
    assert!(row.is_some(), "on-hit message should still be logged");
    let (scope, delivered) = row.unwrap();
    assert_eq!(
        scope, "mentions",
        "unmatched @ still produces mentions scope"
    );
    assert_eq!(delivered, "[]", "nobody matched → empty delivered_to");

    cleanup_test_db(db_path);
}

#[test]
fn test_delivery_only_subscription_does_not_emit_notifications() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let member = serde_json::json!({
        "id": "sub-thread123",
        "caller": "luna",
        "thread_name": "debate-1",
        "auto_thread_member": true,
        "delivery_only": true,
        "created": 1000.0,
        "last_id": 0,
        "once": false
    });
    db.kv_set("events_sub:sub-thread123", Some(&member.to_string()))
        .unwrap();

    let data = serde_json::json!({
        "from": "nova",
        "sender_kind": "instance",
        "scope": "broadcast",
        "text": "hello",
        "delivered_to": ["luna"]
    });
    db.log_event("message", "nova", &data).unwrap();

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "delivery-only subscriptions must not create notifications"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_delivery_only_subscription_does_not_emit_status_notifications() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let member = serde_json::json!({
        "id": "sub-thread123",
        "caller": "luna",
        "thread_name": "debate-1",
        "auto_thread_member": true,
        "delivery_only": true,
        "created": 1000.0,
        "last_id": 0,
        "once": false
    });
    db.kv_set("events_sub:sub-thread123", Some(&member.to_string()))
        .unwrap();

    let data = serde_json::json!({
        "status": "active",
        "context": "tool:shell",
        "detail": "hcom listen 1 --name nova"
    });
    db.log_event("status", "nova", &data).unwrap();

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "delivery-only subscriptions must not create status notifications"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_cleanup_subscriptions_keeps_delivery_only_memberships() {
    let (db, db_path) = setup_full_test_db();

    let normal = serde_json::json!({
        "id": "sub-normal",
        "caller": "luna",
        "sql": "type = 'message'",
        "last_id": 0
    });
    let thread_member = serde_json::json!({
        "id": "sub-thread",
        "caller": "luna",
        "thread_name": "debate-1",
        "auto_thread_member": true,
        "delivery_only": true,
        "created": 1000.0,
        "last_id": 0
    });
    db.kv_set("events_sub:sub-normal", Some(&normal.to_string()))
        .unwrap();
    db.kv_set("events_sub:sub-thread", Some(&thread_member.to_string()))
        .unwrap();

    let deleted = db.cleanup_subscriptions("luna").unwrap();
    assert_eq!(deleted, 1);
    assert!(db.kv_get("events_sub:sub-normal").unwrap().is_none());
    assert!(db.kv_get("events_sub:sub-thread").unwrap().is_some());

    cleanup_test_db(db_path);
}

#[test]
fn test_get_thread_members_filters_stale_names() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    db.add_thread_memberships(
        "debate-1",
        Some("luna"),
        &["nova".to_string(), "ghost".to_string()],
    );

    let stored: String = db
        .conn
        .query_row(
            "SELECT value FROM kv WHERE key = ?",
            params![format!(
                "events_sub:{}",
                thread_membership_sub_id("debate-1", "luna")
            )],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.contains("\"sql\":\"0\""));

    assert_eq!(
        db.get_thread_members("debate-1"),
        vec!["nova".to_string(), "luna".to_string()]
    );

    cleanup_test_db(db_path);
}
