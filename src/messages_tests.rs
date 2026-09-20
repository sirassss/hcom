use super::*;

// ---- validate_message ----

#[test]
fn test_validate_message_empty() {
    assert_eq!(validate_message(""), Err("Message required".to_string()));
    assert_eq!(validate_message("   "), Err("Message required".to_string()));
}

#[test]
fn test_validate_message_valid() {
    assert!(validate_message("hello world").is_ok());
    assert!(validate_message("line1\nline2\ttab").is_ok());
}

#[test]
fn test_validate_message_control_chars() {
    assert!(validate_message("hello\x00world").is_err());
    assert!(validate_message("hello\x07world").is_err());
}

#[test]
fn test_validate_message_too_large() {
    let big = "x".repeat(MAX_MESSAGE_SIZE + 1);
    assert!(validate_message(&big).is_err());
}

// ---- format_recipients ----

#[test]
fn test_format_recipients_empty() {
    assert_eq!(format_recipients(&[], 30), "(none)");
}

#[test]
fn test_format_recipients_normal() {
    let names = vec!["luna".to_string(), "nova".to_string()];
    assert_eq!(format_recipients(&names, 30), "luna, nova");
}

#[test]
fn test_format_recipients_truncated() {
    let names: Vec<String> = (0..5).map(|i| format!("agent{}", i)).collect();
    let result = format_recipients(&names, 3);
    assert!(result.contains("+2 more"));
}

// ---- validate_scope / validate_intent ----

#[test]
fn test_validate_scope() {
    assert!(validate_scope("broadcast").is_ok());
    assert!(validate_scope("mentions").is_ok());
    assert!(validate_scope("invalid").is_err());
}

#[test]
fn test_validate_intent() {
    assert!(validate_intent("request").is_ok());
    assert!(validate_intent("inform").is_ok());
    assert!(validate_intent("ack").is_ok());
    assert!(validate_intent("invalid").is_err());
}

// ---- match_target ----

fn make_instances(names: &[(&str, Option<&str>)]) -> Vec<InstanceInfo> {
    names.iter().map(|(name, tag)| info(name, *tag)).collect()
}

#[test]
fn test_match_target_exact() {
    let instances = make_instances(&[("luna", None), ("nova", None)]);
    assert_eq!(match_target("luna", &instances).unwrap(), vec!["luna"]);
}

#[test]
fn test_match_target_tagged() {
    let instances = make_instances(&[("luna", Some("api")), ("nova", None)]);
    assert_eq!(match_target("api-luna", &instances).unwrap(), vec!["luna"]);
}

#[test]
fn test_match_target_tag_prefix() {
    let instances = make_instances(&[("luna", Some("api")), ("nova", Some("api")), ("kira", None)]);
    let result = match_target("api-", &instances).unwrap();
    assert!(result.contains(&"luna".to_string()));
    assert!(result.contains(&"nova".to_string()));
    assert!(!result.contains(&"kira".to_string()));
}

#[test]
fn test_match_target_exact_base_name_with_tag() {
    let instances = make_instances(&[("luna", Some("api"))]);
    assert_eq!(match_target("luna", &instances).unwrap(), vec!["luna"]);
}

#[test]
fn test_match_target_exact_name_excludes_prefixes() {
    let instances = make_instances(&[
        ("giru", None),
        ("lasa", Some("giru-test")),
        ("giru2", None),
        ("giru_sub", None),
    ]);
    let result = match_target("giru", &instances).unwrap();
    assert_eq!(result, vec!["giru"]);
}

#[test]
fn test_match_target_rejects_partial_local_name() {
    let instances = make_instances(&[("luna", None), ("lunatic", None)]);
    assert!(match_target("lun", &instances).unwrap().is_empty());
}

#[test]
fn test_match_target_group_requires_exact_tag() {
    let instances = make_instances(&[("luna", Some("api")), ("nova", Some("api-extra"))]);
    let result = match_target("api-", &instances).unwrap();
    assert_eq!(result, vec!["luna"]);
}

#[test]
fn test_match_target_bigboss_remote() {
    let instances = make_instances(&[("luna", None), ("bigboss", None)]);
    assert_eq!(
        match_target("bigboss:BOXE", &instances).unwrap(),
        vec!["bigboss"]
    );
}

#[test]
fn test_match_target_remote_prefix() {
    let instances = make_instances(&[("luna:BOXE", None)]);
    assert_eq!(
        match_target("luna:BO", &instances).unwrap(),
        vec!["luna:BOXE"]
    );
}

