use super::*;
use crate::hooks::test_helpers::{EnvGuard, isolated_test_env};
use serial_test::serial;

#[test]
fn test_hook_payload_factory_uses_native_fields() {
    let payload = HookPayload::from_codex_native(
        "UserPromptSubmit",
        serde_json::json!({
            "session_id": "sess-1",
            "prompt": "<hcom>",
        }),
    );
    assert_eq!(payload.session_id.as_deref(), Some("sess-1"));
    assert_eq!(payload.hook_name, "UserPromptSubmit");
}

#[test]
fn test_derive_transcript_empty_thread_id() {
    assert!(derive_codex_transcript_path("").is_none());
}

#[test]
fn test_derive_transcript_no_match() {
    assert!(derive_codex_transcript_path("nonexistent-thread-12345").is_none());
}

#[test]
fn test_normalize_transcript_path() {
    assert_eq!(
        normalize_codex_transcript_path("C:\\Users\\runner\\session.jsonl"),
        "C:\\Users\\runner\\session.jsonl"
    );
    assert_eq!(
        normalize_codex_transcript_path("\\\\?\\C:\\Users\\runner\\session.jsonl"),
        "C:\\Users\\runner\\session.jsonl"
    );
    assert_eq!(
        normalize_codex_transcript_path("\\\\?\\UNC\\server\\share\\session.jsonl"),
        "\\\\server\\share\\session.jsonl"
    );
}

#[test]
#[serial]
fn test_derive_transcript_finds_file() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions").join("project");
    std::fs::create_dir_all(&sessions).unwrap();

    let transcript = sessions.join("rollout-1-abc-123-def.jsonl");
    std::fs::File::create(&transcript).unwrap();

    let saved = std::env::var("CODEX_HOME").ok();
    unsafe { std::env::set_var("CODEX_HOME", dir.path()) };

    let result = derive_codex_transcript_path("abc-123-def");
    assert!(result.is_some(), "should find transcript file");
    assert!(result.unwrap().contains("rollout-1-abc-123-def.jsonl"));

    if let Some(v) = saved {
        unsafe { std::env::set_var("CODEX_HOME", v) };
    } else {
        unsafe { std::env::remove_var("CODEX_HOME") };
    }
}

// -- build_codex_rules --

#[test]
fn test_build_codex_rules_contains_send() {
    let rules = build_codex_rules();
    assert!(rules.contains("\"send\""));
    assert!(rules.contains("\"list\""));
    assert!(rules.contains("decision=\"allow\""));
}

#[test]
fn test_build_codex_rules_contains_tool_help() {
    let rules = build_codex_rules();
    assert!(rules.contains("\"claude\", \"--help\""));
    assert!(rules.contains("\"gemini\", \"-h\""));
}

// -- settings setup/remove/verify --

#[test]
#[serial]
fn test_setup_and_remove_codex_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.130.0") };
    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));

    let hooks_path = get_codex_hooks_path();
    let config_path = get_codex_config_path();
    let hooks_content = std::fs::read_to_string(hooks_path).unwrap();
    let config_content = std::fs::read_to_string(config_path).unwrap();

    assert!(hooks_content.contains("codex-sessionstart"));
    assert!(config_content.contains("hooks = true"));
    assert!(!config_content.contains("codex-notify"));

    assert!(remove_codex_hooks());
    assert!(!verify_codex_hooks_installed(false));
}

#[test]
#[serial]
fn test_setup_codex_hooks_targets_effective_child_home() {
    let (tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.130.0") };
    let ambient_config = get_codex_config_path();
    let child_home = tmp.path().join("child-codex-home");

    try_setup_codex_hooks_at(false, &child_home).unwrap();

    assert!(child_home.join("config.toml").exists());
    assert!(child_home.join("hooks.json").exists());
    assert!(verify_codex_hooks_installed_at(false, &child_home));
    assert!(!ambient_config.exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn plugin_status_uses_effective_child_home() {
    use crate::instance_binding::EnvVarGuard;
    use std::os::unix::fs::PermissionsExt;

    let (tmp, _, _, _guard) = isolated_test_env();
    let fake = tmp.path().join("codex");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _path = EnvVarGuard::set("PATH", tmp.path().to_str().unwrap());
    let _version = EnvVarGuard::set("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.130.0");
    let _inventory = EnvVarGuard::unset("HCOM_TEST_CODEX_HOOKS_LIST_JSON");
    let parent = tmp.path().join("parent-codex");
    let _parent = EnvVarGuard::set("CODEX_HOME", parent.to_str().unwrap());
    let child = tmp.path().join("child-codex");
    try_setup_codex_hooks_at(false, &child).unwrap();

    assert_eq!(
        codex_plugin_status_at(tmp.path(), &child).state,
        CodexPluginState::LegacyOnly
    );
    assert!(
        !parent.exists(),
        "inspection must not write the parent's home"
    );
}

#[test]
#[serial]
fn test_setup_codex_hooks_trusts_hcom_hooks_for_modern_codex() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.131.0") };

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));

    let config_content = std::fs::read_to_string(get_codex_config_path()).unwrap();
    assert!(config_content.contains("trusted_hash"));
    assert!(config_content.contains("enabled = true"));
    assert!(config_content.contains("hcom_codex_cli_version = \"0.131.0\""));
    assert!(config_content.contains("hcom_hook_definition_hash"));
}

#[test]
#[serial]
fn test_setup_codex_hooks_repairs_disabled_hcom_hook_state() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.131.0") };

    assert!(setup_codex_hooks(false));

    let config_path = get_codex_config_path();
    let content = std::fs::read_to_string(&config_path).unwrap();
    let mut doc = content.parse::<DocumentMut>().unwrap();
    let state = doc["hooks"]["state"].as_table_like_mut().unwrap();
    let first_key = state.iter().next().unwrap().0.to_string();
    state.get_mut(&first_key).unwrap()["enabled"] = value(false);
    paths::atomic_write_io(&config_path, &doc.to_string()).unwrap();

    assert!(!verify_codex_hooks_installed(false));

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));
    let repaired = std::fs::read_to_string(&config_path).unwrap();
    let repaired_doc = repaired.parse::<DocumentMut>().unwrap();
    assert_eq!(
        repaired_doc["hooks"]["state"][&first_key]["enabled"].as_bool(),
        Some(true)
    );
}

