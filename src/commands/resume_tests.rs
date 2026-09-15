use super::*;
use crate::db::HcomDb;

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|i| i.to_string()).collect()
}

fn test_db() -> HcomDb {
    crate::config::Config::init();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    std::mem::forget(dir);
    db
}

#[test]
fn test_parse_resume_argv() {
    let (name, extra) = parse_resume_argv(&s(&["r", "luna"]), "r").unwrap();
    assert_eq!(name, "luna");
    assert!(extra.is_empty());
}

#[test]
fn test_parse_resume_argv_with_extra() {
    let (name, extra) = parse_resume_argv(&s(&["r", "luna", "--model", "opus"]), "r").unwrap();
    assert_eq!(name, "luna");
    assert_eq!(extra, s(&["--model", "opus"]));
}

#[test]
fn test_parse_resume_argv_empty_fails() {
    assert!(parse_resume_argv(&s(&["r"]), "r").is_err());
}

#[test]
fn test_build_resume_args_claude() {
    let args = build_resume_args("claude", "sess-123", false);
    assert_eq!(args, s(&["--resume", "sess-123"]));
}

#[test]
fn test_build_resume_args_claude_fork() {
    let args = build_resume_args("claude", "sess-123", true);
    assert_eq!(args, s(&["--resume", "sess-123", "--fork-session"]));
}

#[test]
fn test_recover_cursor_cwd_from_workspace_trusted() {
    let dir = tempfile::tempdir().unwrap();
    // Mirror ~/.cursor/projects/<slug>/{agent-transcripts/<uuid>/<uuid>.jsonl,.workspace-trusted}
    let slug = dir.path().join("projects").join("Users-anno-Dev-x");
    let tdir = slug.join("agent-transcripts").join("uuid-1");
    std::fs::create_dir_all(&tdir).unwrap();
    std::fs::write(
        slug.join(".workspace-trusted"),
        json!({"workspacePath": "/Users/anno/Dev/x", "trustMethod": null}).to_string(),
    )
    .unwrap();
    let transcript = tdir.join("uuid-1.jsonl");
    std::fs::write(&transcript, "{}").unwrap();

    assert_eq!(
        recover_cursor_cwd(&transcript.to_string_lossy()),
        Some("/Users/anno/Dev/x".to_string())
    );
    // No marker → None (graceful: caller falls back to $PWD).
    std::fs::remove_file(slug.join(".workspace-trusted")).unwrap();
    assert_eq!(recover_cursor_cwd(&transcript.to_string_lossy()), None);
}

#[test]
fn test_build_resume_args_codex_resume() {
    let args = build_resume_args("codex", "sess-456", false);
    assert_eq!(args, s(&["resume", "sess-456"]));
}

#[test]
fn test_build_resume_args_codex_fork() {
    let args = build_resume_args("codex", "sess-456", true);
    assert_eq!(args, s(&["fork", "sess-456"]));
}

#[test]
fn test_build_resume_args_gemini() {
    let args = build_resume_args("gemini", "sess-789", false);
    assert_eq!(args, s(&["--resume", "sess-789"]));
}

#[test]
fn test_build_resume_args_omp_resume() {
    let args = build_resume_args("omp", "sess-omp", false);
    assert_eq!(args, s(&["--resume", "sess-omp"]));
}

#[test]
fn test_build_resume_args_omp_fork() {
    // Top-level `omp --help` omits `--fork`, but v15.1.9+ implements it:
    // `omp --fork <id>` takes the id as its value and routes through
    // SessionManager.forkFrom(...). Same shape as Pi — fork must emit
    // `["--fork", <id>]`, replacing `--resume`, not `["--resume", <id>,
    // "--fork"]`. hcom must not degrade `hcom f` into a plain `--resume`.
    let args = build_resume_args("omp", "sess-omp", true);
    assert_eq!(args, s(&["--fork", "sess-omp"]));
}

#[test]
fn test_merge_omp_args_fork_replaces_prior_session_controls() {
    // A fork's build_resume_args output (`--fork <id>`) merged over stored
    // launch args must strip the old resume/session/fork controls and keep
    // config flags.
    let fork = s(&["--fork", "new"]);
    assert_eq!(
        merge_omp_args(&s(&["--model", "opus", "--fork", "old"]), &fork),
        s(&["--fork", "new", "--model", "opus"])
    );
    assert_eq!(
        merge_omp_args(&s(&["--fork=old", "-r", "prev"]), &fork),
        s(&["--fork", "new"])
    );
}

#[test]
fn test_merge_omp_args_strips_resume_aliases_and_values() {
    let resume = s(&["--resume", "new"]);
    assert_eq!(
        merge_omp_args(&s(&["--model", "opus", "-r", "old"]), &resume),
        s(&["--resume", "new", "--model", "opus"])
    );
    assert_eq!(
        merge_omp_args(&s(&["--resume=old", "--thinking", "high"]), &resume),
        s(&["--resume", "new", "--thinking", "high"])
    );
    assert_eq!(
        merge_omp_args(
            &s(&["--continue", "-c", "--session-dir", "/tmp/s"]),
            &resume
        ),
        s(&["--resume", "new"])
    );
}

#[test]
fn test_merge_omp_args_does_not_drop_following_flags() {
    let resume = s(&["--resume", "new"]);
    assert_eq!(
        merge_omp_args(&s(&["--session-dir", "--model", "opus"]), &resume),
        s(&["--resume", "new", "--model", "opus"])
    );
}

