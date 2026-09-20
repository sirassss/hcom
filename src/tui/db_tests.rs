use super::{
    compute_unread_batch, count_gt, load_instances, load_recently_stopped, parse_message_row,
    parse_status_or_life_row, parse_tool,
};
use crate::tui::model::{
    ActivityKind, Agent, AgentStatus, EventKind, Message, MessageScope, SenderKind, Tool,
};
use rusqlite::Connection;

// Blocker 1 regression: the no-arg TUI must route through the owner-only
// permission boundary rather than leaving/creating a broad database. `db_path`
// is pinned to the isolated temp rather than left to be re-read from global
// `Config` mid-test, whose process-wide cache other parallel tests can reset.
#[cfg(unix)]
#[test]
fn ensure_conn_secures_existing_broad_database() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    // Pre-existing broad db (legacy install opened only through the TUI).
    let db_path = hcom_dir.join("hcom.db");
    std::fs::write(&db_path, b"").unwrap();
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    // Opening the TUI data source must run the permission boundary, which
    // secures the db with a lock-free chmod before any connection is
    // opened. Assert on the resulting mode, not the connection: whether the
    // read open itself succeeds is irrelevant to the security invariant and
    // would otherwise couple the test to SQLite locking under parallel load.
    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path.clone();
    let _ = ds.ensure_conn();

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&db_path),
        0o600,
        "TUI open left db broad: {:?}",
        ds.last_error
    );
}

// Regression: a viewport switch only calls `set_timeline_limit`. If that
// leaves the cache in place, `load_if_changed` keeps returning `None` while
// the DB is quiet and the other viewport renders the previous window — and
// its now-wrong `timeline_limit` in the scope header.
#[test]
fn set_timeline_limit_invalidates_cached_window() {
    use crate::tui::data::DataSource;
    let mut ds = super::DbDataSource::new();
    ds.cached = Some(crate::tui::app::DataState::empty());
    ds.last_data_version = 7;

    ds.set_timeline_limit(5000);
    assert!(
        ds.cached.is_none(),
        "stale window kept after a viewport limit change"
    );

    // Re-setting the same limit is a no-op and must not throw away a cache.
    ds.cached = Some(crate::tui::app::DataState::empty());
    ds.set_timeline_limit(5000);
    assert!(ds.cached.is_some(), "same limit must not invalidate");
}

// The observable half: after a limit change `load` must re-query instead of
// handing back the previous viewport's snapshot, even though the DB has not
// been written to (so `PRAGMA data_version` is unchanged).
#[test]
fn load_after_limit_change_returns_a_fresh_window() {
    use crate::tui::data::DataSource;
    let conn = setup_conn();
    for (id, body) in [(1i64, "one"), (2i64, "two")] {
        conn.execute(
            "INSERT INTO events (id, type, instance, data, timestamp) VALUES (?, 'message', 'nova', ?, ?)",
            rusqlite::params![
                id,
                format!(r#"{{"from":"nova","message":"{body}","scope":"broadcast"}}"#),
                "2026-02-18T00:09:30+00:00"
            ],
        )
        .unwrap();
    }

    let mut ds = super::DbDataSource::new();
    // Pin the change detectors to what the connection and config actually
    // report, so `data_version_changed()` is demonstrably false: without the
    // cache invalidation this test would keep the stale snapshot.
    ds.last_data_version = conn
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .map(|v| v as u64)
        .unwrap();
    ds.config_mtime = super::config_toml_mtime();
    ds.conn = Some(conn);
    assert!(
        !ds.data_version_changed(),
        "fixture must look unchanged to the cache"
    );
    // Stale snapshot from the other viewport, tagged with a limit no real
    // load can produce.
    let mut stale = crate::tui::app::DataState::empty();
    stale.timeline_limit = 999;
    ds.cached = Some(stale);

    ds.set_timeline_limit(5000);
    let data = ds.load();
    assert_ne!(
        data.timeline_limit, 999,
        "load returned the previous viewport's window"
    );
    assert_eq!(data.messages.len(), 2, "rows must come from the DB");
}

fn setup_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            type TEXT NOT NULL,
            instance TEXT NOT NULL,
            data TEXT NOT NULL,
            timestamp TEXT NOT NULL
        );
        CREATE TABLE instances (
            name TEXT PRIMARY KEY
        );
        ",
    )
    .unwrap();
    conn
}

