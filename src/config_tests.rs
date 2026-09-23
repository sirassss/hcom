use super::*;
use crate::hooks::test_helpers::{EnvGuard, isolated_test_env};
use serial_test::serial;
use std::env;

/// Helper to set env var for test scope
fn with_env<F>(key: &str, value: &str, f: F)
where
    F: FnOnce(),
{
    // SAFETY: Tests use serial_test to run single-threaded.
    unsafe {
        env::set_var(key, value);
    }
    f();
    unsafe {
        env::remove_var(key);
    }
}

/// Helper to clear multiple env vars for test scope
fn without_env<F>(keys: &[&str], f: F)
where
    F: FnOnce(),
{
    let saved: Vec<_> = keys.iter().map(|k| (*k, env::var(k).ok())).collect();
    for key in keys {
        unsafe {
            env::remove_var(key);
        }
    }
    f();
    for (key, val) in saved {
        if let Some(v) = val {
            unsafe {
                env::set_var(key, v);
            }
        }
    }
}

// Unix-only: asserts against $HOME and POSIX absolute paths; Windows
// resolves the base dir from USERPROFILE and treats "/x" as drive-relative.
#[test]
#[serial]
fn test_guard_redirects_non_temp_hcom_dir() {
    let _guard = EnvGuard::new();
    let unsafe_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".hcom-unsafe-test");
    unsafe {
        env::set_var("HCOM_DIR", &unsafe_dir);
    }
    Config::reset();
    Config::init();

    let actual = Config::get().hcom_dir;
    assert_ne!(actual, unsafe_dir);
    assert!(actual.starts_with(env::temp_dir()), "actual={actual:?}");
}

#[test]
#[serial]
fn test_guard_allows_registered_hcom_dir() {
    let _guard = EnvGuard::new();
    let temp = tempfile::tempdir().unwrap();
    let expected = temp.path().join(".hcom");
    // A fixture must claim the root before Config will keep it.
    paths::test_roots::register(temp.path());
    unsafe {
        env::set_var("HCOM_DIR", &expected);
    }
    Config::reset();
    Config::init();

    assert_eq!(Config::get().hcom_dir, expected);
}

#[test]
#[serial]
fn test_guard_redirects_unregistered_temp_hcom_dir() {
    // Geography is not ownership: a temp path no fixture registered is not
    // trusted, even though it sits under $TMPDIR. This is the finding-3
    // guarantee that a real hcom DB happening to live under /tmp is not
    // waved through.
    let _guard = EnvGuard::new();
    let temp = tempfile::tempdir().unwrap();
    let unregistered = temp.path().join(".hcom");
    unsafe {
        env::set_var("HCOM_DIR", &unregistered);
    }
    Config::reset();
    Config::init();

    assert_ne!(Config::get().hcom_dir, unregistered);
}

#[cfg(unix)]
#[test]
#[serial]
fn test_guard_rejects_temp_symlink_to_non_temp_hcom_dir() {
    use std::os::unix::fs::symlink;

    let _guard = EnvGuard::new();
    let temp = tempfile::tempdir().unwrap();
    let link = temp.path().join("outside");
    symlink(env!("CARGO_MANIFEST_DIR"), &link).unwrap();
    let unsafe_dir = link.join(".hcom");
    unsafe {
        env::set_var("HCOM_DIR", &unsafe_dir);
    }
    Config::reset();
    Config::init();

    let actual = Config::get().hcom_dir;
    assert_ne!(actual, unsafe_dir);
    assert!(actual.starts_with(env::temp_dir()), "actual={actual:?}");
}

#[test]
#[serial]
fn test_raw_resolution_and_db_open_do_not_share_mutable_escape_state() {
    use std::sync::{Arc, Barrier};

    let (_dir, hcom_dir, _home, _guard) = isolated_test_env();
    let barrier = Arc::new(Barrier::new(2));
    let raw_barrier = Arc::clone(&barrier);
    let db_barrier = Arc::clone(&barrier);
    let raw_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".hcom-resolution-test");
    let expected_db = hcom_dir.join("hcom.db");

    let resolver = std::thread::spawn(move || {
        let env = HashMap::from([(
            "HCOM_DIR".to_string(),
            raw_path.to_string_lossy().into_owned(),
        )]);
        raw_barrier.wait();
        let (resolved, explicit) =
            paths::resolve_hcom_dir_from_env(&env, std::path::Path::new("/worktree"));
        assert_eq!(resolved, raw_path);
        assert!(explicit);
    });
    let db_open = std::thread::spawn(move || {
        db_barrier.wait();
        let db = crate::db::HcomDb::open().unwrap();
        assert_eq!(db.path(), expected_db);
    });

    resolver.join().unwrap();
    db_open.join().unwrap();
}

