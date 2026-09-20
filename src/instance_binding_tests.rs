use super::*;
use serial_test::serial;
use std::path::PathBuf;

fn setup_test_db() -> (HcomDb, PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_instance_binding_{}_{}.db",
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

#[test]
fn auto_subscribe_eligibility_follows_released_specs() {
    assert!(auto_subscribe_eligible("pi"));
    assert!(auto_subscribe_eligible("kimi"));
    assert!(!auto_subscribe_eligible("adhoc"));
    assert!(!auto_subscribe_eligible("unknown"));
}

#[test]
fn test_persist_terminal_launch_context_stores_presets_in_launch_context() {
    let (db, path) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna", "codex", 1.0f64],
        )
        .unwrap();

    persist_terminal_launch_context(&db, "luna", Some("kitty"), "kitty-tab", Some("proc-1"));

    let row = db.get_instance_full("luna").unwrap().unwrap();
    let ctx: serde_json::Value =
        serde_json::from_str(row.launch_context.as_deref().unwrap_or("{}")).unwrap();

    assert_eq!(
        ctx.get("terminal_preset_effective")
            .and_then(|v| v.as_str()),
        Some("kitty-tab")
    );
    assert_eq!(
        ctx.get("terminal_preset").and_then(|v| v.as_str()),
        Some("kitty-tab")
    );
    assert_eq!(
        ctx.get("terminal_preset_requested")
            .and_then(|v| v.as_str()),
        Some("kitty")
    );
    assert_eq!(
        ctx.get("process_id").and_then(|v| v.as_str()),
        Some("proc-1")
    );

    cleanup(path);
}

#[test]
#[serial]
fn test_capture_context_records_launched_preset() {
    // capture_context tags the launch with HCOM_LAUNCHED_PRESET so later
    // child agents launched from inside this pane can inherit the preset.
    // Running tests inside a herdr session would otherwise leak
    // HERDR_PANE_ID into the captured context, so explicitly clear the
    // herdr-related identity vars before exercising the capture path.
    crate::config::Config::init();
    let _preset = EnvVarGuard::set("HCOM_LAUNCHED_PRESET", "herdr");
    let _herdr_pane = EnvVarGuard::set("HERDR_PANE_ID", "");
    let _herdr_socket = EnvVarGuard::set("HERDR_SOCKET_PATH", "");
    let _herdr_env = EnvVarGuard::set("HERDR_ENV", "");
    let _process_id = EnvVarGuard::set("HCOM_PROCESS_ID", "");

    let ctx = capture_context();

    assert_eq!(
        ctx.get("terminal_preset_effective")
            .and_then(|v| v.as_str()),
        Some("herdr")
    );
    assert!(
        ctx.get("pane_id").is_none(),
        "pane_id should be absent when HERDR_PANE_ID isn't set"
    );
}

#[test]
#[serial]
fn test_capture_and_store_launch_context_preserves_terminal_metadata() {
    // Preserve only the fields we can't recapture from hook env:
    // pane_id, terminal_id, kitty_listen_on, process_id, and the resolved
    // terminal preset name.
    let (db, path) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, created_at, launch_context) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "luna",
                "claude",
                1.0f64,
                r#"{"terminal_preset_effective":"herdr","pane_id":"p_7","process_id":"proc-1"}"#
            ],
        )
        .unwrap();
    let _preset = EnvVarGuard::set("HCOM_LAUNCHED_PRESET", "");
    let _process_id = EnvVarGuard::set("HCOM_PROCESS_ID", "");

    capture_and_store_launch_context(&db, "luna");

    let row = db.get_instance_full("luna").unwrap().unwrap();
    let ctx: serde_json::Value =
        serde_json::from_str(row.launch_context.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(
        ctx.get("terminal_preset_effective")
            .and_then(|v| v.as_str()),
        Some("herdr")
    );
    assert_eq!(ctx.get("pane_id").and_then(|v| v.as_str()), Some("p_7"));
    assert_eq!(
        ctx.get("process_id").and_then(|v| v.as_str()),
        Some("proc-1")
    );

    cleanup(path);
}

