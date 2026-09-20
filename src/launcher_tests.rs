use super::*;
use serial_test::serial;
use std::collections::BTreeMap;

/// A plugin already running hcom's Codex handlers must not be shadowed by a
/// fresh native install: that is how `hooks remove codex --legacy-only` got
/// undone by the next agent launch, back into a double-fire.
#[test]
fn codex_launch_leaves_hooks_alone_when_the_plugin_runs_them() {
    use crate::hooks::codex::CodexPluginState as S;

    for state in [S::Active, S::ReviewRequired, S::Duplicate, S::Disabled] {
        assert!(
            !super::codex_launch_needs_native_hooks(state),
            "{state:?} would reinstall the legacy hooks at launch"
        );
    }
    for state in [S::Missing, S::Incomplete, S::LegacyOnly, S::Unverified] {
        assert!(
            super::codex_launch_needs_native_hooks(state),
            "{state:?} must still get working hooks"
        );
    }
}

struct EnvVarGuard {
    saved: BTreeMap<String, Option<String>>,
}

impl EnvVarGuard {
    fn clean_detection_env() -> Self {
        let mut keys: Vec<String> = [
            "CLAUDECODE",
            "CLAUDE_ENV_FILE",
            "ANTIGRAVITY_AGENT",
            "GEMINI_CLI",
            "CODEX_SANDBOX",
            "CODEX_SANDBOX_NETWORK_DISABLED",
            "CODEX_MANAGED_BY_NPM",
            "CODEX_MANAGED_BY_BUN",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
            "OPENCODE",
            "KILO",
            "CURSOR_AGENT",
            "CURSOR_PROJECT_DIR",
            "KIMI_CODE_CLI",
            "KIMI_SESSION_ID",
            "HCOM_TOOL",
            "HCOM_LAUNCHED",
            "HCOM_PI",
            "CI",
            "GITHUB_ACTIONS",
            "CARGO_TEST_PARENT",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        keys.extend(
            std::env::vars()
                .map(|(key, _)| key)
                .filter(|key| key.starts_with("CARGO_")),
        );
        Self::remove(keys)
    }

    fn remove<I>(keys: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let mut saved = BTreeMap::new();
        for key in keys {
            saved
                .entry(key.clone())
                .or_insert_with(|| std::env::var(&key).ok());
            unsafe { std::env::remove_var(key) };
        }
        Self { saved }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

#[test]
fn test_launch_tool_from_str() {
    assert_eq!(LaunchTool::from_str("claude").unwrap(), LaunchTool::Claude);
    assert_eq!(
        LaunchTool::from_str("claude-pty").unwrap(),
        LaunchTool::ClaudePty
    );
    assert_eq!(LaunchTool::from_str("gemini").unwrap(), LaunchTool::Gemini);
    assert_eq!(LaunchTool::from_str("codex").unwrap(), LaunchTool::Codex);
    assert_eq!(
        LaunchTool::from_str("opencode").unwrap(),
        LaunchTool::OpenCode
    );
    assert_eq!(LaunchTool::from_str("kilo").unwrap(), LaunchTool::Kilo);
    assert_eq!(LaunchTool::from_str("kilocode").unwrap(), LaunchTool::Kilo);
    assert_eq!(
        LaunchTool::from_str("antigravity").unwrap(),
        LaunchTool::Antigravity
    );
    assert_eq!(
        LaunchTool::from_str("agy").unwrap(),
        LaunchTool::Antigravity
    );
    assert_eq!(
        LaunchTool::from_str("copilot").unwrap(),
        LaunchTool::Copilot
    );
    assert!(LaunchTool::from_str("unknown").is_err());
}

#[test]
fn plugin_permission_error_tells_sandboxed_agents_how_to_retry() {
    let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "sandbox denied");
    let message = format_plugin_install_error(
        "OpenCode",
        "opencode",
        std::path::Path::new("/home/test/.config/opencode/plugins/hcom.ts"),
        &error,
        "Diagnostic context:\n",
    );

    assert!(message.contains("sandbox denied"));
    assert!(
        message.contains("retry the original hcom launch with approval or elevated permission")
    );
    assert!(message.contains("Manual retry: hcom hooks add opencode"));
}

#[test]
fn plugin_non_permission_error_preserves_cause_without_sandbox_advice() {
    let error = std::io::Error::new(std::io::ErrorKind::InvalidData, "bad plugin data");
    let message = format_plugin_install_error(
        "OpenCode",
        "opencode",
        std::path::Path::new("/tmp/hcom.ts"),
        &error,
        "Diagnostic context:\n",
    );

    assert!(message.contains("bad plugin data"));
    assert!(!message.contains("run outside the sandbox"));
}

#[test]
fn launch_count_uses_per_tool_spec_limit() {
    assert!(validate_launch_count(&LaunchTool::Kimi, 10).is_ok());
    let err = validate_launch_count(&LaunchTool::Kimi, 11).unwrap_err();
    assert!(err.to_string().contains("max 10"));

    assert!(validate_launch_count(&LaunchTool::Claude, 100).is_ok());
    let err = validate_launch_count(&LaunchTool::Claude, 101).unwrap_err();
    assert!(err.to_string().contains("max 100"));
}

#[test]
fn unsupported_initial_prompt_is_spec_driven() {
    let mut args = Vec::new();
    let err = append_initial_prompt_args(&LaunchTool::Kimi, &mut args, "task".into()).unwrap_err();
    assert!(
        err.to_string()
            .contains("kimi does not support an initial prompt")
    );
    assert!(args.is_empty());

    append_initial_prompt_args(&LaunchTool::Gemini, &mut args, "task".into()).unwrap();
    assert_eq!(args, vec!["task"]);
}

#[test]
fn initial_prompt_flag_shape_appends_after_native_prompt() {
    let mut args = vec!["--prompt".to_string(), "native prompt".to_string()];
    append_initial_prompt_args(&LaunchTool::OpenCode, &mut args, "hcom prompt".into()).unwrap();
    assert_eq!(
        args,
        vec!["--prompt", "native prompt", "--prompt", "hcom prompt"]
    );
}

#[test]
fn initial_prompt_positional_shape_appends_after_native_prompt() {
    let mut args = vec!["native prompt".to_string()];
    append_initial_prompt_args(&LaunchTool::Gemini, &mut args, "hcom prompt".into()).unwrap();
    assert_eq!(args, vec!["native prompt", "hcom prompt"]);
}

#[test]
fn initial_prompt_dash_dash_shape_appends_after_native_prompt() {
    let mut args = vec!["--".to_string(), "native prompt".to_string()];
    append_initial_prompt_args(&LaunchTool::Claude, &mut args, "hcom prompt".into()).unwrap();
    assert_eq!(args, vec!["--", "native prompt", "--", "hcom prompt"]);
}

#[test]
fn hcom_prompt_alone_is_injected_for_every_shape() {
    for (tool, expected) in [
        (
            LaunchTool::OpenCode,
            vec!["--prompt".to_string(), "hcom".to_string()],
        ),
        (LaunchTool::Gemini, vec!["hcom".to_string()]),
        (
            LaunchTool::Claude,
            vec!["--".to_string(), "hcom".to_string()],
        ),
    ] {
        let mut args = Vec::new();
        append_initial_prompt_args(&tool, &mut args, "hcom".into()).unwrap();
        assert_eq!(args, expected);
    }
}

#[test]
fn positional_shape_does_not_treat_model_value_as_prompt() {
    let mut args = vec!["--model".to_string(), "safe-model".to_string()];
    append_initial_prompt_args(&LaunchTool::Gemini, &mut args, "hcom".into()).unwrap();
    assert_eq!(args.last().map(String::as_str), Some("hcom"));
}

#[test]
fn validate_cursor_print_mode_fails_fast() {
    let errors = validate_tool_args(&LaunchTool::Cursor, &["--print".to_string()]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("not supported"));
}

#[test]
fn validate_kimi_rejects_prompt_mode_but_allows_resume() {
    let errors = validate_tool_args(&LaunchTool::Kimi, &["--prompt".to_string()]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("--prompt"));
    assert!(validate_tool_args(&LaunchTool::Kimi, &["--session".to_string()]).is_empty());
}

#[test]
fn validate_opencode_rejects_run_but_allows_session() {
    let errors = validate_tool_args(&LaunchTool::OpenCode, &["run".to_string()]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("run"));
    assert!(validate_tool_args(&LaunchTool::OpenCode, &["--session".to_string()]).is_empty());
    assert!(validate_tool_args(&LaunchTool::OpenCode, &["--prompt".to_string()]).is_empty());
}

#[test]
fn validate_kilo_rejects_serve_but_allows_continue() {
    let errors = validate_tool_args(&LaunchTool::Kilo, &["serve".to_string()]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("serve"));
    assert!(validate_tool_args(&LaunchTool::Kilo, &["--continue".to_string()]).is_empty());
}

#[test]
fn validate_pi_rejects_print_but_allows_fork() {
    let errors = validate_tool_args(&LaunchTool::Pi, &["--print".to_string()]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("--print"));
    assert!(validate_tool_args(&LaunchTool::Pi, &["--fork".to_string()]).is_empty());
}

#[test]
fn omp_extension_args_are_injected_once() {
    // Holds the same lock as HOME-mutating tests: inject_omp_extension_args
    // resolves the plugin path through current_home_dir(), so without this a
    // parallel test swapping HOME out from under us can make the two calls
    // below disagree on the path and fail the idempotency assert (B2).
    let _env_lock = crate::hooks::test_helpers::EnvGuard::new();
    let mut args = vec!["--model".to_string(), "opus".to_string()];
    inject_omp_extension_args(&LaunchTool::Omp, &mut args);
    assert!(args.iter().any(|arg| arg == "-e"));
    assert!(args.iter().any(|arg| arg.ends_with("hcom.ts")));

    let once = args.clone();
    inject_omp_extension_args(&LaunchTool::Omp, &mut args);
    assert_eq!(args, once);
}

#[test]
fn validate_antigravity_rejects_print_alias_but_allows_conversation() {
    let errors = validate_tool_args(&LaunchTool::Antigravity, &["--prompt".to_string()]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("--prompt"));
    assert!(
        validate_tool_args(&LaunchTool::Antigravity, &["--conversation".to_string()]).is_empty()
    );
    assert!(
        validate_tool_args(
            &LaunchTool::Antigravity,
            &["--prompt-interactive".to_string()]
        )
        .is_empty()
    );
}

#[test]
fn test_launch_tool_as_str() {
    assert_eq!(LaunchTool::Claude.as_str(), "claude");
    assert_eq!(LaunchTool::ClaudePty.as_str(), "claude-pty");
    assert_eq!(LaunchTool::Gemini.as_str(), "gemini");
    assert_eq!(LaunchTool::Antigravity.as_str(), "antigravity");
    assert_eq!(LaunchTool::Copilot.as_str(), "copilot");
}

#[test]
fn test_launch_tool_base_tool() {
    assert_eq!(LaunchTool::Claude.base_tool(), "claude");
    assert_eq!(LaunchTool::ClaudePty.base_tool(), "claude");
    assert_eq!(LaunchTool::Codex.base_tool(), "codex");
    assert_eq!(LaunchTool::Antigravity.base_tool(), "antigravity");
    assert_eq!(LaunchTool::Copilot.base_tool(), "copilot");
}

#[test]
fn test_launch_tool_uses_pty() {
    assert!(!LaunchTool::Claude.uses_pty());
    assert!(LaunchTool::ClaudePty.uses_pty());
    assert!(LaunchTool::Gemini.uses_pty());
    assert!(LaunchTool::Codex.uses_pty());
    assert!(LaunchTool::OpenCode.uses_pty());
    assert!(LaunchTool::Kilo.uses_pty());
    assert!(LaunchTool::Copilot.uses_pty());
}

#[test]
fn test_launch_backend_resolve_interactive() {
    // Any tool, !background → InteractiveVisible (visible terminal).
    for tool in [
        LaunchTool::Claude,
        LaunchTool::ClaudePty,
        LaunchTool::Gemini,
        LaunchTool::Codex,
        LaunchTool::OpenCode,
        LaunchTool::Kilo,
        LaunchTool::Antigravity,
        LaunchTool::Copilot,
    ] {
        assert_eq!(
            LaunchBackend::resolve(&tool, false),
            LaunchBackend::InteractiveVisible,
            "{:?} should resolve to InteractiveVisible without background",
            tool
        );
    }
}

#[test]
fn test_launch_backend_resolve_claude_native_print() {
    // claude `-p`/`--print` surface (`Claude`) + background → NativePrint
    // (detached -p stream-json). Only chosen when the caller passes -p.
    assert_eq!(
        LaunchBackend::resolve(&LaunchTool::Claude, true),
        LaunchBackend::NativePrint
    );
}

#[test]
fn test_launch_backend_resolve_claude_pty_headless() {
    // claude --headless default surface (`ClaudePty`) → HeadlessPty
    // (PTY wrapper, live TUI).
    assert_eq!(
        LaunchBackend::resolve(&LaunchTool::ClaudePty, true),
        LaunchBackend::HeadlessPty
    );
}

#[test]
fn test_launch_backend_resolve_other_tools_headless() {
    // gemini/codex/opencode + --headless → HeadlessPty (unchanged from today).
    for tool in [
        LaunchTool::Gemini,
        LaunchTool::Codex,
        LaunchTool::OpenCode,
        LaunchTool::Kilo,
        LaunchTool::Antigravity,
        LaunchTool::Copilot,
    ] {
        assert_eq!(
            LaunchBackend::resolve(&tool, true),
            LaunchBackend::HeadlessPty,
            "{:?} --headless should be HeadlessPty",
            tool
        );
    }
}

#[test]
fn test_will_run_in_current_terminal() {
    // Explicit override
    assert!(will_run_in_current_terminal(
        5,
        false,
        Some(true),
        None,
        false
    ));
    assert!(!will_run_in_current_terminal(
        1,
        false,
        Some(false),
        None,
        false
    ));

    // terminal=here
    assert!(will_run_in_current_terminal(
        5,
        false,
        None,
        Some("here"),
        false
    ));

    // Inside AI tool → always new window
    assert!(!will_run_in_current_terminal(1, false, None, None, true));

    // Background → never run here
    assert!(!will_run_in_current_terminal(1, true, None, None, false));

    // Single → run here, multiple → new window
    assert!(will_run_in_current_terminal(1, false, None, None, false));
    assert!(!will_run_in_current_terminal(2, false, None, None, false));
}

#[test]
fn test_build_claude_command() {
    let args = vec!["--model".to_string(), "sonnet".to_string()];
    let cmd = build_claude_command(&args);
    if cfg!(windows) {
        // ps_quote() always quotes, unlike the POSIX shell_quote() used
        // elsewhere, which leaves plain args unquoted.
        assert_eq!(cmd, "claude '--model' 'sonnet'");
    } else {
        assert_eq!(cmd, "claude --model sonnet");
    }
}

#[test]
fn test_build_claude_command_with_spaces() {
    let args = vec!["--prompt".to_string(), "fix all tests".to_string()];
    let cmd = build_claude_command(&args);
    assert!(cmd.contains("'fix all tests'"));
}

#[test]
fn test_background_runner_env_includes_instance_name() {
    let mut env = HashMap::new();
    env.insert("HCOM_PROCESS_ID".to_string(), "pid-123".to_string());

    let runner_env = background_runner_env("codex", &env, "nita");

    assert_eq!(
        runner_env.get("HCOM_INSTANCE_NAME").map(String::as_str),
        Some("nita")
    );
    assert_eq!(
        runner_env.get("HCOM_PROCESS_ID").map(String::as_str),
        Some("pid-123")
    );
    assert!(!runner_env.contains_key("HCOM_PTY_MODE"));
}

#[test]
fn test_background_runner_env_includes_claude_pty_mode() {
    let env = HashMap::new();

    let runner_env = background_runner_env("claude", &env, "hone");

    assert_eq!(
        runner_env.get("HCOM_INSTANCE_NAME").map(String::as_str),
        Some("hone")
    );
    assert_eq!(
        runner_env.get("HCOM_PTY_MODE").map(String::as_str),
        Some("1")
    );
}

#[test]
fn test_background_runner_env_antigravity_sets_agent() {
    let runner_env = background_runner_env("antigravity", &HashMap::new(), "nabe");
    assert_eq!(
        runner_env.get("ANTIGRAVITY_AGENT").map(String::as_str),
        Some("1")
    );
}

#[test]
fn test_env_strip_set_strips_closed_categories() {
    let strip = env_strip_set();
    // HCOM identity
    assert!(strip.contains("HCOM_PROCESS_ID"));
    assert!(strip.contains("HCOM_LAUNCHED"));
    // Tool markers
    assert!(strip.contains("CLAUDECODE"));
    assert!(strip.contains("CLAUDE_ENV_FILE"));
    assert!(strip.contains("CODEX_SANDBOX"));
    assert!(strip.contains("CODEX_THREAD_ID"));
    assert!(strip.contains("GEMINI_SYSTEM_MD"));
    assert!(strip.contains("HCOM_TOOL"));
    assert!(strip.contains("HCOM_PI"));
    assert!(!strip.contains("PI_CODING_AGENT_DIR"));
    // Terminal context
    assert!(strip.contains("KITTY_WINDOW_ID"));
    assert!(strip.contains("TMUX_PANE"));
    assert!(!strip.contains("COLORTERM"));
    assert!(!strip.contains("TERM"));
}

#[test]
#[serial]
fn test_contaminated_parent_detection() {
    let _guard = EnvVarGuard::clean_detection_env();
    assert!(!contaminated_parent());

    unsafe { std::env::set_var("CLAUDECODE", "1") };
    assert!(contaminated_parent());
    unsafe { std::env::remove_var("CLAUDECODE") };

    unsafe { std::env::set_var("CODEX_THREAD_ID", "thread") };
    assert!(contaminated_parent());
    unsafe { std::env::remove_var("CODEX_THREAD_ID") };

    unsafe { std::env::set_var("CI", "1") };
    assert!(contaminated_parent());
    unsafe { std::env::remove_var("CI") };

    unsafe { std::env::set_var("CARGO_TEST_PARENT", "1") };
    assert!(contaminated_parent());
}

#[test]
#[serial]
fn test_build_launch_env_inherits_parent_env() {
    unsafe { std::env::set_var("RORI_TEST_MY_VAR", "hello") }
    unsafe { std::env::set_var("RORI_TEST_OPENROUTER_API_KEY", "sk-test-123") }
    unsafe { std::env::set_var("RORI_TEST_PI_OFFLINE", "1") }

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env(&config, LaunchEnvRegime::HumanShell);

    assert_eq!(
        env.get("RORI_TEST_MY_VAR").map(String::as_str),
        Some("hello")
    );
    assert_eq!(
        env.get("RORI_TEST_OPENROUTER_API_KEY").map(String::as_str),
        Some("sk-test-123")
    );
    assert_eq!(
        env.get("RORI_TEST_PI_OFFLINE").map(String::as_str),
        Some("1")
    );

    unsafe { std::env::remove_var("RORI_TEST_MY_VAR") }
    unsafe { std::env::remove_var("RORI_TEST_OPENROUTER_API_KEY") }
    unsafe { std::env::remove_var("RORI_TEST_PI_OFFLINE") }
}

#[test]
#[serial]
fn test_build_launch_env_agent_regime_uses_resolved_shell_base() {
    let _guard = EnvVarGuard::remove(vec![
        "RORI_PARENT_CONTAMINATION".to_string(),
        "RORI_RESOLVED_AUTH".to_string(),
    ]);
    unsafe { std::env::set_var("RORI_PARENT_CONTAMINATION", "leak") };

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env_with_resolver(&config, LaunchEnvRegime::ContaminatedParent, || {
        Some(HashMap::from([(
            "RORI_RESOLVED_AUTH".to_string(),
            "auth-token".to_string(),
        )]))
    });

    assert!(!env.contains_key("RORI_PARENT_CONTAMINATION"));
    assert_eq!(
        env.get("RORI_RESOLVED_AUTH").map(String::as_str),
        Some("auth-token")
    );
}

#[test]
#[serial]
fn test_tool_config_env_copies_ambient_override_into_clean_child_env() {
    let _guard = EnvVarGuard::remove(vec!["CODEX_HOME".to_string()]);
    unsafe { std::env::set_var("CODEX_HOME", "/isolated/codex-home") };
    let mut env = HashMap::from([("HOME".to_string(), "/clean-shell-home".to_string())]);

    ensure_tool_config_env(&LaunchTool::Codex, &mut env);

    assert_eq!(
        env.get("CODEX_HOME").map(String::as_str),
        Some("/isolated/codex-home")
    );
}

#[test]
fn test_windows_env_override_replaces_different_key_casing() {
    let mut env = HashMap::from([(
        "CODEX_HOME".to_string(),
        r"C:\ambient-codex-home".to_string(),
    )]);

    insert_effective_env(
        &mut env,
        "Codex_Home".to_string(),
        r"C:\caller-codex-home".to_string(),
        true,
    );

    assert_eq!(env.len(), 1);
    assert_eq!(
        effective_env_value(&env, "CODEX_HOME", true),
        Some(r"C:\caller-codex-home")
    );
}

#[test]
#[serial]
fn test_build_launch_env_strips_closed_categories() {
    unsafe { std::env::set_var("HCOM_PROCESS_ID", "pid-stale") }
    unsafe { std::env::set_var("CLAUDECODE", "1") }
    unsafe { std::env::set_var("CODEX_THREAD_ID", "thread-stale") }
    unsafe { std::env::set_var("PI_CODING_AGENT_DIR", "/tmp/pi-config") }
    unsafe { std::env::set_var("KITTY_WINDOW_ID", "1337") }

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env(&config, LaunchEnvRegime::HumanShell);

    assert!(!env.contains_key("HCOM_PROCESS_ID"));
    assert!(!env.contains_key("CLAUDECODE"));
    assert!(!env.contains_key("CODEX_THREAD_ID"));
    assert_eq!(
        env.get("PI_CODING_AGENT_DIR").map(String::as_str),
        Some("/tmp/pi-config")
    );
    assert!(!env.contains_key("KITTY_WINDOW_ID"));

    unsafe { std::env::remove_var("HCOM_PROCESS_ID") }
    unsafe { std::env::remove_var("CLAUDECODE") }
    unsafe { std::env::remove_var("CODEX_THREAD_ID") }
    unsafe { std::env::remove_var("PI_CODING_AGENT_DIR") }
    unsafe { std::env::remove_var("KITTY_WINDOW_ID") }
}

#[test]
#[serial]
fn test_build_launch_env_run_here_inherits_terminal_vars() {
    let _guard = EnvVarGuard::remove(vec!["NO_COLOR".to_string(), "HCOM_PROCESS_ID".to_string()]);
    unsafe { std::env::set_var("NO_COLOR", "1") };
    unsafe { std::env::set_var("HCOM_PROCESS_ID", "pid-stale") };

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env_with_resolver(&config, LaunchEnvRegime::RunHere, || {
        panic!("run_here must not resolve shell env")
    });

    assert_eq!(env.get("NO_COLOR").map(String::as_str), Some("1"));
    assert!(!env.contains_key("HCOM_PROCESS_ID"));
}

#[test]
#[serial]
fn test_build_launch_env_fail_open_to_parent_env() {
    let _guard = EnvVarGuard::remove(vec!["RORI_FAIL_OPEN_PARENT".to_string()]);
    unsafe { std::env::set_var("RORI_FAIL_OPEN_PARENT", "present") };

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env_with_resolver(&config, LaunchEnvRegime::ContaminatedParent, || None);

    assert_eq!(
        env.get("RORI_FAIL_OPEN_PARENT").map(String::as_str),
        Some("present")
    );
}

#[test]
#[serial]
fn test_build_launch_env_config_overrides_ambient() {
    unsafe { std::env::set_var("HCOM_TAG", "ambient-tag") }

    let config = crate::config::HcomConfig {
        tag: "config-tag".to_string(),
        ..Default::default()
    };
    let env = build_launch_env(&config, LaunchEnvRegime::HumanShell);

    assert_eq!(env.get("HCOM_TAG").map(String::as_str), Some("config-tag"));

    unsafe { std::env::remove_var("HCOM_TAG") }
}

#[test]
fn test_codex_bootstrap_includes_notes_from_effective_instance_env() {
    let db = launcher_test_db();
    let hcom_dir = tempfile::tempdir().unwrap();
    let instance_env = HashMap::from([(
        "HCOM_NOTES".to_string(),
        "instance-specific notes".to_string(),
    )]);

    let bootstrap = build_codex_bootstrap(
        &db,
        hcom_dir.path(),
        "luna",
        false,
        &instance_env,
        "",
        false,
    );

    assert!(bootstrap.contains("## NOTES"));
    assert!(bootstrap.contains("instance-specific notes"));
}

#[test]
fn test_codex_bootstrap_omits_notes_section_when_effective_env_has_none() {
    let db = launcher_test_db();
    let hcom_dir = tempfile::tempdir().unwrap();

    let bootstrap = build_codex_bootstrap(
        &db,
        hcom_dir.path(),
        "luna",
        false,
        &HashMap::new(),
        "",
        false,
    );

    assert!(!bootstrap.contains("## NOTES"));
}

#[test]
fn test_codex_bootstrap_omits_notes_section_for_empty_env_value() {
    // `HCOM_NOTES=""` is the documented way to clear notes; it is
    // structurally different from a missing key and must still produce no
    // `## NOTES` section.
    let db = launcher_test_db();
    let hcom_dir = tempfile::tempdir().unwrap();
    let instance_env = HashMap::from([("HCOM_NOTES".to_string(), String::new())]);

    let bootstrap = build_codex_bootstrap(
        &db,
        hcom_dir.path(),
        "luna",
        false,
        &instance_env,
        "",
        false,
    );

    assert!(!bootstrap.contains("## NOTES"));
}

#[test]
fn test_inherited_notes_fill_gap_when_absent() {
    // Nested launch with no explicit notes: the parent's inherited value
    // propagates so the child doesn't lose it.
    let mut env = HashMap::new();
    apply_inherited_notes(&mut env, Some("from-parent".to_string()));
    assert_eq!(
        env.get("HCOM_NOTES").map(String::as_str),
        Some("from-parent")
    );
}

#[test]
fn test_inherited_notes_do_not_override_explicit_value() {
    // Explicit notes (config.toml / ~/.hcom/env / LaunchParams.env, already
    // in instance_env via base_env) must win over an inherited parent value.
    let mut env = HashMap::from([("HCOM_NOTES".to_string(), "explicit".to_string())]);
    apply_inherited_notes(&mut env, Some("from-parent".to_string()));
    assert_eq!(env.get("HCOM_NOTES").map(String::as_str), Some("explicit"));
}

#[test]
fn test_inherited_notes_do_not_override_intentional_clear() {
    // An intentional empty value (`HCOM_NOTES=""`) clears notes and must not
    // be resurrected by an inherited parent value.
    let mut env = HashMap::from([("HCOM_NOTES".to_string(), String::new())]);
    apply_inherited_notes(&mut env, Some("from-parent".to_string()));
    assert_eq!(env.get("HCOM_NOTES").map(String::as_str), Some(""));
}

#[test]
fn test_inherited_notes_noop_when_parent_unset() {
    let mut env = HashMap::new();
    apply_inherited_notes(&mut env, None);
    assert!(!env.contains_key("HCOM_NOTES"));
}

#[test]
fn test_codex_notes_survive_developer_instructions_toml_transport() {
    // The helper only produces the intermediate bootstrap string. Notes
    // actually reach Codex through `preprocess_codex_args`, which
    // TOML-encodes the bootstrap into `-c developer_instructions=...`.
    // Assert on the final, TOML-decoded argument so quotes, backslashes,
    // braces, and newlines are proven to survive the real transport.
    let db = launcher_test_db();
    let hcom_dir = tempfile::tempdir().unwrap();
    let notes = "Use \"review mode\".\nWindows path: C:\\work\\repo\n{literal braces}\nSecond line";
    let instance_env = HashMap::from([("HCOM_NOTES".to_string(), notes.to_string())]);

    let bootstrap = build_codex_bootstrap(
        &db,
        hcom_dir.path(),
        "luna",
        false,
        &instance_env,
        "",
        false,
    );

    let args = crate::tools::codex_preprocessing::preprocess_codex_args(
        &[],
        &bootstrap,
        "workspace",
        crate::tools::codex_preprocessing::CodexHookTrustOutcome::NoActionNeeded,
    );

    // Locate the `-c developer_instructions=<TOML>` value and decode it.
    // preprocess also injects sandbox `-c` args, so match by prefix rather
    // than by the first `-c` position.
    let encoded = args
        .iter()
        .find_map(|a| a.strip_prefix("developer_instructions="))
        .expect("developer_instructions arg present");
    let decoded: toml::Table =
        toml::from_str(&format!("x = {encoded}")).expect("developer_instructions is valid TOML");
    let dev_instructions = decoded["x"].as_str().expect("string value");

    assert!(dev_instructions.contains("## NOTES"));
    assert!(dev_instructions.contains("Use \"review mode\"."));
    assert!(dev_instructions.contains("C:\\work\\repo"));
    assert!(dev_instructions.contains("{literal braces}"));
    assert!(dev_instructions.contains("Second line"));
}

#[test]
#[serial]
fn test_background_runner_env_uses_upstream_resolved_base() {
    let _guard = EnvVarGuard::remove(vec!["RORI_BACKGROUND_CONTAMINATION".to_string()]);
    unsafe { std::env::set_var("RORI_BACKGROUND_CONTAMINATION", "leak") };

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env_with_resolver(&config, LaunchEnvRegime::ContaminatedParent, || {
        Some(HashMap::from([(
            "RORI_BACKGROUND_AUTH".to_string(),
            "auth-token".to_string(),
        )]))
    });
    let runner_env = background_runner_env("codex", &env, "nita");

    assert!(!runner_env.contains_key("RORI_BACKGROUND_CONTAMINATION"));
    assert_eq!(
        runner_env.get("RORI_BACKGROUND_AUTH").map(String::as_str),
        Some("auth-token")
    );
}

#[test]
fn test_runner_binary_dirs_prioritize_selected_node_and_deduplicate() {
    let resolved = HashMap::from([
        ("codex", "pnpm/bin/codex"),
        ("hcom", "target/debug/hcom"),
        ("node", "nvm/bin/node"),
        ("python3", "system/bin/python3"),
    ]);
    // Model an earlier dev-root/current-exe insertion. Resolving hcom to the
    // same directory must not add it twice.
    let binaries = resolve_runner_binaries(vec!["target/debug".to_string()], "codex", |name| {
        resolved.get(name).map(ToString::to_string)
    });

    assert_eq!(
        binaries.path_dirs,
        ["nvm/bin", "target/debug", "pnpm/bin", "system/bin"].map(ToString::to_string)
    );
    assert_eq!(binaries.tool_path.as_deref(), Some("pnpm/bin/codex"));
    assert!(
        binaries.path_dirs.iter().position(|dir| dir == "nvm/bin")
            < binaries
                .path_dirs
                .iter()
                .position(|dir| dir == "system/bin"),
        "the selected Node directory must precede every generic system directory"
    );
}

#[test]
fn test_runner_binary_dirs_keep_system_tool_explicit_behind_selected_node() {
    let resolved = HashMap::from([
        ("codex", "system/bin/codex"),
        ("hcom", "system/bin/hcom"),
        ("node", "nvm/bin/node"),
        ("python3", "system/bin/python3"),
    ]);

    let binaries = resolve_runner_binaries(vec![], "codex", |name| {
        resolved.get(name).map(ToString::to_string)
    });

    assert_eq!(
        binaries.path_dirs,
        ["nvm/bin", "system/bin"].map(ToString::to_string)
    );
    assert_eq!(binaries.tool_path.as_deref(), Some("system/bin/codex"));
}

// Unix-only: asserts the bash runner's `. 'sidecar'` sourcing + unset block;
// Windows generates a PowerShell runner with a different shape.
#[cfg(unix)]
#[test]
fn test_runner_script_strips_instance_state_vars() {
    // Isolated HCOM_DIR: create_runner_script writes into it, and parallel
    // tests swap the global one out from under us mid-test.
    let _env = hooks_missing_test_env();
    let env = HashMap::from([
        ("GEMINI_PTY_INFO".to_string(), "child_process".to_string()),
        ("GEMINI_API_KEY".to_string(), "gem-key".to_string()),
        ("GEMINI_CLI".to_string(), "1".to_string()),
        ("HERDR_PANE_ID".to_string(), "w1:old".to_string()),
        (
            "HERDR_SOCKET_PATH".to_string(),
            "/tmp/herdr.sock".to_string(),
        ),
        ("KITTY_WINDOW_ID".to_string(), "17".to_string()),
        ("TERM".to_string(), "dumb".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
        ("NO_COLOR".to_string(), "1".to_string()),
        ("FORCE_COLOR".to_string(), "1".to_string()),
        ("RORI_MY_VAR".to_string(), "myval".to_string()),
    ]);

    let script = create_runner_script("gemini", "/tmp", "test", &env, &[], false).unwrap();

    let content = std::fs::read_to_string(&script).unwrap();
    // Instance-state stripped from unset block
    assert!(
        content.contains("GEMINI_PTY_INFO"),
        "GEMINI_PTY_INFO should appear in unset"
    );
    let env_file = content
        .lines()
        .find_map(|line| line.trim().strip_prefix(". "))
        .map(|path| path.trim_matches('\'').to_string())
        .expect("runner script should source a sidecar env file");
    let sidecar = std::fs::read_to_string(&env_file).unwrap();
    assert!(!sidecar.contains("GEMINI_PTY_INFO"));
    assert!(!sidecar.contains("GEMINI_CLI"));
    assert!(!sidecar.contains("TERM="));
    assert!(!sidecar.contains("COLORTERM="));
    assert!(!sidecar.contains("NO_COLOR="));
    assert!(!sidecar.contains("FORCE_COLOR="));
    assert!(!sidecar.contains("HERDR_PANE_ID="));
    assert!(!sidecar.contains("KITTY_WINDOW_ID="));
    assert!(sidecar.contains("HERDR_SOCKET_PATH="));
    assert!(sidecar.contains("GEMINI_API_KEY"));
    assert!(sidecar.contains("RORI_MY_VAR"));

    std::fs::remove_file(&script).ok();
    std::fs::remove_file(env_file).ok();
}

/// herdr classifies an agent pane from its foreground process, which under PTY
/// mode is `hcom pty`. The `HERDR_AGENT` hint names the tool for it, so it has
/// to name the tool being launched (not the one that inherited the parent
/// pane's value) and has to survive the sidecar's strip list.
#[cfg(unix)]
#[test]
fn test_runner_forwards_herdr_agent_hint() {
    let _env = hooks_missing_test_env();

    // Overwrites an inherited value: a claude pane spawning codex names codex.
    let mut env = HashMap::from([("HERDR_AGENT".to_string(), "claude".to_string())]);
    env.extend(tool_extra_env("codex"));
    assert_eq!(env.get("HERDR_AGENT").map(String::as_str), Some("codex"));

    let script = create_runner_script("codex", "/tmp", "test", &env, &[], false).unwrap();
    let content = std::fs::read_to_string(&script).unwrap();
    let env_file = content
        .lines()
        .find_map(|line| line.trim().strip_prefix(". "))
        .map(|path| path.trim_matches('\'').to_string())
        .expect("runner script should source a sidecar env file");
    let sidecar = std::fs::read_to_string(&env_file).unwrap();
    assert!(
        sidecar.contains("HERDR_AGENT=codex"),
        "sidecar should carry the herdr agent hint, got: {sidecar}"
    );

    std::fs::remove_file(&script).ok();
    std::fs::remove_file(env_file).ok();
}

#[cfg(unix)]
#[test]
fn test_run_here_runner_preserves_current_pane_identity() {
    // Isolated HCOM_DIR: create_runner_script writes into it, and parallel
    // tests swap the global one out from under us mid-test.
    let _env = hooks_missing_test_env();
    let env = HashMap::from([
        ("HERDR_PANE_ID".to_string(), "w1:current".to_string()),
        ("RORI_MY_VAR".to_string(), "myval".to_string()),
    ]);

    let script = create_runner_script("gemini", "/tmp", "test", &env, &[], true).unwrap();
    let content = std::fs::read_to_string(&script).unwrap();
    let env_file = content
        .lines()
        .find_map(|line| line.trim().strip_prefix(". "))
        .map(|path| path.trim_matches('\'').to_string())
        .expect("runner script should source a sidecar env file");
    let sidecar = std::fs::read_to_string(&env_file).unwrap();
    assert!(sidecar.contains("HERDR_PANE_ID="));

    std::fs::remove_file(&script).ok();
    std::fs::remove_file(env_file).ok();
}

// create_runner_script_windows() isn't cfg(windows)-gated (only its call
// site is, via a runtime cfg!(windows) check), so this runs on any host.
#[test]
fn test_runner_script_windows_has_bom_and_propagates_exit_code() {
    // Isolated HCOM_DIR: create_runner_script writes into it, and parallel
    // tests swap the global one out from under us mid-test.
    let _env = hooks_missing_test_env();
    let env = HashMap::from([
        ("SOME_SECRET".to_string(), "sekrit".to_string()),
        ("herdr_pane_id".to_string(), "w1:old".to_string()),
        (
            "HERDR_SOCKET_PATH".to_string(),
            r"C:\tmp\herdr.sock".to_string(),
        ),
    ]);

    let script =
        create_runner_script_windows("gemini", "/tmp", "test-win", &env, &[], false).unwrap();

    let bytes = std::fs::read(&script).unwrap();
    assert_eq!(
        &bytes[..3],
        b"\xEF\xBB\xBF",
        "PowerShell 5.1 misreads BOM-less files as the legacy ANSI code page"
    );
    let content = String::from_utf8(bytes[3..].to_vec()).unwrap();
    assert!(
        content.contains("exit $LASTEXITCODE"),
        "runner must surface the wrapped process's real exit code, not always report success"
    );
    let environment_ready = content
        .find("[hcom runner] environment ready")
        .expect("background launches should expose the environment stage");
    let sidecar_source = content
        .find("Test-Path '")
        .expect("ambient env should be sourced from a sidecar file");
    let wrapper_start = content
        .find("[hcom runner] starting PTY wrapper")
        .expect("background launches should expose the wrapper stage");
    assert!(environment_ready < sidecar_source);
    assert!(sidecar_source < wrapper_start);

    let sidecar = content
        .split("Test-Path '")
        .nth(1)
        .and_then(|s| s.split('\'').next())
        .expect("ambient env should be sourced from a sidecar file");
    let sidecar_bytes = std::fs::read(sidecar).unwrap();
    assert_eq!(
        &sidecar_bytes[..3],
        b"\xEF\xBB\xBF",
        "sidecar env file needs the same BOM as the runner script"
    );
    let sidecar_content = String::from_utf8_lossy(&sidecar_bytes);
    assert!(sidecar_content.contains("SOME_SECRET"));
    assert!(
        !sidecar_content
            .to_ascii_lowercase()
            .contains("herdr_pane_id")
    );
    assert!(sidecar_content.contains("HERDR_SOCKET_PATH"));

    std::fs::remove_file(&script).ok();
    std::fs::remove_file(sidecar).ok();
}

#[test]
fn test_windows_runner_invocation_reuses_outer_powershell() {
    let command = runner_invocation_command_for_platform(r"C:\tmp\it's runner.ps1", true);
    assert_eq!(command, r"& 'C:\tmp\it''s runner.ps1'");
    assert!(
        !command.to_ascii_lowercase().contains("powershell"),
        "the outer PowerShell must not launch a redundant nested host"
    );
}

#[test]
fn test_unix_runner_invocation_uses_bash() {
    let command = runner_invocation_command_for_platform("/tmp/it's runner.sh", false);
    assert_eq!(command, "bash '/tmp/it'\\''s runner.sh'");
}

// Tool args must travel via the JSON sidecar, never inline on the run
// line: powershell.exe passes embedded double quotes unescaped to native
// executables, so the child re-splits argv at quote boundaries (#66 —
// `hcom codex` failed on its quote-bearing `-c` values).
#[test]
fn test_runner_script_windows_passes_args_via_sidecar_file() {
    // Isolated HCOM_DIR: create_runner_script writes into it, and parallel
    // tests swap the global one out from under us mid-test.
    let _env = hooks_missing_test_env();
    let env = HashMap::new();
    let args = vec![
        "-c".to_string(),
        r#"projects={ "C:\repo" = { trust_level = "trusted" } }"#.to_string(),
        "-c".to_string(),
        "developer_instructions=multi\nline \"quoted\" text".to_string(),
    ];

    let script =
        create_runner_script_windows("codex", "/tmp", "test-args", &env, &args, false).unwrap();
    let content = std::fs::read_to_string(&script).unwrap();

    let run_line = content
        .lines()
        .find(|l| l.contains(" pty codex"))
        .expect("runner must invoke hcom pty");
    assert!(run_line.contains("--hcom-args-file"));
    assert!(!run_line.contains("trust_level"));
    assert!(!run_line.contains("developer_instructions"));

    let args_file = run_line
        .split("--hcom-args-file '")
        .nth(1)
        .and_then(|s| s.split('\'').next())
        .expect("run line should quote the args file path");
    let json = std::fs::read_to_string(args_file).unwrap();
    let roundtrip: Vec<String> = serde_json::from_str(&json).unwrap();
    assert_eq!(
        roundtrip, args,
        "args must survive the file round-trip exactly"
    );

    std::fs::remove_file(&script).ok();
    std::fs::remove_file(args_file).ok();
}

#[test]
fn test_runner_script_windows_no_args_skips_sidecar_file() {
    // Isolated HCOM_DIR: create_runner_script writes into it, and parallel
    // tests swap the global one out from under us mid-test.
    let _env = hooks_missing_test_env();
    let env = HashMap::new();
    let script =
        create_runner_script_windows("gemini", "/tmp", "test-noargs", &env, &[], false).unwrap();
    let content = std::fs::read_to_string(&script).unwrap();
    let run_line = content
        .lines()
        .find(|l| l.contains(" pty gemini"))
        .expect("runner must invoke hcom pty");
    assert!(!run_line.contains("--hcom-args-file"));
    std::fs::remove_file(&script).ok();
}

#[test]
#[serial]
fn test_same_tool_nesting_strips_instance_state() {
    unsafe { std::env::set_var("GEMINI_PTY_INFO", "child_process") }
    unsafe { std::env::set_var("GEMINI_API_KEY", "parent-key") }

    let config = crate::config::HcomConfig::default();
    let mut env = build_launch_env(&config, LaunchEnvRegime::HumanShell);

    let gemini_spec: &'static crate::integration_spec::IntegrationSpec =
        crate::tool::Tool::Gemini.spec();
    for var in gemini_spec.instance_state_env {
        env.remove(*var);
    }

    assert!(!env.contains_key("GEMINI_PTY_INFO"));
    assert_eq!(
        env.get("GEMINI_API_KEY").map(String::as_str),
        Some("parent-key")
    );

    unsafe { std::env::remove_var("GEMINI_PTY_INFO") }
    unsafe { std::env::remove_var("GEMINI_API_KEY") }
}

#[test]
#[serial]
fn test_cross_tool_nesting_forwards_auth() {
    unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-parent") }

    let config = crate::config::HcomConfig::default();
    let env = build_launch_env(&config, LaunchEnvRegime::HumanShell);

    assert_eq!(
        env.get("OPENROUTER_API_KEY").map(String::as_str),
        Some("sk-parent")
    );

    unsafe { std::env::remove_var("OPENROUTER_API_KEY") }
}

fn launcher_test_db() -> crate::db::HcomDb {
    let db = crate::db::HcomDb::open_raw(std::path::Path::new(":memory:")).unwrap();
    db.init_db().unwrap();
    db
}

fn insert_test_instance(db: &crate::db::HcomDb, name: &str, status: &str) {
    let now = chrono::Utc::now().timestamp() as f64;
    db.conn()
            .execute(
                "INSERT INTO instances (name, status, created_at, tool) VALUES (?1, ?2, ?3, 'antigravity')",
                rusqlite::params![name, status, now],
            )
            .unwrap();
}

#[test]
fn resolve_explicit_name_conflict_allows_free_name() {
    let db = launcher_test_db();
    assert!(resolve_explicit_name_conflict(&db, "luna").is_ok());
}

#[test]
fn resolve_explicit_name_conflict_consumes_inactive_resume_row() {
    let db = launcher_test_db();
    insert_test_instance(&db, "zeno", "inactive");
    assert!(resolve_explicit_name_conflict(&db, "zeno").is_ok());
    // Row must be gone so the launcher can create a fresh row with the same name.
    assert!(db.get_instance("zeno").unwrap().is_none());
}

#[test]
fn resolve_explicit_name_conflict_allows_pending_placeholder() {
    // A pending placeholder is the fork/resume path's own reservation
    // (reserve_generated_name). It must pass through so the launcher's
    // pre-register step can promote it — bailing here broke `hcom f`.
    let db = launcher_test_db();
    let now = chrono::Utc::now().timestamp() as f64;
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at, tool) \
                 VALUES (?1, ?2, ?3, ?4, 'claude')",
            rusqlite::params![
                "milo",
                instance_names::PLACEHOLDER_STATUS,
                instance_names::PLACEHOLDER_CONTEXT,
                now
            ],
        )
        .unwrap();
    assert!(resolve_explicit_name_conflict(&db, "milo").is_ok());
    // Row must survive — the launcher promotes it in place.
    assert!(db.get_instance("milo").unwrap().is_some());
}

#[test]
fn resolve_explicit_name_conflict_rejects_active_row() {
    let db = launcher_test_db();
    insert_test_instance(&db, "rune", "listening");
    let err = resolve_explicit_name_conflict(&db, "rune")
        .unwrap_err()
        .to_string();
    assert!(err.contains("already exists"), "unexpected: {err}");
    // Row should still be present — no deletion on conflict.
    assert!(db.get_instance("rune").unwrap().is_some());
}

// ── inject_workspace_trust_args ──────────────────────────────────────────

#[test]
fn test_auto_trust_workspace_default_true() {
    assert!(crate::config::HcomConfig::default().auto_trust_workspace);
}

#[test]
fn test_gemini_flag_on_injects_skip_trust() {
    let dir = std::path::Path::new("/some/workspace");
    let mut args = vec!["--model".to_string(), "gemini-2.5-flash".to_string()];
    inject_workspace_trust_args(&LaunchTool::Gemini, dir, &mut args, true);
    assert!(args.contains(&"--skip-trust".to_string()));
}

#[test]
fn test_gemini_flag_off_no_injection() {
    let dir = std::path::Path::new("/some/workspace");
    let mut args = vec!["--model".to_string(), "gemini-2.5-flash".to_string()];
    inject_workspace_trust_args(&LaunchTool::Gemini, dir, &mut args, false);
    assert!(!args.contains(&"--skip-trust".to_string()));
}

#[test]
fn test_gemini_inject_idempotent_when_present() {
    let dir = std::path::Path::new("/some/workspace");
    let mut args = vec![
        "--skip-trust".to_string(),
        "--model".to_string(),
        "x".to_string(),
    ];
    inject_workspace_trust_args(&LaunchTool::Gemini, dir, &mut args, true);
    assert_eq!(
        args.iter().filter(|a| a.as_str() == "--skip-trust").count(),
        1
    );
}

#[test]
fn test_codex_flag_on_injects_trust_level() {
    let dir = std::path::Path::new("/my/project");
    let mut args = vec!["--model".to_string(), "o4-mini".to_string()];
    inject_workspace_trust_args(&LaunchTool::Codex, dir, &mut args, true);
    let c_idx = args
        .iter()
        .position(|a| a == "-c")
        .expect("-c not injected");
    let val = &args[c_idx + 1];
    assert!(val.contains("/my/project"), "path missing: {val}");
    assert!(val.contains("trust_level"), "trust_level missing: {val}");
    assert!(val.contains("\"trusted\""), "trusted value missing: {val}");
}

#[test]
fn test_codex_flag_off_no_injection() {
    let dir = std::path::Path::new("/my/project");
    let mut args = vec!["--model".to_string(), "o4-mini".to_string()];
    inject_workspace_trust_args(&LaunchTool::Codex, dir, &mut args, false);
    assert!(!args.iter().any(|a| a == "-c"));
}

#[test]
fn test_codex_inject_idempotent_when_present() {
    let dir = std::path::Path::new("/my/project");
    // Any -c value containing "trust_level" suppresses re-injection.
    let existing = r#"projects={ "/my/project" = { trust_level = "trusted" } }"#.to_string();
    let mut args = vec!["-c".to_string(), existing];
    inject_workspace_trust_args(&LaunchTool::Codex, dir, &mut args, true);
    assert_eq!(args.iter().filter(|a| a.as_str() == "-c").count(), 1);
}

#[test]
fn test_codex_dotted_path_encoded_as_single_key() {
    // A path like /Users/x/proj.v2 has a dot in a component. The inline-table
    // format keeps the whole path as one TOML quoted key — no dot-splitting.
    let dir = std::path::Path::new("/Users/x/proj.v2");
    let mut args: Vec<String> = vec![];
    inject_workspace_trust_args(&LaunchTool::Codex, dir, &mut args, true);
    let c_idx = args
        .iter()
        .position(|a| a == "-c")
        .expect("-c not injected");
    let val = &args[c_idx + 1];
    // The full dotted path must survive intact as one quoted string.
    assert!(
        val.contains("\"/Users/x/proj.v2\""),
        "full dotted path must be a single quoted key: {val}"
    );
    assert!(val.contains("trust_level"), "trust_level missing: {val}");
}

#[test]
fn test_codex_windows_verbatim_path_stripped_and_toml_escaped() {
    // std::fs::canonicalize yields \\?\-prefixed paths on Windows. The
    // prefix must go (codex keys projects by the plain absolute form) and
    // backslashes must be TOML-escaped or codex rejects the inline table.
    let dir = std::path::Path::new(r"\\?\C:\Users\x\proj");
    let mut args: Vec<String> = vec![];
    inject_workspace_trust_args(&LaunchTool::Codex, dir, &mut args, true);
    let val = &args[1];
    assert!(
        val.contains(r#""C:\\Users\\x\\proj""#),
        "path must be prefix-stripped and backslash-escaped: {val}"
    );
    assert!(
        !val.contains(r"\\?\"),
        "verbatim prefix must be stripped: {val}"
    );
}

#[test]
fn test_codex_windows_verbatim_unc_path_keeps_server_form() {
    let dir = std::path::Path::new(r"\\?\UNC\server\share\dir");
    let mut args: Vec<String> = vec![];
    inject_workspace_trust_args(&LaunchTool::Codex, dir, &mut args, true);
    let val = &args[1];
    // \\?\UNC\server\... → \\server\... → TOML-escaped \\\\server\\...
    assert!(
        val.contains(r#""\\\\server\\share\\dir""#),
        "UNC path must keep its \\\\server form, escaped: {val}"
    );
}

#[test]
fn test_non_trust_tools_unaffected() {
    let dir = std::path::Path::new("/some/workspace");
    for tool in &[
        LaunchTool::Claude,
        LaunchTool::ClaudePty,
        LaunchTool::OpenCode,
    ] {
        let mut args = vec!["--model".to_string(), "x".to_string()];
        inject_workspace_trust_args(tool, dir, &mut args, true);
        assert_eq!(
            args,
            vec!["--model".to_string(), "x".to_string()],
            "{tool:?} args should be unchanged"
        );
    }
}

#[test]
fn sidecar_ambient_env_folds_case_on_windows_only() {
    let mut env = std::collections::HashMap::new();
    env.insert("no_color".to_string(), "1".to_string());
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("MY_SECRET".to_string(), "x".to_string());
    env.insert("HCOM_X".to_string(), "y".to_string());
    let strip = ["NO_COLOR"];
    let win = sidecar_ambient_env(&env, strip.iter().copied(), true);
    assert!(!win.contains_key("no_color") && !win.contains_key("NO_COLOR"));
    assert!(win.contains_key("MY_SECRET") && !win.contains_key("HCOM_X"));
    let unix = sidecar_ambient_env(&env, strip.iter().copied(), false);
    assert!(!unix.contains_key("NO_COLOR") && unix.contains_key("no_color")); // Unix exact-case preserved
}

/// Same isolation `hooks::plugin::tests::plugin_test_env` uses: a fresh
/// HOME so `Config` (cached, and what the plugin verifiers resolve paths
/// through) agrees with the fixtures a test writes.
fn hooks_missing_test_env() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    crate::hooks::test_helpers::EnvGuard,
) {
    let guard = crate::hooks::test_helpers::EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::remove_var("CURSOR_CONFIG_DIR");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::remove_var("GEMINI_CLI_HOME");
    }
    crate::paths::test_roots::register(&home);
    crate::config::Config::reset();
    crate::config::Config::init();
    (dir, home, guard)
}

/// Every file under `root`, by path, with its bytes — used to prove a call
/// touched nothing on disk rather than just the one file a test happened
/// to hardcode.
fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap_or_default();
                out.insert(p, bytes);
            }
        }
    }
    out
}

