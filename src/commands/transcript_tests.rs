use super::*;
use crate::transcript::ToolUse;
use crate::transcript::shared::finalize_action_text;
use std::fs;

fn test_db() -> HcomDb {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    std::mem::forget(dir);
    db
}

#[test]
fn known_agent_without_transcript_points_to_transport_history() {
    let db = test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, transcript_path, tool) \
         VALUES ('pita', 100.0, '', 'adhoc')",
            [],
        )
        .unwrap();

    let error = no_transcript_error(&db, "pita", "pita", None);
    assert_eq!(
        error,
        "No model transcript is registered for pita.\n\
View transport messages with: hcom events --participant pita --type message"
    );
    assert!(!error.contains("no messages have been exchanged"));
}

#[test]
fn remote_agent_without_transcript_queries_its_origin_device() {
    let db = test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, transcript_path, tool) \
             VALUES ('pita', 100.0, '', 'adhoc')",
            [],
        )
        .unwrap();

    let error = no_transcript_error(&db, "pita", "pita:ABCD", Some("ABCD"));
    assert_eq!(
        error,
        "No model transcript is registered for pita:ABCD.\n\
View transport messages with: hcom events --remote-fetch --device ABCD --participant pita --type message"
    );
}

#[test]
fn test_parse_range() {
    assert_eq!(parse_range("5"), (Some(5), Some(5)));
    assert_eq!(parse_range("3-10"), (Some(3), Some(10)));
    assert_eq!(parse_range("abc"), (None, None));
}

#[test]
fn test_centered_snippet_shows_match_deep_in_long_line() {
    // A whole-JSON transcript line: the match sits far past the start, where
    // start-anchored truncation would never reach it.
    let prefix = "{\"parentUuid\":null,".repeat(40); // long metadata head
    let line = format!("{prefix}\"text\":\"NEEDLE here\"}}");
    let col = line.find("NEEDLE").unwrap() + 1; // rg is 1-based
    let snip = centered_snippet(&line, col, SEARCH_SNIPPET_WIDTH);
    assert!(snip.contains("NEEDLE"), "match must be visible: {snip}");
    assert!(snip.starts_with('…'), "elided head marked: {snip}");
    assert!(snip.len() <= SEARCH_SNIPPET_WIDTH + 8); // window + ellipses/trim slack
}

#[test]
fn test_centered_snippet_short_line_returned_whole() {
    let line = "{\"text\":\"hi ping there\"}";
    let col = line.find("ping").unwrap() + 1;
    let snip = centered_snippet(line, col, SEARCH_SNIPPET_WIDTH);
    assert_eq!(snip, line, "short line kept intact, no ellipsis");
}

#[test]
fn test_centered_snippet_multibyte_safe() {
    // Match adjacent to multi-byte chars; slicing must not panic and must
    // stay on char boundaries.
    let line = format!("{}émoji→NEEDLE←café{}", "🚀".repeat(60), "ü".repeat(60));
    let col = line.find("NEEDLE").unwrap() + 1;
    let snip = centered_snippet(&line, col, SEARCH_SNIPPET_WIDTH);
    assert!(snip.contains("NEEDLE"));
    assert!(std::str::from_utf8(snip.as_bytes()).is_ok());
}

#[test]
fn test_parse_match_line_rg_with_column() {
    // rg --column format: LINE:COL:TEXT — COL points at the match.
    let head = "x".repeat(300);
    let text = format!("{head}FINDME{head}");
    let col = 301; // 1-based, right after the 300-char head
    let raw = format!("44:{col}:{text}");
    let (line, snip) = parse_match_line(&raw, true, "FINDME");
    assert_eq!(line, 44);
    assert!(snip.contains("FINDME"), "centered on rg column: {snip}");
}

#[test]
fn test_parse_match_line_grep_fallback_locates_pattern() {
    // grep format: LINE:TEXT (no column) — pattern located case-insensitively.
    let head = "y".repeat(300);
    let text = format!("{head}findME{head}");
    let raw = format!("7:{text}");
    let (line, snip) = parse_match_line(&raw, false, "FINDME");
    assert_eq!(line, 7);
    assert!(
        snip.contains("findME"),
        "grep fallback centers on match: {snip}"
    );
}

