use super::*;

fn make_test_db() -> (tempfile::TempDir, HcomDb) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    (dir, db)
}

fn make_delivery_test_db() -> (tempfile::TempDir, HcomDb) {
    let (dir, db) = make_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    db.conn()
            .execute(
                "INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-01-01T00:00:00Z', 'luna', '{\"from\":\"luna\",\"text\":\"hello\",\"scope\":\"broadcast\"}')",
                [],
            )
            .unwrap();
    (dir, db)
}

fn delivery_cursor(db: &HcomDb) -> i64 {
    db.conn()
        .query_row(
            "SELECT last_event_id FROM instances WHERE name = 'nova'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

struct FailingWriter;

impl std::io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "test write failure",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn hcom_fork_skips_copied_transcript_lineage() {
    assert!(!should_scan_sessionstart_lineage(
        true, "fork", false, false, true,
    ));
}

#[test]
fn inherited_hcom_fork_flag_does_not_skip_native_switch_lineage() {
    assert!(should_scan_sessionstart_lineage(
        false, "startup", true, false, false,
    ));
}

#[test]
#[serial]
fn inherited_hcom_fork_env_native_switch_uses_validated_ancestry() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    for (name, session_id) in [("kolo", "sess-fork"), ("lava", "sess-lava")] {
        db.conn()
                .execute(
                    "INSERT INTO instances
                     (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                     VALUES (?1, ?2, 'claude', 'listening', 'start', 0, 0, 0)",
                    rusqlite::params![name, session_id],
                )
                .unwrap();
        bind_validated_session(&db, session_id, name);
    }
    db.set_process_binding("process-restored", "sess-lava", "lava")
        .unwrap();

    let transcript = hcom_dir.join("fork-origin-switch.jsonl");
    std::fs::write(
        &transcript,
        "{\"sessionId\":\"sess-new\",\"message\":{\"session_id\":\"sess-fork\"}}\n",
    )
    .unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_IS_FORK".to_string(), "1".to_string());
    env.insert(
        "HCOM_PROCESS_ID".to_string(),
        "process-restored".to_string(),
    );
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-new",
        "transcript_path": transcript.to_string_lossy(),
    });

    let _ = handle_sessionstart(&db, &ctx, "sess-new", transcript.to_str(), &raw);

    assert_eq!(
        db.get_session_binding("sess-new").unwrap().as_deref(),
        Some("kolo")
    );
    assert_eq!(
        db.get_session_binding("sess-fork").unwrap().as_deref(),
        Some("kolo"),
        "promoting the new generation must preserve the ancestor alias"
    );
    assert_eq!(
        db.get_instance_full("kolo")
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("sess-new")
    );
    assert_eq!(
        db.get_process_binding_full("process-restored").unwrap(),
        Some((Some("sess-lava".to_string()), "lava".to_string()))
    );
    assert_eq!(
        db.get_instance_full("lava")
            .unwrap()
            .unwrap()
            .status_context,
        "start"
    );

    let second_transcript = hcom_dir.join("fork-origin-switch-2.jsonl");
    std::fs::write(
        &second_transcript,
        "{\"sessionId\":\"sess-new-2\",\"message\":{\"session_id\":\"sess-fork\"}}\n",
    )
    .unwrap();
    let second_raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-new-2",
        "transcript_path": second_transcript.to_string_lossy(),
    });
    let _ = handle_sessionstart(
        &db,
        &ctx,
        "sess-new-2",
        second_transcript.to_str(),
        &second_raw,
    );
    assert_eq!(
        db.get_session_binding("sess-new-2").unwrap().as_deref(),
        Some("kolo"),
        "a second switch may only carry the original copied ancestor"
    );
    assert_eq!(
        db.get_instance_full("kolo")
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("sess-new-2")
    );
}

#[test]
#[serial]
fn fresh_hcom_fork_placeholder_does_not_adopt_parent_ancestry() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('niza', 'sess-parent', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-parent", "niza");
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('kolo', 'claude', 'inactive', 'new', 0, 1, 0)",
            [],
        )
        .unwrap();
    db.set_process_binding("process-fork", "", "kolo").unwrap();

    let transcript = hcom_dir.join("hcom-fork-launch.jsonl");
    std::fs::write(
        &transcript,
        "{\"sessionId\":\"sess-child\",\"message\":{\"session_id\":\"sess-parent\"}}\n",
    )
    .unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_IS_FORK".to_string(), "1".to_string());
    env.insert("HCOM_PROCESS_ID".to_string(), "process-fork".to_string());
    env.insert("HCOM_LAUNCHED".to_string(), "1".to_string());
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-child",
        "transcript_path": transcript.to_string_lossy(),
    });

    let _ = handle_sessionstart(&db, &ctx, "sess-child", transcript.to_str(), &raw);

    assert_eq!(
        db.get_session_binding("sess-child").unwrap().as_deref(),
        Some("kolo")
    );
    assert_eq!(
        db.get_session_binding("sess-parent").unwrap().as_deref(),
        Some("niza")
    );
    assert_eq!(
        db.get_validated_claude_session_owner("sess-child")
            .unwrap()
            .as_deref(),
        Some("kolo")
    );
}

#[test]
#[serial]
fn fresh_standard_launch_bootstraps_and_validates_placeholder() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'claude', 'inactive', 'new', 0, 0, 0)",
            [],
        )
        .unwrap();
    db.set_process_binding("process-fresh", "", "nova").unwrap();

    let transcript = hcom_dir.join("fresh-start.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_PROCESS_ID".to_string(), "process-fresh".to_string());
    env.insert("HCOM_LAUNCHED".to_string(), "1".to_string());
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-fresh",
        "transcript_path": transcript.to_string_lossy(),
    });

    let _ = handle_sessionstart(&db, &ctx, "sess-fresh", transcript.to_str(), &raw);

    assert_eq!(
        db.get_session_binding("sess-fresh").unwrap().as_deref(),
        Some("nova")
    );
    assert_eq!(
        db.get_validated_claude_session_owner("sess-fresh")
            .unwrap()
            .as_deref(),
        Some("nova")
    );
    assert_eq!(
        db.get_process_binding_full("process-fresh").unwrap(),
        Some((Some("sess-fresh".to_string()), "nova".to_string()))
    );
}

#[test]
#[serial]
fn launch_blocked_placeholder_binds_on_sessionstart() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'claude', 'blocked', 'launch_blocked', 0, 0, 0)",
            [],
        )
        .unwrap();
    db.set_process_binding("process-blocked", "", "nova")
        .unwrap();

    let transcript = hcom_dir.join("launch-blocked-start.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_PROCESS_ID".to_string(), "process-blocked".to_string());
    env.insert("HCOM_LAUNCHED".to_string(), "1".to_string());
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-blocked",
        "transcript_path": transcript.to_string_lossy(),
    });

    let _ = handle_sessionstart(
        &db,
        &ctx,
        "sess-blocked",
        raw["transcript_path"].as_str(),
        &raw,
    );

    assert_eq!(
        db.get_session_binding("sess-blocked").unwrap().as_deref(),
        Some("nova")
    );
    assert_eq!(
        db.get_validated_claude_session_owner("sess-blocked")
            .unwrap()
            .as_deref(),
        Some("nova")
    );
}

#[test]
fn resumed_generation_with_fresh_process_binding_is_fresh() {
    let (_dir, db) = make_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-resume', 'claude', 'pending', 'new', 0, 0, 0)",
                [],
            )
            .unwrap();
    db.set_process_binding("process-resume", "", "nova")
        .unwrap();

    assert!(is_fresh_claude_process_placeholder(
        &db,
        None,
        Some("nova"),
        Some("sess-resume"),
    ));
    assert!(!is_fresh_claude_process_placeholder(
        &db,
        None,
        Some("nova"),
        None,
    ));
}

#[test]
#[serial]
fn unknown_native_startup_without_process_row_is_rejected() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('niza', 'sess-old', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-old", "niza");

    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_PROCESS_ID".to_string(), "process-gone".to_string());
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-ephemeral",
        "transcript_path": hcom_dir.join("missing.jsonl").to_string_lossy(),
    });

    let _ = handle_sessionstart(
        &db,
        &ctx,
        "sess-ephemeral",
        raw["transcript_path"].as_str(),
        &raw,
    );

    assert_eq!(db.get_session_binding("sess-ephemeral").unwrap(), None);
    assert_eq!(db.get_process_binding("process-gone").unwrap(), None);
    assert_eq!(
        db.get_instance_full("niza")
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("sess-old")
    );
}

#[test]
#[serial]
fn switched_lineage_removes_process_only_stale_binding_without_blocking_unrelated_status() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('niza', 'sess-old', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-old", "niza");
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('stale', 'claude', 'inactive', 'exit:old', 0, 1, 0)",
            [],
        )
        .unwrap();
    db.set_process_binding("process-stale", "", "stale")
        .unwrap();

    let transcript = hcom_dir.join("stale-process-switch.jsonl");
    std::fs::write(
        &transcript,
        "{\"sessionId\":\"sess-new\",\"message\":{\"session_id\":\"sess-old\"}}\n",
    )
    .unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_PROCESS_ID".to_string(), "process-stale".to_string());
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "sess-new",
        "transcript_path": transcript.to_string_lossy(),
    });

    let _ = handle_sessionstart(&db, &ctx, "sess-new", transcript.to_str(), &raw);

    assert_eq!(db.get_process_binding("process-stale").unwrap(), None);
    assert_eq!(
        db.get_instance_full("stale")
            .unwrap()
            .unwrap()
            .status_context,
        "exit:old"
    );
    assert_eq!(
        db.get_session_binding("sess-new").unwrap().as_deref(),
        Some("niza")
    );
}

#[test]
fn native_switch_or_unbound_session_scans_lineage() {
    assert!(should_scan_sessionstart_lineage(
        false, "startup", true, false, false,
    ));
    assert!(should_scan_sessionstart_lineage(
        false, "clear", false, false, false,
    ));
}

#[test]
fn settled_matching_session_skips_lineage_scan() {
    assert!(!should_scan_sessionstart_lineage(
        false, "clear", true, true, false,
    ));
}

#[test]
fn inherited_fork_flag_cannot_override_native_switch_session_id() {
    let (dir, db) = make_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at)
                 VALUES ('lava', 'sess-lava', 'claude', 'listening', 'start', 0, 0)",
            [],
        )
        .unwrap();
    db.set_session_binding("sess-lava", "lava").unwrap();
    db.set_process_binding("process-restored", "sess-lava", "lava")
        .unwrap();
    let env_file = dir
        .path()
        .join(".claude/session-env/12345678-1234-1234-1234-123456789012/hook-1.sh");
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_IS_FORK".to_string(), "1".to_string());
    env.insert(
        "HCOM_PROCESS_ID".to_string(),
        "process-restored".to_string(),
    );
    env.insert(
        "CLAUDE_ENV_FILE".to_string(),
        env_file.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "87654321-4321-4321-4321-210987654321"
    });

    assert!(!should_use_fork_env_session_id(&db, &ctx, &raw));
    assert_eq!(
        get_real_session_id(&raw, ctx.claude_env_file.as_deref(), false),
        "87654321-4321-4321-4321-210987654321"
    );
}