#[cfg(unix)]
#[test]
fn test_default_config_uses_home_hcom() {
    let env = HashMap::from([("HOME".to_string(), "/home/test".to_string())]);
    let (actual, explicit) =
        paths::resolve_hcom_dir_from_env(&env, std::path::Path::new("/worktree"));

    assert_eq!(actual, PathBuf::from("/home/test/.hcom"));
    assert!(!explicit);
}

#[cfg(unix)]
#[test]
fn test_hcom_dir_overrides_home() {
    let env = HashMap::from([("HCOM_DIR".to_string(), "/custom/hcom".to_string())]);
    let (actual, explicit) =
        paths::resolve_hcom_dir_from_env(&env, std::path::Path::new("/worktree"));

    assert_eq!(actual, PathBuf::from("/custom/hcom"));
    assert!(explicit);
}

#[test]
#[serial]
fn test_instance_name_some_when_set() {
    Config::reset();
    with_env("HCOM_INSTANCE_NAME", "test-instance", || {
        Config::init();
        let config = Config::get();
        assert_eq!(config.instance_name, Some("test-instance".to_string()));
    });
}

#[test]
#[serial]
fn test_instance_name_none_when_unset() {
    Config::reset();
    without_env(&["HCOM_INSTANCE_NAME"], || {
        Config::init();
        let config = Config::get();
        assert_eq!(config.instance_name, None);
    });
}

#[test]
#[serial]
fn test_process_id_some_when_set() {
    Config::reset();
    with_env("HCOM_PROCESS_ID", "pid-123", || {
        Config::init();
        let config = Config::get();
        assert_eq!(config.process_id, Some("pid-123".to_string()));
    });
}

#[test]
#[serial]
fn test_process_id_none_when_unset() {
    Config::reset();
    without_env(&["HCOM_PROCESS_ID"], || {
        Config::init();
        let config = Config::get();
        assert_eq!(config.process_id, None);
    });
}

#[test]
#[serial]
fn test_reset_allows_reinit() {
    Config::reset();
    with_env("HCOM_INSTANCE_NAME", "first", || {
        Config::init();
        assert_eq!(Config::get().instance_name, Some("first".to_string()));
    });

    Config::reset();
    with_env("HCOM_INSTANCE_NAME", "second", || {
        Config::init();
        assert_eq!(Config::get().instance_name, Some("second".to_string()));
    });
}

#[test]
fn test_hcom_dir_tilde_expansion() {
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let env = HashMap::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("HCOM_DIR".to_string(), "~/.hcom".to_string()),
    ]);
    let (actual, explicit) =
        paths::resolve_hcom_dir_from_env(&env, std::path::Path::new("/worktree"));

    assert_eq!(actual, home.join(".hcom"));
    assert!(explicit);
}

#[test]
fn test_hcom_dir_relative_resolved_to_absolute() {
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let env = HashMap::from([("HCOM_DIR".to_string(), "relative/path".to_string())]);
    let (actual, explicit) = paths::resolve_hcom_dir_from_env(&env, &cwd);

    assert_eq!(actual, cwd.join("relative/path"));
    assert!(explicit);
}

#[cfg(unix)]
#[test]
fn test_hcom_dir_absolute_stays_absolute() {
    let env = HashMap::from([("HCOM_DIR".to_string(), "/absolute/hcom".to_string())]);
    let (actual, explicit) =
        paths::resolve_hcom_dir_from_env(&env, std::path::Path::new("/worktree"));

    assert_eq!(actual, PathBuf::from("/absolute/hcom"));
    assert!(explicit);
}

#[test]
fn test_hcom_config_defaults() {
    let mut config = HcomConfig::default();
    assert_eq!(config.timeout, 86400);
    assert_eq!(config.subagent_timeout, 30);
    assert_eq!(config.terminal, "default");
    assert_eq!(config.tag, "");
    assert_eq!(config.codex_sandbox_mode, "workspace");
    assert!(config.relay_enabled);
    assert!(config.auto_approve);
    assert_eq!(config.auto_subscribe, "collision");
    assert!(config.collect_errors().is_empty());
}

