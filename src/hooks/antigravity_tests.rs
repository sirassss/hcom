use super::*;
use crate::hooks::test_helpers::{EnvGuard, isolated_test_env};
use serial_test::serial;

fn antigravity_test_env() -> (tempfile::TempDir, PathBuf, PathBuf, EnvGuard) {
    let (dir, _hcom_dir, test_home, guard) = isolated_test_env();
    let hooks_path = test_home.join(".gemini").join("config").join("hooks.json");
    (dir, test_home, hooks_path, guard)
}

#[test]
#[serial]
fn test_setup_creates_all_lifecycle_hooks() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();

    assert!(!hooks_path.exists());
    try_setup_antigravity_hooks(false).unwrap();
    assert!(hooks_path.exists());

    // Verify with the validation function
    assert!(verify_antigravity_hooks_installed(false));
}

#[test]
#[serial]
fn test_setup_preserves_other_groups() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();

    // Write a pre-existing custom group
    if let Some(parent) = hooks_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let pre_existing = json!({
        "guard-shell": {
            "PreToolUse": [
                {
                    "matcher": "run_command",
                    "hooks": [
                        {
                            "name": "guard-shell",
                            "type": "command",
                            "command": "python3 guard.py",
                            "description": "some description",
                            "timeout": 2000
                        }
                    ]
                }
            ]
        }
    });
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&pre_existing).unwrap(),
    )
    .unwrap();

    // Setup antigravity hooks
    try_setup_antigravity_hooks(false).unwrap();

    // Read back and verify both exist
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    let root: Value = serde_json::from_str(&content).unwrap();

    assert!(root.get("hcom-lifecycle").is_some());
    assert_eq!(
        root["guard-shell"]["PreToolUse"][0]["hooks"][0]["name"],
        "guard-shell"
    );
}

#[test]
#[serial]
fn test_setup_idempotent() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();

    try_setup_antigravity_hooks(false).unwrap();
    let content1 = std::fs::read_to_string(&hooks_path).unwrap();

    try_setup_antigravity_hooks(false).unwrap();
    let content2 = std::fs::read_to_string(&hooks_path).unwrap();

    assert_eq!(content1, content2);
    assert!(verify_antigravity_hooks_installed(false));
}

#[test]
#[serial]
fn test_remove_only_hcom_lifecycle() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();

    // Setup hooks
    try_setup_antigravity_hooks(false).unwrap();
    assert!(verify_antigravity_hooks_installed(false));

    // Remove hooks
    assert!(remove_antigravity_hooks());
    assert!(!hooks_path.exists()); // Since it was the only group, the file is deleted.

    // Write with multiple groups
    if let Some(parent) = hooks_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let pre_existing = json!({
        "guard-shell": {
            "PreToolUse": []
        }
    });
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&pre_existing).unwrap(),
    )
    .unwrap();

    try_setup_antigravity_hooks(false).unwrap();
    assert!(remove_antigravity_hooks());

    assert!(hooks_path.exists());
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    let root: Value = serde_json::from_str(&content).unwrap();
    assert!(root.get("hcom-lifecycle").is_none());
    assert!(root.get("guard-shell").is_some());
}

#[test]
#[serial]
fn test_remove_also_strips_hcom_permissions() {
    let (_dir, test_home, _hooks_path, _guard) = antigravity_test_env();

    // Install hooks WITH permissions
    try_setup_antigravity_hooks(true).unwrap();
    assert!(verify_antigravity_hooks_installed(true));

    // Check permissions were written
    let settings_path = test_home
        .join(".gemini")
        .join("antigravity-cli")
        .join("settings.json");
    assert!(
        settings_path.exists(),
        "settings.json should exist after install"
    );
    let content = std::fs::read_to_string(&settings_path).unwrap();
    let val: Value = serde_json::from_str(&content).unwrap();
    let allow = val["permissions"]["allow"].as_array().unwrap();
    assert!(
        !allow.is_empty(),
        "hcom rules should be present after install"
    );

    // Remove hooks — should also strip hcom permissions
    assert!(remove_antigravity_hooks());

    // Permissions should now be gone
    if settings_path.exists() {
        let content2 = std::fs::read_to_string(&settings_path).unwrap();
        let val2: Value = serde_json::from_str(&content2).unwrap();
        // permissions key should be absent, or allow should not contain hcom rules
        let has_hcom_rules = val2
            .get("permissions")
            .and_then(|p| p.get("allow"))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .any(|v| v.as_str().is_some_and(|s| s.contains("hcom")))
            })
            .unwrap_or(false);
        assert!(!has_hcom_rules, "hcom permission rules should be removed");
    }
}

