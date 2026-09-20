use super::*;
use clap::Parser;
use rusqlite::params;
use serde_json::json;
use serial_test::serial;
use std::collections::HashMap;
use std::path::PathBuf;

fn make_ctx(tool_env: &[(&str, &str)], cwd: &str) -> HcomContext {
    let mut env = HashMap::new();
    for (k, v) in tool_env {
        env.insert((*k).to_string(), (*v).to_string());
    }
    HcomContext::from_env(&env, PathBuf::from(cwd))
}

/// Claude context carrying exactly one session-id source, so an ambient
/// value from the shell running the tests cannot decide the outcome.
fn make_claude_ctx(session: Option<(&str, &str)>, cwd: &str) -> HcomContext {
    let mut env = HashMap::new();
    env.insert("CLAUDECODE".to_string(), "1".to_string());
    if let Some((key, value)) = session {
        env.insert(key.to_string(), value.to_string());
    }
    HcomContext::from_env(&env, PathBuf::from(cwd))
}

fn log_stopped_snapshot(
    db: &HcomDb,
    name: &str,
    tool: &str,
    directory: &str,
    session_id: &str,
    last_event_id: i64,
) {
    db.log_event(
        "life",
        name,
        &json!({
            "action": "stopped",
            "snapshot": {
                "tool": tool,
                "directory": directory,
                "session_id": session_id,
                "last_event_id": last_event_id
            }
        }),
    )
    .unwrap();
}
#[test]
fn test_start_args_bare() {
    let args = StartArgs::try_parse_from(["start"]).unwrap();
    assert!(args.orphan.is_none());
    assert!(args.as_name.is_none());
}

#[test]
fn test_start_args_orphan() {
    let args = StartArgs::try_parse_from(["start", "--orphan", "1234"]).unwrap();
    assert_eq!(args.orphan, Some("1234".to_string()));
    assert!(args.as_name.is_none());
}

#[test]
fn test_start_args_rebind() {
    let args = StartArgs::try_parse_from(["start", "--as", "luna"]).unwrap();
    assert!(args.orphan.is_none());
    assert_eq!(args.as_name, Some("luna".to_string()));
}

#[test]
fn test_start_args_bare_as_errors() {
    let err = StartArgs::try_parse_from(["start", "--as"]);
    assert!(err.is_err());
}

#[test]
fn test_start_args_bare_orphan_errors() {
    let err = StartArgs::try_parse_from(["start", "--orphan"]);
    assert!(err.is_err());
}

#[test]
fn test_start_args_unknown_flag_errors() {
    let err = StartArgs::try_parse_from(["start", "--bogus"]);
    assert!(err.is_err());
}

#[test]
#[serial]
fn test_start_rejects_remote_instances() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, origin_device_id, created_at) VALUES (?1, ?2, ?3)",
            params![
                "luna:ABCD",
                "remote-device",
                crate::shared::time::now_epoch_f64()
            ],
        )
        .unwrap();

    let flags = crate::router::GlobalFlags {
        name: Some("luna:ABCD".to_string()),
        go: false,
    };
    let err = run(&["start".to_string()], &flags).unwrap_err();
    assert!(
        err.to_string().contains("Remote start is not supported"),
        "unexpected error: {err}"
    );
}

