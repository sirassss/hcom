use super::*;

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|i| i.to_string()).collect()
}

fn lt(tool: &str) -> LaunchTool {
    LaunchTool::from_str(tool).unwrap()
}

#[test]
fn test_parse_launch_argv_simple() {
    let (count, tool, _flags, args) = parse_launch_argv(&s(&["claude"])).unwrap();
    assert_eq!(count, 1);
    assert_eq!(tool, "claude");
    assert!(args.is_empty());
}

#[test]
fn test_parse_launch_argv_with_count() {
    let (count, tool, _, args) = parse_launch_argv(&s(&["3", "gemini", "-m", "flash"])).unwrap();
    assert_eq!(count, 3);
    assert_eq!(tool, "gemini");
    assert_eq!(args, s(&["-m", "flash"]));
}

#[test]
fn test_parse_launch_argv_with_tag() {
    let (_, tool, flags, args) =
        parse_launch_argv(&s(&["claude", "--tag", "test", "--model", "haiku"])).unwrap();
    assert_eq!(tool, "claude");
    assert_eq!(flags.tag, Some("test".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_accepts_legacy_pty() {
    let (_, tool, flags, args) =
        parse_launch_argv(&s(&["claude", "--headless", "--pty", "--model", "haiku"])).unwrap();
    assert_eq!(tool, "claude");
    assert!(flags.headless);
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_tag_after_tool_args() {
    // --tag after tool-specific args should still be extracted (order-independent)
    let (_, tool, flags, args) =
        parse_launch_argv(&s(&["claude", "--model", "haiku", "--tag", "test"])).unwrap();
    assert_eq!(tool, "claude");
    assert_eq!(flags.tag, Some("test".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_headless() {
    let (_, _, flags, _) = parse_launch_argv(&s(&["claude", "--headless"])).unwrap();
    assert!(flags.headless);
}

#[test]
fn test_parse_launch_argv_no_run_here() {
    let (_, _, flags, _) = parse_launch_argv(&s(&["claude", "--no-run-here"])).unwrap();
    assert_eq!(flags.run_here, Some(false));
}

#[test]
fn test_parse_launch_argv_with_terminal() {
    let (_, _, flags, _) = parse_launch_argv(&s(&["claude", "--terminal", "kitty-tab"])).unwrap();
    assert_eq!(flags.terminal, Some("kitty-tab".to_string()));
}

#[test]
fn test_parse_launch_argv_skips_global_flags() {
    let (count, tool, _, _) =
        parse_launch_argv(&s(&["--name", "bot", "--go", "2", "codex"])).unwrap();
    assert_eq!(count, 2);
    assert_eq!(tool, "codex");
}

#[test]
fn test_parse_launch_argv_empty_fails() {
    assert!(parse_launch_argv(&[]).is_err());
}

#[test]
fn test_primary_tool_args_are_concatenated_verbatim() {
    for (tool, field) in [
        ("claude", "claude_args"),
        ("gemini", "gemini_args"),
        ("codex", "codex_args"),
    ] {
        let mut config = HcomConfig::default();
        config.set_field(field, "--future-config value").unwrap();
        let cli = s(&["--future-upstream-flag", "raw-value"]);
        let merged = merge_tool_args(&lt(tool), &cli, &config);
        assert_eq!(
            merged,
            s(&[
                "--future-config",
                "value",
                "--future-upstream-flag",
                "raw-value"
            ])
        );
    }
}

#[test]
fn test_merge_tool_args_applies_config_for_opencode_family_and_kimi() {
    // These tools previously fell through to the `_` pass-through arm, which
    // silently dropped their `*_args` config at launch.
    let cli = s(&["--yolo"]);
    for (tool, field) in [
        ("opencode", "opencode_args"),
        ("kilo", "kilo_args"),
        ("kimi", "kimi_args"),
    ] {
        let mut config = HcomConfig::default();
        config.set_field(field, "--model from-config").unwrap();
        let merged = merge_tool_args(&lt(tool), &cli, &config);
        assert_eq!(
            merged,
            s(&["--model", "from-config", "--yolo"]),
            "config args must be merged for {tool}"
        );
    }
}

#[test]
fn test_parse_launch_argv_name_after_tool_args() {
    // --name after tool args should be stripped, not passed as tool arg
    let (count, tool, flags, args) = parse_launch_argv(&s(&[
        "1", "claude", "--model", "haiku", "--tag", "test-cl", "--name", "nafo",
    ]))
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(tool, "claude");
    assert_eq!(flags.tag, Some("test-cl".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_go_after_tool_args() {
    // --go after tool args should be stripped
    let (_, _, _, args) = parse_launch_argv(&s(&["claude", "--model", "haiku", "--go"])).unwrap();
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_hcom_prompt() {
    let (_, _, flags, args) = parse_launch_argv(&s(&[
        "claude",
        "--hcom-prompt",
        "do the thing",
        "--model",
        "haiku",
    ]))
    .unwrap();
    assert_eq!(flags.initial_prompt, Some("do the thing".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_hcom_system_prompt() {
    let (_, _, flags, args) = parse_launch_argv(&s(&[
        "claude",
        "--hcom-system-prompt",
        "you are helpful",
        "--model",
        "haiku",
    ]))
    .unwrap();
    assert_eq!(flags.system_prompt, Some("you are helpful".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_system_legacy_alias() {
    let (_, _, flags, args) =
        parse_launch_argv(&s(&["claude", "--system", "you are helpful"])).unwrap();
    assert_eq!(flags.system_prompt, Some("you are helpful".to_string()));
    assert!(args.is_empty());
}

#[test]
fn test_parse_launch_argv_batch_id() {
    let (_, _, flags, args) = parse_launch_argv(&s(&[
        "claude",
        "--batch-id",
        "batch-123",
        "--model",
        "haiku",
    ]))
    .unwrap();
    assert_eq!(flags.batch_id, Some("batch-123".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_device() {
    let (_, _, flags, args) =
        parse_launch_argv(&s(&["claude", "--device", "ABCD", "--model", "haiku"])).unwrap();
    assert_eq!(flags.device, Some("ABCD".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_prepare_launch_execution_claude_print_adds_background_defaults() {
    // Explicit `-p` opts into print mode → detached print-mode defaults applied.
    let config = HcomConfig::default();
    let (args, background) = prepare_launch_execution(&lt("claude"), &s(&["-p"]), &config, true);
    assert!(background);

    assert!(
        args.windows(2)
            .any(|w| w == ["--output-format", "stream-json"])
    );
    assert!(args.iter().any(|arg| arg == "--verbose"));
}

#[test]
fn test_prepare_launch_execution_headless_no_print_flag_stays_pty() {
    // `hcom claude --headless` (no -p) is the live PTY session now — no -p is
    // injected and no print-mode defaults are added.
    let config = HcomConfig::default();
    let (args, background) = prepare_launch_execution(&lt("claude"), &s(&[]), &config, true);
    assert!(background);
    assert!(
        !args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-p" | "--print"))
    );
    assert!(!args.iter().any(|arg| arg == "--output-format"));
}

#[test]
fn test_prepare_launch_execution_headless_positional_prompt_stays_pty() {
    // `hcom claude --headless "task text"` — positional prompt, no -p → PTY.
    let config = HcomConfig::default();
    let (args, _background) =
        prepare_launch_execution(&lt("claude"), &s(&["task text"]), &config, true);
    assert_eq!(args, s(&["task text"]));
}

#[test]
fn test_prepare_launch_execution_headless_only_applies_to_claude() {
    // --headless on other tools must not grow a -p; that flag is Claude-specific.
    let config = HcomConfig::default();
    let (args, _bg) = prepare_launch_execution(&lt("codex"), &s(&[]), &config, true);
    assert!(!args.iter().any(|t| t == "-p"));
}

#[test]
fn test_prepare_launch_execution_interactive_claude_unchanged() {
    // Foreground `hcom claude` (no --headless, no -p) stays untouched.
    let config = HcomConfig::default();
    let (args, background) = prepare_launch_execution(&lt("claude"), &s(&[]), &config, false);
    assert!(!background);
    assert!(args.is_empty());
}

#[test]
fn test_validate_claude_print_defers_prompt_validation_to_claude() {
    assert!(validate_claude_headless_launch("claude", true, &s(&["-p"]), None).is_ok());
}

#[test]
fn test_validate_claude_print_accepts_cli_prompt() {
    assert!(
        validate_claude_headless_launch("claude", true, &s(&["-p", "say hi in hcom"]), None)
            .is_ok()
    );
}

#[test]
fn test_validate_claude_print_accepts_hcom_prompt() {
    assert!(
        validate_claude_headless_launch("claude", true, &s(&["-p"]), Some("say hi in hcom"))
            .is_ok()
    );
}

#[test]
fn test_validate_claude_headless_pty_allows_no_prompt() {
    // Bare `hcom claude --headless` (no -p) is a valid live-session launch —
    // the PTY wrapper keeps the TUI alive waiting for hcom inject.
    assert!(validate_claude_headless_launch("claude", true, &[], None).is_ok());
}

#[test]
fn test_launch_result_json_roundtrip() {
    let result = LaunchResult {
        tool: "claude".to_string(),
        batch_id: "batch-1".to_string(),
        launched: 1,
        failed: 0,
        background: true,
        log_files: vec!["/tmp/test.log".to_string()],
        handles: vec![serde_json::json!({"instance_name": "luna"})],
        errors: Vec::new(),
    };
    let parsed = launch_result_from_json(&launch_result_to_json(&result)).unwrap();
    assert_eq!(parsed.tool, "claude");
    assert_eq!(parsed.batch_id, "batch-1");
    assert_eq!(parsed.launched, 1);
    assert!(parsed.background);
}

#[test]
fn test_format_inline_launch_readiness_ready() {
    let result = LaunchResult {
        tool: "codex".to_string(),
        batch_id: "batch-1".to_string(),
        launched: 1,
        failed: 0,
        background: false,
        log_files: Vec::new(),
        handles: vec![serde_json::json!({"instance_name": "luna"})],
        errors: Vec::new(),
    };

    let line = format_inline_launch_readiness(
        InlineLaunchReadiness::Ready,
        &result,
        &["luna".to_string()],
        2.2,
        &[],
    );

    assert_eq!(line, "Launch ready: luna (1/1 ready, 2.2s).");
}

#[test]
fn test_format_inline_launch_readiness_launching_has_followup_command() {
    let result = LaunchResult {
        tool: "gemini".to_string(),
        batch_id: "batch-2".to_string(),
        launched: 1,
        failed: 0,
        background: false,
        log_files: Vec::new(),
        handles: vec![serde_json::json!({"instance_name": "mari"})],
        errors: Vec::new(),
    };

    let line =
        format_inline_launch_readiness(InlineLaunchReadiness::Launching, &result, &[], 10.0, &[]);

    assert!(line.contains("Still launching after 10.0s: mari (0/1 ready"));
    assert!(line.contains("hcom events launch batch-2 --timeout 30"));
}

#[test]
fn test_format_inline_launch_readiness_failed_includes_detail() {
    let result = LaunchResult {
        tool: "claude".to_string(),
        batch_id: "batch-3".to_string(),
        launched: 1,
        failed: 0,
        background: true,
        log_files: Vec::new(),
        handles: vec![serde_json::json!({"instance_name": "nola"})],
        errors: Vec::new(),
    };

    let line = format_inline_launch_readiness(
        InlineLaunchReadiness::Failed,
        &result,
        &[],
        0.5,
        &["nola: executable not found".to_string()],
    );

    assert_eq!(
        line,
        "Launch failed: nola: executable not found (batch: batch-3)."
    );
}

#[test]
fn test_build_remote_launch_output_prefers_remote_background() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();

    let output = build_remote_launch_output(
        &db,
        &GlobalFlags::default(),
        &LaunchResult {
            tool: "claude".to_string(),
            batch_id: "batch-1".to_string(),
            launched: 1,
            failed: 0,
            background: false,
            log_files: Vec::new(),
            handles: Vec::new(),
            errors: Vec::new(),
        },
        Some("ops".to_string()),
        Some("kitty".to_string()),
        Some(false),
    );

    assert_eq!(output.tool, "claude");
    assert_eq!(output.tag.as_deref(), Some("ops"));
    assert_eq!(output.terminal.as_deref(), Some("kitty"));
    assert!(!output.background);
    assert_eq!(output.run_here, Some(false));
}

#[test]
fn test_build_remote_launch_output_uses_remote_launch_result_background() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();

    let output = build_remote_launch_output(
        &db,
        &GlobalFlags::default(),
        &LaunchResult {
            tool: "codex".to_string(),
            batch_id: "batch-2".to_string(),
            launched: 1,
            failed: 0,
            background: false,
            log_files: Vec::new(),
            handles: Vec::new(),
            errors: Vec::new(),
        },
        None,
        None,
        None,
    );

    assert_eq!(output.tool, "codex");
    assert!(!output.background);
}

#[test]
fn test_is_background_claude_headless() {
    assert!(is_background_from_args(
        &lt("claude"),
        &s(&["-p", "fix tests", "--output-format", "json"])
    ));
}

#[test]
fn test_is_background_claude_interactive() {
    assert!(!is_background_from_args(
        &lt("claude"),
        &s(&["--model", "haiku"])
    ));
}

#[test]
fn test_resolve_launcher_name_prefers_explicit_name() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    let flags = GlobalFlags {
        name: Some("explicit".to_string()),
        go: false,
    };

    let name = resolve_launcher_name(&db, &flags, Some("pid-123"));
    assert_eq!(name, "explicit");
}

#[test]
fn test_resolve_launcher_name_falls_back_to_process_binding() {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    let now = crate::shared::time::now_epoch_f64();
    db.conn()
        .execute(
            "INSERT INTO instances (name, session_id, directory, last_event_id, last_stop, created_at, status, status_time, status_context, tool)
             VALUES (?1, '', '.', 0, 0, ?2, 'active', ?2, 'test', 'claude')",
            rusqlite::params!["bound", now],
        )
        .unwrap();
    db.set_process_binding("pid-123", "", "bound").unwrap();

    let name = resolve_launcher_name(&db, &GlobalFlags::default(), Some("pid-123"));
    assert_eq!(name, "bound");
}

#[test]
fn test_parse_launch_argv_dir_flag() {
    let (_, _, flags, args) =
        parse_launch_argv(&s(&["claude", "--dir", "/tmp/project", "--model", "haiku"])).unwrap();
    assert_eq!(flags.dir, Some("/tmp/project".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_dir_equals() {
    let (_, _, flags, args) =
        parse_launch_argv(&s(&["claude", "--dir=/tmp/project", "--model", "haiku"])).unwrap();
    assert_eq!(flags.dir, Some("/tmp/project".to_string()));
    assert_eq!(args, s(&["--model", "haiku"]));
}

#[test]
fn test_parse_launch_argv_dir_not_passed_to_tool() {
    let (_, _, flags, args) =
        parse_launch_argv(&s(&["gemini", "--dir", "/tmp/proj", "-m", "flash"])).unwrap();
    assert_eq!(flags.dir, Some("/tmp/proj".to_string()));
    assert_eq!(args, s(&["-m", "flash"]));
}
