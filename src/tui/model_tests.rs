use super::*;

fn test_agent(name: &str) -> Agent {
    Agent {
        name: name.into(),
        tool: Tool::Claude,
        status: AgentStatus::Active,
        status_context: String::new(),
        status_detail: String::new(),
        created_at: 1000.0,
        status_time: 1000.0,
        last_heartbeat: 1000.0,
        has_tcp: true,
        directory: String::new(),
        tag: String::new(),
        unread: 0,
        last_event_id: None,
        device_name: None,
        sync_age: None,
        headless: false,
        session_id: None,
        pid: None,
        terminal_preset: None,
    }
}

// ── format_duration_short ─────────────────────────────────────

#[test]
fn duration_zero_is_now() {
    assert_eq!(format_duration_short(0), "now");
}

#[test]
fn duration_seconds_boundary() {
    assert_eq!(format_duration_short(1), "1s");
    assert_eq!(format_duration_short(59), "59s");
}

#[test]
fn duration_minutes_boundary() {
    assert_eq!(format_duration_short(60), "1m");
    assert_eq!(format_duration_short(119), "1m"); // truncates, not rounds
    assert_eq!(format_duration_short(3599), "59m");
}

#[test]
fn duration_hours_boundary() {
    assert_eq!(format_duration_short(3600), "1h");
    assert_eq!(format_duration_short(86399), "23h");
}

#[test]
fn duration_days() {
    assert_eq!(format_duration_short(86400), "1d");
    assert_eq!(format_duration_short(172800), "2d");
}

// ── cursor_left / cursor_right ────────────────────────────────

#[test]
fn cursor_left_ascii() {
    let s = "abc";
    let mut c = 3;
    cursor_left(s, &mut c);
    assert_eq!(c, 2);
    cursor_left(s, &mut c);
    assert_eq!(c, 1);
}

#[test]
fn cursor_left_at_zero_is_noop() {
    let mut c = 0;
    cursor_left("abc", &mut c);
    assert_eq!(c, 0);
}

#[test]
fn cursor_right_ascii() {
    let s = "abc";
    let mut c = 0;
    cursor_right(s, &mut c);
    assert_eq!(c, 1);
    cursor_right(s, &mut c);
    assert_eq!(c, 2);
}

#[test]
fn cursor_right_at_end_is_noop() {
    let s = "abc";
    let mut c = 3;
    cursor_right(s, &mut c);
    assert_eq!(c, 3);
}

#[test]
fn cursor_moves_by_grapheme_multibyte() {
    let s = "aéb"; // é is 2 bytes
    let mut c = 0;
    cursor_right(s, &mut c);
    assert_eq!(c, 1); // past 'a'
    cursor_right(s, &mut c);
    assert_eq!(c, 3); // past 'é' (2 bytes)
    cursor_left(s, &mut c);
    assert_eq!(c, 1); // back to before 'é'
}

#[test]
fn cursor_on_empty_string() {
    let mut c = 0;
    cursor_left("", &mut c);
    assert_eq!(c, 0);
    cursor_right("", &mut c);
    assert_eq!(c, 0);
}

// ── delete_back ───────────────────────────────────────────────

#[test]
fn delete_back_ascii() {
    let mut s = "abc".to_string();
    let mut c = 3;
    delete_back(&mut s, &mut c);
    assert_eq!(s, "ab");
    assert_eq!(c, 2);
}

#[test]
fn delete_back_at_zero_is_noop() {
    let mut s = "abc".to_string();
    let mut c = 0;
    delete_back(&mut s, &mut c);
    assert_eq!(s, "abc");
    assert_eq!(c, 0);
}

#[test]
fn delete_back_multibyte() {
    let mut s = "aé".to_string();
    let mut c = s.len(); // 3
    delete_back(&mut s, &mut c);
    assert_eq!(s, "a");
    assert_eq!(c, 1);
}

// ── delete_word_back ──────────────────────────────────────────

#[test]
fn delete_word_back_single_word() {
    let mut s = "hello".to_string();
    let mut c = 5;
    delete_word_back(&mut s, &mut c);
    assert_eq!(s, "");
    assert_eq!(c, 0);
}

#[test]
fn delete_word_back_two_words() {
    let mut s = "hello world".to_string();
    let mut c = 11;
    delete_word_back(&mut s, &mut c);
    assert_eq!(s, "hello ");
    assert_eq!(c, 6);
}