#[test]
fn test_bind_session_path2_placeholder() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("luna"));
    data.insert("status".into(), serde_json::json!("pending"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert("created_at".into(), serde_json::json!(now));
    db.save_instance_named("luna", &data).unwrap();

    db.set_process_binding("pid-123", "", "luna").unwrap();

    let result = bind_session_to_process(&db, "sid-456", Some("pid-123"));
    assert_eq!(result, Some("luna".to_string()));

    let inst = db.get_instance_full("luna").unwrap().unwrap();
    assert_eq!(inst.session_id.as_deref(), Some("sid-456"));

    let binding = db.get_session_binding("sid-456").unwrap();
    assert_eq!(binding, Some("luna".to_string()));

    cleanup(path);
}

#[test]
fn test_bind_session_path1a_true_placeholder_merge() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut canonical_data = serde_json::Map::new();
    canonical_data.insert("name".into(), serde_json::json!("miso"));
    canonical_data.insert("session_id".into(), serde_json::json!("sid-789"));
    canonical_data.insert("created_at".into(), serde_json::json!(now));
    canonical_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("miso", &canonical_data).unwrap();
    db.rebind_session("sid-789", "miso").unwrap();

    let mut ph_data = serde_json::Map::new();
    ph_data.insert("name".into(), serde_json::json!("temp"));
    ph_data.insert("tag".into(), serde_json::json!("team"));
    ph_data.insert("created_at".into(), serde_json::json!(now));
    ph_data.insert("status".into(), serde_json::json!("pending"));
    ph_data.insert("status_context".into(), serde_json::json!("new"));
    db.save_instance_named("temp", &ph_data).unwrap();

    db.set_process_binding("pid-123", "", "temp").unwrap();

    let result = bind_session_to_process(&db, "sid-789", Some("pid-123"));
    assert_eq!(result, Some("miso".to_string()));

    assert!(db.get_instance_full("temp").unwrap().is_none());

    let inst = db.get_instance_full("miso").unwrap().unwrap();
    assert_eq!(inst.tag.as_deref(), Some("team"));

    cleanup(path);
}

#[test]
fn test_bind_session_path1b_cursor_keeps_process_instance() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut canonical_data = serde_json::Map::new();
    canonical_data.insert("name".into(), serde_json::json!("miso"));
    canonical_data.insert("session_id".into(), serde_json::json!("sid-789"));
    canonical_data.insert("tool".into(), serde_json::json!("cursor"));
    canonical_data.insert("created_at".into(), serde_json::json!(now));
    canonical_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("miso", &canonical_data).unwrap();
    db.rebind_session("sid-789", "miso").unwrap();

    let mut ph_data = serde_json::Map::new();
    ph_data.insert("name".into(), serde_json::json!("temp"));
    ph_data.insert("session_id".into(), serde_json::json!("sid-old"));
    ph_data.insert("tool".into(), serde_json::json!("cursor"));
    ph_data.insert("created_at".into(), serde_json::json!(now));
    ph_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("temp", &ph_data).unwrap();
    db.rebind_session("sid-old", "temp").unwrap();
    db.set_process_binding("pid-123", "sid-old", "temp")
        .unwrap();

    let result = bind_session_to_process(&db, "sid-789", Some("pid-123"));
    assert_eq!(result, Some("miso".to_string()));

    let placeholder = db.get_instance_full("temp").unwrap().unwrap();
    assert_ne!(placeholder.status_context, "exit:session_switch");
    assert_ne!(placeholder.status, ST_INACTIVE);

    cleanup(path);
}

#[test]
fn test_bind_session_path1b_non_cursor_still_session_switches() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut canonical_data = serde_json::Map::new();
    canonical_data.insert("name".into(), serde_json::json!("miso"));
    canonical_data.insert("session_id".into(), serde_json::json!("sid-789"));
    canonical_data.insert("tool".into(), serde_json::json!("claude"));
    canonical_data.insert("created_at".into(), serde_json::json!(now));
    canonical_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("miso", &canonical_data).unwrap();
    db.rebind_session("sid-789", "miso").unwrap();

    let mut ph_data = serde_json::Map::new();
    ph_data.insert("name".into(), serde_json::json!("temp"));
    ph_data.insert("session_id".into(), serde_json::json!("sid-old"));
    ph_data.insert("tool".into(), serde_json::json!("claude"));
    ph_data.insert("created_at".into(), serde_json::json!(now));
    ph_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("temp", &ph_data).unwrap();
    db.rebind_session("sid-old", "temp").unwrap();
    db.set_process_binding("pid-123", "sid-old", "temp")
        .unwrap();

    let result = bind_session_to_process(&db, "sid-789", Some("pid-123"));
    assert_eq!(result, Some("miso".to_string()));
    let placeholder = db.get_instance_full("temp").unwrap().unwrap();
    assert_eq!(placeholder.status, ST_INACTIVE);
    assert_eq!(placeholder.status_context, "exit:session_switch");

    cleanup(path);
}

