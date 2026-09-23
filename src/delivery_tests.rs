use super::*;

/// Helper: create DeliveryState with given screen state
fn make_state(screen: ScreenState, cooldown_ms: u64) -> DeliveryState {
    DeliveryState {
        screen: Arc::new(std::sync::RwLock::new(screen)),
        launch_phase_active: Arc::new(AtomicBool::new(true)),
        inject_port: 0,
        user_activity_cooldown_ms: cooldown_ms,
    }
}

/// Helper: screen state where everything is safe for injection
fn safe_screen() -> ScreenState {
    ScreenState {
        ready: true,
        approval: false,
        prompt_empty: true,
        input_text: None,
        visible_tail: None,
        last_user_input: Instant::now() - Duration::from_secs(10),
        last_output: Instant::now() - Duration::from_secs(10),
        cols: 80,
        last_prompt_submit: None,
        approval_scrape_latched: false,
        nav_overlay: false,
    }
}

#[test]
fn status_refresh_repairs_codex_approval_cache_divergence() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances
                 (name, tool, status, status_context, status_time, created_at)
                 VALUES ('halo', 'codex', 'active', 'tool:Bash', 0, 0)",
            [],
        )
        .unwrap();

    let shared_status = Arc::new(std::sync::RwLock::new(ST_BLOCKED.to_string()));
    let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wake_count_for_callback = wake_count.clone();
    let title_wake: TitleWake = Arc::new(move || {
        wake_count_for_callback.fetch_add(1, Ordering::Relaxed);
    });
    // Codex approval detection updates the PTY-owned shared status directly.
    // The delivery loop's private cache can therefore still say active when
    // the approval clears and the database returns to active.
    let mut current_status = ST_ACTIVE.to_string();

    refresh_status_and_wake(
        &db,
        "halo",
        &mut current_status,
        &Some(shared_status.clone()),
        &Some(title_wake.clone()),
    );

    assert_eq!(current_status, ST_ACTIVE);
    assert_eq!(*shared_status.read().unwrap(), ST_ACTIVE);
    assert_eq!(wake_count.load(Ordering::Relaxed), 1);

    // A context/detail-only status event does not change the title icon and
    // must not create redundant proxy wakeups.
    refresh_status_and_wake(
        &db,
        "halo",
        &mut current_status,
        &Some(shared_status),
        &Some(title_wake),
    );
    assert_eq!(wake_count.load(Ordering::Relaxed), 1);
}

#[test]
fn pty_cleanup_does_not_log_stop_after_instance_already_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, status, status_context, status_time, created_at)
                 VALUES ('buli', 'pi', 'active', 'running', 0, 0)",
            [],
        )
        .unwrap();

    let snapshot = db.get_instance_snapshot("buli").unwrap();
    db.log_life_event("buli", "stopped", "samu", "killed", snapshot)
        .unwrap();
    db.delete_instance("buli").unwrap();

    cleanup_deleted_instance(&mut db, "buli");

    let events: Vec<(String, String)> = db
        .conn()
        .prepare(
            "SELECT json_extract(data, '$.by'), json_extract(data, '$.reason')
                 FROM events
                 WHERE type = 'life'
                   AND instance = 'buli'
                   AND json_extract(data, '$.action') = 'stopped'
                 ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();

    assert_eq!(events, vec![("samu".to_string(), "killed".to_string())]);
}

#[test]
fn soft_stopped_instance_survives_pty_exit_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let mut db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances (name, tool, status, status_context, status_time, created_at, session_id)
                 VALUES ('luna', 'omp', 'inactive', 'exit:turn_end', 0, 0, 'sid-soft')",
                [],
            )
            .unwrap();
    db.set_process_binding("pid-soft", "sid-soft", "luna")
        .unwrap();

    antigravity::cleanup_antigravity_pty_exit(&mut db, "luna", "pid-soft", true);

    assert!(db.get_instance_full("luna").unwrap().is_some());
    assert_eq!(
        db.get_status("luna").unwrap().map(|(s, _)| s),
        Some(ST_INACTIVE.to_string())
    );
}

// ---- phase-1 ownership tests ----

#[test]
fn phase1_timeout_is_ten_seconds() {
    assert_eq!(PHASE1_TIMEOUT, Duration::from_secs(10));
}