#[test]
fn test_match_target_ambiguous_remote_prefix_fails() {
    let instances = make_instances(&[("luna:BOXE", None), ("luna:BOLT", None)]);
    let err = match_target("luna:BO", &instances).unwrap_err();
    assert!(err.contains("Ambiguous remote @mention @luna:BO"));
}

#[test]
fn test_match_target_no_match() {
    let instances = make_instances(&[("luna", None)]);
    assert!(match_target("nonexistent", &instances).unwrap().is_empty());
}

// ---- compute_scope ----

fn info(name: &str, tag: Option<&str>) -> InstanceInfo {
    InstanceInfo {
        name: name.to_string(),
        tag: tag.map(|t| t.to_string()),
    }
}

#[test]
fn test_compute_scope_broadcast() {
    let instances = vec![info("luna", None), info("nova", None)];
    let result = compute_scope("hello everyone", &instances, None).unwrap();
    assert_eq!(result.scope, MessageScope::Broadcast);
    assert!(result.mentions.is_empty());
}

#[test]
fn test_compute_scope_mention_in_text() {
    let instances = vec![info("luna", None), info("nova", None)];
    let result = compute_scope("hey @luna fix this", &instances, None).unwrap();
    assert_eq!(result.scope, MessageScope::Mentions);
    assert_eq!(result.mentions, vec!["luna"]);
}

#[test]
fn test_compute_scope_exact_name_beats_tag_prefix_collision() {
    let instances = vec![info("giru", None), info("lasa", Some("giru-test"))];
    let result = compute_scope("hey @giru", &instances, None).unwrap();
    assert_eq!(result.mentions, vec!["giru"]);
}

#[test]
fn test_compute_scope_explicit_targets() {
    let instances = vec![info("luna", None), info("nova", None)];
    let targets = vec!["luna".to_string()];
    let result = compute_scope("fix this", &instances, Some(&targets)).unwrap();
    assert_eq!(result.scope, MessageScope::Mentions);
    assert_eq!(result.mentions, vec!["luna"]);
}

#[test]
fn test_compute_scope_explicit_empty_broadcast() {
    let instances = vec![info("luna", None)];
    let targets: Vec<String> = vec![];
    let result = compute_scope("hello", &instances, Some(&targets)).unwrap();
    assert_eq!(result.scope, MessageScope::Broadcast);
}

#[test]
fn test_compute_scope_unknown_target_fails() {
    let instances = vec![info("luna", None)];
    let targets = vec!["nonexistent".to_string()];
    let result = compute_scope("hello", &instances, Some(&targets));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("non-existent or stopped"));
}

#[test]
fn test_compute_scope_suggests_remote_match() {
    // Bare `@zeli` shouldn't silently match remote `zeli:ZOME`, but the error
    // should point users at the right form instead of just listing 30 names.
    let instances = vec![info("zeli:ZOME", None), info("luna", None)];
    let targets = vec!["zeli".to_string()];
    let err = compute_scope("hello", &instances, Some(&targets)).unwrap_err();
    assert!(err.contains("@zeli"), "got: {err}");
    assert!(err.contains("Did you mean: @zeli:ZOME"), "got: {err}");
}

#[test]
fn test_compute_scope_no_suggestion_when_no_remote_match() {
    let instances = vec![info("luna", None), info("nova", None)];
    let targets = vec!["zeli".to_string()];
    let err = compute_scope("hello", &instances, Some(&targets)).unwrap_err();
    assert!(!err.contains("Did you mean"), "got: {err}");
}

#[test]
fn test_compute_scope_unknown_mention_fails() {
    let instances = vec![info("luna", None)];
    let result = compute_scope("hey @nonexistent fix this", &instances, None);
    assert!(result.is_err());
}

#[test]
fn test_compute_scope_system_mention_fails() {
    let instances = vec![info("luna", None)];
    let result = compute_scope("hey @[hcom-events]", &instances, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("System notifications"));
}

#[test]
fn test_compute_scope_literal_mention_fails() {
    let instances = vec![info("luna", None)];
    let result = compute_scope("use @mention to target", &instances, None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("literal text @mention is not a valid target")
    );
}