#[test]
#[serial]
fn test_verify_detects_missing_hooks() {
    let (_dir, _test_home, _hooks_path, _guard) = antigravity_test_env();
    // File doesn't exist
    assert!(!verify_antigravity_hooks_installed(false));
}

#[test]
fn test_hook_sh_cmd_includes_subcmd_and_hcom() {
    let cmd = hook_sh_cmd("hcom gemini-beforeagent", "gemini-beforeagent", "");
    assert!(cmd.contains("gemini-beforeagent"));
    if cfg!(windows) {
        assert!(cmd.contains("where hcom"));
        assert!(!cmd.contains("sh -c"));
    } else {
        assert!(cmd.contains("command -v hcom"));
    }
    assert!(cmd.contains("ANTIGRAVITY_AGENT=1"));
    assert!(cmd.contains("hcom gemini-beforeagent"));
}

#[test]
fn test_hook_sh_cmd_with_fallback_uses_base64_pipeline() {
    let cmd = hook_sh_cmd("hcom", "gemini-beforetool", "{\"decision\":\"allow\"}");
    assert!(cmd.contains("gemini-beforetool"));
    if cfg!(windows) {
        assert!(cmd.contains("echo {\"decision\":\"allow\"}"));
    } else {
        assert!(cmd.contains("base64 -d"));
    }
}

#[test]
fn test_hook_sh_cmd_without_fallback_exits_zero_only() {
    let cmd = hook_sh_cmd("hcom", "gemini-afteragent", "");
    // no printf/echo when fallback is empty
    assert!(!cmd.contains("printf"));
    assert!(!cmd.contains("base64"));
    assert!(cmd.contains(if cfg!(windows) { "exit /b 0" } else { "exit 0" }));
}

/// Actually execute the generated command with a missing binary and confirm
/// the fallback JSON survives the inner shell pass (no quote stripping).
#[cfg(unix)]
#[test]
fn test_hook_sh_cmd_fallback_emits_valid_json_when_bin_missing() {
    use std::process::Command;
    // Reference an obviously-missing binary so the `||` fallback branch fires.
    let cmd = hook_sh_cmd(
        "definitely_missing_hcom_xyz123",
        "gemini-beforetool",
        "{\"decision\":\"allow\"}",
    );
    let out = Command::new("sh").arg("-c").arg(&cmd).output().unwrap();
    assert!(
        out.status.success(),
        "shell exited non-zero: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("fallback stdout is not valid JSON: stdout={stdout:?} err={e}"));
    assert_eq!(
        parsed.get("decision").and_then(|v| v.as_str()),
        Some("allow")
    );
}

/// Same but with an apostrophe in the fallback to exercise the inner-quote escape.
#[cfg(unix)]
#[test]
fn test_hook_sh_cmd_fallback_handles_apostrophe_in_payload() {
    use std::process::Command;
    let payload = "{\"reason\":\"don't allow\"}";
    let cmd = hook_sh_cmd(
        "definitely_missing_hcom_xyz123",
        "gemini-beforetool",
        payload,
    );
    let out = Command::new("sh").arg("-c").arg(&cmd).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("fallback stdout is not valid JSON: stdout={stdout:?} err={e}"));
    assert_eq!(
        parsed.get("reason").and_then(|v| v.as_str()),
        Some("don't allow")
    );
}