#[test]
fn phase1_complete_render_wins_at_deadline() {
    assert_eq!(
        phase1_decision(
            Some("<hcom>"),
            "<hcom>",
            PHASE1_TIMEOUT + Duration::from_millis(1),
        ),
        Phase1Decision::Rendered,
    );
}

#[test]
fn phase1_rejects_user_text_after_injected_text() {
    assert_eq!(
        phase1_decision(Some("<hcom> user draft"), "<hcom>", Duration::ZERO),
        Phase1Decision::MixedPrompt,
    );
}

#[test]
fn phase1_rejects_user_text_before_injected_text() {
    assert_eq!(
        phase1_decision(Some("user draft <hcom>"), "<hcom>", Duration::ZERO),
        Phase1Decision::MixedPrompt,
    );
}

#[test]
fn phase1_rejects_mixed_prompt_after_activity_cooldown() {
    assert_eq!(
        phase1_decision(
            Some("<hcom> user draft"),
            "<hcom>",
            Duration::from_millis(501),
        ),
        Phase1Decision::MixedPrompt,
    );
}

#[test]
fn claude_fast_fails_after_first_unacknowledged_wake() {
    assert_eq!(
        verify_timeout_decision(Some(Tool::Claude), true, 1),
        VerifyTimeoutDecision::FastFail
    );
}

#[test]
fn claude_accepts_consumed_queue_without_cursor_advance() {
    assert_eq!(
        verify_timeout_decision(Some(Tool::Claude), false, 1),
        VerifyTimeoutDecision::DeliveredWithoutCursor
    );
}

#[test]
fn non_claude_keeps_existing_verify_retry_contract() {
    assert_eq!(
        verify_timeout_decision(Some(Tool::Codex), true, 1),
        VerifyTimeoutDecision::Retry
    );
    assert_eq!(
        verify_timeout_decision(Some(Tool::Codex), true, 3),
        VerifyTimeoutDecision::Reset
    );
}

#[test]
fn phase1_unrelated_text_times_out_normally() {
    assert_eq!(
        phase1_decision(
            Some("user draft"),
            "<hcom>",
            PHASE1_TIMEOUT + Duration::from_millis(1),
        ),
        Phase1Decision::TimedOut,
    );
}

#[test]
fn submit_authority_requires_exact_prompt_ownership() {
    assert_eq!(
        prompt_ownership(Some("<hcom>"), "<hcom>"),
        PromptOwnership::Exclusive,
    );
    assert_eq!(
        prompt_ownership(Some("<hcom> user draft"), "<hcom>"),
        PromptOwnership::Mixed,
    );
    assert_eq!(
        prompt_ownership(Some("user draft"), "<hcom>"),
        PromptOwnership::Other,
    );
}

// ---- evaluate_gate tests ----