#[test]
#[serial_test::serial]
fn test_derive_omp_transcript_path_checks_xdg_data_home() {
    if !cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "android"
    )) {
        return;
    }
    let _guard = crate::hooks::test_helpers::EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("omp").join("sessions").join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("session-xyz.jsonl"), "{}").unwrap();
    unsafe {
        std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::remove_var("OMP_PROFILE");
        std::env::remove_var("PI_PROFILE");
        std::env::set_var("XDG_DATA_HOME", dir.path());
    }

    let path = derive_omp_transcript_path("session-xyz").unwrap();
    assert!(
        path.contains("session-xyz.jsonl"),
        "unexpected path: {path}"
    );
}

// Unix-only: relies on redirecting the home dir via `isolated_test_env`'s
// $HOME, but on Windows `dirs::home_dir()` queries the OS profile folder
// directly and ignores it.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn test_find_session_on_disk_prefers_omp_for_omp_paths() {
    let (_dir, _hcom, home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    unsafe {
        std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("OMP_PROFILE");
        std::env::remove_var("PI_PROFILE");
    }
    let root = home
        .join(".omp")
        .join("agent")
        .join("sessions")
        .join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("session-omp.jsonl"), "{}").unwrap();

    let found = find_session_on_disk("session-omp").unwrap();
    assert_eq!(found.0, "omp");
}

#[test]
#[serial_test::serial]
fn test_find_session_on_disk_attributes_pi_session_dir_to_pi() {
    // Regression guard for the shared-root bug: a Pi session reached via the
    // Pi-exclusive PI_CODING_AGENT_SESSION_DIR override must be attributed to
    // Pi, not stolen by OMP (which never reads that variable).
    let (_dir, _hcom, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let pi_sessions = tempfile::tempdir().unwrap();
    let root = pi_sessions.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("session-pi.jsonl"), "{}").unwrap();
    unsafe {
        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("HCOM_TOOL");
        std::env::set_var("PI_CODING_AGENT_SESSION_DIR", pi_sessions.path());
    }

    // OMP must not find it at all.
    assert!(derive_omp_transcript_path("session-pi").is_none());
    let found = find_session_on_disk("session-pi").unwrap();
    assert_eq!(found.0, "pi");
}

#[test]
#[serial_test::serial]
fn test_find_session_on_disk_attributes_shared_agent_dir_by_path_marker() {
    // The genuinely-shared PI_CODING_AGENT_DIR: attribution must key on the
    // path's product marker (.pi vs .omp), not on which tool's root list or
    // probe order found it. hcom isolates managed configs as <root>/.pi and
    // <root>/.omp, so the marker is present.
    let (_dir, _hcom, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let base = tempfile::tempdir().unwrap();
    unsafe {
        std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("OMP_PROFILE");
        std::env::remove_var("PI_PROFILE");
        std::env::remove_var("HCOM_TOOL");
    }

    // Managed Pi: PI_CODING_AGENT_DIR=<base>/.pi -> sessions/<file>.
    let pi_dir = base.path().join(".pi");
    let pi_root = pi_dir.join("sessions").join("proj");
    std::fs::create_dir_all(&pi_root).unwrap();
    std::fs::write(pi_root.join("mgpi.jsonl"), "{}").unwrap();
    unsafe { std::env::set_var("PI_CODING_AGENT_DIR", &pi_dir) }
    assert_eq!(find_session_on_disk("mgpi").unwrap().0, "pi");

    // Managed OMP: PI_CODING_AGENT_DIR=<base>/.omp -> sessions/<file>.
    let omp_dir = base.path().join(".omp");
    let omp_root = omp_dir.join("sessions").join("proj");
    std::fs::create_dir_all(&omp_root).unwrap();
    std::fs::write(omp_root.join("mgomp.jsonl"), "{}").unwrap();
    unsafe { std::env::set_var("PI_CODING_AGENT_DIR", &omp_dir) }
    assert_eq!(find_session_on_disk("mgomp").unwrap().0, "omp");
}

#[test]
#[serial_test::serial]
fn test_markerless_shared_agent_dir_is_ambiguous_not_guessed() {
    // Arbitrary shared PI_CODING_AGENT_DIR with no .pi/.omp marker must be
    // reported ambiguous, never silently attributed — even when the caller
    // itself is Pi or OMP.
    let (_dir, _hcom, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let shared = tempfile::tempdir().unwrap();
    let root = shared.path().join("sessions").join("proj");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("amb.jsonl"), "{}").unwrap();
    unsafe {
        std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("OMP_PROFILE");
        std::env::remove_var("PI_PROFILE");
        std::env::remove_var("HCOM_TOOL");
        std::env::set_var("PI_CODING_AGENT_DIR", shared.path());
    }

    // No determinate attribution -> find_session_on_disk yields None, and the
    // match is flagged ambiguous (found-but-unattributable).
    assert!(find_session_on_disk("amb").is_none());
    assert!(ambiguous_pi_omp_session("amb").is_some());

    for caller in ["pi", "omp"] {
        unsafe { std::env::set_var("HCOM_TOOL", caller) }
        assert!(
            find_session_on_disk("amb").is_none(),
            "caller {caller} must not claim a markerless shared transcript"
        );
        assert!(ambiguous_pi_omp_session("amb").is_some());
    }
}

// Unix-only: relies on redirecting the home dir via `isolated_test_env`'s
// $HOME, but on Windows `dirs::home_dir()` queries the OS profile folder
// directly and ignores it.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn test_derive_omp_transcript_path_checks_named_profile() {
    let (_dir, _hcom, home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    unsafe {
        std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("PI_PROFILE");
        std::env::set_var("OMP_PROFILE", "work");
    }
    let root = home
        .join(".omp")
        .join("profiles")
        .join("work")
        .join("agent")
        .join("sessions")
        .join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("session-prof.jsonl"), "{}").unwrap();

    let path = derive_omp_transcript_path("session-prof").unwrap();
    assert!(path.contains("session-prof.jsonl"), "unexpected: {path}");
}

