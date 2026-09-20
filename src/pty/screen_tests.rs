use super::*;

/// Helper: create tracker without debug/config dependencies
fn make_tracker(rows: u16, cols: u16, ready_pattern: &str) -> ScreenTracker {
    ScreenTracker {
        parser: vt100::Parser::new(rows, cols, 0),
        rows,
        cols,
        ready_pattern: ready_pattern.to_string(),
        waiting_approval: false,
        last_child_title: None,
        last_output: Instant::now(),
        last_change: Instant::now(),
        output_buffer: Vec::new(),
        debug_enabled: false,
        debug_file: None,
        debug_counter: 0,
        debug_last_dump: Instant::now(),
        debug_last_flag_check: Instant::now(),
        debug_flag_path: std::path::PathBuf::new(),
        instance_name: None,
    }
}

#[test]
fn sanitize_child_title_strips_controls_and_collapses_whitespace() {
    assert_eq!(
        sanitize_child_title("  Working\t\non   task  "),
        "Working on task"
    );
    // Embedded ESC / BEL (the only bytes that could break out of our OSC)
    // are dropped; the harmless leftover text stays.
    assert_eq!(sanitize_child_title("a\x1b]2;evil\x07b"), "a]2;evilb");
    assert_eq!(sanitize_child_title(""), "");
}

#[test]
fn sanitize_child_title_bounds_length() {
    let long = "x".repeat(MAX_CHILD_TITLE_CHARS + 50);
    assert_eq!(
        sanitize_child_title(&long).chars().count(),
        MAX_CHILD_TITLE_CHARS
    );
}

#[test]
fn process_captures_child_osc_title() {
    let mut t = make_tracker(24, 80, "");
    assert_eq!(t.child_title(), None);
    t.process(b"before\x1b]0;\xe2\xa0\x8b Working\x07after");
    assert_eq!(t.child_title(), Some("⠋ Working"));
}

#[test]
fn child_title_keeps_last_complete_through_eviction() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"\x1b]0;First title\x07");
    assert_eq!(t.child_title(), Some("First title"));
    // Flood past the 4KB rolling buffer with plain output containing no
    // complete title; the last good title must survive rather than clear.
    t.process(&vec![b'.'; 8192]);
    assert_eq!(t.child_title(), Some("First title"));
}

// ---- vt100 panic containment (issue #73) ----

#[test]
fn process_survives_vt100_wide_char_resize_panic() {
    // A double-width (wide) character whose continuation cell gets
    // truncated by a downward resize used to panic inside vt100
    // (upstream doy/vt100-rust#28: `Row::clear_wide` indexes one past
    // the row's new length) the next time that cell was erased. That
    // panic used to unwind straight through `process`/`resize` and kill
    // the PTY wrapper (hcom issue #73, observed as repeated
    // `stopped by pty: closed` on real Codex sessions). It must now be
    // contained: the tracker rebuilds its parser and stays usable.
    let mut t = make_tracker(3, 10, "");

    // Wide CJK char printed so it spans the last two columns (8, 9).
    t.process(b"\x1b[1;9H");
    t.process("\u{4e2d}".as_bytes());

    // Shrink to 9 columns: the continuation cell (old col 9) is
    // truncated away, orphaning the wide flag on the new last column.
    t.resize(3, 9);

    // Erase-in-line on that orphaned wide cell is what panicked upstream.
    t.process(b"\x1b[1;9H\x1b[K");

    // Tracker must have survived and still be fully usable.
    assert_eq!(t.cols(), 9);
    t.process(b"still alive\r\n");
}

// ---- is_ready ----

#[test]
fn is_ready_when_pattern_visible() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process(b"Some output\r\n? for shortcuts\r\n");
    assert!(t.is_ready());
}

#[test]
fn is_ready_false_when_pattern_absent() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process(b"Some output\r\nno pattern here\r\n");
    assert!(!t.is_ready());
}

#[test]
fn is_ready_true_when_no_pattern_configured() {
    let t = make_tracker(24, 80, "");
    assert!(t.is_ready());
}

// ---- Codex approval detection ----