#[test]
fn gate_all_conditions_pass() {
    let config = ToolConfig::claude();
    let state = make_state(safe_screen(), 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(result.safe);
    assert_eq!(result.reason, "ok");
}

#[test]
fn gate_blocks_when_not_idle() {
    let config = ToolConfig::claude();
    let state = make_state(safe_screen(), 500);
    let result = evaluate_gate(&config, &state, false);
    assert!(!result.safe);
    assert_eq!(result.reason, "not_idle");
}

#[test]
fn gate_blocks_on_approval() {
    let config = ToolConfig::claude();
    let mut screen = safe_screen();
    screen.approval = true;
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(!result.safe);
    assert_eq!(result.reason, "approval");
}

#[test]
fn antigravity_config_allows_ready_footer_with_placeholder_text() {
    let config = ToolConfig::antigravity();
    assert!(config.require_ready_prompt);
    assert!(config.require_prompt_empty);
    assert!(!config.block_on_user_activity);
}

#[test]
fn gate_antigravity_blocks_prompt_text() {
    let config = ToolConfig::antigravity();
    let mut screen = safe_screen();
    screen.prompt_empty = false;
    screen.input_text = Some("uncommitted".to_string());
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(!result.safe);
    assert_eq!(result.reason, "prompt_has_text");
}

#[test]
fn gate_blocks_on_user_activity() {
    let config = ToolConfig::claude();
    let mut screen = safe_screen();
    screen.last_user_input = Instant::now(); // just typed
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(!result.safe);
    assert_eq!(result.reason, "user_active");
}

#[test]
fn gate_blocks_while_nav_overlay_open() {
    // A Claude nav overlay (subagent view or session switcher) is focused:
    // the box-emptiness checks would otherwise scrape the overlay's (empty)
    // input box and pass, landing the wake trigger in the wrong box.
    let config = ToolConfig::claude();
    let mut screen = safe_screen(); // ready + prompt_empty: would pass otherwise
    screen.nav_overlay = true;
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(!result.safe);
    assert_eq!(result.reason, "nav_overlay");
}

#[test]
fn gate_blocks_during_submit_settle_window() {
    let config = ToolConfig::codex();
    let mut screen = safe_screen();
    screen.last_prompt_submit = Some(Instant::now());
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(
        !result.safe,
        "gate must block during submit-settle window to prevent racing hook delivery"
    );
    assert_eq!(result.reason, "submit_settle");
}

#[test]
fn gate_passes_after_submit_settle_expires() {
    let config = ToolConfig::codex();
    let mut screen = safe_screen();
    screen.last_prompt_submit =
        Some(Instant::now() - Duration::from_millis(SUBMIT_SETTLE_COOLDOWN_MS + 100));
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(result.safe);
    assert_eq!(result.reason, "ok");
}

#[test]
fn gate_skips_submit_settle_when_idle_not_required() {
    // OpenCode bootstrap path runs with require_idle=false. The hook-vs-PTY
    // race that submit_settle guards against can't happen there, so the
    // cooldown shouldn't apply.
    let config = ToolConfig::opencode();
    let mut screen = safe_screen();
    screen.last_prompt_submit = Some(Instant::now());
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(result.safe);
}

#[test]
fn gate_blocks_when_not_ready_for_gemini() {
    let config = ToolConfig::gemini();
    let mut screen = safe_screen();
    screen.ready = false;
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(!result.safe);
    assert_eq!(result.reason, "not_ready");
}

#[test]
fn gate_claude_skips_ready_check() {
    // Claude has require_ready_prompt=false
    let config = ToolConfig::claude();
    let mut screen = safe_screen();
    screen.ready = false;
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(result.safe);
}

#[test]
fn gate_blocks_on_prompt_text_for_claude() {
    let config = ToolConfig::claude();
    let mut screen = safe_screen();
    screen.prompt_empty = false;
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(!result.safe);
    assert_eq!(result.reason, "prompt_has_text");
}

fn open_ready_test_db() -> (tempfile::TempDir, HcomDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_raw(&dir.path().join("test.db")).unwrap();
    db.init_db().unwrap();
    (dir, db)
}

#[test]
fn launch_ready_observed_follows_tool_gate_shape() {
    let (_dir, db) = open_ready_test_db();
    let n = "toli";
    let mut screen = safe_screen();
    screen.ready = false;
    screen.prompt_empty = true;

    let state = make_state(screen.clone(), 500);
    assert!(launch_ready_observed(&db, n, &ToolConfig::codex(), &state));
    assert!(launch_ready_observed(&db, n, &ToolConfig::claude(), &state));
    assert!(!launch_ready_observed(
        &db,
        n,
        &ToolConfig::opencode(),
        &state
    ));
    assert!(!launch_ready_observed(
        &db,
        n,
        &ToolConfig::cursor(),
        &state
    ));

    let state = make_state(screen.clone(), 500);
    assert!(!launch_ready_observed(
        &db,
        n,
        &ToolConfig::gemini(),
        &state
    ));

    screen.ready = true;
    let state = make_state(screen.clone(), 500);
    assert!(launch_ready_observed(
        &db,
        n,
        &ToolConfig::opencode(),
        &state
    ));
    assert!(launch_ready_observed(&db, n, &ToolConfig::cursor(), &state));

    screen.prompt_empty = false;
    let state = make_state(screen, 500);
    assert!(!launch_ready_observed(&db, n, &ToolConfig::codex(), &state));
    assert!(!launch_ready_observed(
        &db,
        n,
        &ToolConfig::cursor(),
        &state
    ));
}

#[test]
fn omp_launch_ready_requires_plugin_bind_not_screen() {
    // OMP readiness is bind-driven: a rendered/ready screen must NOT be
    // enough, and a kind='plugin' notify endpoint must flip it ready even
    // with no on-screen marker.
    let (_dir, db) = open_ready_test_db();
    let config = ToolConfig::for_tool(crate::tool::Tool::Omp);
    assert!(config.launch_ready_on_plugin_bind);

    let mut screen = safe_screen();
    screen.ready = true; // empty pattern => is_ready() always true
    screen.prompt_empty = true;
    let state = make_state(screen, 500);

    // No plugin endpoint yet -> not ready despite the "ready" screen.
    assert!(!launch_ready_observed(&db, "vupo", &config, &state));

    // A pty endpoint (registered at launch, before the extension binds) must
    // not count as readiness.
    db.upsert_notify_endpoint("vupo", "pty", 4001).unwrap();
    assert!(!launch_ready_observed(&db, "vupo", &config, &state));

    // The extension bind is the authoritative signal.
    db.upsert_notify_endpoint("vupo", "plugin", 4002).unwrap();
    assert!(launch_ready_observed(&db, "vupo", &config, &state));
}

#[test]
fn copilot_session_binding_satisfies_launch_readiness() {
    let (_dir, db) = open_ready_test_db();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, session_id, created_at)
                 VALUES ('mira', 'copilot', 'copilot-session-1', 0)",
            [],
        )
        .unwrap();
    let mut screen = safe_screen();
    screen.ready = false;
    screen.prompt_empty = false;
    let state = make_state(screen, 500);

    assert!(launch_ready_observed(
        &db,
        "mira",
        &ToolConfig::for_tool(crate::tool::Tool::Copilot),
        &state
    ));
}

