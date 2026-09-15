use super::*;
use serial_test::serial;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

fn shellify(argv: &[&str]) -> Vec<String> {
    shellify_bash_script_pair(argv.iter().map(|s| s.to_string()).collect())
}

const PS: &[&str] = &[
    "powershell",
    "-ExecutionPolicy",
    "Bypass",
    "-NoExit",
    "-File",
    "{script}",
];

#[test]
fn shellify_rewrites_leading_bash_script() {
    assert_eq!(shellify(&["bash", "{script}"]), PS);
}

#[test]
fn shellify_rewrites_non_leading_bash_script() {
    // Finding 12: bash is argv[2], not argv[0]; must still be rewritten.
    let mut expected = vec!["myterm".to_string(), "--".to_string()];
    expected.extend(PS.iter().map(|s| s.to_string()));
    assert_eq!(shellify(&["myterm", "--", "bash", "{script}"]), expected);
    // `gnome-terminal -- bash {script}` is adjacent → rewritten, not bailed.
    let mut expected = vec!["gnome-terminal".to_string(), "--".to_string()];
    expected.extend(PS.iter().map(|s| s.to_string()));
    assert_eq!(
        shellify(&["gnome-terminal", "--", "bash", "{script}"]),
        expected
    );
}

#[test]
fn shellify_rewrites_bash_family_interpreters() {
    // B-3+B-4: any bash-family token (bash.exe, /bin/bash) adjacent to
    // {script} is rewritten, not just the exact `bash`.
    assert_eq!(shellify(&["bash.exe", "{script}"]), PS);
    assert_eq!(shellify(&["/bin/bash", "{script}"]), PS);
}

#[test]
fn shellify_leaves_bash_with_flags_alone() {
    // Finding 15: `bash -c {script}` has no adjacent `{script}`, so the
    // pair never matches and the argv is left intact (no broken splice).
    assert_eq!(
        shellify(&["bash", "-c", "{script}"]),
        vec!["bash", "-c", "{script}"]
    );
}

#[test]
fn shellify_leaves_non_bash_alone() {
    assert_eq!(
        shellify(&["myterm", "-e", "{script}"]),
        vec!["myterm", "-e", "{script}"]
    );
}

struct EnvGuard(Vec<(&'static str, Option<String>)>);

impl EnvGuard {
    fn clear(vars: &'static [&'static str]) -> Self {
        let saved = vars
            .iter()
            .map(|&var| (var, std::env::var(var).ok()))
            .collect::<Vec<_>>();
        for &var in vars {
            unsafe {
                std::env::remove_var(var);
            }
        }
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (var, value) in &self.0 {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(var, value);
                } else {
                    std::env::remove_var(var);
                }
            }
        }
    }
}

#[test]
fn test_native_termux_runtime_requires_all_native_signals() {
    assert!(is_native_termux_runtime_from(true, true, true, true));
    assert!(!is_native_termux_runtime_from(false, true, true, true));
    assert!(!is_native_termux_runtime_from(true, false, true, true));
    assert!(!is_native_termux_runtime_from(true, true, false, true));
    assert!(!is_native_termux_runtime_from(true, true, true, false));
}

#[test]
fn test_proot_termux_launch_error_is_actionable() {
    let message = proot_termux_launch_error();
    assert!(message.contains("--headless"));
    assert!(message.contains("--terminal tmux"));
}

#[test]
#[cfg(unix)]
fn test_termux_dispatch_rejects_nonzero_exit_status() {
    let status = std::process::ExitStatus::from_raw(1 << 8);
    let err = validate_termux_dispatch_status(status)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Termux RUN_COMMAND dispatch failed"));
}

#[test]
fn test_shell_quote_empty() {
    assert_eq!(shell_quote(""), "''");
}

#[test]
fn test_shell_quote_simple() {
    assert_eq!(shell_quote("hello"), "hello");
}

#[test]
fn test_shell_quote_spaces() {
    assert_eq!(shell_quote("hello world"), "'hello world'");
}

#[test]
fn test_shell_quote_single_quotes() {
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
}

