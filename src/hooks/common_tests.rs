use super::*;
use crate::hooks::test_helpers::isolated_test_env;
use serial_test::serial;
use std::io::Write;

#[test]
fn test_check_stdin_closed_does_not_panic() {
    // Verify the function runs without panicking regardless of stdin state.
    // In test context stdin is typically a pipe — check_stdin_closed should
    // return false because POLLHUP (normal pipe EOF) is NOT treated as closed.
    let result = check_stdin_closed();
    // Don't assert specific value — stdin state varies across test runners.
    let _ = result;
}

#[test]
fn test_setup_tcp_notification() {
    let (server, tcp_mode) = setup_tcp_notification("test_instance");
    assert!(tcp_mode);
    assert!(server.is_some());

    let addr = server.as_ref().unwrap().local_addr().unwrap();
    assert!(addr.port() > 0);
}

#[test]
#[serial]
fn test_notify_hook_instance_missing_instance() {
    // Best-effort wake must not panic when the DB opens but the named
    // instance has no row (the common case for a stale notify target).
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    notify_hook_instance("nonexistent");
}

fn make_test_db() -> (tempfile::TempDir, crate::db::HcomDb) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = crate::db::HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    (dir, db)
}

fn insert_test_instance(db: &crate::db::HcomDb, name: &str) {
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, last_event_id)
             VALUES (?1, 'claude', 'listening', 'start', 0, 0, 0)",
            [name],
        )
        .unwrap();
}

fn insert_bound_claude_instance(
    db: &crate::db::HcomDb,
    name: &str,
    session_id: &str,
    transcript_path: &str,
) {
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, tool, session_id, transcript_path, status, status_context, status_time, created_at, last_event_id)
             VALUES (?1, 'claude', ?2, ?3, 'listening', 'start', 0, 0, 0)",
            rusqlite::params![name, session_id, transcript_path],
        )
        .unwrap();
    db.set_session_binding(session_id, name).unwrap();
    db.mark_claude_session_validated(session_id, name).unwrap();
}

fn context_with_process_id(
    cwd: &std::path::Path,
    process_id: Option<&str>,
) -> crate::shared::context::HcomContext {
    let mut env = std::collections::HashMap::new();
    if let Some(process_id) = process_id {
        env.insert("HCOM_PROCESS_ID".to_string(), process_id.to_string());
    }
    crate::shared::context::HcomContext::from_env(&env, cwd.to_path_buf())
}

#[test]
fn transcript_lineage_uses_structured_fork_ancestry() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-original", "");
    let transcript = dir.path().join("fork.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"sessionId\":\"session-new\",\"message\":{\"session_id\":\"session-original\"}}\n",
            "{\"session_id\":\"session-new\"}\n"
        ),
    )
    .unwrap();

    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), Some("session-new"),)
            .unwrap(),
        TranscriptOwnerResolution::Owner("niza".to_string())
    );
}

#[test]
fn transcript_lineage_deduplicates_multiple_records_for_one_owner() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-original", "");
    let transcript = dir.path().join("same-owner.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"sessionId\":\"session-original\"}\n",
            "{\"session_id\":\"session-original\"}\n",
            "{\"message\":{\"session_id\":\"session-original\"}}\n"
        ),
    )
    .unwrap();

    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), None).unwrap(),
        TranscriptOwnerResolution::Owner("niza".to_string())
    );
}

#[test]
fn transcript_lineage_rejects_conflicting_owners() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-niza", "");
    insert_bound_claude_instance(&db, "lava", "session-lava", "");
    let transcript = dir.path().join("conflict.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"sessionId\":\"session-niza\"}\n",
            "{\"message\":{\"session_id\":\"session-lava\"}}\n"
        ),
    )
    .unwrap();

    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), None).unwrap(),
        TranscriptOwnerResolution::Ambiguous(vec!["lava".to_string(), "niza".to_string()])
    );
}

