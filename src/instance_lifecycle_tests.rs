use super::*;
use std::path::PathBuf;

fn setup_test_db() -> (HcomDb, PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_instance_lifecycle_{}_{}.db",
        std::process::id(),
        test_id
    ));

    let db = HcomDb::open_at(&db_path).unwrap();
    (db, db_path)
}

fn cleanup(path: PathBuf) {
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

/// `WAKE_STATE` is process-global, so a test that arms grace would leak it
/// into any reaper test running beside it. Serialize the ones that care.
static WAKE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn wake_test_guard() -> std::sync::MutexGuard<'static, ()> {
    let guard = WAKE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_wake_state_for_test();
    guard
}

/// A PID above every platform's pid_max, so it names no live process.
const DEAD_PID: i64 = 4_194_305;

/// Insert an instance parked in `active` with a stale status clock — the
/// shape a launched agent has when its heartbeat froze (system sleep).
fn insert_stale_active(db: &HcomDb, name: &str, status_age: i64, heartbeat_age: i64, pid: i64) {
    let now = now_epoch_i64();
    db.conn()
        .execute(
            "INSERT INTO instances
                (name, tool, status, status_context, status_time, last_stop, created_at, \
                 pid, tcp_mode, pid_namespace)
             VALUES (?, 'claude', ?, 'tool:Bash', ?, ?, ?, ?, 1, ?)",
            rusqlite::params![
                name,
                ST_ACTIVE,
                now - status_age,
                now - heartbeat_age,
                (now - status_age) as f64,
                pid,
                crate::sys::process::current_pid_namespace().unwrap_or_default(),
            ],
        )
        .unwrap();
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_pid_namespace(db: &HcomDb, name: &str, namespace: Option<&str>) {
    db.conn()
        .execute(
            "UPDATE instances SET pid_namespace = ? WHERE name = ?",
            rusqlite::params![namespace.unwrap_or(""), name],
        )
        .unwrap();
}

fn instance_exists(db: &HcomDb, name: &str) -> bool {
    db.get_instance_full(name).unwrap().is_some()
}

#[test]
fn test_cleanup_spares_stale_instance_with_live_pid() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    // Our own PID is unambiguously alive.
    insert_stale_active(&db, "alive", 3700, 2400, std::process::id() as i64);

    let deleted = cleanup_stale_instances(&db, 3600, 3600);

    assert_eq!(deleted, 0, "a live process must never be unlinked");
    assert!(instance_exists(&db, "alive"));
    cleanup(path);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn test_cleanup_spares_pid_from_foreign_namespace() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();
    insert_stale_active(&db, "foreign", 3700, 2400, DEAD_PID);
    set_pid_namespace(&db, "foreign", Some("pid:[foreign]"));

    assert_eq!(cleanup_stale_instances(&db, 3600, 3600), 0);
    assert!(instance_exists(&db, "foreign"));
    cleanup(path);
}

#[test]
fn test_cleanup_reaps_stale_instance_with_dead_pid() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    insert_stale_active(&db, "dead", 3700, 2400, DEAD_PID);

    let deleted = cleanup_stale_instances(&db, 3600, 3600);

    assert_eq!(deleted, 1);
    assert!(!instance_exists(&db, "dead"));
    cleanup(path);
}

#[test]
fn test_cleanup_reaps_every_expired_instance_in_one_pass() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    insert_stale_active(&db, "dead1", 3700, 3700, DEAD_PID);
    insert_stale_active(&db, "dead2", 3800, 3800, DEAD_PID);
    insert_stale_active(&db, "dead3", 3900, 3900, DEAD_PID);

    let deleted = cleanup_stale_instances(&db, 3600, 3600);

    assert_eq!(deleted, 3, "one pass should not leave expired rows behind");
    cleanup(path);
}

#[test]
fn test_cleanup_honors_wake_grace_published_by_another_process() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    insert_stale_active(&db, "sleeper", 3700, 2400, DEAD_PID);

    // What a delivery loop writes the moment it observes a wake.
    let grace_until = now_epoch_f64() + WAKE_GRACE_PERIOD;
    db.kv_set("_wake_grace_until", Some(&grace_until.to_string()))
        .unwrap();

    let deleted = cleanup_stale_instances(&db, 3600, 3600);

    assert_eq!(deleted, 0, "cleanup must yield to a published wake window");
    assert!(instance_exists(&db, "sleeper"));

    reset_wake_state_for_test();
    cleanup(path);
}

#[test]
fn test_beacon_gap_grace_arms_once_per_beacon_value() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    insert_stale_active(&db, "dead", 3700, 3700, DEAD_PID);

    // A beacon frozen by the last delivery loop exiting: the gap sits in
    // the wake range but no wake is coming.
    let frozen = now_epoch_f64() - 120.0;
    db.kv_set("_wake_last_wall", Some(&frozen.to_string()))
        .unwrap();

    assert_eq!(
        cleanup_stale_instances(&db, 3600, 3600),
        0,
        "first sighting of a beacon gap should still grace"
    );
    assert!(instance_exists(&db, "dead"));

    // A later `hcom list` is a fresh process, so it starts from a clean
    // WAKE_STATE and re-reads the same unchanged beacon.
    reset_wake_state_for_test();

    assert_eq!(
        cleanup_stale_instances(&db, 3600, 3600),
        1,
        "an unchanged beacon must not keep suppressing cleanup"
    );
    assert!(!instance_exists(&db, "dead"));

    reset_wake_state_for_test();
    cleanup(path);
}