#[test]
#[serial]
fn test_setup_codex_hooks_repairs_stale_trusted_hash() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.131.0") };

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));

    let config_path = get_codex_config_path();
    let content = std::fs::read_to_string(&config_path).unwrap();
    let mut doc = content.parse::<DocumentMut>().unwrap();
    let state = doc["hooks"]["state"].as_table_like_mut().unwrap();
    let first_key = state.iter().next().unwrap().0.to_string();
    state.get_mut(&first_key).unwrap()["trusted_hash"] = value("sha256:stale");
    paths::atomic_write_io(&config_path, &doc.to_string()).unwrap();

    // Cheap verify does not spawn Codex app-server to compare currentHash.
    assert!(verify_codex_hooks_installed(false));

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));
    let repaired = std::fs::read_to_string(&config_path).unwrap();
    let repaired_doc = repaired.parse::<DocumentMut>().unwrap();
    assert_ne!(
        repaired_doc["hooks"]["state"][&first_key]["trusted_hash"].as_str(),
        Some("sha256:stale")
    );
}

#[test]
#[serial]
fn test_setup_codex_hooks_repairs_version_stamped_trust_state() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.131.0") };

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));

    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.132.0") };
    assert!(!verify_codex_hooks_installed(false));

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));
    let repaired = std::fs::read_to_string(get_codex_config_path()).unwrap();
    assert!(repaired.contains("hcom_codex_cli_version = \"0.132.0\""));
}

#[test]
#[serial]
fn test_setup_codex_hooks_repairs_drifted_hcom_hook_definition() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    unsafe { std::env::set_var("HCOM_TEST_CODEX_CLI_VERSION", "codex-cli 0.131.0") };

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));

    let hooks_path = get_codex_hooks_path();
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    let mut json: Value = serde_json::from_str(&content).unwrap();
    json["hooks"]["PreToolUse"][0]["hooks"][0]["statusMessage"] =
        Value::String("running".to_string());
    paths::atomic_write_io(&hooks_path, &serde_json::to_string_pretty(&json).unwrap()).unwrap();

    assert!(!verify_codex_hooks_installed(false));

    assert!(setup_codex_hooks(false));
    assert!(verify_codex_hooks_installed(false));
    let repaired: Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks_path).unwrap()).unwrap();
    assert!(
        repaired["hooks"]["PreToolUse"][0]["hooks"][0]
            .get("statusMessage")
            .is_none()
    );
}

// ── Codex plugin activation classifier ──────────────────────────────────

const PLUGIN_ROOT: &str = "/codex-home/plugins/cache/hcom";

/// One inventory entry. `origin` picks the source/sourcePath pair, so a
/// fixture cannot accidentally claim plugin origin with a user path.
fn entry(
    command: &str,
    event: &str,
    origin: &str,
    hooks_path: &Path,
    enabled: bool,
    trust: Option<&str>,
) -> CodexHookListEntry {
    let (source, source_path) = match origin {
        "plugin" => (
            Some(CODEX_HOOK_SOURCE_PLUGIN.to_string()),
            Some(PathBuf::from(PLUGIN_ROOT).join("hooks/hooks-codex.json")),
        ),
        "legacy" => (
            Some(CODEX_HOOK_SOURCE_USER.to_string()),
            Some(hooks_path.to_path_buf()),
        ),
        "project" => (
            Some("project".to_string()),
            Some(PathBuf::from("/repo/.codex/hooks.json")),
        ),
        other => panic!("unknown origin {other}"),
    };
    CodexHookListEntry {
        key: Some(format!(
            "{}:{event}:0:0",
            source_path.as_ref().unwrap().display()
        )),
        command: Some(command.to_string()),
        event_name: Some(event.to_string()),
        plugin_id: (origin == "plugin").then(|| "hcom".to_string()),
        source,
        source_path,
        enabled,
        trust_status: trust.map(str::to_string),
        current_hash: Some("sha256:fixture".to_string()),
    }
}

/// The full correct handler set from one origin.
fn full_set(
    origin: &str,
    hooks_path: &Path,
    enabled: bool,
    trust: Option<&str>,
) -> Vec<CodexHookListEntry> {
    expected_codex_handlers()
        .into_iter()
        .map(|(forms, event)| entry(&forms[0], &event, origin, hooks_path, enabled, trust))
        .collect()
}

fn classify(entries: &[CodexHookListEntry], roots: &[PathBuf]) -> CodexPluginStatus {
    classify_codex_plugin_hooks(entries, Path::new("/codex-home/hooks.json"), roots)
}

fn plugin_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/codex-home/plugins/cache")]
}

