use super::*;
use clap::Parser;
use serial_test::serial;
use std::path::PathBuf;

type TestEnv = (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    crate::hooks::test_helpers::EnvGuard,
);

#[test]
fn parse_basic_send() {
    let args = SendArgs::try_parse_from(["send", "@luna", "--", "hello", "there"]).unwrap();
    assert_eq!(args.positionals, vec!["@luna"]);
    assert_eq!(args.message, vec!["hello", "there"]);
}

#[test]
fn parse_multiple_targets() {
    let args = SendArgs::try_parse_from(["send", "@luna", "@nova", "--", "hello"]).unwrap();
    assert_eq!(args.positionals, vec!["@luna", "@nova"]);
    assert_eq!(args.message, vec!["hello"]);
}

#[test]
fn parse_broadcast() {
    let args = SendArgs::try_parse_from(["send", "--", "broadcast", "msg"]).unwrap();
    assert!(args.positionals.is_empty());
    assert_eq!(args.message, vec!["broadcast", "msg"]);
}

#[test]
fn parse_with_intent_flag() {
    let args =
        SendArgs::try_parse_from(["send", "--intent", "request", "@luna", "--", "hello"]).unwrap();
    assert_eq!(args.intent.as_deref(), Some("request"));
    assert_eq!(args.positionals, vec!["@luna"]);
}

#[test]
fn parse_flags_after_targets() {
    let args =
        SendArgs::try_parse_from(["send", "@luna", "--intent", "request", "--", "hello"]).unwrap();
    assert_eq!(args.intent.as_deref(), Some("request"));
    assert_eq!(args.positionals, vec!["@luna"]);
}

#[test]
fn parse_bigboss_flag() {
    let args = SendArgs::try_parse_from(["send", "-b", "--", "hello"]).unwrap();
    assert!(args.bigboss);
    assert_eq!(args.sender_name(), Some("bigboss".to_string()));
}

#[test]
fn parse_from_overrides_bigboss() {
    let args =
        SendArgs::try_parse_from(["send", "-b", "--from", "reviewer", "--", "hello"]).unwrap();
    assert_eq!(args.sender_name(), Some("reviewer".to_string()));
}

#[test]
fn parse_stdin_flag() {
    let args = SendArgs::try_parse_from(["send", "--stdin", "@luna"]).unwrap();
    assert!(args.stdin);
    assert_eq!(args.positionals, vec!["@luna"]);
    assert!(args.message.is_empty());
}

#[test]
fn parse_file_flag() {
    let args = SendArgs::try_parse_from(["send", "--file", "/tmp/msg.txt", "@luna"]).unwrap();
    assert_eq!(args.file.as_deref(), Some("/tmp/msg.txt"));
    assert_eq!(args.positionals, vec!["@luna"]);
}

#[test]
fn parse_base64_flag() {
    let args = SendArgs::try_parse_from(["send", "--base64", "aGVsbG8=", "@luna"]).unwrap();
    assert_eq!(args.base64.as_deref(), Some("aGVsbG8="));
}

#[test]
fn parse_reply_to_and_thread() {
    let args = SendArgs::try_parse_from([
        "send",
        "--reply-to",
        "42",
        "--thread",
        "pr-99",
        "@luna",
        "--",
        "hi",
    ])
    .unwrap();
    assert_eq!(args.reply_to.as_deref(), Some("42"));
    assert_eq!(args.thread.as_deref(), Some("pr-99"));
}

#[test]
fn parse_quiet_flag() {
    let args = SendArgs::try_parse_from(["send", "--quiet", "-b", "--", "hi"]).unwrap();
    assert!(args.quiet);
}