#[test]
fn test_compute_scope_tagged_instances() {
    let instances = vec![info("luna", Some("api")), info("nova", Some("api"))];
    let targets = vec!["api-".to_string()];
    let result = compute_scope("hello", &instances, Some(&targets)).unwrap();
    assert_eq!(result.scope, MessageScope::Mentions);
    assert!(result.mentions.contains(&"luna".to_string()));
    assert!(result.mentions.contains(&"nova".to_string()));
}

#[test]
fn test_compute_scope_deduplicates() {
    let instances = vec![info("luna", Some("api"))];
    // Both api-luna and luna resolve to the same instance
    let targets = vec!["api-luna".to_string(), "luna".to_string()];
    let result = compute_scope("hello", &instances, Some(&targets)).unwrap();
    assert_eq!(result.mentions.len(), 1);
    assert_eq!(result.mentions[0], "luna");
}

// ---- should_deliver_message ----

#[test]
fn test_should_deliver_broadcast() {
    let data = serde_json::json!({"scope": "broadcast", "from": "sender"});
    assert!(should_deliver_message(&data, "receiver", "sender").unwrap());
}

#[test]
fn test_should_deliver_skip_self() {
    let data = serde_json::json!({"scope": "broadcast", "from": "luna"});
    assert!(!should_deliver_message(&data, "luna", "luna").unwrap());
}

#[test]
fn test_should_deliver_mentions_match() {
    let data = serde_json::json!({"scope": "mentions", "mentions": ["luna"]});
    assert!(should_deliver_message(&data, "luna", "nova").unwrap());
}

#[test]
fn test_should_deliver_mentions_no_match() {
    let data = serde_json::json!({"scope": "mentions", "mentions": ["luna"]});
    assert!(!should_deliver_message(&data, "nova", "kira").unwrap());
}

#[test]
fn test_should_deliver_cross_device() {
    let data = serde_json::json!({"scope": "mentions", "mentions": ["luna:BOXE"]});
    // luna matches luna:BOXE after stripping device suffix
    assert!(should_deliver_message(&data, "luna", "nova").unwrap());
}

#[test]
fn test_should_deliver_missing_scope() {
    let data = serde_json::json!({"from": "sender"});
    assert!(should_deliver_message(&data, "receiver", "sender").is_err());
}

// ---- build_message_prefix ----

#[test]
fn test_build_prefix_intent_thread() {
    let msg = serde_json::json!({"intent": "request", "thread": "pr-42", "event_id": 42});
    assert_eq!(build_message_prefix(&msg), "[request:pr-42 #42]");
}

#[test]
fn test_build_prefix_intent_only() {
    let msg = serde_json::json!({"intent": "ack", "event_id": 10});
    assert_eq!(build_message_prefix(&msg), "[ack #10]");
}

#[test]
fn test_build_prefix_thread_only() {
    let msg = serde_json::json!({"thread": "testing", "event_id": 5});
    assert_eq!(build_message_prefix(&msg), "[thread:testing #5]");
}

#[test]
fn test_build_prefix_no_envelope() {
    let msg = serde_json::json!({"event_id": 1});
    assert_eq!(build_message_prefix(&msg), "[new message #1]");
}

#[test]
fn test_build_prefix_remote() {
    let msg = serde_json::json!({"intent": "inform", "_relay": {"short": "BOXE", "id": 42}});
    assert_eq!(build_message_prefix(&msg), "[inform #42:BOXE]");
}

// ---- unescape_bash ----

#[test]
fn test_unescape_bash() {
    assert_eq!(unescape_bash("hello\\!world"), "hello!world");
    assert_eq!(unescape_bash("\\$HOME"), "$HOME");
    assert_eq!(unescape_bash("\\`cmd\\`"), "`cmd`");
    assert_eq!(unescape_bash("say \\\"hello\\\""), "say \"hello\"");
    assert_eq!(unescape_bash("it\\'s"), "it's");
}

#[test]
fn test_unescape_bash_preserves_backslash() {
    // Double backslashes are NOT unescaped
    assert_eq!(unescape_bash("path\\\\to\\\\file"), "path\\\\to\\\\file");
}

// ---- build_message_preview ----

#[test]
fn test_build_message_preview_empty() {
    assert_eq!(build_message_preview("", 60), "<hcom></hcom>");
}