#[test]
fn gate_gemini_skips_prompt_empty_check() {
    // Gemini has require_prompt_empty=false
    let config = ToolConfig::gemini();
    let mut screen = safe_screen();
    screen.prompt_empty = false;
    let state = make_state(screen, 500);
    let result = evaluate_gate(&config, &state, true);
    assert!(result.safe);
}

#[test]
fn gate_fail_fast_order() {
    // When multiple gates fail, first one wins
    let config = ToolConfig::gemini();
    let mut screen = safe_screen();
    screen.approval = true;
    screen.ready = false;
    let state = make_state(screen, 500);
    // not idle + approval + not ready → not_idle wins
    let result = evaluate_gate(&config, &state, false);
    assert_eq!(result.reason, "not_idle");
}

// ---- Screen-scraped approval latch ----

#[test]
fn latch_holds_through_transient_false_scrape() {
    // A positive scrape latches true regardless of prior state.
    assert!(latch_scraped_approval(false, true, false));
    assert!(latch_scraped_approval(false, true, true));
    // Latched true survives a transient false scrape while output is still
    // churning (a partial-render frame, not a real dismissal).
    assert!(latch_scraped_approval(true, false, false));
    // Once output settles and the scrape is still false, the prompt has
    // genuinely left the screen -> clear.
    assert!(!latch_scraped_approval(true, false, true));
    // Never spuriously latches from a clean idle state.
    assert!(!latch_scraped_approval(false, false, false));
    assert!(!latch_scraped_approval(false, false, true));
}

// ---- Lookup functions ----

#[test]
fn gate_block_detail_known_reasons() {
    assert_eq!(gate_block_detail("not_idle"), "waiting for idle status");
    assert_eq!(gate_block_detail("approval"), "waiting for user approval");
    assert_eq!(
        gate_block_detail("submit_settle"),
        "waiting for prompt submit to settle"
    );
    assert_eq!(
        gate_block_detail("nav_overlay"),
        "waiting for subagent nav / session switcher to close"
    );
    assert_eq!(gate_block_detail("unknown"), "blocked");
}

// ---- ToolConfig ----

#[test]
fn tool_config_for_adhoc_uses_adhoc_identity_and_gates() {
    let config = ToolConfig::for_tool(crate::tool::Tool::Adhoc);
    let gates = &crate::tool::Tool::Adhoc.spec().gates;
    assert_eq!(config.tool, "adhoc");
    assert_eq!(config.require_idle, gates.require_idle);
    assert_eq!(config.require_ready_prompt, gates.require_ready_prompt);
    assert_eq!(config.require_prompt_empty, gates.require_prompt_empty);
    assert_eq!(config.block_on_user_activity, gates.block_on_user_activity);
    assert_eq!(config.block_on_approval, gates.block_on_approval);
    assert_eq!(config.launch_requires_ready, gates.launch_requires_ready);
}