#[test]
fn test_summarize_action() {
    let short = "Hello world";
    assert_eq!(summarize_action(short), "Hello world");

    let multi = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5";
    let result = summarize_action(multi);
    assert!(result.contains("Line 1"));
    assert!(result.ends_with("..."));
}

#[test]
fn test_detect_agent_type() {
    assert_eq!(
        detect_agent_type("/home/user/.claude/projects/x/transcript.jsonl"),
        "claude"
    );
    assert_eq!(
        detect_agent_type("/home/user/.gemini/tmp/project/chats/session-1-abc.json"),
        "gemini"
    );
    assert_eq!(
        detect_agent_type("/home/user/.codex/sessions/x/rollout.jsonl"),
        "codex"
    );
    assert_eq!(
        detect_agent_type("/home/user/.local/share/opencode/opencode.db"),
        "opencode"
    );
    assert_eq!(
        detect_agent_type("/home/user/.local/share/kilo/kilo.db"),
        "kilo"
    );
    assert_eq!(
        detect_agent_type("/home/user/Library/Application Support/Antigravity/session.jsonl"),
        "antigravity"
    );
    assert_eq!(
        detect_agent_type("/home/user/.copilot/session-state/abc/events.jsonl"),
        "copilot"
    );
}

#[test]
fn detect_agent_type_covers_released_integrations_with_transcript_parsers() {
    let cases = [
        ("/home/user/.claude/projects/x/transcript.jsonl", "claude"),
        (
            "/home/user/.gemini/tmp/project/chats/session-1-abc.json",
            "gemini",
        ),
        ("/home/user/.codex/sessions/x/rollout.jsonl", "codex"),
        ("/home/user/.local/share/opencode/opencode.db", "opencode"),
        ("/home/user/.local/share/kilo/kilo.db", "kilo"),
        (
            "/home/user/Library/Application Support/Antigravity/session.jsonl",
            "antigravity",
        ),
        (
            "/home/user/.cursor/projects/x/agent-transcripts/abc/abc.jsonl",
            "cursor",
        ),
        (
            "/home/user/.kimi-code/sessions/wd_x/abc123/agents/main/wire.jsonl",
            "kimi",
        ),
        (
            "/home/user/.copilot/session-state/abc/events.jsonl",
            "copilot",
        ),
        ("/home/user/.pi/agent/sessions/x/20260603_abc.jsonl", "pi"),
        ("/home/user/.omp/agent/sessions/x/20260603_abc.jsonl", "omp"),
    ];
    let expected: std::collections::HashSet<&str> = crate::integration_spec::released_tool_names()
        .into_iter()
        .collect();
    let actual: std::collections::HashSet<&str> = cases
        .iter()
        .map(|(path, expected_tool)| {
            let detected = detect_agent_type(path);
            assert_eq!(detected, *expected_tool);
            detected
        })
        .collect();

    assert_eq!(
        actual, expected,
        "transcript path detection cases must cover every released integration"
    );
}

#[test]
fn attribute_disk_match_uses_provenance_for_unsignatured_pi_sessions() {
    let pi_root = PathBuf::from("/data/pi-sessions");
    let gem_root = PathBuf::from("/home/u/.gemini");
    let owners = vec![
        (pi_root.clone(), Tool::Pi),
        // gemini and antigravity share one root — the ambiguous case.
        (gem_root.clone(), Tool::Gemini),
        (gem_root.clone(), Tool::Antigravity),
    ];
    let selected = [Tool::Pi, Tool::Gemini, Tool::Antigravity];

    // A bare uuid.jsonl under a custom PI_CODING_AGENT_SESSION_DIR has no
    // content signature, so it is attributed by provenance.
    assert_eq!(
        attribute_disk_match("/data/pi-sessions/abc/9f8e.jsonl", &selected, &owners),
        Some(Tool::Pi)
    );
    // A signatured gemini file under the shared root resolves by content.
    assert_eq!(
        attribute_disk_match(
            "/home/u/.gemini/tmp/p/chats/session-1-x.json",
            &selected,
            &owners
        ),
        Some(Tool::Gemini)
    );
    // An unsignatured file under the shared gemini/antigravity root is
    // ambiguous by provenance and must not be guessed.
    assert_eq!(
        attribute_disk_match("/home/u/.gemini/tmp/p/notes.jsonl", &selected, &owners),
        None
    );
    // Provenance only counts roots for selected tools.
    assert_eq!(
        attribute_disk_match("/data/pi-sessions/abc/9f8e.jsonl", &[Tool::Gemini], &owners),
        None
    );
}