#[test]
fn parse_inline_bundle_flags() {
    let args = SendArgs::try_parse_from([
        "send",
        "-b",
        "--title",
        "my-bundle",
        "--description",
        "desc",
        "--events",
        "1-10",
        "--files",
        "a.py",
        "--transcript",
        "1-5:normal",
        "--",
        "msg",
    ])
    .unwrap();
    assert_eq!(args.title.as_deref(), Some("my-bundle"));
    assert_eq!(args.description.as_deref(), Some("desc"));
    assert_eq!(args.events.as_deref(), Some("1-10"));
    assert_eq!(args.files.as_deref(), Some("a.py"));
    assert_eq!(args.transcript.as_deref(), Some("1-5:normal"));
}

#[test]
fn parse_no_separator_targets_only() {
    let args = SendArgs::try_parse_from(["send", "@luna"]).unwrap();
    assert_eq!(args.positionals, vec!["@luna"]);
    assert!(args.message.is_empty());
}

#[test]
fn parse_compat_at_name_with_space() {
    // Backward compat: '@luna hi' as a single quoted arg
    // process_positionals treats as full message text
    let args = SendArgs::try_parse_from(["send", "@luna hi"]).unwrap();
    assert_eq!(args.positionals, vec!["@luna hi"]);
}

#[test]
fn parse_bare_text_accepted() {
    // Bare text without @ is accepted by clap; cmd_send handles as message
    let args = SendArgs::try_parse_from(["send", "hello everyone"]).unwrap();
    assert_eq!(args.positionals, vec!["hello everyone"]);
}

#[test]
fn parse_empty_target_rejected() {
    let result = SendArgs::try_parse_from(["send", "@", "--", "hi"]);
    assert!(result.is_err());
}

#[test]
fn parse_bare_text_with_separator_accepted() {
    // Bare text before -- is accepted by clap; validated in cmd_send
    let args = SendArgs::try_parse_from(["send", "luna", "--", "hi"]).unwrap();
    assert_eq!(args.positionals, vec!["luna"]);
    assert_eq!(args.message, vec!["hi"]);
}

#[test]
fn parse_message_with_dashes() {
    let args =
        SendArgs::try_parse_from(["send", "@luna", "--", "--this", "is", "a", "message"]).unwrap();
    assert_eq!(args.message, vec!["--this", "is", "a", "message"]);
}

#[test]
fn resolve_message_strips_redundant_trailing_auto_name_tokens() {
    let mut args =
        SendArgs::try_parse_from(["send", "@luna", "--", "message", "--name", "beru"]).unwrap();
    args.had_separator = true;

    assert_eq!(resolve_message(&args, Some("beru")).unwrap().0, "message");
}

#[test]
fn resolve_message_keeps_quoted_name_suffix_inside_one_argument() {
    let mut args = SendArgs::try_parse_from([
        "send",
        "@luna",
        "--",
        "I always end commands with --name beru",
    ])
    .unwrap();
    args.had_separator = true;

    assert_eq!(
        resolve_message(&args, Some("beru")).unwrap().0,
        "I always end commands with --name beru"
    );
}

#[test]
fn resolve_message_keeps_trailing_name_for_different_sender() {
    let mut args =
        SendArgs::try_parse_from(["send", "@luna", "--", "message", "--name", "nova"]).unwrap();
    args.had_separator = true;

    assert_eq!(
        resolve_message(&args, Some("beru")).unwrap().0,
        "message --name nova"
    );
}

#[test]
fn parse_extends_flag() {
    let args = SendArgs::try_parse_from([
        "send",
        "-b",
        "--title",
        "t",
        "--extends",
        "abc123",
        "--",
        "msg",
    ])
    .unwrap();
    assert_eq!(args.extends.as_deref(), Some("abc123"));
}

// ── process_positionals tests ──

#[test]
fn process_empty() {
    let (targets, msg) = process_positionals(&[]);
    assert!(targets.is_empty());
    assert!(msg.is_none());
}

fn setup_test_db() -> (HcomDb, PathBuf, TestEnv) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    // send_message() reaches the process-global relay notification path,
    // so its ambient HCOM_DIR must live as long as the test DB.
    let env = crate::hooks::test_helpers::isolated_test_env();
    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_send_{}_{}.db",
        std::process::id(),
        test_id
    ));

    let db = HcomDb::open_at(&db_path).unwrap();
    (db, db_path, env)
}