#[test]
fn test_hcom_config_validation_timeout() {
    let mut config = HcomConfig {
        timeout: 0,
        ..HcomConfig::default()
    };
    let errors = config.collect_errors();
    assert!(errors.contains_key("timeout"));

    config.timeout = 86401;
    let errors = config.collect_errors();
    assert!(errors.contains_key("timeout"));

    config.timeout = 3600;
    let errors = config.collect_errors();
    assert!(!errors.contains_key("timeout"));
}

#[test]
fn test_hcom_config_validation_tag() {
    let mut config = HcomConfig {
        tag: "valid-tag".to_string(),
        ..HcomConfig::default()
    };
    assert!(!config.collect_errors().contains_key("tag"));

    config.tag = "invalid tag!".to_string();
    assert!(config.collect_errors().contains_key("tag"));

    config.tag = "".to_string(); // empty is valid
    assert!(!config.collect_errors().contains_key("tag"));
}

#[test]
fn test_hcom_config_validation_sandbox_mode() {
    let mut config = HcomConfig::default();

    for mode in VALID_SANDBOX_MODES {
        config.codex_sandbox_mode = mode.to_string();
        assert!(
            !config.collect_errors().contains_key("codex_sandbox_mode"),
            "mode '{mode}' should be valid"
        );
    }

    config.codex_sandbox_mode = "invalid".to_string();
    assert!(config.collect_errors().contains_key("codex_sandbox_mode"));
}

#[test]
fn test_hcom_config_validation_shell_args() {
    let mut config = HcomConfig {
        claude_args: "--model opus".to_string(),
        ..HcomConfig::default()
    };
    assert!(!config.collect_errors().contains_key("claude_args"));

    config.claude_args = "unclosed 'quote".to_string();
    assert!(config.collect_errors().contains_key("claude_args"));
}

#[test]
fn test_hcom_config_validation_auto_subscribe() {
    let mut config = HcomConfig {
        auto_subscribe: "collision,created".to_string(),
        ..HcomConfig::default()
    };
    assert!(!config.collect_errors().contains_key("auto_subscribe"));

    config.auto_subscribe = "bad preset!".to_string();
    assert!(config.collect_errors().contains_key("auto_subscribe"));
}

#[test]
fn test_terminal_case_normalization() {
    let mut config = HcomConfig {
        terminal: "WezTerm".to_string(),
        ..HcomConfig::default()
    };
    let errors = config.collect_errors();
    assert!(!errors.contains_key("terminal"));
    assert_eq!(config.terminal, "wezterm"); // Normalized

    config.terminal = "Alacritty".to_string();
    let errors = config.collect_errors();
    assert!(!errors.contains_key("terminal"));
    assert_eq!(config.terminal, "alacritty");

    config.terminal = "KITTY".to_string();
    let errors = config.collect_errors();
    assert_eq!(config.terminal, "kitty"); // Normalized regardless of platform
    // kitty is Darwin/Linux-only (DL); on Windows it's correctly rejected
    // by the platform-availability check added for finding #17.
    if crate::shared::platform::platform_name() == "Windows" {
        assert!(errors.contains_key("terminal"));
    } else {
        assert!(!errors.contains_key("terminal"));
    }
}

#[test]
fn test_terminal_custom_command_requires_script() {
    let mut config = HcomConfig {
        terminal: "my-terminal -e bash {script}".to_string(),
        ..HcomConfig::default()
    };
    assert!(!config.collect_errors().contains_key("terminal"));

    // Unknown name without {script} is rejected
    config.terminal = "not-a-preset".to_string();
    assert!(config.collect_errors().contains_key("terminal"));
}

