use super::*;
use crate::hooks::test_helpers::EnvGuard;
use serial_test::serial;

fn cursor_test_env() -> (tempfile::TempDir, PathBuf, EnvGuard) {
    let guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", workspace.join(".hcom"));
        std::env::remove_var("CURSOR_CONFIG_DIR");
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    (dir, workspace, guard)
}

/// Seeds a Cursor instance row for tests. Caller contract: call after
/// `cursor_test_env()` and after `crate::config::Config::init()`; the caller is
/// also responsible for setting `HCOM_PROCESS_ID` (via `EnvVarGuard`) to match
/// `process_id`.
fn seed_cursor_row(name: &str, session_id: &str, process_id: &str) {
    let db = HcomDb::open().unwrap();
    let initialized = crate::instance_binding::initialize_instance_in_position_file(
        &db,
        name,
        Some(session_id),
        None,
        None,
        None,
        None,
        Some("cursor"),
        false,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(
        initialized,
        "failed to initialize instance in position file"
    );
    db.rebind_session(session_id, name).unwrap();
    db.set_process_binding(process_id, session_id, name)
        .unwrap();
    lifecycle::set_status(&db, name, ST_ACTIVE, "prompt", Default::default());
}

fn insert_broadcast(db: &HcomDb, from: &str, text: &str) {
    db.log_event(
        "message",
        from,
        &json!({"from": from, "text": text, "scope": "broadcast"}),
    )
    .unwrap();
}

#[test]
#[serial]
fn stop_followup_without_completed_status() {
    let (_dir, _workspace, _guard) = cursor_test_env();
    crate::config::Config::init();
    let _process_id = crate::instance_binding::EnvVarGuard::set("HCOM_PROCESS_ID", "proc-kali");
    seed_cursor_row("kali", "sess-k", "proc-kali");
    let db = HcomDb::open().unwrap();
    insert_broadcast(&db, "ops", "task for kali");
    let ctx = HcomContext::from_os();
    let payload = HookPayload::from_cursor_native(
        "cursor-stop",
        json!({"session_id": "sess-k", "conversation_id": "sess-k"}),
    );
    let (out, ack) = handle_stop(&db, &ctx, &payload);
    assert!(
        out.get("followup_message")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("task for kali")),
        "{out}"
    );
    assert!(ack.is_some());
}

#[test]
#[serial]
fn stop_empty_queue_has_no_followup() {
    let (_dir, _workspace, _guard) = cursor_test_env();
    crate::config::Config::init();
    let _process_id = crate::instance_binding::EnvVarGuard::set("HCOM_PROCESS_ID", "proc-idle");
    seed_cursor_row("idle", "sess-i", "proc-idle");
    let db = HcomDb::open().unwrap();
    let ctx = HcomContext::from_os();
    let payload = HookPayload::from_cursor_native(
        "cursor-stop",
        json!({"session_id": "sess-i", "status": "completed"}),
    );
    let (out, ack) = handle_stop(&db, &ctx, &payload);
    assert_eq!(out, json!({}));
    assert!(ack.is_none());
}

#[test]
#[serial]
fn sessionend_completed_keeps_instance() {
    let (_dir, _workspace, _guard) = cursor_test_env();
    crate::config::Config::init();
    let _process_id = crate::instance_binding::EnvVarGuard::set("HCOM_PROCESS_ID", "proc-zilo");
    seed_cursor_row("zilo", "sess-a", "proc-zilo");
    let db = HcomDb::open().unwrap();
    let ctx = HcomContext::from_os();
    let raw = json!({
        "session_id": "sess-a",
        "conversation_id": "sess-a",
        "reason": "completed"
    });
    let payload = HookPayload::from_cursor_native("cursor-sessionend", raw);
    let out = handle_sessionend(&db, &ctx, &payload);
    assert_eq!(out, json!({}));
    let row = db.get_instance_full("zilo").unwrap().expect("row deleted");
    assert_ne!(row.status, crate::shared::ST_INACTIVE);
    assert!(!row.status_context.starts_with("exit:"));
}

#[test]
#[serial]
fn sessionstart_second_uuid_keeps_first_session_binding() {
    let (_dir, _workspace, _guard) = cursor_test_env();
    crate::config::Config::init();
    let _process_id = crate::instance_binding::EnvVarGuard::set("HCOM_PROCESS_ID", "proc-dual");
    seed_cursor_row("dual", "uuid-a", "proc-dual");
    let db = HcomDb::open().unwrap();
    let ctx = HcomContext::from_os();
    let payload = HookPayload::from_cursor_native(
        "cursor-sessionstart",
        json!({"session_id": "uuid-b", "conversation_id": "uuid-b"}),
    );
    let _ = handle_sessionstart(&db, &ctx, &payload);
    assert_eq!(
        db.get_session_binding("uuid-a").unwrap(),
        Some("dual".to_string())
    );
    assert_eq!(
        db.get_session_binding("uuid-b").unwrap(),
        Some("dual".to_string())
    );
    assert!(db.get_instance_full("dual").unwrap().is_some());
}

