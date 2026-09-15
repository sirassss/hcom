use super::*;

#[test]
fn test_hook_command_platform_specific() {
    let cmd = hook_command("hcom", "gemini-beforeagent");
    if cfg!(windows) {
        assert_eq!(
            cmd,
            "if (Get-Command hcom -ErrorAction SilentlyContinue) { hcom gemini-beforeagent } else { exit 0 }"
        );
    } else {
        assert_eq!(
            cmd,
            "sh -c 'command -v hcom >/dev/null 2>&1 && exec hcom gemini-beforeagent || exit 0'"
        );
    }
}

#[test]
fn test_hook_command_uses_first_word_as_bin_for_uvx() {
    let cmd = hook_command("uvx hcom", "gemini-sessionend");
    if cfg!(windows) {
        assert_eq!(
            cmd,
            "if (Get-Command uvx -ErrorAction SilentlyContinue) { uvx hcom gemini-sessionend } else { exit 0 }"
        );
    } else {
        assert_eq!(
            cmd,
            "sh -c 'command -v uvx >/dev/null 2>&1 && exec uvx hcom gemini-sessionend || exit 0'"
        );
    }
}

#[test]
fn test_matches_session_pattern() {
    assert!(matches_session_pattern(
        "session-1-abc123-uuid-here.json",
        "session-*-abc123*.json"
    ));
    assert!(matches_session_pattern(
        "session-42-abc123.json",
        "session-*-abc123*.json"
    ));
    assert!(!matches_session_pattern(
        "session-1-xyz999.json",
        "session-*-abc123*.json"
    ));
    assert!(!matches_session_pattern(
        "other-file.txt",
        "session-*-abc123*.json"
    ));
}

#[test]
fn test_derive_gemini_transcript_path_empty() {
    assert!(derive_gemini_transcript_path("").is_none());
}

#[test]
fn test_derive_gemini_transcript_path_no_panic() {
    // Non-existent session prefix — must not panic (returns None or Some depending on fs state)
    let _ = derive_gemini_transcript_path("nonexistent-uuid-12345678");
}

#[test]
fn test_get_handler_known() {
    assert!(get_handler("gemini-sessionstart").is_some());
    assert!(get_handler("gemini-beforeagent").is_some());
    assert!(get_handler("gemini-afteragent").is_some());
    assert!(get_handler("gemini-beforetool").is_some());
    assert!(get_handler("gemini-aftertool").is_some());
    assert!(get_handler("gemini-notification").is_some());
    assert!(get_handler("gemini-sessionend").is_some());
}

#[test]
fn test_get_handler_unknown() {
    assert!(get_handler("gemini-unknown").is_none());
    assert!(get_handler("sessionstart").is_none());
}

#[test]
fn test_detect_antigravity_via_env() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let env: HashMap<String, String> = [("ANTIGRAVITY_AGENT", "1"), ("HOME", "/home/test")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let stdin = serde_json::json!({"sessionId": "abc"});
    let (is_agy, fallback) = detect_antigravity_payload(&ctx, &stdin);
    assert!(is_agy);
    assert!(!fallback);
}

#[test]
fn test_detect_gemini_session_without_tool_call() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let env: HashMap<String, String> = [("GEMINI_CLI", "1"), ("HOME", "/home/test")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let stdin = serde_json::json!({"sessionId": "abc", "conversationId": "legacy"});
    let (is_agy, fallback) = detect_antigravity_payload(&ctx, &stdin);
    assert!(!is_agy);
    assert!(!fallback);
}

#[test]
fn test_detect_antigravity_tool_call_fallback() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let env: HashMap<String, String> = [("HOME", "/home/test")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let stdin = serde_json::json!({
        "conversationId": "6f000787",
        "toolCall": {"name": "run_command", "args": {}}
    });
    let (is_agy, fallback) = detect_antigravity_payload(&ctx, &stdin);
    assert!(is_agy);
    assert!(fallback);
}

#[test]
fn test_detect_antigravity_lifecycle_schema_fallback() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let env: HashMap<String, String> = [("HOME", "/home/test")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let stdin = serde_json::json!({
        "conversationId": "6f000787",
        "workspacePaths": ["/tmp/project"],
        "invocationNum": 1
    });
    let (is_agy, fallback) = detect_antigravity_payload(&ctx, &stdin);
    assert!(is_agy);
    assert!(fallback);
}

#[test]
fn test_detect_plain_conversation_id_does_not_steal_gemini_payload() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let env: HashMap<String, String> = [("HOME", "/home/test")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let stdin = serde_json::json!({"conversationId": "legacy"});
    let (is_agy, fallback) = detect_antigravity_payload(&ctx, &stdin);
    assert!(!is_agy);
    assert!(!fallback);
}

#[test]
fn test_hook_noop() {
    let result = hook_noop();
    assert_eq!(result.exit_code(), 0);
    match &result {
        HookResult::Allow {
            additional_context,
            system_message,
            delivery_ack,
        } => {
            assert!(additional_context.is_none());
            assert!(system_message.is_none());
            assert!(delivery_ack.is_none());
        }
        _ => panic!("expected Allow"),
    }
}