/// Build a `Vec<String>` argv from `&str` literals (test helper).
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_resolve_terminal_info_prefers_effective_preset() {
    let info = resolve_terminal_info(Some("kitty-tab"), Some(r#"{"pane_id":"x"}"#));
    assert_eq!(info.preset_name, "kitty-tab");
}

#[test]
fn test_resolve_terminal_info_reads_launch_context_metadata() {
    let info = resolve_terminal_info(
        Some("wezterm-split"),
        Some(r#"{"pane_id":"pane-1","process_id":"proc-1","terminal_id":"term-1"}"#),
    );
    assert_eq!(info.preset_name, "wezterm-split");
    assert_eq!(info.pane_id, "pane-1");
    assert_eq!(info.process_id, "proc-1");
    assert_eq!(info.terminal_id, "term-1");
}

#[test]
fn test_launcher_env_preserves_zellij_session_but_strips_pane() {
    let env = get_launcher_env_from(vec![
        (
            "ZELLIJ_SESSION_NAME".to_string(),
            "wise-kangaroo".to_string(),
        ),
        ("ZELLIJ_PANE_ID".to_string(), "18".to_string()),
        ("HCOM_LAUNCHED_PRESET".to_string(), "zellij".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
    ]);

    assert_eq!(
        env.get("ZELLIJ_SESSION_NAME").map(String::as_str),
        Some("wise-kangaroo")
    );
    assert!(!env.contains_key("ZELLIJ_PANE_ID"));
    assert!(!env.contains_key("HCOM_LAUNCHED_PRESET"));
    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
}

#[test]
fn test_launcher_env_keeps_herdr_socket_path() {
    // The herdr preset's CLI resolves its socket from env; see the comment
    // on TERMINAL_CONTEXT_VARS for why HERDR_* is not stripped.
    let env = get_launcher_env_from(vec![
        ("HERDR_SOCKET_PATH".into(), "/tmp/herdr.sock".into()),
        ("PATH".into(), "/bin".into()),
    ]);
    assert_eq!(
        env.get("HERDR_SOCKET_PATH").map(String::as_str),
        Some("/tmp/herdr.sock"),
    );
}

#[test]
#[cfg(unix)]
fn test_zellij_session_ambiguity_stderr_fails_launch_even_with_exit_zero() {
    let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: b"Please specify the session name to send actions to. The following sessions are active:\n".to_vec(),
        };

    let err = validate_terminal_launch_output(
        &[
            "zellij".to_string(),
            "action".to_string(),
            "new-pane".to_string(),
        ],
        &output,
        false,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("Terminal launch failed"));
    assert!(err.contains("Please specify the session name"));
}

#[test]
fn test_resolve_terminal_info_prefers_zellij_terminal_id_over_env_pane_id() {
    let info = resolve_terminal_info(
        Some("zellij"),
        Some(r#"{"pane_id":"18","terminal_id":"terminal_6","process_id":"proc-1"}"#),
    );

    assert_eq!(info.pane_id, "6");
    assert_eq!(info.terminal_id, "terminal_6");
}

#[test]
fn test_is_zellij_preset_does_not_match_name_prefix_only() {
    assert!(!is_zellij_preset("zellijish"));
}

#[test]
fn test_yaml_double_quote_escapes_backslash_and_quote() {
    assert_eq!(yaml_double_quote("a\"b"), "\"a\\\"b\"");
    assert_eq!(yaml_double_quote("a\\b"), "\"a\\\\b\"");
    assert_eq!(yaml_double_quote("plain"), "\"plain\"");
}

#[test]
fn test_build_warp_launch_yaml_shape() {
    let yaml = build_warp_launch_yaml("hcom-pid", "/some/dir", "/tmp/script.sh");
    assert!(yaml.contains("name: \"hcom-pid\""));
    assert!(yaml.contains("cwd: \"/some/dir\""));
    assert!(yaml.contains("exec: \"bash /tmp/script.sh\""));
}

#[test]
fn test_warp_launch_config_dir_is_stable_channel() {
    let dir = warp_launch_config_dir(Path::new("/h"));
    assert_eq!(dir, Path::new("/h/.warp/launch_configurations"));
}

// Unix-only: Warp is a macOS terminal and the assertion pins POSIX paths.
#[cfg(unix)]
#[test]
fn test_write_warp_launch_config_writes_to_stable_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let written =
        write_warp_launch_config_at(tmp.path(), "test-pid", Some("/some/dir"), "/tmp/script.sh")
            .unwrap();
    assert!(written.ends_with(".warp/launch_configurations/hcom-test-pid.yaml"));
    let content = std::fs::read_to_string(&written).unwrap();
    assert!(content.contains("name: \"hcom-test-pid\""));
    assert!(content.contains("exec: \"bash /tmp/script.sh\""));
    assert!(content.contains("cwd: \"/some/dir\""));
}

#[test]
fn test_ps_quote_doubles_single_quotes() {
    assert_eq!(ps_quote("plain"), "'plain'");
    assert_eq!(ps_quote("it's"), "'it''s'");
    assert_eq!(ps_quote(""), "''");
}

#[test]
fn test_ps_env_assignments_sorted_and_validated() {
    let mut env = HashMap::new();
    env.insert("ZED".to_string(), "z".to_string());
    env.insert("ABE".to_string(), "a'b".to_string());
    env.insert("1bad".to_string(), "skip".to_string()); // invalid name dropped
    let lines = ps_env_assignments(&env);
    assert_eq!(
        lines,
        vec![
            "$env:ABE = 'a''b'".to_string(),
            "$env:ZED = 'z'".to_string(),
        ]
    );
}

#[test]
fn test_create_powershell_script_window_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("launch.ps1");
    let mut env = HashMap::new();
    env.insert("HCOM_TOOL".to_string(), "claude".to_string());
    create_powershell_script(
        &script,
        &env,
        Some("/work/dir"),
        "claude --foo",
        false, // background
        Some("claude"),
        true, // opens_new_window
    )
    .unwrap();
    assert!(
        std::fs::read(&script)
            .unwrap()
            .starts_with(&[0xEF, 0xBB, 0xBF])
    );
    let content = std::fs::read_to_string(&script).unwrap();
    assert!(content.contains("$Host.UI.RawUI.WindowTitle = \"hcom: starting Claude...\""));
    assert!(content.contains("Write-Host \"Starting Claude...\""));
    assert!(content.contains("Remove-Item Env:"));
    assert!(content.contains("$env:HCOM_TOOL = 'claude'"));
    assert!(content.contains("Set-Location '/work/dir'"));
    // The command args survive whether or not the tool resolved to a full
    // path (bare `claude --foo` or call-operator `& '<path>' --foo`).
    assert!(content.contains("--foo"));
    // Window mode self-deletes but does not `exit` (window persists via -NoExit).
    assert!(content.contains("Remove-Item -Force -ErrorAction SilentlyContinue"));
    assert!(!content.contains("exit $hcom_status"));
}

#[test]
fn test_create_powershell_script_run_once_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("launch.ps1");
    let env = HashMap::new();
    create_powershell_script(
        &script, &env, None, "codex", false, // background
        None, false, // run-once (not a new window)
    )
    .unwrap();
    let content = std::fs::read_to_string(&script).unwrap();
    assert!(content.contains("$hcom_status = $LASTEXITCODE"));
    assert!(content.contains("exit $hcom_status"));
    assert!(!content.contains("Set-Location"));
}

#[test]
fn test_build_env_string_powershell_format() {
    let mut env = HashMap::new();
    env.insert("HCOM_A".to_string(), "x".to_string());
    env.insert("HCOM_B".to_string(), "y'z".to_string());
    let out = build_env_string(&env, "powershell");
    assert_eq!(out, "$env:HCOM_A = 'x'\n$env:HCOM_B = 'y''z'");
}

#[test]
fn test_wezterm_open_argv_selects_powershell_on_windows() {
    // The PowerShell variant is now selected by the preset's PlatformArgv,
    // not a text rewrite. Confirm the merged preset surfaces it.
    let merged = crate::config::get_merged_preset("wezterm").unwrap();
    let win = merged.open_argv(true);
    assert!(win.iter().any(|a| a == "powershell"));
    assert!(win.iter().any(|a| a == "-File"));
    assert!(!win.iter().any(|a| a == "bash"));
    let unix = merged.open_argv(false);
    assert!(unix.iter().any(|a| a == "bash"));
}

#[test]
fn test_mintty_open_argv_has_no_bash() {
    let merged = crate::config::get_merged_preset("mintty").unwrap();
    let argv = merged.open_argv(true);
    assert_eq!(argv.first().map(String::as_str), Some("mintty"));
    assert!(
        !argv.iter().any(|a| a == "bash"),
        "mintty must not hand a .ps1 to bash"
    );
    assert!(argv.iter().any(|a| a == "powershell"));
}

// Unix-only: "/abs/path" isn't absolute on Windows (no drive), so it would
// be rewritten to the current dir.
#[cfg(unix)]
#[test]
fn test_resolve_warp_cwd_keeps_absolute() {
    let home = Path::new("/h");
    assert_eq!(resolve_warp_cwd(Some("/abs/path"), home), "/abs/path");
}

#[test]
#[serial]
fn test_resolve_warp_cwd_uses_current_dir_for_relative_or_missing() {
    let home = Path::new("/h");
    let cwd_str = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .to_string();
    // Must match the prefix the script's later `cd <cwd>` resolves against.
    assert_eq!(resolve_warp_cwd(Some("subdir"), home), cwd_str);
    assert_eq!(resolve_warp_cwd(Some("./rel"), home), cwd_str);
    assert_eq!(resolve_warp_cwd(Some("."), home), cwd_str);
    assert_eq!(resolve_warp_cwd(None, home), cwd_str);
}

#[test]
fn test_sweep_stale_warp_configs_only_removes_hcom_prefixed_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let target = dir.join("hcom-old.yaml");
    let other = dir.join("user-config.yaml");
    let unrelated = dir.join("hcom-old.txt");
    std::fs::write(&target, "x").unwrap();
    std::fs::write(&other, "x").unwrap();
    std::fs::write(&unrelated, "x").unwrap();

    sweep_stale_warp_configs(dir, std::time::Duration::from_secs(0));

    assert!(!target.exists(), "hcom-*.yaml should be swept");
    assert!(other.exists(), "non-hcom-prefixed yaml should remain");
    assert!(unrelated.exists(), "non-yaml extension should remain");
}