#[test]
fn test_terminal_known_presets_accepted() {
    // Finding 17: presets are now validated against the host platform, so
    // only assert presets that are actually supported here.
    let platform = crate::shared::platform::platform_name();
    let mut config = HcomConfig::default();
    for preset in &[
        "kitty",
        "wezterm",
        "tmux",
        "alacritty",
        "ptyxis",
        "terminal.app",
        "iterm",
    ] {
        if !terminal_preset_supported_on(preset, platform) {
            continue;
        }
        config.terminal = preset.to_string();
        assert!(
            !config.collect_errors().contains_key("terminal"),
            "preset '{preset}' should be valid on {platform}"
        );
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn wrong_platform_builtin_preset_is_rejected() {
    // Finding 17: a built-in preset not available on the host platform
    // (here, "wttab" is Windows-only) must be rejected at validation time,
    // not just silently accepted and left to fail at launch.
    let mut config = HcomConfig {
        terminal: "wttab".to_string(),
        ..HcomConfig::default()
    };
    assert!(config.collect_errors().contains_key("terminal"));
}

#[test]
fn test_set_field_full_auto_normalization() {
    let mut config = HcomConfig::default();
    config.set_field("codex_sandbox_mode", "full-auto").unwrap();
    assert_eq!(config.codex_sandbox_mode, "workspace");
}

#[test]
fn test_set_field_bool_coercion() {
    let mut config = HcomConfig::default();

    config.set_field("auto_approve", "0").unwrap();
    assert!(!config.auto_approve);

    config.set_field("auto_approve", "1").unwrap();
    assert!(config.auto_approve);

    config.set_field("auto_approve", "false").unwrap();
    assert!(!config.auto_approve);

    config.set_field("auto_approve", "yes").unwrap();
    assert!(config.auto_approve);

    config.set_field("relay_enabled", "off").unwrap();
    assert!(!config.relay_enabled);

    config.set_field("relay_enabled", "on").unwrap();
    assert!(config.relay_enabled);
}

#[test]
fn test_is_falsy() {
    assert!(is_falsy("0"));
    assert!(is_falsy("false"));
    assert!(is_falsy("False"));
    assert!(is_falsy("no"));
    assert!(is_falsy("off"));
    assert!(is_falsy(""));
    assert!(!is_falsy("1"));
    assert!(!is_falsy("true"));
    assert!(!is_falsy("yes"));
    assert!(!is_falsy("on"));
}

#[test]
fn test_to_env_dict_roundtrip() {
    let config = HcomConfig::default();
    let dict = config.to_env_dict();

    assert_eq!(dict.get("HCOM_TIMEOUT"), Some(&"86400".to_string()));
    assert_eq!(dict.get("HCOM_TERMINAL"), Some(&"default".to_string()));
    assert_eq!(dict.get("HCOM_AUTO_APPROVE"), Some(&"1".to_string()));
    assert_eq!(dict.get("HCOM_RELAY_ENABLED"), Some(&"1".to_string()));

    let roundtrip = HcomConfig::from_env_dict(&dict).unwrap();
    assert_eq!(config, roundtrip);
}

#[test]
fn test_to_env_dict_never_exposes_relay_psk() {
    // The PSK is the decrypt/forge authority for the whole relay group.
    // `build_launch_env` feeds `to_env_dict` into every spawned child
    // process's environment, so anything emitted here crosses a
    // process boundary. The PSK must stay file-only — verified by
    // checking that even a populated field is suppressed.
    let config = HcomConfig {
        relay_psk: "an-example-secret-value-xxxxxxxxxxxxxxxxxxxxxxxx".to_string(),
        ..Default::default()
    };
    let dict = config.to_env_dict();
    assert!(!dict.contains_key("HCOM_RELAY_PSK"));
    for v in dict.values() {
        assert!(
            !v.contains("an-example-secret-value"),
            "PSK leaked into launch env dict: {v}"
        );
    }
}

#[test]
fn test_load_from_sources_empty() {
    let file_config = HashMap::new();
    let env = HashMap::new();
    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert_eq!(config, HcomConfig::default());
}

#[test]
fn test_load_from_sources_toml_values() {
    let mut file_config = HashMap::new();
    file_config.insert("timeout".to_string(), TomlFieldValue::Int(3600));
    file_config.insert("tag".to_string(), TomlFieldValue::Str("test".to_string()));
    file_config.insert("relay_enabled".to_string(), TomlFieldValue::Bool(false));

    let env = HashMap::new();
    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();

    assert_eq!(config.timeout, 3600);
    assert_eq!(config.tag, "test");
    assert!(!config.relay_enabled);
}

#[test]
fn test_load_from_sources_title_mode_and_env_override() {
    let mut file_config = HashMap::new();
    file_config.insert(
        "title_mode".to_string(),
        TomlFieldValue::Str("label".to_string()),
    );
    let mut env = HashMap::new();
    env.insert("HCOM_TITLE_MODE".to_string(), "off".to_string());

    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert_eq!(config.title_mode, "off");
}

#[test]
fn test_load_from_sources_env_overrides_toml() {
    let mut file_config = HashMap::new();
    file_config.insert("timeout".to_string(), TomlFieldValue::Int(3600));
    file_config.insert(
        "tag".to_string(),
        TomlFieldValue::Str("file-tag".to_string()),
    );

    let mut env = HashMap::new();
    env.insert("HCOM_TAG".to_string(), "env-tag".to_string());

    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();

    assert_eq!(config.timeout, 3600); // From file (no env override)
    assert_eq!(config.tag, "env-tag"); // Env wins over file
}

#[test]
fn bigboss_default_get_set_and_validation() {
    let mut c = HcomConfig::default();
    assert_eq!(c.bigboss, "bigboss");
    assert_eq!(c.get_field("bigboss").as_deref(), Some("bigboss"));

    c.set_field("bigboss", "review-luna:BOXE").unwrap();
    assert_eq!(c.get_field("bigboss").as_deref(), Some("review-luna:BOXE"));
    assert!(c.collect_errors().is_empty(), "tagged device name is valid");

    // Empty resets to the default via normalize.
    c.set_field("bigboss", "").unwrap();
    assert!(c.collect_errors().is_empty());
    assert_eq!(c.bigboss, "bigboss");

    // Whitespace and "*" are rejected.
    c.set_field("bigboss", "big boss").unwrap();
    assert!(c.collect_errors().contains_key("bigboss"));
    c.bigboss = "*".to_string();
    assert!(c.collect_errors().contains_key("bigboss"));
}

#[test]
fn bigboss_env_overrides_toml_and_empty_falls_back() {
    let mut file_config = HashMap::new();
    file_config.insert(
        "bigboss".to_string(),
        TomlFieldValue::Str("file-lead".to_string()),
    );
    let mut env = HashMap::new();
    env.insert("HCOM_BIGBOSS".to_string(), "env-lead".to_string());
    let c = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert_eq!(c.bigboss, "env-lead");

    // An empty env value is skipped so the default stands.
    let mut env2 = HashMap::new();
    env2.insert("HCOM_BIGBOSS".to_string(), String::new());
    let c2 = HcomConfig::load_from_sources(&HashMap::new(), Some(&env2)).unwrap();
    assert_eq!(c2.bigboss, "bigboss");
}

#[test]
fn test_load_from_sources_relay_fields_file_only() {
    let mut file_config = HashMap::new();
    file_config.insert(
        "relay".to_string(),
        TomlFieldValue::Str("mqtt://file.example.com".to_string()),
    );

    let mut env = HashMap::new();
    env.insert(
        "HCOM_RELAY".to_string(),
        "mqtt://env.example.com".to_string(),
    );

    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();

    // Relay fields should come from file, not env
    assert_eq!(config.relay, "mqtt://file.example.com");
}

#[test]
fn test_load_from_sources_int_coercion() {
    let mut file_config = HashMap::new();
    file_config.insert(
        "timeout".to_string(),
        TomlFieldValue::Str("7200".to_string()),
    );

    let env = HashMap::new();
    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert_eq!(config.timeout, 7200);
}

#[test]
fn test_load_from_sources_bool_string_coercion() {
    let mut file_config = HashMap::new();
    file_config.insert(
        "auto_approve".to_string(),
        TomlFieldValue::Str("0".to_string()),
    );

    let env = HashMap::new();
    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert!(!config.auto_approve);
}

#[test]
fn test_load_from_sources_sandbox_mode_empty_uses_default() {
    let mut file_config = HashMap::new();
    file_config.insert(
        "codex_sandbox_mode".to_string(),
        TomlFieldValue::Str("".to_string()),
    );

    let env = HashMap::new();
    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert_eq!(config.codex_sandbox_mode, "workspace"); // Default, not empty
}

#[test]
fn test_load_from_sources_terminal_empty_uses_default() {
    let mut file_config = HashMap::new();
    file_config.insert("terminal".to_string(), TomlFieldValue::Str("".to_string()));

    let env = HashMap::new();
    let config = HcomConfig::load_from_sources(&file_config, Some(&env)).unwrap();
    assert_eq!(config.terminal, "default");
}

#[test]
fn test_toml_roundtrip() {
    let config = HcomConfig {
        timeout: 3600,
        tag: "dev".to_string(),
        auto_approve: false,
        relay: "mqtt://test.com".to_string(),
        ..HcomConfig::default()
    };

    let toml_table = config.to_toml_table();
    let toml_str = toml::to_string_pretty(&toml_table).unwrap();

    // Parse it back
    let parsed: toml::Value = toml::Value::Table(toml_str.parse::<toml::Table>().unwrap());
    let mut file_config = HashMap::new();
    for &(field_name, toml_path) in TOML_KEY_MAP {
        if let Some(val) = get_nested(&parsed, toml_path) {
            let typed = match &val {
                toml::Value::String(s) => TomlFieldValue::Str(s.clone()),
                toml::Value::Integer(i) => TomlFieldValue::Int(*i),
                toml::Value::Boolean(b) => TomlFieldValue::Bool(*b),
                _ => continue,
            };
            file_config.insert(field_name.to_string(), typed);
        }
    }

    let roundtrip = HcomConfig::load_from_sources(&file_config, Some(&HashMap::new())).unwrap();
    assert_eq!(config, roundtrip);
}

#[test]
fn test_load_toml_config_with_dangerous_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[terminal]
active = "echo `whoami`"
[preferences]
timeout = 3600
"#,
    )
    .unwrap();

    let result = load_toml_config(&path);
    // Terminal with dangerous chars should be removed
    assert!(!result.contains_key("terminal"));
    // Other values should load fine
    assert!(result.contains_key("timeout"));
}

