use super::*;

#[test]
fn test_normalize_key() {
    assert_eq!(normalize_key("tag"), "HCOM_TAG");
    assert_eq!(normalize_key("HCOM_TAG"), "HCOM_TAG");
    assert_eq!(normalize_key("terminal"), "HCOM_TERMINAL");
    assert_eq!(normalize_key("hcom_timeout"), "HCOM_TIMEOUT");
}

/// Pins the printed tool list to `auto_approve_managed_tools()` itself,
/// not a hardcoded string — so a future edit that reaches for
/// `hook_tools()` (which still includes the plugin tools) here instead
/// fails this test rather than silently naming tools the command no
/// longer touches.
#[test]
fn auto_approve_enabled_message_names_only_managed_tools() {
    let expected = super::super::hooks::auto_approve_managed_tools()
        .iter()
        .map(|tool| tool.spec().label)
        .collect::<Vec<_>>()
        .join("/");
    let message = super::auto_approve_enabled_message();
    assert_eq!(
        message,
        format!("Auto-approve enabled for safe hcom commands in {expected}")
    );
    // Direct regression guard for the exact mutation caught by hand:
    // reverting to `hook_tools()` re-lists the three plugin tools.
    for tool in super::super::hooks::hook_tools() {
        if tool.hooks_ship_as_plugin() {
            assert!(
                !message.contains(tool.spec().label),
                "plugin tool {} must not appear in: {message}",
                tool.as_str()
            );
        }
    }
}

#[test]
fn test_config_args_json_flag() {
    use clap::Parser;
    let args = ConfigArgs::try_parse_from(["config", "--json", "key"]).unwrap();
    assert!(args.json);
    assert_eq!(args.key, Some("key".to_string()));
}

#[test]
fn test_config_args_info_not_swallowed() {
    use clap::Parser;
    // "hcom config tag --info" should set info=true, not treat --info as value
    let args = ConfigArgs::try_parse_from(["config", "tag", "--info"]).unwrap();
    assert!(args.info);
    assert_eq!(args.key, Some("tag".to_string()));
    assert!(args.value.is_none());
}

#[test]
fn test_config_args_set_value() {
    use clap::Parser;
    let args = ConfigArgs::try_parse_from(["config", "tag", "myvalue"]).unwrap();
    assert_eq!(args.key, Some("tag".to_string()));
    assert_eq!(args.value.as_deref(), Some("myvalue"));
}

#[test]
fn test_config_args_instance() {
    use clap::Parser;
    let args = ConfigArgs::try_parse_from(["config", "-i", "self", "tag", "mytag"]).unwrap();
    assert_eq!(args.instance, Some("self".to_string()));
    assert_eq!(args.key, Some("tag".to_string()));
    assert_eq!(args.value.as_deref(), Some("mytag"));
}

#[test]
fn test_config_args_dev_root_unset() {
    use clap::Parser;
    let args = ConfigArgs::try_parse_from(["config", "dev_root", "--unset"]).unwrap();
    assert_eq!(args.key, Some("dev_root".to_string()));
    assert!(args.unset);
    assert!(args.value.is_none());
}

#[test]
fn test_config_args_hyphen_value() {
    use clap::Parser;
    // "hcom config codex_args '--model o3'" — quoted so shell passes as one token
    let args = ConfigArgs::try_parse_from(["config", "codex_args", "--model o3"]).unwrap();
    assert_eq!(args.key, Some("codex_args".to_string()));
    assert_eq!(args.value.as_deref(), Some("--model o3"));
}

#[test]
fn test_config_args_flags_after_value_not_swallowed() {
    use clap::Parser;
    // "hcom config tag myval --json" should NOT swallow --json as value
    let args = ConfigArgs::try_parse_from(["config", "tag", "myval", "--json"]).unwrap();
    assert_eq!(args.key, Some("tag".to_string()));
    assert_eq!(args.value.as_deref(), Some("myval"));
    assert!(args.json);
}

#[test]
fn test_instance_key_validation() {
    assert!(INSTANCE_KEYS.iter().any(|(k, _)| *k == "tag"));
    assert!(INSTANCE_KEYS.iter().any(|(k, _)| *k == "timeout"));
    assert!(INSTANCE_KEYS.iter().any(|(k, _)| *k == "hints"));
    assert!(INSTANCE_KEYS.iter().any(|(k, _)| *k == "subagent_timeout"));
    assert!(!INSTANCE_KEYS.iter().any(|(k, _)| *k == "invalid"));
}

#[test]
fn test_config_key_info_exists() {
    // All keys should have descriptions
    for (key, desc, typ) in CONFIG_KEYS {
        assert!(!key.is_empty());
        assert!(!desc.is_empty());
        assert!(!typ.is_empty());
    }
}