#[test]
fn test_validate_resume_operation_rejects_gemini_fork() {
    let err = validate_resume_operation("gemini", true)
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Gemini does not support session forking (hcom f)");
}

#[test]
fn test_validate_resume_operation_allows_gemini_resume() {
    assert!(validate_resume_operation("gemini", false).is_ok());
}

#[test]
fn test_validate_resume_operation_rejects_unknown_tool() {
    let err = validate_resume_operation("future-tool", false)
        .unwrap_err()
        .to_string();
    assert_eq!(err, "Unknown tool 'future-tool' in saved session metadata");
}

#[test]
fn test_build_resume_args_antigravity_resume() {
    let args = build_resume_args("antigravity", "conv-abc", false);
    assert_eq!(args, s(&["--conversation", "conv-abc"]));
}

#[test]
fn test_validate_resume_operation_rejects_antigravity_fork() {
    let err = validate_resume_operation("antigravity", true)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Antigravity") && err.contains("fork"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_validate_resume_operation_allows_antigravity_resume() {
    assert!(validate_resume_operation("antigravity", false).is_ok());
}

#[test]
fn test_validate_resume_operation_rejects_agy_alias_fork() {
    // The alias is launcher-canonicalised today, but fork validation must
    // still reject `"agy"` so the rule lives on the spec, not on the DB
    // shape.
    let err = validate_resume_operation("agy", true)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Antigravity") && err.contains("fork"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_merge_resume_args_antigravity_strips_session_and_prompt_flags() {
    // --conversation (value-consuming), --continue/-c (bare), --prompt-interactive (value), -p (bare)
    let merged = merge_resume_args(
        "antigravity",
        &s(&[
            "--conversation",
            "old-conv",
            "--sandbox",
            "--continue",
            "--prompt-interactive",
            "old prompt",
            "-p",
        ]),
        &s(&["--conversation", "new-conv"]),
    );
    assert_eq!(merged, s(&["--conversation", "new-conv", "--sandbox"]));
}

#[test]
fn test_merge_resume_args_antigravity_preserves_sandbox_and_add_dir() {
    let merged = merge_resume_args(
        "antigravity",
        &s(&["--sandbox", "--add-dir", "/some/path"]),
        &s(&["--conversation", "conv-xyz"]),
    );
    assert_eq!(
        merged,
        s(&[
            "--conversation",
            "conv-xyz",
            "--sandbox",
            "--add-dir",
            "/some/path"
        ])
    );
}

/// agy accepts `--flag=value` (Go flag convention). Stale `--conversation=old`
/// in the original launch_args must not survive to conflict with the new --conversation.
#[test]
fn test_merge_resume_args_antigravity_strips_equals_form() {
    let merged = merge_resume_args(
        "antigravity",
        &s(&[
            "--conversation=old-id",
            "--sandbox",
            "--prompt-interactive=old prompt",
            "--prompt=alt",
        ]),
        &s(&["--conversation", "new-id"]),
    );
    assert_eq!(merged, s(&["--conversation", "new-id", "--sandbox"]));
}

/// agy / Go flag also accepts single-dash long form (`-conversation=old`).
#[test]
fn test_merge_resume_args_antigravity_strips_single_dash_long_form() {
    let merged = merge_resume_args(
        "antigravity",
        &s(&[
            "-conversation",
            "old-id",
            "-prompt-interactive=stale",
            "--sandbox",
            "-c",
        ]),
        &s(&["--conversation", "new-id"]),
    );
    assert_eq!(merged, s(&["--conversation", "new-id", "--sandbox"]));
}

/// cursor launch_args bake in HCOM_CURSOR_ARGS config plus a trailing
/// positional prompt. Resume must preserve config flags, drop the stale
/// prompt + old --resume, and prepend the new resume args.
#[test]
fn test_merge_resume_args_cursor_preserves_config_drops_prompt_and_session() {
    let merged = merge_resume_args(
        "cursor",
        &s(&[
            "--model",
            "composer-2.5",
            "--force",
            "--resume",
            "old-chat",
            "fix the parser bug",
        ]),
        &s(&["--resume", "new-chat-id"]),
    );
    assert_eq!(
        merged,
        s(&[
            "--resume",
            "new-chat-id",
            "--model",
            "composer-2.5",
            "--force"
        ])
    );
}

/// cwd/worktree selectors are owned by the launcher (snapshot dir or --dir),
/// so a stale --workspace / -w must be stripped and not fight the recovered
/// cwd. --continue and the prompt are also dropped.
#[test]
fn test_merge_resume_args_cursor_strips_workspace_worktree_continue() {
    let merged = merge_resume_args(
        "cursor",
        &s(&[
            "--workspace",
            "/old/path",
            "--continue",
            "-w",
            "feature-x",
            "--sandbox",
            "enabled",
            "do the task",
        ]),
        &s(&["--resume", "sid"]),
    );
    assert_eq!(merged, s(&["--resume", "sid", "--sandbox", "enabled"]));
}

/// `--flag=value` form and the repeatable `-H` header value flag must be
/// preserved intact; the stale `--resume=old` equals-form is stripped.
#[test]
fn test_merge_resume_args_cursor_equals_form_and_header() {
    let merged = merge_resume_args(
        "cursor",
        &s(&[
            "--model=sonnet-4",
            "--resume=old",
            "-H",
            "X-Trace: 1",
            "summarize",
        ]),
        &s(&["--resume", "sid"]),
    );
    assert_eq!(
        merged,
        s(&["--resume", "sid", "--model=sonnet-4", "-H", "X-Trace: 1"])
    );
}