#[test]
fn test_build_message_preview_truncates_at_colon() {
    let formatted = "[request #42] luna → nova: here is a long message";
    let result = build_message_preview(formatted, 60);
    // Should include up to the colon but not the message content
    assert!(result.starts_with("<hcom>"));
    assert!(result.ends_with("</hcom>"));
    assert!(result.contains("[request #42] luna → nova"));
    assert!(!result.contains("here is a long message"));
}

#[test]
fn test_build_message_preview_no_colon() {
    let formatted = "short text";
    let result = build_message_preview(formatted, 60);
    assert_eq!(result, "<hcom>short text</hcom>");
}

// ---- MessageScope / MessageIntent ----

#[test]
fn test_message_scope_roundtrip() {
    assert_eq!(
        MessageScope::Broadcast
            .as_str()
            .parse::<MessageScope>()
            .ok(),
        Some(MessageScope::Broadcast)
    );
    assert_eq!(
        MessageScope::Mentions.as_str().parse::<MessageScope>().ok(),
        Some(MessageScope::Mentions)
    );
    assert!("invalid".parse::<MessageScope>().is_err());
}

#[test]
fn test_message_intent_roundtrip() {
    assert_eq!(
        MessageIntent::Request
            .as_str()
            .parse::<MessageIntent>()
            .ok(),
        Some(MessageIntent::Request)
    );
    assert_eq!(
        MessageIntent::Inform.as_str().parse::<MessageIntent>().ok(),
        Some(MessageIntent::Inform)
    );
    assert_eq!(
        MessageIntent::Ack.as_str().parse::<MessageIntent>().ok(),
        Some(MessageIntent::Ack)
    );
    assert!("invalid".parse::<MessageIntent>().is_err());
}

// ---- format_hook_messages / format_messages_json ----

#[test]
fn test_format_hook_messages_single() {
    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hello there",
        "event_id": 42,
        "delivered_to": ["nova"],
    })];

    let result = format_hook_messages(&msgs, "nova", &|_name| None, &|| String::new(), None);
    assert!(result.contains("luna"));
    assert!(result.contains("nova"));
    assert!(result.contains("hello there"));
    assert!(result.contains("#42"));
}

#[test]
fn test_format_hook_messages_multiple() {
    let msgs = vec![
        serde_json::json!({
            "from": "luna",
            "message": "first",
            "event_id": 1,
            "delivered_to": ["nova"],
        }),
        serde_json::json!({
            "from": "kira",
            "message": "second",
            "event_id": 2,
            "delivered_to": ["nova"],
        }),
    ];

    let result = format_hook_messages(&msgs, "nova", &|_name| None, &|| String::new(), None);
    assert!(result.contains("[2 new messages]"));
    assert!(result.contains("first"));
    assert!(result.contains("second"));
}

#[test]
fn test_format_hook_messages_with_hints() {
    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hi",
        "event_id": 1,
        "delivered_to": ["nova"],
    })];

    let result = format_hook_messages(
        &msgs,
        "nova",
        &|_name| None,
        &|| "respond with hcom send".to_string(),
        None,
    );
    assert!(result.contains("[respond with hcom send]"));
}

#[test]
fn test_format_messages_json_wraps_in_tags() {
    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hi",
        "event_id": 1,
        "delivered_to": ["nova"],
    })];

    let result = format_messages_json(&msgs, "nova", &|_name| None, &|| String::new(), None);
    assert!(result.starts_with("<hcom>"));
    assert!(result.ends_with("</hcom>"));
}

#[test]
fn test_format_hook_messages_appends_recv_tip_once() {
    use std::cell::Cell;
    use std::rc::Rc;

    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hi",
        "event_id": 1,
        "intent": "request",
        "delivered_to": ["nova"],
    })];
    let marks = Rc::new(Cell::new(0));
    let tip_checker = |_: &str, _: &str| -> (bool, Box<dyn Fn()>) {
        let marks = Rc::clone(&marks);
        let mark = Box::new(move || marks.set(marks.get() + 1)) as Box<dyn Fn()>;
        (false, mark)
    };

    let result = format_hook_messages(
        &msgs,
        "nova",
        &|_name| None,
        &|| String::new(),
        Some(&tip_checker),
    );
    assert!(result.contains("[tip] intent=request: Sender expects a response."));
    assert_eq!(marks.get(), 1);
}