#[test]
fn test_hook_payload_gemini_tool_result() {
    // Test dict format with llmContent
    let raw = serde_json::json!({
        "session_id": "gem-1",
        "tool_response": {"llmContent": "command output here"},
        "tool_name": "run_shell_command"
    });
    let payload = HookPayload::from_gemini(raw);
    assert_eq!(payload.tool_result, "command output here");
    assert_eq!(payload.tool_name, "run_shell_command");

    // Test dict format with output
    let raw2 = serde_json::json!({
        "session_id": "gem-2",
        "tool_response": {"output": "other output"}
    });
    let payload2 = HookPayload::from_gemini(raw2);
    assert_eq!(payload2.tool_result, "other output");

    // Test no tool_response
    let raw3 = serde_json::json!({"session_id": "gem-3"});
    let payload3 = HookPayload::from_gemini(raw3);
    assert_eq!(payload3.tool_result, "");
}

#[test]
fn test_hook_payload_gemini_notification_type() {
    let raw = serde_json::json!({
        "session_id": "gem-1",
        "notification_type": "ToolPermission"
    });
    let payload = HookPayload::from_gemini(raw);
    assert_eq!(payload.notification_type.as_deref(), Some("ToolPermission"));
}

#[test]
fn test_hook_payload_gemini_tool_name_variants() {
    // Test toolName field (camelCase fallback)
    let raw = serde_json::json!({
        "session_id": "gem-1",
        "toolName": "run_shell_command"
    });
    let payload = HookPayload::from_gemini(raw);
    assert_eq!(payload.tool_name, "run_shell_command");

    // Test tool_name field (snake_case — primary format)
    let raw2 = serde_json::json!({
        "session_id": "gem-2",
        "tool_name": "write_file"
    });
    let payload2 = HookPayload::from_gemini(raw2);
    assert_eq!(payload2.tool_name, "write_file");

    // Test missing tool_name → empty
    let raw3 = serde_json::json!({
        "session_id": "gem-3"
    });
    let payload3 = HookPayload::from_gemini(raw3);
    assert_eq!(payload3.tool_name, "");
}

#[test]
fn test_hook_payload_gemini_tool_input_variants() {
    // Test tool_input (snake_case — primary format)
    let raw = serde_json::json!({
        "session_id": "gem-1",
        "tool_input": {"command": "ls"}
    });
    let payload = HookPayload::from_gemini(raw);
    assert_eq!(payload.tool_input["command"], "ls");

    // Test toolInput (camelCase fallback)
    let raw2 = serde_json::json!({
        "session_id": "gem-2",
        "toolInput": {"file_path": "/tmp/test"}
    });
    let payload2 = HookPayload::from_gemini(raw2);
    assert_eq!(payload2.tool_input["file_path"], "/tmp/test");

    // Test missing tool_input → empty object
    let raw3 = serde_json::json!({
        "session_id": "gem-3"
    });
    let payload3 = HookPayload::from_gemini(raw3);
    assert!(payload3.tool_input.is_object());
}

#[test]
fn test_is_hcom_hook() {
    let hcom_hook = serde_json::json!({
        "name": "hcom-sessionstart",
        "type": "command",
        "command": "hcom gemini-sessionstart"
    });
    assert!(is_hcom_hook(&hcom_hook));

    let user_hook = serde_json::json!({
        "name": "my-hook",
        "type": "command",
        "command": "/usr/local/bin/my-script"
    });
    assert!(!is_hcom_hook(&user_hook));
}

#[test]
fn test_set_hooks_enabled() {
    let mut settings = serde_json::Map::new();
    set_hooks_enabled(&mut settings);
    assert!(is_hooks_enabled(&settings));
}

#[test]
fn test_set_hooks_enabled_migrates_legacy() {
    let mut settings = serde_json::Map::new();
    let mut hooks = serde_json::Map::new();
    hooks.insert("enabled".into(), Value::Bool(true));
    hooks.insert("SessionStart".into(), serde_json::json!([]));
    settings.insert("hooks".into(), Value::Object(hooks));

    set_hooks_enabled(&mut settings);

    // hooksConfig.enabled should be true
    assert!(is_hooks_enabled(&settings));
    // Legacy hooks.enabled should be removed
    let hooks = settings.get("hooks").and_then(|v| v.as_object()).unwrap();
    assert!(hooks.get("enabled").is_none());
}

#[test]
fn test_remove_hcom_hooks_preserves_user_hooks() {
    let mut settings: serde_json::Map<String, Value> = serde_json::from_value(serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "*",
                    "hooks": [
                        {"name": "hcom-sessionstart", "type": "command", "command": "hcom gemini-sessionstart"},
                        {"name": "my-hook", "type": "command", "command": "/usr/bin/my-script"}
                    ]
                }]
            }
        })).unwrap();

    remove_hcom_hooks_from_settings(&mut settings);

    // User hook should be preserved
    let hooks = settings.get("hooks").unwrap();
    let ss = hooks.get("SessionStart").unwrap().as_array().unwrap();
    assert_eq!(ss.len(), 1);
    let remaining = ss[0].get("hooks").unwrap().as_array().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        remaining[0].get("name").unwrap().as_str().unwrap(),
        "my-hook"
    );
}