#[test]
fn test_bind_session_path1b_session_switch_marks_old_inactive() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut canonical_data = serde_json::Map::new();
    canonical_data.insert("name".into(), serde_json::json!("miso"));
    canonical_data.insert("session_id".into(), serde_json::json!("sid-789"));
    canonical_data.insert("created_at".into(), serde_json::json!(now));
    canonical_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("miso", &canonical_data).unwrap();
    db.rebind_session("sid-789", "miso").unwrap();

    let mut ph_data = serde_json::Map::new();
    ph_data.insert("name".into(), serde_json::json!("temp"));
    ph_data.insert("session_id".into(), serde_json::json!("sid-old"));
    ph_data.insert("created_at".into(), serde_json::json!(now));
    ph_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("temp", &ph_data).unwrap();
    db.rebind_session("sid-old", "temp").unwrap();
    db.set_process_binding("pid-123", "sid-old", "temp")
        .unwrap();

    let result = bind_session_to_process(&db, "sid-789", Some("pid-123"));
    assert_eq!(result, Some("miso".to_string()));

    let placeholder = db.get_instance_full("temp").unwrap().unwrap();
    assert_eq!(placeholder.status, ST_INACTIVE);
    assert_eq!(placeholder.status_context, "exit:session_switch");

    assert_eq!(db.get_session_binding("sid-old").unwrap(), None);
    assert_eq!(
        db.get_process_binding("pid-123").unwrap(),
        Some("miso".to_string())
    );

    cleanup(path);
}

#[test]
fn test_bind_session_no_match() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let result = bind_session_to_process(&db, "sid-999", None);
    assert_eq!(result, None);

    cleanup(path);
}

#[test]
fn test_create_orphaned_pty_identity_basic() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let result = create_orphaned_pty_identity(&db, "sess-orphan", Some("pid-orphan"), "claude");
    assert!(result.is_some(), "should create orphaned identity");

    let name = result.unwrap();
    let inst = db.get_instance_full(&name).unwrap().unwrap();
    assert_eq!(inst.session_id.as_deref(), Some("sess-orphan"));
    assert_eq!(inst.tool, "claude");

    assert_eq!(
        db.get_session_binding("sess-orphan").unwrap(),
        Some(name.clone())
    );
    assert_eq!(db.get_process_binding("pid-orphan").unwrap(), Some(name));

    cleanup(path);
}

#[test]
fn test_create_orphaned_pty_identity_no_process_id() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let result = create_orphaned_pty_identity(&db, "sess-orphan2", None, "gemini");
    assert!(result.is_some());

    let name = result.unwrap();
    let inst = db.get_instance_full(&name).unwrap().unwrap();
    assert_eq!(inst.tool, "gemini");
    assert_eq!(db.get_session_binding("sess-orphan2").unwrap(), Some(name));

    cleanup(path);
}

#[test]
fn test_resolve_from_binding_process_binding() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("luna"));
    data.insert("session_id".into(), serde_json::json!("sess-1"));
    data.insert("created_at".into(), serde_json::json!(now));
    data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("luna", &data).unwrap();
    db.set_process_binding("pid-1", "sess-1", "luna").unwrap();

    let result = resolve_instance_from_binding(&db, None, Some("pid-1"));
    assert!(result.is_some());
    assert_eq!(result.unwrap().name, "luna");

    cleanup(path);
}

#[test]
fn test_resolve_from_binding_session_binding() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("nova"));
    data.insert("session_id".into(), serde_json::json!("sess-2"));
    data.insert("created_at".into(), serde_json::json!(now));
    data.insert("status".into(), serde_json::json!("active"));
    db.save_instance_named("nova", &data).unwrap();
    db.rebind_session("sess-2", "nova").unwrap();

    let result = resolve_instance_from_binding(&db, Some("sess-2"), None);
    assert!(result.is_some());
    assert_eq!(result.unwrap().name, "nova");

    cleanup(path);
}

