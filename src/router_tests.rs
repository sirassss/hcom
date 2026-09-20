use super::*;
use crate::db::DEV_ROOT_KV_KEY;

fn sv(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| s.to_string()).collect()
}

#[test]
fn hook_tools_are_derived_from_released_specs() {
    let actual = crate::commands::hooks::hook_tools();
    let expected: Vec<Tool> = crate::integration_spec::ALL
        .iter()
        .filter(|spec| spec.released && !spec.hooks.names.is_empty())
        .map(|spec| spec.tool)
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn launch_tools_covers_every_released_spec() {
    // Launch recognition is derived from canonical Tool parsing; every
    // released canonical name and alias must therefore route automatically.
    for spec in crate::integration_spec::ALL {
        if !spec.released {
            continue;
        }
        assert!(
            is_launch_tool(spec.name),
            "router did not recognise released tool {}",
            spec.name
        );
        for alias in spec.aliases {
            assert!(
                is_launch_tool(alias),
                "router did not recognise alias {} for {}",
                alias,
                spec.name
            );
        }
    }
}

#[test]
fn read_dev_root_from_kv_returns_stored_value() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("hcom.db");
    let db = crate::db::HcomDb::open_at(&db_path).unwrap();
    db.conn()
        .execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)",
            rusqlite::params![DEV_ROOT_KV_KEY, "/tmp/dev-root"],
        )
        .unwrap();

    assert_eq!(
        read_dev_root_from_kv(&db_path),
        Some(PathBuf::from("/tmp/dev-root"))
    );
}

#[test]
fn read_dev_root_from_kv_returns_none_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("hcom.db");
    crate::db::HcomDb::open_at(&db_path).unwrap();

    assert_eq!(read_dev_root_from_kv(&db_path), None);
}

#[test]
fn is_config_dev_root_invocation_matches_expected_shapes() {
    assert!(is_config_dev_root_invocation(&sv(&["config", "dev_root"])));
    assert!(is_config_dev_root_invocation(&sv(&[
        "config", "dev_root", "/tmp/x"
    ])));
    assert!(is_config_dev_root_invocation(&sv(&[
        "config", "dev_root", "--unset"
    ])));
    assert!(is_config_dev_root_invocation(&sv(&[
        "--name", "vami", "config", "dev_root", "/tmp/x"
    ])));

    assert!(!is_config_dev_root_invocation(&sv(&["config"])));
    assert!(!is_config_dev_root_invocation(&sv(&[
        "config", "terminal", "tmux"
    ])));
    assert!(!is_config_dev_root_invocation(&sv(&["list"])));
    assert!(!is_config_dev_root_invocation(&[]));
}

#[test]
fn is_update_invocation_matches_expected_shapes() {
    assert!(is_update_invocation(&sv(&["update"])));
    assert!(is_update_invocation(&sv(&["update", "--check"])));
    assert!(is_update_invocation(&sv(&["--go", "update"])));
    assert!(is_update_invocation(&sv(&[
        "--name", "lovi", "update", "--check"
    ])));
    assert!(!is_update_invocation(&sv(&["config", "update"])));
    assert!(!is_update_invocation(&sv(&[
        "send", "@lovi", "--", "update"
    ])));
    assert!(!is_update_invocation(&[]));
}

// ── resolve_action tests ────────────────────────────────────────────

#[test]
fn no_args_runs_tui() {
    let action = resolve_action(&[]);
    assert_eq!(action, Action::Tui);
}

#[test]
fn pty_mode() {
    let action = resolve_action(&sv(&["pty", "claude"]));
    assert_eq!(
        action,
        Action::Pty {
            args: sv(&["claude"])
        }
    );
}

#[test]
fn pty_mode_with_args() {
    let action = resolve_action(&sv(&["pty", "claude", "--arg1"]));
    assert_eq!(
        action,
        Action::Pty {
            args: sv(&["claude", "--arg1"])
        }
    );
}

// ── Hook detection ──────────────────────────────────────────────────