#[test]
fn fresh_fork_placeholder_may_use_env_file_session_id() {
    let (dir, db) = make_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, tool, status, status_context, status_time, created_at)
                 VALUES ('kolo', 'claude', 'inactive', 'new', 0, 0)",
            [],
        )
        .unwrap();
    db.set_process_binding("process-fork", "", "kolo").unwrap();
    let env_file = dir
        .path()
        .join(".claude/session-env/12345678-1234-1234-1234-123456789012/hook-1.sh");
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_IS_FORK".to_string(), "1".to_string());
    env.insert("HCOM_PROCESS_ID".to_string(), "process-fork".to_string());
    env.insert(
        "CLAUDE_ENV_FILE".to_string(),
        env_file.to_string_lossy().to_string(),
    );
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "source": "startup",
        "session_id": "parent-session"
    });

    assert!(should_use_fork_env_session_id(&db, &ctx, &raw));
    assert_eq!(
        get_real_session_id(&raw, ctx.claude_env_file.as_deref(), true),
        "12345678-1234-1234-1234-123456789012"
    );
}

#[test]
fn test_get_real_session_id_normal() {
    let raw = serde_json::json!({"session": {"session_id": "abc-123"}});
    assert_eq!(get_real_session_id(&raw, None, false), "abc-123");
}

#[test]
fn test_get_real_session_id_fork() {
    crate::config::Config::init(); // log_info needs Config
    let raw = serde_json::json!({"session": {"session_id": "old-parent-id"}});
    let env_file = "/home/user/.claude/session-env/12345678-1234-1234-1234-123456789012/hook-1.sh";
    assert_eq!(
        get_real_session_id(&raw, Some(env_file), true),
        "12345678-1234-1234-1234-123456789012"
    );
}

#[test]
fn test_get_real_session_id_non_fork_ignores_env() {
    let raw = serde_json::json!({"session": {"session_id": "correct-id"}});
    let env_file = "/home/user/.claude/session-env/wrong-id-from-env-file-path/hook-1.sh";
    // is_fork=false, so env_file should be ignored
    assert_eq!(
        get_real_session_id(&raw, Some(env_file), false),
        "correct-id"
    );
}

#[test]
fn test_subagent_start_with_agent_id() {
    let raw = serde_json::json!({"agent_id": "agent-uuid-123"});
    let result = build_subagent_start_output(&raw, "nova");
    assert!(result.is_some());
    let output = result.unwrap();
    let ctx = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("agent-uuid-123"));
    assert!(ctx.contains("nova"));
}

#[test]
fn test_subagent_start_no_agent_id() {
    let raw = serde_json::json!({});
    assert!(build_subagent_start_output(&raw, "nova").is_none());
}

#[test]
fn test_subagent_start_empty_agent_id() {
    let raw = serde_json::json!({"agent_id": ""});
    assert!(build_subagent_start_output(&raw, "nova").is_none());
}

#[test]
fn test_posttooluse_delivery_commits_after_output_write() {
    let (_dir, db) = make_delivery_test_db();
    let (output, ack) = get_posttooluse_messages(&db, "nova").unwrap();
    let system_message = output["systemMessage"].as_str().unwrap();
    assert!(system_message.contains("luna"));
    assert!(system_message.contains("nova"));
    assert!(system_message.contains("hello"));
    let ctx = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("luna"));
    assert!(ctx.contains("hello"));

    assert_eq!(delivery_cursor(&db), 0);
    let stdout = serde_json::to_string(&output).unwrap();
    let mut writer = Vec::new();
    write_hook_output(&db, &mut writer, &stdout, Some(&ack)).unwrap();

    assert_eq!(writer, stdout.as_bytes());
    assert_eq!(delivery_cursor(&db), ack.last_event_id);
}

#[test]
fn test_posttooluse_delivery_write_failure_keeps_message_unread() {
    let (_dir, db) = make_delivery_test_db();
    let (output, ack) = get_posttooluse_messages(&db, "nova").unwrap();
    let stdout = serde_json::to_string(&output).unwrap();

    let error = write_hook_output(&db, &mut FailingWriter, &stdout, Some(&ack)).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(delivery_cursor(&db), 0);
    assert_eq!(db.get_unread_messages("nova").len(), 1);
}

#[test]
fn test_combine_posttooluse_single() {
    let output = serde_json::json!({
        "systemMessage": "test msg",
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": "context1",
        }
    });
    let combined = combine_posttooluse_outputs(std::slice::from_ref(&output));
    assert_eq!(combined, output);
}

#[test]
fn test_combine_posttooluse_multiple() {
    let o1 = serde_json::json!({
        "systemMessage": "msg1",
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": "ctx1",
        }
    });
    let o2 = serde_json::json!({
        "systemMessage": "msg2",
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": "ctx2",
        }
    });
    let combined = combine_posttooluse_outputs(&[o1, o2]);
    assert_eq!(
        combined["hookSpecificOutput"]["additionalContext"],
        "ctx1\n\n---\n\nctx2"
    );
    assert_eq!(combined["systemMessage"], "msg1 + msg2");
}

#[test]
#[serial]
fn passive_markers_cannot_adopt_pending_identity() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_delivery_test_db();
    let ctx = make_ctx();
    for tool in ["claude", "codex", "gemini"] {
        db.conn()
            .execute(
                "UPDATE instances SET session_id = NULL, tool = ? WHERE name = 'nova'",
                [tool],
            )
            .unwrap();
        for command in ["ps -eo pid,command", "hcom transcript nova", "hcom start"] {
            let transcript = _dir.path().join("passive.jsonl");
            std::fs::write(&transcript, "[hcom:nova]").unwrap();
            let mut payload = HookPayload::from_claude(serde_json::json!({
                "session_id": "unjoined-session",
                "transcript_path": transcript,
                "tool_name": "Bash",
                "tool_input": {"command": command},
                "tool_response": {"stdout": "[hcom:nova]"}
            }));
            let (code, stdout, ack, _) = route_claude_hook(&db, &ctx, HOOK_POST, &mut payload);
            assert_eq!(code, 0);
            assert!(stdout.is_empty());
            assert!(ack.is_none());
            assert!(
                db.get_session_binding("unjoined-session")
                    .unwrap()
                    .is_none()
            );
            let row = db.get_instance_full("nova").unwrap().unwrap();
            assert!(row.session_id.is_none());
            assert_eq!(row.tool, tool);
            assert_eq!(delivery_cursor(&db), 0);
            assert_eq!(db.get_unread_messages("nova").len(), 1);
        }
    }
}

#[test]
fn test_is_hcom_hook_command() {
    assert!(is_hcom_hook_command("${HCOM} sessionstart"));
    assert!(is_hcom_hook_command("${HCOM} post"));
    assert!(is_hcom_hook_command("hcom sessionstart"));
    assert!(is_hcom_hook_command("hcom post"));
    assert!(is_hcom_hook_command("uvx hcom claude-notify"));
    assert!(!is_hcom_hook_command("echo hello"));
    assert!(!is_hcom_hook_command(""));
}

#[test]
fn test_is_hcom_hook_command_legacy() {
    assert!(is_hcom_hook_command("HCOM_ACTIVE=1 hcom.py sessionstart"));
    assert!(is_hcom_hook_command("sh -c 'hcom something'"));
}

#[test]
fn test_build_hook_entry_command_avoids_nested_shell() {
    let command = build_hook_entry_command("poll");
    assert_eq!(
        command,
        "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 || cmd=\"uvx hcom\"; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd poll || exit 0"
    );
    assert!(!command.starts_with("sh -c"));
}

#[test]
fn test_remove_hcom_hooks_empty() {
    let mut settings = serde_json::json!({});
    assert!(!remove_hcom_hooks_from_settings(&mut settings));
}

#[test]
fn test_remove_hcom_hooks_no_hooks_section() {
    let mut settings = serde_json::json!({"env": {"FOO": "bar"}});
    assert!(!remove_hcom_hooks_from_settings(&mut settings));
}

#[test]
fn test_remove_hcom_hooks_with_hcom() {
    let mut settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": "${HCOM} sessionstart"
                }]
            }]
        },
        "env": {"HCOM": "hcom"},
    });
    assert!(remove_hcom_hooks_from_settings(&mut settings));
    // SessionStart should be removed entirely
    assert!(settings["hooks"].get("SessionStart").is_none());
    // HCOM env should be removed
    assert!(settings.get("env").is_none());
}

#[test]
fn test_remove_hcom_hooks_preserves_non_hcom() {
    let mut settings = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "hooks": [
                    {"type": "command", "command": "${HCOM} post"},
                    {"type": "command", "command": "echo custom hook"},
                ]
            }]
        }
    });
    assert!(remove_hcom_hooks_from_settings(&mut settings));
    // Matcher should be preserved with only the custom hook
    let matchers = settings["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(matchers.len(), 1);
    let hooks = matchers[0]["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["command"], "echo custom hook");
}

#[test]
fn test_remove_hcom_permissions() {
    let mut settings = serde_json::json!({
        "hooks": {},
        "permissions": {
            "allow": [
                "Bash(hcom send:*)",
                "Bash(custom:*)",
            ]
        }
    });
    remove_hcom_hooks_from_settings(&mut settings);
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    assert_eq!(allow.len(), 1);
    assert_eq!(allow[0], "Bash(custom:*)");
}

#[test]
fn test_claude_hook_configs_count() {
    assert_eq!(CLAUDE_HOOK_CONFIGS.len(), 13);
    assert_eq!(CLAUDE_HOOK_TYPES.len(), 13);
    assert_eq!(CLAUDE_HOOK_COMMANDS.len(), 13);
}

#[test]
fn test_format_claude_permission() {
    assert_eq!(
        format_claude_permission("hcom", "send"),
        "Bash(hcom send:*)"
    );
    assert_eq!(
        format_claude_permission("hcom", "--help"),
        "Bash(hcom --help)"
    );
    assert_eq!(
        format_claude_permission("uvx hcom", "list"),
        "Bash(uvx hcom list:*)"
    );
}

#[test]
fn test_format_claude_powershell_permission() {
    assert_eq!(
        format_claude_powershell_permission("hcom", "send"),
        "PowerShell(hcom send:*)"
    );
    assert_eq!(
        format_claude_powershell_permission("hcom", "--help"),
        "PowerShell(hcom --help)"
    );
    assert_eq!(
        format_claude_powershell_permission("uvx hcom", "list"),
        "PowerShell(uvx hcom list:*)"
    );
}

#[test]
fn test_build_claude_permissions() {
    let perms = build_claude_permissions();
    assert!(!perms.is_empty());
    // Both shell variants are installed for every safe command, plus the
    // three actor-prelude statements used only by Claude subagents.
    assert_eq!(perms.len(), SAFE_HCOM_COMMANDS.len() * 2 + 3);
    assert_eq!(
        perms.iter().filter(|p| p.starts_with("Bash(")).count(),
        SAFE_HCOM_COMMANDS.len() + 1
    );
    assert_eq!(
        perms
            .iter()
            .filter(|p| p.starts_with("PowerShell("))
            .count(),
        SAFE_HCOM_COMMANDS.len() + 2
    );
    assert!(
        perms
            .iter()
            .any(|p| { p == "Bash(export HCOM_CLAUDE_ACTOR=* HCOM_CLAUDE_ACTOR_SESSION=*)" })
    );
    // All should start with "Bash(" or "PowerShell("
    for p in &perms {
        assert!(
            p.starts_with("Bash(") || p.starts_with("PowerShell("),
            "bad permission: {}",
            p
        );
    }
}

#[test]
fn test_build_all_claude_permission_patterns() {
    let patterns = build_all_claude_permission_patterns();
    // Both hcom prefixes and shell variants, plus actor-prelude cleanup.
    let expected = (SAFE_HCOM_COMMANDS.len() + LEGACY_HCOM_COMMANDS.len()) * 2 * 2 + 3;
    assert_eq!(patterns.len(), expected);
    assert!(patterns.iter().any(|p| p.contains("hcom send")));
    assert!(patterns.iter().any(|p| p == "PowerShell(hcom send:*)"));
    assert!(patterns.iter().any(|p| p == "PowerShell(uvx hcom send:*)"));
    assert!(patterns.iter().any(|p| p.contains("uvx hcom send")));
    // Legacy commands included for removal
    assert!(patterns.iter().any(|p| p.contains("hcom daemon")));
}

#[test]
fn test_setup_and_verify_claude_hooks() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join(".claude").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();

    // Write empty settings
    std::fs::write(&settings_path, "{}").unwrap();

    // Can't call setup_claude_hooks directly (uses get_claude_settings_path),
    // but we can test the verify path with a hand-built settings file.
    let hook_cmd = "${HCOM}";
    let mut settings = serde_json::json!({"hooks": {}, "env": {"HCOM": "hcom"}});

    for &(hook_type, matcher, cmd_suffix, timeout) in CLAUDE_HOOK_CONFIGS {
        let mut hook_entry = serde_json::json!({
            "type": "command",
            "command": format!("{} {}", hook_cmd, cmd_suffix),
        });
        if let Some(t) = timeout {
            hook_entry["timeout"] = serde_json::json!(t);
        }
        let mut hook_dict = serde_json::json!({"hooks": [hook_entry]});
        if !matcher.is_empty() {
            hook_dict["matcher"] = Value::String(matcher.to_string());
        }
        settings["hooks"][hook_type] = serde_json::json!([hook_dict]);
    }

    // Add permissions
    settings["permissions"] = serde_json::json!({"allow": build_claude_permissions()});

    let json_str = serde_json::to_string_pretty(&settings).unwrap();
    std::fs::write(&settings_path, &json_str).unwrap();

    // Verify should pass
    assert!(verify_claude_hooks_installed(Some(&settings_path), true,));

    // Verify without permissions check
    assert!(verify_claude_hooks_installed(Some(&settings_path), false,));
}