#[test]
fn test_resolve_from_binding_process_over_session() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut d1 = serde_json::Map::new();
    d1.insert("name".into(), serde_json::json!("luna"));
    d1.insert("created_at".into(), serde_json::json!(now));
    d1.insert("status".into(), serde_json::json!("active"));
    db.save_instance_named("luna", &d1).unwrap();
    db.set_process_binding("pid-1", "", "luna").unwrap();

    let mut d2 = serde_json::Map::new();
    d2.insert("name".into(), serde_json::json!("nova"));
    d2.insert("session_id".into(), serde_json::json!("sess-2"));
    d2.insert("created_at".into(), serde_json::json!(now));
    d2.insert("status".into(), serde_json::json!("active"));
    db.save_instance_named("nova", &d2).unwrap();
    db.rebind_session("sess-2", "nova").unwrap();

    let result = resolve_instance_from_binding(&db, Some("sess-2"), Some("pid-1"));
    assert_eq!(result.unwrap().name, "luna");

    cleanup(path);
}

#[test]
fn test_resolve_from_binding_process_binding_instance_deleted() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    db.set_process_binding("pid-ghost", "", "ghost").unwrap();

    let result = resolve_instance_from_binding(&db, None, Some("pid-ghost"));
    assert!(result.is_none());

    cleanup(path);
}

#[test]
fn test_resolve_from_binding_no_match() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();

    let result = resolve_instance_from_binding(&db, Some("nonexistent"), Some("nope"));
    assert!(result.is_none());

    cleanup(path);
}

#[test]
fn test_session_binding_cascade_on_instance_delete() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("luna"));
    data.insert("session_id".into(), serde_json::json!("sess-1"));
    data.insert("created_at".into(), serde_json::json!(now));
    data.insert("status".into(), serde_json::json!("active"));
    db.save_instance_named("luna", &data).unwrap();
    db.rebind_session("sess-1", "luna").unwrap();

    assert_eq!(
        db.get_session_binding("sess-1").unwrap(),
        Some("luna".to_string())
    );

    db.delete_instance("luna").unwrap();
    assert_eq!(db.get_session_binding("sess-1").unwrap(), None);

    cleanup(path);
}

#[test]
fn test_bind_session_restores_deleted_canonical_from_placeholder() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut canonical_data = serde_json::Map::new();
    canonical_data.insert("name".into(), serde_json::json!("miso"));
    canonical_data.insert("session_id".into(), serde_json::json!("sid-resume"));
    canonical_data.insert("tool".into(), serde_json::json!("antigravity"));
    canonical_data.insert("created_at".into(), serde_json::json!(now));
    canonical_data.insert("status".into(), serde_json::json!("listening"));
    db.save_instance_named("miso", &canonical_data).unwrap();
    db.rebind_session("sid-resume", "miso").unwrap();

    let mut ph_data = serde_json::Map::new();
    ph_data.insert("name".into(), serde_json::json!("nova"));
    ph_data.insert("tool".into(), serde_json::json!("antigravity"));
    ph_data.insert("tag".into(), serde_json::json!("work"));
    ph_data.insert("created_at".into(), serde_json::json!(now));
    ph_data.insert("status".into(), serde_json::json!("pending"));
    ph_data.insert("status_context".into(), serde_json::json!("new"));
    db.save_instance_named("nova", &ph_data).unwrap();
    db.set_process_binding("pid-agy", "", "nova").unwrap();

    let snapshot = serde_json::json!({
        "session_id": "sid-resume",
        "tool": "antigravity",
        "tag": "work",
    });
    db.log_life_event("miso", "stopped", "test", "exit", Some(snapshot))
        .unwrap();

    db.delete_instance("miso").unwrap();
    assert!(db.get_instance_full("miso").unwrap().is_none());
    assert_eq!(db.get_session_binding("sid-resume").unwrap(), None);

    let result = bind_session_to_process(&db, "sid-resume", Some("pid-agy"));
    assert_eq!(result, Some("miso".to_string()));

    let restored = db.get_instance_full("miso").unwrap().unwrap();
    assert_eq!(restored.session_id.as_deref(), Some("sid-resume"));
    assert_eq!(restored.tool, "antigravity");
    assert_eq!(restored.tag.as_deref(), Some("work"));
    assert!(db.get_instance_full("nova").unwrap().is_none());

    cleanup(path);
}