#[test]
fn claude_hooks_detected() {
    for hook_name in Tool::Claude.hooks() {
        let action = resolve_action(&sv(&[hook_name]));
        match &action {
            Action::Hook { hook, .. } => assert_eq!(hook, hook_name),
            _ => panic!("expected Hook for {}, got {:?}", hook_name, action),
        }
    }
}

#[test]
fn gemini_hooks_detected() {
    for hook_name in Tool::Gemini.hooks() {
        let action = resolve_action(&sv(&[hook_name]));
        match &action {
            Action::Hook { hook, .. } => assert_eq!(hook, hook_name),
            _ => panic!("expected Hook for {}, got {:?}", hook_name, action),
        }
    }
}

#[test]
fn codex_hook_detected() {
    let action = resolve_action(&sv(&["codex-sessionstart"]));
    match &action {
        Action::Hook { hook, .. } => {
            assert_eq!(hook, "codex-sessionstart");
        }
        _ => panic!("expected Hook, got {:?}", action),
    }
}

#[test]
fn opencode_hooks_detected() {
    for hook_name in Tool::OpenCode.hooks() {
        let action = resolve_action(&sv(&[hook_name]));
        match &action {
            Action::Hook { hook, .. } => assert_eq!(hook, hook_name),
            _ => panic!("expected Hook for {}, got {:?}", hook_name, action),
        }
    }
}

// ── Command detection ───────────────────────────────────────────────

#[test]
fn cli_commands_detected() {
    for cmd_name in COMMANDS {
        let action = resolve_action(&sv(&[cmd_name]));
        match &action {
            Action::Command { cmd, .. } => assert_eq!(cmd, cmd_name),
            _ => panic!("expected Command for {}, got {:?}", cmd_name, action),
        }
    }
}

#[test]
fn command_with_args() {
    let action = resolve_action(&sv(&["send", "@luna", "--", "hello"]));
    match &action {
        Action::Command { cmd, args } => {
            assert_eq!(cmd, "send");
            assert_eq!(*args, sv(&["send", "@luna", "--", "hello"]));
        }
        _ => panic!("expected Command, got {:?}", action),
    }
}

// ── Launch detection ────────────────────────────────────────────────

#[test]
fn launch_tool_direct() {
    let action = resolve_action(&sv(&["claude"]));
    assert_eq!(
        action,
        Action::Launch {
            args: sv(&["claude"])
        }
    );
}

#[test]
fn launch_tool_with_count() {
    let action = resolve_action(&sv(&["3", "claude", "--model", "haiku"]));
    assert_eq!(
        action,
        Action::Launch {
            args: sv(&["3", "claude", "--model", "haiku"])
        }
    );
}

#[test]
fn launch_tool_with_global_flags() {
    let action = resolve_action(&sv(&["--name", "mybot", "--go", "claude"]));
    assert_eq!(
        action,
        Action::Launch {
            args: sv(&["--name", "mybot", "--go", "claude"])
        }
    );
}

#[test]
fn launch_antigravity_direct() {
    let action = resolve_action(&sv(&["antigravity"]));
    assert_eq!(
        action,
        Action::Launch {
            args: sv(&["antigravity"])
        }
    );
}

#[test]
fn launch_agy_direct() {
    let action = resolve_action(&sv(&["agy"]));
    assert_eq!(action, Action::Launch { args: sv(&["agy"]) });
}

#[test]
fn launch_agy_with_count() {
    let action = resolve_action(&sv(&["3", "agy", "--some-flag"]));
    assert_eq!(
        action,
        Action::Launch {
            args: sv(&["3", "agy", "--some-flag"])
        }
    );
}

// ── Global flags ────────────────────────────────────────────────────

#[test]
fn extract_name_flag() {
    let (remaining, flags) = extract_global_flags(&sv(&["--name", "foo", "list"]));
    assert_eq!(remaining, sv(&["list"]));
    assert_eq!(flags.name, Some("foo".to_string()));
    assert!(!flags.go);
}