#[test]
fn test_remove_hcom_hooks_drops_empty() {
    let mut settings: serde_json::Map<String, Value> = serde_json::from_value(serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "*",
                    "hooks": [
                        {"name": "hcom-sessionstart", "type": "command", "command": "hcom gemini-sessionstart"}
                    ]
                }]
            }
        })).unwrap();

    remove_hcom_hooks_from_settings(&mut settings);

    // hooks dict should be removed (all empty)
    assert!(settings.get("hooks").is_none());
}

#[test]
fn test_build_gemini_policy() {
    let policy = build_gemini_policy();
    assert!(policy.contains("[[rule]]"));
    assert!(policy.contains("toolName = \"run_shell_command\""));
    assert!(policy.contains("decision = \"allow\""));
    assert!(policy.contains("priority = 300"));
    assert!(policy.contains("hcom send"));
    assert!(policy.contains("hcom list"));
    assert!(policy.contains("commandPrefix"));
}

#[test]
#[serial]
fn test_setup_and_verify_gemini_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let hcom_dir = dir.path().join(".hcom");
    std::fs::create_dir_all(&hcom_dir).unwrap();
    let settings_dir = dir.path().join(".gemini");
    std::fs::create_dir_all(&settings_dir).unwrap();

    // Redirect paths via HCOM_DIR
    let saved = std::env::var("HCOM_DIR").ok();
    unsafe { std::env::set_var("HCOM_DIR", &hcom_dir) };

    let success = setup_gemini_hooks(true);
    assert!(success, "setup should succeed");

    let verified = verify_gemini_hooks_installed(true);
    assert!(verified, "verify should pass after setup");

    // Check settings file was written
    let settings_path = dir.path().join(".gemini").join("settings.json");
    assert!(settings_path.exists());
    let content = std::fs::read_to_string(&settings_path).unwrap();
    assert!(content.contains("hcom-sessionstart"));
    assert!(content.contains("hcom-beforeagent"));
    assert!(content.contains("enableHooks"));

    // Check policy file was written
    let policy_path = dir
        .path()
        .join(".gemini")
        .join("policies")
        .join("hcom.toml");
    assert!(policy_path.exists(), "policy file should be created");

    // Remove hooks
    let remove_ok = remove_hooks_from_path(&settings_path);
    assert!(remove_ok);
    let verify_after_remove = verify_hooks_at(&settings_path, false).is_ok();
    assert!(!verify_after_remove, "verify should fail after remove");

    // Restore
    if let Some(v) = saved {
        unsafe { std::env::set_var("HCOM_DIR", v) };
    } else {
        unsafe { std::env::remove_var("HCOM_DIR") };
    }
}

use crate::hooks::test_helpers::{EnvGuard, isolated_test_env};
use serial_test::serial;

fn gemini_test_env() -> (tempfile::TempDir, PathBuf, PathBuf, EnvGuard) {
    let (dir, _hcom_dir, test_home, guard) = isolated_test_env();
    let settings_path = test_home.join(".gemini").join("settings.json");
    (dir, test_home, settings_path, guard)
}

/// Independent verification: check no hcom hooks present in settings JSON.
fn independently_verify_no_hcom_hooks(settings: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    let hooks = match settings.get("hooks").and_then(|v| v.as_object()) {
        Some(h) => h,
        None => return violations,
    };
    for (hook_type, matchers_val) in hooks {
        if hook_type == "enabled" || hook_type == "disabled" {
            continue;
        }
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
                let name = hook.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let command = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if name.contains("hcom") || command.contains("hcom") {
                    violations.push(format!(
                        "{hook_type}[{i}].hooks[{j}]: name={name}, command={command}"
                    ));
                }
            }
        }
    }
    violations
}

/// Independent verification: check expected hcom hooks are present.
fn independently_verify_hcom_hooks_present(
    settings: &Value,
    expected: &[(&str, &str)], // (hook_type, cmd_suffix)
) -> Vec<String> {
    let hcom_cmd = crate::runtime_env::build_hcom_command();
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
        let expected_full = hook_command(&hcom_cmd, cmd_suffix);
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

fn read_json(path: &Path) -> Value {
    let content = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&content).unwrap()
}