#[test]
fn test_verify_missing_file() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("nonexistent.json");
    assert!(!verify_claude_hooks_installed(Some(&settings_path), false,));
}

#[test]
fn test_verify_incomplete_hooks() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    // Only has SessionStart, missing others
    let settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{"type": "command", "command": "${HCOM} sessionstart"}]
            }]
        },
        "env": {"HCOM": "hcom"}
    });
    std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

    assert!(!verify_claude_hooks_installed(Some(&settings_path), false,));
}

fn write_settings_with_mutated_timeout(
    settings_path: &Path,
    new_timeout: Option<u64>,
    include_permissions: bool,
) {
    let hook_cmd = "${HCOM}";
    let mut settings = serde_json::json!({"hooks": {}, "env": {"HCOM": "hcom"}});

    for &(hook_type, matcher, cmd_suffix, timeout) in CLAUDE_HOOK_CONFIGS {
        let mut hook_entry = serde_json::json!({
            "type": "command",
            "command": format!("{} {}", hook_cmd, cmd_suffix),
        });
        if timeout.is_some()
            && let Some(t) = new_timeout
        {
            hook_entry["timeout"] = serde_json::json!(t);
        }
        let mut hook_dict = serde_json::json!({"hooks": [hook_entry]});
        if !matcher.is_empty() {
            hook_dict["matcher"] = Value::String(matcher.to_string());
        }
        settings["hooks"][hook_type] = serde_json::json!([hook_dict]);
    }

    if include_permissions {
        settings["permissions"] = serde_json::json!({"allow": build_claude_permissions()});
    }

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let json_str = serde_json::to_string_pretty(&settings).unwrap();
    std::fs::write(settings_path, &json_str).unwrap();
}

#[test]
fn test_verify_accepts_timeout_value_edit() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    // External edit: timeouts rewritten to 10 across all entries that
    // originally carried a timeout. Numeric value edits stay accepted —
    // only presence + numeric type are checked.
    write_settings_with_mutated_timeout(&settings_path, Some(10), false);
    assert!(verify_claude_hooks_installed(Some(&settings_path), false));
}

#[test]
fn test_verify_catches_timeout_field_dropped() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    write_settings_with_mutated_timeout(&settings_path, None, false);
    assert!(!verify_claude_hooks_installed(Some(&settings_path), false));
}

#[test]
fn test_verify_rejects_non_numeric_timeout() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    write_settings_with_mutated_timeout(&settings_path, Some(86400), false);
    let mut settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    for &(hook_type, _, _, expected_timeout) in CLAUDE_HOOK_CONFIGS {
        if expected_timeout.is_none() {
            continue;
        }
        if let Some(arr) = settings["hooks"][hook_type].as_array_mut() {
            for matcher_obj in arr {
                if let Some(hooks) = matcher_obj["hooks"].as_array_mut() {
                    for hook in hooks {
                        if hook.get("timeout").is_some() {
                            hook["timeout"] = serde_json::json!("86400");
                        }
                    }
                }
            }
        }
    }
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(!verify_claude_hooks_installed(Some(&settings_path), false));
}

#[test]
fn test_verify_rejects_missing_env() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    write_settings_with_mutated_timeout(&settings_path, None, false);
    let mut settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    settings.as_object_mut().unwrap().remove("env");
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(!verify_claude_hooks_installed(Some(&settings_path), false));
}

#[test]
fn test_verify_rejects_missing_command() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    write_settings_with_mutated_timeout(&settings_path, None, false);
    // Strip the hcom command from one required hook (PostToolUse) to
    // simulate a partial install / external removal.
    let mut settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    if let Some(post) = settings["hooks"]["PostToolUse"].as_array_mut() {
        post.clear();
    }
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(!verify_claude_hooks_installed(Some(&settings_path), false));
}

#[test]
fn test_remove_hooks_from_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    assert!(remove_hooks_from_settings_path(&path));
}

#[test]
fn test_remove_hooks_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{"type": "command", "command": "${HCOM} sessionstart"}]
            }]
        },
        "env": {"HCOM": "hcom"},
        "other_key": "preserved"
    });
    std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

    assert!(remove_hooks_from_settings_path(&path));

    // Verify hooks are gone but other_key preserved
    let content = std::fs::read_to_string(&path).unwrap();
    let result: Value = serde_json::from_str(&content).unwrap();
    assert!(result["hooks"].get("SessionStart").is_none());
    assert_eq!(result["other_key"], "preserved");
}

use crate::hooks::test_helpers::{EnvGuard, isolated_test_env};
use serial_test::serial;

fn claude_test_env() -> (tempfile::TempDir, PathBuf, PathBuf, EnvGuard) {
    let (dir, _hcom_dir, test_home, guard) = isolated_test_env();
    let settings_path = test_home.join(".claude").join("settings.json");
    (dir, test_home, settings_path, guard)
}

fn read_json(path: &Path) -> Value {
    let content = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&content).unwrap()
}

/// Independent verification: no hcom hooks in Claude settings JSON.
fn independently_verify_no_hcom_hooks_claude(settings: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    let hooks = match settings.get("hooks").and_then(|v| v.as_object()) {
        Some(h) => h,
        None => return violations,
    };
    let hcom_patterns = ["hcom", "HCOM", "${HCOM}"];
    for (hook_type, matchers_val) in hooks {
        let matchers = match matchers_val.as_array() {
            Some(a) => a,
            None => continue,
        };
        for (i, matcher) in matchers.iter().enumerate() {
            let hooks_arr = match matcher.get("hooks").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => continue,
            };
            for (j, hook) in hooks_arr.iter().enumerate() {
                let command = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if hcom_patterns.iter().any(|p| command.contains(p)) {
                    violations.push(format!("{hook_type}[{i}].hooks[{j}]: command={command}"));
                }
            }
        }
    }
    violations
}

/// Independent verification: expected hcom hooks present.
fn independently_verify_hcom_hooks_present_claude(
    settings: &Value,
    expected: &[(&str, &str)], // (hook_type, command_substring)
) -> Vec<String> {
    let mut missing = Vec::new();
    let hooks = match settings.get("hooks").and_then(|v| v.as_object()) {
        Some(h) => h,
        None => {
            return expected
                .iter()
                .map(|(ht, _)| format!("{ht}: hooks dict missing"))
                .collect();
        }
    };
    for &(hook_type, cmd_suffix) in expected {
        let expected_full = build_hook_entry_command(cmd_suffix);
        let matchers = match hooks.get(hook_type).and_then(|v| v.as_array()) {
            Some(a) => a,
            None => {
                missing.push(format!("{hook_type}: not present"));
                continue;
            }
        };
        let mut found = false;
        for matcher in matchers {
            if let Some(hook_list) = matcher.get("hooks").and_then(|v| v.as_array()) {
                for hook in hook_list {
                    if let Some(cmd) = hook.get("command").and_then(|v| v.as_str())
                        && cmd == expected_full
                    {
                        found = true;
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
        if !found {
            missing.push(format!(
                "{hook_type}: expected exact command '{expected_full}', not found"
            ));
        }
    }
    missing
}

#[test]
#[serial]
fn test_setup_claude_hooks_from_scratch() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    assert!(setup_claude_hooks(false));
    assert!(settings_path.exists());

    let settings = read_json(&settings_path);

    // All hook types should be present
    assert!(settings.get("hooks").unwrap().is_object());
    for &(hook_type, matcher, cmd_suffix, timeout) in CLAUDE_HOOK_CONFIGS {
        let arr = settings["hooks"]
            .get(hook_type)
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{hook_type} missing or not array"));
        assert!(!arr.is_empty(), "{hook_type} should have entries");

        // Find the hcom hook entry with exact command match
        let expected_command = build_hook_entry_command(cmd_suffix);
        let mut found = false;
        for entry in arr {
            let hooks_list = entry.get("hooks").and_then(|v| v.as_array());
            if let Some(hooks) = hooks_list {
                for hook in hooks {
                    let cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    if cmd == expected_command {
                        found = true;
                        // Verify matcher if non-empty
                        if !matcher.is_empty() {
                            assert_eq!(
                                entry.get("matcher").and_then(|v| v.as_str()).unwrap_or(""),
                                matcher,
                                "{hook_type} matcher mismatch"
                            );
                        }
                        // Verify timeout if set
                        if let Some(t) = timeout {
                            assert_eq!(
                                hook.get("timeout").and_then(|v| v.as_u64()),
                                Some(t),
                                "{hook_type} timeout mismatch"
                            );
                        }
                    }
                }
            }
        }
        assert!(
            found,
            "{hook_type}: expected exact command '{expected_command}', not found"
        );
    }

    // HCOM env var should be set
    assert!(
        settings.get("env").and_then(|v| v.get("HCOM")).is_some(),
        "HCOM env var should be set"
    );

    assert!(verify_claude_hooks_installed(Some(&settings_path), false));

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_claude_preserves_user_data() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "env": {"MY_VAR": "test", "OTHER": "value"},
        "permissions": {
            "deny": ["Bash(rm -rf:*)"],
        },
        "hooks": {
            "PostToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": "echo user hook",
                    "name": "my-logger",
                }]
            }]
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    assert!(setup_claude_hooks(false));

    let updated = read_json(&settings_path);

    // User env keys preserved (HCOM is added by setup)
    assert_eq!(updated["env"]["MY_VAR"], "test");
    assert_eq!(updated["env"]["OTHER"], "value");
    assert!(updated["env"].get("HCOM").is_some());

    // permissions.deny preserved
    assert_eq!(
        updated["permissions"]["deny"],
        serde_json::json!(["Bash(rm -rf:*)"])
    );

    // User hook preserved
    let post_hooks = updated["hooks"]["PostToolUse"].as_array().unwrap();
    let mut found_user_hook = false;
    for entry in post_hooks {
        if let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) {
            for hook in hooks {
                if hook.get("command").and_then(|v| v.as_str()) == Some("echo user hook") {
                    found_user_hook = true;
                }
            }
        }
    }
    assert!(found_user_hook, "user hook should be preserved");

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_claude_idempotent() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, r#"{"env": {"MY_VAR": "test"}}"#).unwrap();

    assert!(setup_claude_hooks(false));
    let first = std::fs::read_to_string(&settings_path).unwrap();

    assert!(setup_claude_hooks(false));
    let second = std::fs::read_to_string(&settings_path).unwrap();

    assert_eq!(first, second, "setup should be idempotent");

    drop(_guard);
}