#[test]
fn tool_configs_match_expected_differences() {
    let claude = ToolConfig::claude();
    let gemini = ToolConfig::gemini();
    let codex = ToolConfig::codex();

    // Claude: no ready_prompt, yes prompt_empty
    assert!(!claude.require_ready_prompt);
    assert!(claude.require_prompt_empty);

    // Gemini: yes ready_prompt, no prompt_empty
    assert!(gemini.require_ready_prompt);
    assert!(!gemini.require_prompt_empty);

    // Codex: same as Claude (ready pattern unreliable in narrow terminals)
    assert!(!codex.require_ready_prompt);
    assert!(codex.require_prompt_empty);

    // All require idle
    assert!(claude.require_idle);
    assert!(gemini.require_idle);
    assert!(codex.require_idle);

    // Copilot: footer-gated ready prompt + empty-prompt + approval gating.
    let copilot = ToolConfig::copilot();
    assert!(copilot.require_idle);
    assert!(copilot.require_ready_prompt);
    assert!(copilot.require_prompt_empty);
    assert!(copilot.block_on_user_activity);
    assert!(copilot.block_on_approval);
}

#[test]
fn wake_inject_includes_prompt_safe_metadata_only() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("hcom.db");
    let db = HcomDb::open_at(&db_path).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at, last_event_id)
                 VALUES ('keno', 'listening', '', 1.0, 0)",
            [],
        )
        .unwrap();
    let data = serde_json::json!({
        "from": "life",
        "text": "ping. Always reply to @life, not @bigboss.",
        "scope": "mentions",
        "mentions": ["keno"],
        "intent": "request",
        "thread": "hcom-routing-test",
    });
    db.conn()
        .execute(
            "INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-05-25T12:00:00Z', 'keno', ?1)",
            rusqlite::params![data.to_string()],
        )
        .unwrap();

    let text = build_wake_inject_text(&db, "keno", 120);
    assert!(text.starts_with("<hcom>"), "text={text}");
    assert!(text.ends_with("</hcom>"), "text={text}");
    assert!(text.contains("life"), "text={text}");
    assert!(text.contains("request"), "text={text}");
    assert!(!text.contains('@'));
    assert!(!text.contains("Always reply"));
}

#[test]
fn wake_inject_falls_back_to_minimal_trigger_when_preview_would_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("hcom.db");
    let db = HcomDb::open_at(&db_path).unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, status, status_context, created_at, last_event_id)
                 VALUES ('keno', 'listening', '', 1.0, 0)",
            [],
        )
        .unwrap();
    let data = serde_json::json!({
        "from": "life",
        "text": "short",
        "scope": "mentions",
        "mentions": ["keno"],
        "intent": "request",
        "thread": "a-thread-name-that-is-too-wide-for-the-input",
    });
    db.conn()
        .execute(
            "INSERT INTO events (type, timestamp, instance, data)
                 VALUES ('message', '2026-05-25T12:00:00Z', 'keno', ?1)",
            rusqlite::params![data.to_string()],
        )
        .unwrap();

    assert_eq!(build_wake_inject_text(&db, "keno", 24), "<hcom>");
}

#[test]
fn delivery_block_escalates_only_past_the_threshold() {
    use std::time::Duration;
    let threshold = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS);
    assert!(!should_escalate_block(Duration::from_secs(0), threshold));
    assert!(!should_escalate_block(
        threshold - Duration::from_millis(1),
        threshold
    ));
    assert!(should_escalate_block(threshold, threshold));
}

#[test]
fn delivery_block_context_is_stable_across_polls() {
    use std::time::Duration;
    let below = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS - 1);
    let at = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS);
    let later = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS + 30);

    assert_eq!(
        gate_block_context("prompt_has_text", below),
        "tui:prompt-has-text"
    );
    assert_eq!(
        gate_block_context("prompt_has_text", at),
        "tui:prompt-has-text:stalled"
    );
    // The bug this test exists for: the 2s updater recomputes the context every
    // poll and writes whenever it differs from the last one written. If it
    // rebuilt the unsuffixed string after the escalation, the stalled marker
    // would vanish on the very next poll and never come back — escalation
    // fires once. Same input, same string, at any elapsed time past the threshold.
    assert_eq!(
        gate_block_context("prompt_has_text", later),
        gate_block_context("prompt_has_text", at)
    );
    // A gate whose reason changes gets a new context and is written again.
    assert_ne!(
        gate_block_context("not_idle", at),
        gate_block_context("prompt_has_text", at)
    );
}