#[test]
#[serial]
fn test_setup_gemini_hooks_installs_expected() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    let result = setup_gemini_hooks(false);
    assert!(result, "setup should succeed");
    assert!(settings_path.exists());

    let settings = read_json(&settings_path);

    // tools.enableHooks must be true
    assert_eq!(settings["tools"]["enableHooks"], true);

    // hooksConfig.enabled must be true (not legacy hooks.enabled)
    assert_eq!(
        settings
            .get("hooksConfig")
            .and_then(|v| v.get("enabled"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(
        settings
            .get("hooks")
            .and_then(|v| v.get("enabled"))
            .is_none(),
        "legacy hooks.enabled should not be present"
    );

    // Each hook type from GEMINI_HOOK_CONFIGS must be present with correct values
    let hooks = settings.get("hooks").unwrap();
    for &(hook_type, expected_matcher, cmd_suffix, expected_timeout, _) in GEMINI_HOOK_CONFIGS {
        let arr = hooks
            .get(hook_type)
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{hook_type} missing or not array"));
        assert_eq!(arr.len(), 1, "{hook_type} should have 1 matcher");

        let matcher_dict = &arr[0];
        assert_eq!(
            matcher_dict
                .get("matcher")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            expected_matcher,
            "{hook_type} matcher mismatch"
        );

        let hook_list = matcher_dict
            .get("hooks")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(hook_list.len(), 1, "{hook_type} should have 1 hook");

        let hook = &hook_list[0];
        assert_eq!(hook["type"], "command");
        assert_eq!(
            hook["name"].as_str().unwrap(),
            format!("hcom-{}", hook_type.to_lowercase())
        );
        assert_eq!(hook["timeout"].as_u64().unwrap(), expected_timeout as u64);
        let hcom = crate::runtime_env::build_hcom_command();
        let expected_command = hook_command(&hcom, cmd_suffix);
        assert_eq!(
            hook["command"].as_str().unwrap(),
            expected_command,
            "{hook_type} command mismatch"
        );
    }

    assert!(verify_gemini_hooks_installed(false));

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_gemini_preserves_user_model_settings() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "model": {
            "name": "gemini-2.5-pro",
            "maxSessionTurns": 50,
            "compressionThreshold": 0.3,
        },
        "ui": {
            "hideBanner": true,
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    assert!(setup_gemini_hooks(false));

    let updated = read_json(&settings_path);
    assert_eq!(updated["model"]["name"], "gemini-2.5-pro");
    assert_eq!(updated["model"]["maxSessionTurns"], 50);
    assert_eq!(updated["model"]["compressionThreshold"], 0.3);
    assert_eq!(updated["ui"]["hideBanner"], true);

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_gemini_preserves_user_tools_allowed_and_uses_policy() {
    let (_dir, test_home, settings_path, _guard) = gemini_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "tools": {
            "allowed": ["run_shell_command(git status)", "read_file"]
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    assert!(setup_gemini_hooks(true));

    let updated = read_json(&settings_path);
    let allowed = updated["tools"]["allowed"].as_array().unwrap();
    assert!(
        allowed
            .iter()
            .any(|v| v.as_str() == Some("run_shell_command(git status)")),
        "user's allowed entry should be preserved"
    );
    assert!(
        allowed.iter().any(|v| v.as_str() == Some("read_file")),
        "user's read_file entry should be preserved"
    );
    // hcom permissions should NOT be in tools.allowed (moved to policy engine)
    assert!(
        !allowed
            .iter()
            .any(|v| v.as_str().map(|s| s.contains("hcom")).unwrap_or(false)),
        "hcom permissions should not be in tools.allowed"
    );

    // Policy file should exist instead
    let policy_file = test_home.join(".gemini").join("policies").join("hcom.toml");
    assert!(policy_file.exists(), "policy file should be created");
    let policy_content = std::fs::read_to_string(&policy_file).unwrap();
    assert!(policy_content.contains("hcom send"));
    assert!(policy_content.contains("decision = \"allow\""));

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_gemini_idempotent() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(false));
    let first = std::fs::read_to_string(&settings_path).unwrap();

    assert!(setup_gemini_hooks(false));
    let second = std::fs::read_to_string(&settings_path).unwrap();

    assert_eq!(first, second, "setup should be idempotent");

    drop(_guard);
}

#[test]
#[serial]
fn test_remove_gemini_preserves_hooks_disabled_and_user_hooks() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let settings = serde_json::json!({
        "tools": {"enableHooks": true},
        "model": {"skipNextSpeakerCheck": false},
        "hooks": {
            "disabled": ["keep-me"],
            "SessionStart": [{
                "matcher": "startup",
                "hooks": [{
                    "name": "hcom-sessionstart",
                    "type": "command",
                    "command": "hcom gemini-sessionstart",
                    "timeout": 5000,
                }],
            }],
            "BeforeAgent": [{
                "matcher": "*",
                "hooks": [
                    {
                        "name": "hcom-beforeagent",
                        "type": "command",
                        "command": "hcom gemini-beforeagent",
                        "timeout": 5000,
                    },
                    {
                        "name": "keep-other",
                        "type": "command",
                        "command": "echo hi",
                        "timeout": 1,
                    },
                ],
            }],
        },
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(remove_hooks_from_path(&settings_path));

    let updated = read_json(&settings_path);
    // disabled list preserved
    assert_eq!(updated["hooks"]["disabled"], serde_json::json!(["keep-me"]));
    // model preserved
    assert_eq!(updated["model"]["skipNextSpeakerCheck"], false);
    // SessionStart removed (only had hcom hooks)
    assert!(updated["hooks"].get("SessionStart").is_none());
    // BeforeAgent user hook preserved
    let before_agent = updated["hooks"]["BeforeAgent"].as_array().unwrap();
    assert_eq!(before_agent.len(), 1);
    let remaining = before_agent[0]["hooks"].as_array().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["name"], "keep-other");

    // Independent check: no hcom hooks remain
    let violations = independently_verify_no_hcom_hooks(&updated);
    assert!(violations.is_empty(), "hcom hooks remain: {violations:?}");

    drop(_guard);
}

#[test]
#[serial]
fn test_verify_gemini_detects_missing_hooks_enabled() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(false));
    assert!(verify_gemini_hooks_installed(false));

    // Remove hooksConfig.enabled → verify should fail
    let mut settings = read_json(&settings_path);
    settings["hooksConfig"]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
    assert!(verify_hooks_at(&settings_path, false).is_err());

    // Set hooksConfig.enabled to false → verify should fail
    settings["hooksConfig"]["enabled"] = Value::Bool(false);
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
    assert!(verify_hooks_at(&settings_path, false).is_err());

    drop(_guard);
}

#[test]
#[serial]
fn test_verify_accepts_alternate_hcom_prefix() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(false));

    let mut settings = read_json(&settings_path);
    let (hook_type, _, cmd_suffix, _, _) = GEMINI_HOOK_CONFIGS[0];
    settings["hooks"][hook_type][0]["hooks"][0]["command"] =
        Value::String(format!("uvx hcom {cmd_suffix}"));
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(verify_hooks_at(&settings_path, false).is_ok());

    drop(_guard);
}