#[test]
fn transcript_lineage_ignores_ids_inside_message_content() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "lava", "session-lava", "");
    let transcript = dir.path().join("quoted.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"content\":\"quoted session_id session-lava\",\"nested\":{\"session_id\":\"session-lava\"}}}\n",
    )
    .unwrap();

    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), None).unwrap(),
        TranscriptOwnerResolution::Unknown
    );
}

#[test]
fn transcript_lineage_handles_missing_and_oversized_files() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-original", "");
    assert_eq!(
        resolve_claude_transcript_owner(
            &db,
            dir.path().join("missing.jsonl").to_str().unwrap(),
            None,
        )
        .unwrap(),
        TranscriptOwnerResolution::Unknown
    );

    let transcript = dir.path().join("oversized.jsonl");
    let mut file = std::fs::File::create(&transcript).unwrap();
    for _ in 0..2048 {
        writeln!(file, "{{\"type\":\"padding\"}}").unwrap();
    }
    writeln!(file, "{{\"sessionId\":\"session-original\"}}").unwrap();
    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), None).unwrap(),
        TranscriptOwnerResolution::Unknown
    );
}

#[test]
fn transcript_lineage_ignores_truncated_utf8_tail() {
    const MAX_BYTES: usize = 512 * 1024;

    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-original", "");
    let transcript = dir.path().join("truncated-utf8.jsonl");
    let mut contents = b"{\"sessionId\":\"session-original\"}\n".to_vec();
    contents.resize(MAX_BYTES - 1, b' ');
    contents.extend_from_slice("€\n".as_bytes());
    std::fs::write(&transcript, contents).unwrap();

    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), None).unwrap(),
        TranscriptOwnerResolution::Owner("niza".to_string())
    );
}

#[test]
fn transcript_lineage_rejects_duplicate_exact_path_owners() {
    let (dir, db) = make_test_db();
    let transcript = dir.path().join("shared.jsonl");
    std::fs::write(&transcript, "").unwrap();
    insert_bound_claude_instance(&db, "niza", "session-niza", transcript.to_str().unwrap());
    insert_bound_claude_instance(&db, "lava", "session-lava", transcript.to_str().unwrap());

    assert_eq!(
        resolve_claude_transcript_owner(&db, transcript.to_str().unwrap(), None).unwrap(),
        TranscriptOwnerResolution::Ambiguous(vec!["lava".to_string(), "niza".to_string()])
    );
}

#[test]
fn hook_context_skips_transcript_scan_when_process_and_session_agree() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-niza", "");
    insert_bound_claude_instance(&db, "lava", "session-lava", "");
    db.set_process_binding("process-niza", "session-niza", "niza")
        .unwrap();
    let transcript = dir.path().join("irrelevant-conflict.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"session_id\":\"session-lava\"}}\n",
    )
    .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-niza"));

    let (owner, _, _) = init_hook_context(&db, &ctx, "session-niza", transcript.to_str().unwrap());
    assert_eq!(owner.as_deref(), Some("niza"));
}

#[test]
fn hook_context_uses_session_owner_with_empty_process_id() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-niza", "");
    let ctx = context_with_process_id(dir.path(), Some(""));

    let (owner, _, _) = init_hook_context(&db, &ctx, "session-niza", "");
    assert_eq!(owner.as_deref(), Some("niza"));
}

#[test]
fn hook_context_prefers_session_owner_over_conflicting_process_owner() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-niza", "");
    insert_bound_claude_instance(&db, "lava", "session-lava", "");
    db.set_process_binding("process-restored", "session-lava", "lava")
        .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-restored"));

    let (owner, _, _) = init_hook_context(&db, &ctx, "session-niza", "");
    assert_eq!(owner.as_deref(), Some("niza"));
}

#[test]
fn hook_context_fails_closed_on_poisoned_session_vs_transcript_ancestry() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-original", "");
    insert_bound_claude_instance(&db, "lava", "session-poisoned", "");
    db.kv_set("claude_lineage_validated:session-poisoned", None)
        .unwrap();
    db.set_process_binding("process-restored", "session-original", "niza")
        .unwrap();
    let transcript = dir.path().join("poisoned.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"session_id\":\"session-original\"}}\n",
    )
    .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-restored"));

    let (owner, _, _) =
        init_hook_context(&db, &ctx, "session-poisoned", transcript.to_str().unwrap());
    assert!(owner.is_none());
}