#[test]
fn detects_action_required_title() {
    let mut t = make_tracker(24, 80, "");
    assert!(!t.is_waiting_approval());
    t.process(b"\x1b]0;[ ! ] Action Required | proj | Working\x07");
    assert!(t.is_waiting_approval());
}

#[test]
fn detects_hidden_action_required_title() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"\x1b]2;[ . ] Action Required | proj | Working\x1b\\");
    assert!(t.is_waiting_approval());
}

#[test]
fn title_refresh_clears_approval() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"\x1b]0;[ ! ] Action Required | proj | Working\x07");
    assert!(t.is_waiting_approval());
    t.process(b"\x1b]0;proj | Working\x07");
    assert!(!t.is_waiting_approval());
}

#[test]
fn detects_title_split_across_process_calls() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"\x1b]0;[ ! ] Action");
    assert!(!t.is_waiting_approval());
    t.process(b" Required | proj | Working\x07");
    assert!(t.is_waiting_approval());
}

#[test]
fn action_required_body_text_does_not_trigger() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"Action Required is ordinary agent output\r\n");
    assert!(!t.is_waiting_approval());
}

#[test]
fn clear_approval_resets_title_state() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"\x1b]0;[ ! ] Action Required | proj | Working\x07");
    assert!(t.is_waiting_approval());
    t.clear_approval();
    assert!(!t.is_waiting_approval());
}

#[test]
fn codex_detects_visible_approval_dialog() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"Run command\r\nAllow command?\r\nPress enter to confirm or esc to cancel\r\n");
    assert!(t.is_codex_approval_visible());
}

#[test]
fn codex_transcript_viewer_is_not_approval() {
    let mut t = make_tracker(24, 80, "");
    t.process("Transcript\r\n↑/↓ to scroll  q to quit  esc to edit prev\r\n".as_bytes());
    assert!(!t.is_codex_approval_visible());
}

// ---- Antigravity approval detection ----

#[test]
fn antigravity_detects_approval_prompt() {
    let mut t = make_tracker(24, 80, "");
    assert!(!t.is_antigravity_approval_visible());
    t.process(b"Requesting permission for: hcom list --name lida\r\nDo you want to proceed?\r\n");
    assert!(t.is_antigravity_approval_visible());
}

#[test]
fn antigravity_no_false_positive_without_marker() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"> hello world\r\nrunning hcom list\r\n");
    assert!(!t.is_antigravity_approval_visible());
}

#[test]
fn antigravity_marker_alone_does_not_trigger() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"agent said: Requesting permission for: something earlier\r\n> idle\r\n");
    assert!(!t.is_antigravity_approval_visible());
}

#[test]
fn antigravity_detects_via_control_footer() {
    let mut t = make_tracker(24, 80, "");
    t.process(
        b"Requesting permission for: rm -rf /tmp/x\r\n  1. Yes\r\n  2. No\r\n  tab Amend . e edit command\r\n",
    );
    assert!(t.is_antigravity_approval_visible());
}

// ---- Cursor approval detection ----

#[test]
fn cursor_detects_approval_prompt() {
    let mut t = make_tracker(24, 80, "");
    assert!(!t.is_cursor_approval_visible());
    // Real cursor shell-approval menu (captured live).
    t.process(
        b"Run this command?\r\nNot in allowlist: uptime\r\n  Run (once) (y)\r\n  Auto-run everything (shift+tab)\r\n  Skip (esc or n)\r\n",
    );
    assert!(t.is_cursor_approval_visible());
}

#[test]
fn cursor_question_alone_does_not_trigger() {
    // The question text in narration/scrollback without the menu footer
    // must not flip approval on.
    let mut t = make_tracker(24, 80, "");
    t.process(b"I'll ask: Run this command? then proceed.\r\n> idle\r\n");
    assert!(!t.is_cursor_approval_visible());
}

#[test]
fn cursor_no_false_positive_without_question() {
    let mut t = make_tracker(24, 80, "");
    t.process(b"> hello world\r\nrunning a build\r\n");
    assert!(!t.is_cursor_approval_visible());
}

// ---- Claude subagent navigator detection ----
// Fixtures are faithful bottom-of-screen captures from Claude Code v2.1.218.