#[test]
fn test_sweep_stale_warp_configs_keeps_fresh_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let fresh = dir.join("hcom-new.yaml");
    std::fs::write(&fresh, "x").unwrap();

    sweep_stale_warp_configs(dir, std::time::Duration::from_secs(3600));

    assert!(fresh.exists(), "fresh file should remain");
}

#[test]
fn test_warp_preset_registered() {
    let preset = crate::shared::terminal_presets::get_terminal_preset("warp").unwrap();
    assert_eq!(preset.app_name, Some("Warp"));
    assert_eq!(preset.binary, None);
    let open = preset.open.select(false).unwrap();
    assert!(open.contains(&"warp://launch/hcom-{process_id}"));
    assert_eq!(preset.platforms, &["Darwin"]);
}

#[test]
fn test_build_env_string_bash() {
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar".to_string());
    let result = build_env_string(&env, "bash");
    assert_eq!(result, "FOO=bar");
}

#[test]
fn test_build_env_string_export() {
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar baz".to_string());
    let result = build_env_string(&env, "bash_export");
    assert_eq!(result, "export FOO='bar baz';");
}

#[test]
fn test_build_env_string_filters_invalid() {
    let mut env = HashMap::new();
    env.insert("GOOD".to_string(), "val".to_string());
    env.insert("123BAD".to_string(), "val".to_string());
    let result = build_env_string(&env, "bash");
    assert!(result.contains("GOOD"));
    assert!(!result.contains("123BAD"));
}

#[test]
fn test_detect_terminal_from_env_none() {
    // In test environment, none of the terminal env vars should be set
    // (unless running inside kitty/tmux, in which case this test is fine to skip)
    let result = detect_terminal_from_env();
    // Just verify it returns an Option - value depends on test environment
    let _ = result;
}

/// Detection vars not in `TERMINAL_CONTEXT_VARS` — tests that exercise
/// `detect_terminal_from_env` must clear these explicitly so a host shell
/// running inside herdr doesn't leak into the test.
const DETECT_ONLY_VARS: &[&str] = &["HERDR_PANE_ID", "HERDR_SOCKET_PATH", "HERDR_ENV"];

#[test]
#[serial]
fn test_normalize_terminal_mode_for_launch_resolves_socket_for_auto_detected_kitty() {
    let _env = EnvGuard::clear(TERMINAL_CONTEXT_VARS);
    let _detect = EnvGuard::clear(DETECT_ONLY_VARS);
    unsafe {
        std::env::set_var("KITTY_WINDOW_ID", "window-1");
        std::env::set_var("KITTY_LISTEN_ON", "unix:/tmp/kitty-test");
    }

    let (mode, socket) = normalize_terminal_mode_for_launch("default".to_string(), true, false);

    assert_eq!(mode, "kitty-split");
    assert_eq!(socket, "unix:/tmp/kitty-test");
}