/// The regression guard this task exists for: launching must never install
/// hooks, write config, or block on a missing plugin — for every tool
/// `ensure_hooks_installed` now only warns about, not just Claude.
#[test]
#[serial]
fn launching_never_installs_hooks() {
    let (_dir, home, _guard) = hooks_missing_test_env();
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, "{}\n").unwrap();

    // Snapshot after Config::init (in hooks_missing_test_env) has already
    // done its own writes, so those don't read as noise below.
    let before = snapshot(&home);

    for tool in [
        LaunchTool::Claude,
        LaunchTool::ClaudePty,
        LaunchTool::Cursor,
        LaunchTool::Antigravity,
    ] {
        let result = super::ensure_hooks_installed(&tool, false, None, &home);
        assert!(
            result.is_ok(),
            "{tool:?}: a missing plugin must not block a launch"
        );
    }

    let after = snapshot(&home);
    assert_eq!(
        before, after,
        "launching must not write, create, or delete any file under HOME"
    );
    assert!(
        !home.join(".claude/plugins").exists(),
        "launching must not install a plugin"
    );
}

#[test]
#[serial]
fn launching_reports_the_install_command_when_hooks_are_missing() {
    let (_dir, _home, _guard) = hooks_missing_test_env();
    for (tool, name) in [
        (LaunchTool::Claude, "claude"),
        (LaunchTool::ClaudePty, "claude"),
        (LaunchTool::Antigravity, "antigravity"),
    ] {
        let warning = super::hooks_missing_warning(&tool);
        assert!(
            warning.contains(&format!("hcom hooks add {name}")),
            "{tool:?}: {warning}"
        );
        assert!(warning.contains("not installed"), "{warning}");
    }
}