#[test]
#[serial]
fn test_remove_claude_only_removes_hcom() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    // Mixed hcom + user hooks in same type
    let settings = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "hooks": [
                    {"type": "command", "command": "${HCOM} post"},
                    {"type": "command", "command": "echo user hook", "name": "my-logger"},
                ]
            }]
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(remove_hooks_from_settings_path(&settings_path));

    let updated = read_json(&settings_path);
    // User hook should remain
    let post_hooks = updated["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post_hooks.len(), 1);
    let hooks_list = post_hooks[0]["hooks"].as_array().unwrap();
    assert_eq!(hooks_list.len(), 1);
    assert_eq!(hooks_list[0]["command"], "echo user hook");

    // No hcom hooks
    let violations = independently_verify_no_hcom_hooks_claude(&updated);
    assert!(violations.is_empty(), "hcom hooks remain: {violations:?}");

    drop(_guard);
}

#[test]
#[serial]
fn test_claude_setup_remove_roundtrip() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "env": {"MY_VAR": "test"},
        "permissions": {"deny": ["dangerous"]},
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    // Setup
    assert!(setup_claude_hooks(false));
    let after_setup = read_json(&settings_path);
    let expected = vec![
        ("PostToolUse", "post"),
        ("Stop", "poll"),
        ("PermissionRequest", "permission-request"),
        ("Notification", "notify"),
    ];
    let missing = independently_verify_hcom_hooks_present_claude(&after_setup, &expected);
    assert!(
        missing.is_empty(),
        "after setup, missing hooks: {missing:?}"
    );

    // Remove
    assert!(remove_hooks_from_settings_path(&settings_path));
    let after_remove = read_json(&settings_path);
    let violations = independently_verify_no_hcom_hooks_claude(&after_remove);
    assert!(
        violations.is_empty(),
        "after remove, hcom hooks still present: {violations:?}"
    );

    // User data preserved
    assert_eq!(after_remove["env"]["MY_VAR"], "test");
    assert_eq!(
        after_remove["permissions"]["deny"],
        serde_json::json!(["dangerous"])
    );

    drop(_guard);
}

#[test]
#[serial]
fn test_claude_handles_empty_file() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, "{}").unwrap();

    assert!(setup_claude_hooks(false));

    let settings = read_json(&settings_path);
    assert!(settings.get("hooks").unwrap().is_object());
    assert!(settings["hooks"].get("PostToolUse").is_some());

    drop(_guard);
}

#[test]
#[serial]
fn test_claude_handles_no_file() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    assert!(!settings_path.exists());
    assert!(setup_claude_hooks(false));
    assert!(settings_path.exists());

    let settings = read_json(&settings_path);
    assert!(settings.get("hooks").is_some());

    drop(_guard);
}

#[test]
#[serial]
fn test_claude_handles_malformed_hooks() {
    let corrupt_cases: Vec<Value> = vec![
        Value::Null,
        Value::String("string".into()),
        serde_json::json!([]),
        serde_json::json!({"PreToolUse": "not_a_list"}),
        serde_json::json!({"PreToolUse": [null, "string", 123]}),
        serde_json::json!({"PreToolUse": [{"matcher": "*", "hooks": "not_a_list"}]}),
    ];

    for corrupt_hooks in corrupt_cases {
        let (_dir, _test_home, settings_path, _guard) = claude_test_env();
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();

        let settings = serde_json::json!({
            "hooks": corrupt_hooks,
            "env": {"MY_VAR": "test"},
        });
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        // Should not crash
        let _ = setup_claude_hooks(false);

        // User data should still be there
        let updated = read_json(&settings_path);
        assert_eq!(updated["env"]["MY_VAR"], "test");
    }
}

#[test]
#[serial]
fn test_setup_claude_with_permissions() {
    let (_dir, _test_home, settings_path, _guard) = claude_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "permissions": {
            "allow": ["Bash(custom:*)"],
            "deny": ["Bash(rm -rf:*)"],
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    assert!(setup_claude_hooks(true));

    let updated = read_json(&settings_path);
    let allow = updated["permissions"]["allow"].as_array().unwrap();

    // User's custom permission preserved
    assert!(
        allow.iter().any(|v| v.as_str() == Some("Bash(custom:*)")),
        "user permission should be preserved"
    );
    // hcom permissions added
    let perms = build_claude_permissions();
    for p in &perms {
        assert!(
            allow.iter().any(|v| v.as_str() == Some(p.as_str())),
            "hcom permission {p} should be added"
        );
    }
    // deny preserved
    assert_eq!(
        updated["permissions"]["deny"],
        serde_json::json!(["Bash(rm -rf:*)"])
    );

    assert!(verify_claude_hooks_installed(Some(&settings_path), true));

    drop(_guard);
}

// ---- Hook-actor routing (raw.agent_id is authoritative) ----
//
// These are dispatcher-level tests: they drive `route_claude_hook` itself
// (the function `dispatch_claude_hook` calls after reading stdin), not the
// individual handler functions, because the bug class here is a routing
// bug — which branch a given hook payload falls into — not a bug inside
// any one handler. They need `isolated_test_env()` because
// `route_claude_hook` exercises real log::log_info call sites (spawn ownership,
// sessionstart and lifecycle handlers), which resolve the hcom log path
// through the global `Config`; without isolation that would touch the
// real `~/.hcom` of whatever machine runs the test.

fn make_ctx() -> HcomContext {
    HcomContext::from_env(&std::collections::HashMap::new(), PathBuf::from("/tmp"))
}

fn make_isolated_test_db() -> (tempfile::TempDir, EnvGuard, HcomDb) {
    let (dir, hcom_dir, _test_home, guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    (dir, guard, db)
}

fn bind_validated_session(db: &HcomDb, session_id: &str, instance_name: &str) {
    db.set_session_binding(session_id, instance_name).unwrap();
    db.mark_claude_session_validated(session_id, instance_name)
        .unwrap();
}

#[test]
#[serial]
fn test_root_bash_pretooluse_does_not_inject_actor_capability() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, session_id, tool, status, status_time, last_seen, created_at)
                 VALUES ('nova', 'sess-1', 'claude', 'active', 0, 0, 0)",
            [],
        )
        .unwrap();

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "tool_name": "Bash",
        "tool_use_id": "toolu-1",
        "tool_input": {"command": "hcom list"},
    });
    let payload = HookPayload::from_claude(raw);
    let (_, stdout) = handle_pretooluse(&db, &payload, "nova", "sess-1", None);
    assert!(
        stdout.is_empty(),
        "root command must remain byte-for-byte intact"
    );
    let capabilities: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM claude_actor_capabilities",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capabilities, 0);
}

#[test]
fn test_hcom_command_detection_biases_toward_instrumentation() {
    for command in [
        "hcom list",
        "PATH=/tmp/bin hcom list",
        "echo ready && hcom send @nova -- hi",
        "printf data | hcom events",
        "/opt/hcom/bin/hcom list | head",
        "uvx hcom list",
        "$HCOM list",
        "${HCOM} list",
        "timeout 5 hcom list",
        "bash -c 'hcom list'",
        "echo hcom list",
        "printf 'run hcom list later'",
        "rg hcom src",
    ] {
        assert!(
            visibly_invokes_hcom(command),
            "expected hcom token to trigger instrumentation: {command}"
        );
    }

    for command in [
        "git status",
        "./script-containing-hcom-in-its-name",
        "echo hcommunication",
        "rg hook-comms src",
        "node tool.js",
    ] {
        assert!(
            !visibly_invokes_hcom(command),
            "non-hcom token must not trigger instrumentation: {command}"
        );
    }
}

#[test]
#[serial]
fn test_subagent_non_hcom_shell_command_is_not_modified() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
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

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-1",
        "tool_name": "Bash",
        "tool_use_id": "toolu-ordinary",
        "tool_input": {"command": "node -e 'console.log(42)'"},
    });
    let payload = HookPayload::from_claude(raw);
    let (_, stdout) = handle_pretooluse(&db, &payload, "nova_task_1", "sess-1", Some("agent-1"));
    assert!(
        stdout.is_empty(),
        "non-hcom command must remain byte-for-byte untouched"
    );
    let capabilities: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM claude_actor_capabilities",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capabilities, 0);
}

#[test]
#[serial]
fn test_subagent_bash_pretooluse_injects_stable_actor_capability_and_failure_revokes_it() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
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

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-1",
        "tool_name": "Bash",
        "tool_use_id": "toolu-1",
        "tool_input": {"command": "hcom list | head"},
    });
    let payload = HookPayload::from_claude(raw);
    let (_, first) = handle_pretooluse(&db, &payload, "nova_task_1", "sess-1", Some("agent-1"));
    let (_, second) = handle_pretooluse(&db, &payload, "nova_task_1", "sess-1", Some("agent-1"));
    assert_eq!(
        first, second,
        "duplicate hook delivery must reuse one token"
    );

    let output: Value = serde_json::from_str(&first).unwrap();
    let command = output["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    let prefix = format!("export {}=", crate::claude_actor::ENV_VAR);
    assert!(command.starts_with(&prefix));
    assert!(command.contains(&format!("{}=sess-1", crate::claude_actor::SESSION_ENV_VAR)));
    assert!(command.ends_with("\nhcom list | head"));
    let token = command
        .strip_prefix(&prefix)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert_eq!(
        db.resolve_claude_actor_capability(token, "sess-1").unwrap(),
        Some("nova_task_1".to_string())
    );

    let failure_raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-1",
        "tool_name": "Bash",
        "tool_use_id": "toolu-1",
        "tool_input": {"command": "hcom list | head"},
        "error": "failed",
    });
    let failure = HookPayload::from_claude(failure_raw);
    handle_tool_failure(&db, &failure, "nova_task_1", "sess-1", Some("agent-1"));
    assert_eq!(
        db.resolve_claude_actor_capability(token, "sess-1").unwrap(),
        None
    );
}