/// `hcom start` asks to join the bus, not to change the machine. For tools
/// whose hooks ship as a plugin, installing means shelling out to that
/// tool's CLI and cloning a marketplace over the network — so this path
/// must report and stop, never install. Without the guard this test hung
/// for two minutes on a real clone attempt.
#[test]
#[serial]
fn test_vanilla_start_never_installs_a_plugin_tool() {
    let (_dir, hcom_dir, home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    let ctx = make_claude_ctx(
        Some(("CLAUDE_CODE_SESSION_ID", "native-session")),
        "/tmp/project",
    );

    let before = std::fs::read_to_string(home.join(".claude/settings.json")).ok();
    assert_eq!(
        start_bare(&db, &hcom_dir, &ctx, None).unwrap(),
        1,
        "a missing plugin must stop bare start, not proceed"
    );
    let after = std::fs::read_to_string(home.join(".claude/settings.json")).ok();
    assert_eq!(before, after, "bare start must not write hook config");
    assert!(
        !home.join(".claude/plugins").exists(),
        "bare start must not install a plugin"
    );
}

#[test]
fn test_resolve_claude_session_id() {
    let env = |value: Option<&str>| -> HashMap<String, String> {
        value
            .map(|value| HashMap::from([("CLAUDE_CODE_SESSION_ID".to_string(), value.to_string())]))
            .unwrap_or_default()
    };

    assert_eq!(
        resolve_claude_session_id(&env(Some("claude-sess"))),
        Some("claude-sess".to_string())
    );
    assert_eq!(resolve_claude_session_id(&env(Some(""))), None);
    assert_eq!(resolve_claude_session_id(&env(None)), None);
}

#[test]
fn codex_native_identity_prefers_session_and_supports_older_builds() {
    for (pairs, expected) in [
        (vec![("CODEX_THREAD_ID", "thread")], "thread"),
        (
            vec![("CODEX_THREAD_ID", "thread"), ("CODEX_SESSION_ID", "root")],
            "root",
        ),
    ] {
        let env = pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
        assert_eq!(resolve_vanilla_session_id(&ctx).as_deref(), Some(expected));
    }
}

#[test]
#[serial]
fn manual_tools_without_native_identity_start_as_adhoc() {
    let (_dir, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    for tool in ["gemini", "antigravity", "claude", "codex", "pi"] {
        let env = HashMap::from([("HCOM_TOOL".to_string(), tool.to_string())]);
        let mut ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
        ctx.tool = tool.parse().unwrap();
        assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    }
    let rows = db.iter_instances_full().unwrap();
    assert_eq!(rows.len(), 5);
    assert!(
        rows.iter()
            .all(|row| row.tool == "adhoc" && row.session_id.is_none())
    );
}

#[test]
#[serial]
fn test_vanilla_claude_start_reuses_claude_code_session_id() {
    let (_dir, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    crate::hooks::test_helpers::install_fake_claude_plugin(&_home);

    // Claude's own session id is sufficient; no SessionStart env-file
    // round trip is required.
    let ctx = make_claude_ctx(
        Some(("CLAUDE_CODE_SESSION_ID", "sess-claude-env")),
        "/tmp/project",
    );

    assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    let name = db
        .get_session_binding("sess-claude-env")
        .unwrap()
        .expect("CLAUDE_CODE_SESSION_ID must bind identity");
    assert_eq!(
        db.get_validated_claude_session_owner("sess-claude-env")
            .unwrap()
            .as_deref(),
        Some(name.as_str()),
        "hooks must trust the binding the CLI just created"
    );

    assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    assert_eq!(
        db.get_session_binding("sess-claude-env")
            .unwrap()
            .as_deref(),
        Some(name.as_str()),
        "repeat start must return the first identity, not mint a second"
    );
    let claude_rows: Vec<String> = db
        .iter_instances_full()
        .unwrap()
        .into_iter()
        .filter(|row| row.tool == "claude")
        .map(|row| row.name)
        .collect();
    assert_eq!(claude_rows, vec![name], "exactly one identity per session");
}

#[test]
#[serial]
fn test_vanilla_codex_start_reuses_codex_session_id() {
    let (_dir, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    assert!(crate::hooks::codex::setup_codex_hooks(false));

    let env = HashMap::from([
        ("CODEX_SANDBOX".to_string(), "1".to_string()),
        (
            "CODEX_SESSION_ID".to_string(),
            "sess-codex-native".to_string(),
        ),
    ]);
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp/project"));

    assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    let name = db
        .get_session_binding("sess-codex-native")
        .unwrap()
        .expect("CODEX_SESSION_ID must bind identity");
    let row = db.get_instance_full(&name).unwrap().unwrap();
    assert_eq!(row.tool, "codex");
    assert_eq!(row.session_id.as_deref(), Some("sess-codex-native"));
    assert_eq!(
        db.get_validated_claude_session_owner("sess-codex-native")
            .unwrap(),
        None,
        "Codex must not populate Claude's validation cache"
    );

    assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    assert_eq!(
        db.get_session_binding("sess-codex-native")
            .unwrap()
            .as_deref(),
        Some(name.as_str()),
        "the hook's session id must retain the original identity"
    );
    assert_eq!(
        db.get_validated_claude_session_owner("sess-codex-native")
            .unwrap(),
        None,
        "repeated Codex start must not populate Claude's validation cache"
    );
}

#[test]
#[serial]
fn test_vanilla_claude_rebind_binds_session_and_drops_old_identity() {
    let (_dir, hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    crate::hooks::test_helpers::install_fake_claude_plugin(&_home);

    let ctx = make_claude_ctx(
        Some(("CLAUDE_CODE_SESSION_ID", "sess-rebind")),
        "/tmp/project",
    );
    assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    let first = db.get_session_binding("sess-rebind").unwrap().unwrap();

    assert_eq!(start_rebind(&db, "nova", &ctx, None).unwrap(), 0);
    assert_eq!(
        db.get_session_binding("sess-rebind").unwrap().as_deref(),
        Some("nova"),
        "a reclaimed name must own the session that reclaimed it"
    );
    assert!(
        db.get_instance_full(&first).unwrap().is_none(),
        "the identity being replaced must not be left behind"
    );
    assert_eq!(
        db.get_validated_claude_session_owner("sess-rebind")
            .unwrap()
            .as_deref(),
        Some("nova"),
        "hooks must resolve the reclaimed name, not reject the session"
    );

    assert_eq!(start_bare(&db, &hcom_dir, &ctx, None).unwrap(), 0);
    assert_eq!(
        db.get_session_binding("sess-rebind").unwrap().as_deref(),
        Some("nova"),
        "a start after the rebind returns the reclaimed identity"
    );
}

#[test]
#[serial]
fn test_root_rebind_preserves_child_hierarchy_and_actor_state() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();

    db.conn()
        .execute(
            "INSERT INTO instances
             (name, session_id, tool, status, status_time, last_seen, created_at)
             VALUES ('nova', 'sess-1', 'claude', 'active', 0, 0, 0)",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, parent_session_id, parent_name, agent_id, tool, status,
              status_time, last_seen, created_at)
             VALUES ('nova_task_1', 'sess-1', 'nova', 'agent-1', 'claude',
                     'active', 0, 0, 0)",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, parent_session_id, parent_name, agent_id, tool, status,
              status_time, last_seen, created_at)
             VALUES ('nova_task_2', 'sess-1', 'nova_task_1', 'agent-2', 'claude',
                     'active', 0, 0, 0)",
            [],
        )
        .unwrap();

    let token = db
        .issue_claude_actor_capability("sess-1", "tool-root", None, "nova")
        .unwrap();

    let links = snapshot_child_links(&db, Some("sess-1")).unwrap();
    assert_eq!(links.len(), 2);
    db.delete_instance("nova").unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, session_id, tool, status, status_time, last_seen, created_at)
             VALUES ('sol', 'sess-1', 'claude', 'active', 0, 0, 0)",
            [],
        )
        .unwrap();

    restore_child_links_after_root_rebind(&db, &links, "sess-1", "nova", "sol").unwrap();
    db.rebind_claude_root_actor_state("sess-1", "nova", "sol")
        .unwrap();

    let direct = db.get_instance_full("nova_task_1").unwrap().unwrap();
    assert_eq!(direct.parent_session_id.as_deref(), Some("sess-1"));
    assert_eq!(direct.parent_name.as_deref(), Some("sol"));
    let nested = db.get_instance_full("nova_task_2").unwrap().unwrap();
    assert_eq!(nested.parent_session_id.as_deref(), Some("sess-1"));
    assert_eq!(nested.parent_name.as_deref(), Some("nova_task_1"));
    assert_eq!(
        db.resolve_claude_actor_capability(&token, "sess-1")
            .unwrap(),
        Some("sol".to_string())
    );
}

