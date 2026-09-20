use super::*;
use rusqlite::{Connection, params};
use std::path::PathBuf;

/// Clean up test database
pub(super) fn cleanup_test_db(path: PathBuf) {
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(PathBuf::from(format!("{}-wal", path.display())));
    let _ = std::fs::remove_file(PathBuf::from(format!("{}-shm", path.display())));
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn open_raw_creates_private_database_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("hcom.db");

    let db = HcomDb::open_raw(&db_path).unwrap();
    db.conn()
        .execute("CREATE TABLE permission_probe (id INTEGER)", [])
        .unwrap();
    db.conn()
        .execute("INSERT INTO permission_probe VALUES (1)", [])
        .unwrap();

    assert_eq!(mode(&db_path), 0o600);
    assert_eq!(mode(&crate::paths::sidecar_path(&db_path, "-wal")), 0o600);
    assert_eq!(mode(&crate::paths::sidecar_path(&db_path, "-shm")), 0o600);
}

#[cfg(unix)]
#[test]
fn open_raw_restricts_existing_database_files() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("hcom.db");
    let first = HcomDb::open_raw(&db_path).unwrap();
    first
        .conn()
        .execute("CREATE TABLE permission_probe (id INTEGER)", [])
        .unwrap();
    first
        .conn()
        .execute("INSERT INTO permission_probe VALUES (1)", [])
        .unwrap();

    for path in [
        db_path.clone(),
        crate::paths::sidecar_path(&db_path, "-wal"),
        crate::paths::sidecar_path(&db_path, "-shm"),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    let _second = HcomDb::open_raw(&db_path).unwrap();

    assert_eq!(mode(&db_path), 0o600);
    assert_eq!(mode(&crate::paths::sidecar_path(&db_path, "-wal")), 0o600);
    assert_eq!(mode(&crate::paths::sidecar_path(&db_path, "-shm")), 0o600);
}

#[cfg(unix)]
#[test]
fn open_raw_restricts_sidecars_for_non_db_filename() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("state.sqlite");

    let db = HcomDb::open_raw(&db_path).unwrap();
    db.conn()
        .execute("CREATE TABLE permission_probe (id INTEGER)", [])
        .unwrap();
    db.conn()
        .execute("INSERT INTO permission_probe VALUES (1)", [])
        .unwrap();

    let wal_path = tmp.path().join("state.sqlite-wal");
    let shm_path = tmp.path().join("state.sqlite-shm");
    std::fs::set_permissions(&wal_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&shm_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let _second = HcomDb::open_raw(&db_path).unwrap();

    assert_eq!(mode(&wal_path), 0o600);
    assert_eq!(mode(&shm_path), 0o600);
}

#[cfg(unix)]
#[test]
fn open_restricts_the_configured_hcom_directory_and_database() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    std::fs::set_permissions(&hcom_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let db = HcomDb::open().unwrap();

    assert_eq!(mode(&hcom_dir), 0o700);
    assert_eq!(mode(db.path()), 0o600);
}

#[cfg(unix)]
#[test]
fn reconnect_if_stale_resecures_replaced_database() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("hcom.db");
    let mut db = HcomDb::open_raw(&db_path).unwrap();

    // Simulate another process replacing the DB with a broad-mode file
    // (new inode), as reset/schema-archive does.
    std::fs::remove_file(&db_path).unwrap();
    let _ = std::fs::remove_file(crate::paths::sidecar_path(&db_path, "-wal"));
    let _ = std::fs::remove_file(crate::paths::sidecar_path(&db_path, "-shm"));
    drop(HcomDb::open_raw(&db_path).unwrap());
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert!(db.reconnect_if_stale());
    assert_eq!(mode(&db_path), 0o600);
}

#[test]
#[should_panic(expected = "not a registered or temp-tree path")]
fn test_open_raw_rejects_non_temp_path() {
    // Not CARGO_MANIFEST_DIR: a worktree checked out under /tmp (a normal
    // thing for an agent to do) makes the repo path itself a temp path,
    // producing a false failure unrelated to any real regression. Derive
    // a guaranteed-non-temp fixture from temp_dir()'s own parent instead.
    let temp = std::env::temp_dir();
    let parent = temp
        .parent()
        .expect("the OS temp dir has a parent directory");
    let db_path = parent.join(".hcom-unsafe-test").join("hcom.db");
    let _ = HcomDb::open_raw(&db_path);
}