/// Render `lines` top-to-bottom onto the screen (index i -> row i).
fn render_rows(t: &mut ScreenTracker, lines: &[&str]) {
    t.process(lines.join("\r\n").as_bytes());
}

#[test]
fn claude_subagent_nav_detected_when_focused_in() {
    // Danger state: navigated into the subagent — its own input box is shown
    // (labeled with the task, not the session) with the navigator focused.
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 24];
    lines.extend_from_slice(&[
        "───────────────────────────────────────────── Run echo and sleep ──",
        "❯",
        "───────────────────────────────────────────────────────────────────",
        "  Enter to view · x to clear                                   /rc",
        "",
        "  ◯ main",
        "❯ ⏺ general-purpose  Run echo and sleep        16s · ↓ 26.7k tokens",
    ]);
    render_rows(&mut t, &lines);
    assert!(t.is_claude_subagent_nav_visible());
}

#[test]
fn claude_subagent_nav_detected_while_browsing_list() {
    // Navigator has focus, browsing the list (a different build's hint line).
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 26];
    lines.extend_from_slice(&[
        "  ↑/↓ to select · Enter to view                                /rc",
        "",
        "❯ ⏺ main",
        "  ◯ general-purpose  Run echo and sleep         9s · ↓ 26.6k tokens",
    ]);
    render_rows(&mut t, &lines);
    assert!(t.is_claude_subagent_nav_visible());
}

#[test]
fn claude_passive_peek_with_root_focused_is_not_gated() {
    // Auto-peek right after a background launch: the agent/token row is
    // present but there is NO "Enter to view" hint and the ROOT box holds
    // focus — injecting there is correct, so this must NOT be gated.
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 26];
    lines.extend_from_slice(&[
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent · esc to inter…",
        "                                                               /rc",
        "  ⏺ main",
        "  ◯ general-purpose  Run echo and sleep         2s · ↑ 26.1k tokens",
    ]);
    render_rows(&mut t, &lines);
    assert!(!t.is_claude_subagent_nav_visible());
}

#[test]
fn claude_collapsed_footer_after_subagent_done_is_not_gated() {
    // Subagent finished and the navigator collapsed to the plain footer:
    // no agent/token row, no hint. Proves gating cannot outlast the nav.
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 26];
    lines.extend_from_slice(&[
        "─────────────────────────────────────── set-default-model-sonnet ──",
        "❯",
        "───────────────────────────────────────────────────────────────────",
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent             /rc",
    ]);
    render_rows(&mut t, &lines);
    assert!(!t.is_claude_subagent_nav_visible());
}

#[test]
fn claude_plain_footer_token_counter_is_not_gated() {
    // The plain footer's session token counter ("47408 tokens") has no
    // direction arrow, so the agent-row anchor must not match it.
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 26];
    lines.extend_from_slice(&[
        "                                                       47408 tokens",
        "─────────────────────────────────────────────────────── claude ──",
        "❯",
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent             /rc",
    ]);
    render_rows(&mut t, &lines);
    assert!(!t.is_claude_subagent_nav_visible());
}

#[test]
fn claude_subagent_nav_in_scrollback_is_ignored() {
    // Both markers present, but only near the TOP (older scrollback that has
    // scrolled up); the live navigator is pinned to the bottom, so a settled
    // root prompt below it must not be gated.
    let mut t = make_tracker(30, 80, "");
    let mut lines = vec![
        "❯ ⏺ general-purpose  old task    9s · ↓ 5.2k tokens",
        "  Enter to view · x to clear",
    ];
    lines.extend(std::iter::repeat_n("", 26));
    lines.push("╭──────────────────────────────────────────────╮");
    lines.push("│ ❯                                              │");
    render_rows(&mut t, &lines);
    assert!(!t.is_claude_subagent_nav_visible());
}

#[test]
fn claude_session_switcher_placeholder_is_parsed_from_input_box() {
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 27];
    lines.extend_from_slice(&[
        "───────────────────────────────────────────────────────────────────",
        "❯ describe a task for a new session",
        "───────────────────────────────────────────────────────────────────",
        "  ⏵⏵ auto mode · enter to collapse · ? for shortcuts",
    ]);
    render_rows(&mut t, &lines);
    assert_eq!(
        t.get_input_box_text("claude").as_deref(),
        Some("describe a task for a new session")
    );
    assert!(t.is_claude_session_switcher_visible());
}