#[test]
fn classifier_covers_every_observable_state() {
    let hooks_path = Path::new("/codex-home/hooks.json");
    let complete_plugin = full_set("plugin", hooks_path, true, Some("trusted"));
    let complete_legacy = full_set("legacy", hooks_path, true, Some("trusted"));

    let mut duplicate = complete_plugin.clone();
    duplicate.extend(complete_legacy.clone());

    let mut disabled = complete_plugin.clone();
    disabled[0].enabled = false;

    let untrusted = full_set("plugin", hooks_path, true, Some("untrusted"));
    let unknown_trust = full_set("plugin", hooks_path, true, Some("something-new"));

    // Missing PostToolUse.
    let incomplete: Vec<_> = complete_plugin
        .iter()
        .filter(|e| !e.command.as_deref().unwrap().ends_with("codex-posttooluse"))
        .cloned()
        .collect();

    // Codex reading Claude's hooks.json out of the shared package.
    let claude_handlers: Vec<_> = claude_handler_commands()
        .into_iter()
        .map(|command| {
            entry(
                &command,
                "sessionStart",
                "plugin",
                hooks_path,
                true,
                Some("trusted"),
            )
        })
        .collect();

    // A repo shipping hcom's exact command completes nobody's set.
    let foreign: Vec<_> = expected_codex_handlers()
        .into_iter()
        .map(|(forms, event)| {
            entry(
                &forms[0],
                &event,
                "project",
                hooks_path,
                true,
                Some("trusted"),
            )
        })
        .collect();

    let cases: Vec<(
        &str,
        Vec<CodexHookListEntry>,
        Vec<PathBuf>,
        CodexPluginState,
    )> = vec![
        (
            "complete plugin set",
            complete_plugin.clone(),
            plugin_roots(),
            CodexPluginState::Active,
        ),
        (
            "plugin and legacy",
            duplicate,
            plugin_roots(),
            CodexPluginState::Duplicate,
        ),
        (
            "one handler disabled",
            disabled,
            plugin_roots(),
            CodexPluginState::Disabled,
        ),
        (
            "untrusted",
            untrusted,
            plugin_roots(),
            CodexPluginState::ReviewRequired,
        ),
        (
            "unknown trust status",
            unknown_trust,
            plugin_roots(),
            CodexPluginState::ReviewRequired,
        ),
        (
            "legacy only",
            complete_legacy,
            Vec::new(),
            CodexPluginState::LegacyOnly,
        ),
        (
            "missing PostToolUse",
            incomplete,
            plugin_roots(),
            CodexPluginState::Incomplete,
        ),
        (
            "claude handlers",
            claude_handlers,
            plugin_roots(),
            CodexPluginState::Incompatible,
        ),
        (
            "foreign project hooks",
            foreign,
            Vec::new(),
            CodexPluginState::Missing,
        ),
        (
            "plugin store only",
            Vec::new(),
            plugin_roots(),
            CodexPluginState::Discovered,
        ),
        (
            "nothing at all",
            Vec::new(),
            Vec::new(),
            CodexPluginState::Missing,
        ),
    ];

    for (label, entries, roots, expected) in cases {
        assert_eq!(classify(&entries, &roots).state, expected, "{label}");
    }
}

/// A discovery hint never stands in for a runtime handler: the same empty
/// inventory is `Discovered` with a populated store and `Missing` without,
/// and neither ever reaches an active state.
#[test]
fn a_populated_plugin_store_never_reaches_an_active_state() {
    let status = classify(&[], &plugin_roots());
    assert_eq!(status.state, CodexPluginState::Discovered);
    assert!(!status.state.headline().contains("active"));
}

/// The correct command bound to the wrong event is not a working handler.
#[test]
fn a_handler_on_the_wrong_event_does_not_complete_the_set() {
    let hooks_path = Path::new("/codex-home/hooks.json");
    let mut entries = full_set("plugin", hooks_path, true, Some("trusted"));
    entries[0].event_name = Some("postCompact".to_string());

    let status = classify(&entries, &plugin_roots());
    assert_eq!(status.state, CodexPluginState::Incomplete);
    assert!(
        status.details.iter().any(|d| d.contains("expected")),
        "no wrong-event detail in {:?}",
        status.details
    );
}

/// Task 3 must not widen hcom's ownership predicate: a plugin-sourced entry
/// is never eligible for trust-state writes or the invocation-wide bypass.
#[test]
fn plugin_entries_are_not_hcom_owned_user_hooks() {
    let hooks_path = Path::new("/codex-home/hooks.json");
    let expected = expected_hcom_hook_commands();
    for entry in full_set("plugin", hooks_path, true, Some("trusted")) {
        assert!(!hook_list_entry_is_hcom_owned(
            &entry, &expected, hooks_path
        ));
    }
}

/// Codex returns one group per hook layer; reading only the first would
/// drop a plugin's handlers and report the plugin as missing.
#[test]
fn parser_reads_every_returned_hook_group() {
    let response = serde_json::json!({
        "result": { "data": [
            { "hooks": [{ "key": "a", "command": "hcom codex-stop", "eventName": "stop" }] },
            { "hooks": [{ "key": "b", "command": "hcom codex-sessionstart", "eventName": "sessionStart" }] },
        ]}
    });
    let entries = parse_codex_hook_list_entries(&response).unwrap();
    assert_eq!(entries.len(), 2, "second group dropped: {entries:?}");
    assert_eq!(entries[1].event_name.as_deref(), Some("sessionStart"));
}

/// movu (Codex) found this: the committed overlay ships the self-resolving
/// guard, not the resolved `hcom codex-stop`, so a classifier matching only
/// the resolved form sees none of the plugin's own handlers and reports a
/// live plugin as missing. Built from the manifest hcom actually ships.
#[test]
fn the_shipped_overlay_commands_are_recognized() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../plugin/hcom/hooks/hooks-codex.json")).unwrap();
    let hooks_path = Path::new("/codex-home/hooks.json");

    let entries: Vec<CodexHookListEntry> = CODEX_HOOK_COMMANDS
        .iter()
        .map(|(event, _, _)| {
            let command = manifest["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            entry(
                command,
                &codex_hook_event_wire_name(event),
                "plugin",
                hooks_path,
                true,
                Some("trusted"),
            )
        })
        .collect();

    let status = classify_codex_plugin_hooks(&entries, hooks_path, &plugin_roots());
    assert_eq!(
        status.state,
        CodexPluginState::Active,
        "shipped overlay commands unrecognized: {:?}",
        status.details
    );
}

/// An entry that never says which event it is bound to has not shown it is
/// bound to the right one.
#[test]
fn a_handler_without_event_identity_does_not_count() {
    let hooks_path = Path::new("/codex-home/hooks.json");
    let mut entries = full_set("plugin", hooks_path, true, Some("trusted"));
    entries[0].event_name = None;
    entries[0].key = None;

    assert_eq!(
        classify(&entries, &plugin_roots()).state,
        CodexPluginState::Incomplete
    );
}