#[test]
fn delete_word_back_trailing_spaces() {
    let mut s = "hello   ".to_string();
    let mut c = 8;
    delete_word_back(&mut s, &mut c);
    assert_eq!(s, "");
    assert_eq!(c, 0);
}

#[test]
fn delete_word_back_at_zero_is_noop() {
    let mut s = "hello".to_string();
    let mut c = 0;
    delete_word_back(&mut s, &mut c);
    assert_eq!(s, "hello");
    assert_eq!(c, 0);
}

#[test]
fn delete_word_back_mid_string() {
    let mut s = "one two three".to_string();
    let mut c = 7; // after "two"
    delete_word_back(&mut s, &mut c);
    assert_eq!(s, "one  three");
    assert_eq!(c, 4);
}

// ── delete_to_start ───────────────────────────────────────────

#[test]
fn delete_to_start_from_middle() {
    let mut s = "hello world".to_string();
    let mut c = 5;
    delete_to_start(&mut s, &mut c);
    assert_eq!(s, " world");
    assert_eq!(c, 0);
}

#[test]
fn delete_to_start_at_zero_is_noop() {
    let mut s = "hello".to_string();
    let mut c = 0;
    delete_to_start(&mut s, &mut c);
    assert_eq!(s, "hello");
    assert_eq!(c, 0);
}

// ── insert_at ─────────────────────────────────────────────────

#[test]
fn insert_at_start() {
    let mut s = "bc".to_string();
    let mut c = 0;
    insert_at(&mut s, &mut c, 'a');
    assert_eq!(s, "abc");
    assert_eq!(c, 1);
}

#[test]
fn insert_at_end() {
    let mut s = "ab".to_string();
    let mut c = 2;
    insert_at(&mut s, &mut c, 'c');
    assert_eq!(s, "abc");
    assert_eq!(c, 3);
}

#[test]
fn insert_multibyte_char() {
    let mut s = "ab".to_string();
    let mut c = 1;
    insert_at(&mut s, &mut c, 'é');
    assert_eq!(s, "aéb");
    assert_eq!(c, 3); // é is 2 bytes
}

// ── Agent::display_name ───────────────────────────────────────

#[test]
fn display_name_plain() {
    let a = test_agent("nova");
    assert_eq!(a.display_name(), "nova");
}

#[test]
fn display_name_with_tag() {
    let mut a = test_agent("nova");
    a.tag = "dev".into();
    assert_eq!(a.display_name(), "dev-nova");
}

#[test]
fn display_name_remote() {
    let mut a = test_agent("nova");
    a.device_name = Some("BOXE".into());
    assert_eq!(a.display_name(), "nova:BOXE");
}

#[test]
fn display_name_tag_and_remote() {
    let mut a = test_agent("nova");
    a.tag = "dev".into();
    a.device_name = Some("BOXE".into());
    assert_eq!(a.display_name(), "dev-nova:BOXE");
}

// ── Agent::context_display ────────────────────────────────────

#[test]
fn context_display_strips_prefix() {
    let mut a = test_agent("nova");
    a.status_context = "tool:Bash".into();
    assert_eq!(a.context_display(), "Bash");
}

#[test]
fn context_display_with_detail() {
    let mut a = test_agent("nova");
    a.status_context = "tool:Read".into();
    a.status_detail = "/tmp/foo.rs".into();
    assert_eq!(a.context_display(), "Read: /tmp/foo.rs");
}

#[test]
fn context_display_no_prefix() {
    let mut a = test_agent("nova");
    a.status_context = "listening".into();
    assert_eq!(a.context_display(), "listening");
}

#[test]
fn context_display_all_prefixes_stripped() {
    for (input, expected) in [
        ("tool:Edit", "Edit"),
        ("deliver:pending", "pending"),
        ("approved:yes", "yes"),
        ("exit:0", "0"),
        ("stale:active", "active"),
        ("tui:not-ready", "not-ready"),
    ] {
        let mut a = test_agent("nova");
        a.status_context = input.into();
        assert_eq!(
            a.context_display(),
            expected,
            "prefix not stripped from {input}"
        );
    }
}

// ── Agent::is_pty_blocked ─────────────────────────────────────

#[test]
fn pty_blocked_with_tui_prefix() {
    let mut a = test_agent("nova");
    a.status_context = "tui:not-ready".into();
    assert!(a.is_pty_blocked());
}

#[test]
fn pty_not_blocked_without_tui_prefix() {
    let mut a = test_agent("nova");
    a.status_context = "tool:Bash".into();
    assert!(!a.is_pty_blocked());
}