#[test]
fn test_format_messages_json_marks_tip_without_duplicate_text() {
    use std::cell::Cell;
    use std::rc::Rc;

    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hi",
        "event_id": 1,
        "intent": "request",
        "delivered_to": ["nova"],
    })];
    let seen = Rc::new(Cell::new(false));
    let tip_checker = |_: &str, _: &str| -> (bool, Box<dyn Fn()>) {
        let seen = Rc::clone(&seen);
        let already_seen = seen.get();
        let mark = Box::new(move || seen.set(true)) as Box<dyn Fn()>;
        (already_seen, mark)
    };

    let first = format_messages_json(
        &msgs,
        "nova",
        &|_name| None,
        &|| String::new(),
        Some(&tip_checker),
    );
    let second = format_messages_json(
        &msgs,
        "nova",
        &|_name| None,
        &|| String::new(),
        Some(&tip_checker),
    );
    assert!(first.contains("[tip] intent=request: Sender expects a response."));
    assert!(!second.contains("[tip] intent=request: Sender expects a response."));
}

#[test]
fn test_format_hook_messages_with_others() {
    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hi",
        "event_id": 1,
        "delivered_to": ["nova", "kira", "miso"],
    })];

    let result = format_hook_messages(&msgs, "nova", &|_name| None, &|| String::new(), None);
    // Should show "+2 others" for single message
    assert!(result.contains("+2 others"));
}

#[test]
fn test_format_hook_messages_appends_thread_tip_once() {
    use std::cell::Cell;
    use std::rc::Rc;

    let msgs = vec![serde_json::json!({
        "from": "luna",
        "message": "hi",
        "thread": "debate-1",
        "event_id": 1,
        "delivered_to": ["nova"],
    })];
    let marks = Rc::new(Cell::new(0));
    let tip_checker = |_: &str, tip_key: &str| -> (bool, Box<dyn Fn()>) {
        assert_eq!(tip_key, "recv:thread:debate-1");
        let marks = Rc::clone(&marks);
        let mark = Box::new(move || marks.set(marks.get() + 1)) as Box<dyn Fn()>;
        (false, mark)
    };

    let result = format_hook_messages(
        &msgs,
        "nova",
        &|_name| None,
        &|| String::new(),
        Some(&tip_checker),
    );
    assert!(result.contains("[tip] You joined thread debate-1."));
    assert!(result.contains("hcom events unsub sub-"));
    assert_eq!(marks.get(), 1);
}

// ---- compute_read_receipts ----

#[test]
fn test_compute_read_receipts_basic() {
    let sent = vec![(
        42_i64,
        "2024-01-01T00:00:00Z".to_string(),
        serde_json::json!({
            "scope": "broadcast",
            "text": "hello world",
            "delivered_to": ["nova", "kira"],
        }),
    )];

    let active: HashMap<String, Value> = HashMap::from([
        (
            "nova".to_string(),
            serde_json::json!({"tag": null, "session_id": "sess-1"}),
        ),
        (
            "kira".to_string(),
            serde_json::json!({"tag": null, "session_id": "sess-2"}),
        ),
    ]);

    let mut deliver_events = HashMap::new();
    let mut delivered = HashSet::new();
    delivered.insert("nova".to_string());
    deliver_events.insert(42_i64, delivered);

    let receipts = compute_read_receipts(
        &sent,
        &active,
        &deliver_events,
        &HashMap::new(),
        50,
        &|secs| format!("{}s", secs as i64),
        100.0,
        &|_ts| Some(0.0),
    );

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].id, 42);
    assert_eq!(receipts[0].read_by, vec!["nova"]);
    assert_eq!(receipts[0].total_recipients, 2);
}

#[test]
fn test_compute_read_receipts_remote() {
    let sent = vec![(
        42_i64,
        "2024-01-01T00:00:00Z".to_string(),
        serde_json::json!({
            "scope": "broadcast",
            "text": "hello",
            "delivered_to": ["luna:BOXE"],
        }),
    )];

    let active: HashMap<String, Value> = HashMap::from([(
        "luna:BOXE".to_string(),
        serde_json::json!({"origin_device_id": "device-1"}),
    )]);

    let remote_ts: HashMap<String, String> = HashMap::from([(
        "luna:BOXE".to_string(),
        "2024-01-02T00:00:00Z".to_string(), // After message
    )]);

    let receipts = compute_read_receipts(
        &sent,
        &active,
        &HashMap::new(),
        &remote_ts,
        50,
        &|_| "1h".to_string(),
        100.0,
        &|_| Some(0.0),
    );

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].read_by, vec!["luna:BOXE"]);
}