/// Double-fire is per event and about what is enabled — not about two
/// complete sets. One enabled legacy handler beside a complete plugin set
/// fires twice; a complete but fully disabled legacy set fires once.
#[test]
fn duplicate_tracks_enabled_overlap_not_two_complete_sets() {
    let hooks_path = Path::new("/codex-home/hooks.json");

    let mut one_legacy = full_set("plugin", hooks_path, true, Some("trusted"));
    one_legacy.push(full_set("legacy", hooks_path, true, Some("trusted")).remove(0));
    assert_eq!(
        classify(&one_legacy, &plugin_roots()).state,
        CodexPluginState::Duplicate,
        "a single enabled legacy handler still double-fires"
    );

    let mut disabled_legacy = full_set("plugin", hooks_path, true, Some("trusted"));
    disabled_legacy.extend(full_set("legacy", hooks_path, false, Some("trusted")));
    assert_eq!(
        classify(&disabled_legacy, &plugin_roots()).state,
        CodexPluginState::Active,
        "a disabled legacy set does not fire"
    );
}

/// A group Codex could not evaluate is not an empty group: swallowing it
/// turns a failed inventory into "nothing installed", which is what decides
/// whether hcom installs.
#[test]
fn a_group_error_or_malformed_group_is_not_an_empty_inventory() {
    let with_errors = serde_json::json!({
        "result": { "data": [{ "hooks": [], "errors": ["config parse failed"] }] }
    });
    assert!(
        parse_codex_hook_list_entries(&with_errors)
            .unwrap_err()
            .contains("reported errors")
    );

    let malformed = serde_json::json!({ "result": { "data": [{ "hooks": "nope" }] } });
    assert!(
        parse_codex_hook_list_entries(&malformed)
            .unwrap_err()
            .contains("without a hooks array")
    );
}

/// Pinned to a real `hooks/list` response: codex-cli 0.154.0, `CODEX_HOME`
/// in a scratch dir carrying hcom's five hooks (2026-09-14). This is where
/// the wire vocabulary is measured rather than assumed — `eventName` is
/// lowerCamelCase while the same entry's `key` segment is snake_case, so a
/// classifier comparing the two would call a healthy install incomplete.
#[test]
fn a_measured_inventory_classifies_as_the_legacy_native_install() {
    let hooks_path = Path::new("/tmp/scratch-codex/hooks.json");
    let measured = serde_json::json!({ "result": { "data": [{
        "cwd": "/work",
        "hooks": [
            { "key": "/tmp/scratch-codex/hooks.json:pre_tool_use:0:0", "eventName": "preToolUse",
              "command": "hcom codex-pretooluse", "matcher": "Bash", "sourcePath": "/tmp/scratch-codex/hooks.json",
              "source": "user", "pluginId": null, "enabled": true, "isManaged": false, "trustStatus": "trusted" },
            { "key": "/tmp/scratch-codex/hooks.json:post_tool_use:0:0", "eventName": "postToolUse",
              "command": "hcom codex-posttooluse", "matcher": "Bash", "sourcePath": "/tmp/scratch-codex/hooks.json",
              "source": "user", "pluginId": null, "enabled": true, "isManaged": false, "trustStatus": "trusted" },
            { "key": "/tmp/scratch-codex/hooks.json:session_start:0:0", "eventName": "sessionStart",
              "command": "hcom codex-sessionstart", "matcher": "startup|resume|clear", "sourcePath": "/tmp/scratch-codex/hooks.json",
              "source": "user", "pluginId": null, "enabled": true, "isManaged": false, "trustStatus": "trusted" },
            { "key": "/tmp/scratch-codex/hooks.json:user_prompt_submit:0:0", "eventName": "userPromptSubmit",
              "command": "hcom codex-userpromptsubmit", "matcher": null, "sourcePath": "/tmp/scratch-codex/hooks.json",
              "source": "user", "pluginId": null, "enabled": true, "isManaged": false, "trustStatus": "trusted" },
            { "key": "/tmp/scratch-codex/hooks.json:stop:0:0", "eventName": "stop",
              "command": "hcom codex-stop", "matcher": null, "sourcePath": "/tmp/scratch-codex/hooks.json",
              "source": "user", "pluginId": null, "enabled": true, "isManaged": false, "trustStatus": "trusted" }
        ],
        "warnings": [], "errors": []
    }]}});

    let entries = parse_codex_hook_list_entries(&measured).unwrap();
    assert_eq!(entries.len(), 5);
    let status = classify_codex_plugin_hooks(&entries, hooks_path, &[]);
    assert_eq!(
        status.state,
        CodexPluginState::LegacyOnly,
        "measured inventory misclassified: {:?}",
        status.details
    );
}

// ── Task 4: add/remove routing ──────────────────────────────────────────

fn status_of(state: CodexPluginState) -> CodexPluginStatus {
    CodexPluginStatus {
        state,
        details: vec!["detail".to_string()],
    }
}