#[test]
fn test_beacon_gap_grace_covers_sleeps_longer_than_an_hour() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    // No PID, so the liveness gate cannot protect this row — the beacon is
    // the only thing standing between an overnight sleep and a reclaim.
    insert_stale_active(&db, "adopted", 7300, 7300, DEAD_PID);
    db.conn()
        .execute(
            "UPDATE instances SET pid = NULL WHERE name = 'adopted'",
            rusqlite::params![],
        )
        .unwrap();

    let slept_two_hours = now_epoch_f64() - 7200.0;
    db.kv_set("_wake_last_wall", Some(&slept_two_hours.to_string()))
        .unwrap();

    assert_eq!(
        cleanup_stale_instances(&db, 3600, 3600),
        0,
        "a multi-hour sleep is still a wake worth gracing"
    );
    assert!(instance_exists(&db, "adopted"));

    reset_wake_state_for_test();
    cleanup(path);
}

#[test]
fn test_publishing_wake_grace_leaves_state_a_one_shot_can_read() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    is_in_wake_grace_publishing(&db);

    assert!(
        db.kv_get("_wake_last_wall").unwrap().is_some(),
        "long-lived loops must publish the liveness beacon"
    );
    cleanup(path);
}

#[test]
fn test_shared_wake_grace_never_publishes() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();

    is_in_wake_grace_shared(&db);

    assert!(
        db.kv_get("_wake_last_wall").unwrap().is_none(),
        "a one-shot writing the beacon would grace every later invocation"
    );
    cleanup(path);
}

#[test]
fn test_active_status_survives_heartbeat_gap_within_grace() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "busy".into(),
        status: ST_ACTIVE.into(),
        status_context: "tool:Bash".into(),
        status_time: now - 3700,
        last_stop: now - (ACTIVE_HEARTBEAT_GRACE - 20),
        tcp_mode: 1,
        ..default_instance()
    };

    let computed = get_instance_status(&data, &db);

    assert_eq!(
        computed.status, ST_ACTIVE,
        "a heartbeat inside the grace window still proves life"
    );
    cleanup(path);
}

#[test]
fn test_active_status_goes_stale_past_heartbeat_grace() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "gone".into(),
        status: ST_ACTIVE.into(),
        status_context: "tool:Bash".into(),
        status_time: now - 3700,
        last_stop: now - (ACTIVE_HEARTBEAT_GRACE + 60),
        tcp_mode: 1,
        ..default_instance()
    };

    let computed = get_instance_status(&data, &db);

    assert_eq!(computed.status, ST_INACTIVE);
    assert_eq!(computed.context, "stale");
    cleanup(path);
}

fn default_instance() -> InstanceRow {
    InstanceRow {
        name: String::new(),
        session_id: None,
        parent_session_id: None,
        parent_name: None,
        agent_id: None,
        tag: None,
        last_event_id: 0,
        last_stop: 0,
        status: ST_INACTIVE.into(),
        status_time: 0,
        last_seen: 0,
        status_context: String::new(),
        status_detail: String::new(),
        directory: String::new(),
        created_at: 0.0,
        transcript_path: String::new(),
        tool: "claude".into(),
        background: 0,
        background_log_file: String::new(),
        tcp_mode: 0,
        wait_timeout: None,
        subagent_timeout: None,
        hints: None,
        origin_device_id: None,
        pid: None,
        pid_namespace: None,
        launch_args: None,
        terminal_preset_requested: None,
        terminal_preset_effective: None,
        launch_context: None,
        name_announced: 0,
        idle_since: None,
    }
}

#[test]
fn test_status_launching_new() {
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_INACTIVE.into(),
        status_context: "new".into(),
        created_at: now as f64,
        ..default_instance()
    };

    let result = get_instance_status(&data, &db);
    assert_eq!(result.status, ST_LAUNCHING);
    assert_eq!(result.context, "new");
    cleanup(path);
}

#[test]
fn test_status_launch_failed() {
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_INACTIVE.into(),
        status_context: "new".into(),
        created_at: (now - LAUNCH_PLACEHOLDER_TIMEOUT - 1) as f64,
        ..default_instance()
    };

    let result = get_instance_status(&data, &db);
    assert_eq!(result.status, ST_INACTIVE);
    assert_eq!(result.context, "launch_failed");
    cleanup(path);
}