#[test]
fn claude_dim_session_switcher_placeholder_is_detected_before_discard() {
    let mut t = make_tracker(24, 80, "");
    t.process(format!("{}\r\n", "─".repeat(80)).as_bytes());
    let mut data = Vec::new();
    data.extend_from_slice("❯ ".as_bytes());
    data.extend_from_slice(b"\x1b[2m");
    data.extend_from_slice(b"describe a task for a new session");
    data.extend_from_slice(b"\x1b[0m\r\n");
    t.process(&data);
    t.process(format!("{}\r\n", "─".repeat(80)).as_bytes());

    assert_eq!(t.get_input_box_text("claude"), Some(String::new()));
    assert!(t.is_claude_session_switcher_visible());
}

#[test]
fn claude_session_switcher_markers_in_scrolled_chat_do_not_replace_input_text() {
    // Ordinary conversation, scrolled mid-screen, that discusses/quotes the
    // switcher UI must not be mistaken for the input-box placeholder.
    let mut t = make_tracker(31, 67, "");
    let mut lines = vec![""; 10];
    lines.push("⏺ The header shows \"2 awaiting input · 0 working · 1 completed\"");
    lines.push("  and the box placeholder reads \"describe a task for a new session\".");
    lines.resize(27, "");
    lines.push("───────────────────────────────────────────────────────────────────");
    lines.push("❯");
    lines.push("───────────────────────────────────────────────────────────────────");
    lines.push("  ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent             /rc");
    render_rows(&mut t, &lines);
    assert_eq!(t.get_input_box_text("claude"), Some(String::new()));
}

// ---- Codex input extraction ----

#[test]
fn codex_known_placeholder_without_dim_returns_empty() {
    let mut t = make_tracker(24, 80, "? for shortcuts");

    // 模拟 Windows PTY 丢失 dim 样式：
    // 占位文字以普通文本形式输出。
    t.process("› Improve documentation in @filename\r\n? for shortcuts\r\n".as_bytes());

    assert_eq!(t.get_codex_input_text(), Some(String::new()));
    assert!(t.is_prompt_empty("codex"));
}

#[test]
fn codex_extracts_text_after_prompt() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("› hello world\r\n".as_bytes());
    assert_eq!(t.get_codex_input_text(), Some("hello world".to_string()));
}

#[test]
fn codex_ignores_spinner_glyphs_after_input() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("› <hcom>  ⠄  ⠠⡀⠀\r\n".as_bytes());
    assert_eq!(t.get_codex_input_text(), Some("<hcom>".to_string()));
}

#[test]
fn codex_keeps_braille_within_input() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("› <hcom>⠄user  ⠠\r\n".as_bytes());
    assert_eq!(t.get_codex_input_text(), Some("<hcom>⠄user".to_string()));
}

#[test]
fn codex_keeps_unknown_prompt_suffix() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("› <hcom>  ⚙\r\n".as_bytes());
    assert_eq!(t.get_codex_input_text(), Some("<hcom>  ⚙".to_string()));
}

#[test]
fn codex_empty_prompt() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("› \r\n".as_bytes());
    assert_eq!(t.get_codex_input_text(), Some(String::new()));
}

#[test]
fn codex_no_prompt_no_ready() {
    let t = make_tracker(24, 80, "? for shortcuts");
    assert_eq!(t.get_codex_input_text(), None);
}

#[test]
fn codex_dim_placeholder_with_ready_returns_empty() {
    // Codex shows dim placeholder text when idle + ready pattern visible
    // Should return empty (it's placeholder, not real input)
    let mut t = make_tracker(24, 80, "? for shortcuts");
    // SGR 2 = dim, SGR 0 = reset
    let mut data = Vec::new();
    data.extend_from_slice("› ".as_bytes());
    data.extend_from_slice(b"\x1b[2m"); // dim on
    data.extend_from_slice(b"Improve docs");
    data.extend_from_slice(b"\x1b[0m"); // reset
    data.extend_from_slice(b"\r\n? for shortcuts\r\n");
    t.process(&data);
    assert_eq!(t.get_codex_input_text(), Some(String::new()));
}