#[test]
fn delivery_block_event_retries_failed_write_and_emits_once_per_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_raw(&dir.path().join("test.db")).unwrap();
    db.init_db().unwrap();
    let mut clock = BlockClock::start();
    clock.since -= Duration::from_secs(63);

    // Force a real SQLite write failure without changing the fixture's rows.
    db.conn().execute_batch("PRAGMA query_only = ON").unwrap();
    assert!(
        clock
            .emit_escalation(&db, "nova", ST_ACTIVE, "not_idle")
            .is_err()
    );
    assert!(!clock.escalated);
    assert!(
        db.get_events_since(0, Some("life"), Some("nova"))
            .unwrap()
            .is_empty()
    );

    db.conn().execute_batch("PRAGMA query_only = OFF").unwrap();
    assert!(
        clock
            .emit_escalation(&db, "nova", ST_ACTIVE, "not_idle")
            .unwrap()
    );
    assert!(
        !clock
            .emit_escalation(&db, "nova", ST_ACTIVE, "not_idle")
            .unwrap()
    );
    let events = db.get_events_since(0, Some("life"), Some("nova")).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["data"]["action"], "delivery_blocked");
    assert_eq!(events[0]["data"]["status"], ST_ACTIVE);
    let blocked_secs: u64 = events[0]["data"]["detail"]
        .as_str()
        .unwrap()
        .strip_prefix("gate blocked ")
        .unwrap()
        .strip_suffix("s continuously")
        .unwrap()
        .parse()
        .unwrap();
    assert!(blocked_secs >= 63);

    // A later block can emit its own event.
    assert!(
        BlockClock::start()
            .emit_escalation(&db, "nova", ST_ACTIVE, "not_idle")
            .unwrap()
    );
    assert_eq!(
        db.get_events_since(0, Some("life"), Some("nova"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn delivery_block_context_failed_write_preserves_cleanup_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_raw(&dir.path().join("test.db")).unwrap();
    db.init_db().unwrap();
    db.conn().execute(
            "INSERT INTO instances (name, tool, created_at, status) VALUES ('nova', 'antigravity', 1, 'listening')",
            [],
        ).unwrap();
    let old = "tui:prompt-has-text:stalled";
    let new = "tui:not-ready:stalled";
    let mut marker = String::new();

    for retry_write in [false, true] {
        update_gate_context(&db, "nova", old, "old detail", &mut marker).unwrap();
        db.conn().execute_batch("PRAGMA query_only = ON").unwrap();
        assert!(update_gate_context(&db, "nova", new, "new detail", &mut marker).is_err());
        assert_eq!(marker, old);
        assert_eq!(db.get_status("nova").unwrap().unwrap().1, old);
        db.conn().execute_batch("PRAGMA query_only = OFF").unwrap();

        if retry_write {
            update_gate_context(&db, "nova", new, "new detail", &mut marker).unwrap();
            assert_eq!(marker, new);
            assert_eq!(db.get_status("nova").unwrap().unwrap().1, new);
            assert_eq!(
                db.get_instance_status("nova").unwrap().unwrap().detail,
                "new detail"
            );
        }
        // The queue may drain before the replacement succeeds. Either way,
        // cleanup must still name and remove the context actually on disk.
        release_gate_context(&db, "nova", &mut marker);
        assert!(marker.is_empty());
        assert_eq!(db.get_status("nova").unwrap().unwrap().1, "");
        assert_eq!(db.get_instance_status("nova").unwrap().unwrap().detail, "");
    }
}

#[test]
fn a_failed_gate_clear_is_retried_and_still_respects_ownership() {
    // The db::tests helpers are `pub(super)` — visible inside `db`, not here.
    // This is the fixture `delivery.rs` already uses (see
    // `status_refresh_repairs_codex_approval_cache_divergence`).
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
            .execute(
                "INSERT INTO instances (name, tool, created_at, status, status_context) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["nova", "antigravity", 1.0f64, ST_LISTENING, "start"],
            )
            .unwrap();
    db.set_gate_status("nova", "tui:not-idle:stalled", "gate blocked 60s")
        .unwrap();
    let mut marker = "tui:not-idle:stalled".to_string();

    // The clear fails. The marker must survive, or nothing knows the row is
    // still dirty — this is the leak the Idle retry exists for.
    db.conn()
        .execute("ALTER TABLE instances RENAME TO instances_hidden", [])
        .unwrap();
    release_gate_context(&db, "nova", &mut marker);
    assert_eq!(
        marker, "tui:not-idle:stalled",
        "a failed clear keeps the marker"
    );

    // Next idle iteration: same call, and now it lands.
    db.conn()
        .execute("ALTER TABLE instances_hidden RENAME TO instances", [])
        .unwrap();
    release_gate_context(&db, "nova", &mut marker);
    assert!(marker.is_empty(), "the retry drops the marker once cleared");
    let (_, context) = db.get_status("nova").unwrap().unwrap();
    assert_eq!(context, "");

    // Ownership still holds on the retry path: a hook took the row while the
    // marker was being carried, so the retry must leave it alone.
    db.set_gate_status("nova", "tui:not-idle:stalled", "gate blocked 60s")
        .unwrap();
    let mut stale = "tui:not-idle:stalled".to_string();
    db.set_status("nova", ST_ACTIVE, "tool:Bash").unwrap();
    release_gate_context(&db, "nova", &mut stale);
    assert!(stale.is_empty(), "the row is a hook's now; we own nothing");
    let (status, context) = db.get_status("nova").unwrap().unwrap();
    assert_eq!(status, ST_ACTIVE);
    assert_eq!(context, "tool:Bash");

    drop(db); // tempdir cleans up behind it
}

#[test]
fn gate_publication_does_not_clobber_a_hook_transition() {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_at(&dir.path().join("test.db")).unwrap();
    db.conn().execute("INSERT INTO instances (name, tool, created_at, status, status_context, status_detail) VALUES ('nova', 'claude', 1, 'active', 'tool:Bash', 'running tests')", []).unwrap();
    let mut marker = String::new();
    update_gate_context(
        &db,
        "nova",
        "tui:wake-unacknowledged",
        "not ready",
        &mut marker,
    )
    .unwrap();
    assert!(
        marker.is_empty(),
        "a skipped write must not claim ownership"
    );
    assert_eq!(db.get_status("nova").unwrap().unwrap().1, "tool:Bash");
    assert_eq!(
        db.get_instance_status("nova").unwrap().unwrap().detail,
        "running tests"
    );
    db.set_status("nova", ST_LISTENING, "stop").unwrap();
    update_gate_context(
        &db,
        "nova",
        "tui:wake-unacknowledged",
        "not ready",
        &mut marker,
    )
    .unwrap();
    assert_eq!(marker, "tui:wake-unacknowledged");
}

#[test]
fn durable_gate_feedback_is_immediate_and_transient_gates_are_debounced() {
    for reason in [
        "not_ready",
        "prompt_has_text",
        "user_active",
        "approval",
        "nav_overlay",
        "wake_unacknowledged",
    ] {
        assert_eq!(
            gate_status_publication_delay(reason),
            Duration::ZERO,
            "{reason}"
        );
    }
    for reason in ["not_idle", "output_unstable", "cooldown", "unknown"] {
        assert_eq!(
            gate_status_publication_delay(reason),
            Duration::from_secs(2),
            "{reason}"
        );
    }
    assert_eq!(
        gate_block_context("prompt_has_text", Duration::from_secs(60)),
        "tui:prompt-has-text:stalled"
    );
}

#[test]
fn gate_rebind_clears_the_old_owner_and_retries_failed_clear() {
    let dir = tempfile::tempdir().unwrap();
    let db = HcomDb::open_at(&dir.path().join("test.db")).unwrap();
    for name in ["old", "new"] {
        db.conn()
            .execute(
                "INSERT INTO instances (name, created_at, status) VALUES (?1, 1, 'listening')",
                [name],
            )
            .unwrap();
        db.set_gate_status(name, "tui:not-ready", "not ready")
            .unwrap();
    }
    let mut owner = "old".to_string();
    let mut marker = "tui:not-ready".to_string();
    db.conn().execute_batch("PRAGMA query_only=ON").unwrap();
    assert!(!reconcile_gate_owner(&db, "new", &mut owner, &mut marker));
    assert_eq!(owner, "old");
    assert_eq!(marker, "tui:not-ready");
    db.conn().execute_batch("PRAGMA query_only=OFF").unwrap();
    assert!(reconcile_gate_owner(&db, "new", &mut owner, &mut marker));
    assert_eq!(owner, "new");
    assert!(marker.is_empty());
    assert_eq!(db.get_status("old").unwrap().unwrap().1, "");
    assert_eq!(
        db.get_status("new").unwrap().unwrap().1,
        "tui:not-ready",
        "the new owner's equal context was written by someone else"
    );
}