fn assert_pi_family_skips_duplicate(tool: &str) {
    let (db, path) = setup_test_db();
    let mut row = serde_json::Map::new();
    row.insert("name".into(), serde_json::json!("luna"));
    row.insert("tool".into(), serde_json::json!(tool));
    row.insert("status".into(), serde_json::json!(ST_ACTIVE));
    row.insert("status_context".into(), serde_json::json!("tool:bash"));
    row.insert("status_detail".into(), serde_json::json!("echo hi"));
    row.insert("status_time".into(), serde_json::json!(1));
    row.insert("last_stop".into(), serde_json::json!(0));
    row.insert("created_at".into(), serde_json::json!(1.0));
    db.save_instance_named("luna", &row).unwrap();

    // Two identical unchanged writes (reportStatus + beforetool) → one event.
    set_status(
        &db,
        "luna",
        ST_ACTIVE,
        "tool:bash",
        StatusUpdate {
            detail: "ls -la",
            ..Default::default()
        },
    );
    set_status(
        &db,
        "luna",
        ST_ACTIVE,
        "tool:bash",
        StatusUpdate {
            detail: "ls -la",
            ..Default::default()
        },
    );
    let event_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'status' AND instance = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        event_count, 1,
        "pi-family tool {tool} should dedup identical status events"
    );
    cleanup(path);
}

#[test]
fn test_set_status_dedup_covers_pi_family() {
    assert_pi_family_skips_duplicate("pi");
    assert_pi_family_skips_duplicate("omp");
}

#[test]
fn test_set_status_skips_duplicate_status_events_but_refreshes_heartbeat() {
    let (db, path) = setup_test_db();
    let mut row = serde_json::Map::new();
    row.insert("name".into(), serde_json::json!("luna"));
    row.insert("tool".into(), serde_json::json!("pi"));
    row.insert("status".into(), serde_json::json!(ST_ACTIVE));
    row.insert("status_context".into(), serde_json::json!("prompt"));
    row.insert("status_detail".into(), serde_json::json!(""));
    row.insert("status_time".into(), serde_json::json!(1));
    row.insert("last_stop".into(), serde_json::json!(0));
    row.insert("created_at".into(), serde_json::json!(1.0));
    db.save_instance_named("luna", &row).unwrap();

    set_status(&db, "luna", ST_LISTENING, "", Default::default());
    let event_count_after_change: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'status' AND instance = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count_after_change, 1);
    let first_last_stop: i64 = db.get_instance_full("luna").unwrap().unwrap().last_stop;

    set_status(&db, "luna", ST_LISTENING, "", Default::default());
    let event_count_after_duplicate: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'status' AND instance = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count_after_duplicate, 1);
    let refreshed_last_stop: i64 = db.get_instance_full("luna").unwrap().unwrap().last_stop;
    assert!(refreshed_last_stop >= first_last_stop);

    cleanup(path);
}

#[test]
fn test_set_status_logs_duplicate_status_events_for_non_pi_tools() {
    let (db, path) = setup_test_db();
    let mut row = serde_json::Map::new();
    row.insert("name".into(), serde_json::json!("luna"));
    row.insert("tool".into(), serde_json::json!("claude"));
    row.insert("status".into(), serde_json::json!(ST_LISTENING));
    row.insert("status_context".into(), serde_json::json!(""));
    row.insert("status_detail".into(), serde_json::json!(""));
    row.insert("status_time".into(), serde_json::json!(1));
    row.insert("last_stop".into(), serde_json::json!(0));
    row.insert("created_at".into(), serde_json::json!(1.0));
    db.save_instance_named("luna", &row).unwrap();

    set_status(&db, "luna", ST_LISTENING, "", Default::default());
    set_status(&db, "luna", ST_LISTENING, "", Default::default());

    let event_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'status' AND instance = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 2);

    cleanup(path);
}

#[test]
fn test_finalize_launch_failure_detail_uses_fallback() {
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut row = serde_json::Map::new();
    row.insert("name".into(), serde_json::json!("test"));
    row.insert("status".into(), serde_json::json!(ST_INACTIVE));
    row.insert("status_context".into(), serde_json::json!("new"));
    row.insert(
        "created_at".into(),
        serde_json::json!((now - LAUNCH_PLACEHOLDER_TIMEOUT - 1) as f64),
    );
    row.insert("status_time".into(), serde_json::json!(0));
    row.insert("tool".into(), serde_json::json!("codex"));
    db.save_instance_named("test", &row).unwrap();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_INACTIVE.into(),
        status_context: "new".into(),
        created_at: (now - LAUNCH_PLACEHOLDER_TIMEOUT - 1) as f64,
        ..default_instance()
    };

    let detail = finalize_launch_failure_detail(
        &db,
        &data,
        Some("process exited before startup completed (exit code 1)"),
    );
    assert_eq!(
        detail.as_deref(),
        Some("process exited before startup completed (exit code 1)")
    );

    let stored = db.get_instance_full("test").unwrap().unwrap();
    assert_eq!(stored.status_context, "launch_failed");
    assert_eq!(
        stored.status_detail,
        "process exited before startup completed (exit code 1)"
    );
    cleanup(path);
}

