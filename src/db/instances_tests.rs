use super::super::HcomDb;
use super::super::tests::{cleanup_test_db, setup_full_test_db};
use rusqlite::params;

fn reopen_broken_schema(db_path: &std::path::Path) -> HcomDb {
    // Use open_raw here: open_at would repair the table we deliberately dropped.
    HcomDb::open_raw(db_path).unwrap()
}

#[test]
fn test_get_instance_status_propagates_prepare_error() {
    // Verify that SQL errors are propagated as Err (not silently converted to None)
    let (db, db_path) = setup_full_test_db();

    // Drop the instances table to cause SQL error
    db.conn().execute("DROP TABLE instances", []).unwrap();
    drop(db);

    // Now HcomDb will fail when trying to query
    let db = reopen_broken_schema(&db_path);

    let result = db.get_instance_status("test");

    // SQL error should be propagated as Err, not None
    let err = result.expect_err("SQL error should propagate as Err, not None");
    assert!(
        err.to_string().contains("instances"),
        "expected missing instances table error, got: {err:#}"
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_get_instance_status_returns_ok_none_when_not_found() {
    // Verify that "not found" is distinguished from "error" via Ok(None)

    let (db, db_path) = setup_full_test_db();

    // Query non-existent instance
    let result = db.get_instance_status("nonexistent");

    // Should be Ok(None) - not found is not an error
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());

    cleanup_test_db(db_path);
}

#[test]
fn test_get_status_propagates_prepare_error() {
    let (db, db_path) = setup_full_test_db();
    db.conn().execute("DROP TABLE instances", []).unwrap();
    drop(db);

    let db = reopen_broken_schema(&db_path);
    let result = db.get_status("test");

    let err = result.expect_err("SQL error should propagate as Err");
    assert!(
        err.to_string().contains("instances"),
        "expected missing instances table error, got: {err:#}"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_get_transcript_path_propagates_prepare_error() {
    let (db, db_path) = setup_full_test_db();
    db.conn().execute("DROP TABLE instances", []).unwrap();
    drop(db);

    let db = reopen_broken_schema(&db_path);
    let result = db.get_transcript_path("test");

    let err = result.expect_err("SQL error should propagate as Err");
    assert!(
        err.to_string().contains("instances"),
        "expected missing instances table error, got: {err:#}"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_get_instance_snapshot_propagates_prepare_error() {
    let (db, db_path) = setup_full_test_db();
    db.conn().execute("DROP TABLE instances", []).unwrap();
    drop(db);

    let db = reopen_broken_schema(&db_path);
    let result = db.get_instance_snapshot("test");

    let err = result.expect_err("SQL error should propagate as Err");
    assert!(
        err.to_string().contains("instances"),
        "expected missing instances table error, got: {err:#}"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_set_status_does_not_emit_launch_ready() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tool, created_at, status, status_context) VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["luna", "codex", 1.0f64, "inactive", "new"],
        )
        .unwrap();

    db.set_status("luna", "listening", "start").unwrap();

    let (status, context) = db.get_status("luna").unwrap().unwrap();
    assert_eq!(status, "listening");
    assert_eq!(context, "start");

    let ready_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND json_extract(data, '$.action') = 'ready'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ready_count, 0);

    cleanup_test_db(db_path);
}

#[test]
fn test_store_launch_context_merges_late_pty_metadata() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, tool, created_at, launch_context) VALUES (?1, ?2, ?3, ?4)",
            params![
                "luna",
                "codex",
                1.0f64,
                r#"{"process_id":"proc-1","terminal_preset_effective":"kitty-split","terminal_preset":"kitty-split"}"#
            ],
        )
        .unwrap();

    db.store_launch_context(
        "luna",
        r#"{"process_id":"proc-2","kitty_listen_on":"unix:/tmp/kitty","terminal_id":"11","pane_id":"11"}"#,
    )
    .unwrap();

    let launch_context: String = db
        .conn
        .query_row(
            "SELECT launch_context FROM instances WHERE name = ?",
            params!["luna"],
            |row| row.get(0),
        )
        .unwrap();
    let launch_context: serde_json::Value = serde_json::from_str(&launch_context).unwrap();

    assert_eq!(launch_context["process_id"], "proc-1");
    assert_eq!(launch_context["terminal_preset_effective"], "kitty-split");
    assert_eq!(launch_context["terminal_preset"], "kitty-split");
    assert_eq!(launch_context["kitty_listen_on"], "unix:/tmp/kitty");
    assert_eq!(launch_context["terminal_id"], "11");
    assert_eq!(launch_context["pane_id"], "11");

    cleanup_test_db(db_path);
}