// ── Tool cycling ──────────────────────────────────────────────

#[test]
fn tool_next_cycles_through_launchable() {
    assert_eq!(Tool::Claude.next(), Tool::Gemini);
    assert_eq!(Tool::Gemini.next(), Tool::Codex);
    assert_eq!(Tool::Codex.next(), Tool::OpenCode);
    assert_eq!(Tool::OpenCode.next(), Tool::Kilo);
    assert_eq!(Tool::Kilo.next(), Tool::Pi);
    assert_eq!(Tool::Pi.next(), Tool::Omp);
    assert_eq!(Tool::Omp.next(), Tool::Antigravity);
    assert_eq!(Tool::Antigravity.next(), Tool::Cursor);
    assert_eq!(Tool::Cursor.next(), Tool::Kimi);
    assert_eq!(Tool::Kimi.next(), Tool::Copilot);
    assert_eq!(Tool::Copilot.next(), Tool::Claude);
}

#[test]
fn tool_prev_cycles_backward() {
    assert_eq!(Tool::Claude.prev(), Tool::Copilot);
    assert_eq!(Tool::Copilot.prev(), Tool::Kimi);
    assert_eq!(Tool::Kimi.prev(), Tool::Cursor);
    assert_eq!(Tool::Cursor.prev(), Tool::Antigravity);
    assert_eq!(Tool::Antigravity.prev(), Tool::Omp);
    assert_eq!(Tool::Omp.prev(), Tool::Pi);
    assert_eq!(Tool::Pi.prev(), Tool::Kilo);
    assert_eq!(Tool::Kilo.prev(), Tool::OpenCode);
    assert_eq!(Tool::OpenCode.prev(), Tool::Codex);
    assert_eq!(Tool::Codex.prev(), Tool::Gemini);
    assert_eq!(Tool::Gemini.prev(), Tool::Claude);
}

#[test]
fn tool_adhoc_does_not_cycle() {
    assert_eq!(Tool::Adhoc.next(), Tool::Adhoc);
    assert_eq!(Tool::Adhoc.prev(), Tool::Adhoc);
}

// ── CommandPalette ────────────────────────────────────────────

fn test_palette() -> CommandPalette {
    CommandPalette::new(vec![
        CommandSuggestion {
            command: "list".into(),
            description: "Show agents",
        },
        CommandSuggestion {
            command: "kill".into(),
            description: "Stop agent",
        },
        CommandSuggestion {
            command: "send".into(),
            description: "Send message",
        },
    ])
}

#[test]
fn palette_filter_narrows_by_command() {
    let mut p = test_palette();
    p.filter("ki");
    assert_eq!(p.filtered.len(), 1);
    assert_eq!(p.all[p.filtered[0]].command, "kill");
}

#[test]
fn palette_filter_matches_description() {
    let mut p = test_palette();
    p.filter("agent");
    assert_eq!(p.filtered.len(), 2); // "Show agents" and "Stop agent"
}

#[test]
fn palette_filter_empty_shows_all() {
    let mut p = test_palette();
    p.filter("");
    assert_eq!(p.filtered.len(), 3);
}

#[test]
fn palette_cursor_down_from_none() {
    let mut p = test_palette();
    assert_eq!(p.cursor, None);
    p.cursor_down();
    assert_eq!(p.cursor, Some(0));
}

#[test]
fn palette_cursor_up_to_none() {
    let mut p = test_palette();
    p.cursor = Some(0);
    p.cursor_up();
    assert_eq!(p.cursor, None);
}

#[test]
fn palette_cursor_clamps_on_filter() {
    let mut p = test_palette();
    p.cursor = Some(2); // last item
    p.filter("list"); // only 1 result
    assert_eq!(p.cursor, Some(0)); // clamped
}

#[test]
fn palette_selected_returns_highlighted() {
    let mut p = test_palette();
    p.cursor_down();
    let sel = p.selected().unwrap();
    assert_eq!(sel.command, "list");
}

#[test]
fn palette_selected_none_when_no_cursor() {
    let p = test_palette();
    assert!(p.selected().is_none());
}

// ── LaunchState navigation ────────────────────────────────────

fn test_launch() -> LaunchState {
    LaunchState {
        tool: Tool::Claude,
        count: 1,
        options_cursor: None,
        tag: String::new(),
        headless: false,
        headless_pty: false,
        terminal: 0,
        terminal_presets: vec!["default".into(), "kitty".into()],
        editing: None,
        edit_cursor: 0,
        edit_snapshot: None,
    }
}