#[test]
fn add_routes_every_state_without_installing_on_its_own() {
    use ClaudePresence::*;
    use CodexAddPlan::{InstallNatively, Report};

    let present = Present;
    let absent = Absent;
    let unknown = Indeterminate("permission denied".to_string());

    // Nothing but a complete, trusted, enabled set counts as done.
    assert_eq!(
        plan_codex_add(&status_of(CodexPluginState::Active), &absent, false),
        Report(CodexAddOutcome::AlreadyActive)
    );

    // Every state that needs the user says so, and installs nothing.
    for state in [
        CodexPluginState::Duplicate,
        CodexPluginState::ReviewRequired,
        CodexPluginState::Disabled,
        CodexPluginState::Incompatible,
        CodexPluginState::Unverified,
        CodexPluginState::LegacyOnly,
    ] {
        let outcome = plan_codex_add(&status_of(state), &absent, false);
        assert!(
            matches!(outcome, Report(CodexAddOutcome::ActionRequired(_))),
            "{state:?} did not stop for the user: {outcome:?}"
        );
    }

    // Duplicate must point at the legacy-only removal, never a plain
    // remove (which would take the plugin down too).
    let Report(CodexAddOutcome::ActionRequired(text)) =
        plan_codex_add(&status_of(CodexPluginState::Duplicate), &absent, false)
    else {
        panic!("duplicate must be action-required");
    };
    assert!(text.contains("--legacy-only"), "{text}");

    // An unusable inventory installs nothing, whatever Claude's state is.
    for claude in [&present, &absent, &unknown] {
        assert!(
            matches!(
                plan_codex_add(&status_of(CodexPluginState::Unverified), claude, false),
                Report(CodexAddOutcome::ActionRequired(_))
            ),
            "unverified installed anyway with claude {claude:?}"
        );
    }

    // Claude present: import guidance, and the prerequisite only when
    // Claude's own plugin is not installed yet.
    let Report(CodexAddOutcome::ActionRequired(text)) =
        plan_codex_add(&status_of(CodexPluginState::Missing), &present, false)
    else {
        panic!("claude present must be action-required");
    };
    assert!(text.contains("/import"), "{text}");
    assert!(text.contains("hcom hooks add claude"), "{text}");
    let Report(CodexAddOutcome::ActionRequired(text)) =
        plan_codex_add(&status_of(CodexPluginState::Missing), &present, true)
    else {
        panic!("claude present must be action-required");
    };
    assert!(!text.contains("hcom hooks add claude"), "{text}");

    // Indeterminate never writes.
    assert!(matches!(
        plan_codex_add(&status_of(CodexPluginState::Missing), &unknown, false),
        Report(CodexAddOutcome::ActionRequired(_))
    ));

    // Only a confirmed absence reaches the installer.
    for state in [
        CodexPluginState::Missing,
        CodexPluginState::Incomplete,
        CodexPluginState::Discovered,
    ] {
        assert_eq!(
            plan_codex_add(&status_of(state), &absent, false),
            InstallNatively,
            "{state:?}"
        );
    }
}

/// The probe must distinguish "no Claude here" from "Claude did not
/// answer": only the first may install, and a wrong answer creates the
/// duplicate the import route exists to avoid.
#[cfg(unix)]
#[test]
#[serial]
fn claude_presence_separates_absence_from_an_unusable_binary() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let original = std::env::var_os("PATH");
    let original_home = std::env::var_os("HOME");
    let restore = |key: &str, value: &Option<std::ffi::OsString>| match value {
        Some(path) => unsafe { std::env::set_var(key, path) },
        None => unsafe { std::env::remove_var(key) },
    };
    unsafe { std::env::set_var("PATH", dir.path()) };
    // `which_bin` also probes well-known install locations under $HOME, so
    // an isolated PATH alone would still find this machine's real Claude.
    unsafe { std::env::set_var("HOME", dir.path()) };

    assert_eq!(claude_presence(), ClaudePresence::Absent, "empty PATH");

    let fake = dir.path().join("claude");
    let write = |body: &str| {
        std::fs::write(&fake, body).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    };

    write("#!/bin/sh\nexit 0\n");
    assert_eq!(claude_presence(), ClaudePresence::Present);

    write("#!/bin/sh\nexit 3\n");
    assert!(
        matches!(claude_presence(), ClaudePresence::Indeterminate(_)),
        "a non-zero exit is not proof Claude is absent"
    );

    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        matches!(claude_presence(), ClaudePresence::Indeterminate(_)),
        "an unrunnable binary is not proof Claude is absent"
    );

    restore("PATH", &original);
    restore("HOME", &original_home);
}

/// A malformed response is unverified, never "not installed".
#[test]
fn a_schema_error_is_not_a_verdict_about_installation() {
    let error = parse_codex_hook_list_entries(&serde_json::json!({ "result": {} })).unwrap_err();
    assert!(error.contains("did not contain hooks"), "{error}");
    assert_eq!(CodexPluginState::Unverified.headline(), "state unverified");
}

// ── GHSA-pwv3-8r7h-p373: hook identity must be source-scoped ────────────

/// hooks/list entries for hcom's own five hooks, plus whatever `extra` adds.
fn hooks_list_value(hooks_path: &Path, extra: Vec<Value>) -> Value {
    let mut hooks: Vec<Value> = test_expected_hook_specs()
        .into_iter()
        .enumerate()
        .map(|(index, (event_label, command))| {
            serde_json::json!({
                "key": format!("{}:{event_label}:0:0", hooks_path.display()),
                "command": command,
                "source": "user",
                "sourcePath": hooks_path.to_string_lossy(),
                "enabled": true,
                "trustStatus": "untrusted",
                "currentHash": format!("sha256:list-{index}"),
            })
        })
        .collect();
    hooks.extend(extra);
    serde_json::json!({ "result": { "data": [{ "hooks": hooks }] } })
}

#[test]
#[serial]
fn test_impersonating_project_hook_gets_no_trust_entry() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    let impersonated = test_expected_hook_specs()[0].1.clone();
    let response = hooks_list_value(
        &hooks_path,
        vec![serde_json::json!({
            "key": "/repo/.codex/hooks.json:pre_tool_use:0:0",
            "command": impersonated,
            "source": "project",
            "sourcePath": "/repo/.codex/hooks.json",
            "enabled": true,
            "trustStatus": "untrusted",
            "currentHash": "sha256:impostor",
        })],
    );

    let entries = parse_hcom_hook_entries_from_hooks_list(&response).unwrap();
    assert_eq!(entries.len(), CODEX_HOOK_COMMANDS.len());
    assert!(
        entries.iter().all(|entry| !entry.key.contains("/repo/")),
        "a project hook copying an hcom command must not receive trust state"
    );
}

#[test]
#[serial]
fn test_hcom_commands_from_another_source_path_are_not_hcom() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    // Same commands, same user layer, but a different file: not hcom's.
    let response = hooks_list_value(Path::new("/elsewhere/hooks.json"), Vec::new());

    let error = parse_hcom_hook_entries_from_hooks_list(&response)
        .expect_err("entries from a foreign hooks file must not count as hcom's");
    assert!(error.contains("missing hcom hooks"), "{error}");
}