#[test]
fn test_save_and_get_instance() {
    let (db, db_path) = setup_full_test_db();

    let mut data = std::collections::HashMap::new();
    data.insert("name".to_string(), serde_json::json!("luna"));
    data.insert("tool".to_string(), serde_json::json!("claude"));
    data.insert("created_at".to_string(), serde_json::json!(1000.0));
    data.insert("status".to_string(), serde_json::json!("active"));

    db.save_instance(&data).unwrap();

    let inst = db.get_instance("luna").unwrap().unwrap();
    assert_eq!(inst["name"], "luna");
    assert_eq!(inst["tool"], "claude");
    assert_eq!(inst["status"], "active");

    cleanup_test_db(db_path);
}

#[test]
fn test_update_instance() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, status, created_at) VALUES ('luna', 'active', 1000.0)",
            [],
        )
        .unwrap();

    let mut updates = std::collections::HashMap::new();
    updates.insert("status".to_string(), serde_json::json!("listening"));
    updates.insert("tag".to_string(), serde_json::json!("api"));

    db.update_instance("luna", &updates).unwrap();

    let inst = db.get_instance("luna").unwrap().unwrap();
    assert_eq!(inst["status"], "listening");
    assert_eq!(inst["tag"], "api");

    cleanup_test_db(db_path);
}

#[test]
fn test_iter_instances() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 2000.0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let instances = db.iter_instances().unwrap();
    assert_eq!(instances.len(), 2);
    // Should be ordered by created_at DESC
    assert_eq!(instances[0]["name"], "luna");
    assert_eq!(instances[1]["name"], "nova");

    cleanup_test_db(db_path);
}

/// Recreate session_bindings without the `ON DELETE CASCADE` FK. No
/// migration in this file touches session_bindings and init_db uses
/// `CREATE TABLE IF NOT EXISTS`, so an existing table's shape is never
/// rebuilt on upgrade — this pins the explicit delete below as correct
/// independent of whether FK enforcement happens to cover it.
fn drop_session_bindings_cascade(db: &HcomDb) {
    db.conn()
        .execute_batch(
            "DROP TABLE session_bindings;
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             CREATE INDEX idx_session_bindings_instance ON session_bindings(instance_name);",
        )
        .unwrap();
}

#[test]
fn finalize_instance_stop_deletes_all_session_aliases() {
    let (db, db_path) = setup_full_test_db();
    drop_session_bindings_cascade(&db);
    db.conn()
        .execute(
            "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
             VALUES ('zilo', 'uuid-a', 'cursor', 'listening', 'start', 0, 1.0, 0)",
            [],
        )
        .unwrap();
    db.rebind_session("uuid-a", "zilo").unwrap();
    db.rebind_session("uuid-b", "zilo").unwrap();
    let won = db
        .finalize_instance_stop(
            "zilo",
            1.0,
            Some("uuid-a"),
            None,
            &serde_json::json!({"action":"stopped"}),
        )
        .unwrap();
    assert!(won);
    assert_eq!(db.get_session_binding("uuid-a").unwrap(), None);
    assert_eq!(db.get_session_binding("uuid-b").unwrap(), None);
    assert!(db.get_instance_full("zilo").unwrap().is_none());

    cleanup_test_db(db_path);
}

fn insert_basic_instance(db: &HcomDb, name: &str, status: &str, pid: i64, created_at: f64) {
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, pid, last_event_id)
             VALUES (?, 'claude', ?, 'start', 0, ?, ?, 0)",
            params![name, status, created_at, pid],
        )
        .unwrap();
}