#[test]
fn codex_non_dim_text_with_ready_returns_text() {
    // Injected text is NOT dim, even if ready pattern still visible (race condition)
    // Should return the text (it's real input, not placeholder)
    let mut t = make_tracker(24, 80, "? for shortcuts");
    // Non-dim text after prompt, ready pattern on next line
    t.process("› <hcom>test message</hcom>\r\n? for shortcuts\r\n".as_bytes());
    // Current bug: returns empty because is_ready()=true
    // After fix: should return the actual text
    assert_eq!(
        t.get_codex_input_text(),
        Some("<hcom>test message</hcom>".to_string())
    );
}

// ---- Cursor input extraction ----

#[test]
fn cursor_extracts_non_dim_text_after_prompt() {
    let mut t = make_tracker(24, 80, "");
    t.process("→ <hcom>\r\n".as_bytes());
    assert_eq!(t.get_cursor_input_text(), Some("<hcom>".to_string()));
}

#[test]
fn cursor_dim_placeholder_returns_empty() {
    let mut t = make_tracker(24, 80, "");
    let mut data = Vec::new();
    data.extend_from_slice("→ ".as_bytes());
    data.extend_from_slice(b"\x1b[2mPlan, search, build anything\x1b[0m");
    data.extend_from_slice(b"\r\n");
    t.process(&data);
    assert_eq!(t.get_cursor_input_text(), Some(String::new()));
    assert!(t.is_prompt_empty("cursor"));
}

// ---- Gemini input extraction ----

#[test]
fn gemini_extracts_text_from_bordered_box() {
    let mut t = make_tracker(24, 80, "Type your message");
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ > hello gemini           │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(t.get_gemini_input_text(), Some("hello gemini".to_string()));
}

#[test]
fn gemini_empty_box() {
    let mut t = make_tracker(24, 80, "Type your message");
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ >                        │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(t.get_gemini_input_text(), Some(String::new()));
}

#[test]
fn gemini_no_box_but_ready_pattern() {
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(b"Type your message\r\n");
    // No box found, but ready pattern visible → fallback to empty
    assert_eq!(t.get_gemini_input_text(), Some(String::new()));
}

#[test]
fn gemini_dash_border_single_line() {
    let border = "─".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", border).as_bytes());
    t.process(b" > hello gemini\r\n");
    t.process(format!("{}\r\n", border).as_bytes());
    assert_eq!(t.get_gemini_input_text(), Some("hello gemini".to_string()));
}

#[test]
fn gemini_dash_border_multi_line() {
    let border = "─".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", border).as_bytes());
    t.process(b" > first line of text\r\n");
    t.process(b"   second line of text\r\n");
    t.process(format!("{}\r\n", border).as_bytes());
    assert_eq!(
        t.get_gemini_input_text(),
        Some("first line of text second line of text".to_string())
    );
}

#[test]
fn gemini_new_format_multi_line() {
    let top = "▀".repeat(80);
    let bottom = "▄".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", top).as_bytes());
    t.process(b" > first line\r\n");
    t.process(b"   second line\r\n");
    t.process(format!("{}\r\n", bottom).as_bytes());
    assert_eq!(
        t.get_gemini_input_text(),
        Some("first line second line".to_string())
    );
}

#[test]
fn gemini_inverted_block_borders() {
    // Gemini CLI v0.40+ renders ▄ above the prompt and ▀ below it
    // (visually correct: ▄ fills bottom of its row → line above next row).
    let top = "▄".repeat(80);
    let bottom = "▀".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", top).as_bytes());
    t.process(b" > injected text\r\n");
    t.process(format!("{}\r\n", bottom).as_bytes());
    assert_eq!(t.get_gemini_input_text(), Some("injected text".to_string()));
}