#[test]
fn test_load_toml_config_valid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[terminal]
active = "kitty"

[launch]
tag = "myteam"
subagent_timeout = 60

[launch.claude]
args = "--model opus"

[preferences]
timeout = 7200
auto_approve = false
"#,
    )
    .unwrap();

    let result = load_toml_config(&path);
    assert_eq!(
        result.get("terminal").map(|v| v.as_string()),
        Some("kitty".to_string())
    );
    assert_eq!(
        result.get("tag").map(|v| v.as_string()),
        Some("myteam".to_string())
    );
    assert_eq!(
        result.get("claude_args").map(|v| v.as_string()),
        Some("--model opus".to_string())
    );
    assert_eq!(
        result.get("timeout").map(|v| v.as_string()),
        Some("7200".to_string())
    );
}

#[test]
fn test_load_toml_config_missing_file() {
    let result = load_toml_config(std::path::Path::new("/nonexistent/config.toml"));
    assert!(result.is_empty());
}

#[test]
fn test_load_toml_config_invalid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "this is not valid toml [[[[").unwrap();

    let result = load_toml_config(&path);
    assert!(result.is_empty());
}

#[test]
fn test_parse_env_value_unquoted() {
    assert_eq!(parse_env_value("hello"), "hello");
    assert_eq!(parse_env_value("  hello  "), "hello");
}