#[test]
fn test_sessionstart_cmd_invokes_hcom_with_env() {
    let cmd = hook_sessionstart_cmd("hcom");
    assert!(cmd.contains("gemini-sessionstart"));
    assert!(cmd.contains("ANTIGRAVITY_AGENT=1"));
    // Lockfile machinery removed — sessionstart is idempotent via name_announced.
    assert!(!cmd.contains("mkdir"));
    assert!(!cmd.contains("parent_pid="));
}

#[test]
#[serial]
fn test_setup_rejects_invalid_existing_hooks_json() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(&hooks_path, "{not json").unwrap();

    let err = try_setup_antigravity_hooks(false).unwrap_err();
    assert!(matches!(err, SetupError::ExistingParseFailed { .. }));
}

#[test]
#[serial]
fn test_setup_rejects_non_object_existing_hooks_json() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(&hooks_path, "[]").unwrap();

    let err = try_setup_antigravity_hooks(false).unwrap_err();
    assert!(matches!(err, SetupError::ExistingRootNotObject { .. }));
}

#[test]
#[serial]
fn test_remove_reports_invalid_existing_hooks_json() {
    let (_dir, _test_home, hooks_path, _guard) = antigravity_test_env();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(&hooks_path, "{not json").unwrap();

    assert!(!remove_antigravity_hooks());
}

// Unix-only: redirects the home dir via $HOME, but on Windows
// `dirs::home_dir()` reads USERPROFILE and ignores it, so the home-based
// cleanup dir points outside the test's temp tree.
#[cfg(unix)]
#[test]
#[serial]
fn test_remove_cleans_default_and_active_hcom_dir_local_paths() {
    let _guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", workspace.join(".hcom"));
        std::env::remove_var("GEMINI_CLI_HOME");
    }
    let gemini_dirs = [home.join(".gemini"), workspace.join(".gemini")];
    for gemini_dir in &gemini_dirs {
        let hooks = antigravity_hooks_path(gemini_dir);
        let settings = antigravity_settings_path(gemini_dir);
        std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &hooks,
            serde_json::to_string_pretty(&json!({
                "hcom-lifecycle": {},
                "custom": true
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &settings,
            serde_json::to_string_pretty(&json!({
                "permissions": {
                    "allow": ["command(hcom send)", "custom"]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    assert!(remove_antigravity_hooks());

    for gemini_dir in gemini_dirs {
        let hooks: Value = serde_json::from_str(
            &std::fs::read_to_string(antigravity_hooks_path(&gemini_dir)).unwrap(),
        )
        .unwrap();
        assert_eq!(hooks, json!({ "custom": true }));
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(antigravity_settings_path(&gemini_dir)).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["permissions"]["allow"], json!(["custom"]));
    }
}

#[test]
fn test_sessionend_reason_from_termination_reason() {
    let raw = json!({
        "terminationReason": "USER_CANCEL",
        "fullyIdle": true
    });
    assert_eq!(sessionend_reason(&raw), "user_cancel");
    assert!(stop_should_skip_soft_finalize(&raw));
}

#[test]
fn test_sessionend_reason_defaults_closed() {
    let raw = json!({ "fullyIdle": false });
    assert_eq!(sessionend_reason(&raw), "closed");
    assert!(!stop_should_skip_soft_finalize(&raw));
}

#[test]
fn test_no_tool_call_skips_soft_finalize_when_not_fully_idle() {
    let raw = json!({
        "terminationReason": "NO_TOOL_CALL",
        "fullyIdle": false
    });
    assert!(stop_should_skip_soft_finalize(&raw));
}

#[test]
fn test_real_teardown_does_not_skip_on_unknown_reason() {
    let raw = json!({
        "terminationReason": "USER_CLOSED",
        "fullyIdle": false
    });
    assert!(!stop_should_skip_soft_finalize(&raw));
}
