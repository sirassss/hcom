use super::super::HcomDb;
use super::super::tests::{cleanup_test_db, setup_full_test_db};
use rusqlite::params;
use serial_test::serial;

fn reopen_broken_schema(db_path: &std::path::Path) -> HcomDb {
    // Use open_raw here: open_at would repair the table we deliberately dropped.
    HcomDb::open_raw(db_path).unwrap()
}

// Regression: a deleted/missing instance row must not make has_pending fall back
// to cursor 0, which would treat the whole channel backlog (broadcasts match every
// recipient) as unread and replay a stale message into a freshly-resumed session.
#[test]
fn test_has_pending_false_for_missing_instance() {
    let (db, db_path) = setup_full_test_db();

    // A broadcast in history (delivers to all recipients).
    db.conn
        .execute(
            "INSERT INTO events (type, timestamp, instance, data)
             VALUES ('message', '2026-01-01T00:00:00Z', 'kera',
                     '{\"from\":\"kera\",\"scope\":\"broadcast\",\"text\":\"ack\"}')",
            [],
        )
        .unwrap();

    // No instance row named "ghost" exists.
    assert!(
        !db.has_pending("ghost"),
        "missing instance must have nothing pending, not the full backlog"
    );

    // Sanity: a real instance with cursor 0 still sees the broadcast.
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at, last_event_id) VALUES ('real', 1.0, 0)",
            [],
        )
        .unwrap();
    assert!(db.has_pending("real"));

    cleanup_test_db(db_path);
}