/// Precedence: a resume-time `--model x` must beat the baked `--model y`.
/// The baked singular copy is dropped (resume wins), while repeatable
/// flags from both sides concatenate.
#[test]
fn test_merge_resume_args_cursor_resume_value_beats_baked() {
    let merged = merge_resume_args(
        "cursor",
        &s(&["--model", "y", "-H", "X-Baked: 1", "--force", "stale task"]),
        &s(&["--resume", "sid", "--model", "x", "-H", "X-Resume: 1"]),
    );
    assert_eq!(
        merged,
        s(&[
            "--resume",
            "sid",
            "--model",
            "x",
            "-H",
            "X-Resume: 1",
            // baked --model y dropped (resume wins); -H kept (repeatable);
            // --force preserved.
            "-H",
            "X-Baked: 1",
            "--force",
        ])
    );
}

/// A baked `--print`/`-p`/`--stream-partial-output` stays visible so the
/// launcher can reject it clearly instead of silently changing semantics.
#[test]
fn test_merge_resume_args_cursor_preserves_print_flags_for_validation() {
    let merged = merge_resume_args(
        "cursor",
        &s(&[
            "-p",
            "--print",
            "--stream-partial-output",
            "--model",
            "composer-2.5",
        ]),
        &s(&["--resume", "sid"]),
    );
    assert_eq!(
        merged,
        s(&[
            "--resume",
            "sid",
            "-p",
            "--print",
            "--stream-partial-output",
            "--model",
            "composer-2.5"
        ])
    );
}

#[test]
fn test_resume_inactive_agy_row_is_resumable() {
    let db = test_db();
    let mut data = serde_json::Map::new();
    data.insert("session_id".into(), json!("agy-session-001"));
    data.insert("tool".into(), json!("antigravity"));
    // Soft-finalized: instance row exists but status=inactive
    data.insert("status".into(), json!(ST_INACTIVE));
    data.insert("created_at".into(), json!(1.0));
    db.save_instance_named("zeno", &data).unwrap();

    // Emit a stopped life event so load_stopped_snapshot can find the snapshot
    let snapshot = serde_json::json!({
        "action": "stopped",
        "snapshot": {
            "tool": "antigravity",
            "session_id": "agy-session-001",
            "launch_args": "[]",
            "tag": "",
            "background": 0,
            "last_event_id": 0,
            "directory": "/tmp"
        }
    });
    db.conn()
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES (?, 'life', 'zeno', ?)",
            rusqlite::params!["2026-01-01T00:00:00Z", snapshot.to_string()],
        )
        .unwrap();

    // Should NOT bail — inactive agy row is resumable.
    let result = prepare_resume_plan(&db, "zeno", false, &[], &GlobalFlags::default());
    assert!(
        result.is_ok(),
        "expected inactive agy row to be resumable, got: {:?}",
        result.err()
    );
}

#[test]
fn test_merge_resume_args_opencode_preserves_non_session_flags() {
    let merged = merge_resume_args(
        "opencode",
        &s(&[
            "--model",
            "openai/gpt-5.4",
            "--session",
            "old-sess",
            "--prompt",
            "old prompt",
            "--approval-mode",
            "on-request",
        ]),
        &s(&["--session", "new-sess", "--fork"]),
    );
    assert_eq!(
        merged,
        s(&[
            "--session",
            "new-sess",
            "--fork",
            "--model",
            "openai/gpt-5.4",
            "--approval-mode",
            "on-request",
        ])
    );
}

#[test]
fn test_merge_resume_args_opencode_strips_equals_form_session_and_prompt() {
    let merged = merge_resume_args(
        "opencode",
        &s(&[
            "--session=old-sess",
            "--prompt=old prompt",
            "--model",
            "anthropic/claude-sonnet-4-6",
        ]),
        &s(&["--session", "new-sess"]),
    );
    assert_eq!(
        merged,
        s(&[
            "--session",
            "new-sess",
            "--model",
            "anthropic/claude-sonnet-4-6",
        ])
    );
}

#[test]
fn test_build_resume_args_opencode_fork() {
    let args = build_resume_args("opencode", "sess-000", true);
    assert_eq!(args, s(&["--session", "sess-000", "--fork"]));
}

#[test]
fn test_build_resume_args_kilo_fork() {
    let args = build_resume_args("kilo", "sess-000", true);
    assert_eq!(args, s(&["--session", "sess-000", "--fork"]));
}

#[test]
fn test_build_resume_args_pi_fork() {
    // Pi's `--fork <id>` takes the id as its value and is mutually exclusive
    // with `--session` (pi errors "--fork cannot be combined with
    // --session"), so fork must emit `["--fork", <id>]`, not
    // `["--session", <id>, "--fork"]`.
    let args = build_resume_args("pi", "sess-000", true);
    assert_eq!(args, s(&["--fork", "sess-000"]));
}

#[test]
fn test_merge_resume_args_pi_strips_session_controls_and_positional_prompt() {
    let merged = merge_resume_args(
        "pi",
        &s(&[
            "--model",
            "claude-3-5-sonnet",
            "--session-id",
            "old-sess",
            "--session-dir",
            "/tmp/old",
            "--continue",
            "old prompt",
        ]),
        &s(&["--session", "new-sess"]),
    );
    assert_eq!(
        merged,
        s(&["--session", "new-sess", "--model", "claude-3-5-sonnet"])
    );
}

#[test]
fn test_merge_resume_args_kilo_preserves_non_session_flags() {
    let merged = merge_resume_args(
        "kilo",
        &s(&["--model", "kilo/kilo-auto/free", "--prompt", "old prompt"]),
        &s(&["--session", "new-sess"]),
    );
    assert_eq!(
        merged,
        s(&["--session", "new-sess", "--model", "kilo/kilo-auto/free"])
    );
}