#[test]
#[serial]
fn test_write_hook_trust_state_refuses_foreign_key() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[features]\nhooks = true\n").unwrap();

    let entries = vec![CodexHookTrustEntry {
        key: "/repo/.codex/hooks.json:pre_tool_use:0:0".to_string(),
        command: test_expected_hook_specs()[0].1.clone(),
        current_hash: "sha256:impostor".to_string(),
    }];
    let error = write_hcom_hook_trust_state(
        &config_path,
        &hooks_path,
        &entries,
        &HashSet::new(),
        "0.131.0",
        &HashMap::new(),
    )
    .expect_err("a key outside hcom's hooks.json must be refused");
    assert!(
        error.contains("does not belong to hcom's own hooks file"),
        "{error}"
    );
    assert!(
        !std::fs::read_to_string(&config_path)
            .unwrap()
            .contains("trusted_hash"),
        "the refused entry must not have been written"
    );
}

#[test]
#[serial]
fn test_hook_state_key_ownership() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    let key = format!("{}:pre_tool_use:0:0", hooks_path.display());
    assert!(hook_state_key_belongs_to_hcom_hooks_json(&key, &hooks_path));
    // Same file, but an event hcom does not install.
    assert!(!hook_state_key_belongs_to_hcom_hooks_json(
        &format!("{}:session_end:0:0", hooks_path.display()),
        &hooks_path
    ));
    // A different file entirely.
    assert!(!hook_state_key_belongs_to_hcom_hooks_json(
        "/repo/.codex/hooks.json:pre_tool_use:0:0",
        &hooks_path
    ));
    // Malformed positional suffix.
    assert!(!hook_state_key_belongs_to_hcom_hooks_json(
        &format!("{}:pre_tool_use:x:0", hooks_path.display()),
        &hooks_path
    ));
}

#[test]
#[serial]
fn test_foreign_hooks_unlocked_by_bypass_ignores_trusted_and_hcom() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    let response = hooks_list_value(
        &hooks_path,
        vec![
            // Already trusted — the flag changes nothing for it.
            serde_json::json!({
                "key": "/repo/.codex/hooks.json:stop:0:0",
                "command": "trusted-tool",
                "source": "project",
                "sourcePath": "/repo/.codex/hooks.json",
                "enabled": true,
                "trustStatus": "trusted",
                "currentHash": "sha256:a",
            }),
            // Disabled — the flag does not enable it.
            serde_json::json!({
                "key": "/repo/.codex/hooks.json:stop:1:0",
                "command": "disabled-tool",
                "source": "project",
                "sourcePath": "/repo/.codex/hooks.json",
                "enabled": false,
                "trustStatus": "untrusted",
                "currentHash": "sha256:b",
            }),
        ],
    );
    let entries = parse_codex_hook_list_entries(&response).unwrap();
    assert!(foreign_hooks_unlocked_by_bypass(&entries, &hooks_path).is_empty());
}

#[test]
#[serial]
fn test_foreign_hooks_unlocked_by_bypass_flags_modified_and_unknown() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    let response = hooks_list_value(
        &hooks_path,
        vec![
            serde_json::json!({
                "key": "/repo/.codex/hooks.json:stop:0:0",
                "command": "modified-tool",
                "source": "project",
                "sourcePath": "/repo/.codex/hooks.json",
                "enabled": true,
                "trustStatus": "modified",
                "currentHash": "sha256:a",
            }),
            // No trustStatus at all: hcom cannot prove it is safe.
            serde_json::json!({
                "key": "/plugin/hooks.json:stop:0:0",
                "command": "plugin-tool",
                "source": "plugin",
                "sourcePath": "/plugin/hooks.json",
                "enabled": true,
                "currentHash": "sha256:b",
            }),
        ],
    );
    let entries = parse_codex_hook_list_entries(&response).unwrap();
    let foreign = foreign_hooks_unlocked_by_bypass(&entries, &hooks_path);
    assert_eq!(foreign.len(), 2, "{foreign:?}");
    assert!(foreign.iter().any(|f| f.contains("modified-tool")));
    assert!(foreign.iter().any(|f| f.contains("plugin-tool")));
}

#[test]
fn test_paths_equivalent_handles_dot_components() {
    assert!(paths_equivalent(
        Path::new("/home/u/.codex/./hooks.json"),
        Path::new("/home/u/.codex/hooks.json")
    ));
    assert!(paths_equivalent(
        Path::new("/home/u/other/../.codex/hooks.json"),
        Path::new("/home/u/.codex/hooks.json")
    ));
    assert!(!paths_equivalent(
        Path::new("/home/u/.codex/hooks.json"),
        Path::new("/repo/.codex/hooks.json")
    ));
}

#[test]
fn test_hcom_command_for_hook_state_key() {
    assert_eq!(
        hcom_command_for_hook_state_key("/tmp/codex/hooks.json:pre_tool_use:0:0"),
        build_codex_hook_command("codex-pretooluse")
    );
}

#[test]
#[serial]
fn test_setup_preserves_unrelated_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "other-hook"}]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    assert!(setup_codex_hooks(false));
    let content = std::fs::read_to_string(hooks_path).unwrap();
    assert!(content.contains("other-hook"));
    assert!(content.contains("codex-posttooluse"));
}

#[test]
#[serial]
fn test_mixed_group_merge_preserves_user_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [
                        {"type": "command", "command": "user-mixed-hook"},
                        {"type": "command", "command": "old-path codex-posttooluse"}
                    ]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    assert!(setup_codex_hooks(false));
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    assert!(content.contains("user-mixed-hook"), "user hook was dropped");
    assert!(content.contains("codex-posttooluse"), "hcom hook missing");
    let json: Value = serde_json::from_str(&content).unwrap();
    let posttool_groups = json["hooks"]["PostToolUse"].as_array().unwrap();
    let bash_group = posttool_groups
        .iter()
        .find(|g| g.get("matcher").and_then(|v| v.as_str()) == Some("Bash"))
        .expect("Bash group missing");
    let hook_cmds: Vec<&str> = bash_group["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h.get("command").and_then(|v| v.as_str()))
        .collect();
    let hcom_count = hook_cmds
        .iter()
        .filter(|c| c.contains("codex-posttooluse"))
        .count();
    assert_eq!(
        hcom_count, 1,
        "expected exactly one hcom hook, got {hcom_count}"
    );
}