#[test]
#[serial]
fn test_verify_accepts_timeout_value_edit() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(false));

    let mut settings = read_json(&settings_path);
    for &(hook_type, _, _, _, _) in GEMINI_HOOK_CONFIGS {
        if let Some(arr) = settings["hooks"][hook_type].as_array_mut() {
            for matcher_obj in arr {
                if let Some(hooks) = matcher_obj["hooks"].as_array_mut() {
                    for hook in hooks {
                        hook["timeout"] = serde_json::json!(10);
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

    assert!(verify_gemini_hooks_installed(false));

    drop(_guard);
}

#[test]
#[serial]
fn test_verify_catches_timeout_field_dropped() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(false));

    let mut settings = read_json(&settings_path);
    for &(hook_type, _, _, _, _) in GEMINI_HOOK_CONFIGS {
        if let Some(arr) = settings["hooks"][hook_type].as_array_mut() {
            for matcher_obj in arr {
                if let Some(hooks) = matcher_obj["hooks"].as_array_mut() {
                    for hook in hooks {
                        hook.as_object_mut().unwrap().remove("timeout");
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

    assert!(!verify_gemini_hooks_installed(false));

    drop(_guard);
}

#[test]
#[serial]
fn test_verify_rejects_non_numeric_timeout() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(false));

    let mut settings = read_json(&settings_path);
    for &(hook_type, _, _, _, _) in GEMINI_HOOK_CONFIGS {
        if let Some(arr) = settings["hooks"][hook_type].as_array_mut() {
            for matcher_obj in arr {
                if let Some(hooks) = matcher_obj["hooks"].as_array_mut() {
                    for hook in hooks {
                        hook["timeout"] = serde_json::json!("5000");
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

    assert!(!verify_gemini_hooks_installed(false));

    drop(_guard);
}

#[test]
#[serial]
fn test_remove_gemini_cleans_legacy_enabled() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let settings = serde_json::json!({
        "tools": {"enableHooks": true},
        "hooks": {
            "enabled": true,
            "SessionStart": [{
                "matcher": "*",
                "hooks": [{
                    "name": "hcom-sessionstart",
                    "type": "command",
                    "command": "hcom gemini-sessionstart",
                    "timeout": 5000,
                }],
            }],
        },
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(remove_hooks_from_path(&settings_path));

    let updated = read_json(&settings_path);
    // Legacy hooks.enabled should be gone
    assert!(
        updated.get("hooks").is_none() || updated["hooks"].get("enabled").is_none(),
        "legacy hooks.enabled should be removed"
    );
    // hcom hooks should be gone
    assert!(
        updated.get("hooks").is_none() || updated["hooks"].get("SessionStart").is_none(),
        "hcom hooks should be removed"
    );
}

#[test]
#[serial]
fn test_setup_gemini_remove_roundtrip() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    // Pre-populate with user data
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "model": {"name": "gemini-2.5-pro"},
        "ui": {"theme": "Dark"},
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    // Setup
    assert!(setup_gemini_hooks(false));
    let after_setup = read_json(&settings_path);
    let expected: Vec<(&str, &str)> = GEMINI_HOOK_CONFIGS
        .iter()
        .map(|&(ht, _, cmd, _, _)| (ht, cmd))
        .collect();
    let missing = independently_verify_hcom_hooks_present(&after_setup, &expected);
    assert!(
        missing.is_empty(),
        "after setup, missing hooks: {missing:?}"
    );

    // Remove
    assert!(remove_hooks_from_path(&settings_path));
    let after_remove = read_json(&settings_path);
    let violations = independently_verify_no_hcom_hooks(&after_remove);
    assert!(
        violations.is_empty(),
        "after remove, hcom hooks still present: {violations:?}"
    );

    // User data preserved
    assert_eq!(after_remove["model"]["name"], "gemini-2.5-pro");
    assert_eq!(after_remove["ui"]["theme"], "Dark");

    drop(_guard);
}

#[test]
#[serial]
fn test_gemini_handles_empty_file() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, "{}").unwrap();

    assert!(setup_gemini_hooks(false));

    let settings = read_json(&settings_path);
    let expected: Vec<(&str, &str)> = GEMINI_HOOK_CONFIGS
        .iter()
        .map(|&(ht, _, cmd, _, _)| (ht, cmd))
        .collect();
    let missing = independently_verify_hcom_hooks_present(&settings, &expected);
    assert!(missing.is_empty(), "missing hooks: {missing:?}");

    drop(_guard);
}

#[test]
#[serial]
fn test_gemini_handles_no_file() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    assert!(!settings_path.exists());
    assert!(setup_gemini_hooks(false));
    assert!(settings_path.exists());

    let settings = read_json(&settings_path);
    let expected: Vec<(&str, &str)> = GEMINI_HOOK_CONFIGS
        .iter()
        .map(|&(ht, _, cmd, _, _)| (ht, cmd))
        .collect();
    let missing = independently_verify_hcom_hooks_present(&settings, &expected);
    assert!(missing.is_empty(), "missing hooks: {missing:?}");

    drop(_guard);
}

#[test]
#[serial]
fn test_gemini_mixed_hcom_and_user_hooks() {
    let (_dir, _test_home, settings_path, _guard) = gemini_test_env();

    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let settings = serde_json::json!({
        "hooks": {
            "enabled": true,
            "SessionStart": [{
                "matcher": "*",
                "hooks": [
                    {"name": "hcom-sessionstart", "type": "command",
                     "command": "hcom gemini-sessionstart", "timeout": 5000},
                    {"name": "my-logger", "type": "command",
                     "command": "echo session started", "timeout": 1000},
                ]
            }]
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    assert!(remove_hooks_from_path(&settings_path));

    let updated = read_json(&settings_path);
    // User hook should remain
    let session_hooks = updated["hooks"]["SessionStart"].as_array().unwrap();
    assert_eq!(session_hooks.len(), 1);
    let hooks_list = session_hooks[0]["hooks"].as_array().unwrap();
    assert_eq!(hooks_list.len(), 1);
    assert_eq!(hooks_list[0]["name"], "my-logger");

    // No hcom hooks
    let violations = independently_verify_no_hcom_hooks(&updated);
    assert!(violations.is_empty());

    drop(_guard);
}

#[test]
#[serial]
fn test_gemini_handles_malformed_hooks() {
    let corrupt_cases: Vec<Value> = vec![
        Value::Null,
        Value::String("string".into()),
        serde_json::json!([]),
        serde_json::json!({"SessionStart": "not_a_list"}),
        serde_json::json!({"SessionStart": [null, "string", 123]}),
        serde_json::json!({"SessionStart": [{"matcher": "*", "hooks": "not_a_list"}]}),
    ];

    for corrupt_hooks in corrupt_cases {
        let (_dir, _test_home, settings_path, _guard) = gemini_test_env();
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();

        let settings = serde_json::json!({
            "hooks": corrupt_hooks,
            "ui": {"theme": "Dark"},
        });
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        // Should not crash
        let _ = setup_gemini_hooks(false);

        // User data should still be readable
        let updated = read_json(&settings_path);
        assert_eq!(updated["ui"]["theme"], "Dark");
    }
}

#[test]
#[serial]
fn test_setup_gemini_cleans_legacy_tools_allowed() {
    let (_dir, test_home, settings_path, _guard) = gemini_test_env();

    // Pre-populate with legacy hcom tools.allowed entries
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let user_settings = serde_json::json!({
        "tools": {
            "allowed": [
                "run_shell_command(hcom send)",
                "run_shell_command(hcom list)",
                "run_shell_command(git status)",
            ]
        }
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&user_settings).unwrap(),
    )
    .unwrap();

    assert!(setup_gemini_hooks(true));

    let updated = read_json(&settings_path);
    let allowed = updated["tools"]["allowed"].as_array().unwrap();
    // User's non-hcom entry preserved
    assert!(
        allowed
            .iter()
            .any(|v| v.as_str() == Some("run_shell_command(git status)")),
        "user's git status entry should be preserved"
    );
    // Legacy hcom entries removed
    assert!(
        !allowed
            .iter()
            .any(|v| v.as_str().map(|s| s.contains("hcom")).unwrap_or(false)),
        "legacy hcom tools.allowed entries should be removed"
    );
    // Policy file should exist instead
    let policy_file = test_home.join(".gemini").join("policies").join("hcom.toml");
    assert!(policy_file.exists(), "policy file should be created");

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_gemini_creates_and_removes_policy() {
    let (_dir, test_home, _settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(true));
    let policy_file = test_home.join(".gemini").join("policies").join("hcom.toml");
    assert!(policy_file.exists(), "policy file should be created");

    // Verify content
    let content = std::fs::read_to_string(&policy_file).unwrap();
    assert!(content.contains("[[rule]]"));
    assert!(content.contains("toolName = \"run_shell_command\""));
    assert!(content.contains("commandPrefix"));
    assert!(content.contains("decision = \"allow\""));
    assert!(content.contains("priority = 300"));

    // Setup without permissions should remove policy
    assert!(setup_gemini_hooks(false));
    assert!(
        !policy_file.exists(),
        "policy file should be removed when permissions disabled"
    );

    drop(_guard);
}

#[test]
#[serial]
fn test_setup_gemini_policy_idempotent() {
    let (_dir, test_home, _settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(true));
    let policy_file = test_home.join(".gemini").join("policies").join("hcom.toml");
    let first = std::fs::read_to_string(&policy_file).unwrap();

    assert!(setup_gemini_hooks(true));
    let second = std::fs::read_to_string(&policy_file).unwrap();

    assert_eq!(first, second, "policy content should be idempotent");

    drop(_guard);
}

#[test]
#[serial]
fn test_remove_gemini_hooks_removes_policy() {
    let (_dir, test_home, _settings_path, _guard) = gemini_test_env();

    assert!(setup_gemini_hooks(true));
    let policy_file = test_home.join(".gemini").join("policies").join("hcom.toml");
    assert!(policy_file.exists());

    remove_gemini_hooks();
    assert!(
        !policy_file.exists(),
        "policy file should be removed on hook removal"
    );

    drop(_guard);
}

#[test]
fn test_antigravity_serialization_allow_no_context() {
    let result = HookResult::Allow {
        additional_context: None,
        system_message: None,
        delivery_ack: None,
    };
    // PreToolUse REQUIRES `decision` — `{}` is treated as deny and blocks all tools.
    let beforetool = serialize_hook_result("antigravity", "gemini-beforetool", &result).unwrap();
    assert_eq!(beforetool, serde_json::json!({ "decision": "allow" }));

    // Other phases (PostToolUse, PreInvocation noop, etc.) emit `{}`.
    let aftertool = serialize_hook_result("antigravity", "gemini-aftertool", &result).unwrap();
    assert_eq!(aftertool, serde_json::json!({}));

    let sessionend = serialize_hook_result("antigravity", "gemini-sessionend", &result).unwrap();
    assert_eq!(sessionend, serde_json::json!({ "decision": "allow" }));
}

#[test]
fn test_antigravity_serialization_allow_with_context() {
    let result = HookResult::Allow {
        additional_context: Some("pending messages".to_string()),
        system_message: None,
        delivery_ack: None,
    };
    // agy PreInvocation/PostInvocation hooks accept injectSteps; hookSpecificOutput is
    // silently ignored. Only gemini-sessionstart, -beforeagent, -afteragent emit injection.
    let out = serialize_hook_result("antigravity", "gemini-beforeagent", &result).unwrap();
    assert_eq!(
        out["injectSteps"][0]["ephemeralMessage"],
        "pending messages"
    );
    assert!(out.get("hookSpecificOutput").is_none());
    assert!(out.get("decision").is_none());

    // PostToolUse (gemini-aftertool) cannot inject in agy — drop context, emit `{}`.
    let aftertool = serialize_hook_result("antigravity", "gemini-aftertool", &result).unwrap();
    assert_eq!(aftertool, serde_json::json!({}));
}

#[test]
fn test_antigravity_serialization_block() {
    let result = HookResult::Block {
        reason: "permission denied".to_string(),
        delivery_ack: None,
    };
    let out = serialize_hook_result("antigravity", "gemini-beforetool", &result).unwrap();
    assert_eq!(out["decision"], "deny");
    assert_eq!(out["reason"], "permission denied");
    assert!(out.get("hookSpecificOutput").is_none());
}

#[test]
fn test_antigravity_serialization_update_input() {
    let result = HookResult::UpdateInput {
        updated_input: serde_json::json!({"command": "ls -l"}),
    };
    let out = serialize_hook_result("antigravity", "gemini-beforetool", &result).unwrap();
    assert_eq!(out["updatedInput"]["command"], "ls -l");
}

#[test]
fn test_gemini_serialization_allow_no_context() {
    let result = HookResult::Allow {
        additional_context: None,
        system_message: None,
        delivery_ack: None,
    };
    let out = serialize_hook_result("gemini", "gemini-beforetool", &result);
    assert!(out.is_none());
}

#[test]
fn test_gemini_serialization_allow_with_context() {
    let result = HookResult::Allow {
        additional_context: Some("injected context".to_string()),
        system_message: None,
        delivery_ack: None,
    };
    let out = serialize_hook_result("gemini", "gemini-beforetool", &result).unwrap();
    assert_eq!(out["decision"], "allow");
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "BeforeTool");
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "injected context"
    );
}

fn make_test_db() -> (tempfile::TempDir, HcomDb) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    (dir, db)
}

fn insert_test_instance(db: &HcomDb, name: &str, tool: &str) {
    let now = chrono::Utc::now().timestamp() as f64;
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, created_at, tool) VALUES (?1, 'active', ?2, ?3)",
            rusqlite::params![name, now, tool],
        )
        .unwrap();
}

fn insert_test_message(db: &HcomDb, instance: &str, from: &str, text: &str) -> i64 {
    let data = serde_json::json!({
        "from": from,
        "text": text,
        "scope": "broadcast",
    })
    .to_string();
    db.conn()
            .execute(
                "INSERT INTO events (type, timestamp, instance, data) VALUES ('message', '2026-01-01T00:00:01Z', ?1, ?2)",
                rusqlite::params![instance, data],
            )
            .unwrap();
    db.conn().last_insert_rowid()
}

#[test]
fn test_antigravity_aftertool_does_not_ack_pending_delivery() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let (_dir, db) = make_test_db();
    insert_test_instance(&db, "vago", "antigravity");
    db.rebind_session("sess-vago", "vago").unwrap();
    let message_id = insert_test_message(&db, "homo", "homo", "secret body");

    let env: HashMap<String, String> = [("ANTIGRAVITY_AGENT", "1"), ("HOME", "/home/test")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = HcomContext::from_env(&env, PathBuf::from("/tmp"));
    let payload = HookPayload {
        session_id: Some("sess-vago".to_string()),
        transcript_path: None,
        hook_name: "gemini-aftertool".to_string(),
        tool: "antigravity".to_string(),
        tool_name: "run_command".to_string(),
        tool_input: serde_json::Value::Null,
        tool_result: String::new(),
        notification_type: None,
        raw: serde_json::Value::Null,
    };

    let result = handle_aftertool(&db, &ctx, &payload);
    match result {
        HookResult::Allow {
            additional_context,
            delivery_ack,
            ..
        } => {
            assert!(additional_context.is_none());
            assert!(delivery_ack.is_none());
        }
        _ => panic!("expected Allow"),
    }

    let cursor: i64 = db
        .conn()
        .query_row(
            "SELECT last_event_id FROM instances WHERE name = 'vago'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor, 0);
    assert_eq!(db.get_unread_messages("vago").len(), 1);
    assert_eq!(db.get_unread_messages("vago")[0].event_id, Some(message_id));
}

#[test]
fn test_antigravity_afteragent_does_not_mark_idle_mid_turn() {
    let (_dir, db) = make_test_db();
    insert_test_instance(&db, "vago", "antigravity");
    db.rebind_session("sess-vago", "vago").unwrap();

    let payload = HookPayload {
        session_id: Some("sess-vago".to_string()),
        transcript_path: None,
        hook_name: "gemini-afteragent".to_string(),
        tool: "antigravity".to_string(),
        tool_name: String::new(),
        tool_input: serde_json::Value::Null,
        tool_result: String::new(),
        notification_type: None,
        raw: serde_json::Value::Null,
    };

    let result = handle_afteragent(&db, &HcomContext::from_os(), &payload);
    assert_eq!(result.exit_code(), 0);
    let instance = db.get_instance_full("vago").unwrap().unwrap();
    assert_eq!(instance.status, "active");
}

#[test]
fn test_antigravity_turn_end_stop_marks_idle() {
    let (_dir, db) = make_test_db();
    insert_test_instance(&db, "vago", "antigravity");
    db.rebind_session("sess-vago", "vago").unwrap();

    let payload = HookPayload {
        session_id: Some("sess-vago".to_string()),
        transcript_path: None,
        hook_name: "gemini-sessionend".to_string(),
        tool: "antigravity".to_string(),
        tool_name: String::new(),
        tool_input: serde_json::Value::Null,
        tool_result: String::new(),
        notification_type: None,
        raw: serde_json::json!({
            "fullyIdle": true,
            "terminationReason": "NO_TOOL_CALL",
        }),
    };

    let result = handle_sessionend(&db, &HcomContext::from_os(), &payload);
    assert_eq!(result.exit_code(), 0);
    let instance = db.get_instance_full("vago").unwrap().unwrap();
    assert_eq!(instance.status, ST_LISTENING);
}

#[test]
fn test_antigravity_beforetool_uses_antigravity_status_detail() {
    let (_dir, db) = make_test_db();
    insert_test_instance(&db, "vago", "antigravity");
    db.rebind_session("sess-vago", "vago").unwrap();

    let payload = HookPayload {
        session_id: Some("sess-vago".to_string()),
        transcript_path: None,
        hook_name: "gemini-beforetool".to_string(),
        tool: "antigravity".to_string(),
        tool_name: "run_command".to_string(),
        tool_input: serde_json::json!({ "CommandLine": "cargo test" }),
        tool_result: String::new(),
        notification_type: None,
        raw: serde_json::Value::Null,
    };

    let result = handle_beforetool(&db, &HcomContext::from_os(), &payload);
    assert_eq!(result.exit_code(), 0);

    let detail: String = db
        .conn()
        .query_row(
            "SELECT status_detail FROM instances WHERE name = 'vago'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(detail, "cargo test");
}