#[test]
fn test_extract_resume_flags_terminal() {
    let (dir, flags, remaining) =
        extract_resume_flags(&s(&["--terminal", "alacritty", "--model", "opus"]));
    assert_eq!(dir, None);
    assert_eq!(flags.terminal, Some("alacritty".to_string()));
    assert_eq!(remaining, s(&["--model", "opus"]));
}

#[test]
fn test_extract_resume_flags_tag_and_terminal() {
    let (dir, flags, remaining) =
        extract_resume_flags(&s(&["--tag", "test", "--terminal", "kitty"]));
    assert_eq!(dir, None);
    assert_eq!(flags.tag, Some("test".to_string()));
    assert_eq!(flags.terminal, Some("kitty".to_string()));
    assert!(remaining.is_empty());
}

#[test]
fn test_extract_resume_flags_equals_form() {
    let (dir, flags, remaining) = extract_resume_flags(&s(&["--tag=test", "--terminal=alacritty"]));
    assert_eq!(dir, None);
    assert_eq!(flags.tag, Some("test".to_string()));
    assert_eq!(flags.terminal, Some("alacritty".to_string()));
    assert!(remaining.is_empty());
}

#[test]
fn test_extract_resume_flags_none() {
    let (dir, flags, remaining) = extract_resume_flags(&s(&["--model", "opus"]));
    assert_eq!(dir, None);
    assert_eq!(flags.tag, None);
    assert_eq!(flags.terminal, None);
    assert_eq!(remaining, s(&["--model", "opus"]));
}

#[test]
fn test_extract_resume_flags_dir() {
    let (dir, flags, remaining) =
        extract_resume_flags(&s(&["--dir", "/tmp/test", "--model", "opus"]));
    assert_eq!(dir, Some("/tmp/test".to_string()));
    assert_eq!(flags.tag, None);
    assert_eq!(flags.terminal, None);
    assert_eq!(remaining, s(&["--model", "opus"]));
}

#[test]
fn test_extract_resume_flags_shared_launch_flags() {
    let (dir, flags, remaining) = extract_resume_flags(&s(&[
        "--dir=/tmp/test",
        "--headless",
        "--batch-id",
        "batch-1",
        "--run-here",
        "--hcom-prompt",
        "hi",
        "--hcom-system-prompt",
        "sys",
        "--model",
        "opus",
    ]));
    assert_eq!(dir, Some("/tmp/test".to_string()));
    assert!(flags.headless);
    assert_eq!(flags.batch_id, Some("batch-1".to_string()));
    assert_eq!(flags.run_here, Some(true));
    assert_eq!(flags.initial_prompt, Some("hi".to_string()));
    assert_eq!(flags.system_prompt, Some("sys".to_string()));
    assert_eq!(remaining, s(&["--model", "opus"]));
}

#[test]
fn test_extract_resume_flags_stops_at_double_dash() {
    let (dir, flags, remaining) = extract_resume_flags(&s(&[
        "--dir",
        "/tmp/test",
        "--",
        "--dir",
        "tool-dir",
        "--model",
        "opus",
    ]));
    assert_eq!(dir, Some("/tmp/test".to_string()));
    assert_eq!(flags, crate::commands::launch::HcomLaunchFlags::default());
    assert_eq!(remaining, s(&["--dir", "tool-dir", "--model", "opus"]));
}

#[test]
fn test_should_preview_resume_false_for_plain_resume() {
    assert!(!should_preview_resume(
        &crate::commands::launch::HcomLaunchFlags::default(),
        &[]
    ));
}

#[test]
fn test_should_preview_resume_true_for_tool_args() {
    assert!(should_preview_resume(
        &crate::commands::launch::HcomLaunchFlags::default(),
        &s(&["--model", "opus"])
    ));
}

#[test]
fn test_should_preview_resume_rpc_true_for_hcom_flags() {
    assert!(should_preview_resume_rpc(&s(&["--terminal", "kitty"])));
}

#[test]
fn test_should_preview_resume_true_for_hcom_only_flags() {
    let flags = crate::commands::launch::HcomLaunchFlags {
        terminal: Some("kitty".to_string()),
        ..Default::default()
    };
    assert!(should_preview_resume(&flags, &[]));
}

#[test]
fn test_tracked_fork_plan_does_not_reserve_until_execution() {
    let db = test_db();
    let mut data = serde_json::Map::new();
    data.insert("session_id".into(), json!("session-123"));
    data.insert("tool".into(), json!("codex"));
    data.insert("status".into(), json!("listening"));
    data.insert("created_at".into(), json!(1.0));
    db.save_instance_named("luna", &data).unwrap();

    let before_count = db.iter_instances_full().unwrap().len();
    let plan = prepare_resume_plan(&db, "luna", true, &[], &GlobalFlags::default()).unwrap();
    let preview_name = plan
        .launch
        .name
        .as_ref()
        .expect("tracked fork should have preview name")
        .clone();

    assert_eq!(db.iter_instances_full().unwrap().len(), before_count);
    assert!(db.get_instance_full(&preview_name).unwrap().is_none());
    assert!(plan.tracked_fork_identity.is_some());

    let launch = prepare_launch_for_execution(&db, &plan).unwrap();
    let reserved_name = launch.name.as_ref().expect("reserved name");
    assert!(db.get_instance_full(reserved_name).unwrap().is_some());
    assert_eq!(db.iter_instances_full().unwrap().len(), before_count + 1);
    assert!(
        launch
            .initial_prompt
            .as_deref()
            .unwrap_or("")
            .contains(&format!("Your hcom name is {reserved_name}."))
    );
}