#[test]
fn hook_context_revalidates_agreeing_but_untrusted_poisoned_binding() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-original", "");
    insert_bound_claude_instance(&db, "lava", "session-poisoned", "");
    db.kv_set("claude_lineage_validated:session-poisoned", None)
        .unwrap();
    db.set_process_binding("process-poisoned", "session-poisoned", "lava")
        .unwrap();
    let transcript = dir.path().join("poisoned-agree.jsonl");
    std::fs::write(
        &transcript,
        "{\"sessionId\":\"session-poisoned\",\"message\":{\"session_id\":\"session-original\"}}\n",
    )
    .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-poisoned"));

    let (owner, _, is_primary) =
        init_hook_context(&db, &ctx, "session-poisoned", transcript.to_str().unwrap());
    assert!(owner.is_none());
    assert!(!is_primary, "ordinary hooks must not promote a generation");
    assert_eq!(
        db.get_instance_full("lava")
            .unwrap()
            .unwrap()
            .status_context,
        "start",
        "rejecting a hook must not mutate the other instance's status"
    );
}

#[test]
fn hook_context_caches_validated_lineage_after_one_scan() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "niza", "session-niza", "");
    db.kv_set("claude_lineage_validated:session-niza", None)
        .unwrap();
    db.set_process_binding("process-niza", "session-niza", "niza")
        .unwrap();
    let transcript = dir.path().join("validate-once.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"session_id\":\"session-niza\"}}\n",
    )
    .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-niza"));

    let (owner, _, is_primary) =
        init_hook_context(&db, &ctx, "session-niza", transcript.to_str().unwrap());
    assert_eq!(owner.as_deref(), Some("niza"));
    assert!(is_primary);
    assert_eq!(
        db.get_validated_claude_session_owner("session-niza")
            .unwrap()
            .as_deref(),
        Some("niza")
    );

    std::fs::write(
        &transcript,
        "{\"message\":{\"session_id\":\"session-other\"}}\n",
    )
    .unwrap();
    let (owner, _, _) = init_hook_context(&db, &ctx, "session-niza", transcript.to_str().unwrap());
    assert_eq!(owner.as_deref(), Some("niza"));
}

#[test]
fn hook_context_rejects_historical_process_fallback_for_unbound_session() {
    let (dir, db) = make_test_db();
    insert_bound_claude_instance(&db, "lava", "session-old", "");
    db.set_process_binding("process-restored", "session-old", "lava")
        .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-restored"));

    let (owner, _, _) = init_hook_context(&db, &ctx, "session-new", "");
    assert!(owner.is_none());
}

#[test]
fn hook_context_keeps_fresh_process_fallback_without_lineage() {
    let (dir, db) = make_test_db();
    insert_test_instance(&db, "fresh");
    db.set_process_binding("process-fresh", "", "fresh")
        .unwrap();
    let ctx = context_with_process_id(dir.path(), Some("process-fresh"));

    let (owner, _, _) = init_hook_context(&db, &ctx, "session-new", "");
    assert_eq!(owner.as_deref(), Some("fresh"));
}

fn insert_test_message(
    db: &crate::db::HcomDb,
    instance: &str,
    from: &str,
    text: &str,
    timestamp: &str,
) {
    let data = serde_json::json!({
        "from": from,
        "text": text,
        "scope": "broadcast",
    })
    .to_string();
    db.conn()
        .execute(
            "INSERT INTO events (type, timestamp, instance, data) VALUES ('message', ?1, ?2, ?3)",
            rusqlite::params![timestamp, instance, data],
        )
        .unwrap();
}