#[test]
#[serial]
fn test_mixed_group_remove_preserves_user_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[features]\nhooks = true\n").unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash",
                    "hooks": [
                        {"type": "command", "command": "user-remove-hook"},
                        {"type": "command", "command": "old-path codex-posttooluse"}
                    ]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    assert!(remove_codex_hooks());
    assert!(
        hooks_path.exists(),
        "hooks.json was deleted but user hook was present"
    );
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    assert!(
        content.contains("user-remove-hook"),
        "user hook was dropped"
    );
    assert!(
        !content.contains("codex-posttooluse"),
        "hcom hook was not removed"
    );
}

#[test]
#[serial]
fn test_ensure_feature_enabled_preserves_unrelated_notify() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "notify = \"some-other-notify-tool\"\n").unwrap();

    assert!(setup_codex_hooks(false));
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("some-other-notify-tool"),
        "unrelated notify was removed"
    );
    assert!(content.contains("hooks = true"), "feature flag not set");
}

#[test]
#[serial]
fn test_ensure_feature_enabled_preserves_notify_with_codex_notify_but_no_hcom_owner() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "notify = \"other-tool codex-notify\"\n").unwrap();

    assert!(setup_codex_hooks(false));
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("other-tool codex-notify"),
        "non-hcom notify mentioning codex-notify was removed"
    );
    assert!(content.contains("hooks = true"), "feature flag not set");
}

#[test]
#[serial]
fn test_ensure_feature_enabled_removes_hcom_notify() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "notify = \"hcom internal codex-notify --name luna\"\n",
    )
    .unwrap();

    assert!(setup_codex_hooks(false));
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !content.contains("notify"),
        "hcom notify key was not removed"
    );
    assert!(content.contains("hooks = true"), "feature flag not set");
}

#[test]
#[serial]
fn test_remove_codex_hooks_preserves_feature_flag() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    assert!(setup_codex_hooks(false));

    let config_path = get_codex_config_path();
    let before = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        before.contains("hooks = true"),
        "setup did not enable feature flag"
    );

    assert!(remove_codex_hooks());
    let after = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        after.contains("hooks = true"),
        "feature flag should be preserved"
    );
}

#[test]
#[serial]
fn test_setup_codex_creates_execpolicy() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    assert!(setup_codex_hooks(true));

    let rules_file = get_codex_rules_path().join("hcom.rules");
    assert!(rules_file.exists(), "execpolicy rules should be created");
    let content = std::fs::read_to_string(&rules_file).unwrap();
    assert!(content.contains("hcom"));
}

#[test]
#[serial]
fn test_remove_codex_removes_execpolicy() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    assert!(setup_codex_hooks(true));
    let rules_file = get_codex_rules_path().join("hcom.rules");
    assert!(rules_file.exists());

    assert!(remove_codex_hooks());
    assert!(!rules_file.exists(), "execpolicy rules should be removed");
}

#[test]
#[serial]
fn test_remove_codex_noop_when_no_hooks_json() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    assert!(remove_codex_hooks());
}

#[test]
#[serial]
fn test_codex_feature_enabled_with_fallback() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[features]\ncodex_hooks = true\n").unwrap();

    // Both keys resolve because the selected key is checked first,
    // then the alternate acts as a fallback.
    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::CodexHooks
    ));
    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::Hooks
    ));

    // Reverse: only hooks key present.
    std::fs::write(&config_path, "[features]\nhooks = true\n").unwrap();
    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::Hooks
    ));
    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::CodexHooks
    ));
}

#[test]
#[serial]
fn test_ensure_feature_upgrade_cleans_stale_codex_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    // Seed config with the deprecated key, simulating an old hcom install.
    std::fs::write(&config_path, "[features]\ncodex_hooks = true\n").unwrap();

    ensure_codex_feature_enabled(&config_path, CodexHooksFeatureKey::Hooks).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("hooks = true"), "upgrade should set hooks");
    assert!(
        !content.contains("codex_hooks"),
        "upgrade should remove stale codex_hooks"
    );
}

#[test]
#[serial]
fn test_ensure_feature_upgrade_cleans_profile_stale_codex_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
            &config_path,
            "profile = \"work\"\n\n[features]\nhooks = true\n\n[profiles.work.features]\ncodex_hooks = true\n",
        )
        .unwrap();

    assert!(!codex_current_feature_enabled());

    ensure_codex_feature_enabled(&config_path, CodexHooksFeatureKey::Hooks).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("hooks = true"), "upgrade should set hooks");
    assert!(
        !content.contains("codex_hooks"),
        "upgrade should remove stale profile codex_hooks"
    );
    assert!(codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_ensure_feature_upgrade_cleans_inline_profile_stale_codex_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
            &config_path,
            "profile = \"work\"\nprofiles = { work = { features = { codex_hooks = true } } }\n\n[features]\nhooks = true\n",
        )
        .unwrap();

    assert!(!codex_current_feature_enabled());

    ensure_codex_feature_enabled(&config_path, CodexHooksFeatureKey::Hooks).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("hooks = true"), "upgrade should set hooks");
    assert!(
        !content.contains("codex_hooks"),
        "upgrade should remove stale inline profile codex_hooks"
    );
    assert!(codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_current_feature_enabled_requires_selected_key() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[features]\ncodex_hooks = true\n").unwrap();

    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::Hooks
    ));
    assert!(!codex_current_feature_enabled());

    ensure_codex_feature_enabled(&config_path, CodexHooksFeatureKey::Hooks).unwrap();
    assert!(codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_current_feature_enabled_rejects_mixed_deprecated_key() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "[features]\nhooks = true\ncodex_hooks = true\n",
    )
    .unwrap();

    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::Hooks
    ));
    assert!(!codex_current_feature_enabled());

    ensure_codex_feature_enabled(&config_path, CodexHooksFeatureKey::Hooks).unwrap();
    assert!(codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_current_feature_enabled_rejects_profile_deprecated_key() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
            &config_path,
            "profile = \"work\"\n\n[features]\nhooks = true\n\n[profiles.work.features]\ncodex_hooks = true\n",
        )
        .unwrap();

    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::Hooks
    ));
    assert!(!codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_current_feature_enabled_ignores_inactive_profile_deprecated_key() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "[features]\nhooks = true\n\n[profiles.work.features]\ncodex_hooks = true\n",
    )
    .unwrap();

    assert!(codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_current_feature_enabled_rejects_inline_profile_deprecated_key() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
            &config_path,
            "profile = \"work\"\nprofiles = { work = { features = { codex_hooks = true } } }\n\n[features]\nhooks = true\n",
        )
        .unwrap();

    assert!(codex_feature_enabled(
        &config_path,
        CodexHooksFeatureKey::Hooks
    ));
    assert!(!codex_current_feature_enabled());
}