#[test]
fn test_finalize_launch_failure_detail_leaves_fresh_placeholder_launching() {
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut row = serde_json::Map::new();
    row.insert("name".into(), serde_json::json!("test"));
    row.insert("status".into(), serde_json::json!(ST_INACTIVE));
    row.insert("status_context".into(), serde_json::json!("new"));
    row.insert("created_at".into(), serde_json::json!(now as f64));
    row.insert("status_time".into(), serde_json::json!(0));
    row.insert("tool".into(), serde_json::json!("codex"));
    db.save_instance_named("test", &row).unwrap();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_INACTIVE.into(),
        status_context: "new".into(),
        created_at: now as f64,
        ..default_instance()
    };

    let detail = finalize_launch_failure_detail(&db, &data, None);
    assert_eq!(detail, None);

    let stored = db.get_instance_full("test").unwrap().unwrap();
    assert_eq!(stored.status_context, "new");
    cleanup(path);
}

#[test]
fn test_parse_tmux_launch_failure_output_prefers_error() {
    let captured = "\
Starting Codex...
WARNING: proceeding, even though we could not update PATH: Operation not permitted (os error 1)
Error: Operation not permitted (os error 1)
";

    let result = parse_tmux_launch_failure_output(captured, "codex");
    assert_eq!(
        result.as_deref(),
        Some(
            "Error: Operation not permitted (os error 1) Fully reset tmux first (`tmux kill-server`), then start a fresh tmux server with approval/escalation (for example: `tmux new-session -d -s hcom-external`), then retry."
        )
    );
}

#[test]
fn test_parse_tmux_launch_failure_output_falls_back_to_warning() {
    let captured = "\
Starting Codex...
WARNING: proceeding, even though we could not update PATH: Operation not permitted (os error 1)
";

    let result = parse_tmux_launch_failure_output(captured, "codex");
    assert_eq!(
        result.as_deref(),
        Some(
            "WARNING: proceeding, even though we could not update PATH: Operation not permitted (os error 1) Fully reset tmux first (`tmux kill-server`), then start a fresh tmux server with approval/escalation (for example: `tmux new-session -d -s hcom-external`), then retry."
        )
    );
}

#[test]
fn test_status_listening_fresh_heartbeat() {
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_LISTENING.into(),
        status_time: now - 5,
        last_stop: now - 2,
        tcp_mode: 1,
        ..default_instance()
    };

    let result = get_instance_status(&data, &db);
    assert_eq!(result.status, ST_LISTENING);
    assert_eq!(result.age_string, "now");
    cleanup(path);
}

#[test]
fn test_status_listening_stale_heartbeat() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_LISTENING.into(),
        status_time: now - 100,
        last_stop: now - 100,
        tcp_mode: 1,
        ..default_instance()
    };

    let result = get_instance_status(&data, &db);
    assert_eq!(result.status, ST_INACTIVE);
    assert!(
        result.context.starts_with("stale"),
        "context should be stale, got: {}",
        result.context
    );
    cleanup(path);
}

#[test]
fn test_status_active_stale_activity() {
    let _guard = wake_test_guard();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_ACTIVE.into(),
        status_context: "tool:Bash".into(),
        status_time: now - STATUS_ACTIVITY_TIMEOUT - 10,
        last_stop: 0,
        ..default_instance()
    };

    let result = get_instance_status(&data, &db);
    assert_eq!(result.status, ST_INACTIVE);
    assert!(result.context.starts_with("stale"));
    cleanup(path);
}

#[test]
fn test_status_remote_instance_trusted() {
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let data = InstanceRow {
        name: "test".into(),
        status: ST_LISTENING.into(),
        status_time: now - 100,
        last_stop: 0,
        origin_device_id: Some("device-abc".into()),
        ..default_instance()
    };

    let result = get_instance_status(&data, &db);
    assert_eq!(result.status, ST_LISTENING);
    cleanup(path);
}

#[test]
fn test_status_descriptions() {
    assert_eq!(
        get_status_description(ST_ACTIVE, "tool:Bash"),
        "active: Bash"
    );
    assert_eq!(
        get_status_description(ST_ACTIVE, "deliver:luna"),
        "active: msg from luna"
    );
    assert_eq!(get_status_description(ST_ACTIVE, ""), "active");
    assert_eq!(get_status_description(ST_LISTENING, ""), "listening");
    assert_eq!(
        get_status_description(ST_LISTENING, "tui:not-ready"),
        "listening: blocked"
    );
    // An escalated (stalled) gate keeps the friendly reason and is marked.
    assert_eq!(
        get_status_description(ST_LISTENING, "tui:not-idle:stalled"),
        "listening: waiting for idle (stalled)"
    );
    assert_eq!(
        get_status_description(ST_LISTENING, "tui:prompt-has-text:stalled"),
        "listening: uncommitted text (stalled)"
    );
    assert_eq!(
        get_status_description(ST_BLOCKED, ""),
        "blocked: permission needed"
    );
    assert_eq!(
        get_status_description(ST_INACTIVE, "stale:listening"),
        "inactive: stale"
    );
    assert_eq!(
        get_status_description(ST_INACTIVE, "exit:timeout"),
        "inactive: timeout"
    );
}