#[test]
fn test_prepare_and_commit_delivery() {
    let (_dir, db) = make_test_db();
    insert_test_instance(&db, "nova");
    insert_test_message(&db, "luna", "luna", "hello", "2026-01-01T00:00:01Z");

    let prepared = prepare_pending_messages(&db, "nova").unwrap();
    assert!(!prepared.formatted.is_empty());
    assert_eq!(prepared.ack.instance_name, "nova");

    // Before commit: cursor not advanced (prepare_raw_messages defers)
    let cursor_before: i64 = db
        .conn()
        .query_row(
            "SELECT last_event_id FROM instances WHERE name = 'nova'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor_before, 0);

    // Commit
    commit_delivery_ack(&db, &prepared.ack);

    // After commit: cursor advanced, status updated
    let cursor_after: i64 = db
        .conn()
        .query_row(
            "SELECT last_event_id FROM instances WHERE name = 'nova'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor_after, prepared.ack.last_event_id);

    let instance = db.get_instance_full("nova").unwrap().unwrap();
    assert_eq!(instance.status, ST_ACTIVE);
    assert!(instance.status_context.starts_with("deliver:"));
}

#[test]
fn test_stop_instance_basic_cleanup() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // Create parent instance
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, status, status_context, status_time, created_at)
         VALUES ('parent', 'claude', 'sess-1', 'active', 'new', 0, 0)",
        [],
    );
    // Add notify endpoint
    let _ = db.conn().execute(
        "INSERT INTO notify_endpoints (instance, kind, port, updated_at) VALUES ('parent', 'pty', 9999, 0)",
        [],
    );
    // Add process binding and Claude actor correlation state
    let _ = db.conn().execute(
        "INSERT INTO process_bindings (process_id, session_id, instance_name, updated_at) VALUES ('proc-1', 'sess-1', 'parent', 0)",
        [],
    );
    let token = db
        .issue_claude_actor_capability("sess-1", "tool-1", None, "parent")
        .unwrap();
    db.kv_set("subagent_stop_inflight:sess-1:a1:x", Some("owner"))
        .unwrap();

    stop_instance(&db, "parent", "test", "test_cleanup");

    // Instance should be deleted
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM instances WHERE name = 'parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "instance should be deleted");

    // Notify endpoints should be deleted
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM notify_endpoints WHERE instance = 'parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "notify endpoints should be deleted");

    // Process bindings should be deleted
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM process_bindings WHERE instance_name = 'parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "process bindings should be deleted");

    assert_eq!(
        db.resolve_claude_actor_capability(&token, "sess-1")
            .unwrap(),
        None,
        "root stop should revoke session actor capabilities"
    );
    let claude_kv: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM kv WHERE key LIKE 'subagent_stop_inflight:sess-1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        claude_kv, 0,
        "root stop should remove Claude actor correlation state"
    );

    // Life event should be logged
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "life event should be logged");
}

#[test]
fn test_stop_instance_recursive_subagent_cleanup() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // Create parent instance with session_id
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, status, status_context, status_time, created_at)
         VALUES ('parent', 'claude', 'sess-parent', 'active', 'new', 0, 0)",
        [],
    );
    // Create subagent linked to parent via parent_session_id
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, parent_session_id, parent_name, status, status_context, status_time, created_at)
         VALUES ('sub1', 'claude', 'sess-sub1', 'sess-parent', 'parent', 'active', 'new', 0, 0)",
        [],
    );
    // Create second subagent
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, parent_session_id, parent_name, status, status_context, status_time, created_at)
         VALUES ('sub2', 'claude', 'sess-sub2', 'sess-parent', 'parent', 'active', 'new', 0, 0)",
        [],
    );

    // Stop parent — should recursively stop subagents
    stop_instance(&db, "parent", "test", "test_recursive");

    // All three instances should be deleted
    let count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM instances", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "all instances (parent + subagents) should be deleted"
    );

    // Life events should be logged for all three
    let count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM events WHERE type = 'life'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 3, "life events for parent + 2 subagents");
}

