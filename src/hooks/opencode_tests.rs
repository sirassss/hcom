use super::*;
use crate::hooks::test_helpers::EnvGuard;
use serial_test::serial;

fn sv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// Create a fresh test DB in a temp directory with schema initialized.
fn test_db() -> (tempfile::TempDir, HcomDb) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    (dir, db)
}

// ── Argv parsing ──

#[test]
fn test_parse_flag_found() {
    let argv = sv(&["--session-id", "abc", "--notify-port", "12345"]);
    assert_eq!(parse_flag(&argv, "--session-id"), Some("abc".to_string()));
    assert_eq!(
        parse_flag(&argv, "--notify-port"),
        Some("12345".to_string())
    );
}

#[test]
fn test_parse_flag_not_found() {
    let argv = sv(&["--session-id", "abc"]);
    assert_eq!(parse_flag(&argv, "--name"), None);
}

#[test]
fn test_parse_flag_at_end() {
    // Flag at end with no value
    let argv = sv(&["--session-id"]);
    assert_eq!(parse_flag(&argv, "--session-id"), None);
}

#[test]
fn test_has_flag() {
    let argv = sv(&["--name", "foo", "--format", "--check"]);
    assert!(has_flag(&argv, "--format"));
    assert!(has_flag(&argv, "--check"));
    assert!(!has_flag(&argv, "--ack"));
}

#[test]
fn test_parse_value_arg_supports_split_and_equals_forms() {
    let split = sv(&["--agent", "reviewer", "-m", "openai/gpt-5.4"]);
    assert_eq!(
        parse_value_arg(&split, &["--agent"]),
        Some("reviewer".to_string())
    );
    assert_eq!(
        parse_value_arg(&split, &["--model", "-m"]),
        Some("openai/gpt-5.4".to_string())
    );

    let equals = sv(&["--agent=planner", "--model=anthropic/claude-sonnet-4-6"]);
    assert_eq!(
        parse_value_arg(&equals, &["--agent"]),
        Some("planner".to_string())
    );
    assert_eq!(
        parse_value_arg(&equals, &["--model", "-m"]),
        Some("anthropic/claude-sonnet-4-6".to_string())
    );
}

#[test]
fn test_parse_launch_model_validates_provider_and_model() {
    assert_eq!(
        parse_launch_model("openai/gpt-5.4"),
        Some(serde_json::json!({
            "providerID": "openai",
            "modelID": "gpt-5.4",
        }))
    );
    assert_eq!(parse_launch_model("openai"), None);
    assert_eq!(parse_launch_model("/gpt-5.4"), None);
    assert_eq!(parse_launch_model("openai/"), None);
}

#[test]
fn test_launch_agent_and_model_from_args_parses_stored_launch_args() {
    let launch_args =
        serde_json::to_string(&sv(&["--agent=planner", "--model", "openai/gpt-5.4"])).unwrap();

    let (agent, model) = launch_agent_and_model_from_args(Some(&launch_args));
    assert_eq!(agent.as_deref(), Some("planner"));
    assert_eq!(
        model,
        Some(serde_json::json!({
            "providerID": "openai",
            "modelID": "gpt-5.4",
        }))
    );
}

#[test]
fn test_launch_agent_and_model_from_args_supports_short_model_flag() {
    let launch_args = serde_json::to_string(&sv(&[
        "--agent",
        "reviewer",
        "-m",
        "anthropic/claude-opus-4",
    ]))
    .unwrap();

    let (agent, model) = launch_agent_and_model_from_args(Some(&launch_args));
    assert_eq!(agent.as_deref(), Some("reviewer"));
    assert_eq!(
        model,
        Some(serde_json::json!({
            "providerID": "anthropic",
            "modelID": "claude-opus-4",
        }))
    );
}