fn cleanup_test_db(path: PathBuf) {
    let _ = std::fs::remove_file(&path);
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    let shm = PathBuf::from(format!("{}-shm", path.display()));
    let _ = std::fs::remove_file(wal);
    let _ = std::fs::remove_file(shm);
}

#[test]
#[serial]
fn send_message_threads_seed_and_reuse_memberships() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0), ('miso', 1000.0)",
            [],
        )
        .unwrap();

    let sender = SenderIdentity {
        kind: SenderKind::Instance,
        name: "luna".into(),
        instance_data: None,
        session_id: None,
    };
    let envelope = MessageEnvelope {
        thread: Some("debate-1".into()),
        ..Default::default()
    };

    let (_, delivered) = send_message(
        &db,
        &sender,
        "hello",
        Some(&envelope),
        Some(&["nova".to_string(), "miso".to_string()]),
    )
    .unwrap();
    assert_eq!(delivered, vec!["nova".to_string(), "miso".to_string()]);

    let members = db.get_thread_members("debate-1");
    assert_eq!(
        members,
        vec!["nova".to_string(), "miso".to_string(), "luna".to_string()]
    );

    let (_, delivered) = send_message(&db, &sender, "round 2", Some(&envelope), None).unwrap();
    assert_eq!(delivered, vec!["nova".to_string(), "miso".to_string()]);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn send_message_thread_without_members_errors() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    let sender = SenderIdentity {
        kind: SenderKind::Instance,
        name: "luna".into(),
        instance_data: None,
        session_id: None,
    };
    let envelope = MessageEnvelope {
        thread: Some("empty-thread".into()),
        ..Default::default()
    };

    let err = send_message(&db, &sender, "hello", Some(&envelope), None).unwrap_err();
    assert!(err.contains("has no members"));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn send_message_external_sender_does_not_auto_subscribe_to_thread() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let sender = SenderIdentity {
        kind: SenderKind::External,
        name: "bigboss".into(),
        instance_data: None,
        session_id: None,
    };
    let envelope = MessageEnvelope {
        thread: Some("ops".into()),
        ..Default::default()
    };

    let (_, delivered) = send_message(
        &db,
        &sender,
        "hello",
        Some(&envelope),
        Some(&["nova".to_string()]),
    )
    .unwrap();
    assert_eq!(delivered, vec!["nova".to_string()]);
    assert_eq!(db.get_thread_members("ops"), vec!["nova".to_string()]);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn send_message_thread_request_does_not_create_request_watch_rows() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0), ('nova', 1000.0)",
            [],
        )
        .unwrap();

    let sender = SenderIdentity {
        kind: SenderKind::Instance,
        name: "luna".into(),
        instance_data: None,
        session_id: None,
    };
    let seed_envelope = MessageEnvelope {
        thread: Some("ops".into()),
        ..Default::default()
    };
    send_message(
        &db,
        &sender,
        "seed",
        Some(&seed_envelope),
        Some(&["nova".to_string()]),
    )
    .unwrap();

    let request_envelope = MessageEnvelope {
        intent: Some(crate::messages::MessageIntent::Request),
        thread: Some("ops".into()),
        ..Default::default()
    };
    let (_, delivered) =
        send_message(&db, &sender, "status?", Some(&request_envelope), None).unwrap();
    assert_eq!(delivered, vec!["nova".to_string()]);

    let reqwatch_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM kv WHERE key LIKE 'events_sub:reqwatch-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reqwatch_count, 0);

    let (scope, mentions_json): (String, String) = db
        .conn()
        .query_row(
            "SELECT json_extract(data, '$.scope'), json_extract(data, '$.mentions')
             FROM events
             WHERE type = 'message'
             ORDER BY id DESC
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(scope, "mentions");
    assert!(mentions_json.contains("nova"));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn send_mention_excludes_inactive_instances() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at)
             VALUES ('luna', 'listening', '', 1000.0),
                    ('vine', 'inactive', 'exit:unknown', 1000.0)",
            [],
        )
        .unwrap();

    let sender = SenderIdentity {
        kind: SenderKind::Instance,
        name: "luna".into(),
        instance_data: None,
        session_id: None,
    };

    let err = send_message(&db, &sender, "ping", None, Some(&["vine".to_string()])).unwrap_err();
    assert!(err.contains("@vine"), "err={err}");
    assert!(err.contains("Available:"), "err={err}");
    assert!(
        err.contains("luna"),
        "listening agent should be listed: {err}"
    );
    let available_line = err.lines().find(|l| l.contains("Available:")).unwrap_or("");
    assert!(
        !available_line.contains("vine"),
        "inactive agent must not appear in Available: {err}"
    );

    cleanup_test_db(path);
}