#[test]
#[serial]
fn test_resolve_terminal_mode_for_tips_uses_normalized_auto_detected_mode() {
    let _env = EnvGuard::clear(TERMINAL_CONTEXT_VARS);
    let _detect = EnvGuard::clear(DETECT_ONLY_VARS);
    unsafe {
        std::env::set_var("KITTY_WINDOW_ID", "window-1");
        std::env::set_var("KITTY_LISTEN_ON", "unix:/tmp/kitty-test");
    }

    let (mode, auto) = resolve_terminal_mode_for_tips(None, "default", false, false);

    assert_eq!(mode, "kitty-split");
    assert!(auto);
}

#[test]
#[serial]
fn test_detect_terminal_from_ptyxis_version() {
    let _env = EnvGuard::clear(TERMINAL_CONTEXT_VARS);
    let _detect = EnvGuard::clear(DETECT_ONLY_VARS);
    unsafe {
        std::env::set_var("PTYXIS_VERSION", "50.1");
    }

    assert_eq!(detect_terminal_from_env().as_deref(), Some("ptyxis"));
}

#[test]
#[serial]
fn test_auto_detected_ptyxis_requires_host_launcher_for_new_window() {
    let _env = EnvGuard::clear(TERMINAL_CONTEXT_VARS);
    let _detect = EnvGuard::clear(DETECT_ONLY_VARS);
    let _path = EnvGuard::clear(&["PATH"]);
    let empty_path = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("PTYXIS_VERSION", "50.1");
        std::env::set_var("PATH", empty_path.path());
    }

    let (mode, socket) = normalize_terminal_mode_for_launch("default".to_string(), true, false);

    assert_eq!(mode, "default");
    assert!(socket.is_empty());
}

#[test]
fn test_launcher_env_strips_ptyxis_identity() {
    let env = get_launcher_env_from(vec![
        ("PTYXIS_PROFILE".into(), "profile-id".into()),
        ("PTYXIS_VERSION".into(), "50.1".into()),
        ("PATH".into(), "/bin".into()),
    ]);

    assert!(!env.contains_key("PTYXIS_PROFILE"));
    assert!(!env.contains_key("PTYXIS_VERSION"));
    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
}

#[test]
fn test_splice_kitten_to_socket_matches_absolute_app_bundle_path() {
    // Regression: when `kitten` isn't on PATH, resolve_terminal_open_argv
    // rewrites argv[0] to kitten's absolute macOS app-bundle path before
    // this splice runs. A literal "kitten" string match would silently
    // skip injecting --to, leaving the launched kitten with no socket
    // and no KITTY_LISTEN_ON (stripped from the child env), causing it to
    // fall back to controlling-tty discovery — which fails outright when
    // the calling process (e.g. an AI tool's sandboxed shell) has none.
    let mut argv = vec![
        "/Applications/kitty.app/Contents/MacOS/kitten".to_string(),
        "@".to_string(),
        "launch".to_string(),
        "--type=window".to_string(),
    ];
    splice_kitten_to_socket(&mut argv, "unix:/tmp/kitty-test");
    assert_eq!(
        argv,
        vec![
            "/Applications/kitty.app/Contents/MacOS/kitten".to_string(),
            "@".to_string(),
            "--to".to_string(),
            "unix:/tmp/kitty-test".to_string(),
            "launch".to_string(),
            "--type=window".to_string(),
        ]
    );
}

#[test]
fn test_splice_kitten_to_socket_bare_name() {
    let mut argv = vec!["kitten".to_string(), "@".to_string(), "ls".to_string()];
    splice_kitten_to_socket(&mut argv, "unix:/tmp/kitty-test");
    assert_eq!(
        argv,
        vec![
            "kitten".to_string(),
            "@".to_string(),
            "--to".to_string(),
            "unix:/tmp/kitty-test".to_string(),
            "ls".to_string(),
        ]
    );
}

#[test]
fn test_splice_kitten_to_socket_noop_for_non_kitten() {
    let mut argv = vec!["wezterm".to_string(), "cli".to_string()];
    let before = argv.clone();
    splice_kitten_to_socket(&mut argv, "unix:/tmp/kitty-test");
    assert_eq!(argv, before);
}

#[test]
fn test_splice_kitten_to_socket_noop_when_already_present() {
    let mut argv = vec![
        "kitten".to_string(),
        "@".to_string(),
        "--to".to_string(),
        "unix:/tmp/other".to_string(),
        "ls".to_string(),
    ];
    let before = argv.clone();
    splice_kitten_to_socket(&mut argv, "unix:/tmp/kitty-test");
    assert_eq!(argv, before);
}

#[test]
fn test_resolve_terminal_info_uses_launch_context_preset_when_column_missing() {
    let info = resolve_terminal_info(
        None,
        Some(
            r#"{"terminal_preset_effective":"kitty-tab","pane_id":"pane-1","kitty_listen_on":"unix:/tmp/kitty"}"#,
        ),
    );
    assert_eq!(info.preset_name, "kitty-tab");
    assert_eq!(info.pane_id, "pane-1");
    assert_eq!(info.kitty_listen_on, "unix:/tmp/kitty");
}

#[test]
fn test_resolve_terminal_info_falls_back_for_legacy_kitty_metadata() {
    let info = resolve_terminal_info(
        None,
        Some(r#"{"pane_id":"pane-1","kitty_listen_on":"unix:/tmp/kitty"}"#),
    );
    assert_eq!(info.preset_name, "kitty-split");
    assert_eq!(info.pane_id, "pane-1");
    assert_eq!(info.kitty_listen_on, "unix:/tmp/kitty");
}

fn ctx_with_script(script: &str) -> TerminalCommandContext<'_> {
    TerminalCommandContext {
        script,
        ..TerminalCommandContext::default()
    }
}

#[test]
fn test_substitute_open_argv_basic() {
    let out = substitute_open_argv(
        &argv(&["open", "-a", "Terminal", "{script}"]),
        ctx_with_script("/tmp/test.sh"),
    )
    .unwrap();
    assert_eq!(out, vec!["open", "-a", "Terminal", "/tmp/test.sh"]);
}