#[test]
fn gemini_yolo_mode_extracts_text() {
    let top = "▀".repeat(80);
    let bottom = "▄".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", top).as_bytes());
    t.process(b" *   hello from yolo\r\n");
    t.process(format!("{}\r\n", bottom).as_bytes());
    assert_eq!(
        t.get_gemini_input_text(),
        Some("hello from yolo".to_string())
    );
}

#[test]
fn gemini_yolo_mode_empty_with_ready() {
    let top = "▀".repeat(80);
    let bottom = "▄".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", top).as_bytes());
    t.process(b" *   Type your message or @path/to/file\r\n");
    t.process(format!("{}\r\n", bottom).as_bytes());
    // Ready pattern visible in prompt text → empty
    assert_eq!(t.get_gemini_input_text(), Some(String::new()));
}

// ---- Kimi input extraction ----

#[test]
fn kimi_empty_box_is_empty() {
    let mut t = make_tracker(24, 80, "> ");
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ >                        │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(t.get_kimi_input_text(), Some(String::new()));
    assert!(t.is_prompt_empty("kimi"));
}

#[test]
fn kimi_extracts_typed_text() {
    let mut t = make_tracker(24, 80, "> ");
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ > hello kimi             │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(t.get_kimi_input_text(), Some("hello kimi".to_string()));
    // Crucial: ready pattern `> ` is still on screen, but the box has text,
    // so the prompt must NOT be reported empty (would clobber user input).
    assert!(!t.is_prompt_empty("kimi"));
}

#[test]
fn kimi_picks_input_box_over_welcome_banner() {
    let mut t = make_tracker(30, 80, "> ");
    // Welcome banner box (also uses ╭ … ╰) above the input box.
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│  Welcome to Kimi Code!   │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ >                        │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(t.get_kimi_input_text(), Some(String::new()));
}

#[test]
fn kimi_multi_line_input() {
    let mut t = make_tracker(24, 80, "> ");
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ > first line             │\r\n".as_bytes());
    t.process("│   second line            │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(
        t.get_kimi_input_text(),
        Some("first line second line".to_string())
    );
}

#[test]
fn kimi_yolo_marker_stripped() {
    let mut t = make_tracker(24, 80, "> ");
    t.process("╭──────────────────────────╮\r\n".as_bytes());
    t.process("│ *                        │\r\n".as_bytes());
    t.process("╰──────────────────────────╯\r\n".as_bytes());
    assert_eq!(t.get_kimi_input_text(), Some(String::new()));
}

#[test]
fn gemini_dash_border_empty_with_ready() {
    let border = "─".repeat(80);
    let mut t = make_tracker(24, 80, "Type your message");
    t.process(format!("{}\r\n", border).as_bytes());
    t.process(b" >   Type your message or @path/to/file\r\n");
    t.process(format!("{}\r\n", border).as_bytes());
    assert_eq!(t.get_gemini_input_text(), Some(String::new()));
}

// ---- Antigravity input extraction ----

#[test]
fn antigravity_extracts_text_after_prompt() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("> hello agy\r\n".as_bytes());
    assert_eq!(
        t.get_antigravity_input_text(),
        Some("hello agy".to_string())
    );
    assert_eq!(
        t.get_input_box_text("antigravity"),
        Some("hello agy".to_string())
    );
}

#[test]
fn antigravity_prompt_without_trailing_space() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process(">\r\n".as_bytes());
    assert_eq!(t.get_antigravity_input_text(), Some(String::new()));
    assert!(t.is_prompt_empty("antigravity"));
}

#[test]
fn antigravity_dim_placeholder_with_ready_returns_empty() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    let mut data = Vec::new();
    data.extend_from_slice(b"> ");
    data.extend_from_slice(b"\x1b[2mType your message\x1b[0m");
    data.extend_from_slice(b"\r\n? for shortcuts\r\n");
    t.process(&data);
    assert_eq!(t.get_antigravity_input_text(), Some(String::new()));
}

#[test]
fn antigravity_empty_prompt_with_ready() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("> \r\n? for shortcuts\r\n".as_bytes());
    assert_eq!(t.get_antigravity_input_text(), Some(String::new()));
}

#[test]
fn antigravity_injected_text_with_ready_footer() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("> <hcom>test</hcom>\r\n? for shortcuts\r\n".as_bytes());
    assert_eq!(
        t.get_antigravity_input_text(),
        Some("<hcom>test</hcom>".to_string())
    );
}