#[test]
fn parse_tool_preserves_unknown_persisted_value() {
    assert_eq!(
        parse_tool("future-tool"),
        Tool::Unknown("future-tool".to_string())
    );
}

fn make_agent(name: &str, last_event_id: u64) -> Agent {
    Agent {
        name: name.into(),
        tool: Tool::Claude,
        status: AgentStatus::Active,
        status_context: String::new(),
        status_detail: String::new(),
        created_at: 0.0,
        status_time: 0.0,
        last_heartbeat: 0.0,
        has_tcp: false,
        directory: String::new(),
        tag: String::new(),
        unread: 0,
        last_event_id: Some(last_event_id),
        device_name: None,
        sync_age: None,
        headless: false,
        session_id: None,
        pid: None,
        terminal_preset: None,
    }
}

#[test]
fn load_recently_stopped_filters_older_than_ten_minutes() {
    let conn = setup_conn();
    let now = chrono::DateTime::parse_from_rfc3339("2026-02-18T00:10:00+00:00")
        .unwrap()
        .timestamp() as f64;

    conn.execute(
        "INSERT INTO events (id, type, instance, data, timestamp) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            1i64,
            "life",
            "olda",
            r#"{"action":"stopped","snapshot":{"tool":"claude"}}"#,
            "2026-02-17T23:59:00+00:00"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (id, type, instance, data, timestamp) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            2i64,
            "life",
            "reca",
            r#"{"action":"killed","snapshot":{"tool":"gemini"}}"#,
            "2026-02-18T00:09:30+00:00"
        ],
    )
    .unwrap();

    let stopped = load_recently_stopped(&conn, now);
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].name, "reca");
}

#[test]
fn load_recently_stopped_excludes_currently_active_instances() {
    let conn = setup_conn();
    let now = chrono::DateTime::parse_from_rfc3339("2026-02-18T00:10:00+00:00")
        .unwrap()
        .timestamp() as f64;

    conn.execute(
        "INSERT INTO instances (name) VALUES (?)",
        rusqlite::params!["live"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (id, type, instance, data, timestamp) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            1i64,
            "life",
            "live",
            r#"{"action":"stopped","snapshot":{"tool":"claude"}}"#,
            "2026-02-18T00:09:30+00:00"
        ],
    )
    .unwrap();

    let stopped = load_recently_stopped(&conn, now);
    assert!(stopped.is_empty());
}

#[test]
fn unread_batch_counts_mentions_and_broadcasts_without_self_messages() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE events (
            id INTEGER PRIMARY KEY,
            type TEXT NOT NULL,
            data TEXT NOT NULL
        );
        ",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO events (id, type, data) VALUES (1, 'message', ?)",
        rusqlite::params![r#"{"from":"aone","scope":"broadcast","sender_kind":"agent"}"#],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (id, type, data) VALUES (2, 'message', ?)",
        rusqlite::params![r#"{"from":"sys","scope":"broadcast","sender_kind":"system"}"#],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (id, type, data) VALUES (3, 'message', ?)",
        rusqlite::params![
            r#"{"from":"cone","scope":"mentions","mentions":["aone","btwo"],"sender_kind":"agent"}"#
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (id, type, data) VALUES (4, 'message', ?)",
        rusqlite::params![r#"{"from":"btwo","scope":"broadcast","sender_kind":"agent"}"#],
    )
    .unwrap();

    let mut agents = vec![make_agent("aone", 0), make_agent("btwo", 2)];
    compute_unread_batch(&conn, &mut agents);

    assert_eq!(agents[0].unread, 2, "aone: mention(3) + broadcast(4)");
    assert_eq!(
        agents[1].unread, 1,
        "btwo: mention(3), self broadcast(4) ignored"
    );
}

#[test]
fn unknown_status_defaults_to_inactive_in_instance_load() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE instances (
            name TEXT,
            tool TEXT,
            status TEXT,
            status_context TEXT,
            status_detail TEXT,
            created_at REAL,
            status_time INTEGER,
            last_stop INTEGER,
            tcp_mode INTEGER,
            directory TEXT,
            tag TEXT,
            last_event_id INTEGER,
            origin_device_id TEXT,
            pid INTEGER,
            session_id TEXT,
            background INTEGER,
            terminal_preset_effective TEXT
        );
        CREATE TABLE kv (key TEXT, value TEXT);
        ",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO instances (name, tool, status, status_context, status_detail, created_at, status_time, last_stop, tcp_mode, directory, tag, last_event_id, origin_device_id, pid, session_id, background, terminal_preset_effective)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "nazo",
            "claude",
            "teleporting",
            "",
            "",
            10.0f64,
            0i64,
            0i64,
            0i64,
            "/tmp",
            "",
            0i64,
            "",
            Option::<i64>::None,
            Option::<String>::None,
            0i64,
            Option::<String>::None
        ],
    )
    .unwrap();

    let (local, remote) = load_instances(&conn, "", 100.0);
    assert!(remote.is_empty());
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].status, AgentStatus::Inactive);
    assert_eq!(local[0].status_context, "unknown_status");
    assert!(local[0].status_detail.contains("teleporting"));
}