#[test]
fn detect_agent_type_cursor_keys_on_agent_transcripts_not_dotcursor() {
    // Regression: a Claude transcript path with a LITERAL `.cursor` segment
    // (the CLAUDE_CONFIG_DIR-style vector the old `.contains(".cursor")`
    // matcher WOULD have misrouted to cursor) must detect claude. Feeds
    // resume tool detection → wrong match would break resume + parser.
    assert_eq!(
        detect_agent_type("/home/u/.claude/projects/x/.cursor/abcd.jsonl"),
        "claude"
    );
    // A real cursor transcript (the `agent-transcripts` segment) detects cursor.
    assert_eq!(
        detect_agent_type("/home/u/.cursor/projects/repo/agent-transcripts/uuid/uuid.jsonl"),
        "cursor"
    );
}

#[test]
fn test_correlate_paths_to_hcom_uses_session_id_for_opencode() {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_raw(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute_batch(
            "CREATE TABLE instances (
                 name text,
                 transcript_path text,
                 session_id text
             );
             CREATE TABLE events (
                 id integer PRIMARY KEY,
                 type text,
                 instance text,
                 data text
             );",
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, transcript_path, session_id) VALUES (?, ?, ?)",
            rusqlite::params!["luna", "/tmp/opencode.db", "ses_a"],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, transcript_path, session_id) VALUES (?, ?, ?)",
            rusqlite::params!["nova", "/tmp/opencode.db", "ses_b"],
        )
        .unwrap();

    let correlated = correlate_paths_to_hcom(
        &db,
        &[
            ("/tmp/opencode.db".to_string(), Some("ses_a".to_string())),
            ("/tmp/opencode.db".to_string(), Some("ses_b".to_string())),
            ("/tmp/file.jsonl".to_string(), None),
        ],
    );

    assert_eq!(
        correlated.get(&transcript_search_key("/tmp/opencode.db", Some("ses_a"))),
        Some(&"luna".to_string())
    );
    assert_eq!(
        correlated.get(&transcript_search_key("/tmp/opencode.db", Some("ses_b"))),
        Some(&"nova".to_string())
    );
}

#[test]
fn test_transcript_display_for_tool_only_and_error_turns() {
    let tools = vec![ToolUse {
        name: "Bash".to_string(),
        is_error: false,
        file: None,
        command: Some("pwd".to_string()),
        output: None,
    }];
    assert_eq!(
        finalize_action_text("", &tools, &[], false),
        "(tool-only turn: Bash)"
    );

    let errors = vec![json!({"tool": "Bash", "content": "Exit code: 1"})];
    assert_eq!(
        finalize_action_text("", &tools, &errors, true),
        "(turn ended in error after using Bash)"
    );
}