#[test]
fn launch_panel_height_constant_across_tools() {
    // Headless is available for every tool, so the panel is the same height.
    let mut ls = test_launch();
    assert_eq!(ls.panel_height(), 6);
    ls.tool = Tool::Gemini;
    assert_eq!(ls.panel_height(), 6);
}

#[test]
fn launch_settings_fields_have_headless_for_all_tools() {
    let mut ls = test_launch();
    assert!(ls.settings_fields().contains(&LaunchField::Headless));
    ls.tool = Tool::Gemini;
    assert!(ls.settings_fields().contains(&LaunchField::Headless));
}

#[test]
fn launch_cursor_down_from_none_goes_to_first() {
    let mut ls = test_launch();
    ls.cursor_down();
    assert_eq!(ls.options_cursor, Some(LaunchField::Tool));
}

#[test]
fn launch_cursor_down_wraps_to_none() {
    let mut ls = test_launch();
    ls.options_cursor = Some(LaunchField::Terminal); // last field
    ls.cursor_down();
    assert_eq!(ls.options_cursor, None);
}

#[test]
fn launch_cursor_up_from_none_goes_to_last() {
    let mut ls = test_launch();
    ls.cursor_up();
    assert_eq!(ls.options_cursor, Some(LaunchField::Terminal));
}

#[test]
fn launch_cursor_up_wraps_to_none() {
    let mut ls = test_launch();
    ls.options_cursor = Some(LaunchField::Tool); // first field
    ls.cursor_up();
    assert_eq!(ls.options_cursor, None);
}

#[test]
fn launch_count_bounds() {
    let mut ls = test_launch();
    ls.options_cursor = Some(LaunchField::Count);
    ls.count = 1;
    ls.adjust_left();
    assert_eq!(ls.count, 1); // can't go below 1

    ls.count = 99;
    ls.adjust_right();
    assert_eq!(ls.count, 99); // can't go above 99
}

#[test]
fn launch_tool_cycles_with_adjust() {
    let mut ls = test_launch();
    ls.options_cursor = Some(LaunchField::Tool);
    assert_eq!(ls.tool, Tool::Claude);
    ls.adjust_right();
    assert_eq!(ls.tool, Tool::Gemini);
    ls.adjust_left();
    assert_eq!(ls.tool, Tool::Claude);
}

#[test]
fn launch_terminal_wraps() {
    let mut ls = test_launch();
    ls.options_cursor = Some(LaunchField::Terminal);
    assert_eq!(ls.terminal, 0);
    ls.adjust_left(); // wraps to last
    assert_eq!(ls.terminal, 1);
    ls.adjust_right(); // wraps to first
    assert_eq!(ls.terminal, 0);
}

#[test]
fn launch_headless_cycles_off_pty_print_for_claude() {
    let mut ls = test_launch(); // claude by default
    ls.options_cursor = Some(LaunchField::Headless);
    assert!(!ls.headless && !ls.headless_pty); // off
    ls.toggle_or_select();
    assert!(ls.headless && ls.headless_pty); // pty (default)
    ls.toggle_or_select();
    assert!(ls.headless && !ls.headless_pty); // print
    ls.toggle_or_select();
    assert!(!ls.headless && !ls.headless_pty); // back to off
}

#[test]
fn launch_headless_is_plain_toggle_for_non_claude() {
    let mut ls = test_launch();
    ls.tool = Tool::Gemini;
    ls.options_cursor = Some(LaunchField::Headless);
    assert!(!ls.headless);
    ls.toggle_or_select();
    assert!(ls.headless && !ls.headless_pty);
    ls.toggle_or_select();
    assert!(!ls.headless && !ls.headless_pty);
}

#[test]
fn launch_switching_tool_off_claude_clears_pty() {
    let mut ls = test_launch(); // claude
    ls.options_cursor = Some(LaunchField::Tool);
    ls.headless = true;
    ls.headless_pty = true;
    ls.adjust_right(); // cycle off claude
    assert_ne!(ls.tool, Tool::Claude);
    assert!(!ls.headless_pty);
}

#[test]
fn launch_tag_auto_edits() {
    let mut ls = test_launch();
    // Navigate to Tag field
    ls.options_cursor = Some(LaunchField::Count);
    ls.cursor_down(); // → Tag
    assert_eq!(ls.options_cursor, Some(LaunchField::Tag));
    assert_eq!(ls.editing, Some(LaunchField::Tag));
}