#[test]
fn parse_message_row_defaults_missing_fields() {
    let msg = parse_message_row(7, "2026-02-18T00:09:30+00:00", "{}").unwrap();
    assert_eq!(msg.event_id, 7);
    assert_eq!(msg.sender, "?");
    assert_eq!(msg.body, "");
    assert_eq!(msg.scope, MessageScope::Broadcast);
    assert_eq!(msg.sender_kind, SenderKind::Instance);
    assert!(msg.recipients.is_empty());
    assert!(msg.delivered.is_empty());
    assert!(msg.intent.is_none());
    assert!(msg.reply_to.is_none());
}

#[test]
fn parse_message_row_maps_mentions_and_external_sender_kind() {
    let msg = parse_message_row(
        8,
        "2026-02-18T00:09:30+00:00",
        r#"{"from":"sys","text":"hi","scope":"mentions","sender_kind":"human","mentions":["nova"],"delivered_to":["nova"],"intent":"request","reply_to":42}"#,
    )
    .unwrap();

    assert_eq!(msg.scope, MessageScope::Mentions);
    assert_eq!(msg.sender_kind, SenderKind::External);
    assert_eq!(msg.recipients, vec!["nova"]);
    assert_eq!(msg.delivered, vec!["nova"]);
    assert_eq!(msg.intent.as_deref(), Some("request"));
    assert_eq!(msg.reply_to, Some(42));
    assert!(msg.delivery_known);
}

fn parse_msg(data: &str) -> Message {
    parse_message_row(1, "2026-02-18T00:09:30+00:00", data).unwrap()
}