#[test]
#[serial]
fn test_bind_session_restore_stopped_deletes_true_placeholder() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut fano_data = serde_json::Map::new();
    fano_data.insert("name".into(), serde_json::json!("fano"));
    fano_data.insert("tool".into(), serde_json::json!("opencode"));
    fano_data.insert("created_at".into(), serde_json::json!(now));
    fano_data.insert("status".into(), serde_json::json!("inactive"));
    db.save_instance_named("fano", &fano_data).unwrap();

    let snapshot = serde_json::json!({
        "session_id": "ses-opencode-1",
        "tool": "opencode",
    });
    db.log_life_event("fano", "stopped", "test", "exit", Some(snapshot))
        .unwrap();

    let mut mozi_data = serde_json::Map::new();
    mozi_data.insert("name".into(), serde_json::json!("mozi"));
    mozi_data.insert("tool".into(), serde_json::json!("opencode"));
    mozi_data.insert("created_at".into(), serde_json::json!(now));
    mozi_data.insert("status".into(), serde_json::json!("pending"));
    mozi_data.insert("status_context".into(), serde_json::json!("new"));
    db.save_instance_named("mozi", &mozi_data).unwrap();
    db.set_process_binding("pid-oc", "", "mozi").unwrap();

    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();
    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();

    let result = bind_session_to_process(&db, "ses-opencode-1", Some("pid-oc"));
    assert_eq!(result, Some("fano".to_string()));

    assert!(db.get_instance_full("mozi").unwrap().is_none());
    let fano = db.get_instance_full("fano").unwrap().unwrap();
    assert_eq!(fano.session_id.as_deref(), Some("ses-opencode-1"));
    assert_eq!(
        db.get_session_binding("ses-opencode-1").unwrap(),
        Some("fano".to_string())
    );
    assert_eq!(
        db.get_process_binding("pid-oc").unwrap(),
        Some("fano".to_string())
    );

    cleanup(path);
}

#[test]
#[serial]
fn test_restore_stopped_migrates_pid_and_launch_context_to_canonical() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut fano_data = serde_json::Map::new();
    fano_data.insert("name".into(), serde_json::json!("fano"));
    fano_data.insert("tool".into(), serde_json::json!("opencode"));
    fano_data.insert("created_at".into(), serde_json::json!(now));
    fano_data.insert("status".into(), serde_json::json!("inactive"));
    db.save_instance_named("fano", &fano_data).unwrap();
    db.log_life_event(
        "fano",
        "stopped",
        "test",
        "exit",
        Some(serde_json::json!({ "session_id": "ses-oc-pid", "tool": "opencode" })),
    )
    .unwrap();

    // Launch placeholder with the runtime state the PTY wrapper writes at spawn.
    let mut mozi_data = serde_json::Map::new();
    mozi_data.insert("name".into(), serde_json::json!("mozi"));
    mozi_data.insert("tool".into(), serde_json::json!("opencode"));
    mozi_data.insert("created_at".into(), serde_json::json!(now));
    mozi_data.insert("status".into(), serde_json::json!("pending"));
    mozi_data.insert("status_context".into(), serde_json::json!("new"));
    db.save_instance_named("mozi", &mozi_data).unwrap();
    db.set_process_binding("pid-oc", "", "mozi").unwrap();
    db.update_instance_pid("mozi", 4242).unwrap();
    db.store_launch_context("mozi", r#"{"pane_id":"kitty-99"}"#)
        .unwrap();

    let result = bind_session_to_process(&db, "ses-oc-pid", Some("pid-oc"));
    assert_eq!(result, Some("fano".to_string()));

    // Placeholder gone; pid + launch_context now live on the canonical so the
    // restored agent stays killable and its pane closeable.
    assert!(db.get_instance_full("mozi").unwrap().is_none());
    let fano = db.get_instance_full("fano").unwrap().unwrap();
    assert_eq!(fano.pid, Some(4242));
    assert!(
        fano.launch_context
            .as_deref()
            .unwrap_or_default()
            .contains("kitty-99"),
        "launch_context not migrated: {:?}",
        fano.launch_context
    );

    cleanup(path);
}