#[test]
fn test_cleanup_stale_placeholders_deletes_old() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let old_time = now_epoch_f64() - 200.0;
    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("stale"));
    data.insert("status".into(), serde_json::json!("pending"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert("created_at".into(), serde_json::json!(old_time));
    db.save_instance_named("stale", &data).unwrap();

    let deleted = cleanup_stale_placeholders(&db);
    assert_eq!(deleted, 1);
    assert!(db.get_instance_full("stale").unwrap().is_none());
    let placeholder: i64 = db
        .conn()
        .query_row(
            "SELECT COALESCE(json_extract(data, '$.placeholder'), 0)
             FROM events
             WHERE type = 'life' AND instance = 'stale'
             ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(placeholder, 1);

    cleanup(path);
}

#[test]
fn test_cleanup_stale_placeholders_keeps_fresh() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let now = now_epoch_f64();
    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("fresh"));
    data.insert("status".into(), serde_json::json!("pending"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert("created_at".into(), serde_json::json!(now));
    db.save_instance_named("fresh", &data).unwrap();

    let deleted = cleanup_stale_placeholders(&db);
    assert_eq!(deleted, 0);
    assert!(db.get_instance_full("fresh").unwrap().is_some());

    cleanup(path);
}

#[test]
fn test_cleanup_stale_placeholders_skips_non_placeholder() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let old_time = now_epoch_f64() - 200.0;
    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("real"));
    data.insert("session_id".into(), serde_json::json!("sess-1"));
    data.insert("status".into(), serde_json::json!("pending"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert("created_at".into(), serde_json::json!(old_time));
    db.save_instance_named("real", &data).unwrap();

    let deleted = cleanup_stale_placeholders(&db);
    assert_eq!(deleted, 0);
    assert!(db.get_instance_full("real").unwrap().is_some());

    cleanup(path);
}

#[test]
fn mark_dead_deletes_all_session_aliases() {
    let (db, path) = setup_test_db();
    // Recreate session_bindings without ON DELETE CASCADE. No migration
    // touches this table and init_db uses CREATE TABLE IF NOT EXISTS, so
    // an existing table's shape is never rebuilt on upgrade — this pins
    // the explicit delete as correct independent of FK enforcement.
    db.conn()
        .execute_batch(
            "DROP TABLE session_bindings;
             CREATE TABLE session_bindings (session_id TEXT PRIMARY KEY, instance_name TEXT NOT NULL, created_at REAL NOT NULL);
             CREATE INDEX idx_session_bindings_instance ON session_bindings(instance_name);",
        )
        .unwrap();
    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("deadc"));
    data.insert("tool".into(), serde_json::json!("cursor"));
    data.insert("status".into(), serde_json::json!(ST_LISTENING));
    data.insert("pid".into(), serde_json::json!(1_000_000_007));
    data.insert("created_at".into(), serde_json::json!(1.0));
    db.save_instance_named("deadc", &data).unwrap();

    db.rebind_session("uuid-a", "deadc").unwrap();
    db.rebind_session("uuid-b", "deadc").unwrap();
    let n = mark_dead_instances(&db);
    assert!(n >= 1);
    assert_eq!(db.get_session_binding("uuid-a").unwrap(), None);
    assert_eq!(db.get_session_binding("uuid-b").unwrap(), None);
    cleanup(path);
}

fn insert_active_with_session(
    db: &HcomDb,
    name: &str,
    session_id: &str,
    created_at: f64,
    pid: i64,
) {
    let now = now_epoch_i64();
    db.conn()
        .execute(
            "INSERT INTO instances
                (name, tool, session_id, status, status_context, status_time, \
                 last_stop, created_at, pid, tcp_mode, pid_namespace)
             VALUES (?, 'claude', ?, ?, '', ?, ?, ?, ?, 1, ?)",
            rusqlite::params![
                name,
                session_id,
                ST_ACTIVE,
                now,
                now,
                created_at,
                pid,
                crate::sys::process::current_pid_namespace().unwrap_or_default()
            ],
        )
        .unwrap();
}

fn last_life_field(db: &HcomDb, name: &str, field: &str) -> String {
    let data: String = db
        .conn()
        .query_row(
            "SELECT data FROM events WHERE type = 'life' AND instance = ? \
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![name],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str::<serde_json::Value>(&data).unwrap()[field]
        .as_str()
        .unwrap()
        .to_string()
}

fn last_life_reason(db: &HcomDb, name: &str) -> String {
    last_life_field(db, name, "reason")
}

fn life_event_count(db: &HcomDb, name: &str) -> i64 {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = ?",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn reconcile_preserves_row_rebound_to_another_pid_namespace() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "rebound", "session", 1.0, 4242);
    crate::hooks::common::claim_stop_reason(&db, "rebound", 1.0, "user", "killed");
    let reconciled =
        reconcile_dead_instances_with_probe(&db, DeadProcessDetector::Tui, |inst, _| {
            // Rebind between the reaper's snapshot/probe and DELETE,
            // keeping the numeric PID and every other guarded field.
            db.conn()
                .execute(
                    "UPDATE instances SET pid_namespace = 'pid:[foreign]' WHERE name = ?",
                    [&inst.name],
                )
                .unwrap();
            ProcessProbe::Dead
        })
        .unwrap();
    assert_eq!(reconciled, 0);
    assert_eq!(
        db.get_instance_full("rebound")
            .unwrap()
            .unwrap()
            .pid_namespace
            .as_deref(),
        Some("pid:[foreign]")
    );
    assert!(crate::hooks::common::read_stop_reason(&db, "rebound", 1.0).is_some());
    let events: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'rebound'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 0);
    cleanup(path);
}

/// Step 1 regression fixture: a probe-injected pass over every row shape
/// the reaper's contract distinguishes. Only the local active/listening
/// rows whose PID probes `Dead` get reconciled; everything else — an
/// `Alive` or `Unknown` probe result, remote, PID-less, `launching`, or
/// `inactive` — must survive untouched. The one row that also carries
/// session aliases/process binding/notify endpoint/subscription proves
/// the full cascade still runs, and exactly one stopped life event is
/// published per successful cleanup.
#[test]
fn reconcile_dead_instances_removes_only_dead_local_active_or_listening_rows() {
    let (db, path) = setup_test_db();

    // Dead + active + local, with full cascade fan-out. Must be removed.
    insert_active_with_session(&db, "dead_active", "sess-active", 1.0, 100);
    db.rebind_session("alias-active", "dead_active").unwrap();
    db.conn()
        .execute(
            "INSERT INTO process_bindings (process_id, session_id, instance_name, updated_at) \
             VALUES ('proc-active', 'sess-active', 'dead_active', 0)",
            [],
        )
        .unwrap();
    db.register_notify_port("dead_active", 9001).unwrap();
    db.kv_set(
        "events_sub:sub-active",
        Some(&serde_json::json!({"caller": "dead_active", "events": ["life"]}).to_string()),
    )
    .unwrap();

    // Dead + listening + local. Must be removed.
    let mut listening = serde_json::Map::new();
    listening.insert("name".into(), serde_json::json!("dead_listening"));
    listening.insert("tool".into(), serde_json::json!("claude"));
    listening.insert("status".into(), serde_json::json!(ST_LISTENING));
    listening.insert("pid".into(), serde_json::json!(200));
    listening.insert("created_at".into(), serde_json::json!(2.0));
    db.save_instance_named("dead_listening", &listening)
        .unwrap();

    // Alive + active + local. Probe says Alive — must survive.
    insert_active_with_session(&db, "alive_active", "sess-alive", 3.0, 300);

    // Unknown probe result (uncertain PID check). Fail-safe — must survive.
    insert_active_with_session(&db, "unknown_active", "sess-unknown", 4.0, 400);

    // Remote instance, dead PID. Skipped regardless of probe.
    let mut remote = serde_json::Map::new();
    remote.insert("name".into(), serde_json::json!("remote_dead"));
    remote.insert("tool".into(), serde_json::json!("claude"));
    remote.insert("status".into(), serde_json::json!(ST_ACTIVE));
    remote.insert("pid".into(), serde_json::json!(500));
    remote.insert("created_at".into(), serde_json::json!(5.0));
    remote.insert("origin_device_id".into(), serde_json::json!("device-1"));
    db.save_instance_named("remote_dead", &remote).unwrap();

    // PID-less instance. Skipped (nothing to probe).
    let mut pidless = serde_json::Map::new();
    pidless.insert("name".into(), serde_json::json!("pidless_active"));
    pidless.insert("tool".into(), serde_json::json!("claude"));
    pidless.insert("status".into(), serde_json::json!(ST_ACTIVE));
    pidless.insert("created_at".into(), serde_json::json!(6.0));
    db.save_instance_named("pidless_active", &pidless).unwrap();

    // launching, dead PID. Skipped per the existing status contract.
    let mut launching = serde_json::Map::new();
    launching.insert("name".into(), serde_json::json!("launching_dead"));
    launching.insert("tool".into(), serde_json::json!("claude"));
    launching.insert("status".into(), serde_json::json!(ST_LAUNCHING));
    launching.insert("pid".into(), serde_json::json!(700));
    launching.insert("created_at".into(), serde_json::json!(7.0));
    db.save_instance_named("launching_dead", &launching)
        .unwrap();

    // inactive, dead PID. Skipped per the existing status contract.
    let mut inactive = serde_json::Map::new();
    inactive.insert("name".into(), serde_json::json!("inactive_dead"));
    inactive.insert("tool".into(), serde_json::json!("claude"));
    inactive.insert("status".into(), serde_json::json!(ST_INACTIVE));
    inactive.insert("pid".into(), serde_json::json!(800));
    inactive.insert("created_at".into(), serde_json::json!(8.0));
    db.save_instance_named("inactive_dead", &inactive).unwrap();

    let reconciled =
        reconcile_dead_instances_with_probe(&db, DeadProcessDetector::Tui, |_, pid| match pid {
            100 | 200 => ProcessProbe::Dead,
            300 => ProcessProbe::Alive,
            400 => ProcessProbe::Unknown,
            other => {
                panic!("unexpected probe on pid {other}: skip-list rows must never reach the probe")
            }
        })
        .unwrap();

    assert_eq!(reconciled, 2);
    assert!(db.get_instance_full("dead_active").unwrap().is_none());
    assert!(db.get_instance_full("dead_listening").unwrap().is_none());
    assert!(db.get_instance_full("alive_active").unwrap().is_some());
    assert!(db.get_instance_full("unknown_active").unwrap().is_some());
    assert!(db.get_instance_full("remote_dead").unwrap().is_some());
    assert!(db.get_instance_full("pidless_active").unwrap().is_some());
    assert!(db.get_instance_full("launching_dead").unwrap().is_some());
    assert!(db.get_instance_full("inactive_dead").unwrap().is_some());

    // Full cascade ran for the identity-cleaned row.
    assert_eq!(db.get_session_binding("sess-active").unwrap(), None);
    assert_eq!(db.get_session_binding("alias-active").unwrap(), None);
    let proc_bindings: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM process_bindings WHERE instance_name = 'dead_active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(proc_bindings, 0);
    let notify: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM notify_endpoints WHERE instance = 'dead_active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(notify, 0);
    let sub: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM kv WHERE key = 'events_sub:sub-active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sub, 0);

    // Exactly one stopped life event per successful identity cleanup,
    // each stamped with the detector that ran the reconcile pass.
    assert_eq!(life_event_count(&db, "dead_active"), 1);
    assert_eq!(life_event_count(&db, "dead_listening"), 1);
    assert_eq!(last_life_field(&db, "dead_active", "detector"), "tui");
    assert_eq!(last_life_field(&db, "dead_listening", "detector"), "tui");
    assert_eq!(last_life_reason(&db, "dead_active"), "exit:dead_process");

    cleanup(path);
}

#[test]
fn mark_dead_prefers_claimed_stop_reason() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "sess-kume", 1000.0, DEAD_PID);

    crate::hooks::common::claim_stop_reason(&db, "kume", 1000.0, "alam", "killed");
    assert_eq!(mark_dead_instances(&db), 1);
    assert_eq!(last_life_reason(&db, "kume"), "killed");
    assert_eq!(last_life_field(&db, "kume", "by"), "alam");

    cleanup(path);
}

