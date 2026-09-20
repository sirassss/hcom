use super::*;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

// ===== expand_shortcuts =====

#[test]
fn test_expand_idle() {
    let result = expand_shortcuts(&s(&["--idle", "peso"]));
    assert_eq!(result, s(&["--agent", "peso", "--status", "listening"]));
}

#[test]
fn test_expand_blocked() {
    let result = expand_shortcuts(&s(&["--blocked", "peso"]));
    assert_eq!(result, s(&["--agent", "peso", "--status", "blocked"]));
}

#[test]
fn test_expand_passthrough() {
    let result = expand_shortcuts(&s(&["--last", "20", "--collision"]));
    assert_eq!(result, s(&["--last", "20", "--collision"]));
}

// ===== parse_event_flags =====

#[test]
fn test_parse_agent_flag() {
    let (filters, remaining) = parse_event_flags(&s(&["--agent", "peso", "--last", "20"])).unwrap();
    assert_eq!(filters["instance"], vec!["peso"]);
    assert_eq!(remaining, s(&["--last", "20"]));
}

#[test]
fn test_parse_collision_boolean() {
    let (filters, _) = parse_event_flags(&s(&["--collision"])).unwrap();
    assert!(filters.contains_key("collision"));
}

#[test]
fn test_parse_missing_value() {
    let result = parse_event_flags(&s(&["--agent"]));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("requires a value"));
}

#[test]
fn test_parse_multiple_same_flag() {
    let (filters, _) = parse_event_flags(&s(&["--agent", "peso", "--agent", "luna"])).unwrap();
    assert_eq!(filters["instance"], vec!["peso", "luna"]);
}

// ===== validate_type_constraints =====

#[test]
fn test_validate_no_conflict() {
    let mut filters = FilterMap::new();
    filters.insert("status".into(), vec!["listening".into()]);
    filters.insert("context".into(), vec!["tool:Write".into()]);
    assert!(validate_type_constraints(&filters).is_ok());
}

#[test]
fn test_validate_conflict() {
    let mut filters = FilterMap::new();
    filters.insert("status".into(), vec!["listening".into()]);
    filters.insert("from".into(), vec!["bigboss".into()]);
    let err = validate_type_constraints(&filters).unwrap_err();
    assert!(err.contains("Cannot combine"));
}

// ===== build_sql_from_flags =====

#[test]
fn test_build_empty() {
    assert_eq!(build_sql_from_flags(&FilterMap::new()).unwrap(), "");
}