#[test]
fn test_render_instance_transcript_with_options_range_matches_cli_shape() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("rollout.jsonl");
    let db = test_db();
    let now = crate::shared::time::now_epoch_f64();
    let lines = [
        json!({
            "type": "response_item",
            "timestamp": "2026-03-27T10:00:00Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "first user"}]
            }
        }),
        json!({
            "type": "response_item",
            "timestamp": "2026-03-27T10:00:01Z",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "first answer"}]
            }
        }),
        json!({
            "type": "response_item",
            "timestamp": "2026-03-27T10:01:00Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "second user"}]
            }
        }),
        json!({
            "type": "response_item",
            "timestamp": "2026-03-27T10:01:01Z",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "second answer"}]
            }
        }),
    ];
    fs::write(
        &transcript_path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let mut data = serde_json::Map::new();
    data.insert("created_at".into(), json!(now));
    data.insert("tool".into(), json!("codex"));
    data.insert(
        "transcript_path".into(),
        json!(transcript_path.to_string_lossy().to_string()),
    );
    db.save_instance_named("luna", &data).unwrap();

    let rendered =
        render_instance_transcript_with_options(&db, "luna", Some("2"), 10, false, false, false)
            .unwrap();

    assert!(rendered.contains("Recent conversation (1 exchanges, 2-2 of 2) - @luna:"));
    assert!(rendered.contains("second user"));
    assert!(rendered.contains("second answer"));
    assert!(!rendered.contains("first user"));
}

#[test]
fn test_render_instance_transcript_with_options_json_contract() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("rollout.jsonl");
    let db = test_db();
    let now = crate::shared::time::now_epoch_f64();
    let lines = [
        json!({
            "type": "response_item",
            "timestamp": "2026-03-27T10:00:00Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "user prompt"}]
            }
        }),
        json!({
            "type": "response_item",
            "timestamp": "2026-03-27T10:00:01Z",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "assistant answer"}]
            }
        }),
    ];
    fs::write(
        &transcript_path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let mut data = serde_json::Map::new();
    data.insert("created_at".into(), json!(now));
    data.insert("tool".into(), json!("codex"));
    data.insert(
        "transcript_path".into(),
        json!(transcript_path.to_string_lossy().to_string()),
    );
    db.save_instance_named("luna", &data).unwrap();

    let rendered =
        render_instance_transcript_with_options(&db, "luna", None, 10, true, false, false).unwrap();
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    let first = parsed.as_array().unwrap().first().unwrap();

    assert_eq!(first["position"], 1);
    assert_eq!(first["user"], "user prompt");
    assert_eq!(first["action"], "assistant answer");
}

#[test]
fn test_render_antigravity_transcript_user_input_and_planner_response() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("Antigravity-session.jsonl");
    let db = test_db();
    let now = crate::shared::time::now_epoch_f64();
    let lines = [
        json!({
            "type": "USER_INPUT",
            "timestamp": "2026-03-27T10:00:00Z",
            "text": "review the hook changes"
        }),
        json!({
            "type": "PLANNER_RESPONSE",
            "timestamp": "2026-03-27T10:00:01Z",
            "text": "I will inspect the Antigravity hook path."
        }),
    ];
    fs::write(
        &transcript_path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let mut data = serde_json::Map::new();
    data.insert("created_at".into(), json!(now));
    data.insert("tool".into(), json!("antigravity"));
    data.insert(
        "transcript_path".into(),
        json!(transcript_path.to_string_lossy().to_string()),
    );
    db.save_instance_named("vibo", &data).unwrap();

    let rendered =
        render_instance_transcript_with_options(&db, "vibo", None, 10, false, false, false)
            .unwrap();

    assert!(rendered.contains("review the hook changes"));
    assert!(rendered.contains("inspect the Antigravity hook path."));
    assert!(!rendered.contains("No exchanges found"));
}

#[test]
fn test_finalize_action_text_uses_final_error_state_only() {
    let tools = vec![
        ToolUse {
            name: "Edit".to_string(),
            is_error: true,
            file: Some("a.rs".to_string()),
            command: None,
            output: None,
        },
        ToolUse {
            name: "Edit".to_string(),
            is_error: false,
            file: Some("a.rs".to_string()),
            command: None,
            output: None,
        },
    ];
    let errors = vec![json!({"tool": "Edit", "content": "old failure"})];
    assert_eq!(
        finalize_action_text("", &tools, &errors, false),
        "(tool-only turn: Edit)"
    );
}

// ── Clap parse tests ─────────────────────────────────────────────

use clap::Parser;

#[test]
fn test_transcript_view_basic() {
    let args = TranscriptArgs::try_parse_from(["transcript", "peso"]).unwrap();
    assert!(args.subcmd.is_none());
    assert_eq!(args.name.as_deref(), Some("peso"));
    assert!(!args.json);
}