#[test]
fn test_compute_read_receipts_external_sender_gating() {
    // External sender (no session_id) should only count as read if @mentioned
    let sent = vec![(
        42_i64,
        "2024-01-01T00:00:00Z".to_string(),
        serde_json::json!({
            "scope": "broadcast",
            "text": "hello everyone",  // No @mention of watcher
            "delivered_to": ["nova", "watcher"],
        }),
    )];

    let active: HashMap<String, Value> = HashMap::from([
        (
            "nova".to_string(),
            serde_json::json!({"tag": null, "session_id": "sess-1"}),
        ),
        // External sender: no session_id → should be gated
        ("watcher".to_string(), serde_json::json!({"tag": null})),
    ]);

    let mut deliver_events = HashMap::new();
    let mut delivered = HashSet::new();
    delivered.insert("nova".to_string());
    delivered.insert("watcher".to_string());
    deliver_events.insert(42_i64, delivered);

    let receipts = compute_read_receipts(
        &sent,
        &active,
        &deliver_events,
        &HashMap::new(),
        50,
        &|secs| format!("{}s", secs as i64),
        100.0,
        &|_ts| Some(0.0),
    );

    assert_eq!(receipts.len(), 1);
    // nova has session_id → counted as read
    // watcher has no session_id (external) and not @mentioned → NOT counted
    assert_eq!(receipts[0].read_by, vec!["nova"]);
    assert_eq!(receipts[0].total_recipients, 2);
}

#[test]
fn test_compute_read_receipts_external_sender_mentioned() {
    // External sender IS @mentioned → should count as read
    let sent = vec![(
        42_i64,
        "2024-01-01T00:00:00Z".to_string(),
        serde_json::json!({
            "scope": "mentions",
            "text": "hey @watcher check this",
            "mentions": ["watcher"],
            "delivered_to": ["watcher"],
        }),
    )];

    let active: HashMap<String, Value> = HashMap::from([
        ("watcher".to_string(), serde_json::json!({"tag": null})), // External
    ]);

    let mut deliver_events = HashMap::new();
    let mut delivered = HashSet::new();
    delivered.insert("watcher".to_string());
    deliver_events.insert(42_i64, delivered);

    let receipts = compute_read_receipts(
        &sent,
        &active,
        &deliver_events,
        &HashMap::new(),
        50,
        &|secs| format!("{}s", secs as i64),
        100.0,
        &|_ts| Some(0.0),
    );

    assert_eq!(receipts.len(), 1);
    // watcher is external but was @mentioned → counted as read
    assert_eq!(receipts[0].read_by, vec!["watcher"]);
}

#[test]
fn test_compute_read_receipts_uses_canonical_mentions_not_text_prefixes() {
    let sent = vec![(
        42_i64,
        "2024-01-01T00:00:00Z".to_string(),
        serde_json::json!({
            "scope": "mentions",
            "text": "hey @giru check this",
            "mentions": ["giru"],
            "delivered_to": ["giru", "lasa"],
        }),
    )];

    let active: HashMap<String, Value> = HashMap::from([
        (
            "giru".to_string(),
            serde_json::json!({"session_id": "sess-1"}),
        ),
        ("lasa".to_string(), serde_json::json!({"tag": "giru-test"})),
    ]);
    let deliver_events = HashMap::from([(
        42_i64,
        HashSet::from(["giru".to_string(), "lasa".to_string()]),
    )]);

    let receipts = compute_read_receipts(
        &sent,
        &active,
        &deliver_events,
        &HashMap::new(),
        50,
        &|_| "1s".to_string(),
        100.0,
        &|_| Some(0.0),
    );

    assert_eq!(receipts[0].read_by, vec!["giru"]);
}

#[test]
fn test_is_external_sender_data() {
    // Normal instance with session_id → not external
    assert!(!is_external_sender_data(
        &serde_json::json!({"session_id": "sess-1"})
    ));

    // External: no session_id
    assert!(is_external_sender_data(&serde_json::json!({"tag": null})));
    assert!(is_external_sender_data(
        &serde_json::json!({"session_id": ""})
    ));

    // Remote: has origin_device_id → not external
    assert!(!is_external_sender_data(
        &serde_json::json!({"origin_device_id": "dev-1"})
    ));

    // Subagent: has parent_session_id → not external
    assert!(!is_external_sender_data(
        &serde_json::json!({"parent_session_id": "parent-sess"})
    ));
}