fn life_event_count(db: &HcomDb, name: &str) -> i64 {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = ?",
            params![name],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn finalize_instance_stop_guarded_rejects_stale_created_at() {
    let (db, db_path) = setup_full_test_db();
    insert_basic_instance(&db, "mochi", "active", 111, 1.0);

    // A resume replaces the row under the same name with a new lifetime.
    db.conn()
        .execute(
            "UPDATE instances SET created_at = 2.0 WHERE name = 'mochi'",
            [],
        )
        .unwrap();

    let won = db
        .finalize_instance_stop_guarded(
            "mochi",
            1.0, // stale created_at read before the resume
            None,
            None,
            Some(111),
            "",
            "active",
            &serde_json::json!({"action": "stopped"}),
        )
        .unwrap();
    assert!(!won, "stale created_at must not finalize the resumed row");
    assert!(db.get_instance_full("mochi").unwrap().is_some());
    assert_eq!(life_event_count(&db, "mochi"), 0);

    cleanup_test_db(db_path);
}

#[test]
fn finalize_instance_stop_guarded_rejects_stale_pid_or_status() {
    let (db, db_path) = setup_full_test_db();
    insert_basic_instance(&db, "poko", "active", 222, 5.0);

    // Rebound to a new PID before the reaper's guarded finalize runs.
    db.conn()
        .execute("UPDATE instances SET pid = 333 WHERE name = 'poko'", [])
        .unwrap();
    let stale_pid = db
        .finalize_instance_stop_guarded(
            "poko",
            5.0,
            None,
            None,
            Some(222),
            "",
            "active",
            &serde_json::json!({"action": "stopped"}),
        )
        .unwrap();
    assert!(
        !stale_pid,
        "stale pid guard must not finalize a rebound row"
    );
    assert!(db.get_instance_full("poko").unwrap().is_some());

    // Status changed (e.g. active -> listening) before finalize runs.
    db.conn()
        .execute(
            "UPDATE instances SET status = 'listening' WHERE name = 'poko'",
            [],
        )
        .unwrap();
    let stale_status = db
        .finalize_instance_stop_guarded(
            "poko",
            5.0,
            None,
            None,
            Some(333),
            "",
            "active",
            &serde_json::json!({"action": "stopped"}),
        )
        .unwrap();
    assert!(
        !stale_status,
        "stale status guard must not finalize a re-statused row"
    );
    assert!(db.get_instance_full("poko").unwrap().is_some());

    // Matching the row's *current* pid/status wins.
    let won = db
        .finalize_instance_stop_guarded(
            "poko",
            5.0,
            None,
            None,
            Some(333),
            "",
            "listening",
            &serde_json::json!({"action": "stopped"}),
        )
        .unwrap();
    assert!(won);
    assert!(db.get_instance_full("poko").unwrap().is_none());
    assert_eq!(life_event_count(&db, "poko"), 1);

    cleanup_test_db(db_path);
}

#[test]
fn finalize_instance_stop_guarded_rolls_back_on_event_insert_failure() {
    let (db, db_path) = setup_full_test_db();
    insert_basic_instance(&db, "ren", "active", 444, 9.0);
    db.conn()
        .execute_batch(
            "CREATE TRIGGER reject_life_guarded BEFORE INSERT ON events
             WHEN NEW.type = 'life' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        )
        .unwrap();

    let result = db.finalize_instance_stop_guarded(
        "ren",
        9.0,
        None,
        None,
        Some(444),
        "",
        "active",
        &serde_json::json!({"action": "stopped"}),
    );
    assert!(
        result.is_err(),
        "event insert failure must surface as Err, not a false no-op"
    );
    assert!(
        db.get_instance_full("ren").unwrap().is_some(),
        "the delete must roll back together with the failed event insert"
    );

    db.conn()
        .execute_batch("DROP TRIGGER reject_life_guarded")
        .unwrap();
    let won = db
        .finalize_instance_stop_guarded(
            "ren",
            9.0,
            None,
            None,
            Some(444),
            "",
            "active",
            &serde_json::json!({"action": "stopped"}),
        )
        .unwrap();
    assert!(won);
    assert_eq!(
        life_event_count(&db, "ren"),
        1,
        "retrying after the trigger is gone must publish exactly one event"
    );

    cleanup_test_db(db_path);
}

#[test]
fn finalize_instance_stop_race_pty_then_reaper_yields_one_event() {
    let (db, db_path) = setup_full_test_db();
    insert_basic_instance(&db, "kobi", "active", 555, 3.0);

    // PTY exit finalizer (unguarded) wins first.
    let pty_won = db
        .finalize_instance_stop(
            "kobi",
            3.0,
            None,
            None,
            &serde_json::json!({"action": "stopped", "reason": "pty_exit"}),
        )
        .unwrap();
    assert!(pty_won);

    // The reaper detects the same dead PID a moment later and races in.
    let reaper_won = db
        .finalize_instance_stop_guarded(
            "kobi",
            3.0,
            None,
            None,
            Some(555),
            "",
            "active",
            &serde_json::json!({"action": "stopped", "reason": "exit:dead_process"}),
        )
        .unwrap();
    assert!(
        !reaper_won,
        "the losing side must see a normal no-op, not an error"
    );
    assert_eq!(life_event_count(&db, "kobi"), 1);

    cleanup_test_db(db_path);
}

#[test]
fn finalize_instance_stop_race_reaper_then_pty_yields_one_event() {
    let (db, db_path) = setup_full_test_db();
    insert_basic_instance(&db, "sula", "active", 666, 4.0);

    // The reaper (guarded) wins the race this time.
    let reaper_won = db
        .finalize_instance_stop_guarded(
            "sula",
            4.0,
            None,
            None,
            Some(666),
            "",
            "active",
            &serde_json::json!({"action": "stopped", "reason": "exit:dead_process"}),
        )
        .unwrap();
    assert!(reaper_won);

    // PTY exit finalizer arrives after; the row is already gone.
    let pty_won = db
        .finalize_instance_stop(
            "sula",
            4.0,
            None,
            None,
            &serde_json::json!({"action": "stopped", "reason": "pty_exit"}),
        )
        .unwrap();
    assert!(
        !pty_won,
        "the losing side must see a normal no-op, not an error"
    );
    assert_eq!(life_event_count(&db, "sula"), 1);

    cleanup_test_db(db_path);
}

#[test]
fn clear_gate_status_only_clears_our_own_context() {
    use crate::shared::ST_ACTIVE;
    let (db, db_path) = setup_full_test_db();
    db.conn
        .execute(
            "INSERT INTO instances (name, tool, created_at, status, status_context) VALUES (?1, ?2, ?3, ?4, ?5)",
            params!["nova", "antigravity", 1.0f64, "listening", "start"],
        )
        .unwrap();

    // Our own row: both columns cleared.
    db.set_gate_status("nova", "tui:prompt-has-text:stalled", "gate blocked 60s")
        .unwrap();
    assert!(
        db.clear_gate_status_if("nova", "tui:prompt-has-text:stalled")
            .unwrap()
    );
    let (_, context) = db.get_status("nova").unwrap().unwrap();
    assert_eq!(context, "");
    assert_eq!(db.get_instance_status("nova").unwrap().unwrap().detail, "");

    // A hook wrote its own context AND detail after ours. Neither may move.
    db.set_gate_status("nova", "tui:prompt-has-text:stalled", "gate blocked 60s")
        .unwrap();
    db.set_status("nova", ST_ACTIVE, "tool:Bash").unwrap();
    db.conn
        .execute(
            "UPDATE instances SET status_detail = 'running tests' WHERE name = ?1",
            params!["nova"],
        )
        .unwrap();
    assert!(
        !db.clear_gate_status_if("nova", "tui:prompt-has-text:stalled")
            .unwrap()
    );
    let (status, context) = db.get_status("nova").unwrap().unwrap();
    assert_eq!(status, ST_ACTIVE);
    assert_eq!(context, "tool:Bash");
    assert_eq!(
        db.get_instance_status("nova").unwrap().unwrap().detail,
        "running tests"
    );

    // A hand-joined instance keeps its cmd:listen detail, as set_gate_status does.
    db.set_status("nova", "listening", "start").unwrap();
    db.set_gate_status("nova", "tui:not-idle:stalled", "gate blocked 60s")
        .unwrap();
    db.conn
        .execute(
            "UPDATE instances SET status_detail = 'cmd:listen' WHERE name = ?1",
            params!["nova"],
        )
        .unwrap();
    assert!(
        db.clear_gate_status_if("nova", "tui:not-idle:stalled")
            .unwrap()
    );
    assert_eq!(
        db.get_instance_status("nova").unwrap().unwrap().detail,
        "cmd:listen"
    );

    cleanup_test_db(db_path);
}