#[test]
fn test_launch_agent_and_model_reads_instance_launch_args() {
    let (_dir, db) = test_db();
    let launch_args =
        serde_json::to_string(&sv(&["--agent", "reviewer", "--model", "openai/gpt-5.4"])).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, status, created_at, launch_args)
             VALUES (?1, 'opencode', 'active', 0, ?2)",
            rusqlite::params!["luna", launch_args],
        )
        .unwrap();

    let (agent, model) = launch_agent_and_model(&db, "luna");
    assert_eq!(agent.as_deref(), Some("reviewer"));
    assert_eq!(
        model,
        Some(serde_json::json!({
            "providerID": "openai",
            "modelID": "gpt-5.4",
        }))
    );
}

// ── Plugin management ──

#[test]
fn test_plugin_source_contains_entrypoint() {
    assert!(PLUGIN_SOURCE.contains("HcomPlugin"));
}

#[test]
#[serial]
fn test_get_opencode_plugin_dir_defaults_to_xdg_global_path() {
    let dir = tempfile::tempdir().unwrap();
    let saved_home = std::env::var("HOME").ok();
    let saved_hcom = std::env::var("HCOM_DIR").ok();
    let saved_xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = dir.path().join("home");
    let xdg = dir.path().join("xdg");
    std::fs::create_dir_all(home.join(".hcom")).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }

    assert_eq!(
        get_opencode_plugin_dir(),
        xdg.join("opencode").join("plugins")
    );

    if let Some(home) = saved_home {
        unsafe { std::env::set_var("HOME", home) };
    } else {
        unsafe { std::env::remove_var("HOME") };
    }
    if let Some(hcom) = saved_hcom {
        unsafe { std::env::set_var("HCOM_DIR", hcom) };
    } else {
        unsafe { std::env::remove_var("HCOM_DIR") };
    }
    if let Some(xdg) = saved_xdg {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", xdg) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
}

#[test]
fn test_get_opencode_plugin_path() {
    let path = get_opencode_plugin_path();
    assert!(path.ends_with("hcom.ts"));
}

#[test]
#[serial]
fn test_project_local_kilo_plugin_path_uses_kilo_dir() {
    let _guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let hcom_dir = workspace.join(".hcom");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&hcom_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HCOM_DIR", &hcom_dir);
        std::env::set_var("HOME", &home);
    }

    assert_eq!(
        get_kilo_plugin_path(),
        workspace.join(".kilo").join("plugins").join("hcom.ts")
    );
}

#[test]
fn test_plugin_filename_constant() {
    assert_eq!(PLUGIN_FILENAME, "hcom.ts");
}

#[test]
#[serial]
fn test_verify_plugin_installed_rejects_stale_canonical_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let saved_home = std::env::var("HOME").ok();
    let saved_hcom = std::env::var("HCOM_DIR").ok();
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("HCOM_DIR", dir.path().join(".hcom"));
    }

    let plugin_path = get_opencode_plugin_path();
    std::fs::create_dir_all(plugin_path.parent().unwrap()).unwrap();
    std::fs::write(&plugin_path, "// stale plugin").unwrap();

    assert!(!verify_opencode_plugin_installed());

    if let Some(home) = saved_home {
        unsafe { std::env::set_var("HOME", home) };
    } else {
        unsafe { std::env::remove_var("HOME") };
    }
    if let Some(hcom) = saved_hcom {
        unsafe { std::env::set_var("HCOM_DIR", hcom) };
    } else {
        unsafe { std::env::remove_var("HCOM_DIR") };
    }
}

#[test]
#[serial]
fn test_project_local_plugin_path_uses_hcom_dir_parent() {
    let _guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let hcom_dir = workspace.join(".hcom");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&hcom_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HCOM_DIR", &hcom_dir);
        std::env::set_var("HOME", &home);
    }

    assert_eq!(
        get_opencode_plugin_path(),
        workspace.join(".opencode").join("plugins").join("hcom.ts")
    );
}