#[test]
fn antigravity_uses_bottommost_prompt_only() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("> <hcom>old message</hcom>\r\n".as_bytes());
    t.process("some agent output\r\n".as_bytes());
    t.process("> \r\n? for shortcuts\r\n".as_bytes());
    assert_eq!(t.get_antigravity_input_text(), Some(String::new()));
}

#[test]
fn antigravity_no_prompt_line_is_unknown_not_empty() {
    // Only the status bar is on screen — the prompt row is not located.
    // "Unknown" must not be reported as "empty", or the delivery gate would
    // inject over whatever the user has typed.
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process(" Ctx 6% (66k/1048k) |  5h 0% |  ~/workspaces/hcom\r\n".as_bytes());
    assert_eq!(t.get_antigravity_input_text(), None);
    assert!(!t.is_prompt_empty("antigravity"));
}

// ---- Antigravity readiness ----

#[test]
fn antigravity_idle_frame_is_ready() {
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process(
        concat!(
            "> \r\n",
            " Gemini 3.8 Flash (High) |  high |  65ce493d\r\n",
            " Ctx 6% (66k/1048k) |  5h 0% |  ~/workspaces/hcom | branch\r\n",
        )
        .as_bytes(),
    );
    assert!(t.is_ready(), "agy idle frame must satisfy the ready gate");
}

#[test]
fn antigravity_busy_frame_is_also_ready() {
    // The status bar renders while agy is running a command. Readiness answers
    // "is the TUI up", not "is agy idle" — idleness is the gate's own check.
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process(
        concat!(
            "* Running command...\r\n",
            "> \r\n",
            " Ctx 3% (33k/1048k) |  5h 1% |  ~/workspaces/hcom | branch\r\n",
        )
        .as_bytes(),
    );
    assert!(t.is_ready());
}

#[test]
fn antigravity_ready_pattern_survives_a_narrow_terminal() {
    // Claude's pattern hides when the terminal is narrow (integration_spec.rs:536).
    // agy's status bar is left-anchored, so the label survives truncation.
    let mut t = make_tracker(24, 40, "Ctx ");
    t.process(" Ctx 6% (66k/1048k) |  5h 0%\r\n".as_bytes());
    assert!(t.is_ready());
}

#[test]
fn antigravity_frame_without_status_bar_is_not_ready() {
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process("starting agy...\r\n".as_bytes());
    assert!(!t.is_ready());
}

// ---- Claude input extraction ----
// Claude uses dim attribute detection which requires proper VT100 SGR sequences

#[test]
fn claude_no_prompt_returns_none() {
    let t = make_tracker(24, 80, "? for shortcuts");
    assert_eq!(t.get_claude_input_text(), None);
}

#[test]
fn claude_prompt_with_borders_and_empty_text() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("────────────────────\r\n".as_bytes());
    t.process("❯ \r\n".as_bytes());
    t.process("────────────────────\r\n".as_bytes());
    assert_eq!(t.get_claude_input_text(), Some(String::new()));
}

#[test]
fn claude_prompt_with_non_dim_user_text() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("────────────────────\r\n".as_bytes());
    t.process("❯ hello\r\n".as_bytes());
    t.process("────────────────────\r\n".as_bytes());
    let result = t.get_claude_input_text();
    assert_eq!(result, Some("hello".to_string()));
}

#[test]
fn claude_prompt_with_dim_placeholder() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("────────────────────\r\n".as_bytes());
    // ❯ followed by dim text (SGR 2 = dim)
    let mut data = Vec::new();
    data.extend_from_slice("❯ ".as_bytes());
    data.extend_from_slice(b"\x1b[2m"); // SGR dim on
    data.extend_from_slice(b"placeholder text");
    data.extend_from_slice(b"\x1b[0m"); // SGR reset
    data.extend_from_slice(b"\r\n");
    t.process(&data);
    t.process("────────────────────\r\n".as_bytes());
    // Dim text should be treated as empty (placeholder)
    assert_eq!(t.get_claude_input_text(), Some(String::new()));
}