#[test]
fn test_build_instance_status() {
    let mut filters = FilterMap::new();
    filters.insert("instance".into(), vec!["peso".into()]);
    filters.insert("status".into(), vec!["listening".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("instance = 'peso'"));
    assert!(sql.contains("type = 'status'"));
    assert!(sql.contains("status_val = 'listening'"));
}

#[test]
fn test_build_multi_instance() {
    let mut filters = FilterMap::new();
    filters.insert("instance".into(), vec!["peso".into(), "luna".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("instance IN ('peso', 'luna')"));
}

#[test]
fn test_build_participant_matches_sender_and_recipient() {
    let mut filters = FilterMap::new();
    filters.insert("participant".into(), vec!["pita".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("type = 'message'"));
    assert!(sql.contains("instance = 'pita'"));
    assert!(sql.contains("json_each(msg_delivered_to) WHERE value = 'pita'"));
}

#[test]
fn test_build_cmd_exact() {
    let mut filters = FilterMap::new();
    filters.insert("cmd".into(), vec!["=git status".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("status_detail = 'git status'"));
    assert!(sql.contains(SHELL_TOOL_CONTEXTS));
}

#[test]
fn test_build_cmd_starts_with() {
    let mut filters = FilterMap::new();
    filters.insert("cmd".into(), vec!["^git".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("status_detail LIKE 'git%'"));
}

#[test]
fn test_build_cmd_contains_dollar_literal() {
    // $ is treated as literal in contains — no ends-with semantics
    let mut filters = FilterMap::new();
    filters.insert("cmd".into(), vec!["pattern$".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("status_detail LIKE '%pattern$%'"));
}

#[test]
fn test_build_cmd_contains_default() {
    let mut filters = FilterMap::new();
    filters.insert("cmd".into(), vec!["npm install".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("status_detail LIKE '%npm install%'"));
}

#[test]
fn test_build_file_glob() {
    let mut filters = FilterMap::new();
    filters.insert("file".into(), vec!["*.py".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("status_detail LIKE '%.py'"));
    assert!(sql.contains(FILE_WRITE_CONTEXTS));
}

#[test]
fn test_build_context_glob() {
    let mut filters = FilterMap::new();
    filters.insert("context".into(), vec!["tool:*".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("status_context LIKE 'tool:%'"));
}

#[test]
fn test_build_time_range() {
    let mut filters = FilterMap::new();
    filters.insert("after".into(), vec!["2024-01-01T00:00:00Z".into()]);
    filters.insert("before".into(), vec!["2024-12-31T23:59:59Z".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("timestamp >= '2024-01-01T00:00:00Z'"));
    assert!(sql.contains("timestamp < '2024-12-31T23:59:59Z'"));
}

#[test]
fn test_build_collision() {
    let mut filters = FilterMap::new();
    filters.insert("collision".into(), vec!["true".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("EXISTS"));
    assert!(sql.contains("ABS(strftime"));
}

fn sql_context_list_contains(list: &str, operation: &str) -> bool {
    list.contains(&format!("'tool:{operation}'"))
}

#[test]
fn test_activity_contexts_cover_integration_specs() {
    for spec in crate::integration_spec::ALL {
        for operation in spec.status_detail.file {
            let context = format!("tool:{operation}");
            assert!(
                sql_context_list_contains(FILE_WRITE_CONTEXTS, operation),
                "missing file-write context {context} for {}",
                spec.name
            );
            assert!(
                FILE_OP_CONTEXTS.contains(&context.as_str()),
                "missing file-op context {context} for {}",
                spec.name
            );
        }
        for operation in spec.status_detail.bash {
            assert!(
                sql_context_list_contains(SHELL_TOOL_CONTEXTS, operation),
                "missing shell context tool:{operation} for {}",
                spec.name
            );
        }
    }

    assert!(sql_context_list_contains(
        FILE_WRITE_CONTEXTS,
        "NotebookEdit"
    ));
    assert!(FILE_OP_CONTEXTS.contains(&"tool:NotebookEdit"));
}

#[test]
fn test_collision_filter_matches_real_writes_and_rejects_empty_details() {
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();

    let insert = |instance: &str, timestamp: &str, context: &str, detail: Option<&str>| {
        let data = serde_json::json!({
            "status": "active",
            "context": context,
            "detail": detail,
        });
        db.conn()
            .execute(
                "INSERT INTO events (timestamp, type, instance, data) VALUES (?1, 'status', ?2, ?3)",
                rusqlite::params![timestamp, instance, data.to_string()],
            )
            .unwrap();
    };

    insert(
        "luna",
        "2026-06-07T12:00:00Z",
        "tool:StrReplace",
        Some("src/main.rs"),
    );
    insert(
        "nova",
        "2026-06-07T12:00:10Z",
        "tool:create",
        Some("src/main.rs"),
    );
    insert(
        "solo",
        "2026-06-07T12:00:15Z",
        "tool:write_to_file",
        Some("src/solo.rs"),
    );
    insert("empty-a", "2026-06-07T12:00:20Z", "tool:Write", Some(""));
    insert("empty-b", "2026-06-07T12:00:21Z", "tool:Edit", Some(""));
    insert("null-a", "2026-06-07T12:00:22Z", "tool:Write", None);
    insert("null-b", "2026-06-07T12:00:23Z", "tool:Edit", None);

    let mut filters = FilterMap::new();
    filters.insert("collision".into(), vec!["true".into()]);
    let where_sql = build_sql_from_flags(&filters).unwrap();
    let query = format!("SELECT instance FROM events_v WHERE {where_sql} ORDER BY instance");
    let mut stmt = db.conn().prepare(&query).unwrap();
    let matches: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(matches, vec!["luna".to_string(), "nova".to_string()]);
}

#[test]
fn test_build_message_filters() {
    let mut filters = FilterMap::new();
    filters.insert("from".into(), vec!["bigboss".into()]);
    filters.insert("intent".into(), vec!["request".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("msg_from = 'bigboss'"));
    assert!(sql.contains("msg_intent = 'request'"));
    assert!(sql.contains("type = 'message'"));
}

#[test]
fn test_build_mention_filter() {
    let mut filters = FilterMap::new();
    filters.insert("mention".into(), vec!["luna".into(), "nova".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("json_each(msg_mentions) WHERE value = 'luna'"));
    assert!(sql.contains("json_each(msg_mentions) WHERE value = 'nova'"));
    assert!(sql.contains(" OR "));
}

#[test]
fn test_sql_injection_prevention() {
    let mut filters = FilterMap::new();
    filters.insert("instance".into(), vec!["O'Reilly".into()]);
    let sql = build_sql_from_flags(&filters).unwrap();
    assert!(sql.contains("O''Reilly"));
}

// ===== resolve_filter_names =====

#[test]
fn test_resolve_filter_names_with_tag() {
    // Create in-memory DB with an instance that has a tag
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();

    // Insert instance "luna" with tag "team"
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, tag, created_at) \
             VALUES ('luna', 'active', 'team', strftime('%s','now'))",
            [],
        )
        .unwrap();

    // Parse --agent team-luna
    let (mut filters, _) = parse_event_flags(&s(&["--agent", "team-luna"])).unwrap();
    assert_eq!(filters["instance"], vec!["team-luna"]);

    // Resolve: "team-luna" should become "luna"
    resolve_filter_names(&mut filters, &db);
    assert_eq!(filters["instance"], vec!["luna"]);
}

#[test]
fn test_resolve_mention_filter_with_tag() {
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, tag, created_at) \
             VALUES ('luna', 'active', 'team', strftime('%s','now'))",
            [],
        )
        .unwrap();

    let (mut filters, _) = parse_event_flags(&s(&["--mention", "team-luna"])).unwrap();
    resolve_filter_names(&mut filters, &db);
    assert_eq!(filters["mention"], vec!["luna"]);
}

#[test]
fn test_resolve_filter_names_direct_match() {
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();

    db.conn()
        .execute(
            "INSERT INTO instances (name, status, created_at) \
             VALUES ('peso', 'active', strftime('%s','now'))",
            [],
        )
        .unwrap();

    let (mut filters, _) = parse_event_flags(&s(&["--agent", "peso"])).unwrap();
    resolve_filter_names(&mut filters, &db);
    // Should stay "peso" (direct match)
    assert_eq!(filters["instance"], vec!["peso"]);
}

#[test]
fn test_resolve_filter_names_unknown_keeps_original() {
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();

    let (mut filters, _) = parse_event_flags(&s(&["--agent", "nonexistent"])).unwrap();
    resolve_filter_names(&mut filters, &db);
    // Unknown name stays as-is
    assert_eq!(filters["instance"], vec!["nonexistent"]);
}

#[test]
fn test_resolve_filter_names_no_instance_key() {
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();

    let (mut filters, _) = parse_event_flags(&s(&["--status", "listening"])).unwrap();
    // Should not panic when no "instance" key
    resolve_filter_names(&mut filters, &db);
    assert!(!filters.contains_key("instance"));
}

// ===== EventFilterArgs =====

#[test]
fn test_filter_args_to_map_basic() {
    let args = EventFilterArgs {
        agent: vec!["peso".into()],
        event_type: vec!["message".into()],
        from: vec!["bigboss".into()],
        ..Default::default()
    };
    let map = args.to_filter_map();
    assert_eq!(map["instance"], vec!["peso"]);
    assert_eq!(map["type"], vec!["message"]);
    assert_eq!(map["from"], vec!["bigboss"]);
}

#[test]
fn test_filter_args_idle_shortcut() {
    let args = EventFilterArgs {
        idle: vec!["peso".into()],
        ..Default::default()
    };
    let map = args.to_filter_map();
    assert_eq!(map["instance"], vec!["peso"]);
    assert_eq!(map["status"], vec!["listening"]);
}

#[test]
fn test_filter_args_blocked_shortcut() {
    let args = EventFilterArgs {
        blocked: vec!["luna".into()],
        ..Default::default()
    };
    let map = args.to_filter_map();
    assert_eq!(map["instance"], vec!["luna"]);
    assert_eq!(map["status"], vec!["blocked"]);
}

#[test]
fn test_filter_args_collision() {
    let args = EventFilterArgs {
        collision: true,
        ..Default::default()
    };
    let map = args.to_filter_map();
    assert!(map.contains_key("collision"));
}

#[test]
fn test_filter_args_empty() {
    let args = EventFilterArgs::default();
    assert!(!args.has_filters());
    assert!(args.to_filter_map().is_empty());
}

#[test]
fn test_filter_args_has_filters() {
    let args = EventFilterArgs {
        agent: vec!["peso".into()],
        ..Default::default()
    };
    assert!(args.has_filters());
}

#[test]
fn test_filter_args_repeated_agents() {
    let args = EventFilterArgs {
        agent: vec!["peso".into(), "luna".into()],
        ..Default::default()
    };
    let map = args.to_filter_map();
    assert_eq!(map["instance"], vec!["peso", "luna"]);
}