#[test]
fn test_open_raw_allows_temp_path() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("allowed.db");

    let db = HcomDb::open_raw(&db_path).unwrap();

    assert_eq!(db.path(), db_path);
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "not a registered or temp-tree path")]
fn test_open_raw_rejects_temp_symlink_to_non_temp_path() {
    use std::os::unix::fs::symlink;

    // Not CARGO_MANIFEST_DIR as the symlink target: a worktree checked
    // out under /tmp makes the repo itself a temp path, which would make
    // this "escape to non-temp" fixture point right back into /tmp.
    // "/" always exists and is never under the OS temp dir.
    let temp = tempfile::tempdir().unwrap();
    let link = temp.path().join("outside");
    symlink("/", &link).unwrap();
    let db_path = link.join(".hcom").join("hcom.db");

    let _ = HcomDb::open_raw(&db_path);
}

#[test]
fn test_all_methods_return_ok_none_when_not_found() {
    let (db, db_path) = setup_full_test_db();

    // All these should return Ok(None) for non-existent data
    assert!(db.get_instance_status("nonexistent").unwrap().is_none());
    assert!(db.get_status("nonexistent").unwrap().is_none());
    assert!(db.get_process_binding("nonexistent").unwrap().is_none());
    assert!(db.get_transcript_path("nonexistent").unwrap().is_none());
    assert!(db.get_instance_snapshot("nonexistent").unwrap().is_none());

    cleanup_test_db(db_path);
}

/// Create a test DB with full init_db() schema
pub(super) fn setup_full_test_db() -> (HcomDb, PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_full_{}_{}.db",
        std::process::id(),
        test_id
    ));

    let db = HcomDb::open_at(&db_path).unwrap();
    (db, db_path)
}

#[test]
fn test_init_db_creates_all_tables() {
    let (db, db_path) = setup_full_test_db();

    let tables: Vec<String> = db
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(tables.contains(&"events".to_string()));
    assert!(tables.contains(&"instances".to_string()));
    assert!(tables.contains(&"kv".to_string()));
    assert!(tables.contains(&"notify_endpoints".to_string()));
    assert!(tables.contains(&"process_bindings".to_string()));
    assert!(tables.contains(&"session_bindings".to_string()));

    cleanup_test_db(db_path);
}

#[test]
fn test_init_db_sets_schema_version() {
    let (db, db_path) = setup_full_test_db();

    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    cleanup_test_db(db_path);
}

#[test]
fn test_init_db_idempotent() {
    let (db, db_path) = setup_full_test_db();

    // Call init_db again - should be no-op
    db.init_db().unwrap();

    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    cleanup_test_db(db_path);
}

#[test]
fn test_init_db_creates_events_v_view() {
    let (db, db_path) = setup_full_test_db();

    // Check view exists
    let count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='view' AND name='events_v'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    cleanup_test_db(db_path);
}

#[test]
fn test_init_db_creates_fts5_table() {
    let (db, db_path) = setup_full_test_db();

    // FTS5 tables show up as 'table' in sqlite_master
    let count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='events_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(count > 0, "events_fts should exist");

    cleanup_test_db(db_path);
}

#[test]
fn test_init_db_fts_trigger_indexes_on_insert() {
    let (db, db_path) = setup_full_test_db();

    // Insert an event
    db.conn
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES ('2026-01-01T00:00:00Z', 'message', 'luna', ?)",
            params![serde_json::json!({"from": "luna", "text": "hello world"}).to_string()],
        )
        .unwrap();

    // Search FTS
    let count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events_fts WHERE searchable MATCH 'hello'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    cleanup_test_db(db_path);
}

#[test]
fn test_check_schema_compat_fresh_db() {
    let (db, db_path) = setup_full_test_db();
    match db.check_schema_compat().unwrap() {
        SchemaCompat::Ok => {} // expected
        other => panic!(
            "Expected SchemaCompat::Ok, got {:?}",
            match other {
                SchemaCompat::NeedsArchive(r, v) => format!("NeedsArchive({}, {:?})", r, v),
                SchemaCompat::StaleProcess => "StaleProcess".to_string(),
                SchemaCompat::Ok => unreachable!(),
            }
        ),
    }
    cleanup_test_db(db_path);
}