#[test]
fn test_transcript_view_with_flags() {
    let args =
        TranscriptArgs::try_parse_from(["transcript", "@peso", "--json", "--full", "--last", "5"])
            .unwrap();
    assert_eq!(args.name.as_deref(), Some("@peso"));
    assert!(args.json);
    assert!(args.full);
    assert_eq!(args.last, Some(5));
}

#[test]
fn test_transcript_view_range() {
    let args = TranscriptArgs::try_parse_from(["transcript", "peso", "3-10"]).unwrap();
    assert_eq!(args.name.as_deref(), Some("peso"));
    assert_eq!(args.range_positional.as_deref(), Some("3-10"));
}

#[test]
fn test_transcript_search() {
    let args = TranscriptArgs::try_parse_from([
        "transcript",
        "search",
        "error",
        "--live",
        "--limit",
        "50",
    ])
    .unwrap();
    match args.subcmd {
        Some(TranscriptSubcmd::Search(ref s)) => {
            assert_eq!(s.pattern, "error");
            assert!(s.live);
            assert_eq!(s.limit, 50);
        }
        _ => panic!("expected Search subcommand"),
    }
}

#[test]
fn test_transcript_timeline() {
    let args = TranscriptArgs::try_parse_from(["transcript", "timeline", "--json", "--last", "3"])
        .unwrap();
    match args.subcmd {
        Some(TranscriptSubcmd::Timeline(ref t)) => {
            assert!(t.json);
            assert_eq!(t.last, Some(3));
        }
        _ => panic!("expected Timeline subcommand"),
    }
}

#[test]
fn test_transcript_rejects_bogus() {
    assert!(TranscriptArgs::try_parse_from(["transcript", "--bogus"]).is_err());
}

#[test]
fn missing_search_tool_is_an_error_not_an_empty_result() {
    let err = run_search_tool("__hcom_definitely_missing_search_tool__", &["pattern"]).unwrap_err();
    assert!(err.contains("was not found on PATH"));
}

fn insert_test_instance(db: &HcomDb, name: &str, transcript_path: &str, tool: &str) {
    let mut data = serde_json::Map::new();
    data.insert(
        "created_at".into(),
        json!(crate::shared::time::now_epoch_f64()),
    );
    data.insert("tool".into(), json!(tool));
    data.insert("transcript_path".into(), json!(transcript_path));
    db.save_instance_named(name, &data).unwrap();
}

#[test]
fn test_resolve_instance_transcript_literal_underscore_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p_testa = dir.path().join("testa1.jsonl");
    fs::write(&p_testa, "").unwrap();
    insert_test_instance(&db, "testa1", p_testa.to_str().unwrap(), "codex");

    // Literal '_' in requested prefix must not match 'testa1' via SQL LIKE wildcard
    let res = resolve_instance_transcript(&db, "test_");
    assert_eq!(
        res, None,
        "literal '_' must not match 'testa1' via SQL wildcard"
    );
}

#[test]
fn test_resolve_instance_transcript_literal_percent_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p_fooa = dir.path().join("fooa1.jsonl");
    fs::write(&p_fooa, "").unwrap();
    insert_test_instance(&db, "fooa1", p_fooa.to_str().unwrap(), "codex");

    // Literal '%' in requested prefix must not match 'fooa1' via SQL LIKE wildcard
    let res = resolve_instance_transcript(&db, "foo%");
    assert_eq!(
        res, None,
        "literal '%' must not match 'fooa1' via SQL wildcard"
    );
}

#[test]
fn test_resolve_instance_transcript_literal_backslash_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p_slash = dir.path().join("esc_slash.jsonl");
    fs::write(&p_slash, "").unwrap();
    insert_test_instance(&db, "esc\\1", p_slash.to_str().unwrap(), "codex");

    // Literal backslash in prefix must resolve literal instance 'esc\1'
    let res = resolve_instance_transcript(&db, "esc\\");
    assert_eq!(
        res.as_ref().map(|(n, _, _, _)| n.as_str()),
        Some("esc\\1"),
        "literal backslash prefix 'esc\\' must resolve to 'esc\\1'"
    );
}