#[test]
fn extract_go_flag() {
    let (remaining, flags) = extract_global_flags(&sv(&["--go", "stop", "all"]));
    assert_eq!(remaining, sv(&["stop", "all"]));
    assert!(flags.go);
    assert!(flags.name.is_none());
}

#[test]
fn extract_both_flags() {
    let (remaining, flags) = extract_global_flags(&sv(&["--name", "bot", "--go", "stop"]));
    assert_eq!(remaining, sv(&["stop"]));
    assert_eq!(flags.name, Some("bot".to_string()));
    assert!(flags.go);
}

#[test]
fn flags_before_command_extracted() {
    let (remaining, flags) =
        extract_global_flags(&sv(&["--name", "x", "send", "@luna", "--", "hi"]));
    assert_eq!(remaining, sv(&["send", "@luna", "--", "hi"]));
    assert_eq!(flags.name, Some("x".to_string()));
}

#[test]
fn flags_after_command_not_extracted() {
    // --name after a positional stays in rest (clap's trailing_var_arg behavior).
    // This is correct: global flags belong before the command. The full argv
    // is still passed to handlers, which extract --name at the command level.
    let (remaining, flags) =
        extract_global_flags(&sv(&["send", "--name", "x", "@luna", "--", "hi"]));
    assert_eq!(remaining, sv(&["send", "--name", "x", "@luna", "--", "hi"]));
    assert!(flags.name.is_none());
}

// ── extract_global_flags_full (full argv scan) ────────────────────

#[test]
fn full_extract_name_after_command() {
    // --name after command token is extracted (unlike clap-based version)
    let (remaining, flags, help) =
        extract_global_flags_full(&sv(&["send", "--name", "vami", "@luna", "--", "hi"]));
    assert_eq!(remaining, sv(&["send", "@luna", "--", "hi"]));
    assert_eq!(flags.name, Some("vami".to_string()));
    assert!(!help);
}

#[test]
fn full_extract_name_before_command() {
    let (remaining, flags, help) =
        extract_global_flags_full(&sv(&["--name", "vami", "list", "-v"]));
    assert_eq!(remaining, sv(&["list", "-v"]));
    assert_eq!(flags.name, Some("vami".to_string()));
    assert!(!help);
}

#[test]
fn full_extract_respects_separator() {
    // --name after -- should NOT be extracted (it's message text)
    let (remaining, flags, _) =
        extract_global_flags_full(&sv(&["send", "@luna", "--", "--name", "not-a-flag"]));
    assert_eq!(
        remaining,
        sv(&["send", "@luna", "--", "--name", "not-a-flag"])
    );
    assert!(flags.name.is_none());
}

#[test]
fn full_extract_help_detected() {
    let (remaining, flags, help) =
        extract_global_flags_full(&sv(&["send", "--name", "vami", "--help"]));
    assert_eq!(remaining, sv(&["send"]));
    assert_eq!(flags.name, Some("vami".to_string()));
    assert!(help);
}

#[test]
fn full_extract_help_short() {
    let (_, _, help) = extract_global_flags_full(&sv(&["list", "-h"]));
    assert!(help);
}

#[test]
fn full_extract_help_after_separator_ignored() {
    // --help in message text after -- is not a help request
    let (_, _, help) = extract_global_flags_full(&sv(&["send", "@luna", "--", "--help"]));
    assert!(!help);
}

#[test]
fn full_extract_go_flag() {
    let (remaining, flags, _) = extract_global_flags_full(&sv(&["stop", "--go", "all"]));
    assert_eq!(remaining, sv(&["stop", "all"]));
    assert!(flags.go);
}

#[test]
fn full_extract_combined() {
    let (remaining, flags, help) =
        extract_global_flags_full(&sv(&["config", "--name", "bot", "--go", "-h"]));
    assert_eq!(remaining, sv(&["config"]));
    assert_eq!(flags.name, Some("bot".to_string()));
    assert!(flags.go);
    assert!(help);
}

// ── Version / Help / NewTerminal ────────────────────────────────────