#[test]
fn mark_dead_preserves_claim_when_event_write_fails() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "sess-kume", 1000.0, DEAD_PID);
    crate::hooks::common::claim_stop_reason(&db, "kume", 1000.0, "alam", "killed");
    db.conn()
        .execute_batch(
            "CREATE TRIGGER reject_life BEFORE INSERT ON events
         WHEN NEW.type = 'life' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        )
        .unwrap();

    assert_eq!(mark_dead_instances(&db), 0);
    assert!(db.get_instance_full("kume").unwrap().is_some());
    db.conn().execute_batch("DROP TRIGGER reject_life").unwrap();
    assert_eq!(mark_dead_instances(&db), 1);
    assert_eq!(last_life_reason(&db, "kume"), "killed");
    assert_eq!(last_life_field(&db, "kume", "by"), "alam");
    assert!(db.kv_get("stop_reason:kume").unwrap().is_none());
    cleanup(path);
}

#[test]
fn stale_stopper_cannot_overwrite_resumed_instances_claim() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "sess-kume", 2000.0, DEAD_PID);
    crate::hooks::common::claim_stop_reason(&db, "kume", 2000.0, "alam", "killed");
    // An older stopper resumes after the name has already been reused.
    crate::hooks::common::claim_stop_reason(&db, "kume", 1000.0, "system", "stale_cleanup");
    assert_eq!(mark_dead_instances(&db), 1);
    assert_eq!(last_life_reason(&db, "kume"), "killed");
    assert_eq!(last_life_field(&db, "kume", "by"), "alam");
    cleanup(path);
}