#[test]
#[serial]
fn send_mention_excludes_exit_no_tool_call_inactive() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at)
             VALUES ('luna', 'listening', '', 1000.0),
                    ('dove', 'inactive', 'exit:no_tool_call', 1000.0)",
            [],
        )
        .unwrap();

    let sender = SenderIdentity {
        kind: SenderKind::Instance,
        name: "luna".into(),
        instance_data: None,
        session_id: None,
    };

    let err = send_message(&db, &sender, "ping", None, Some(&["dove".to_string()])).unwrap_err();
    assert!(err.contains("@dove"), "err={err}");
    let available_line = err.lines().find(|l| l.contains("Available:")).unwrap_or("");
    assert!(
        !available_line.contains("dove"),
        "soft-stopped agent must not appear in Available: {err}"
    );

    cleanup_test_db(path);
}

#[test]
fn process_compat_at_with_space() {
    // "@luna hi" → full text as message, no targets
    let (targets, msg) = process_positionals(&["@luna hi".to_string()]);
    assert!(targets.is_empty());
    assert_eq!(msg.as_deref(), Some("@luna hi"));
}

#[test]
fn process_bare_text() {
    // "hello everyone" → message text, no targets (broadcast)
    let (targets, msg) = process_positionals(&["hello everyone".to_string()]);
    assert!(targets.is_empty());
    assert_eq!(msg.as_deref(), Some("hello everyone"));
}

#[test]
fn process_pure_targets() {
    // "@luna" → target, no message
    let (targets, msg) = process_positionals(&["@luna".to_string()]);
    assert_eq!(targets, vec!["luna"]);
    assert!(msg.is_none());
}

#[test]
fn process_target_plus_bare_text() {
    // "@luna", "hello" → target + message
    let (targets, msg) = process_positionals(&["@luna".to_string(), "hello".to_string()]);
    assert_eq!(targets, vec!["luna"]);
    assert_eq!(msg.as_deref(), Some("hello"));
}

fn insert_imported_remote_message(
    db: &HcomDb,
    local_id: i64,
    origin_id: serde_json::Value,
    short: &str,
    text: &str,
    thread: Option<&str>,
    intent: Option<&str>,
) {
    let relay_obj = serde_json::json!({
        "id": origin_id,
        "short": short,
        "device": format!("dev-uuid-{short}"),
    });
    let mut data = serde_json::json!({
        "from": format!("remote:{short}"),
        "text": text,
        "_relay": relay_obj,
    });
    if let Some(t) = thread {
        data["thread"] = serde_json::json!(t);
    }
    if let Some(i) = intent {
        data["intent"] = serde_json::json!(i);
    }
    db.conn()
        .execute(
            "INSERT INTO events (id, type, instance, timestamp, data) VALUES (?, 'message', 'remote-inst', '2026-09-04T00:00:00Z', ?)",
            rusqlite::params![local_id, data.to_string()],
        )
        .unwrap();
}