#[test]
#[serial]
fn test_same_name_root_rebind_restores_child_session_links() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, session_id, tool, directory, status, status_time, last_seen, created_at)
             VALUES ('nova', 'sess-1', 'claude', '/tmp/project', 'active', 0, 0, 1)",
            [],
        )
        .unwrap();
    db.set_session_binding("sess-1", "nova").unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, parent_session_id, parent_name, agent_id, tool, status,
              status_time, last_seen, created_at)
             VALUES ('nova_task_1', 'sess-1', 'nova', 'agent-1', 'claude',
                     'active', 0, 0, 2)",
            [],
        )
        .unwrap();
    let token = db
        .issue_claude_actor_capability("sess-1", "tool-child", Some("agent-1"), "nova_task_1")
        .unwrap();

    let ctx = make_ctx(&[("CLAUDECODE", "1")], "/tmp/project");
    assert_eq!(start_rebind(&db, "nova", &ctx, Some("nova")).unwrap(), 0);

    let child = db.get_instance_full("nova_task_1").unwrap().unwrap();
    assert_eq!(child.parent_session_id.as_deref(), Some("sess-1"));
    assert_eq!(child.parent_name.as_deref(), Some("nova"));
    assert_eq!(
        db.resolve_claude_actor_capability(&token, "sess-1")
            .unwrap(),
        Some("nova_task_1".to_string())
    );
}