#[test]
#[serial]
fn test_powershell_pretooluse_injects_and_revokes_actor_capability() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
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

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-1",
        "tool_name": "PowerShell",
        "tool_use_id": "toolu-ps-1",
        "tool_input": {"command": "hcom list"},
    });
    let payload = HookPayload::from_claude(raw);
    let (_, stdout) = handle_pretooluse(&db, &payload, "nova_task_1", "sess-1", Some("agent-1"));
    let output: Value = serde_json::from_str(&stdout).unwrap();
    let command = output["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    let prefix = format!("$env:{} = '", crate::claude_actor::ENV_VAR);
    assert!(command.starts_with(&prefix));
    assert!(command.contains(&format!(
        "$env:{} = 'sess-1'",
        crate::claude_actor::SESSION_ENV_VAR
    )));
    assert!(command.ends_with("\nhcom list"));
    let token = command
        .strip_prefix(&prefix)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert_eq!(
        db.resolve_claude_actor_capability(token, "sess-1").unwrap(),
        Some("nova_task_1".to_string())
    );

    let failure_raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-1",
        "tool_name": "PowerShell",
        "tool_use_id": "toolu-ps-1",
        "tool_input": {"command": "hcom list"},
        "error": "failed",
    });
    let failure = HookPayload::from_claude(failure_raw);
    handle_tool_failure(&db, &failure, "nova_task_1", "sess-1", Some("agent-1"));
    assert_eq!(
        db.resolve_claude_actor_capability(token, "sess-1").unwrap(),
        None
    );
}

#[test]
#[serial]
fn test_agent_pretooluse_updates_status_without_creating_child_state() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, session_id, tool, status, status_time, last_seen, created_at)
                 VALUES ('nova', 'sess-1', 'claude', 'active', 0, 0, 0)",
            [],
        )
        .unwrap();
    bind_validated_session(&db, "sess-1", "nova");

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "prompt_id": "prompt-1",
        "tool_name": "Agent",
        "tool_input": {"prompt": "do work"},
    });
    let mut payload = HookPayload::from_claude(raw);
    let _ = route_claude_hook(&db, &make_ctx(), HOOK_PRE, &mut payload);

    let child_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM instances WHERE parent_name IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        child_count, 0,
        "PreToolUse alone must not create child lifecycle state"
    );
    let root = db.get_instance_full("nova").unwrap().unwrap();
    assert_eq!(root.status, ST_ACTIVE);
    assert_eq!(root.status_context, "tool:Agent");
}

#[test]
#[serial]
fn test_stale_child_remains_addressable_and_subagent_start_revives_same_row() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, session_id, tool, status, status_time, last_seen,
                  subagent_timeout, created_at)
                 VALUES ('nova', 'sess-1', 'claude', 'active', 0, 0, 1, 0)",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, parent_session_id, parent_name, agent_id, tool, status,
                  status_context, status_time, last_seen, created_at)
                 VALUES ('nova_task_1', 'sess-1', 'nova', 'agent-1', 'claude',
                         'active', 'tool:Bash', 1, 1, 0)",
            [],
        )
        .unwrap();
    bind_validated_session(&db, "sess-1", "nova");

    mark_stale_subagents(&db, "sess-1");
    let stale = db.get_instance_full("nova_task_1").unwrap().unwrap();
    assert_eq!(stale.status_context, "subagent:stale");

    let token = db
        .issue_claude_actor_capability("sess-1", "tool-1", Some("agent-1"), "nova_task_1")
        .unwrap();
    assert_eq!(
        db.resolve_claude_actor_capability(&token, "sess-1")
            .unwrap(),
        Some("nova_task_1".to_string()),
        "display staleness must not affect verified actor identity"
    );

    let revived = ensure_subagent_row(&db, "nova", "sess-1", "agent-1", "task").unwrap();
    assert_eq!(revived, "nova_task_1");
    let revived_row = db.get_instance_full(&revived).unwrap().unwrap();
    assert_eq!(revived_row.status_context, "subagent:dormant");
    assert!(revived_row.last_seen > 1);
}

/// Same fixture as `make_delivery_test_db` (instance 'nova' + a pending
/// broadcast message from 'luna'), but under `isolated_test_env()` so
/// dispatcher-level tests that log are safe to run.
fn make_isolated_delivery_test_db() -> (tempfile::TempDir, EnvGuard, HcomDb) {
    let (dir, guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    db.conn()
            .execute(
                "INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-01-01T00:00:00Z', 'luna', '{\"from\":\"luna\",\"text\":\"hello\",\"scope\":\"broadcast\"}')",
                [],
            )
            .unwrap();
    (dir, guard, db)
}

/// `root_session_id` matches what real `ensure_subagent_row`-created rows
/// carry as `parent_session_id` — always the true root session_id — so
/// fixtures built with this helper look the same as a real row would.
fn insert_subagent_row(
    db: &HcomDb,
    name: &str,
    agent_id: &str,
    parent_name: &str,
    root_session_id: &str,
) {
    db.conn()
            .execute(
                "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, last_event_id, agent_id, parent_name, parent_session_id)
                 VALUES (?, 'claude', 'active', 'subagent', 0, 0, 0, ?, ?, ?)",
                rusqlite::params![name, agent_id, parent_name, root_session_id],
            )
            .unwrap();
}

#[test]
#[serial]
fn test_first_resumed_pty_wake_combines_bootstrap_and_pending_delivery() {
    crate::config::Config::init();
    let (dir, _guard, db) = make_isolated_delivery_test_db();
    let mut env = std::collections::HashMap::new();
    env.insert("HCOM_LAUNCHED".to_string(), "1".to_string());
    env.insert("HCOM_PTY_MODE".to_string(), "1".to_string());
    env.insert(
        "HCOM_DIR".to_string(),
        dir.path().join(".hcom").to_string_lossy().into_owned(),
    );
    let ctx = HcomContext::from_env(&env, dir.path().to_path_buf());
    let payload = HookPayload::from_claude(serde_json::json!({
        "session_id": "sess-1",
        "prompt": "<hcom>",
    }));
    let instance = db.get_instance_full("nova").unwrap().unwrap();
    assert_eq!(instance.name_announced, 0);

    let (exit_code, stdout, ack) = handle_userpromptsubmit(
        &db,
        &ctx,
        &payload,
        "nova",
        &serde_json::Map::new(),
        &instance,
    );

    assert_eq!(exit_code, 0);
    assert!(
        stdout.contains("hello"),
        "pending message missing: {stdout}"
    );
    assert!(
        stdout.contains("[hcom:nova]"),
        "bootstrap identity missing: {stdout}"
    );
    let ack = ack.expect("pending message must be acknowledged with the combined output");
    assert_eq!(delivery_cursor(&db), 0, "ack must remain deferred");
    common::commit_delivery_ack(&db, &ack);
    assert!(delivery_cursor(&db) > 0);
    assert_eq!(
        db.get_instance_full("nova")
            .unwrap()
            .unwrap()
            .name_announced,
        1
    );
}

#[test]
#[serial]
fn test_userpromptsubmit_only_blocks_an_exact_bare_wake_when_nothing_is_pending() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id, name_announced)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0, 1)",
                [],
            )
            .unwrap();
    let ctx = make_ctx();
    let instance = db.get_instance_full("nova").unwrap().unwrap();

    for prompt in ["ordinary user prompt", "<hcom> user draft"] {
        let payload = HookPayload::from_claude(serde_json::json!({ "prompt": prompt }));
        let (_code, stdout, ack) = handle_userpromptsubmit(
            &db,
            &ctx,
            &payload,
            "nova",
            &serde_json::Map::new(),
            &instance,
        );
        assert!(stdout.is_empty(), "prompt must pass through: {prompt}");
        assert!(ack.is_none());
    }

    let payload = HookPayload::from_claude(serde_json::json!({ "prompt": " \n<hcom>\t" }));
    let (_code, stdout, ack) = handle_userpromptsubmit(
        &db,
        &ctx,
        &payload,
        "nova",
        &serde_json::Map::new(),
        &instance,
    );
    let output: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["decision"], "block");
    assert_eq!(output["suppressOriginalPrompt"], true);
    assert!(
        output["reason"]
            .as_str()
            .unwrap()
            .contains("Nothing is pending")
    );
    assert!(ack.is_none());
}

/// Property: a subagent-context hook whose row *does* resolve, but which
/// doesn't match any actionable branch (e.g. an ordinary Edit tool call),
/// stays a silent no-op rather than falling through to parent handling.
#[test]
#[serial]
fn test_subagent_hook_unrelated_tool_stays_silent() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    insert_subagent_row(&db, "nova_task_1", "sub-agent-1", "nova", "sess-1");

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "sub-agent-1",
        "tool_name": "Edit",
        "tool_input": {"file_path": "/tmp/x"},
    });
    let mut payload = HookPayload::from_claude(raw);
    let ctx = make_ctx();
    let (exit_code, stdout, ack, _timing) = route_claude_hook(&db, &ctx, HOOK_POST, &mut payload);

    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(ack.is_none());
}

/// Property: stopping a nested native parent must recursively tear down its
/// own children. Native subagent rows carry session_id=NULL and inherit the
/// root session as parent_session_id, so the session-keyed teardown cascade
/// never links a nested parent to its children — only parent_name does.
/// Without the parent_name cascade, stopping parent A would delete A while
/// its child B stayed alive, reparented to a reusable name.
#[test]
#[serial]
fn test_stop_nested_parent_cascades_to_children() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    // A is a native subagent of root nova; B is a native subagent of A.
    // Both share the root session as parent_session_id and have no session
    // of their own (session_id=NULL, as insert_subagent_row leaves it).
    insert_subagent_row(&db, "nova_a_1", "agent-a", "nova", "sess-1");
    insert_subagent_row(&db, "nova_a_1_b_1", "agent-b", "nova_a_1", "sess-1");

    crate::hooks::stop_instance(&db, "nova_a_1", "test", "task_completed");

    assert!(
        db.get_instance_by_agent_id("agent-a").unwrap().is_none(),
        "nested parent A must be torn down"
    );
    assert!(
        db.get_instance_by_agent_id("agent-b").unwrap().is_none(),
        "child B must be cascaded, not orphaned with parent_name=A"
    );
    assert!(
        db.get_instance_full("nova").unwrap().is_some(),
        "the still-live root must not be touched"
    );
}