#[test]
#[serial]
fn test_verify_and_remove_support_opencode_config_dir() {
    let _guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let xdg = dir.path().join("xdg");
    let custom = dir.path().join("custom-opencode");
    std::fs::create_dir_all(home.join(".hcom")).unwrap();
    std::fs::create_dir_all(custom.join("plugins")).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
        std::env::set_var("OPENCODE_CONFIG_DIR", &custom);
    }

    let plugin_path = custom.join("plugins").join("hcom.ts");
    std::fs::write(&plugin_path, PLUGIN_SOURCE).unwrap();

    assert!(verify_opencode_plugin_installed());
    remove_opencode_plugin().unwrap();
    assert!(!plugin_path.exists());
}

#[test]
#[serial]
fn test_verify_and_remove_support_kilo_config_dir() {
    let _guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let xdg = dir.path().join("xdg");
    let custom = dir.path().join("custom-kilo");
    std::fs::create_dir_all(home.join(".hcom")).unwrap();
    std::fs::create_dir_all(custom.join("plugins")).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
        std::env::set_var("KILO_CONFIG_DIR", &custom);
    }

    let plugin_path = custom.join("plugins").join("hcom.ts");
    std::fs::write(&plugin_path, PLUGIN_SOURCE).unwrap();

    assert!(verify_kilo_plugin_installed());
    remove_kilo_plugin().unwrap();
    assert!(!plugin_path.exists());
}

// ── Transcript path ──

#[test]
fn test_get_opencode_db_path_missing() {
    // In test env, opencode db won't exist
    // This tests the "not found" path
    let result = get_opencode_db_path();
    // Could be Some or None depending on environment, just verify it doesn't panic
    let _ = result;
}

// ── Handler tests (unit-level, isolated DB) ──

#[test]
fn test_handle_start_missing_session_id() {
    crate::config::Config::init();
    let ctx = HcomContext::from_os();
    let (_dir, db) = test_db();
    let argv = sv(&[]);
    let (code, output) = handle_start(&ctx, &db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("Missing --session-id"));
}

#[test]
#[serial]
fn test_handle_start_rebind_includes_launch_identity() {
    crate::config::Config::init();
    let (_env_dir, hcom_dir, test_home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let (_db_dir, db) = test_db();
    let launch_args =
        serde_json::to_string(&sv(&["--agent", "reviewer", "--model", "openai/gpt-5.4"])).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, status, created_at, launch_args)
             VALUES (?1, 'opencode', 'active', 0, ?2)",
            rusqlite::params!["luna", launch_args],
        )
        .unwrap();
    db.set_session_binding("sess-1", "luna").unwrap();

    let env = std::collections::HashMap::from([
        (
            "HCOM_DIR".to_string(),
            hcom_dir.to_string_lossy().to_string(),
        ),
        ("HOME".to_string(), test_home.to_string_lossy().to_string()),
        ("HCOM_PROCESS_ID".to_string(), "pid-123".to_string()),
    ]);
    let ctx = HcomContext::from_env(&env, std::path::PathBuf::from("/tmp"));

    let (code, output) = handle_start(&ctx, &db, &sv(&["--session-id", "sess-1"]));
    assert_eq!(code, 0);

    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["name"], "luna");
    assert_eq!(payload["session_id"], "sess-1");
    assert_eq!(payload["agent"], "reviewer");
    assert_eq!(
        payload["model"],
        serde_json::json!({
            "providerID": "openai",
            "modelID": "gpt-5.4",
        })
    );
    assert!(payload["bootstrap"].as_str().is_some());
}

#[test]
fn test_handle_status_missing_name() {
    let (_dir, db) = test_db();
    let argv = sv(&["--status", "listening"]);
    let (code, output) = handle_status(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("Missing --name or --status"));
}

#[test]
fn test_handle_status_missing_status() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "test"]);
    let (code, output) = handle_status(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("Missing --name or --status"));
}

#[test]
fn test_handle_read_missing_name() {
    let (_dir, db) = test_db();
    let argv = sv(&[]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("Missing --name"));
}

#[test]
fn test_handle_read_check_empty() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "testinst", "--check"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert_eq!(output, "false");
}

#[test]
fn test_handle_read_default_empty() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "testinst"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert_eq!(output, "[]");
}

#[test]
fn test_handle_read_format_empty() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "testinst", "--format"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert_eq!(output, "");
}

