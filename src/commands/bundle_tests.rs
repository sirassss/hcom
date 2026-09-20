use super::*;
use crate::shared::time::now_epoch_f64;
use std::fs;

#[test]
fn test_format_age() {
    assert_eq!(format_age(30), "30s");
    assert_eq!(format_age(90), "1m");
    assert_eq!(format_age(3700), "1h");
    assert_eq!(format_age(90000), "1d");
}

#[test]
fn test_format_event_summary_message() {
    let data = json!({"text": "hello world"});
    let summary = format_event_summary(&data);
    assert!(summary.contains("hello world"));
}

#[test]
fn test_format_event_summary_status() {
    let data = json!({"status": "active", "context": "tool:Write"});
    let summary = format_event_summary(&data);
    assert_eq!(summary, "active:tool:Write");
}

#[test]
fn test_format_event_summary_life() {
    let data = json!({"action": "stopped"});
    let summary = format_event_summary(&data);
    assert_eq!(summary, "stopped");
}

#[test]
fn test_format_event_summary_truncation() {
    let long_text = "x".repeat(100);
    let data = json!({"text": long_text});
    let summary = format_event_summary(&data);
    assert!(summary.len() < 60);
    assert!(summary.ends_with("...\""));
}

// ── Clap sub-struct parse tests ────────────────────────────────

use clap::Parser;

#[test]
fn test_bundle_top_level_passthrough() {
    // Top-level BundleArgs uses trailing_var_arg — accepts anything
    let args = BundleArgs::try_parse_from(["bundle"]).unwrap();
    assert!(args.args.is_empty());

    let args = BundleArgs::try_parse_from(["bundle", "--json", "--last", "5"]).unwrap();
    assert_eq!(args.args, vec!["--json", "--last", "5"]);

    let args = BundleArgs::try_parse_from(["bundle", "bundle:abc123"]).unwrap();
    assert_eq!(args.args, vec!["bundle:abc123"]);
}

#[test]
fn test_bundle_list_parse() {
    let a = BundleListArgs::try_parse_from(["list", "--json", "--last", "5"]).unwrap();
    assert!(a.json);
    assert_eq!(a.last, Some(5));
}

#[test]
fn test_bundle_show_parse() {
    let a = BundleShowArgs::try_parse_from(["show", "bundle:abc123"]).unwrap();
    assert_eq!(a.id, "bundle:abc123");
}

#[test]
fn test_bundle_prepare_parse() {
    let a = BundlePrepareArgs::try_parse_from([
        "prepare",
        "--for",
        "peso",
        "--compact",
        "--last-transcript",
        "10",
    ])
    .unwrap();
    assert_eq!(a.for_agent.as_deref(), Some("peso"));
    assert!(a.compact);
    assert_eq!(a.last_transcript, 10);
}

#[test]
fn test_bundle_create_positional_title() {
    let a = BundleCreateArgs::try_parse_from(["create", "My Bundle", "--description", "A test"])
        .unwrap();
    assert_eq!(a.title_positional.as_deref(), Some("My Bundle"));
    assert_eq!(a.description.as_deref(), Some("A test"));
}

#[test]
fn test_bundle_create_flag_title() {
    let a = BundleCreateArgs::try_parse_from([
        "create",
        "--title",
        "My Bundle",
        "--description",
        "A test",
    ])
    .unwrap();
    assert_eq!(a.title_flag.as_deref(), Some("My Bundle"));
    assert_eq!(a.description.as_deref(), Some("A test"));
}

#[test]
fn test_bundle_list_rejects_bogus() {
    assert!(BundleListArgs::try_parse_from(["list", "--bogus"]).is_err());
}

fn test_db() -> HcomDb {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    std::mem::forget(dir);
    db
}

#[test]
fn test_lookup_bundle_transcript_source_falls_back_to_stopped_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("claude.jsonl");
    let db = test_db();

    let lines = [
        json!({
            "type": "user",
            "timestamp": "2026-04-13T12:00:00Z",
            "message": {
                "content": [{"type": "text", "text": "remember marker"}]
            }
        }),
        json!({
            "type": "assistant",
            "timestamp": "2026-04-13T12:00:01Z",
            "message": {
                "content": [{"type": "text", "text": "ack marker"}]
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

    let snapshot = json!({
        "name": "huno",
        "tool": "claude",
        "session_id": "sess-123",
        "transcript_path": transcript_path.to_string_lossy().to_string(),
        "created_at": now_epoch_f64(),
    });
    db.log_life_event("huno", "stopped", "cli", "killed", Some(snapshot))
        .unwrap();

    let (path, tool, sid) = lookup_bundle_transcript_source(&db, "huno")
        .unwrap()
        .unwrap();
    assert_eq!(
        path.as_deref(),
        Some(transcript_path.to_string_lossy().as_ref())
    );
    assert_eq!(tool, "claude");
    assert_eq!(sid.as_deref(), Some("sess-123"));

    let tq = TranscriptQuery {
        path: path.as_deref().unwrap(),
        agent: &tool,
        last: 10,
        detailed: false,
        session_id: sid.as_deref(),
    };
    let exchanges = get_exchanges_pub(&tq).unwrap();
    assert_eq!(exchanges.len(), 1);
    assert_eq!(exchanges[0]["position"], 1);
}

#[test]
fn test_lookup_bundle_transcript_source_does_not_invent_claude_metadata() {
    let db = test_db();
    assert_eq!(
        lookup_bundle_transcript_source(&db, "missing").unwrap(),
        None
    );
}