/// Conflicting structured identity must fail closed before
/// UserPromptSubmit prepares or acknowledges any pending queue.
#[test]
#[serial]
fn test_ambiguous_userpromptsubmit_does_not_consume_messages() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db = HcomDb::open_raw(&hcom_dir.join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, transcript_path, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES
                 ('niza', 'sess-original', '/tmp/niza.jsonl', 'claude', 'listening', 'start', 0, 0, 0),
                 ('lava', 'sess-poisoned', '/tmp/lava.jsonl', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-original", "niza");
    db.set_session_binding("sess-poisoned", "lava").unwrap();
    db.set_process_binding("process-restored", "sess-original", "niza")
        .unwrap();
    db.conn()
        .execute(
            r#"INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-01-01T00:00:00Z', 'sender',
                         '{"from":"sender","text":"secret","scope":"broadcast"}')"#,
            [],
        )
        .unwrap();

    let transcript = hcom_dir.join("poisoned.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"session_id\":\"sess-original\"}}\n",
    )
    .unwrap();
    let mut env = std::collections::HashMap::new();
    env.insert(
        "HCOM_DIR".to_string(),
        hcom_dir.to_string_lossy().to_string(),
    );
    env.insert("CLAUDECODE".to_string(), "1".to_string());
    env.insert(
        "HCOM_PROCESS_ID".to_string(),
        "process-restored".to_string(),
    );
    env.insert("HCOM_PTY_MODE".to_string(), "1".to_string());
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let raw = serde_json::json!({
        "session_id": "sess-poisoned",
        "transcript_path": transcript,
        "prompt": "<hcom>"
    });
    let mut payload = HookPayload::from_claude(raw);

    let (exit_code, stdout, ack, timing) =
        route_claude_hook(&db, &ctx, HOOK_USERPROMPTSUBMIT, &mut payload);

    assert_eq!(exit_code, 0);
    let output: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["decision"], "block");
    assert_eq!(output["suppressOriginalPrompt"], true);
    assert!(
        output["reason"]
            .as_str()
            .unwrap()
            .contains("could not prepare delivery context")
    );
    assert!(ack.is_none());
    assert_eq!(timing.result, Some("no_instance"));
    assert_eq!(db.get_cursor("niza"), 0);
    assert_eq!(db.get_cursor("lava"), 0);
    assert_eq!(
        db.get_instance_full("niza").unwrap().unwrap().status,
        ST_LISTENING
    );
    assert_eq!(
        db.get_instance_full("lava").unwrap().unwrap().status,
        ST_LISTENING
    );
}

/// Property: PostToolUse for the Agent/Task tool fires with
/// `tool_response.status == "async_launched"` when Claude merely
/// dispatched the call to the background (default since Claude Code
/// 2.1.198) — this is not completion, so it must not deliver a
/// "Subagents have finished" summary and must not advance the delivery
/// cursor. Foreground completion is covered separately by
/// `test_task_posttooluse_foreground_completed_delivers`.
#[test]
#[serial]
fn test_task_posttooluse_async_launch_skips_delivery() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_delivery_test_db();
    bind_validated_session(&db, "sess-1", "nova");
    let ctx = make_ctx();

    let raw_async = serde_json::json!({
        "session_id": "sess-1",
        "tool_name": "Agent",
        "tool_response": {"status": "async_launched"},
    });
    let mut payload_async = HookPayload::from_claude(raw_async);
    let (_exit_code, stdout_async, ack_async, _timing) =
        route_claude_hook(&db, &ctx, HOOK_POST, &mut payload_async);
    assert!(
        stdout_async.is_empty(),
        "async_launched must not be treated as Task completion, got: {stdout_async}"
    );
    assert!(ack_async.is_none());
    assert_eq!(
        delivery_cursor(&db),
        0,
        "async_launched must not advance the delivery cursor"
    );
}

/// Property: a foreground (synchronous, non-backgrounded) Agent/Task
/// PostToolUse — no `async_launched` status — is a genuine completion and
/// must deliver freeze-period messages normally. Independent of the
/// async_launched test above: this is not "the same call, later", it's
/// the separately-exercised foreground path.
#[test]
#[serial]
fn test_task_posttooluse_foreground_completed_delivers() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_delivery_test_db();
    bind_validated_session(&db, "sess-1", "nova");
    let ctx = make_ctx();

    let raw_done = serde_json::json!({
        "session_id": "sess-1",
        "tool_name": "Agent",
        "tool_response": {"status": "completed"},
    });
    let mut payload_done = HookPayload::from_claude(raw_done);
    let (_exit_code, stdout_done, _ack, _timing) =
        route_claude_hook(&db, &ctx, HOOK_POST, &mut payload_done);
    assert!(
        stdout_done.contains("hello"),
        "a foreground Task completion must deliver freeze messages, got: {stdout_done}"
    );
    assert!(delivery_cursor(&db) > 0);
}

/// Property: Bash PostToolUse delivery uses Claude's verified `agent_id`,
/// never a caller-controlled `--name` embedded in the command text.
#[test]
#[serial]
fn test_subagent_bash_post_routes_verified_actor_not_command_name() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    insert_subagent_row(&db, "nova_task_1", "agent-a", "nova", "sess-1");
    insert_subagent_row(&db, "nova_task_2", "agent-b", "nova", "sess-1");
    // A direct mention pending for subagent B's inbox only.
    db.conn()
            .execute(
                "INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-01-01T00:00:00Z', 'luna', '{\"from\":\"luna\",\"text\":\"secret for b\",\"scope\":\"mentions\",\"mentions\":[\"nova_task_2\"]}')",
                [],
            )
            .unwrap();
    let ctx = make_ctx();

    // Hook fires inside subagent A's own context (agent_id=agent-a), while
    // the command text names B. Hook delivery must still use A.
    let raw_spoof = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-a",
        "tool_name": "Bash",
        "tool_input": {"command": "hcom send --name agent-b -- hi"},
    });
    let mut payload_spoof = HookPayload::from_claude(raw_spoof);
    let (exit_code, stdout, ack, _timing) =
        route_claude_hook(&db, &ctx, HOOK_POST, &mut payload_spoof);
    assert_eq!(exit_code, 0);
    assert!(
        stdout.is_empty(),
        "verified actor A must not receive actor B's inbox, got: {stdout}"
    );
    assert!(ack.is_none());

    // Actor B receives B's message regardless of command parsing.
    let raw_ok = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-b",
        "tool_name": "Bash",
        "tool_input": {"command": "hcom send --name agent-b -- hi"},
    });
    let mut payload_ok = HookPayload::from_claude(raw_ok);
    let (_exit_code, stdout_ok, ack_ok, _timing) =
        route_claude_hook(&db, &ctx, HOOK_POST, &mut payload_ok);
    assert!(
        stdout_ok.contains("secret for b"),
        "verified actor B must receive its own inbox, got: {stdout_ok}"
    );
    assert!(ack_ok.is_some());
}

/// Property: every Claude subagent carries `agent_id` on its hooks
/// regardless of whether its root ever ran `hcom start` — that's a
/// property of Claude's hook schema, not of hcom participation.
/// SubagentStart must stay a silent no-op (no `hcom start --name ...`
/// hint and no allocated row) when the shared
/// session_id has no hcom root binding at all.
#[test]
#[serial]
fn test_subagent_start_nonparticipant_root_stays_silent() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    // No session binding: "sess-1" is not an hcom participant.

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "child-1",
        "agent_type": "general",
    });
    let mut payload = HookPayload::from_claude(raw);
    let ctx = make_ctx();
    let (exit_code, stdout, ack, _timing) =
        route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload);

    assert_eq!(exit_code, 0);
    assert!(
        stdout.is_empty(),
        "nonparticipant SubagentStart must not inject an hcom hint, got: {stdout}"
    );
    assert!(ack.is_none());
    assert!(
        db.get_instance_by_agent_id("child-1").unwrap().is_none(),
        "nonparticipant SubagentStart must not allocate an instances row"
    );
}

#[test]
#[serial]
fn test_nonparticipant_child_hcom_is_denied_but_other_shell_is_silent() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    let ctx = make_ctx();

    let hcom_raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "child-1",
        "tool_name": "Bash",
        "tool_use_id": "tool-hcom",
        "tool_input": {"command": "hcom start"},
    });
    let mut hcom_payload = HookPayload::from_claude(hcom_raw);
    let (exit_code, stdout, ack, _timing) =
        route_claude_hook(&db, &ctx, HOOK_PRE, &mut hcom_payload);
    assert_eq!(exit_code, 0);
    assert!(ack.is_none());
    let output: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        output["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("parent Claude session")
    );

    let ordinary_raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "child-1",
        "tool_name": "Bash",
        "tool_use_id": "tool-ordinary",
        "tool_input": {"command": "git status"},
    });
    let mut ordinary_payload = HookPayload::from_claude(ordinary_raw);
    let (exit_code, stdout, ack, _timing) =
        route_claude_hook(&db, &ctx, HOOK_PRE, &mut ordinary_payload);
    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(ack.is_none());
}

/// Property: a SubagentStart with no `prompt_id` field at all (Claude
/// Code < 2.1.196, where this correlation doesn't exist on the wire)
/// must still attach to root — the pre-2.1.196 legacy behavior, not
/// nested-spawn support.
#[test]
#[serial]
fn test_subagent_start_without_prompt_id_uses_legacy_root_attribution() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-legacy",
        "agent_type": "general",
    });
    let mut payload = HookPayload::from_claude(raw);
    let ctx = make_ctx();
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload);

    let name = db
        .get_instance_by_agent_id("agent-legacy")
        .unwrap()
        .unwrap();
    let row = db.get_instance_full(&name).unwrap().unwrap();
    assert_eq!(row.parent_name.as_deref(), Some("nova"));
}

/// Property: multiple children spawned in parallel by one actor within
/// one turn share the same `prompt_id`. If an earlier sibling's own
/// Task/Agent PostToolUse completes *before* a later sibling's
/// SubagentStart arrives, the second sibling must still attach cleanly to
/// root — completion of one sibling must not disturb the other's
/// attribution, since both always resolve to root regardless of
/// prompt_id bookkeeping.
#[test]
#[serial]
fn test_parallel_siblings_survive_interleaved_sibling_completion() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    let ctx = make_ctx();

    // Root issues (what will become) two parallel Task calls in one
    // turn — Claude stamps both with the same prompt_id.
    let raw_pre = serde_json::json!({
        "session_id": "sess-1",
        "prompt_id": "p1",
        "tool_name": "Task",
        "tool_input": {"prompt": "spawn two parallel helpers"},
    });
    let mut payload_pre = HookPayload::from_claude(raw_pre);
    let _ = route_claude_hook(&db, &ctx, HOOK_PRE, &mut payload_pre);

    // First sibling starts.
    let raw_start1 = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-1",
        "agent_type": "general",
        "prompt_id": "p1",
    });
    let mut payload_start1 = HookPayload::from_claude(raw_start1);
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload_start1);

    // That Task tool_use's own PostToolUse completes — interleaved
    // *before* the second sibling's SubagentStart arrives.
    let raw_post = serde_json::json!({
        "session_id": "sess-1",
        "prompt_id": "p1",
        "tool_name": "Task",
        "tool_response": {"status": "completed"},
    });
    let mut payload_post = HookPayload::from_claude(raw_post);
    let _ = route_claude_hook(&db, &ctx, HOOK_POST, &mut payload_post);

    // Second sibling starts, same shared prompt_id.
    let raw_start2 = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-2",
        "agent_type": "general",
        "prompt_id": "p1",
    });
    let mut payload_start2 = HookPayload::from_claude(raw_start2);
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload_start2);

    let name1 = db.get_instance_by_agent_id("agent-1").unwrap().unwrap();
    let name2 = db.get_instance_by_agent_id("agent-2").unwrap();
    assert!(
        name2.is_some(),
        "the second sibling must still resolve its owner after the first sibling's PostToolUse completed"
    );
    let row1 = db.get_instance_full(&name1).unwrap().unwrap();
    let row2 = db.get_instance_full(&name2.unwrap()).unwrap().unwrap();
    assert_eq!(row1.parent_name.as_deref(), Some("nova"));
    assert_eq!(
        row2.parent_name.as_deref(),
        Some("nova"),
        "both siblings must attach to the same true owner despite the interleaved completion"
    );
}