#[test]
fn test_config_dev_root_set_get_unset() {
    use clap::Parser;

    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();

    let set_args = ConfigArgs::try_parse_from(["config", "dev_root", "/tmp/worktree"]).unwrap();
    assert_eq!(cmd_config(&db, &set_args, None), 0);
    assert_eq!(
        db.kv_get(DEV_ROOT_KV_KEY).unwrap(),
        Some("/tmp/worktree".to_string())
    );

    let get_args = ConfigArgs::try_parse_from(["config", "dev_root"]).unwrap();
    assert_eq!(cmd_config(&db, &get_args, None), 0);

    let unset_args = ConfigArgs::try_parse_from(["config", "dev_root", "--unset"]).unwrap();
    assert_eq!(cmd_config(&db, &unset_args, None), 0);
    assert_eq!(db.kv_get(DEV_ROOT_KV_KEY).unwrap(), None);
}

#[test]
fn test_config_set_accepts_unknown_upstream_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[launch.claude]\nargs = \"--model keep\"\n").unwrap();

    config_set_at_path(&path, "HCOM_CLAUDE_ARGS", "--future-upstream-flag").unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("--future-upstream-flag")
    );
}

#[test]
fn test_config_set_saves_valid_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    config_set_at_path(&path, "HCOM_PI_ARGS", "--model safe-model").unwrap();

    let parsed: toml::Table = std::fs::read_to_string(path).unwrap().parse().unwrap();
    assert_eq!(
        parsed["launch"]["pi"]["args"].as_str(),
        Some("--model safe-model")
    );
}

#[test]
fn bigboss_maps_to_preferences_and_rejects_bad_values() {
    assert_eq!(normalize_key("bigboss"), "HCOM_BIGBOSS");
    assert_eq!(toml_path_for_key("bigboss"), Some("preferences.bigboss"));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    // Normalised CLI spelling writes preferences.bigboss.
    config_set_at_path(&path, "HCOM_BIGBOSS", "lead-agent:CRAY").unwrap();
    let parsed: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(
        parsed["preferences"]["bigboss"].as_str(),
        Some("lead-agent:CRAY")
    );

    assert!(config_set_at_path(&path, "HCOM_BIGBOSS", "two words").is_err());
    assert!(config_set_at_path(&path, "HCOM_BIGBOSS", "*").is_err());

    // CONFIG_KEYS advertises the key so `config --info` can describe it.
    assert!(CONFIG_KEYS.iter().any(|(k, _, _)| *k == "HCOM_BIGBOSS"));
}

#[test]
#[cfg(target_os = "macos")]
fn test_terminal_help_text_lists_cmux_as_managed() {
    crate::config::Config::reset();
    crate::config::Config::init();
    let help = terminal_help_text(false);
    let managed = help
        .split("Other (opens window only):")
        .next()
        .expect("managed section should exist");
    let other = help
        .split("Other (opens window only):")
        .nth(1)
        .expect("other section should exist");

    assert!(managed.contains("cmux"));
    assert!(!other.contains("cmux"));
}

#[test]
fn test_terminal_help_text_documents_new_placeholders() {
    // If you add a placeholder to substitute_open_argv, document it.
    let help = terminal_help_text(false);
    for placeholder in ["{instance_name}", "{tool}", "{cwd}", "{pane_title}"] {
        assert!(
            help.contains(placeholder),
            "missing {placeholder} placeholder docs",
        );
    }
}

#[test]
fn test_config_instance_set_timeout_default_resets() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, wait_timeout) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64(), 15],
        )
        .unwrap();

    config_instance_set(&db, "luna", "timeout", "default").unwrap();
    let timeout: i64 = db
        .conn()
        .query_row(
            "SELECT wait_timeout FROM instances WHERE name = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(timeout, 86400);
}

#[test]
fn test_config_instance_set_empty_hints_clears() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, hints) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64(), "keep me"],
        )
        .unwrap();

    config_instance_set(&db, "luna", "hints", "").unwrap();
    let hints: Option<String> = db
        .conn()
        .query_row(
            "SELECT hints FROM instances WHERE name = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hints, None);
}

#[test]
fn test_config_instance_set_subagent_timeout_default_clears() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, subagent_timeout) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64(), 20],
        )
        .unwrap();

    config_instance_set(&db, "luna", "subagent_timeout", "default").unwrap();
    let timeout: Option<i64> = db
        .conn()
        .query_row(
            "SELECT subagent_timeout FROM instances WHERE name = 'luna'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(timeout, None);
}

#[test]
fn test_render_config_instance_get_scalar_matches_local_style() {
    let rendered =
        render_config_instance_get(&serde_json::json!({"value": 42}), Some("timeout"), false);
    assert_eq!(rendered, "42");
}

#[test]
fn test_render_config_instance_get_null_renders_empty_string() {
    let rendered = render_config_instance_get(
        &serde_json::json!({"value": serde_json::Value::Null}),
        Some("timeout"),
        false,
    );
    assert_eq!(rendered, "");
}