/// Mirrors `plugin_status_line`'s own ban (hooks.rs) on this exact claim:
/// `cursor-agent` can run hcom's hooks straight out of Claude's plugin cache
/// with no Cursor marketplace at all (measured 2026-09-08), and
/// `install_cursor_plugin` leaves the cache-less state on purpose right after
/// a launch drives the install. Saying "not installed" here would be false in
/// exactly the same way, right after telling the user the install is
/// proceeding normally.
///
/// Without `hooks_missing_test_env()`, which branch of `hooks_missing_warning`
/// this test actually exercises (Claude-covered vs. neither-covers) depended
/// on whatever the real host's Claude plugin state happened to be — a test
/// isolation regression introduced once Task 1 gave the Cursor branch two
/// distinct outcomes instead of one constant string. Fixed by sandboxing
/// `$HOME` the same way `hooks_missing_warning_cursor_is_definitive_when_claude_covers_it`
/// does, and asserting the original two claims hold on BOTH branches instead
/// of leaving it to chance which one ran.
#[test]
#[serial]
fn hooks_missing_warning_cursor_does_not_claim_not_installed() {
    // Branch 1: neither Claude nor Cursor's own cache covers it.
    {
        let (_dir, _home, _guard) = hooks_missing_test_env();
        let warning = super::hooks_missing_warning(&LaunchTool::Cursor);
        assert!(
            !warning.contains("not installed"),
            "must not claim hooks are not installed for cursor. got: {warning:?}"
        );
        assert!(
            warning.contains("hcom hooks add cursor"),
            "must still point at the Cursor-owned install path. got: {warning:?}"
        );
    }
    // Branch 2: Claude's plugin covers it.
    {
        let (_dir, home, _guard) = hooks_missing_test_env();
        crate::hooks::test_helpers::install_fake_claude_plugin(&home);
        let warning = super::hooks_missing_warning(&LaunchTool::Cursor);
        assert!(
            !warning.contains("not installed"),
            "must not claim hooks are not installed for cursor. got: {warning:?}"
        );
        assert!(
            warning.contains("hcom hooks add cursor"),
            "must still point at the Cursor-owned install path. got: {warning:?}"
        );
    }
}

/// Task 8: when `cursor_hooks_covered()` says Claude's plugin covers the
/// hook, `hooks_missing_warning` must give a definitive answer instead of
/// the "may still be running" hedge — this is the launcher-hot-path
/// counterpart to `cursor_status_line`'s Claude-covered branch in hooks.rs.
#[test]
#[serial]
fn hooks_missing_warning_cursor_is_definitive_when_claude_covers_it() {
    let (_dir, home, _guard) = hooks_missing_test_env();
    crate::hooks::test_helpers::install_fake_claude_plugin(&home);

    let warning = super::hooks_missing_warning(&LaunchTool::Cursor);
    assert!(
        warning.contains("Claude") && warning.contains("no separate"),
        "must state plainly that no separate Cursor install is needed: {warning:?}"
    );
    assert!(
        warning.contains("Removing hcom from Claude"),
        "must name the removal consequence: {warning:?}"
    );
    assert!(
        !warning.contains("may still be running"),
        "cursor_hooks_covered() being true makes this definitive: {warning:?}"
    );
}