#[test]
fn test_ensure_schema_fresh_db() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1000);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_ensure_{}_{}.db",
        std::process::id(),
        test_id
    ));

    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.ensure_schema().unwrap();

    // Should have full schema
    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    cleanup_test_db(db_path);
}

#[test]
fn test_ensure_schema_archives_old_version() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(2000);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_archive_{}_{}.db",
        std::process::id(),
        test_id
    ));

    // Create a DB with old schema version
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, timestamp TEXT, type TEXT, instance TEXT, data TEXT);
             CREATE TABLE instances (name TEXT PRIMARY KEY, created_at REAL NOT NULL);
             CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notify_endpoints (instance TEXT, kind TEXT, port INTEGER, updated_at REAL, PRIMARY KEY(instance, kind));
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             PRAGMA user_version = 5;",
        )
        .unwrap();
    }

    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.ensure_schema().unwrap();

    // Should have been archived and recreated at current version
    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    // Archive directory should exist
    let archive_dir = temp_dir.join("archive");
    if archive_dir.exists() {
        let _ = std::fs::remove_dir_all(&archive_dir);
    }

    cleanup_test_db(db_path);
}

#[test]
fn test_ensure_schema_migrates_v16_to_v18_in_place_without_status_time() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(2500);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_migrate_{}_{}.db",
        std::process::id(),
        test_id
    ));

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, timestamp TEXT, type TEXT, instance TEXT, data TEXT);
             CREATE TABLE instances (
                 name TEXT PRIMARY KEY,
                 tool TEXT DEFAULT 'claude',
                 created_at REAL NOT NULL,
                 launch_context TEXT DEFAULT ''
             );
             CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notify_endpoints (instance TEXT, kind TEXT, port INTEGER, updated_at REAL, PRIMARY KEY(instance, kind));
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             PRAGMA user_version = 16;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO instances (name, tool, created_at, launch_context) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "luna",
                "claude",
                1.0f64,
                r#"{"terminal_preset":"ghostty-tab"}"#
            ],
        )
        .unwrap();
    }

    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.ensure_schema().unwrap();

    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let preset: String = db
        .conn
        .query_row(
            "SELECT terminal_preset_effective FROM instances WHERE name = ?",
            params!["luna"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preset, "ghostty-tab");
    let launch_context: String = db
        .conn
        .query_row(
            "SELECT launch_context FROM instances WHERE name = ?",
            params!["luna"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(launch_context, r#"{"terminal_preset":"ghostty-tab"}"#);
    let last_seen: i64 = db
        .conn
        .query_row(
            "SELECT last_seen FROM instances WHERE name = ?",
            params!["luna"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_seen, 0);

    cleanup_test_db(db_path);
}

#[test]
fn test_ensure_schema_migrates_v17_to_v18_using_status_time() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(2750);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_migrate_status_time_{}_{}.db",
        std::process::id(),
        test_id
    ));

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, timestamp TEXT, type TEXT, instance TEXT, data TEXT);
             CREATE TABLE instances (
                 name TEXT PRIMARY KEY,
                 tool TEXT DEFAULT 'claude',
                 status_time INTEGER DEFAULT 0,
                 created_at REAL NOT NULL,
                 launch_context TEXT DEFAULT '',
                 terminal_preset_requested TEXT DEFAULT '',
                 terminal_preset_effective TEXT DEFAULT ''
             );
             CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notify_endpoints (instance TEXT, kind TEXT, port INTEGER, updated_at REAL, PRIMARY KEY(instance, kind));
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             PRAGMA user_version = 17;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO instances (name, tool, status_time, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["luna", "claude", 123i64, 1.0f64],
        )
        .unwrap();
    }

    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.ensure_schema().unwrap();

    let last_seen: i64 = db
        .conn
        .query_row(
            "SELECT last_seen FROM instances WHERE name = ?",
            params!["luna"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_seen, 123);

    cleanup_test_db(db_path);
}