/// The PTY wrapper that spawned the tool can live in a different PID
/// namespace than the hook doing this migration (a sandboxed hcom sharing
/// the host's DB). Stamping our own namespace here would claim we can see
/// a pid we cannot, and the next reconcile pass would reap a live agent.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
#[serial]
fn test_restore_stopped_carries_placeholder_namespace_not_the_hook_caller() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut fano_data = serde_json::Map::new();
    fano_data.insert("name".into(), serde_json::json!("fano"));
    fano_data.insert("tool".into(), serde_json::json!("opencode"));
    fano_data.insert("created_at".into(), serde_json::json!(now));
    fano_data.insert("status".into(), serde_json::json!("inactive"));
    db.save_instance_named("fano", &fano_data).unwrap();
    db.log_life_event(
        "fano",
        "stopped",
        "test",
        "exit",
        Some(serde_json::json!({ "session_id": "ses-oc-ns", "tool": "opencode" })),
    )
    .unwrap();

    let mut mozi_data = serde_json::Map::new();
    mozi_data.insert("name".into(), serde_json::json!("mozi"));
    mozi_data.insert("tool".into(), serde_json::json!("opencode"));
    mozi_data.insert("created_at".into(), serde_json::json!(now));
    mozi_data.insert("status".into(), serde_json::json!("pending"));
    mozi_data.insert("status_context".into(), serde_json::json!("new"));
    db.save_instance_named("mozi", &mozi_data).unwrap();
    db.set_process_binding("pid-oc-ns", "", "mozi").unwrap();
    db.update_instance_pid("mozi", 4242).unwrap();
    db.conn()
        .execute(
            "UPDATE instances SET pid_namespace = 'pid:[foreign]' WHERE name = 'mozi'",
            [],
        )
        .unwrap();

    assert_eq!(
        bind_session_to_process(&db, "ses-oc-ns", Some("pid-oc-ns")),
        Some("fano".to_string())
    );

    let fano = db.get_instance_full("fano").unwrap().unwrap();
    assert_eq!(fano.pid, Some(4242));
    assert_eq!(fano.pid_namespace.as_deref(), Some("pid:[foreign]"));
    assert_ne!(
        fano.pid_namespace.as_deref(),
        crate::sys::process::current_pid_namespace(),
        "the hook caller's namespace must not be stamped onto a copied pid"
    );

    cleanup(path);
}

#[test]
#[serial]
fn test_restore_stopped_migrates_ready_promoted_placeholder_runtime_state() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut fano_data = serde_json::Map::new();
    fano_data.insert("name".into(), serde_json::json!("fano"));
    fano_data.insert("tool".into(), serde_json::json!("opencode"));
    fano_data.insert("created_at".into(), serde_json::json!(now));
    fano_data.insert("status".into(), serde_json::json!("inactive"));
    db.save_instance_named("fano", &fano_data).unwrap();
    db.log_life_event(
        "fano",
        "stopped",
        "test",
        "exit",
        Some(serde_json::json!({ "session_id": "ses-oc-ready", "tool": "opencode" })),
    )
    .unwrap();

    // PTY ready detection can promote the launch row before opencode-start binds
    // the session. It is still the launch placeholder and must be retired.
    let mut mozi_data = serde_json::Map::new();
    mozi_data.insert("name".into(), serde_json::json!("mozi"));
    mozi_data.insert("tool".into(), serde_json::json!("opencode"));
    mozi_data.insert("created_at".into(), serde_json::json!(now));
    mozi_data.insert("status".into(), serde_json::json!("listening"));
    mozi_data.insert("status_context".into(), serde_json::json!("start"));
    db.save_instance_named("mozi", &mozi_data).unwrap();
    db.set_process_binding("pid-oc-ready", "", "mozi").unwrap();
    db.update_instance_pid("mozi", 4343).unwrap();
    db.store_launch_context("mozi", r#"{"pane_id":"kitty-101"}"#)
        .unwrap();

    let result = bind_session_to_process(&db, "ses-oc-ready", Some("pid-oc-ready"));
    assert_eq!(result, Some("fano".to_string()));

    assert!(db.get_instance_full("mozi").unwrap().is_none());
    let fano = db.get_instance_full("fano").unwrap().unwrap();
    assert_eq!(fano.pid, Some(4343));
    assert!(
        fano.launch_context
            .as_deref()
            .unwrap_or_default()
            .contains("kitty-101"),
        "launch_context not migrated: {:?}",
        fano.launch_context
    );

    cleanup(path);
}