#[test]
#[serial]
fn test_start_rebind_rejects_cross_tool_stopped_snapshot_hijack() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();

    log_stopped_snapshot(
        &db,
        "fama",
        "codex",
        "/tmp/dasha-code/.worktrees/layer1-basic-conversation-fixes",
        "sid-fama",
        42,
    );

    let ctx = make_ctx(
        &[("CLAUDECODE", "1")],
        "/tmp/hcom-gan-harness/.worktrees/bench-infra",
    );

    let err = start_rebind(&db, "fama", &ctx, None).unwrap_err();
    assert!(
        err.to_string().contains("Refusing to reclaim 'fama'"),
        "unexpected error: {err}"
    );

    assert!(db.get_instance_full("fama").unwrap().is_none());
    assert_eq!(db.get_session_binding("sid-fama").unwrap(), None);
}

#[test]
#[serial]
fn test_start_rebind_allows_matching_stopped_snapshot_reclaim() {
    for session_id in [None, Some("sid-current")] {
        let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
        let db = HcomDb::open().unwrap();

        log_stopped_snapshot(
            &db,
            "nova",
            "claude",
            "/tmp/dasha-code/.worktrees/layer1-basic-conversation-fixes",
            "sid-nova",
            77,
        );

        let ctx = make_claude_ctx(
            session_id.map(|sid| ("CLAUDE_CODE_SESSION_ID", sid)),
            "/tmp/dasha-code/.worktrees/layer1-basic-conversation-fixes",
        );

        let exit_code = start_rebind(&db, "nova", &ctx, None).unwrap();
        assert_eq!(exit_code, 0);

        let inst = db.get_instance_full("nova").unwrap().unwrap();
        assert_eq!(
            inst.tool,
            if session_id.is_some() {
                "claude"
            } else {
                "adhoc"
            }
        );
        assert_eq!(inst.session_id.as_deref(), session_id);
        assert_eq!(
            inst.directory,
            "/tmp/dasha-code/.worktrees/layer1-basic-conversation-fixes"
        );
        assert_eq!(inst.last_event_id, 77);
    }
}

#[test]
#[serial]
fn test_start_rebind_rejects_cross_directory_stopped_snapshot_hijack() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = HcomDb::open().unwrap();

    log_stopped_snapshot(
        &db,
        "mira",
        "claude",
        "/tmp/dasha-code/.worktrees/layer1-basic-conversation-fixes",
        "sid-mira",
        18,
    );

    let ctx = make_ctx(
        &[("CLAUDECODE", "1")],
        "/tmp/hcom-gan-harness/.worktrees/bench-infra",
    );

    let err = start_rebind(&db, "mira", &ctx, None).unwrap_err();
    assert!(
        err.to_string().contains("Refusing to reclaim 'mira'"),
        "unexpected error: {err}"
    );

    assert!(db.get_instance_full("mira").unwrap().is_none());
}

#[test]
#[cfg(unix)]
fn test_same_path_resolves_symlink_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    let alias = dir.path().join("alias");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    assert!(same_path(
        real.to_string_lossy().as_ref(),
        alias.to_string_lossy().as_ref()
    ));
}