/// v19 moves the PID namespace marker out of the `launch_context` blob and
/// into its own column. Rows whose blob still holds a usable marker carry
/// it across; malformed or markerless blobs stay empty and read as
/// "namespace unknown", which keeps them out of the reaper's reach.
#[test]
fn test_ensure_schema_migrates_v18_to_v19_backfilling_pid_namespace() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(2800);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_migrate_pid_ns_{}_{}.db",
        std::process::id(),
        test_id
    ));

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, timestamp TEXT, type TEXT, instance TEXT, data TEXT);
             CREATE TABLE instances (
                 name TEXT PRIMARY KEY,
                 tool TEXT DEFAULT 'claude',
                 status_time INTEGER DEFAULT 0,
                 last_seen INTEGER DEFAULT 0,
                 created_at REAL NOT NULL,
                 launch_context TEXT DEFAULT '',
                 terminal_preset_requested TEXT DEFAULT '',
                 terminal_preset_effective TEXT DEFAULT ''
             );
             CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notify_endpoints (instance TEXT, kind TEXT, port INTEGER, updated_at REAL, PRIMARY KEY(instance, kind));
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             PRAGMA user_version = 18;",
        )
        .unwrap();
        for (name, ctx) in [
            (
                "marked",
                r#"{"pane_id":"p1","pid_namespace":"pid:[4026531836]"}"#,
            ),
            ("unmarked", r#"{"pane_id":"p2"}"#),
            ("garbage", "not json at all"),
            ("empty", ""),
        ] {
            conn.execute(
                "INSERT INTO instances (name, created_at, launch_context) VALUES (?1, ?2, ?3)",
                rusqlite::params![name, 1.0f64, ctx],
            )
            .unwrap();
        }
    }

    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.ensure_schema().unwrap();

    let namespace = |name: &str| -> String {
        db.conn
            .query_row(
                "SELECT pid_namespace FROM instances WHERE name = ?",
                params![name],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
            .unwrap_or_default()
    };
    assert_eq!(namespace("marked"), "pid:[4026531836]");
    assert_eq!(namespace("unmarked"), "");
    assert_eq!(namespace("garbage"), "");
    assert_eq!(namespace("empty"), "");

    cleanup_test_db(db_path);
}

/// The pre-archive snapshot is what lets a running agent be recovered into
/// the fresh DB. A PID from a namespace this process cannot inspect is not
/// evidence the agent exited, so dropping it here would strand it.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn test_snapshot_keeps_rows_whose_namespace_we_cannot_inspect() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(2900);

    let temp_dir = std::env::temp_dir().join(format!(
        "test_hcom_snapshot_ns_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(temp_dir.join(".tmp")).unwrap();
    let db_path = temp_dir.join("hcom.db");

    let db = HcomDb::open_at(&db_path).unwrap();
    db.conn
        .execute(
            "INSERT INTO instances (name, tool, created_at, pid, pid_namespace)
             VALUES ('host', 'claude', 1.0, 4194305, 'pid:[foreign]'),
                    ('ours', 'claude', 1.0, 4194306, ?)",
            params![crate::sys::process::current_pid_namespace().unwrap_or_default()],
        )
        .unwrap();

    db.snapshot_running_to_pidtrack();

    let pidfile =
        std::fs::read_to_string(temp_dir.join(".tmp").join("launched_pids.json")).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&pidfile).unwrap();
    assert_eq!(
        entries["4194305"]["pid_namespace"], "pid:[foreign]",
        "foreign row kept, marker carried across: {pidfile}"
    );
    assert!(
        entries.get("4194306").is_none(),
        "our own dead pid is not snapshotted: {pidfile}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_ensure_schema_column_guard() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(3000);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_colguard_{}_{}.db",
        std::process::id(),
        test_id
    ));

    // Create a DB at current version but missing 'tool' column
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, timestamp TEXT, type TEXT, instance TEXT, data TEXT);
             CREATE TABLE instances (name TEXT PRIMARY KEY, created_at REAL NOT NULL);
             CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notify_endpoints (instance TEXT, kind TEXT, port INTEGER, updated_at REAL, PRIMARY KEY(instance, kind));
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             CREATE TABLE claude_actor_capabilities (
                 token TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 tool_use_id TEXT NOT NULL,
                 agent_id TEXT NOT NULL DEFAULT '',
                 instance_name TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 last_seen INTEGER NOT NULL,
                 UNIQUE(session_id, tool_use_id, agent_id)
             );
             PRAGMA user_version = {};",
            SCHEMA_VERSION
        ))
        .unwrap();
    }

    let mut db = HcomDb::open_raw(&db_path).unwrap();

    // check_schema_compat should detect missing column
    match db.check_schema_compat().unwrap() {
        SchemaCompat::NeedsArchive(reason, _) => {
            assert!(reason.contains("instances.tool"), "reason: {}", reason);
        }
        _ => panic!("Expected NeedsArchive for missing tool column"),
    }

    // ensure_schema should fix it
    db.ensure_schema().unwrap();

    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    cleanup_test_db(db_path);
}