#[test]
#[serial]
fn test_resolve_reply_to_remote_different_local_id() {
    let (db, path, _env) = setup_test_db();
    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!(42),
        "BOXE",
        "remote msg",
        None,
        None,
    );

    let resolved = resolve_reply_to_local(&db, "42:BOXE");
    assert_eq!(resolved, Some(100));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_remote_colliding_local_id() {
    let (db, path, _env) = setup_test_db();
    let local_data = serde_json::json!({ "text": "unrelated local message" });
    db.conn()
        .execute(
            "INSERT INTO events (id, type, instance, timestamp, data) VALUES (42, 'message', 'local-inst', '2026-09-04T00:00:00Z', ?)",
            [local_data.to_string()],
        )
        .unwrap();

    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!(42),
        "BOXE",
        "remote msg",
        None,
        None,
    );

    // 42:BOXE must resolve to the remote event (100), never the colliding local event (42)
    assert_eq!(resolve_reply_to_local(&db, "42:BOXE"), Some(100));
    // local 42 without suffix must resolve to the local event (42)
    assert_eq!(resolve_reply_to_local(&db, "42"), Some(42));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_two_devices_same_origin_id() {
    let (db, path, _env) = setup_test_db();
    insert_imported_remote_message(&db, 100, serde_json::json!(42), "BOXE", "msg 1", None, None);
    insert_imported_remote_message(&db, 200, serde_json::json!(42), "WAVE", "msg 2", None, None);

    assert_eq!(resolve_reply_to_local(&db, "42:BOXE"), Some(100));
    assert_eq!(resolve_reply_to_local(&db, "42:WAVE"), Some(200));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_canonical_device_suffix_grammar() {
    let (db, path, _env) = setup_test_db();
    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!(42),
        "BOXE",
        "remote msg",
        None,
        None,
    );

    // Exact 4-char uppercase ASCII succeeds
    assert_eq!(resolve_reply_to_local(&db, "42:BOXE"), Some(100));

    // Lowercase, mixed-case, prefixes, or non-4-char suffixes must fail closed
    assert_eq!(resolve_reply_to_local(&db, "42:boxe"), None);
    assert_eq!(resolve_reply_to_local(&db, "42:Boxe"), None);
    assert_eq!(resolve_reply_to_local(&db, "42:BO"), None);
    assert_eq!(resolve_reply_to_local(&db, "42:BOX"), None);
    assert_eq!(resolve_reply_to_local(&db, "42:BOXES"), None);
    assert_eq!(resolve_reply_to_local(&db, "42:NOPE"), None);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_rejects_non_integer_json_id() {
    let (db, path, _env) = setup_test_db();
    // Origin ID stored as JSON string instead of JSON integer
    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!("42"),
        "BOXE",
        "malformed",
        None,
        None,
    );

    // Must fail closed because json_type is not 'integer'
    assert_eq!(resolve_reply_to_local(&db, "42:BOXE"), None);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_duplicate_same_short_origin_fails_closed() {
    let (db, path, _env) = setup_test_db();
    insert_imported_remote_message(&db, 100, serde_json::json!(42), "BOXE", "msg 1", None, None);
    insert_imported_remote_message(&db, 101, serde_json::json!(42), "BOXE", "msg 2", None, None);

    // Multiple rows with origin=42 and short=BOXE must fail closed
    assert_eq!(resolve_reply_to_local(&db, "42:BOXE"), None);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_cmd_send_remote_reply_inherits_thread_at_command_boundary() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('sender', 1000.0), ('target', 1000.0)",
            [],
        )
        .unwrap();

    // Remote request at local row 100 with origin 42 and thread "thread-remote-99"
    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!(42),
        "BOXE",
        "remote req",
        Some("thread-remote-99"),
        Some("request"),
    );

    // Execute send through the full cmd_send entrypoint without specifying --thread
    let mut args = SendArgs::try_parse_from([
        "send",
        "@target",
        "--from",
        "sender",
        "--intent",
        "ack",
        "--reply-to",
        "42:BOXE",
        "--",
        "pong message",
    ])
    .unwrap();
    args.had_separator = true;

    let exit_code = cmd_send(&db, &args, None);
    assert_eq!(exit_code, 0, "cmd_send should succeed");

    // Verify sent event data at command boundary
    let (reply_to, reply_to_local, thread): (Option<String>, Option<i64>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT json_extract(data, '$.reply_to'), json_extract(data, '$.reply_to_local'), json_extract(data, '$.thread')
             FROM events WHERE type = 'message' AND id != 100 ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();

    assert_eq!(reply_to.as_deref(), Some("42:BOXE"));
    assert_eq!(reply_to_local, Some(100));
    assert_eq!(thread.as_deref(), Some("thread-remote-99"));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_cmd_send_unresolved_remote_reply_exits_one() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('sender', 1000.0), ('target', 1000.0)",
            [],
        )
        .unwrap();

    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!(42),
        "BOXE",
        "remote req",
        None,
        None,
    );

    let count_before: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();

    // Unknown device
    let mut args_unknown = SendArgs::try_parse_from([
        "send",
        "@target",
        "--from",
        "sender",
        "--reply-to",
        "42:NOPE",
        "--",
        "hi",
    ])
    .unwrap();
    args_unknown.had_separator = true;
    assert_eq!(cmd_send(&db, &args_unknown, None), 1);

    // Non-canonical lowercase device
    let mut args_lower = SendArgs::try_parse_from([
        "send",
        "@target",
        "--from",
        "sender",
        "--reply-to",
        "42:boxe",
        "--",
        "hi",
    ])
    .unwrap();
    args_lower.had_separator = true;
    assert_eq!(cmd_send(&db, &args_lower, None), 1);

    // Non-existent origin ID
    let mut args_nonexistent = SendArgs::try_parse_from([
        "send",
        "@target",
        "--from",
        "sender",
        "--reply-to",
        "999:BOXE",
        "--",
        "hi",
    ])
    .unwrap();
    args_nonexistent.had_separator = true;
    assert_eq!(cmd_send(&db, &args_nonexistent, None), 1);

    let count_after: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_before, count_after);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_cmd_send_ambiguous_remote_reply_exits_one_and_emits_no_message() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('sender', 1000.0), ('target', 1000.0)",
            [],
        )
        .unwrap();

    // Two distinct local rows with identical origin ID (42) and short device ("BOXE")
    insert_imported_remote_message(
        &db,
        101,
        serde_json::json!(42),
        "BOXE",
        "remote msg 1",
        None,
        None,
    );
    insert_imported_remote_message(
        &db,
        102,
        serde_json::json!(42),
        "BOXE",
        "remote msg 2",
        None,
        None,
    );

    let count_before: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();

    let mut args_ambiguous = SendArgs::try_parse_from([
        "send",
        "@target",
        "--from",
        "sender",
        "--reply-to",
        "42:BOXE",
        "--",
        "hi",
    ])
    .unwrap();
    args_ambiguous.had_separator = true;

    assert_eq!(cmd_send(&db, &args_ambiguous, None), 1);

    let count_after: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_before, count_after);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_send_message_remote_ack_loop_prevention_with_differing_ids() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES ('luna', 1000.0)",
            [],
        )
        .unwrap();

    // 1. Remote inform: local id = 110, origin id = 10 (distinct IDs)
    insert_imported_remote_message(
        &db,
        110,
        serde_json::json!(10),
        "BOXE",
        "remote info",
        None,
        Some("inform"),
    );

    let sender = SenderIdentity {
        kind: SenderKind::Instance,
        name: "luna".into(),
        instance_data: None,
        session_id: None,
    };

    let ack_to_inform = MessageEnvelope {
        intent: Some(crate::messages::MessageIntent::Ack),
        reply_to: Some("10:BOXE".into()),
        ..Default::default()
    };

    let err = send_message(&db, &sender, "ack", Some(&ack_to_inform), None).unwrap_err();
    assert!(err.contains("Cannot ack an inform"));

    // 2. Remote ack: local id = 120, origin id = 20 (distinct IDs)
    insert_imported_remote_message(
        &db,
        120,
        serde_json::json!(20),
        "BOXE",
        "remote ack",
        None,
        Some("ack"),
    );

    let ack_to_ack = MessageEnvelope {
        intent: Some(crate::messages::MessageIntent::Ack),
        reply_to: Some("20:BOXE".into()),
        ..Default::default()
    };

    let err = send_message(&db, &sender, "ack", Some(&ack_to_ack), None).unwrap_err();
    assert!(err.contains("Ack-on-ack loop detected"));

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_malformed_tokens_fail_closed() {
    let (db, path, _env) = setup_test_db();
    insert_imported_remote_message(
        &db,
        100,
        serde_json::json!(42),
        "BOXE",
        "remote msg",
        None,
        None,
    );

    assert_eq!(resolve_reply_to_local(&db, ""), None);
    assert_eq!(resolve_reply_to_local(&db, "   "), None);
    assert_eq!(resolve_reply_to_local(&db, "42:"), None);
    assert_eq!(resolve_reply_to_local(&db, ":BOXE"), None);
    assert_eq!(resolve_reply_to_local(&db, "42:BOXE:EXTRA"), None);
    assert_eq!(resolve_reply_to_local(&db, "abc:BOXE"), None);

    cleanup_test_db(path);
}