#[test]
fn test_restore_stopped_keeps_active_no_session_row() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut fano_data = serde_json::Map::new();
    fano_data.insert("name".into(), serde_json::json!("fano"));
    fano_data.insert("tool".into(), serde_json::json!("opencode"));
    fano_data.insert("created_at".into(), serde_json::json!(now));
    fano_data.insert("status".into(), serde_json::json!("inactive"));
    db.save_instance_named("fano", &fano_data).unwrap();
    db.log_life_event(
        "fano",
        "stopped",
        "test",
        "exit",
        Some(serde_json::json!({ "session_id": "ses-keep", "tool": "opencode" })),
    )
    .unwrap();

    // An ACTIVE row bound to the pid that happens to lack a session_id — NOT a launch
    // placeholder (status_context != "new", status active). It must not be deleted.
    let mut busy_data = serde_json::Map::new();
    busy_data.insert("name".into(), serde_json::json!("busy"));
    busy_data.insert("tool".into(), serde_json::json!("opencode"));
    busy_data.insert("created_at".into(), serde_json::json!(now));
    busy_data.insert("status".into(), serde_json::json!("active"));
    busy_data.insert("status_context".into(), serde_json::json!("tool:write"));
    db.save_instance_named("busy", &busy_data).unwrap();
    db.set_process_binding("pid-busy", "", "busy").unwrap();

    let result = bind_session_to_process(&db, "ses-keep", Some("pid-busy"));
    assert_eq!(result, Some("fano".to_string()));

    assert!(
        db.get_instance_full("busy").unwrap().is_some(),
        "active non-placeholder row must not be deleted by restore_stopped"
    );

    cleanup(path);
}

fn notify_endpoint_port(db: &HcomDb, instance: &str, kind: &str) -> Option<i64> {
    db.conn()
        .query_row(
            "SELECT port FROM notify_endpoints WHERE instance = ?1 AND kind = ?2",
            rusqlite::params![instance, kind],
            |row| row.get(0),
        )
        .ok()
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
fn test_delete_placeholder_failure_keeps_notify_on_canonical() {
    let (db, path) = setup_test_db();

    db.upsert_notify_endpoint("fano", "plugin", 58_898).unwrap();
    db.upsert_notify_endpoint("mozi", "pty", 55_568).unwrap();
    db.migrate_notify_endpoints("mozi", "fano").unwrap();

    delete_true_placeholder_instance(&db, "ghost_placeholder");

    assert_eq!(notify_endpoint_port(&db, "fano", "plugin"), Some(58_898));
    assert_eq!(notify_endpoint_port(&db, "fano", "pty"), Some(55_568));

    cleanup(path);
}

#[test]
#[serial]
fn test_retire_skips_delete_when_migrate_fails() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let _guard = MigrateNotifyFailGuard::enable();
    let now = now_epoch_i64();

    let mut fano_data = serde_json::Map::new();
    fano_data.insert("name".into(), serde_json::json!("fano"));
    fano_data.insert("tool".into(), serde_json::json!("opencode"));
    fano_data.insert("created_at".into(), serde_json::json!(now));
    fano_data.insert("status".into(), serde_json::json!("inactive"));
    db.save_instance_named("fano", &fano_data).unwrap();

    let snapshot = serde_json::json!({
        "session_id": "ses-opencode-1",
        "tool": "opencode",
    });
    db.log_life_event("fano", "stopped", "test", "exit", Some(snapshot))
        .unwrap();

    let mut mozi_data = serde_json::Map::new();
    mozi_data.insert("name".into(), serde_json::json!("mozi"));
    mozi_data.insert("tool".into(), serde_json::json!("opencode"));
    mozi_data.insert("created_at".into(), serde_json::json!(now));
    mozi_data.insert("status".into(), serde_json::json!("pending"));
    mozi_data.insert("status_context".into(), serde_json::json!("new"));
    db.save_instance_named("mozi", &mozi_data).unwrap();
    db.set_process_binding("pid-oc", "", "mozi").unwrap();

    let result = bind_session_to_process(&db, "ses-opencode-1", Some("pid-oc"));
    assert_eq!(result, Some("fano".to_string()));

    assert!(db.get_instance_full("mozi").unwrap().is_some());
    assert_eq!(
        db.get_session_binding("ses-opencode-1").unwrap(),
        Some("fano".to_string())
    );

    cleanup(path);
}