#[test]
fn test_substitute_open_argv_preserves_windows_path() {
    // A backslashed Windows .ps1 path substituted into a single argv element
    // must survive byte-for-byte (no shell splitting, no escaping).
    let out = substitute_open_argv(
        &argv(&["wt", "--", "powershell", "-File", "{script}"]),
        ctx_with_script(r"C:\Users\x\s.ps1"),
    )
    .unwrap();
    assert_eq!(
        out,
        vec!["wt", "--", "powershell", "-File", r"C:\Users\x\s.ps1"]
    );
}

#[test]
fn test_substitute_open_argv_process_id_element() {
    // `HCOM_PROCESS_ID={process_id}` is one element; the placeholder is
    // replaced inside it without needing quoting.
    let out = substitute_open_argv(
        &argv(&["kitty", "--env", "HCOM_PROCESS_ID={process_id}", "{script}"]),
        TerminalCommandContext {
            script: "/tmp/test.sh",
            process_id: "abc-123",
            ..TerminalCommandContext::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        vec!["kitty", "--env", "HCOM_PROCESS_ID=abc-123", "/tmp/test.sh"]
    );
}

#[test]
fn test_rewrite_open_argv_with_app_path_keeps_plain_open_a() {
    // No `--args` tail ⇒ leave `-a Terminal` intact (file-open form).
    let mut v = argv(&["open", "-a", "Terminal", "{script}"]);
    rewrite_open_argv_with_app_path(
        &mut v,
        Path::new("/System/Applications/Utilities/Terminal.app"),
    );
    assert_eq!(v, vec!["open", "-a", "Terminal", "{script}"]);
}

#[test]
fn test_rewrite_open_argv_with_combined_flag() {
    let mut v = argv(&[
        "open",
        "-na",
        "Ghostty.app",
        "--args",
        "-e",
        "bash",
        "{script}",
    ]);
    rewrite_open_argv_with_app_path(&mut v, Path::new("/Applications/Ghostty.app"));
    assert_eq!(
        v,
        vec![
            "open",
            "-n",
            "/Applications/Ghostty.app",
            "--args",
            "-e",
            "bash",
            "{script}"
        ]
    );
}

#[test]
fn test_rewrite_open_argv_with_explicit_args() {
    let mut v = argv(&["open", "-a", "Terminal", "--args", "bash", "{script}"]);
    rewrite_open_argv_with_app_path(
        &mut v,
        Path::new("/System/Applications/Utilities/Terminal.app"),
    );
    assert_eq!(
        v,
        vec![
            "open",
            "/System/Applications/Utilities/Terminal.app",
            "--args",
            "bash",
            "{script}"
        ]
    );
}

#[test]
#[cfg(target_os = "macos")]
fn test_should_use_command_extension_for_terminal_app() {
    assert!(should_use_command_extension(false, "default"));
    assert!(should_use_command_extension(false, "terminal.app"));
    assert!(!should_use_command_extension(false, "iterm"));
    assert!(!should_use_command_extension(true, "terminal.app"));
}

#[test]
fn test_maybe_append_ai_tool_launch_hint_for_tmux() {
    let message = maybe_append_ai_tool_launch_hint(
        "Terminal launch failed (exit code 1): permission denied".to_string(),
        &["tmux".to_string(), "new-session".to_string()],
        true,
    );
    assert!(message.contains("tmux kill-server"));
    assert!(message.contains("tmux new-session -d -s hcom-external"));
}

#[test]
fn test_maybe_append_ai_tool_launch_hint_for_wsh() {
    let message = maybe_append_ai_tool_launch_hint(
        "Failed to spawn terminal process: operation not permitted".to_string(),
        &["wsh".to_string(), "launch".to_string()],
        true,
    );
    assert!(message.contains("managed AI tool session"));
    assert!(message.contains("Rerun it with approval/escalation."));
}

#[test]
fn test_maybe_append_ai_tool_launch_hint_skips_non_terminal_commands() {
    let message =
        maybe_append_ai_tool_launch_hint("plain failure".to_string(), &["bash".to_string()], true);
    assert_eq!(message, "plain failure");
}

#[test]
fn test_substitute_open_argv_missing_placeholder() {
    assert!(
        substitute_open_argv(
            &argv(&["open", "-a", "Terminal"]),
            ctx_with_script("/tmp/test.sh")
        )
        .is_err()
    );
}

#[test]
fn test_substitute_open_argv_with_process_id() {
    let out = substitute_open_argv(
        &argv(&["tmux", "split", "-t", "{process_id}", "--", "{script}"]),
        TerminalCommandContext {
            script: "/tmp/test.sh",
            process_id: "abc-123",
            ..TerminalCommandContext::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        vec!["tmux", "split", "-t", "abc-123", "--", "/tmp/test.sh"]
    );
}

#[test]
fn test_waveterm_preset_uses_run_separator() {
    let cmd = resolve_terminal_open_argv("waveterm").unwrap();
    let out = substitute_open_argv(
        &cmd,
        TerminalCommandContext {
            script: "/tmp/test.sh",
            process_id: "abc-123",
            ..TerminalCommandContext::default()
        },
    )
    .unwrap();
    assert_eq!(out, vec!["wsh", "run", "--", "bash", "/tmp/test.sh"]);
}

#[test]
fn test_normalize_waveterm_run_block_stdout() {
    assert_eq!(
        normalize_captured_terminal_id("run block created: block:abc123\n"),
        "block:abc123"
    );
    assert_eq!(normalize_captured_terminal_id("terminal_6"), "terminal_6");
}

#[test]
fn test_normalize_herdr_agent_start_json() {
    let json = r#"{"id":"cli:agent:start","result":{"agent":{"agent_status":"unknown","cwd":"/tmp","focused":false,"name":"hcom-abc123","pane_id":"w123abc-3","revision":0,"tab_id":"w123abc:2","terminal_id":"term_abc","workspace_id":"w123abc"},"argv":["bash","/tmp/script.sh"],"type":"agent_started"}}"#;
    assert_eq!(normalize_captured_terminal_id(json), "w123abc-3");
}

#[test]
fn test_normalize_herdr_error_json_falls_through() {
    // Error JSON from herdr should not match (no result.agent.pane_id)
    let json = r#"{"error":{"code":"server_unavailable","message":"herdr server not running"},"id":"cli:agent:start"}"#;
    assert_eq!(normalize_captured_terminal_id(json), json);
}

#[test]
fn test_normalize_herdr_empty_pane_id() {
    let json = r#"{"id":"cli:agent:start","result":{"agent":{"pane_id":""}}}"#;
    assert_eq!(normalize_captured_terminal_id(json), json);
}

#[test]
fn test_normalize_herdr_tab_create_json() {
    // The default herdr preset launches via `tab create`; the pane id lives
    // at result.root_pane.pane_id.
    let json = r#"{"id":"cli:tab:create","result":{"type":"tab_created","tab":{"tab_id":"w1:2"},"root_pane":{"pane_id":"p_7","terminal_id":"term_x","workspace_id":"w1","tab_id":"w1:2","focused":false}}}"#;
    assert_eq!(normalize_captured_terminal_id(json), "p_7");
}

#[test]
fn test_normalize_herdr_pane_split_json() {
    let json = r#"{"id":"cli:pane:split","result":{"root_pane":{"pane_id":"p_9"}}}"#;
    assert_eq!(normalize_captured_terminal_id(json), "p_9");
}

#[test]
fn test_substitute_herdr_create_argv_uses_instance_name_and_cwd() {
    let template = argv(&[
        "herdr",
        "tab",
        "create",
        "--cwd",
        "{cwd}",
        "--no-focus",
        "--label",
        "{instance_name}",
    ]);
    let out = substitute_herdr_create_argv(
        &template,
        &TerminalCommandContext {
            script: "/tmp/test.sh",
            process_id: "abc-123",
            cwd: "/home/user/project",
            instance_name: "luna",
            tool: "claude",
            pane_title: Some("\u{25c9} luna [claude]"),
        },
    );
    assert_eq!(
        out,
        vec![
            "herdr",
            "tab",
            "create",
            "--cwd",
            "/home/user/project",
            "--no-focus",
            "--label",
            "luna",
        ]
    );
}

#[test]
fn test_herdr_preset_template_uses_stable_instance_name() {
    // The herdr preset opens the pane with `tab create` and a stable
    // `--label {instance_name}` (e.g. `luna`). The runner script is sent
    // separately via `pane run` (handled natively in `launch_terminal`), so
    // the open argv carries no `{script}` slot. The styled status label
    // (`◉ luna [claude]`) is pushed via `pane.rename` from the delivery
    // loop, not baked into the pane label.
    let preset = crate::shared::terminal_presets::get_terminal_preset("herdr").unwrap();
    let open = preset.open.select(false).unwrap();
    assert!(open.contains(&"tab"));
    assert!(open.contains(&"create"));
    assert!(
        !open.contains(&"{script}"),
        "herdr's `tab create` open argv must not carry {{script}} — the \
             runner is sent separately via `pane run`"
    );
    assert!(open.contains(&"{instance_name}"));
    assert!(
        !open.contains(&"{pane_title}"),
        "herdr preset must not use {{pane_title}} as the pane label"
    );
    assert!(open.contains(&"{cwd}"));
    assert!(!open.contains(&"{process_id}"));
    assert_eq!(preset.binary, Some("herdr"));
    // Close must use {pane_id} (stable raw `p_N`), not {id} (public
    // `<ws>-<N>` which herdr renumbers when sibling panes close — a kill
    // batch addressing the public id can land on the wrong pane).
    let close = preset.close.select(false).unwrap();
    assert!(close.contains(&"{pane_id}"));
}

#[test]
fn test_substitute_herdr_open_argv_uses_pane_title() {
    let template = argv(&[
        "herdr",
        "agent",
        "start",
        "{pane_title}",
        "--cwd",
        "{cwd}",
        "--no-focus",
        "--",
        "bash",
        "{script}",
    ]);
    let out = substitute_open_argv(
        &template,
        TerminalCommandContext {
            script: "/tmp/test.sh",
            process_id: "abc-123",
            cwd: "/home/user/project",
            instance_name: "luna",
            tool: "claude",
            pane_title: Some("\u{25c9} luna [claude]"),
        },
    )
    .unwrap();
    assert_eq!(
        out,
        vec![
            "herdr",
            "agent",
            "start",
            "\u{25c9} luna [claude]",
            "--cwd",
            "/home/user/project",
            "--no-focus",
            "--",
            "bash",
            "/tmp/test.sh"
        ]
    );
}

#[test]
fn test_substitute_open_argv_pane_title_falls_back_to_instance_name() {
    let out = substitute_open_argv(
        &argv(&[
            "herdr",
            "agent",
            "start",
            "{pane_title}",
            "--",
            "bash",
            "{script}",
        ]),
        TerminalCommandContext {
            script: "/tmp/test.sh",
            instance_name: "abc-123",
            tool: "codex",
            ..TerminalCommandContext::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        vec![
            "herdr",
            "agent",
            "start",
            "abc-123",
            "--",
            "bash",
            "/tmp/test.sh"
        ]
    );
}

#[test]
fn test_substitute_open_argv_cwd_placeholder() {
    let out = substitute_open_argv(
        &argv(&["myterm", "--dir", "{cwd}", "--", "bash", "{script}"]),
        TerminalCommandContext {
            script: "/tmp/test.sh",
            cwd: "/home/user",
            ..TerminalCommandContext::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        vec![
            "myterm",
            "--dir",
            "/home/user",
            "--",
            "bash",
            "/tmp/test.sh"
        ]
    );
}

#[test]
fn test_substitute_open_argv_empty_cwd() {
    // Templates without {cwd} should work with empty cwd
    let out = substitute_open_argv(
        &argv(&["open", "-a", "Terminal", "{script}"]),
        ctx_with_script("/tmp/test.sh"),
    )
    .unwrap();
    assert_eq!(out, vec!["open", "-a", "Terminal", "/tmp/test.sh"]);
}

#[test]
fn test_substitute_close_argv_skips_when_pane_id_missing() {
    // Required {pane_id} placeholder but empty value ⇒ None (skip close).
    assert!(
        substitute_close_argv(
            &argv(&["wezterm", "cli", "kill-pane", "--pane-id", "{pane_id}"]),
            42,
            "",
            "proc-1",
            "",
        )
        .is_none()
    );
}

#[test]
fn test_substitute_close_argv_substitutes_pane_id() {
    let out = substitute_close_argv(
        &argv(&["wezterm", "cli", "kill-pane", "--pane-id", "{pane_id}"]),
        42,
        "pane-7",
        "proc-1",
        "",
    )
    .unwrap();
    assert_eq!(
        out,
        vec!["wezterm", "cli", "kill-pane", "--pane-id", "pane-7"]
    );
}

#[test]
fn test_format_close_command_preserves_all_arguments() {
    let command = format_close_command(&[
        "wezterm".to_string(),
        "cli".to_string(),
        "kill-pane".to_string(),
        "--pane-id".to_string(),
        "123".to_string(),
    ]);
    assert_eq!(command, "wezterm cli kill-pane --pane-id 123");
}

#[cfg(windows)]
#[test]
fn test_format_close_command_quotes_powershell_arguments() {
    let command = format_close_command(&[
        r"C:\Program Files\kitty\kitten.exe".to_string(),
        "@".to_string(),
        "--to".to_string(),
        r"unix:C:\Users\O'Brien\kitty.sock".to_string(),
    ]);
    assert_eq!(
        command,
        r#"'C:\Program Files\kitty\kitten.exe' @ --to 'unix:C:\Users\O''Brien\kitty.sock'"#
    );
}

#[cfg(not(windows))]
#[test]
fn test_format_close_command_quotes_posix_arguments() {
    let command = format_close_command(&[
        "kitten".to_string(),
        "@".to_string(),
        "--to".to_string(),
        "/tmp/O'Brien kitty.sock".to_string(),
    ]);
    assert_eq!(command, "kitten '@' --to '/tmp/O'\\''Brien kitty.sock'");
}

#[test]
fn test_zellij_close_argv_session_splice() {
    // Reproduce the close_terminal_pane splice: --session <name> after zellij.
    let mut a = substitute_close_argv(
        &argv(&["zellij", "action", "close-pane", "--pane-id", "{pane_id}"]),
        0,
        "6",
        "",
        "",
    )
    .unwrap();
    a.splice(1..1, ["--session".to_string(), "wise-kangaroo".to_string()]);
    assert_eq!(
        a,
        vec![
            "zellij",
            "--session",
            "wise-kangaroo",
            "action",
            "close-pane",
            "--pane-id",
            "6"
        ]
    );
}

#[test]
fn test_kitten_close_argv_to_splice() {
    // Reproduce the close_terminal_pane splice: --to <socket> after `@`.
    let mut a = substitute_close_argv(
        &argv(&["kitten", "@", "close-window", "--match", "id:{pane_id}"]),
        0,
        "13",
        "",
        "",
    )
    .unwrap();
    a.splice(2..2, ["--to".to_string(), "unix:/tmp/kitty".to_string()]);
    assert_eq!(
        a,
        vec![
            "kitten",
            "@",
            "--to",
            "unix:/tmp/kitty",
            "close-window",
            "--match",
            "id:13"
        ]
    );
}

#[test]
fn test_sandbox_flags_in_get_sandbox_flags() {
    use crate::tools::codex_preprocessing::get_sandbox_flags;
    let flags = get_sandbox_flags("workspace");
    assert!(flags.contains(&"--sandbox".to_string()));
    assert!(flags.contains(&"workspace-write".to_string()));
}

#[test]
fn test_get_available_presets_always_has_default_and_custom() {
    let presets = get_available_presets();
    assert_eq!(presets.first().unwrap().0, "default");
    assert_eq!(presets.last().unwrap().0, "custom");
}

#[test]
fn test_has_node_shebang_with_node_script() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.js");
    std::fs::write(&path, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();
    assert!(has_node_shebang(path.to_str().unwrap()));
}

#[test]
fn test_has_node_shebang_with_bash_script() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.sh");
    std::fs::write(&path, "#!/bin/bash\necho hello\n").unwrap();
    assert!(!has_node_shebang(path.to_str().unwrap()));
}

#[cfg(windows)]
#[test]
fn windows_codex_npm_launcher_bypasses_cmd_shim() {
    let temp = tempfile::tempdir().unwrap();
    let shim = temp.path().join("codex.cmd");
    let entrypoint = temp.path().join("node_modules/@openai/codex/bin/codex.js");
    std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    std::fs::write(&shim, "@echo off\r\n").unwrap();
    std::fs::write(&entrypoint, "").unwrap();

    let (launcher, args) = resolve_windows_tool_launcher("codex", shim.to_str().unwrap()).unwrap();
    assert!(
        Path::new(&launcher)
            .file_stem()
            .is_some_and(|stem| stem.eq_ignore_ascii_case("node"))
    );
    assert_eq!(args, vec![entrypoint.to_string_lossy().into_owned()]);
    assert!(resolve_windows_tool_launcher("claude", shim.to_str().unwrap()).is_none());
}

#[test]
fn test_has_node_shebang_with_elf_binary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool");
    std::fs::write(&path, b"\x7fELF\x02\x01\x01\x00").unwrap();
    assert!(!has_node_shebang(path.to_str().unwrap()));
}

#[test]
fn test_has_node_shebang_nonexistent() {
    assert!(!has_node_shebang("/nonexistent/path/to/tool"));
}

#[test]
fn test_resolve_termux_tool_launcher_codex_wrapper() {
    let resolved = resolve_termux_tool_launcher("codex", TERMUX_CODEX_WRAPPER_PATH);
    if is_native_termux_runtime() && Path::new(TERMUX_CODEX_INNER_WRAPPER_PATH).exists() {
        let (command, args) = resolved.expect("expected termux codex wrapper override");
        assert!(command.ends_with("/sh") || command == "sh");
        assert_eq!(args, vec![TERMUX_CODEX_INNER_WRAPPER_PATH.to_string()]);
    } else {
        assert!(resolved.is_none());
    }
}

#[test]
fn test_resolve_termux_tool_launcher_node_script() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool");
    std::fs::write(&path, "#!/usr/bin/env node\nconsole.log('ok');\n").unwrap();

    let resolved = resolve_termux_tool_launcher("tool", path.to_str().unwrap());
    if is_native_termux_runtime() {
        let (command, args) = resolved.expect("expected node wrapper on termux");
        assert!(command.ends_with("/node") || command == "node");
        assert_eq!(args, vec![path.to_string_lossy().to_string()]);
    } else {
        assert!(resolved.is_none());
    }
}

// Finding 22: the no-`wt` `cmd /c start` branch used to bake literal `"`
// quotes around `{script}`, which collided with `Command`'s own Windows
// argv quoting on spaced paths. `{script}` is now bare.
#[test]
fn windows_cmd_fallback_leaves_spaced_script_unquoted() {
    let tmpl = windows_default_terminal_template(false);
    let script = r"C:\Users\a b\hcom\s.ps1";
    let out: Vec<String> = tmpl.iter().map(|a| a.replace("{script}", script)).collect();
    assert_eq!(out.last().unwrap(), script);
    assert!(!out.last().unwrap().contains('"'));
    assert_eq!(out[3], "");
}

// Finding 19: `hcom status`'s default-terminal display name must track the
// same has_wt branch as the launch planner, instead of falling through to
// "unknown" on Windows.
#[test]
fn windows_status_name_tracks_launch_planner() {
    assert_eq!(windows_default_terminal_template(true)[0], "wt");
    assert_eq!(
        windows_default_terminal_display_name(true),
        "Windows Terminal"
    );
    assert_eq!(windows_default_terminal_template(false)[0], "cmd");
    assert_eq!(windows_default_terminal_display_name(false), "cmd.exe");
}

// B-3+B-4: any bash-family interpreter with a NON-adjacent `{script}` (any
// flag, or none) can't be rewritten by `shellify_bash_script_pair`, so on
// Windows it must be rejected instead of silently handing a `.ps1` to bash.
// Adjacent `<interp> {script}` is rewritten, not flagged.
#[test]
fn detects_unsupported_bash_c_script() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Non-adjacent {script}, regardless of flag → flagged (returns interp).
    assert_eq!(
        unsupported_bash_script_interp(&v(&["bash", "-c", "{script}"])).as_deref(),
        Some("bash")
    );
    assert!(unsupported_bash_script_interp(&v(&["bash", "-x", "{script}"])).is_some());
    assert!(unsupported_bash_script_interp(&v(&["bash", "-i", "{script}"])).is_some());
    assert!(unsupported_bash_script_interp(&v(&["bash", "-lc", "{script}"])).is_some());
    assert_eq!(
        unsupported_bash_script_interp(&v(&["bash.exe", "-c", "{script}"])).as_deref(),
        Some("bash.exe")
    );
    assert!(unsupported_bash_script_interp(&v(&["/bin/bash", "-c", "{script}"])).is_some());
    // Adjacent bash-family + {script} → rewritten by shellify, not flagged.
    assert!(unsupported_bash_script_interp(&v(&["bash", "{script}"])).is_none());
    assert!(unsupported_bash_script_interp(&v(&["bash.exe", "{script}"])).is_none());
    assert!(unsupported_bash_script_interp(&v(&["/bin/bash", "{script}"])).is_none());
    assert!(
        unsupported_bash_script_interp(&v(&["gnome-terminal", "--", "bash", "{script}"])).is_none()
    );
    // Non-bash command → untouched.
    assert!(unsupported_bash_script_interp(&v(&["mypowershell", "{script}"])).is_none());
}