#[test]
fn version_flag() {
    assert_eq!(resolve_action(&sv(&["--version"])), Action::Version);
    assert_eq!(resolve_action(&sv(&["-v"])), Action::Version);
}

#[test]
fn help_flag() {
    assert_eq!(resolve_action(&sv(&["--help"])), Action::Help);
    assert_eq!(resolve_action(&sv(&["-h"])), Action::Help);
}

#[test]
fn new_terminal_flag() {
    assert_eq!(
        resolve_action(&sv(&["--new-terminal"])),
        Action::NewTerminal
    );
}

#[test]
fn new_terminal_after_flags() {
    assert_eq!(
        resolve_action(&sv(&["--name", "foo", "--new-terminal"])),
        Action::NewTerminal
    );
}

// ── Hook with global flags ──────────────────────────────────────────

#[test]
fn hook_with_name_flag() {
    let action = resolve_action(&sv(&["--name", "foo", "sessionstart"]));
    match &action {
        Action::Hook { hook, args } => {
            assert_eq!(hook, "sessionstart");
            assert_eq!(*args, sv(&["--name", "foo", "sessionstart"]));
        }
        _ => panic!("expected Hook, got {:?}", action),
    }
}

#[test]
fn send_not_found_gets_external_sender_hint_outside_ai() {
    let err = HcomError::NotFound(
        "Instance 'healthcheck' not found. Run 'hcom start --as healthcheck' to reclaim your identity.".into(),
    );
    let msg = maybe_external_send_name_hint("send", Some("healthcheck"), false, None, false, &err)
        .expect("expected hint");
    assert!(msg.contains("Hint: If 'healthcheck' is an external sender"));
    assert!(msg.contains("send --from healthcheck"));
}

#[test]
fn send_not_found_keeps_agent_recovery_path_inside_ai() {
    let err = HcomError::NotFound(
        "Instance 'luna' not found. Run 'hcom start --as luna' to reclaim your identity.".into(),
    );
    let msg =
        maybe_external_send_name_hint("send", Some("luna"), false, Some("pid-123"), true, &err);
    assert!(msg.is_none());
}

#[test]
fn non_not_found_name_errors_do_not_get_external_sender_hint() {
    let err = HcomError::InvalidInput(
        "Invalid instance name 'Invalid-Name!'. Use base name only (lowercase letters, numbers, underscore).".into(),
    );
    let msg =
        maybe_external_send_name_hint("send", Some("Invalid-Name!"), false, None, false, &err);
    assert!(msg.is_none());
}

// ── is_hook / is_command ────────────────────────────────────────────

#[test]
fn hook_registry_complete() {
    assert!(is_hook("poll"));
    assert!(is_hook("sessionstart"));
    assert!(is_hook("gemini-beforeagent"));
    assert!(is_hook("codex-sessionstart"));
    assert!(is_hook("opencode-start"));
    assert!(is_hook("pi-start"));
    assert!(is_hook("copilot-sessionstart"));
    assert!(!is_hook("send"));
    assert!(!is_hook("unknown"));
}

#[test]
fn hooks_do_not_collide_with_commands_or_launch_tools() {
    for tool in [
        Tool::Claude,
        Tool::Gemini,
        Tool::Codex,
        Tool::OpenCode,
        Tool::Copilot,
        Tool::Pi,
        Tool::Omp,
    ] {
        for hook in tool.hooks() {
            assert!(!COMMANDS.contains(hook), "{hook} collides with command");
            assert!(!is_launch_tool(hook), "{hook} collides with launch tool");
        }
    }
}

#[test]
fn command_registry_complete() {
    assert!(is_command("send"));
    assert!(is_command("list"));
    assert!(is_command("run"));
    assert!(!is_command("poll"));
    assert!(!is_command("claude"));
}

// ── Dev root re-exec path building ──────────────────────────────────

#[test]
fn is_same_file_works() {
    let tmp = std::env::temp_dir().join("hcom_test_same_file");
    let _ = std::fs::write(&tmp, "test");
    assert!(is_same_file(&tmp, &tmp));
    let _ = std::fs::remove_file(&tmp);
}