#[test]
#[serial]
fn test_ensure_feature_downgrade_uses_codex_hooks() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let config_path = get_codex_config_path();
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[features]\nhooks = true\n").unwrap();

    ensure_codex_feature_enabled(&config_path, CodexHooksFeatureKey::CodexHooks).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    let doc = content.parse::<DocumentMut>().unwrap();
    let features = doc.get("features").unwrap();
    assert!(
        features.get("codex_hooks").and_then(|v| v.as_bool()) == Some(true),
        "old Codex should use codex_hooks"
    );
    // hooks is the shared flag for all Codex hooks — not just hcom's.
    // hcom must not delete it even when writing for an older Codex.
    assert!(
        features.get("hooks").and_then(|v| v.as_bool()) == Some(true),
        "shared hooks flag should be preserved"
    );
}

#[test]
fn test_codex_hooks_feature_key_version_gate() {
    assert_eq!(
        codex_hooks_feature_key_for_version((0, 128, 0)),
        CodexHooksFeatureKey::CodexHooks
    );
    assert_eq!(
        codex_hooks_feature_key_for_version((0, 129, 0)),
        CodexHooksFeatureKey::Hooks
    );
    assert_eq!(
        parse_codex_cli_version("codex-cli 0.129.0"),
        Some((0, 129, 0))
    );
}

// ── regression: legacy "cmd"-format cleanup ─────────────────────────────

/// Old hcom versions wrote hooks as {"type":"cmd","cmd":"hcom codex-..."}.
/// On Codex >= 0.129 (CODEX_HOOKS_FEATURE_RENAME_VERSION), try_setup_codex_hooks
/// must remove those stale entries and replace them with the current format.
///
/// FAILS before the fix: remove_legacy_hcom_cmd_hooks_from_json is defined
/// but not yet called from try_setup_codex_hooks.
#[test]
#[serial]
fn test_legacy_cmd_hooks_cleaned_on_new_codex() {
    // isolated_test_env sets HCOM_TEST_CODEX_CLI_VERSION = "codex-cli 0.129.0"
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(
        &hooks_path,
        serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{"type": "cmd", "cmd": "hcom codex-userpromptsubmit"}]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    assert!(try_setup_codex_hooks(false).is_ok());
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    let hooks_json: Value = serde_json::from_str(&content).unwrap();
    let user_prompt_hooks = hooks_json["hooks"]["UserPromptSubmit"][0]["hooks"]
        .as_array()
        .unwrap();
    assert!(
        !user_prompt_hooks
            .iter()
            .any(|hook| hook["type"] == "cmd" && hook.get("cmd").is_some()),
        "legacy cmd-keyed entry must be removed on Codex >= 0.129"
    );
    assert!(
        user_prompt_hooks.iter().any(|hook| {
            hook["type"] == "command"
                && hook["command"] == build_codex_hook_command("codex-userpromptsubmit")
        }),
        "current command-keyed entry must be present after cleanup"
    );
}

// ── regression: context-mode "matcher":"" groups must not block verify ──

/// context-mode writes groups with "matcher":"" for None-matcher events
/// (UserPromptSubmit, Stop).  Those groups appear before hcom's no-matcher
/// groups in the file.  verify_hooks_json_value must not pick the wrong
/// group and report HookCommandMissing.
#[test]
#[serial]
fn test_context_mode_empty_matcher_does_not_block_verify() {
    let (_tmp, _hcom_dir, _home, _guard) = isolated_test_env();
    let hooks_path = get_codex_hooks_path();
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    // Seed with context-mode "matcher":"" groups appearing FIRST for both
    // None-matcher events, matching the live ~/.codex/hooks.json layout.
    std::fs::write(
            &hooks_path,
            serde_json::json!({
                "hooks": {
                    "UserPromptSubmit": [{
                        "matcher": "",
                        "hooks": [{"type": "command", "command": "context-mode hook codex userpromptsubmit"}]
                    }],
                    "Stop": [{
                        "matcher": "",
                        "hooks": [{"type": "command", "command": "context-mode hook codex stop"}]
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();

    assert!(
        try_setup_codex_hooks(false).is_ok(),
        "setup must succeed even when another tool owns a \"matcher\":\"\" group for the same event"
    );
    let content = std::fs::read_to_string(&hooks_path).unwrap();
    assert!(
        content.contains("context-mode hook codex userpromptsubmit"),
        "third-party hook must be preserved"
    );
    assert!(
        content.contains("codex-userpromptsubmit"),
        "hcom hook must be present"
    );
}

#[test]
#[serial]
fn remove_codex_hooks_cleans_active_hcom_dir_local_path() {
    let _guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let local_dir = workspace.join(".codex");
    std::fs::create_dir_all(local_dir.join("rules")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", workspace.join(".hcom"));
        std::env::remove_var("CODEX_HOME");
    }
    std::fs::write(
        local_dir.join("hooks.json"),
        serde_json::to_string_pretty(&build_expected_hook_json()).unwrap(),
    )
    .unwrap();
    std::fs::write(local_dir.join("rules/hcom.rules"), "allow").unwrap();

    assert!(remove_codex_hooks());
    assert!(!local_dir.join("rules/hcom.rules").exists());
    if local_dir.join("hooks.json").exists() {
        let content = std::fs::read_to_string(local_dir.join("hooks.json")).unwrap();
        assert!(!content.contains("codex-"));
    }
}