#[test]
fn test_render_config_instance_get_full_output_contract() {
    let rendered = render_config_instance_get(
        &serde_json::json!({
            "name": "team-luna",
            "tag": "team",
            "timeout": 120,
            "hints": "ship it",
            "subagent_timeout": 45,
        }),
        None,
        false,
    );
    assert_eq!(
        rendered,
        "Agent: team-luna\n  tag: team\n  timeout: 120s\n  hints: ship it\n  subagent_timeout: 45s"
    );
}

#[test]
fn test_render_config_instance_get_full_output_uses_empty_state_labels() {
    let rendered = render_config_instance_get(
        &serde_json::json!({
            "name": "luna",
            "tag": "",
            "timeout": 86400,
            "hints": "",
            "subagent_timeout": serde_json::Value::Null,
        }),
        None,
        false,
    );
    assert_eq!(
        rendered,
        "Agent: luna\n  tag: (none)\n  timeout: 86400s\n  hints: (none)\n  subagent_timeout: (default)"
    );
}

#[test]
fn test_render_config_instance_get_json_mode_passthrough() {
    let rendered = render_config_instance_get(
        &serde_json::json!({"value": 42, "name": "luna"}),
        Some("timeout"),
        true,
    );
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["value"], 42);
    assert_eq!(parsed["name"], "luna");
}

#[test]
fn test_config_instance_get_full_output_uses_display_name() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, tag) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64(), "team"],
        )
        .unwrap();

    let config = config_instance_get(&db, "team-luna", None).unwrap();
    // Stable shape: name is the base name, full_name is tag-prefixed.
    assert_eq!(config["name"], "luna");
    assert_eq!(config["full_name"], "team-luna");
}

#[test]
fn test_config_instance_get_timeout_uses_default_value() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES (?1, ?2)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64()],
        )
        .unwrap();

    let config = config_instance_get(&db, "luna", Some("timeout")).unwrap();
    assert_eq!(config["value"], 86400);
}

#[test]
fn test_config_instance_get_full_output_schema() {
    // Stable JSON contract for `hcom config -i <name> --json`. Keep the
    // local and remote (RPC) paths emitting identical shapes:
    //   - name: base name (no tag prefix)
    //   - full_name: tag-prefixed display name
    //   - tag / hints: null when empty or unset (not "")
    //   - timeout: null when the row has no explicit value (registration
    //     paths write the resolved HCOM_TIMEOUT explicitly; a bare INSERT
    //     that skips the column, as here, has nothing to fall back on)
    //   - subagent_timeout: null when unset
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, tag) VALUES (?1, ?2, ?3)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64(), "team"],
        )
        .unwrap();

    let config = config_instance_get(&db, "team-luna", None).unwrap();
    assert_eq!(config["name"], "luna");
    assert_eq!(config["full_name"], "team-luna");
    assert_eq!(config["tag"], "team");
    assert!(
        config["timeout"].is_null(),
        "unset timeout must serialize as null (no schema default; issue #71)"
    );
    assert!(
        config["hints"].is_null(),
        "unset hints must serialize as null"
    );
    assert!(
        config["subagent_timeout"].is_null(),
        "unset subagent_timeout must serialize as null"
    );
}

#[test]
fn test_config_instance_get_full_output_nullifies_empty_strings_and_null_timeout() {
    // Scripts consume these as `tag in (null, "<value>")` so empty
    // strings must be normalized to null. Also covers the legacy case
    // where wait_timeout was explicitly NULL before the schema default
    // existed.
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at, tag, hints, wait_timeout) \
             VALUES (?1, ?2, ?3, ?4, NULL)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64(), "", ""],
        )
        .unwrap();

    let config = config_instance_get(&db, "luna", None).unwrap();
    assert!(config["tag"].is_null(), "empty tag must serialize as null");
    assert!(
        config["hints"].is_null(),
        "empty hints must serialize as null"
    );
    assert!(
        config["timeout"].is_null(),
        "explicit NULL wait_timeout must serialize as null"
    );
}

#[test]
fn test_config_instance_get_subagent_timeout_unset_returns_null_value() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::HcomDb::open_at(&dir.path().join("hcom.db")).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, created_at) VALUES (?1, ?2)",
            rusqlite::params!["luna", crate::shared::time::now_epoch_f64()],
        )
        .unwrap();

    let config = config_instance_get(&db, "luna", Some("subagent_timeout")).unwrap();
    assert_eq!(config["value"], serde_json::Value::Null);
}

#[test]
fn test_render_config_instance_set_feedback_timeout_reset() {
    let rendered = render_config_instance_set_feedback("luna", "timeout", "default");
    assert_eq!(rendered, "Reset timeout for luna");
}

#[test]
fn test_render_config_instance_set_feedback_subagent_timeout_contract() {
    let rendered = render_config_instance_set_feedback("team-luna", "subagent_timeout", "30");
    assert_eq!(rendered, "Set subagent_timeout for team-luna: 30s");
}