/// A claim orphaned by a losing `hcom kill` (or SessionEnd) can survive a
/// non-fork `hcom r <name>` resume, which keeps the same name AND the same
/// session_id (prior_session_id, resume.rs). `created_at` is what
/// actually changes across that resume, so it — not session_id — is the
/// discriminator that must reject a stale claim.
#[test]
fn mark_dead_ignores_claim_from_a_different_instance_lifetime() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "sess-kume", 2000.0, DEAD_PID);

    // Chỗ đặt còn sót lại từ một instance cũ trùng tên VÀ trùng session_id
    // (resume không-fork giữ nguyên session_id cũ), nhưng created_at khác.
    db.kv_set("stop_reason:kume", Some("killed|alam|1000"))
        .unwrap();
    assert_eq!(mark_dead_instances(&db), 1);
    assert_eq!(last_life_reason(&db, "kume"), "exit:dead_process");

    cleanup(path);
}

#[test]
fn mark_dead_without_a_claim_reports_dead_process_with_startup_detector() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "sess-kume", 1000.0, DEAD_PID);

    assert_eq!(mark_dead_instances(&db), 1);
    assert_eq!(last_life_reason(&db, "kume"), "exit:dead_process");
    assert_eq!(last_life_field(&db, "kume", "detector"), "startup");

    cleanup(path);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn foreign_pid_namespace_cannot_reap_live_roster() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "claude-session", 1000.0, DEAD_PID);
    insert_active_with_session(&db, "lori", "codex-session", 2000.0, DEAD_PID + 1);
    insert_active_with_session(&db, "legacy", "old-session", 3000.0, DEAD_PID + 2);
    // These PIDs belong to a namespace this CLI cannot inspect. A negative
    // kill(pid, 0) here is not evidence that either agent exited.
    for name in ["kume", "lori"] {
        set_pid_namespace(&db, name, Some("pid:[foreign]"));
    }
    // Predates the column entirely: no evidence either way, so it stays.
    set_pid_namespace(&db, "legacy", None);

    assert_eq!(mark_dead_instances(&db), 0);
    assert!(instance_exists(&db, "kume"));
    assert!(instance_exists(&db, "lori"));
    assert!(instance_exists(&db, "legacy"));
    cleanup(path);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn recording_a_new_pid_replaces_a_stale_foreign_namespace() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "lori", "codex-session", 2000.0, DEAD_PID);
    set_pid_namespace(&db, "lori", Some("pid:[old]"));
    db.store_launch_context("lori", r#"{"pane_id":"pane-1"}"#)
        .unwrap();

    // We are the process that spawned this pid, so our namespace is the
    // one that describes it.
    db.update_instance_pid("lori", DEAD_PID as u32).unwrap();

    let row = db.get_instance_full("lori").unwrap().unwrap();
    assert_eq!(
        row.pid_namespace.as_deref(),
        crate::sys::process::current_pid_namespace()
    );
    assert_eq!(
        row.launch_context.as_deref(),
        Some(r#"{"pane_id":"pane-1"}"#)
    );
    cleanup(path);
}

/// The regression that made the whole guard a no-op: every vendor's
/// SessionStart rebuilds `launch_context` from scratch, so a namespace
/// marker living in that blob was erased seconds after launch and every
/// dead row became permanently unreapable.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn session_start_context_capture_does_not_disarm_the_reaper() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "kume", "claude-session", 1000.0, DEAD_PID);
    assert_eq!(
        db.get_instance_full("kume")
            .unwrap()
            .unwrap()
            .pid_namespace
            .as_deref(),
        crate::sys::process::current_pid_namespace()
    );

    crate::instance_binding::capture_and_store_launch_context(&db, "kume");

    assert_eq!(
        db.get_instance_full("kume")
            .unwrap()
            .unwrap()
            .pid_namespace
            .as_deref(),
        crate::sys::process::current_pid_namespace(),
        "context capture must not touch the namespace stamped beside the pid"
    );
    assert_eq!(mark_dead_instances(&db), 1);
    assert!(!instance_exists(&db, "kume"));
    cleanup(path);
}