#[test]
#[serial]
fn test_resolve_reply_to_local_only_unchanged() {
    let (db, path, _env) = setup_test_db();
    let local_data = serde_json::json!({ "text": "local message" });
    db.conn()
        .execute(
            "INSERT INTO events (id, type, instance, timestamp, data) VALUES (42, 'message', 'local-inst', '2026-09-04T00:00:00Z', ?)",
            [local_data.to_string()],
        )
        .unwrap();

    assert_eq!(resolve_reply_to_local(&db, "42"), Some(42));
    assert_eq!(resolve_reply_to_local(&db, "999"), None);
    assert_eq!(resolve_reply_to_local(&db, "notanumber"), None);

    cleanup_test_db(path);
}

fn insert_feedback_recipient(db: &HcomDb, name: &str, status_context: &str) {
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at)
             VALUES (?1, 'listening', ?2, 1000.0)",
            rusqlite::params![name, status_context],
        )
        .unwrap();
}

fn insert_pty_feedback_recipient(db: &HcomDb, name: &str) {
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, status, status_context, created_at, tcp_mode)
             VALUES (?1, 'listening', '', 1000.0, 1)",
            rusqlite::params![name],
        )
        .unwrap();
}

#[test]
#[serial]
fn recipient_feedback_preserves_healthy_success_wording() {
    let (db, path, _env) = setup_test_db();
    let recipients = [
        ("idle", ""),
        ("using-tool", "tool:Bash"),
        ("receiving", "deliver:rune"),
        ("stopped-hook", "stop"),
    ];
    for (name, status_context) in recipients {
        insert_feedback_recipient(&db, name, status_context);
    }

    let delivered_to = recipients
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<Vec<_>>();
    let feedback = get_recipient_feedback(&db, &delivered_to);

    assert_eq!(
        feedback,
        "Sent to: ◉ idle, ◉ using-tool, ◉ receiving, ◉ stopped-hook"
    );
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_preserves_large_healthy_recipient_count() {
    let (db, path, _env) = setup_test_db();
    let recipients: Vec<String> = (0..11).map(|index| format!("agent{index}")).collect();
    for recipient in &recipients {
        insert_feedback_recipient(&db, recipient, "");
    }

    let feedback = get_recipient_feedback(&db, &recipients);

    assert_eq!(feedback, "Sent to 11 agents");
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_warns_for_delivery_paused_status_family() {
    let (db, path, _env) = setup_test_db();
    let recipients = [
        ("not-ready", "tui:not-ready"),
        ("draft", "tui:prompt-has-text"),
        ("wake", "tui:wake-unacknowledged"),
    ];
    for (name, status_context) in recipients {
        insert_feedback_recipient(&db, name, status_context);
    }

    let delivered_to = recipients
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<Vec<_>>();
    let feedback = get_recipient_feedback(&db, &delivered_to);

    assert_eq!(
        feedback,
        "Queued; delivery paused: ◉ not-ready, ◉ draft, ◉ wake"
    );
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_warns_for_approval_block() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at)
             VALUES ('approval', 'blocked', 'pty:approval', 1000.0)",
            [],
        )
        .unwrap();

    let feedback = get_recipient_feedback(&db, &["approval".to_string()]);

    assert_eq!(feedback, "Queued; delivery paused: ■ approval");
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_warns_for_hook_approval_block() {
    let (db, path, _env) = setup_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at)
             VALUES ('hook-approval', 'blocked', 'approval', 1000.0)",
            [],
        )
        .unwrap();

    let feedback = get_recipient_feedback(&db, &["hook-approval".to_string()]);

    assert_eq!(feedback, "Queued; delivery paused: ■ hook-approval");
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_lists_healthy_before_paused_recipients() {
    let (db, path, _env) = setup_test_db();
    insert_feedback_recipient(&db, "paused", "tui:user-active");
    insert_feedback_recipient(&db, "healthy", "");

    let feedback = get_recipient_feedback(&db, &["paused".to_string(), "healthy".to_string()]);

    assert_eq!(
        feedback,
        "Sent to: ◉ healthy\nQueued; delivery paused: ◉ paused"
    );
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_send_message_persists_paused_recipient_once() {
    let (db, path, _env) = setup_test_db();
    insert_feedback_recipient(&db, "paused", "tui:not-ready");
    let sender = SenderIdentity {
        kind: SenderKind::External,
        name: "bigboss".into(),
        instance_data: None,
        session_id: None,
    };

    let (_, delivered) =
        send_message(&db, &sender, "ping", None, Some(&["paused".to_string()])).unwrap();

    assert_eq!(delivered, vec!["paused".to_string()]);
    let message_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(message_count, 1);
    let delivered_to: String = db
        .conn()
        .query_row(
            "SELECT json_extract(data, '$.delivered_to')
             FROM events
             WHERE type = 'message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(delivered_to, "[\"paused\"]");
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_reports_unread_local_pty_as_pending() {
    let (db, path, _env) = setup_test_db();
    for name in ["local", "remote"] {
        insert_pty_feedback_recipient(&db, name);
    }
    db.conn()
        .execute(
            "UPDATE instances SET origin_device_id='other-device' WHERE name='remote'",
            [],
        )
        .unwrap();
    let event = db
        .log_event(
            "message",
            "bigboss",
            &serde_json::json!({
                "from": "bigboss", "scope": "mentions", "text": "probe",
                "mentions": ["local", "remote"], "delivered_to": ["local", "remote"]
            }),
        )
        .unwrap();
    let feedback = get_recipient_feedback(&db, &["local".into(), "remote".into()]);
    assert!(
        feedback.contains("Queued; delivery pending: ◉ local"),
        "{feedback}"
    );
    assert!(
        feedback.contains("Sent to: ◉ remote"),
        "remote consumption is not tracked locally: {feedback}"
    );
    db.conn()
        .execute(
            "UPDATE instances SET last_event_id=?1 WHERE name='local'",
            [event],
        )
        .unwrap();
    assert_eq!(
        get_recipient_feedback(&db, &["local".into()]),
        "Sent to: ◉ local"
    );
    cleanup_test_db(path);
}

#[test]
#[serial]
fn recipient_feedback_collapses_large_paused_broadcast() {
    let (db, path, _env) = setup_test_db();
    let recipients: Vec<String> = (0..11).map(|index| format!("agent{index}")).collect();
    for recipient in &recipients {
        insert_feedback_recipient(&db, recipient, "tui:approval");
    }
    assert_eq!(
        get_recipient_feedback(&db, &recipients),
        "Queued; delivery paused: 11 agents"
    );
    cleanup_test_db(path);
}