/// Regression test for issue #16: init_db() stamped user_version=17 without
/// actually adding the terminal_preset_* columns. ensure_schema must repair
/// this via migration instead of archiving (which would lose data).
#[test]
fn test_ensure_schema_repairs_stamped_but_not_migrated_db() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(4000);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_repair_{}_{}.db",
        std::process::id(),
        test_id
    ));

    // Simulate the bug: create a v16-style DB but stamp it as v17
    // (this is what init_db() did — CREATE IF NOT EXISTS is a no-op on
    // existing tables, then it unconditionally set user_version = 17)
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp TEXT NOT NULL, type TEXT NOT NULL, instance TEXT NOT NULL, data TEXT NOT NULL);
             CREATE TABLE instances (
                 name TEXT PRIMARY KEY,
                 session_id TEXT UNIQUE,
                 parent_session_id TEXT,
                 parent_name TEXT,
                 tag TEXT,
                 last_event_id INTEGER DEFAULT 0,
                 status TEXT DEFAULT 'active',
                 status_time INTEGER DEFAULT 0,
                 status_context TEXT DEFAULT '',
                 status_detail TEXT DEFAULT '',
                 last_stop INTEGER DEFAULT 0,
                 directory TEXT,
                 created_at REAL NOT NULL,
                 transcript_path TEXT DEFAULT '',
                 tcp_mode INTEGER DEFAULT 0,
                 wait_timeout INTEGER DEFAULT 86400,
                 background INTEGER DEFAULT 0,
                 background_log_file TEXT DEFAULT '',
                 name_announced INTEGER DEFAULT 0,
                 agent_id TEXT UNIQUE,
                 running_tasks TEXT DEFAULT '',
                 origin_device_id TEXT DEFAULT '',
                 hints TEXT DEFAULT '',
                 subagent_timeout INTEGER,
                 tool TEXT DEFAULT 'claude',
                 launch_args TEXT DEFAULT '',
                 idle_since TEXT DEFAULT '',
                 pid INTEGER DEFAULT NULL,
                 launch_context TEXT DEFAULT ''
             );
             CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE notify_endpoints (instance TEXT NOT NULL, kind TEXT NOT NULL, port INTEGER NOT NULL, updated_at REAL NOT NULL, PRIMARY KEY(instance, kind));
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             CREATE TABLE process_bindings (process_id TEXT PRIMARY KEY, session_id TEXT, instance_name TEXT, updated_at REAL NOT NULL);
             PRAGMA user_version = 17;",
        )
        .unwrap();
        // Insert test data that should survive the repair
        conn.execute(
            "INSERT INTO instances (name, tool, created_at) VALUES ('luna', 'claude', 1.0)",
            [],
        )
        .unwrap();
    }

    // Verify columns are missing before repair
    {
        let conn = Connection::open(&db_path).unwrap();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(instances)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            !cols.contains(&"terminal_preset_requested".to_string()),
            "column should be missing before repair"
        );
    }

    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.ensure_schema().unwrap();

    // Should be at current version
    let version: i32 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    // Columns should now exist
    let cols: Vec<String> = db
        .conn
        .prepare("PRAGMA table_info(instances)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(
        cols.contains(&"terminal_preset_requested".to_string()),
        "terminal_preset_requested column should exist after repair"
    );
    assert!(
        cols.contains(&"terminal_preset_effective".to_string()),
        "terminal_preset_effective column should exist after repair"
    );

    // Test data should have survived (not archived)
    let name: String = db
        .conn
        .query_row(
            "SELECT name FROM instances WHERE name = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "luna");

    cleanup_test_db(db_path);
}