#[test]
fn test_resume_inherits_prior_session_id_fork_does_not() {
    let db = test_db();
    let mut data = serde_json::Map::new();
    data.insert("session_id".into(), json!("session-123"));
    data.insert("tool".into(), json!("codex"));
    data.insert("status".into(), json!(ST_INACTIVE));
    data.insert("created_at".into(), json!(1.0));
    db.save_instance_named("luna", &data).unwrap();

    // Inactive rows resolve via the stopped-snapshot life event.
    let snapshot = serde_json::json!({
        "action": "stopped",
        "snapshot": {
            "tool": "codex",
            "session_id": "session-123",
            "launch_args": "[]",
            "tag": "",
            "background": 0,
            "last_event_id": 0,
            "directory": "/tmp"
        }
    });
    db.conn()
        .execute(
            "INSERT INTO events (timestamp, type, instance, data) VALUES (?, 'life', 'luna', ?)",
            rusqlite::params!["2026-01-01T00:00:00Z", snapshot.to_string()],
        )
        .unwrap();

    let resume = prepare_resume_plan(&db, "luna", false, &[], &GlobalFlags::default()).unwrap();
    assert_eq!(
        resume.launch.prior_session_id.as_deref(),
        Some("session-123"),
        "plain resume must pre-seed the recreated row with the prior session id \
             so a kill before the first turn (no hook re-bind) stays resumable"
    );

    let fork = prepare_resume_plan(&db, "luna", true, &[], &GlobalFlags::default()).unwrap();
    assert_eq!(
        fork.launch.prior_session_id, None,
        "forks bind a fresh session on first turn; must not inherit the parent's"
    );
}

#[test]
fn test_resume_system_prompt_codex_fork_does_not_tell_agent_to_rebind() {
    let prompt = resume_system_prompt("codex", "luna", true, None);
    assert!(prompt.contains("already-assigned hcom identity"));
    assert!(!prompt.contains("Run hcom start"));
}

#[test]
fn test_resume_system_prompt_non_codex_fork_states_new_identity() {
    let prompt = resume_system_prompt("claude", "luna", true, Some("feri"));
    assert!(prompt.contains("feri"), "should name the new identity");
    assert!(
        prompt.contains("--name feri"),
        "should state the --name flag"
    );
    assert!(
        !prompt.contains("Run hcom start"),
        "should not tell agent to rebind"
    );
    assert!(
        !prompt.contains("You are still 'luna'"),
        "should not resume as parent"
    );

    // None path falls back to already-assigned wording (no child name available)
    let prompt_none = resume_system_prompt("claude", "luna", true, None);
    assert!(prompt_none.contains("already-assigned hcom identity"));
    assert!(!prompt_none.contains("Run hcom start"));
}

#[test]
fn test_build_remote_resume_output_uses_actual_launch_result_background() {
    let db = test_db();
    let output = build_remote_resume_output(
        &db,
        &LaunchResult {
            tool: "claude".to_string(),
            batch_id: "batch-1".to_string(),
            launched: 1,
            failed: 0,
            background: true,
            log_files: Vec::new(),
            handles: Vec::new(),
            errors: Vec::new(),
        },
        &s(&["--terminal", "kitty", "--tag", "ops", "--run-here"]),
        false,
        &GlobalFlags::default(),
    );

    assert_eq!(output.action, "resume");
    assert_eq!(output.tool, "claude");
    assert_eq!(output.tag.as_deref(), Some("ops"));
    assert_eq!(output.terminal.as_deref(), Some("kitty"));
    assert!(output.background);
    assert_eq!(output.run_here, Some(true));
}

#[test]
fn test_build_remote_resume_output_marks_fork_action() {
    let db = test_db();
    let output = build_remote_resume_output(
        &db,
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
        &[],
        true,
        &GlobalFlags::default(),
    );

    assert_eq!(output.action, "fork");
    assert_eq!(output.tool, "codex");
    assert!(!output.background);
}

#[test]
fn test_resolve_one_match_zero_or_one() {
    assert_eq!(resolve_one_match("Claude", "x", vec![]).unwrap(), None);
    let single = vec![ThreadMatch {
        session_id: "abc".to_string(),
        when: "t1".to_string(),
    }];
    assert_eq!(
        resolve_one_match("Claude", "x", single).unwrap(),
        Some("abc".to_string())
    );
}

#[test]
fn test_resolve_one_match_ambiguous_bails() {
    let multi = vec![
        ThreadMatch {
            session_id: "sid-a".to_string(),
            when: "2026-01-01T00:00:00Z".to_string(),
        },
        ThreadMatch {
            session_id: "sid-b".to_string(),
            when: "2026-02-01T00:00:00Z".to_string(),
        },
    ];
    let err = resolve_one_match("Claude", "dup", multi)
        .unwrap_err()
        .to_string();
    assert!(err.contains("matches 2 Claude sessions"), "got: {err}");
    assert!(err.contains("sid-a") && err.contains("sid-b"), "got: {err}");
    assert!(err.contains("UUID directly"), "got: {err}");
}

/// Point claude_config_dir() at `dir` for the duration of `f` by setting
/// CLAUDE_CONFIG_DIR. Restored on exit. serial_test required.
fn with_claude_config_dir<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let prev = std::env::var("CLAUDE_CONFIG_DIR").ok();
    // SAFETY: tests using this must be serial_test::serial — only one
    // test at a time touches this env var.
    unsafe {
        std::env::set_var("CLAUDE_CONFIG_DIR", dir);
    }
    let out = f();
    match prev {
        Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
    }
    out
}