#[test]
fn test_resolve_instance_transcript_ambiguous_prefix_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p1 = dir.path().join("ambig_one.jsonl");
    let p2 = dir.path().join("ambig_two.jsonl");
    fs::write(&p1, "").unwrap();
    fs::write(&p2, "").unwrap();

    insert_test_instance(&db, "ambig_one", p1.to_str().unwrap(), "codex");
    insert_test_instance(&db, "ambig_two", p2.to_str().unwrap(), "codex");

    // Multiple candidates matching prefix must fail closed (return None)
    // rather than picking an arbitrary candidate based on SQLite row order
    let resolved = resolve_instance_transcript(&db, "ambig_");
    assert_eq!(
        resolved, None,
        "multiple prefix candidates must fail closed to avoid disclosing the wrong transcript"
    );
}

#[test]
fn test_resolve_instance_transcript_exact_match_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p_exact = dir.path().join("exact.jsonl");
    let p_longer = dir.path().join("exact_one.jsonl");
    fs::write(&p_exact, "").unwrap();
    fs::write(&p_longer, "").unwrap();

    insert_test_instance(&db, "exact", p_exact.to_str().unwrap(), "codex");
    insert_test_instance(&db, "exact_one", p_longer.to_str().unwrap(), "codex");

    // Exact match must win immediately, without triggering prefix ambiguity
    let resolved = resolve_instance_transcript(&db, "exact");
    assert!(resolved.is_some(), "exact match must resolve");
    let (name, path, tool, _) = resolved.unwrap();
    assert_eq!(name, "exact");
    assert_eq!(path, p_exact.to_str().unwrap());
    assert_eq!(tool, "codex");
}

#[test]
fn test_resolve_instance_transcript_exact_live_without_transcript_blocks_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, transcript_path, tool) VALUES ('pico', 1.0, '', 'adhoc')",
            [],
        )
        .unwrap();
    let p_longer = dir.path().join("pico_worker.jsonl");
    fs::write(&p_longer, "").unwrap();
    insert_test_instance(&db, "pico_worker", p_longer.to_str().unwrap(), "codex");

    assert_eq!(
        resolve_instance_transcript(&db, "pico"),
        None,
        "an exact live identity without a transcript must not borrow a prefix match"
    );
}

#[test]
fn test_resolve_instance_transcript_exact_stopped_preempts_unique_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p_longer = dir.path().join("pico_worker.jsonl");
    let p_stopped = dir.path().join("pico_stopped.jsonl");
    fs::write(&p_longer, "").unwrap();
    fs::write(&p_stopped, "").unwrap();
    insert_test_instance(&db, "pico_worker", p_longer.to_str().unwrap(), "codex");
    db.conn()
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES (?1, 'life', 'pico', ?2)",
            rusqlite::params![
                "2026-03-27T10:00:00Z",
                json!({
                    "action": "stopped",
                    "snapshot": {
                        "transcript_path": p_stopped.to_str().unwrap(),
                        "session_id": "sess-pico-stopped"
                    }
                })
                .to_string()
            ],
        )
        .unwrap();

    let resolved = resolve_instance_transcript(&db, "pico").unwrap();
    assert_eq!(resolved.0, "pico");
    assert_eq!(resolved.1, p_stopped.to_str().unwrap());
    assert_eq!(resolved.3.as_deref(), Some("sess-pico-stopped"));
}

#[test]
fn test_resolve_instance_transcript_exact_live_without_transcript_blocks_stale_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, transcript_path, tool) VALUES ('pico', 1.0, '', 'adhoc')",
            [],
        )
        .unwrap();
    let p_stopped = dir.path().join("pico_stopped.jsonl");
    fs::write(&p_stopped, "").unwrap();
    db.conn()
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES (?1, 'life', 'pico', ?2)",
            rusqlite::params![
                "2026-03-27T10:00:00Z",
                json!({
                    "action": "stopped",
                    "snapshot": {"transcript_path": p_stopped.to_str().unwrap()}
                })
                .to_string()
            ],
        )
        .unwrap();

    assert_eq!(
        resolve_instance_transcript(&db, "pico"),
        None,
        "a current exact identity must not inherit an older incarnation's transcript"
    );
}