#[test]
fn test_stop_instance_recursive_depth_2() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // parent → sub1 → subsub1 (depth-2 chain proves real recursion)
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, status, status_context, status_time, created_at)
         VALUES ('parent', 'claude', 'sess-p', 'active', 'running', 0, 0)",
        [],
    );
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, parent_session_id, parent_name, status, status_context, status_time, created_at)
         VALUES ('sub1', 'claude', 'sess-s1', 'sess-p', 'parent', 'active', 'running', 0, 0)",
        [],
    );
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, parent_session_id, parent_name, status, status_context, status_time, created_at)
         VALUES ('subsub1', 'claude', 'sess-ss1', 'sess-s1', 'sub1', 'active', 'running', 0, 0)",
        [],
    );

    stop_instance(&db, "parent", "test", "test_depth2");

    // All three levels should be cleaned up
    let count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM instances", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "all 3 levels should be deleted");

    // Verify life events logged for each level
    let stopped: Vec<String> = db
        .conn()
        .prepare(
            "SELECT instance FROM events WHERE type = 'life' AND data LIKE '%stopped%' ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(stopped.len(), 3, "life events for all 3 levels");
    // subsub1 stopped first (deepest), then sub1, then parent
    assert_eq!(stopped[0], "subsub1");
    assert_eq!(stopped[1], "sub1");
    assert_eq!(stopped[2], "parent");
}

#[test]
fn test_stop_instance_depth_limit() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // Create a chain deeper than MAX_STOP_DEPTH to verify the limit kicks in
    // We'll create 12 levels (limit is 10)
    for i in 0..12u32 {
        let name = format!("inst{}", i);
        let session_id = format!("sess-{}", i);
        let parent_sid = if i == 0 {
            String::new()
        } else {
            format!("sess-{}", i - 1)
        };

        if i == 0 {
            let _ = db.conn().execute(
                "INSERT INTO instances (name, tool, session_id, status, status_context, status_time, created_at)
                 VALUES (?1, 'claude', ?2, 'active', 'running', 0, 0)",
                rusqlite::params![name, session_id],
            );
        } else {
            let parent_name = format!("inst{}", i - 1);
            let _ = db.conn().execute(
                "INSERT INTO instances (name, tool, session_id, parent_session_id, parent_name, status, status_context, status_time, created_at)
                 VALUES (?1, 'claude', ?2, ?3, ?4, 'active', 'running', 0, 0)",
                rusqlite::params![name, session_id, parent_sid, parent_name],
            );
        }
    }

    // Stop root. Hitting the depth guard leaves the full chain retryable;
    // partial deletion would orphan the surviving descendants.
    stop_instance(&db, "inst0", "test", "test_depth_limit");

    let remaining: Vec<String> = db
        .conn()
        .prepare("SELECT name FROM instances ORDER BY CAST(SUBSTR(name, 5) AS INTEGER)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(
        remaining,
        (0..12).map(|i| format!("inst{i}")).collect::<Vec<_>>(),
        "an incomplete child cascade must leave every ancestor retryable"
    );
    let stopped: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stopped, 0, "an incomplete cascade must publish no stops");
}

#[test]
fn test_stop_instance_idempotent() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // Create instance
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, status, status_context, status_time, created_at)
         VALUES ('inst', 'claude', 'active', 'new', 0, 0)",
        [],
    );

    // Stop twice — second call should be a no-op
    stop_instance(&db, "inst", "test", "first");
    stop_instance(&db, "inst", "test", "second");

    // Only one life event
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'inst'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "only one life event for idempotent stop");
}

#[test]
fn test_stop_instance_nonexistent() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // Should be a no-op, not panic
    stop_instance(&db, "nonexistent", "test", "test");
}