// ---- Resumed-agent correlation ----

/// Property: a resumed subagent — Claude re-firing SubagentStart for a
/// previously-known `agent_id` under a *new* `prompt_id` — must reattach
/// to root, not fail closed. Sequential: the original subagent fully
/// spawns and stops (row deleted) before the resume fires.
#[test]
#[serial]
fn test_resumed_agent_id_reattaches_to_root_after_original_stopped() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    let ctx = make_ctx();

    // Original spawn.
    let raw_pre = serde_json::json!({
        "session_id": "sess-1",
        "prompt_id": "p1",
        "tool_name": "Task",
        "tool_input": {"prompt": "do a thing"},
    });
    let mut payload_pre = HookPayload::from_claude(raw_pre);
    let _ = route_claude_hook(&db, &ctx, HOOK_PRE, &mut payload_pre);

    let raw_start = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "agent_type": "general",
        "prompt_id": "p1",
    });
    let mut payload_start = HookPayload::from_claude(raw_start);
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload_start);
    let original_name = db.get_instance_by_agent_id("agent-x").unwrap().unwrap();
    assert_eq!(
        db.get_instance_full(&original_name)
            .unwrap()
            .unwrap()
            .parent_name
            .as_deref(),
        Some("nova")
    );

    // It stops (dormant + no direct message => immediate idle stop).
    let raw_stop = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "prompt_id": "p1",
    });
    let mut payload_stop = HookPayload::from_claude(raw_stop);
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_STOP, &mut payload_stop);
    assert!(
        db.get_instance_by_agent_id("agent-x").unwrap().is_none(),
        "row must be gone after stop"
    );

    // Resume: same agent_id, new prompt_id, no PreToolUse for it.
    let raw_resume = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "agent_type": "general",
        "prompt_id": "p2",
    });
    let mut payload_resume = HookPayload::from_claude(raw_resume);
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload_resume);

    let resumed_name = db.get_instance_by_agent_id("agent-x").unwrap();
    assert!(resumed_name.is_some(), "resume must not fail closed");
    let resumed_name = resumed_name.unwrap();
    assert_eq!(
        resumed_name, original_name,
        "resume must preserve the same hcom child identity"
    );
    let resumed_row = db.get_instance_full(&resumed_name).unwrap().unwrap();
    assert_eq!(
        resumed_row.parent_name.as_deref(),
        Some("nova"),
        "resume must reattach to root"
    );
}

/// Property: the resumed subagent's next PostToolUse must resolve its
/// identity (not the reported `unknown_subagent_actor` fail-closed path)
/// once resume has reattached it — otherwise `hcom list`/`send` can't see
/// a subagent Claude's own TUI still shows as live.
#[test]
#[serial]
fn test_resumed_agent_posttooluse_resolves_actor_not_unknown() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    let ctx = make_ctx();

    let raw_resume = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "agent_type": "general",
        "prompt_id": "p2",
    });
    let mut payload_resume = HookPayload::from_claude(raw_resume);
    let _ = route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload_resume);
    assert!(db.get_instance_by_agent_id("agent-x").unwrap().is_some());

    let raw_post = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "tool_name": "Read",
        "tool_input": {},
    });
    let mut payload_post = HookPayload::from_claude(raw_post);
    let (exit_code, _stdout, _ack, timing) =
        route_claude_hook(&db, &ctx, HOOK_POST, &mut payload_post);
    assert_eq!(exit_code, 0);
    assert_ne!(
        timing.result,
        Some("unknown_subagent_actor"),
        "the resumed agent's own row must resolve, not fail closed as unknown"
    );
}

/// Property: concurrent duplicate hook delivery (the other live finding)
/// combined with a resume must still converge on the one true owner —
/// no split attribution, no lost row allocation.
#[test]
#[serial]
fn test_concurrent_resumed_subagent_start_resolves_to_same_owner() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db_path = hcom_dir.join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");

    let n = 4;
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let path = db_path.clone();
            std::thread::spawn(move || {
                let db = HcomDb::open_raw(&path).unwrap();
                let ctx = make_ctx();
                let raw = serde_json::json!({
                    "session_id": "sess-1",
                    "agent_id": "agent-x",
                    "agent_type": "general",
                    "prompt_id": "p-resume",
                });
                let mut payload = HookPayload::from_claude(raw);
                route_claude_hook(&db, &ctx, HOOK_SUBAGENT_START, &mut payload)
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let name = db.get_instance_by_agent_id("agent-x").unwrap();
    assert!(
        name.is_some(),
        "concurrent resumed SubagentStart must not fail closed"
    );
    let row = db.get_instance_full(&name.unwrap()).unwrap().unwrap();
    assert_eq!(row.parent_name.as_deref(), Some("nova"));
}

// ---- Duplicate hook delivery idempotency (SubagentStop) ----

fn expect_stop_claim<'a>(result: SubagentStopClaimResult<'a>) -> SubagentStopClaim<'a> {
    match result {
        SubagentStopClaimResult::Acquired(claim) => claim,
        SubagentStopClaimResult::Duplicate => panic!("expected claim, got duplicate"),
        SubagentStopClaimResult::RetryableError(error) => {
            panic!("expected claim, got retryable error: {error}")
        }
    }
}

/// Property: the claim key must collapse a byte-identical duplicate
/// invocation, but must NOT collapse a same-`prompt_id` invocation whose
/// payload actually differs (SubagentStop legitimately re-fires within
/// one prompt/continuation after an exit_code=2 delivery — we have not
/// established Claude changes `prompt_id` for that re-fire), and must
/// not collapse a genuinely different `prompt_id` either.
#[test]
fn test_subagent_stop_inflight_key_semantics() {
    let raw = serde_json::json!({"agent_id": "a1", "prompt_id": "p1", "x": 1});
    let raw_dup = serde_json::json!({"agent_id": "a1", "prompt_id": "p1", "x": 1});
    assert_eq!(
        subagent_stop_inflight_key("sess-1", "a1", &raw),
        subagent_stop_inflight_key("sess-1", "a1", &raw_dup),
        "byte-identical payloads must collapse to the same key"
    );

    let raw_same_prompt_diff_payload =
        serde_json::json!({"agent_id": "a1", "prompt_id": "p1", "x": 2});
    assert_ne!(
        subagent_stop_inflight_key("sess-1", "a1", &raw),
        subagent_stop_inflight_key("sess-1", "a1", &raw_same_prompt_diff_payload),
        "same prompt_id with different payload content must not collapse"
    );

    let raw_diff_prompt = serde_json::json!({"agent_id": "a1", "prompt_id": "p2", "x": 1});
    assert_ne!(
        subagent_stop_inflight_key("sess-1", "a1", &raw),
        subagent_stop_inflight_key("sess-1", "a1", &raw_diff_prompt),
        "different prompt_id must not collapse"
    );
}

/// Property: while a claim is held, an identical repeat loses (concurrency
/// guard), and a same-prompt-but-different payload gets its own claim.
#[test]
#[serial]
fn test_claim_subagent_stop_collapses_duplicate_invocation() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    let raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});
    let _first_claim =
        expect_stop_claim(SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw));
    assert!(
        matches!(
            SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw),
            SubagentStopClaimResult::Duplicate
        ),
        "duplicate identical invocation must not re-claim while held"
    );

    let raw_different_payload =
        serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1", "note": "different"});
    let _different_claim = expect_stop_claim(SubagentStopClaim::acquire(
        &db,
        "sess-1",
        "agent-x",
        &raw_different_payload,
    ));
}

/// The claim is a transient concurrency guard, not a session-long
/// tombstone. Two distinct SubagentStop invocations can carry identical
/// payloads. While one is
/// in-flight the identical duplicate must lose (concurrency dedup), but
/// once `SubagentStopClaim` releases it, a later identical stop must be able
/// to re-claim and be processed — not suppressed forever.
#[test]
#[serial]
fn test_stop_claim_released_lets_later_identical_stop_reclaim() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    let raw = serde_json::json!({
        "agent_id": "agent-x",
        "prompt_id": "p1",
        "last_assistant_message": "Done.",
    });
    let key = subagent_stop_inflight_key("sess-1", "agent-x", &raw);

    let first_claim = expect_stop_claim(SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw));
    assert!(
        matches!(
            SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw),
            SubagentStopClaimResult::Duplicate
        ),
        "a concurrent duplicate still in-flight must lose the claim"
    );
    drop(first_claim);
    assert!(
        db.kv_get(&key).unwrap().is_none(),
        "guard drop must release the claim key"
    );
    let _later_claim =
        expect_stop_claim(SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw));
}

#[test]
#[serial]
fn test_stop_claim_atomically_replaces_dead_owner() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    let raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});
    let key = subagent_stop_inflight_key("sess-1", "agent-x", &raw);
    let dead_owner = serde_json::to_string(&SubagentStopOwner {
        owner_token: "crashed-owner".to_string(),
        pid: u32::MAX,
        process_start: "dead-process".to_string(),
    })
    .unwrap();
    db.kv_set(&key, Some(&dead_owner)).unwrap();

    let claim = expect_stop_claim(SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw));
    let replacement = db.kv_get(&key).unwrap().unwrap();
    assert_ne!(replacement, dead_owner);
    assert_eq!(replacement, claim.value);
}

#[test]
#[serial]
fn test_stop_claim_drop_deletes_only_its_own_token() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    let raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});
    let key = subagent_stop_inflight_key("sess-1", "agent-x", &raw);
    let claim = expect_stop_claim(SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw));

    let replacement = r#"{"owner_token":"replacement","pid":1,"process_start":"other"}"#;
    db.kv_set(&key, Some(replacement)).unwrap();
    drop(claim);
    assert_eq!(
        db.kv_get(&key).unwrap().as_deref(),
        Some(replacement),
        "an old guard must not delete a newer owner's token"
    );
}

#[test]
#[serial]
fn test_stop_claim_error_blocks_subagent_stop_for_retry() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn().execute("DROP TABLE kv", []).unwrap();
    let raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});

    let (exit_code, stdout, _ack) = subagent_stop(&db, "sess-1", &raw);
    assert_eq!(exit_code, 0, "JSON decisions are processed only on exit 0");
    let output: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["decision"], "block");
    assert!(
        output["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("Please stop again")),
        "the blocking decision must tell Claude to retry SubagentStop"
    );
}