#[test]
fn claude_prompt_with_multiline_input_box_dim_placeholder() {
    // Claude Code sometimes shows a 2-line input box with the bottom border
    // 2 rows below the prompt (empty continuation row in between).
    // Dim placeholder text should still be detected as empty.
    let mut t = make_tracker(24, 52, "? for shortcuts");
    t.process("────────────────────────────────────────────────────\r\n".as_bytes());
    let mut data = Vec::new();
    data.extend_from_slice("❯ ".as_bytes());
    data.extend_from_slice(b"\x1b[2m"); // dim on
    data.extend_from_slice(b"tell the implementation agent to fix those");
    data.extend_from_slice(b"\x1b[0m"); // reset
    data.extend_from_slice(b"\r\n");
    t.process(&data);
    t.process(b"\r\n"); // empty continuation row
    t.process("────────────────────────────────────────────────────\r\n".as_bytes());
    assert_eq!(t.get_claude_input_text(), Some(String::new()));
}

#[test]
fn claude_prompt_picks_bottom_input_box_over_stale_output() {
    // Regression: cargo build output can produce ❯ + ─ patterns in the
    // scrollback that look like an input box. The parser must find the
    // *bottom-most* match (the real input box), not the first one.
    let mut t = make_tracker(30, 69, "? for shortcuts");
    // Stale output with ❯ between ─ lines
    t.process(
        "─────    Finished `dev` profile [unoptimized + debuginfo] target   ──\r\n".as_bytes(),
    );
    t.process("❯    (s) in 1.36s\r\n".as_bytes());
    t.process(
        " ─   Stale entry added, SessionStart groups: 3───────────────────────\r\n".as_bytes(),
    );
    // Some output in between
    t.process("Some other output\r\n".as_bytes());
    t.process("\r\n".as_bytes());
    // Real input box at the bottom
    t.process(
        "─────────────────────────────────────────────────────────────────────\r\n".as_bytes(),
    );
    t.process("❯\r\n".as_bytes());
    t.process(
        "─────────────────────────────────────────────────────────────────────\r\n".as_bytes(),
    );
    // Real prompt is empty — parser should find this, not the stale one
    assert_eq!(t.get_claude_input_text(), Some(String::new()));
}

#[test]
fn claude_bypass_permissions_ascii_prompt_with_empty_text() {
    // `--permission-mode bypassPermissions` renders the same bordered box
    // with a plain `>` instead of the styled `❯`.
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("────────────────────\r\n".as_bytes());
    t.process("> \r\n".as_bytes());
    t.process("────────────────────\r\n".as_bytes());
    assert_eq!(t.get_claude_input_text(), Some(String::new()));
}

#[test]
fn claude_bypass_permissions_ascii_prompt_with_non_dim_user_text() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("────────────────────\r\n".as_bytes());
    t.process("> hello\r\n".as_bytes());
    t.process("────────────────────\r\n".as_bytes());
    assert_eq!(t.get_claude_input_text(), Some("hello".to_string()));
}

#[test]
fn claude_bypass_permissions_ascii_prompt_with_dim_placeholder() {
    let mut t = make_tracker(24, 80, "? for shortcuts");
    t.process("────────────────────\r\n".as_bytes());
    let mut data = Vec::new();
    data.extend_from_slice("> ".as_bytes());
    data.extend_from_slice(b"\x1b[2m"); // SGR dim on
    data.extend_from_slice(b"placeholder text");
    data.extend_from_slice(b"\x1b[0m"); // SGR reset
    data.extend_from_slice(b"\r\n");
    t.process(&data);
    t.process("────────────────────\r\n".as_bytes());
    assert_eq!(t.get_claude_input_text(), Some(String::new()));
}

// ---- trim_with_nbsp ----

#[test]
fn trim_nbsp() {
    assert_eq!(trim_with_nbsp(" hello\u{00A0}"), "hello");
    assert_eq!(trim_with_nbsp("\u{00A0}\u{00A0}"), "");
}

// ---- output stability ----

#[test]
fn output_stable_zero_always_true() {
    let t = make_tracker(24, 80, "");
    assert!(t.is_output_stable(0));
}