#[test]
fn test_stale_stop_cannot_delete_reused_name() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, session_id, agent_id, tool, status, status_context, status_time, created_at)
             VALUES ('inst', 'old-session', 'old-agent', 'claude', 'active', 'running', 0, 1)",
            [],
        )
        .unwrap();
    let old = db.get_instance_full("inst").unwrap().unwrap();
    db.delete_instance("inst").unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, session_id, agent_id, tool, status, status_context, status_time, created_at)
             VALUES ('inst', 'new-session', 'new-agent', 'claude', 'active', 'running', 0, 2)",
            [],
        )
        .unwrap();
    let event = serde_json::json!({"action": "stopped", "snapshot": {"name": "inst"}});

    let won = db
        .finalize_instance_stop(
            "inst",
            old.created_at,
            old.session_id.as_deref(),
            old.agent_id.as_deref(),
            &event,
        )
        .unwrap();
    assert!(!won, "the stale row incarnation must lose its delete CAS");
    let current = db.get_instance_full("inst").unwrap().unwrap();
    assert_eq!(current.session_id.as_deref(), Some("new-session"));
    let stopped: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'inst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stopped, 0, "a stale stopper must publish no event");
}

#[test]
fn test_child_enumeration_error_keeps_parent_retryable() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, session_id, tool, status, status_context, status_time, created_at)
             VALUES ('parent', 'sess-1', 'claude', 'active', 'running', 0, 1)",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
             (name, parent_name, tool, status, status_context, status_time, created_at)
             VALUES (x'80', 'parent', 'claude', 'active', 'running', 0, 2)",
            [],
        )
        .unwrap();

    let outcome = stop_instance(&db, "parent", "test", "child-read-error");
    assert!(matches!(outcome, StopOutcome::RetryableError(_)));
    assert!(db.get_instance("parent").unwrap().is_some());
    let stopped: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stopped, 0);
}

#[test]
fn test_stop_instance_does_not_publish_until_delete_wins() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at)
             VALUES ('inst', 'sess-1', 'claude', 'active', 'running', 0, 0)",
            [],
        )
        .unwrap();
    db.set_session_binding("sess-1", "inst").unwrap();
    // RAISE(IGNORE) makes DELETE report zero affected rows, modeling a
    // contender that lost the teardown ownership CAS.
    db.conn()
        .execute_batch(
            "CREATE TRIGGER suppress_inst_delete BEFORE DELETE ON instances
             WHEN OLD.name = 'inst' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();

    stop_instance(&db, "inst", "test", "first");
    assert!(db.get_instance("inst").unwrap().is_some());
    assert_eq!(
        db.get_session_binding("sess-1").unwrap().as_deref(),
        Some("inst"),
        "a failed ownership delete must leave bindings retryable"
    );
    let stopped: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'inst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stopped, 0, "a losing teardown must not publish stopped");

    db.conn()
        .execute_batch("DROP TRIGGER suppress_inst_delete;")
        .unwrap();

    // Event insertion and deletion form one transaction. If publication
    // fails, SQLite must restore both the row and its binding.
    db.conn()
        .execute_batch(
            "CREATE TRIGGER reject_stopped_event BEFORE INSERT ON events
             WHEN NEW.type = 'life' AND NEW.instance = 'inst'
             BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
        )
        .unwrap();
    stop_instance(&db, "inst", "test", "event-failure");
    assert!(db.get_instance("inst").unwrap().is_some());
    assert_eq!(
        db.get_session_binding("sess-1").unwrap().as_deref(),
        Some("inst"),
        "event failure must roll back deletion and cleanup"
    );
    db.conn()
        .execute_batch("DROP TRIGGER reject_stopped_event;")
        .unwrap();

    stop_instance(&db, "inst", "test", "retry");
    assert!(db.get_instance("inst").unwrap().is_none());
    let stopped: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'inst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stopped, 1, "the retry winner publishes exactly once");
}

#[test]
fn test_finalize_session_calls_stop() {
    crate::config::Config::init();
    let (_dir, db) = make_test_db();

    // Use status_context != "new" to avoid triggering the "ready" life event
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, session_id, status, status_context, status_time, created_at)
         VALUES ('inst', 'claude', 'sess-1', 'active', 'running', 0, 0)",
        [],
    );

    finalize_session(&db, "inst", "user_quit", None);

    // Instance should be deleted
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM instances WHERE name = 'inst'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "finalize_session should delete instance");

    // "stopped" life event logged
    let count: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM events WHERE type = 'life' AND instance = 'inst' AND data LIKE '%stopped%'",
        [], |r| r.get(0)
    ).unwrap();
    assert_eq!(count, 1, "stopped life event should be logged");
}