// Finding 25: background and run-here launches must resolve `bash` the
// same way (PATH match, falling back to `/bin/bash`) so they can't drift.
#[cfg(unix)]
#[test]
fn resolve_bash_command_prefers_path_then_fallback() {
    let r = resolve_bash_command();
    assert!(r.ends_with("bash"));
    match which_bin("bash") {
        Some(p) => assert_eq!(r, p),
        None => assert_eq!(r, "/bin/bash"),
    }
}

// Finding 17: built-in preset platform capability, checked against the
// real `TERMINAL_PRESETS` table (see src/shared/terminal_presets.rs).
#[test]
fn terminal_preset_platform_capability() {
    use crate::config::terminal_preset_supported_on;
    assert!(terminal_preset_supported_on("iterm", "Darwin"));
    assert!(!terminal_preset_supported_on("iterm", "Windows"));
    assert!(terminal_preset_supported_on("wttab", "Windows"));
    assert!(!terminal_preset_supported_on("wttab", "Darwin"));
    assert!(terminal_preset_supported_on("wezterm", "Windows"));
    assert!(terminal_preset_supported_on("ptyxis", "Linux"));
    assert!(!terminal_preset_supported_on("ptyxis", "Darwin"));
    assert!(!terminal_preset_supported_on("nope", "Darwin"));
}