#[test]
fn test_bind_session_idempotent_same_session() {
    crate::config::Config::init();
    let (db, path) = setup_test_db();
    let now = now_epoch_i64();

    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("luna"));
    data.insert("status".into(), serde_json::json!("pending"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert("created_at".into(), serde_json::json!(now));
    db.save_instance_named("luna", &data).unwrap();
    db.set_process_binding("pid-1", "", "luna").unwrap();

    let r1 = bind_session_to_process(&db, "sess-1", Some("pid-1"));
    assert_eq!(r1, Some("luna".to_string()));

    let r2 = bind_session_to_process(&db, "sess-1", Some("pid-1"));
    assert_eq!(r2, Some("luna".to_string()));

    let inst = db.get_instance_full("luna").unwrap().unwrap();
    assert_eq!(inst.session_id.as_deref(), Some("sess-1"));

    cleanup(path);
}

#[test]
fn test_auto_subscribe_creates_collision_subscription() {
    let (db, path) = setup_test_db();

    use std::collections::HashMap;
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("collision".to_string(), vec!["1".to_string()]);

    let result = crate::db::subscriptions::create_filter_subscription(
        &db,
        &filters,
        &[],
        "test-agent",
        false,
        None,
    );
    assert!(result.is_ok(), "subscription creation should succeed");

    let rows: Vec<String> = db
        .conn()
        .prepare("SELECT key FROM kv WHERE key LIKE 'events_sub:%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(rows.len(), 1, "should have 1 subscription");

    cleanup(path);
}

#[test]
#[serial]
fn test_pending_placeholder_promotion_auto_subscribes_without_created_event() {
    let _env = EnvVarGuard::set("HCOM_AUTO_SUBSCRIBE", "collision");
    let (db, path) = setup_test_db();

    let now = now_epoch_i64();
    let mut data = serde_json::Map::new();
    data.insert("name".into(), serde_json::json!("luna"));
    data.insert("status".into(), serde_json::json!(PLACEHOLDER_STATUS));
    data.insert(
        "status_context".into(),
        serde_json::json!(PLACEHOLDER_CONTEXT),
    );
    data.insert("created_at".into(), serde_json::json!(now));
    data.insert(
        "last_event_id".into(),
        serde_json::json!(db.get_last_event_id()),
    );
    db.save_instance_named("luna", &data).unwrap();

    let ok = initialize_instance_in_position_file(
        &db,
        "luna",
        None,
        None,
        None,
        None,
        None,
        Some("codex"),
        false,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(ok);
    let ok_again = initialize_instance_in_position_file(
        &db,
        "luna",
        None,
        None,
        None,
        None,
        None,
        Some("codex"),
        false,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(ok_again);

    let created_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE instance = 'luna'
               AND type = 'life'
               AND json_extract(data, '$.action') = 'created'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_count, 0);

    let collision_sub_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM kv
             WHERE key LIKE 'events_sub:%'
               AND json_extract(value, '$.caller') = 'luna'
               AND json_extract(value, '$.filters.collision[0]') IS NOT NULL
               AND COALESCE(json_extract(value, '$.delivery_only'), 0) != 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(collision_sub_count, 1);

    cleanup(path);
}

#[test]
#[serial]
fn new_row_honors_configured_hcom_timeout() {
    // Regression test for issue #71: a brand-new instance row (the path
    // used by vanilla `hcom start`, launched, and resumed sessions) must
    // carry the effective HCOM_TIMEOUT rather than silently falling back
    // to the old always-86400 schema default.
    let _env = EnvVarGuard::set("HCOM_TIMEOUT", "30");
    let (db, path) = setup_test_db();

    let ok = initialize_instance_in_position_file(
        &db,
        "luna",
        None,
        None,
        None,
        None,
        None,
        Some("claude"),
        false,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(ok);

    let row = db.get_instance_full("luna").unwrap().unwrap();
    assert_eq!(row.wait_timeout, Some(30));

    cleanup(path);
}

#[test]
#[serial]
fn new_row_falls_back_to_120_without_config() {
    let _env = EnvVarGuard::unset("HCOM_TIMEOUT");
    let (db, path) = setup_test_db();

    let ok = initialize_instance_in_position_file(
        &db,
        "luna",
        None,
        None,
        None,
        None,
        None,
        Some("claude"),
        false,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(ok);

    let row = db.get_instance_full("luna").unwrap().unwrap();
    // Default HcomConfig::timeout is 86400 (schema-equivalent default),
    // preserved for anyone who hasn't set HCOM_TIMEOUT.
    assert_eq!(row.wait_timeout, Some(86400));

    cleanup(path);
}