#[test]
#[serial_test::serial]
fn test_resolve_claude_thread_name_prefers_last_custom_title() {
    // A session renamed A → B → C must match for C (the current title),
    // not A or B (obsolete).
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg = cfg_dir.path();
    let projects = cfg.join("projects/proj");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join("s1.jsonl"),
        r#"{"type":"custom-title","customTitle":"A","sessionId":"s1"}
{"type":"custom-title","customTitle":"B","sessionId":"s1"}
{"type":"custom-title","customTitle":"C","sessionId":"s1"}
"#,
    )
    .unwrap();

    let (old, mid, cur) = with_claude_config_dir(cfg, || {
        (
            resolve_claude_thread_name("A").unwrap(),
            resolve_claude_thread_name("B").unwrap(),
            resolve_claude_thread_name("C").unwrap(),
        )
    });

    assert_eq!(old, None, "obsolete title A must not resolve");
    assert_eq!(mid, None, "obsolete title B must not resolve");
    assert_eq!(cur, Some("s1".to_string()), "current title C must resolve");
}

#[test]
#[serial_test::serial]
fn test_resolve_claude_thread_name_bails_on_within_tool_duplicate() {
    // Two distinct sessions both currently have customTitle="dup" — must
    // bail rather than silently pick by mtime.
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg = cfg_dir.path();
    let projects = cfg.join("projects/proj");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join("s1.jsonl"),
        r#"{"type":"custom-title","customTitle":"dup","sessionId":"sess-aaaa"}
"#,
    )
    .unwrap();
    std::fs::write(
        projects.join("s2.jsonl"),
        r#"{"type":"custom-title","customTitle":"dup","sessionId":"sess-bbbb"}
"#,
    )
    .unwrap();

    let res = with_claude_config_dir(cfg, || resolve_claude_thread_name("dup"));

    let err = res.unwrap_err().to_string();
    assert!(err.contains("matches 2 Claude sessions"), "got: {err}");
    assert!(
        err.contains("sess-aaaa") && err.contains("sess-bbbb"),
        "got: {err}"
    );
}

#[test]
fn test_run_local_resume_result_routes_uuid_to_adoption() {
    // Remote-RPC entrypoint must walk the UUID/thread-name resolution
    // chain. A UUID with no on-disk transcript should error with the
    // adoption "Session not found" message (proving we hit find_session_on_disk),
    // not the name-based "No stopped snapshot found" message.
    let db = test_db();
    let err = run_local_resume_result(
        &db,
        "12345678-1234-5678-1234-567812345678",
        false,
        &[],
        &GlobalFlags::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("Session 12345678-1234-5678-1234-567812345678 not found"),
        "expected adoption error, got: {err}"
    );
}

#[test]
fn test_run_local_resume_result_routes_opencode_uuid_to_adoption() {
    // Sanity: opencode-style IDs also route through adoption, with the
    // opencode-specific error.
    let db = test_db();
    let err = run_local_resume_result(
        &db,
        "ses_nonexistentfakesession12345",
        false,
        &[],
        &GlobalFlags::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("Session ses_nonexistentfakesession12345 not found"),
        "expected adoption error, got: {err}"
    );
    assert!(
        err.contains("Opencode"),
        "error should mention opencode: {err}"
    );
}

#[test]
fn test_is_session_id_valid_uuid() {
    assert!(is_session_id("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
    assert!(is_session_id("521cfc2b-be38-403a-b32e-4a49c9551b27"));
}

#[test]
fn test_is_session_id_valid_opencode() {
    // opencode IDs are `ses_` + ULID-ish suffix (see opencode/src/id/id.ts)
    assert!(is_session_id("ses_019b12abcdefGHIJK0123456789"));
    assert!(is_session_id("ses_abcdef"));
}

#[test]
fn test_is_session_id_rejects_names() {
    assert!(!is_session_id("cafe"));
    assert!(!is_session_id("boho"));
    assert!(!is_session_id("my-agent"));
    assert!(!is_session_id("impl-luna"));
    assert!(!is_session_id("review-kira"));
    assert!(!is_session_id(""));
    assert!(!is_session_id("ses_")); // prefix alone, no suffix
    assert!(!is_session_id("ses_with-dash")); // opencode IDs are alnum only
}

#[test]
fn test_extract_cwd_claude() {
    // Claude records cwd in the first (and every) line. Read one line.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"user","cwd":"/start/dir","message":"hi"}
{"type":"assistant","cwd":"/start/dir","message":"hello"}
"#,
    )
    .unwrap();
    let result = extract_cwd_from_transcript(path.to_str().unwrap(), "claude");
    assert_eq!(result, Some("/start/dir".to_string()));
}

#[test]
fn test_extract_cwd_claude_skips_permission_mode_header() {
    // Real Claude transcripts start with a `permission-mode` line that has
    // no `cwd`; cwd first appears on a later entry. Must scan forward.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"permission-mode","permissionMode":"default","sessionId":"abc"}
{"type":"snapshot","messageId":"m1"}
{"parentUuid":null,"type":"user","cwd":"/real/cwd","message":"hi"}
"#,
    )
    .unwrap();
    let result = extract_cwd_from_transcript(path.to_str().unwrap(), "claude");
    assert_eq!(result, Some("/real/cwd".to_string()));
}