/// An orphan recorded in this namespace carries our marker through the
/// pidfile, and adoption must land it on the row — otherwise the adopted
/// agent could never be reconciled. (The foreign case, where adoption must
/// copy rather than stamp, is covered in `pidtrack`.)
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn adopting_an_orphan_carries_the_observing_namespace_onto_the_row() {
    let (db, path) = setup_test_db();
    let orphan = crate::pidtrack::OrphanProcess {
        pid: DEAD_PID as u32,
        tool: "claude".to_string(),
        directory: "/tmp".to_string(),
        pid_namespace: crate::sys::process::current_pid_namespace()
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };

    crate::pidtrack::recover_single_orphan_to_db(&db, &orphan, "poko").unwrap();

    let row = db.get_instance_full("poko").unwrap().unwrap();
    assert_eq!(row.pid, Some(DEAD_PID));
    assert_eq!(
        row.pid_namespace.as_deref(),
        crate::sys::process::current_pid_namespace()
    );
    cleanup(path);
}

/// Clearing a pid must clear the namespace with it: a namespace left
/// behind would describe a pid that is no longer there.
#[test]
fn clearing_a_pid_clears_its_namespace() {
    let (db, path) = setup_test_db();
    insert_active_with_session(&db, "lori", "codex-session", 2000.0, DEAD_PID);

    crate::instances::update_instance_position(
        &db,
        "lori",
        &serde_json::Map::from_iter([("pid".to_string(), serde_json::Value::Null)]),
    );

    let row = db.get_instance_full("lori").unwrap().unwrap();
    assert_eq!(row.pid, None);
    assert_eq!(row.pid_namespace, None);
    cleanup(path);
}