#[test]
fn parse_message_row_thread_variants() {
    assert_eq!(
        parse_msg(r#"{"thread":"hcom-skill"}"#).thread.as_deref(),
        Some("hcom-skill")
    );
    assert_eq!(parse_msg("{}").thread, None);
    assert_eq!(parse_msg(r#"{"thread":null}"#).thread, None);
    assert_eq!(parse_msg(r#"{"thread":42}"#).thread, None);
    assert_eq!(parse_msg(r#"{"thread":["a"]}"#).thread, None);
}

#[test]
fn parse_message_row_delivery_known_tracks_array_presence() {
    assert!(!parse_msg("{}").delivery_known);
    assert!(!parse_msg(r#"{"delivered_to":null}"#).delivery_known);
    assert!(!parse_msg(r#"{"delivered_to":"nova"}"#).delivery_known);

    let empty = parse_msg(r#"{"delivered_to":[]}"#);
    assert!(empty.delivery_known);
    assert!(empty.delivered.is_empty());

    let populated = parse_msg(r#"{"delivered_to":["nova","ligo"]}"#);
    assert!(populated.delivery_known);
    assert_eq!(populated.delivered, vec!["nova", "ligo"]);
}

#[test]
fn parse_status_row_with_tool_context_creates_tool_event() {
    let ev = parse_status_or_life_row(
        9,
        "2026-02-18T00:09:30+00:00",
        "nova",
        "status",
        r#"{"status":"active","context":"tool:Read","detail":"src/lib.rs"}"#,
    )
    .unwrap();

    assert_eq!(ev.row_id, 9);
    assert_eq!(ev.agent, "nova");
    assert_eq!(ev.kind, EventKind::Tool);
    assert_eq!(ev.tool, "Read");
    assert_eq!(ev.detail, "src/lib.rs");
}

#[test]
fn parse_life_stopped_event_includes_resume_subline() {
    let ev = parse_status_or_life_row(
        10,
        "2026-02-18T00:10:00+00:00",
        "nova",
        "life",
        r#"{"action":"stopped","reason":"idle","by":"bigboss","snapshot":{"tool":"claude","pid":1234,"directory":"/tmp/demo","created_at":1739837340.0}}"#,
    )
    .unwrap();

    assert_eq!(ev.kind, EventKind::Activity(ActivityKind::Stopped));
    assert!(ev.detail.contains("stopped"));
    assert!(ev.detail.contains("bigboss"));
    assert!(
        ev.sub_lines
            .iter()
            .any(|l| l.starts_with("resume: hcom r nova")),
        "expected resume hint in sub-lines, got {:?}",
        ev.sub_lines
    );
}

#[test]
fn count_gt_works_with_sorted_ids() {
    let ids = vec![2, 4, 9, 12];
    assert_eq!(count_gt(&ids, 0), 4);
    assert_eq!(count_gt(&ids, 4), 2);
    assert_eq!(count_gt(&ids, 12), 0);
}

// ── reconcile_dead_instances ────────────────────────────────

// A PID above every platform's pid_max, so it names no live process
// (matches instance_lifecycle::tests::DEAD_PID).
const DEAD_PID: i64 = 4_194_305;

/// A local instance parked `active` with a PID that cannot be alive —
/// the shape `reconcile_dead_instances` is supposed to reap.
fn insert_dead_local_instance(db: &crate::db::HcomDb, name: &str) {
    db.conn()
        .execute(
            "INSERT INTO instances
                (name, tool, status, status_context, created_at, pid, tcp_mode, pid_namespace)
             VALUES (?, 'claude', 'active', 'tool:Bash', ?, ?, 1, ?)",
            rusqlite::params![
                name,
                crate::shared::time::now_epoch_f64(),
                DEAD_PID,
                crate::sys::process::current_pid_namespace().unwrap_or_default()
            ],
        )
        .unwrap();
}

/// The TUI's orphan pane is where a sandboxed hcom would silently drop a
/// live host agent: bare `is_alive` on a foreign PID reads as dead.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn load_orphans_lists_processes_from_a_foreign_namespace() {
    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    std::fs::create_dir_all(hcom_dir.join(".tmp")).unwrap();
    std::fs::write(
        hcom_dir.join(".tmp").join("launched_pids.json"),
        serde_json::json!({
            "4194305": {
                "tool": "claude",
                "names": ["host_agent"],
                "launched_at": 1.0,
                "pid_namespace": "pid:[foreign]",
            },
            "4194306": {
                "tool": "claude",
                "names": ["our_dead_agent"],
                "launched_at": 1.0,
                "pid_namespace":
                    crate::sys::process::current_pid_namespace().unwrap_or_default(),
            },
        })
        .to_string(),
    )
    .unwrap();
    if let Ok(mut guard) = crate::tui::db::ORPHAN_CACHE.lock() {
        *guard = None;
    }

    let db = crate::db::HcomDb::open_at(&hcom_dir.join("hcom.db")).unwrap();
    let orphans = crate::tui::db::load_orphans(db.conn());

    assert!(
        orphans.iter().any(|o| o.pid == 4_194_305),
        "a pid we cannot inspect must stay listed"
    );
    assert!(
        !orphans.iter().any(|o| o.pid == 4_194_306),
        "a dead pid in our own namespace is still hidden"
    );
    if let Ok(mut guard) = crate::tui::db::ORPHAN_CACHE.lock() {
        *guard = None;
    }
}

#[test]
fn db_source_reconcile_removes_dead_row_then_returns_zero() {
    use crate::tui::data::DataSource;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");
    let db = crate::db::HcomDb::open_at(&db_path).unwrap();
    insert_dead_local_instance(&db, "ghost");
    drop(db);

    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;

    assert_eq!(
        ds.reconcile_dead_instances().unwrap(),
        1,
        "one dead local row must be reconciled"
    );
    assert_eq!(
        ds.reconcile_dead_instances().unwrap(),
        0,
        "nothing left to reconcile on the next pass"
    );
}

#[test]
fn db_source_reconcile_invalidates_cache_so_load_sees_committed_deletion() {
    use crate::tui::data::DataSource;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");
    let db = crate::db::HcomDb::open_at(&db_path).unwrap();
    insert_dead_local_instance(&db, "ghost");
    drop(db);

    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;

    // Populate the read-path cache first, as the TUI's normal render tick
    // would before a maintenance tick runs.
    let before = ds.load();
    assert!(
        before.agents.iter().any(|a| a.name == "ghost"),
        "fixture: ghost must be visible before reconcile"
    );
    assert!(ds.cached.is_some(), "fixture: load() must cache a snapshot");

    assert_eq!(ds.reconcile_dead_instances().unwrap(), 1);

    // Reconciliation writes through a separate connection than the one
    // `data_version` is read from, so trusting that reading alone could
    // still see the pre-reconcile snapshot. The explicit dirty flag must
    // force a full reload regardless.
    assert!(
        ds.cached.is_none(),
        "a successful reconcile must invalidate the cached snapshot"
    );

    let after = ds.load();
    assert!(
        !after.agents.iter().any(|a| a.name == "ghost"),
        "reload must observe the committed deletion"
    );
}

#[test]
fn write_db_open_success_clears_a_stale_last_error() {
    use crate::tui::data::DataSource;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");
    let db = crate::db::HcomDb::open_at(&db_path).unwrap();
    drop(db);

    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;
    // Simulate the state left behind by an earlier failed open (e.g.
    // transient lock contention) that has since cleared up.
    ds.last_error = Some("open write db ...: stale failure".to_string());

    assert_eq!(ds.reconcile_dead_instances().unwrap(), 0);
    assert!(
        ds.last_error.is_none(),
        "a successful write-db open must clear a stale last_error, not leave it \
         stuck in the TUI status bar forever"
    );
}

// ── Task 4: end-to-end acceptance fixture ──────────────────────
//
// Exercises the pipeline through the `DataSource` trait interface (not
// `instance_lifecycle::reconcile_dead_instances` directly), asserting on
// the acceptance criteria in the spec's "### TUI lifecycle" section.
// Task 1's tests already cover the full skip-list matrix, idempotency
// ordering, and reason/cascade details at the DB layer; this stays a
// lighter, representative pass through `DbDataSource`.

fn insert_local_instance(db: &crate::db::HcomDb, name: &str, status: &str, pid: i64) {
    db.conn()
        .execute(
            "INSERT INTO instances
                (name, tool, status, status_context, created_at, pid, tcp_mode, pid_namespace)
             VALUES (?, 'claude', ?, 'tool:Bash', ?, ?, 1, ?)",
            rusqlite::params![
                name,
                status,
                crate::shared::time::now_epoch_f64(),
                pid,
                crate::sys::process::current_pid_namespace().unwrap_or_default()
            ],
        )
        .unwrap();
}

fn insert_remote_instance(db: &crate::db::HcomDb, name: &str, pid: i64) {
    db.conn()
        .execute(
            "INSERT INTO instances \
                (name, tool, status, status_context, created_at, pid, tcp_mode, origin_device_id)
             VALUES (?, 'claude', 'active', 'tool:Bash', ?, ?, 1, 'device-remote')",
            rusqlite::params![name, crate::shared::time::now_epoch_f64(), pid],
        )
        .unwrap();
}

fn insert_pidless_instance(db: &crate::db::HcomDb, name: &str) {
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, status, status_context, created_at, tcp_mode)
             VALUES (?, 'claude', 'active', 'tool:Bash', ?, 1)",
            rusqlite::params![name, crate::shared::time::now_epoch_f64()],
        )
        .unwrap();
}

fn last_life_detector(db: &crate::db::HcomDb, name: &str) -> String {
    let data: String = db
        .conn()
        .query_row(
            "SELECT data FROM events WHERE type = 'life' AND instance = ? \
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![name],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str::<serde_json::Value>(&data).unwrap()["detector"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Acceptance criteria 1, 2, 8 (spec "### TUI lifecycle"): a local
/// `active` row and a local `listening` row, both with a dead PID, are
/// both removed through the `DataSource` trait interface in one pass,
/// and each leaves exactly one stopped life event stamped
/// `detector: "tui"`.
#[test]
fn db_source_reconcile_removes_dead_active_and_listening_rows_via_trait() {
    use crate::tui::data::DataSource;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");

    // A single connection for the whole test (via `ds`'s own lazily-opened
    // write handle), not a separate setup connection plus `ds`'s own --
    // two connections opening/closing the same fresh sqlite file adds
    // avoidable contention under parallel test load.
    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;
    assert!(
        ds.ensure_write_db(),
        "fixture: write db must open: {:?}",
        ds.last_error
    );
    {
        let db = ds.write_db.as_ref().unwrap();
        insert_local_instance(db, "dead_active", crate::shared::ST_ACTIVE, DEAD_PID);
        insert_local_instance(
            db,
            "dead_listening",
            crate::shared::ST_LISTENING,
            DEAD_PID + 1,
        );
    }

    assert_eq!(
        ds.reconcile_dead_instances().unwrap(),
        2,
        "both dead active and listening rows must reconcile in one pass"
    );

    let db = ds.write_db.as_ref().unwrap();
    assert!(db.get_instance_full("dead_active").unwrap().is_none());
    assert!(db.get_instance_full("dead_listening").unwrap().is_none());
    assert_eq!(last_life_detector(db, "dead_active"), "tui");
    assert_eq!(last_life_detector(db, "dead_listening"), "tui");
}

/// Acceptance criterion 3: a live PID is never removed, across repeated
/// reconcile calls -- not just the first one.
#[test]
fn db_source_reconcile_keeps_live_pid_row_across_repeated_calls() {
    use crate::tui::data::DataSource;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");

    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;
    assert!(
        ds.ensure_write_db(),
        "fixture: write db must open: {:?}",
        ds.last_error
    );
    insert_local_instance(
        ds.write_db.as_ref().unwrap(),
        "alive",
        crate::shared::ST_ACTIVE,
        std::process::id() as i64,
    );

    for pass in 0..3 {
        assert_eq!(
            ds.reconcile_dead_instances().unwrap(),
            0,
            "live PID must survive pass {pass}"
        );
    }

    let db = ds.write_db.as_ref().unwrap();
    assert!(db.get_instance_full("alive").unwrap().is_some());
}

/// Acceptance criterion 4 (lighter touch -- Task 1's tests already cover
/// the full skip-list matrix at the `reconcile_dead_instances` level):
/// one remote row and one PID-less row, both otherwise eligible, survive
/// a reconcile pass through the `DataSource` interface.
#[test]
fn db_source_reconcile_skips_remote_and_pidless_rows_via_trait() {
    use crate::tui::data::DataSource;

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");

    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;
    assert!(
        ds.ensure_write_db(),
        "fixture: write db must open: {:?}",
        ds.last_error
    );
    {
        let db = ds.write_db.as_ref().unwrap();
        insert_remote_instance(db, "remote_dead", DEAD_PID);
        insert_pidless_instance(db, "pidless");
    }

    assert_eq!(ds.reconcile_dead_instances().unwrap(), 0);

    let db = ds.write_db.as_ref().unwrap();
    assert!(db.get_instance_full("remote_dead").unwrap().is_some());
    assert!(db.get_instance_full("pidless").unwrap().is_some());
}

/// Acceptance criterion 7 (timing): measures a real fixture-DB
/// `reconcile_dead_instances()` call with `Instant`. The write handle is
/// opened (and the row inserted) before the clock starts, so the
/// measured span is the steady-state per-tick cost the spec's cadence
/// actually pays -- not the one-time lazy-open cost `tick_reconcile`
/// only incurs once per TUI session.
///
/// This is honest evidence only for the DB-layer half of the spec's
/// <=2s budget -- a tiny local sqlite fixture with no contention is
/// near-instant by construction. It does NOT measure real wall-clock
/// behavior of a live TUI noticing and redrawing (real terminal I/O,
/// the 1s cadence timer driving `tick_reconcile`, reload jitter, or
/// real process-death latency); that half of the budget is a design
/// target (Task 3's 1s cadence + reload-on-change), not something a
/// fixture test can validate. See the spec's Acceptance end-to-end
/// steps 3-6, recorded unverified there.
#[test]
fn db_source_reconcile_dead_pid_row_completes_well_under_budget() {
    use crate::tui::data::DataSource;
    use std::time::{Duration, Instant};

    let (_tmp, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db_path = hcom_dir.join("hcom.db");

    let mut ds = super::DbDataSource::new();
    ds.db_path = db_path;
    assert!(
        ds.ensure_write_db(),
        "fixture: write db must open: {:?}",
        ds.last_error
    );
    insert_local_instance(
        ds.write_db.as_ref().unwrap(),
        "dead_timed",
        crate::shared::ST_ACTIVE,
        DEAD_PID + 2,
    );

    let start = Instant::now();
    let n = ds.reconcile_dead_instances().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(n, 1);
    assert!(
        elapsed < Duration::from_millis(500),
        "fixture DB reconcile took {elapsed:?}; expected near-instant. This measures \
         only the DB-layer half of the spec's <=2s budget, not real TUI wall-clock time \
         (terminal I/O, cadence timer, redraw, real process-death latency)."
    );
}