#[test]
fn test_parse_env_value_double_quoted() {
    assert_eq!(parse_env_value(r#""hello world""#), "hello world");
    assert_eq!(parse_env_value(r#""line1\nline2""#), "line1\nline2");
    assert_eq!(parse_env_value(r#""tab\there""#), "tab\there");
    assert_eq!(parse_env_value(r#""escaped\"quote""#), "escaped\"quote");
}

#[test]
fn test_parse_env_value_single_quoted() {
    assert_eq!(parse_env_value("'literal'"), "literal");
    assert_eq!(parse_env_value(r"'no\nescaping'"), r"no\nescaping");
}

#[test]
fn test_format_env_value_simple() {
    assert_eq!(format_env_value("hello"), "hello");
    assert_eq!(format_env_value(""), "");
}

#[test]
fn test_format_env_value_needs_quoting() {
    assert_eq!(format_env_value("hello world"), "\"hello world\"");
    assert_eq!(format_env_value("line1\nline2"), "\"line1\\nline2\"");
}

#[test]
fn test_get_field_all_fields() {
    let config = HcomConfig::default();
    // All 20 fields should be gettable
    for &(field, _) in FIELD_TO_ENV {
        assert!(
            config.get_field(field).is_some(),
            "get_field('{field}') should return Some"
        );
    }
    assert!(config.get_field("nonexistent").is_none());
}

#[test]
fn args_env_keys_match_integration_specs() {
    let expected: std::collections::HashSet<&str> = crate::integration_spec::ALL
        .iter()
        .filter_map(|spec| spec.launch.args_env)
        .collect();
    let actual: std::collections::HashSet<&str> = FIELD_TO_ENV
        .iter()
        .filter_map(|(field, env_key)| field.ends_with("_args").then_some(*env_key))
        .collect();

    assert_eq!(
        actual, expected,
        "HcomConfig *_args env vars must match IntegrationSpec.launch.args_env"
    );
}

#[test]
fn test_hcom_config_from_env_dict_with_full_auto() {
    let mut data = HcomConfig::default().to_env_dict();
    data.insert(
        "HCOM_CODEX_SANDBOX_MODE".to_string(),
        "full-auto".to_string(),
    );
    let config = HcomConfig::from_env_dict(&data).unwrap();
    assert_eq!(config.codex_sandbox_mode, "workspace");
}

#[test]
fn test_hcom_config_validation_error_display() {
    let errors = HashMap::from([
        ("timeout".to_string(), "timeout must be 1-86400".to_string()),
        ("tag".to_string(), "tag invalid chars".to_string()),
    ]);
    let err = HcomConfigError { errors };
    let display = format!("{err}");
    assert!(display.contains("Invalid config"));
    assert!(display.contains("timeout must be 1-86400"));
    assert!(display.contains("tag invalid chars"));
}

#[test]
fn test_default_toml_structure() {
    let structure = default_toml_structure();
    // Verify key paths exist
    assert!(get_nested(&structure, "terminal.active").is_some());
    assert!(get_nested(&structure, "terminal.title_mode").is_some());
    assert!(get_nested(&structure, "launch.tag").is_some());
    assert!(get_nested(&structure, "launch.claude.args").is_some());
    assert!(get_nested(&structure, "relay.url").is_some());
    assert!(get_nested(&structure, "preferences.timeout").is_some());
}

#[test]
fn test_load_toml_presets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[terminal]
active = "default"

[terminal.presets.myterm]
open = "myterm spawn -- bash {script}"
close = "myterm kill --id {id}"
binary = "myterm"
"#,
    )
    .unwrap();

    let presets = load_toml_presets(&path);
    assert!(presets.is_some());
    let presets = presets.unwrap();
    assert!(presets.as_table().unwrap().contains_key("myterm"));
}

#[test]
fn test_pane_identity_env_vars_include_builtin_and_custom_vars() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[terminal.presets.myterm]
open = "myterm spawn -- bash {script}"
close = "myterm close {pane_id}"
pane_id_env = "MYTERM_PANE_ID"
"#,
    )
    .unwrap();

    let vars = pane_identity_env_vars_from_path(&path);
    assert!(vars.contains("HERDR_PANE_ID"));
    assert!(vars.contains("KITTY_WINDOW_ID"));
    assert!(vars.contains("MYTERM_PANE_ID"));
}

#[test]
fn test_toml_val_to_argv_array_preserves_windows_path() {
    // Array form: elements collected verbatim, so a literal Windows path
    // (backslashes, drive letter) survives without tokenization.
    let v = toml::Value::Array(vec![
        toml::Value::String("myterm".into()),
        toml::Value::String("-e".into()),
        toml::Value::String(r"C:\Users\x\s.ps1".into()),
    ]);
    assert_eq!(
        toml_val_to_argv(&v),
        Ok(Some(vec![
            "myterm".to_string(),
            "-e".to_string(),
            r"C:\Users\x\s.ps1".to_string(),
        ]))
    );
}

#[test]
fn test_toml_val_to_argv_array_rejects_non_string_element() {
    let v = toml::Value::Array(vec![
        toml::Value::String("myterm".into()),
        toml::Value::Integer(42),
        toml::Value::String("{script}".into()),
    ]);
    assert!(toml_val_to_argv(&v).is_err());
}

#[test]
fn test_toml_val_to_argv_string_tokenizes_legacy() {
    let v = toml::Value::String("myterm -e bash {script}".into());
    assert_eq!(
        toml_val_to_argv(&v),
        Ok(Some(vec![
            "myterm".to_string(),
            "-e".to_string(),
            "bash".to_string(),
            "{script}".to_string(),
        ]))
    );
}

#[test]
fn test_toml_val_to_argv_string_invalid_quoting_returns_err() {
    let v = toml::Value::String(r#"kitty -- bash "unterminated"#.into());
    assert!(toml_val_to_argv(&v).is_err());
}

#[test]
fn test_toml_val_to_argv_empty_and_wrong_type() {
    assert!(toml_val_to_argv(&toml::Value::Integer(3)).is_err());
    assert_eq!(toml_val_to_argv(&toml::Value::Array(vec![])), Ok(None));
    assert_eq!(
        toml_val_to_argv(&toml::Value::String(String::new())),
        Ok(None)
    );
}

// B-1: a user-defined `[terminal.presets.<builtin>]` override declares no
// platform and takes precedence over the built-in, so it must be accepted
// even on a platform where the built-in itself is unavailable.
#[test]
#[serial]
fn user_defined_override_exempt_from_builtin_platform_gate() {
    let (_dir, hcom_dir, _home, _guard) = isolated_test_env();
    let platform = crate::shared::platform::platform_name();
    // A built-in preset NOT available on the current host platform.
    let builtin = match platform {
        // windows-terminal is Windows-only.
        "Darwin" | "Linux" => "windows-terminal",
        // iterm is Darwin-only.
        _ => "iterm",
    };

    // Control: without any user override the wrong-platform built-in is
    // rejected at validate time.
    let mut cfg = HcomConfig {
        terminal: builtin.to_string(),
        ..Default::default()
    };
    assert!(
        cfg.collect_errors().contains_key("terminal"),
        "built-in {builtin} should be rejected on {platform} without a user override"
    );

    // Define a user preset with the SAME name — it must now be accepted.
    std::fs::write(
        hcom_dir.join("config.toml"),
        format!(
            "[terminal.presets.{builtin}]\nopen = \"{builtin} -- powershell -File {{script}}\"\n"
        ),
    )
    .unwrap();
    let mut cfg = HcomConfig {
        terminal: builtin.to_string(),
        ..Default::default()
    };
    assert!(
        !cfg.collect_errors().contains_key("terminal"),
        "user-defined override of {builtin} must be accepted on {platform}"
    );
}

#[test]
#[serial]
fn malformed_user_override_does_not_bypass_builtin_platform_gate() {
    let (_dir, hcom_dir, _home, _guard) = isolated_test_env();
    let builtin = match crate::shared::platform::platform_name() {
        "Darwin" | "Linux" => "windows-terminal",
        _ => "iterm",
    };
    std::fs::write(
        hcom_dir.join("config.toml"),
        format!("[terminal.presets.{builtin}]\nopen = \"powershell \\\"unterminated\"\n"),
    )
    .unwrap();

    assert!(!is_user_defined_preset(builtin));
    let mut cfg = HcomConfig {
        terminal: builtin.to_string(),
        ..Default::default()
    };
    assert!(
        cfg.collect_errors().contains_key("terminal"),
        "malformed override must not exempt {builtin} from the platform gate"
    );
}

#[test]
#[serial]
fn malformed_user_override_rejected_for_supported_builtin() {
    let (_dir, hcom_dir, _home, _guard) = isolated_test_env();
    let builtin = if cfg!(windows) { "cmd" } else { "tmux" };
    std::fs::write(
        hcom_dir.join("config.toml"),
        format!("[terminal.presets.{builtin}]\nclose = 42\n"),
    )
    .unwrap();
    let mut cfg = HcomConfig {
        terminal: builtin.to_string(),
        ..Default::default()
    };

    let error = cfg.collect_errors().remove("terminal").unwrap();
    assert!(error.contains("invalid terminal preset"));
    assert!(error.contains("close:"));
}

#[test]
fn test_load_toml_presets_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[terminal]
active = "default"
"#,
    )
    .unwrap();

    let presets = load_toml_presets(&path);
    assert!(presets.is_none());
}

#[test]
#[cfg(unix)]
#[serial]
fn test_save_toml_config_sets_mode_600_for_secret_bearing_config() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let config = HcomConfig {
        relay_psk: "super-secret-psk".to_string(),
        ..Default::default()
    };

    save_toml_config(&config, None).unwrap();

    let mode = std::fs::metadata(paths::config_toml_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn write_default_config_keeps_a_preseeded_env_file() {
    let (_dir, hcom_dir, _home, _guard) = isolated_test_env();
    let env_path = hcom_dir.join("env");
    std::fs::write(&env_path, "ANTHROPIC_BASE_URL=http://127.0.0.1:1\n").unwrap();

    write_default_config().unwrap();

    assert!(hcom_dir.join("config.toml").exists());
    assert_eq!(
        std::fs::read_to_string(&env_path).unwrap(),
        "ANTHROPIC_BASE_URL=http://127.0.0.1:1\n",
        "first-run config creation must not clobber an existing env passthrough"
    );
}