#[test]
fn test_extract_cwd_claude_gives_up_after_cap() {
    // If no cwd appears in the first 20 lines, return None rather than
    // reading the full transcript.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    let mut content = String::new();
    for _ in 0..25 {
        content.push_str(r#"{"type":"noise"}"#);
        content.push('\n');
    }
    content.push_str(r#"{"type":"user","cwd":"/late/cwd"}"#);
    content.push('\n');
    std::fs::write(&path, content).unwrap();
    let result = extract_cwd_from_transcript(path.to_str().unwrap(), "claude");
    assert_eq!(result, None);
}

#[test]
fn test_extract_cwd_codex() {
    // Codex records cwd in the session_meta payload on line 1.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"session_meta","payload":{"cwd":"/start/dir"}}
{"type":"event_msg","payload":{"content":"hello"}}
"#,
    )
    .unwrap();
    let result = extract_cwd_from_transcript(path.to_str().unwrap(), "codex");
    assert_eq!(result, Some("/start/dir".to_string()));
}

#[test]
#[serial_test::serial]
fn test_extract_cwd_gemini_reverse_hash_lookup() {
    // Gemini writes sha256(cwd) as `projectHash` in the session JSON, and
    // stores cwd → short-id in ~/.gemini/projects.json. This test wires up
    // a fake GEMINI_CLI_HOME and confirms the reverse lookup.
    use sha2::{Digest, Sha256};
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();
    let gemini = base.join(".gemini");
    let session_dir = gemini.join("tmp/myproj/chats");
    std::fs::create_dir_all(&session_dir).unwrap();

    let fake_cwd = "/some/fake/cwd";
    let hex = Sha256::digest(fake_cwd.as_bytes())
        .iter()
        .fold(String::new(), |mut a, b| {
            use std::fmt::Write;
            let _ = write!(a, "{:02x}", b);
            a
        });

    // projects.json: cwd → short-id
    std::fs::write(
        gemini.join("projects.json"),
        format!(r#"{{"projects":{{"{fake_cwd}":"myproj"}}}}"#),
    )
    .unwrap();

    // Session JSON: projectHash = sha256(cwd)
    let session_path = session_dir.join("session-x.json");
    std::fs::write(&session_path, format!(r#"{{"projectHash":"{hex}"}}"#)).unwrap();

    // Stub GEMINI_CLI_HOME so recover_gemini_cwd reads from our fake tree.
    let prev = std::env::var("GEMINI_CLI_HOME").ok();
    // SAFETY: test is single-threaded enough for this module; serial_test
    // isn't in scope here, but other tests don't touch GEMINI_CLI_HOME.
    unsafe {
        std::env::set_var("GEMINI_CLI_HOME", base);
    }
    let result = extract_cwd_from_transcript(session_path.to_str().unwrap(), "gemini");
    match prev {
        Some(v) => unsafe { std::env::set_var("GEMINI_CLI_HOME", v) },
        None => unsafe { std::env::remove_var("GEMINI_CLI_HOME") },
    }

    assert_eq!(result, Some(fake_cwd.to_string()));
}

#[test]
fn test_build_resume_args_copilot() {
    let args = build_resume_args("copilot", "sess-abc", false);
    assert_eq!(args, s(&["--resume", "sess-abc"]));
}

#[test]
fn test_build_resume_args_copilot_fork_rejected() {
    // copilot has fork: None, so build_resume_args returns resume-only args
    let args = build_resume_args("copilot", "sess-abc", true);
    assert_eq!(args, s(&["--resume", "sess-abc"]));
}

#[test]
fn test_merge_copilot_args_preserves_model_drops_prompt() {
    let original = s(&["--model", "claude-haiku-4.5", "-i", "do a task"]);
    let resume = s(&["--resume", "sess-abc"]);
    let merged = merge_resume_args("copilot", &original, &resume);
    assert!(merged.contains(&"--resume".to_string()));
    assert!(merged.contains(&"sess-abc".to_string()));
    assert!(merged.contains(&"--model".to_string()));
    assert!(merged.contains(&"claude-haiku-4.5".to_string()));
    // Original -i prompt must be dropped
    assert!(!merged.contains(&"-i".to_string()));
    assert!(!merged.contains(&"do a task".to_string()));
}

#[test]
fn test_merge_copilot_args_resume_model_wins() {
    let original = s(&["--model", "claude-haiku-4.5", "-i", "task"]);
    let resume = s(&["--resume", "sess-abc", "--model", "claude-sonnet-4-5"]);
    let merged = merge_resume_args("copilot", &original, &resume);
    // Only one --model entry
    assert_eq!(merged.iter().filter(|t| t.as_str() == "--model").count(), 1);
    assert!(merged.contains(&"claude-sonnet-4-5".to_string()));
    assert!(!merged.contains(&"claude-haiku-4.5".to_string()));
}

#[test]
fn test_extract_cwd_copilot_from_session_start() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        concat!(
            r#"{"type":"session.start","data":{"cwd":"/home/user/myproject","sessionId":"abc"}}"#,
            "\n",
            r#"{"type":"user.message","data":{"text":"hello"}}"#,
            "\n"
        ),
    )
    .unwrap();
    let cwd = extract_cwd_from_transcript(file.path().to_str().unwrap(), "copilot");
    assert_eq!(cwd, Some("/home/user/myproject".to_string()));
}

#[test]
#[serial_test::serial]
fn test_extract_cwd_gemini_no_registry_returns_none() {
    // When projects.json is missing, we can't reverse the hash → return None.
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();
    let gemini = base.join(".gemini/tmp/x/chats");
    std::fs::create_dir_all(&gemini).unwrap();
    let path = gemini.join("test.json");
    std::fs::write(&path, r#"{"projectHash":"deadbeef"}"#).unwrap();
    let prev = std::env::var("GEMINI_CLI_HOME").ok();
    unsafe {
        std::env::set_var("GEMINI_CLI_HOME", base);
    }
    let result = extract_cwd_from_transcript(path.to_str().unwrap(), "gemini");
    match prev {
        Some(v) => unsafe { std::env::set_var("GEMINI_CLI_HOME", v) },
        None => unsafe { std::env::remove_var("GEMINI_CLI_HOME") },
    }
    assert_eq!(result, None);
}