#[test]
fn test_handle_read_ack_empty() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "testinst", "--ack"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("\"acked\":0") || output.contains("\"acked\": 0"));
}

#[test]
fn test_handle_read_ack_up_to() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "testinst", "--ack", "--up-to", "42"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("42"));
}

#[test]
fn test_handle_read_ack_invalid_up_to() {
    let (_dir, db) = test_db();
    let argv = sv(&["--name", "testinst", "--ack", "--up-to", "abc"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("Invalid --up-to"));
}

#[test]
fn test_handle_stop_missing_name() {
    let (_dir, db) = test_db();
    let argv = sv(&[]);
    let (code, output) = handle_stop(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("Missing --name"));
}

#[test]
fn test_handle_stop_nonexistent() {
    crate::config::Config::init();
    let (_dir, db) = test_db();
    // finalize_session on nonexistent instance should be no-op
    let argv = sv(&["--name", "testinst", "--reason", "test"]);
    let (code, output) = handle_stop(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("\"ok\":true") || output.contains("\"ok\": true"));
}

#[test]
fn test_handle_status_updates_status() {
    crate::config::Config::init();
    let (_dir, db) = test_db();
    // Create an instance first
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, status, status_context, status_time, created_at) VALUES ('testinst', 'opencode', 'active', 'new', 0, 0)",
        [],
    );
    let argv = sv(&[
        "--name",
        "testinst",
        "--status",
        "listening",
        "--context",
        "idle",
    ]);
    let (code, output) = handle_status(&db, &argv);
    assert_eq!(code, 0);
    assert!(output.contains("\"ok\":true") || output.contains("\"ok\": true"));
}

#[test]
fn test_handle_read_with_messages() {
    let (_dir, db) = test_db();
    // Create instance with last_event_id = 0
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, last_event_id) VALUES ('testinst', 'opencode', 'listening', 'start', 0, 0, 0)",
        [],
    );
    // Insert a message event
    let _ = db.conn().execute(
        "INSERT INTO events (type, timestamp, instance, data) VALUES ('message', '2026-01-01T00:00:00Z', 'luna', '{\"from\":\"luna\",\"text\":\"hello\",\"scope\":\"broadcast\"}')",
        [],
    );
    // Default mode: raw JSON array
    let argv = sv(&["--name", "testinst"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["from"], "luna");

    // Check mode
    let argv = sv(&["--name", "testinst", "--check"]);
    let (code, output) = handle_read(&db, &argv);
    assert_eq!(code, 0);
    assert_eq!(output, "true");
}

#[test]
fn test_handle_read_format_does_not_advance_cursor() {
    let (_dir, db) = test_db();
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, last_event_id) VALUES ('testinst', 'opencode', 'listening', 'start', 0, 0, 0)",
        [],
    );
    let _ = db.conn().execute(
        "INSERT INTO events (type, timestamp, instance, data) VALUES ('message', '2026-01-01T00:00:00Z', 'luna', '{\"from\":\"luna\",\"text\":\"hello\",\"scope\":\"broadcast\"}')",
        [],
    );

    let before: i64 = db
        .conn()
        .query_row(
            "SELECT last_event_id FROM instances WHERE name = 'testinst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, 0);

    let (code, output) = handle_read(&db, &sv(&["--name", "testinst", "--format"]));
    assert_eq!(code, 0);
    assert!(output.contains("hello"));

    let after: i64 = db
        .conn()
        .query_row(
            "SELECT last_event_id FROM instances WHERE name = 'testinst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, 0);
}

// ── Dispatcher (unit tests for routing logic, not full integration) ──

#[test]
fn test_dispatch_routes_correctly() {
    // Verify the match arms exist and route correctly via direct handler calls.
    // Full dispatch_opencode_hook() requires runtime (Config, DB) — tested via parity tests.
    let (_dir, db) = test_db();
    // Missing name → error JSON
    let (code, output) = handle_stop(&db, &sv(&[]));
    assert_eq!(code, 0);
    assert!(output.contains("Missing --name"));
}