#[test]
fn test_stale_placeholder_marker_does_not_rebind() {
    crate::config::Config::init();
    let (dir, db) = make_test_db();

    // Simulate a leaked launch placeholder older than the stale threshold.
    let old_time = crate::shared::time::now_epoch_f64()
        - (lifecycle::CLEANUP_PLACEHOLDER_THRESHOLD as f64 + 80.0);
    let _ = db.conn().execute(
        "INSERT INTO instances (name, tool, status, status_context, created_at)
         VALUES ('luna', 'claude', 'pending', 'new', ?1)",
        rusqlite::params![old_time],
    );

    let transcript = dir.path().join("transcript.jsonl");
    std::fs::write(&transcript, "assistant output [hcom:luna]\n").unwrap();

    let ctx = crate::shared::context::HcomContext::from_env(
        &std::collections::HashMap::new(),
        dir.path().to_path_buf(),
    );
    let (instance_name, _updates, _matched_resume) =
        init_hook_context(&db, &ctx, "sess-fresh", transcript.to_str().unwrap());

    assert!(
        instance_name.is_none(),
        "a transcript marker must not establish ownership"
    );

    assert!(
        db.get_instance_full("luna").unwrap().is_some(),
        "unrelated hook must leave the placeholder untouched"
    );

    assert_eq!(
        db.get_session_binding("sess-fresh").unwrap(),
        None,
        "fresh session must not get bound via stale marker"
    );
}

#[test]
fn soft_finalize_session_keeps_instance_row() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = crate::db::HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    let now = chrono::Utc::now().timestamp() as f64;
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, created_at, tool, session_id)
             VALUES ('vine', 'listening', ?1, 'antigravity', 'sess-soft-1')",
            rusqlite::params![now],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO session_bindings (session_id, instance_name, created_at)
             VALUES ('sess-soft-1', 'vine', ?1)",
            rusqlite::params![now],
        )
        .unwrap();

    db.conn()
        .execute(
            "INSERT INTO process_bindings (process_id, session_id, instance_name, updated_at)
             VALUES ('pid-soft', 'sess-soft-1', 'vine', ?1)",
            rusqlite::params![now],
        )
        .unwrap();

    soft_finalize_session(&db, "vine", "unknown", None, false);

    assert!(db.get_instance_full("vine").unwrap().is_some());
    let status = db.get_status("vine").unwrap().map(|(s, _)| s);
    assert_eq!(status.as_deref(), Some(ST_INACTIVE));
    assert_eq!(db.get_session_binding("sess-soft-1").unwrap(), None);
    assert_eq!(
        db.find_stopped_instance_by_session_id("sess-soft-1")
            .unwrap()
            .as_deref(),
        Some("vine")
    );
    assert_eq!(db.get_process_binding("pid-soft").unwrap(), None);
}

#[test]
fn soft_finalize_session_can_keep_process_binding() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = crate::db::HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    let now = chrono::Utc::now().timestamp() as f64;
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, created_at, tool, session_id)
             VALUES ('luna', 'listening', ?1, 'omp', 'sess-keep')",
            rusqlite::params![now],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO session_bindings (session_id, instance_name, created_at)
             VALUES ('sess-keep', 'luna', ?1)",
            rusqlite::params![now],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO process_bindings (process_id, session_id, instance_name, updated_at)
             VALUES ('pid-keep', 'sess-keep', 'luna', ?1)",
            rusqlite::params![now],
        )
        .unwrap();

    soft_finalize_session(&db, "luna", "turn_end", None, true);

    assert_eq!(
        db.get_process_binding("pid-keep").unwrap(),
        Some("luna".to_string())
    );
    assert_eq!(db.get_session_binding("sess-keep").unwrap(), None);
    assert_eq!(
        db.get_status("luna").unwrap().map(|(s, _)| s),
        Some(ST_INACTIVE.to_string())
    );
}