#[test]
fn test_get_process_binding_propagates_prepare_error() {
    let (db, db_path) = setup_full_test_db();
    db.conn()
        .execute("DROP TABLE process_bindings", [])
        .unwrap();
    drop(db);

    let db = reopen_broken_schema(&db_path);
    let result = db.get_process_binding("test_pid");

    let err = result.expect_err("SQL error should propagate as Err");
    assert!(
        err.to_string().contains("process_bindings"),
        "expected missing process_bindings table error, got: {err:#}"
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_session_binding_crud() {
    let (db, db_path) = setup_full_test_db();

    // Create instance first (FK constraint)
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    // No binding initially
    assert!(db.get_session_binding("sess-1").unwrap().is_none());

    // Set binding
    db.set_session_binding("sess-1", "luna").unwrap();
    assert_eq!(
        db.get_session_binding("sess-1").unwrap(),
        Some("luna".to_string())
    );

    // has_session_binding
    assert!(db.has_session_binding("luna"));

    // Delete binding
    db.delete_session_binding("sess-1").unwrap();
    assert!(db.get_session_binding("sess-1").unwrap().is_none());
    assert!(!db.has_session_binding("luna"));

    cleanup_test_db(db_path);
}

#[test]
fn test_session_binding_conflict() {
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

    // Bind session to luna
    db.set_session_binding("sess-1", "luna").unwrap();

    // Try binding same session to nova - should fail
    let result = db.set_session_binding("sess-1", "nova");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("already bound to luna")
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_rebind_session() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, session_id, created_at) VALUES ('luna', 'sess-1', 1000.0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('nova', 1000.0)",
            [],
        )
        .unwrap();

    // Bind to luna first
    db.set_session_binding("sess-1", "luna").unwrap();

    // Rebind to nova (should clear from luna)
    db.rebind_session("sess-1", "nova").unwrap();
    assert_eq!(
        db.get_session_binding("sess-1").unwrap(),
        Some("nova".to_string())
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_rebind_instance_session() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    db.rebind_instance_session("luna", "sess-new").unwrap();
    assert_eq!(
        db.get_session_binding("sess-new").unwrap(),
        Some("luna".to_string())
    );

    cleanup_test_db(db_path);
}

#[test]
fn test_process_binding_crud() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    // Set process binding
    db.set_process_binding("pid-123", "sess-1", "luna").unwrap();
    assert!(db.has_process_binding_for_instance("luna"));

    // Get binding
    let name = db.get_process_binding("pid-123").unwrap();
    assert_eq!(name, Some("luna".to_string()));

    // Delete
    db.delete_process_binding("pid-123").unwrap();
    assert!(!db.has_process_binding_for_instance("luna"));

    cleanup_test_db(db_path);
}

#[test]
fn test_attach_claude_generation_preserves_aliases_and_refreshes_only_current_process() {
    let (db, db_path) = setup_full_test_db();
    for (name, session_id) in [("niza", "sess-old"), ("lava", "sess-lava")] {
        db.conn
            .execute(
                "INSERT INTO instances (name, session_id, created_at) VALUES (?1, ?2, 1000.0)",
                params![name, session_id],
            )
            .unwrap();
        db.set_session_binding(session_id, name).unwrap();
    }
    db.set_process_binding("pid-niza-current", "sess-old", "niza")
        .unwrap();
    db.set_process_binding("pid-niza-historical", "sess-older", "niza")
        .unwrap();
    db.set_process_binding("pid-lava", "sess-lava", "lava")
        .unwrap();

    assert!(
        !db.attach_claude_generation(
            "niza",
            "sess-new",
            "/tmp/new.jsonl",
            "pid-niza-current",
            Some("niza"),
        )
        .unwrap()
    );
    assert_eq!(
        db.get_session_binding("sess-old").unwrap().as_deref(),
        Some("niza")
    );
    assert_eq!(
        db.get_session_binding("sess-new").unwrap().as_deref(),
        Some("niza")
    );
    assert_eq!(
        db.get_validated_claude_session_owner("sess-new")
            .unwrap()
            .as_deref(),
        Some("niza")
    );
    assert_eq!(
        db.get_process_binding_full("pid-niza-current").unwrap(),
        Some((Some("sess-new".to_string()), "niza".to_string()))
    );
    assert_eq!(
        db.get_process_binding_full("pid-niza-historical").unwrap(),
        Some((Some("sess-older".to_string()), "niza".to_string()))
    );
    assert_eq!(
        db.get_process_binding_full("pid-lava").unwrap(),
        Some((Some("sess-lava".to_string()), "lava".to_string()))
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_attach_claude_generation_reparents_existing_subagents() {
    let (db, db_path) = setup_full_test_db();
    db.conn
        .execute(
            "INSERT INTO instances (name, session_id, created_at) VALUES ('niza', 'sess-old', 1000.0)",
            [],
        )
        .unwrap();
    db.set_session_binding("sess-old", "niza").unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (
                name, parent_session_id, parent_name, agent_id, created_at
             ) VALUES ('niza-child', 'sess-old', 'niza', 'agent-1', 1001.0)",
            [],
        )
        .unwrap();
    db.set_process_binding("pid-niza", "sess-old", "niza")
        .unwrap();

    db.attach_claude_generation(
        "niza",
        "sess-new",
        "/tmp/new.jsonl",
        "pid-niza",
        Some("niza"),
    )
    .unwrap();

    assert_eq!(
        db.get_instance_full("niza-child")
            .unwrap()
            .unwrap()
            .parent_session_id
            .as_deref(),
        Some("sess-new")
    );
    assert_eq!(
        db.get_session_binding("sess-old").unwrap().as_deref(),
        Some("niza")
    );
    assert_eq!(
        db.get_session_binding("sess-new").unwrap().as_deref(),
        Some("niza")
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_attach_claude_generation_detaches_displaced_owner_subagents() {
    let (db, db_path) = setup_full_test_db();
    for (name, session_id) in [("niza", "sess-old"), ("lava", "sess-new")] {
        db.conn
            .execute(
                "INSERT INTO instances (name, session_id, created_at) VALUES (?1, ?2, 1000.0)",
                params![name, session_id],
            )
            .unwrap();
        db.set_session_binding(session_id, name).unwrap();
    }
    db.conn
        .execute(
            "INSERT INTO instances (
                name, parent_session_id, parent_name, agent_id, created_at
             ) VALUES ('lava-child', 'sess-new', 'lava', 'agent-lava', 1001.0)",
            [],
        )
        .unwrap();

    db.attach_claude_generation("niza", "sess-new", "/tmp/new.jsonl", "", None)
        .unwrap();

    assert_eq!(
        db.get_instance_full("niza")
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("sess-new")
    );
    assert_eq!(
        db.get_instance_full("lava").unwrap().unwrap().session_id,
        None
    );
    let child = db.get_instance_full("lava-child").unwrap().unwrap();
    assert_eq!(child.parent_session_id, None);
    assert_eq!(child.parent_name.as_deref(), Some("lava"));
    assert_eq!(
        db.get_session_binding("sess-new").unwrap().as_deref(),
        Some("niza")
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_attach_claude_generation_deletes_only_stale_conflicting_process() {
    let (db, db_path) = setup_full_test_db();
    for (name, session_id) in [("niza", "sess-old"), ("stale", "sess-stale-primary")] {
        db.conn
            .execute(
                "INSERT INTO instances (name, session_id, created_at) VALUES (?1, ?2, 1000.0)",
                params![name, session_id],
            )
            .unwrap();
        db.set_session_binding(session_id, name).unwrap();
    }
    db.set_process_binding("pid-stale", "sess-stale-old", "stale")
        .unwrap();

    assert!(
        db.attach_claude_generation("niza", "sess-new", "", "pid-stale", Some("stale"),)
            .unwrap()
    );
    assert_eq!(db.get_process_binding("pid-stale").unwrap(), None);
    cleanup_test_db(db_path);
}

#[test]
fn test_attach_claude_generation_rolls_back_all_identity_writes_on_error() {
    let (db, db_path) = setup_full_test_db();
    db.conn
        .execute(
            "INSERT INTO instances (name, session_id, created_at) VALUES ('niza', 'sess-old', 1000.0)",
            [],
        )
        .unwrap();
    db.set_session_binding("sess-old", "niza").unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (
                name, parent_session_id, parent_name, agent_id, created_at
             ) VALUES ('niza-child', 'sess-old', 'niza', 'agent-1', 1001.0)",
            [],
        )
        .unwrap();
    db.conn.execute("DROP TABLE process_bindings", []).unwrap();

    assert!(
        db.attach_claude_generation(
            "niza",
            "sess-new",
            "/tmp/new.jsonl",
            "pid-broken",
            Some("foreign"),
        )
        .is_err()
    );
    assert!(db.get_session_binding("sess-new").unwrap().is_none());
    let instance = db.get_instance_full("niza").unwrap().unwrap();
    assert_eq!(instance.session_id.as_deref(), Some("sess-old"));
    assert_ne!(instance.transcript_path, "/tmp/new.jsonl");
    assert_eq!(
        db.get_instance_full("niza-child")
            .unwrap()
            .unwrap()
            .parent_session_id
            .as_deref(),
        Some("sess-old")
    );
    assert!(
        db.get_validated_claude_session_owner("sess-new")
            .unwrap()
            .is_none()
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_attach_claude_generation_deletes_process_only_conflict() {
    let (db, db_path) = setup_full_test_db();
    db.conn
        .execute(
            "INSERT INTO instances (name, session_id, created_at) VALUES ('niza', 'sess-old', 1000.0)",
            [],
        )
        .unwrap();
    db.set_session_binding("sess-old", "niza").unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('stale-placeholder', 1001.0)",
            [],
        )
        .unwrap();
    db.set_process_binding("pid-process-only", "", "stale-placeholder")
        .unwrap();

    assert!(
        db.attach_claude_generation(
            "niza",
            "sess-new",
            "",
            "pid-process-only",
            Some("stale-placeholder"),
        )
        .unwrap()
    );
    assert_eq!(db.get_process_binding("pid-process-only").unwrap(), None);
    cleanup_test_db(db_path);
}

#[test]
fn test_validated_claude_session_cache_rejects_rebound_owner() {
    let (db, db_path) = setup_full_test_db();
    for name in ["niza", "lava"] {
        db.conn
            .execute(
                "INSERT INTO instances (name, created_at) VALUES (?1, 1000.0)",
                params![name],
            )
            .unwrap();
    }
    db.set_session_binding("sess-1", "niza").unwrap();
    db.mark_claude_session_validated("sess-1", "niza").unwrap();
    assert_eq!(
        db.get_validated_claude_session_owner("sess-1")
            .unwrap()
            .as_deref(),
        Some("niza")
    );

    db.rebind_session("sess-1", "lava").unwrap();
    assert_eq!(
        db.get_validated_claude_session_owner("sess-1").unwrap(),
        None
    );
    cleanup_test_db(db_path);
}

#[test]
fn test_delete_process_bindings_for_instance() {
    let (db, db_path) = setup_full_test_db();

    db.conn
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    db.set_process_binding("pid-1", "sess-1", "luna").unwrap();
    db.set_process_binding("pid-2", "sess-2", "luna").unwrap();
    assert!(db.has_process_binding_for_instance("luna"));

    db.delete_process_bindings_for_instance("luna").unwrap();
    assert!(!db.has_process_binding_for_instance("luna"));

    cleanup_test_db(db_path);
}

fn endpoint_port(db: &HcomDb, instance: &str, kind: &str) -> Option<i64> {
    db.conn
        .query_row(
            "SELECT port FROM notify_endpoints WHERE instance = ? AND kind = ?",
            params![instance, kind],
            |row| row.get(0),
        )
        .ok()
}

fn endpoint_count_for(db: &HcomDb, instance: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM notify_endpoints WHERE instance = ?",
            params![instance],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
#[serial]
fn test_migrate_notify_endpoints_preserves_plugin_on_target() {
    let (db, db_path) = setup_full_test_db();
    HcomDb::set_test_migrate_notify_fail(false);

    // Canonical already has plugin from opencode-start; placeholder has PTY ports.
    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();
    db.upsert_notify_endpoint("mozi", "inject", 55_558).unwrap();

    db.migrate_notify_endpoints("mozi", "fano").unwrap();

    assert_eq!(endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(endpoint_port(&db, "fano", "pty"), Some(55_568));
    assert_eq!(endpoint_port(&db, "fano", "inject"), Some(55_558));
    assert_eq!(endpoint_count_for(&db, "mozi"), 0);

    cleanup_test_db(db_path);
}

#[test]
#[serial]
fn test_migrate_notify_endpoints_source_wins_on_conflict() {
    let (db, db_path) = setup_full_test_db();
    HcomDb::set_test_migrate_notify_fail(false);

    // Target (fano) holds a stale pty from a prior process; source (mozi) is the
    // freshly launched process. Source's pty must win; target-only plugin is kept.
    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("fano", "pty", 58_321).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();

    db.migrate_notify_endpoints("mozi", "fano").unwrap();

    assert_eq!(endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(endpoint_port(&db, "fano", "pty"), Some(55_568));
    assert_eq!(endpoint_count_for(&db, "mozi"), 0);

    cleanup_test_db(db_path);
}

#[test]
#[serial]
fn test_migrate_notify_endpoints_moves_kind_missing_on_target() {
    let (db, db_path) = setup_full_test_db();
    HcomDb::set_test_migrate_notify_fail(false);

    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();

    db.migrate_notify_endpoints("mozi", "fano").unwrap();

    assert_eq!(endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(endpoint_port(&db, "fano", "pty"), Some(55_568));
    assert_eq!(endpoint_count_for(&db, "mozi"), 0);

    cleanup_test_db(db_path);
}

struct MigrateNotifyFailGuard;

impl MigrateNotifyFailGuard {
    fn enable() -> Self {
        HcomDb::set_test_migrate_notify_fail(true);
        Self
    }
}

impl Drop for MigrateNotifyFailGuard {
    fn drop(&mut self) {
        HcomDb::set_test_migrate_notify_fail(false);
    }
}

#[test]
#[serial]
fn test_migrate_notify_endpoints_rolls_back_on_injected_failure() {
    let (db, db_path) = setup_full_test_db();
    let _guard = MigrateNotifyFailGuard::enable();

    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();

    let err = db
        .migrate_notify_endpoints("mozi", "fano")
        .expect_err("injected migrate failure");
    assert!(
        err.to_string()
            .contains("test_injected_migrate_notify_fail")
    );

    assert_eq!(endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(endpoint_port(&db, "mozi", "pty"), Some(55_568));

    cleanup_test_db(db_path);
}

#[test]
#[serial]
fn test_migrate_notify_endpoints_commits_on_success_after_fail_guard_cleared() {
    let (db, db_path) = setup_full_test_db();
    HcomDb::set_test_migrate_notify_fail(false);

    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();

    db.migrate_notify_endpoints("mozi", "fano").unwrap();

    assert_eq!(endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(endpoint_port(&db, "fano", "pty"), Some(55_568));
    assert_eq!(endpoint_count_for(&db, "mozi"), 0);

    cleanup_test_db(db_path);
}

#[test]
#[serial]
fn test_migrate_notify_endpoints_can_join_outer_transaction() {
    let (db, db_path) = setup_full_test_db();
    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();

    let outer = db.conn().unchecked_transaction().unwrap();
    db.migrate_notify_endpoints("mozi", "fano").unwrap();
    outer.commit().unwrap();

    assert_eq!(endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(endpoint_port(&db, "fano", "pty"), Some(55_568));
    assert_eq!(endpoint_count_for(&db, "mozi"), 0);

    cleanup_test_db(db_path);
}