#[test]
#[serial]
fn setup_is_idempotent_and_preserves_existing_hooks() {
    let (_dir, workspace, _guard) = cursor_test_env();
    let hooks_path = workspace.join(".cursor/hooks.json");
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "hooks": {
                "sessionStart": [{ "command": "./custom-start.sh" }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    try_setup_cursor_hooks(false).unwrap();
    let first = std::fs::read_to_string(&hooks_path).unwrap();
    try_setup_cursor_hooks(false).unwrap();
    let second = std::fs::read_to_string(&hooks_path).unwrap();

    assert_eq!(first, second);
    assert!(verify_cursor_hooks_installed(false));
    let root: Value = serde_json::from_str(&second).unwrap();
    assert!(
        root["hooks"]["sessionStart"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hook| hook["command"] == "./custom-start.sh")
    );
    assert!(!workspace.join(".cursor/cli.json").exists());
}

#[test]
#[serial]
fn permissions_are_project_local_and_cleanup_preserves_other_rules() {
    let (_dir, workspace, _guard) = cursor_test_env();
    let permissions_path = workspace.join(".cursor/cli.json");
    std::fs::create_dir_all(permissions_path.parent().unwrap()).unwrap();
    std::fs::write(
        &permissions_path,
        serde_json::to_string_pretty(&json!({
            "permissions": {
                "allow": ["Shell(custom)"]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    try_setup_cursor_hooks(true).unwrap();
    assert!(verify_cursor_hooks_installed(true));
    try_setup_cursor_hooks(false).unwrap();

    let root: Value =
        serde_json::from_str(&std::fs::read_to_string(permissions_path).unwrap()).unwrap();
    assert_eq!(root["permissions"]["allow"], json!(["Shell(custom)"]));
    assert!(root.get("version").is_none());
    assert!(root.get("editor").is_none());
}

#[test]
#[serial]
fn setup_replaces_legacy_prefixes_with_scoped_permissions() {
    let (_dir, workspace, _guard) = cursor_test_env();
    let permissions_path = workspace.join(".cursor/cli.json");
    std::fs::create_dir_all(permissions_path.parent().unwrap()).unwrap();
    std::fs::write(
        &permissions_path,
        serde_json::to_string_pretty(&json!({
            "permissions": {
                "allow": [
                    "Shell(custom)",
                    "Shell(hcom)",
                    "Shell(uvx hcom)",
                    "Shell(uvx hcom send)"
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    try_setup_cursor_hooks(true).unwrap();

    let root: Value =
        serde_json::from_str(&std::fs::read_to_string(permissions_path).unwrap()).unwrap();
    let allow = root["permissions"]["allow"].as_array().unwrap();
    assert!(allow.iter().any(|rule| rule == "Shell(custom)"));
    assert!(!allow.iter().any(|rule| rule == "Shell(hcom)"));
    assert!(!allow.iter().any(|rule| rule == "Shell(uvx hcom)"));
    for rule in cursor_permission_rules() {
        assert!(allow.iter().any(|entry| entry == &rule), "missing {rule}");
    }
    assert!(!allow.iter().any(|rule| rule == "Shell(hcom kill)"));
    assert!(!allow.iter().any(|rule| rule == "Shell(hcom reset)"));
}

#[test]
#[serial]
fn setup_removes_stale_hook_prefixes() {
    let (_dir, workspace, _guard) = cursor_test_env();
    let hooks_path = workspace.join(".cursor/hooks.json");
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&json!({
            "hooks": {
                "stop": [
                    { "command": "hcom cursor-stop" },
                    { "command": "uvx hcom cursor-stop" },
                    { "command": "./custom-stop.sh" }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    try_setup_cursor_hooks(false).unwrap();

    let root: Value = serde_json::from_str(&std::fs::read_to_string(hooks_path).unwrap()).unwrap();
    let stop = root["hooks"]["stop"].as_array().unwrap();
    assert_eq!(
        stop.iter()
            .filter(|hook| hook["command"] == build_cursor_hook_command("cursor-stop"))
            .count(),
        1
    );
    assert!(
        stop.iter()
            .any(|hook| hook["command"] == "./custom-stop.sh")
    );
    assert_eq!(
        stop.iter()
            .filter(|hook| hook["command"].as_str().is_some_and(is_hcom_cursor_command))
            .count(),
        1
    );
}

// Unix-only: relies on redirecting the home dir via $HOME, but on Windows
// `dirs::home_dir()` reads USERPROFILE and ignores the test's temp HOME, so
// the normal-vs-isolated mode check never sees the override.
#[cfg(unix)]
#[test]
#[serial]
fn normal_mode_permissions_honor_cursor_config_dir() {
    let (dir, _workspace, _guard) = cursor_test_env();
    let home = dir.path().join("home");
    let override_dir = dir.path().join("cursor-override");
    unsafe {
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::set_var("CURSOR_CONFIG_DIR", &override_dir);
    }

    assert_eq!(
        get_cursor_permissions_path(),
        override_dir.join("cli-config.json")
    );
}

#[test]
#[serial]
fn isolated_mode_permissions_ignore_global_override() {
    let (dir, workspace, _guard) = cursor_test_env();
    unsafe {
        std::env::set_var("CURSOR_CONFIG_DIR", dir.path().join("cursor-override"));
    }

    assert_eq!(
        get_cursor_permissions_path(),
        workspace.join(".cursor/cli.json")
    );
}

#[test]
#[serial]
fn normal_mode_permissions_honor_xdg_on_supported_platforms() {
    if !cfg!(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )) {
        return;
    }
    let (dir, _workspace, _guard) = cursor_test_env();
    let home = dir.path().join("home");
    let xdg = dir.path().join("xdg");
    unsafe {
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }

    assert_eq!(
        get_cursor_permissions_path(),
        xdg.join("cursor/cli-config.json")
    );
}

#[test]
#[serial]
fn remove_preserves_unrelated_hooks() {
    let (_dir, workspace, _guard) = cursor_test_env();
    let hooks_path = workspace.join(".cursor/hooks.json");
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "hooks": {
                "sessionEnd": [{ "command": "./custom-end.sh" }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    try_setup_cursor_hooks(false).unwrap();
    assert!(remove_cursor_hooks());

    let root: Value = serde_json::from_str(&std::fs::read_to_string(hooks_path).unwrap()).unwrap();
    assert_eq!(
        root["hooks"]["sessionEnd"],
        json!([{ "command": "./custom-end.sh" }])
    );
    assert!(
        root["hooks"]
            .as_object()
            .unwrap()
            .get("sessionStart")
            .is_none()
    );
}

// Unix-only: same $HOME-redirection limitation as
// `normal_mode_permissions_honor_cursor_config_dir`.
#[cfg(unix)]
#[test]
#[serial]
fn remove_cleans_default_and_isolated_paths() {
    let (dir, workspace, _guard) = cursor_test_env();
    let home = dir.path().join("home");
    let hooks_paths = [
        home.join(".cursor/hooks.json"),
        workspace.join(".cursor/hooks.json"),
    ];
    let permissions_paths = [
        home.join(".cursor/cli-config.json"),
        workspace.join(".cursor/cli.json"),
    ];
    for path in &hooks_paths {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "stop": [
                        { "command": "hcom cursor-stop" },
                        { "command": "./custom-stop.sh" }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }
    for path in &permissions_paths {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::to_string_pretty(&json!({
                "permissions": {
                    "allow": ["Shell(hcom)", "Shell(custom)"]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    assert!(remove_cursor_hooks());

    for path in hooks_paths {
        let root: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            root["hooks"]["stop"],
            json!([{ "command": "./custom-stop.sh" }])
        );
    }
    for path in permissions_paths {
        let root: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(root["permissions"]["allow"], json!(["Shell(custom)"]));
    }
}

#[test]
#[serial]
fn verify_rejects_fifteen_second_stop_timeout() {
    let (_dir, workspace, _guard) = cursor_test_env();
    let hooks_path = workspace.join(".cursor/hooks.json");
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    try_setup_cursor_hooks(false).unwrap();
    let mut root: Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks_path).unwrap()).unwrap();
    for entry in root["hooks"]["stop"].as_array_mut().unwrap() {
        if entry["command"] == build_cursor_hook_command("cursor-stop") {
            entry["timeout"] = json!(15);
        }
    }
    std::fs::write(&hooks_path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert!(!verify_cursor_hooks_installed(false));
}

#[test]
#[serial]
fn setup_writes_stop_timeout_thirty() {
    let (_dir, workspace, _guard) = cursor_test_env();
    try_setup_cursor_hooks(false).unwrap();
    let root: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.join(".cursor/hooks.json")).unwrap(),
    )
    .unwrap();
    let stop = root["hooks"]["stop"].as_array().unwrap();
    let hcom = stop
        .iter()
        .find(|h| h["command"] == build_cursor_hook_command("cursor-stop"))
        .unwrap();
    assert_eq!(hcom["timeout"], json!(30));
    assert!(hcom["loop_limit"].is_null());
    for event in ["sessionStart", "sessionEnd", "preToolUse", "postToolUse"] {
        let entries = root["hooks"][event].as_array().unwrap();
        let hcom = entries
            .iter()
            .find(|h| h["command"].as_str().is_some_and(|c| c.contains("cursor-")))
            .unwrap();
        assert_eq!(hcom["timeout"], json!(15), "{event}");
    }
    assert!(verify_cursor_hooks_installed(false));
}