#[test]
fn test_resolve_instance_transcript_stopped_instance_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let session_dir = dir.path().join(".codex/sessions/sess-stopped-123");
    fs::create_dir_all(&session_dir).unwrap();
    let p_stopped = session_dir.join("rollout.jsonl");
    fs::write(&p_stopped, "").unwrap();

    // Insert a stopped life event into the events table
    db.conn()
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES (?1, 'life', ?2, ?3)",
            rusqlite::params![
                "2026-03-27T10:00:00Z",
                "miso",
                json!({
                    "action": "stopped",
                    "snapshot": {
                        "transcript_path": p_stopped.to_str().unwrap(),
                        "session_id": "sess-stopped-123"
                    }
                })
                .to_string()
            ],
        )
        .unwrap();

    let res = resolve_instance_transcript(&db, "miso");
    assert!(
        res.is_some(),
        "stopped instance should resolve via events table fallback"
    );
    let (name, path, tool, sid) = res.unwrap();
    assert_eq!(name, "miso");
    assert_eq!(path, p_stopped.to_str().unwrap());
    assert_eq!(tool, "codex");
    assert_eq!(sid.as_deref(), Some("sess-stopped-123"));
}

#[test]
fn test_resolve_instance_transcript_single_literal_prefix_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    let p_sole = dir.path().join("sole_worker.jsonl");
    fs::write(&p_sole, "").unwrap();
    insert_test_instance(&db, "sole_worker", p_sole.to_str().unwrap(), "codex");

    // A single unambiguous literal prefix must resolve to its sole candidate
    let res = resolve_instance_transcript(&db, "sole_");
    assert!(
        res.is_some(),
        "unambiguous single prefix match should resolve"
    );
    let (name, path, tool, _) = res.unwrap();
    assert_eq!(name, "sole_worker");
    assert_eq!(path, p_sole.to_str().unwrap());
    assert_eq!(tool, "codex");
}

#[test]
fn test_resolve_instance_transcript_stopped_instance_preempts_ambiguous_prefix_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let db = test_db();

    // Two active prefix candidates starting with "pico_"
    let p_active1 = dir.path().join("pico_one.jsonl");
    let p_active2 = dir.path().join("pico_two.jsonl");
    fs::write(&p_active1, "").unwrap();
    fs::write(&p_active2, "").unwrap();
    insert_test_instance(&db, "pico_one", p_active1.to_str().unwrap(), "codex");
    insert_test_instance(&db, "pico_two", p_active2.to_str().unwrap(), "codex");

    // One stopped instance whose exact name matches input "pico"
    let session_dir = dir.path().join(".codex/sessions/sess-pico-stopped");
    fs::create_dir_all(&session_dir).unwrap();
    let p_stopped = session_dir.join("rollout.jsonl");
    fs::write(&p_stopped, "").unwrap();

    db.conn()
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES (?1, 'life', ?2, ?3)",
            rusqlite::params![
                "2026-03-27T10:00:00Z",
                "pico",
                json!({
                    "action": "stopped",
                    "snapshot": {
                        "transcript_path": p_stopped.to_str().unwrap(),
                        "session_id": "sess-pico-stopped"
                    }
                })
                .to_string()
            ],
        )
        .unwrap();

    let res = resolve_instance_transcript(&db, "pico");
    assert!(
        res.is_some(),
        "exact stopped instance must resolve when active prefix candidates are ambiguous"
    );
    let (name, path, tool, sid) = res.unwrap();
    assert_eq!(
        name, "pico",
        "must resolve stopped instance, not an active prefix candidate"
    );
    assert_eq!(path, p_stopped.to_str().unwrap());
    assert_eq!(tool, "codex");
    assert_eq!(sid.as_deref(), Some("sess-pico-stopped"));
}