#[test]
#[serial]
fn test_post_claim_read_error_blocks_subagent_stop_for_retry() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 1)",
            [],
        )
        .unwrap();
    insert_subagent_row(&db, "nova_task_1", "agent-x", "nova", "sess-1");
    db.conn()
        .execute(
            "UPDATE instances SET transcript_path = x'80' WHERE agent_id = 'agent-x'",
            [],
        )
        .unwrap();
    let raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});

    let (exit_code, stdout, _ack) = subagent_stop(&db, "sess-1", &raw);
    assert_eq!(exit_code, 0);
    let output: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["decision"], "block");
    assert_eq!(
        db.get_instance_by_agent_id("agent-x").unwrap().as_deref(),
        Some("nova_task_1")
    );
    assert!(db.get_instance_full("nova_task_1").is_err());
}

#[test]
#[serial]
fn test_stop_finalization_error_keeps_child_row_retryable() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 1)",
            [],
        )
        .unwrap();
    insert_subagent_row(&db, "nova_task_1", "agent-x", "nova", "sess-1");
    db.conn()
        .execute_batch(
            "CREATE TRIGGER reject_child_stop BEFORE INSERT ON events
                 WHEN NEW.type = 'life' AND NEW.instance = 'nova_task_1'
                 BEGIN SELECT RAISE(ABORT, 'injected stop failure'); END;",
        )
        .unwrap();
    let raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});

    let (exit_code, stdout, _ack) = subagent_stop(&db, "sess-1", &raw);
    assert_eq!(exit_code, 0);
    let output: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["decision"], "block");
    assert!(db.get_instance_by_agent_id("agent-x").unwrap().is_some());
}

/// `subagent_stop` must release its claim so it cannot outlive the
/// invocation. Uses
/// a zero timeout so the poll returns immediately without blocking.
#[test]
#[serial]
fn test_subagent_stop_releases_its_claim_on_completion() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id, subagent_timeout)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0, 0)",
                [],
            )
            .unwrap();
    // name_announced=1 -> skip the dormant idle gate and go straight to the
    // poll, which returns immediately (timeout 0) with no message.
    insert_subagent_row(&db, "nova_task_1", "agent-x", "nova", "sess-1");
    db.conn()
        .execute(
            "UPDATE instances SET name_announced = 1 WHERE name = 'nova_task_1'",
            [],
        )
        .unwrap();

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "prompt_id": "p1",
        "last_assistant_message": "Done.",
    });
    let _ = subagent_stop(&db, "sess-1", &raw);

    assert!(
        db.kv_get(&subagent_stop_inflight_key("sess-1", "agent-x", &raw))
            .unwrap()
            .is_none(),
        "subagent_stop must not leave its claim behind as a permanent tombstone"
    );
}

/// Property: `subagent_stop` must check the claim *before* the idle gate
/// and before `poll_messages` — a duplicate invocation must skip all
/// processing, not just the final teardown. This is what the live
/// teardown-window failure traced back to: with duplicate hook
/// registrations, one invocation could receive a delivered message
/// (exit_code=2) while the other, unclaimed, independently timed out and
/// deleted the row out from under it.
#[test]
#[serial]
fn test_subagent_stop_skips_all_processing_when_already_claimed() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    insert_subagent_row(&db, "nova_task_1", "agent-x", "nova", "sess-1");

    let raw = serde_json::json!({
        "session_id": "sess-1",
        "agent_id": "agent-x",
        "prompt_id": "p1",
    });
    // Simulate a concurrent duplicate having already claimed this exact
    // invocation (and still in-flight, so the claim is unreleased).
    let _existing_claim =
        expect_stop_claim(SubagentStopClaim::acquire(&db, "sess-1", "agent-x", &raw));

    let (exit_code, stdout, _ack) = subagent_stop(&db, "sess-1", &raw);
    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(
        db.get_instance_by_agent_id("agent-x").unwrap().is_some(),
        "an already-claimed duplicate must not delete the row"
    );
    let stopped_events: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life'
                 AND json_extract(data, '$.action') = 'stopped'
                 AND json_extract(data, '$.snapshot.agent_id') = 'agent-x'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stopped_events, 0,
        "an already-claimed duplicate must not log a second life.stopped event"
    );
}

/// Property: genuinely concurrent duplicate SubagentStop hook delivery
/// (separate DB connections, as separate hook processes would be) for
/// the same dormant subagent must produce exactly one teardown — one
/// life.stopped event, one row deletion — not one per invocation.
#[test]
#[serial]
fn test_concurrent_duplicate_subagent_stop_produces_one_teardown() {
    crate::config::Config::init();
    let (_dir, hcom_dir, _test_home, _guard) = isolated_test_env();
    let db_path = hcom_dir.join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    insert_subagent_row(&db, "nova_task_1", "agent-x", "nova", "sess-1");

    let n = 4;
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let path = db_path.clone();
            std::thread::spawn(move || {
                let db = HcomDb::open_raw(&path).unwrap();
                // Identical payload from every "duplicate hook registration".
                let raw = serde_json::json!({
                    "session_id": "sess-1",
                    "agent_id": "agent-x",
                    "prompt_id": "p1",
                });
                subagent_stop(&db, "sess-1", &raw)
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    assert!(
        db.get_instance_by_agent_id("agent-x").unwrap().is_none(),
        "the subagent must end up stopped exactly like a single invocation would"
    );
    let stopped_events: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life'
                 AND json_extract(data, '$.action') = 'stopped'
                 AND json_extract(data, '$.snapshot.agent_id') = 'agent-x'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stopped_events, 1,
        "concurrent duplicate delivery must log exactly one life.stopped event, not {n}"
    );
}

/// A delayed SessionEnd from a historical Claude session must not finalize
/// or overwrite the newer primary generation.
#[test]
#[serial]
fn test_historical_sessionend_preserves_current_primary_generation() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, transcript_path, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-new', '/tmp/new.jsonl', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-new", "nova");
    bind_validated_session(&db, "sess-old", "nova");

    let stop_raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});
    let old_stop_key = subagent_stop_inflight_key("sess-old", "agent-x", &stop_raw);
    db.kv_set(&old_stop_key, Some("1")).unwrap();

    let raw = serde_json::json!({
        "session_id": "sess-old",
        "transcript_path": "/tmp/old.jsonl",
        "reason": "clear"
    });
    let mut payload = HookPayload::from_claude(raw);
    let ctx = make_ctx();
    let _ = route_claude_hook(&db, &ctx, HOOK_SESSIONEND, &mut payload);

    let instance = db.get_instance_full("nova").unwrap().unwrap();
    assert_eq!(instance.session_id.as_deref(), Some("sess-new"));
    assert_eq!(instance.transcript_path, "/tmp/new.jsonl");
    assert_eq!(instance.status, ST_LISTENING);
    assert_eq!(
        db.get_session_binding("sess-new").unwrap().as_deref(),
        Some("nova")
    );
    assert_eq!(
        db.get_session_binding("sess-old").unwrap().as_deref(),
        Some("nova")
    );
    assert_eq!(db.kv_get(&old_stop_key).unwrap(), None);

    let stopped_events: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events
                 WHERE type = 'life'
                   AND json_extract(data, '$.action') = 'stopped'
                   AND instance = 'nova'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stopped_events, 0);
}

#[test]
#[serial]
fn historical_subagent_completion_revokes_actor_capability() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-new', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-new", "nova");
    bind_validated_session(&db, "sess-old", "nova");
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, parent_session_id, parent_name, agent_id, tool, status, status_time, created_at)
                 VALUES ('nova_task_1', 'sess-new', 'nova', 'agent-1', 'claude', 'active', 0, 1)",
                [],
            )
            .unwrap();
    let token = db
        .issue_claude_actor_capability("sess-old", "toolu-old", Some("agent-1"), "nova_task_1")
        .unwrap();

    let raw = serde_json::json!({
        "session_id": "sess-old",
        "agent_id": "agent-1",
        "tool_name": "Bash",
        "tool_use_id": "toolu-old",
        "tool_input": {"command": "hcom list"},
        "tool_response": {"stdout": "done"},
    });
    let mut payload = HookPayload::from_claude(raw);
    let ctx = make_ctx();
    let _ = route_claude_hook(&db, &ctx, HOOK_POST, &mut payload);

    let remaining: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM claude_actor_capabilities WHERE token = ?",
            rusqlite::params![token],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
#[serial]
fn test_historical_root_hooks_are_rejected_before_dispatch() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances
                 (name, session_id, transcript_path, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-new', '/tmp/new.jsonl', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-new", "nova");
    bind_validated_session(&db, "sess-old", "nova");
    db.conn()
        .execute(
            r#"INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-01-01T00:00:00Z', 'sender',
                         '{"from":"sender","text":"secret","scope":"broadcast"}')"#,
            [],
        )
        .unwrap();

    let ctx = make_ctx();
    for (hook_type, raw) in [
        (
            HOOK_USERPROMPTSUBMIT,
            serde_json::json!({
                "session_id": "sess-old",
                "transcript_path": "/tmp/old.jsonl"
            }),
        ),
        (
            HOOK_PRE,
            serde_json::json!({
                "session_id": "sess-old",
                "transcript_path": "/tmp/old.jsonl",
                "tool_name": "Bash",
                "tool_input": {"command": "echo old"}
            }),
        ),
        (
            HOOK_POST,
            serde_json::json!({
                "session_id": "sess-old",
                "transcript_path": "/tmp/old.jsonl",
                "tool_name": "Write",
                "tool_response": {"ok": true}
            }),
        ),
    ] {
        let mut payload = HookPayload::from_claude(raw);
        let (_, stdout, ack, timing) = route_claude_hook(&db, &ctx, hook_type, &mut payload);
        assert!(stdout.is_empty());
        assert!(ack.is_none());
        assert_eq!(timing.result, Some("historical_session"));
    }

    let instance = db.get_instance_full("nova").unwrap().unwrap();
    assert_eq!(instance.session_id.as_deref(), Some("sess-new"));
    assert_eq!(instance.transcript_path, "/tmp/new.jsonl");
    assert_eq!(instance.status, ST_LISTENING);
    assert_eq!(instance.last_event_id, 0);
}

/// Property: SessionEnd sweeps subagent_stop_inflight entries — only for
/// its own session.
#[test]
#[serial]
fn test_sessionend_cleans_stop_inflight_keys() {
    crate::config::Config::init();
    let (_dir, _guard, db) = make_isolated_test_db();
    db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('nova', 'sess-1', 'claude', 'listening', 'start', 0, 0, 0)",
                [],
            )
            .unwrap();
    bind_validated_session(&db, "sess-1", "nova");
    let ctx = make_ctx();

    let stop_raw = serde_json::json!({"agent_id": "agent-x", "prompt_id": "p1"});
    db.kv_set(
        &subagent_stop_inflight_key("sess-1", "agent-x", &stop_raw),
        Some("1"),
    )
    .unwrap();
    db.kv_set(
        &subagent_stop_inflight_key("sess-other", "agent-x", &stop_raw),
        Some("1"),
    )
    .unwrap();

    let raw = serde_json::json!({"session_id": "sess-1", "reason": "clear"});
    let mut payload = HookPayload::from_claude(raw);
    let _ = route_claude_hook(&db, &ctx, HOOK_SESSIONEND, &mut payload);

    assert!(
        db.kv_get(&subagent_stop_inflight_key("sess-1", "agent-x", &stop_raw))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.kv_get(&subagent_stop_inflight_key(
            "sess-other",
            "agent-x",
            &stop_raw
        ))
        .unwrap(),
        Some("1".to_string()),
        "a different session's stop-claim keys must survive"
    );
}
