use super::*;
use crate::shared::status_icon;

#[test]
fn should_start_delivery_on_ready() {
    // Ready alone starts delivery even well before the timeout.
    assert!(should_start_delivery(
        true,
        Duration::from_millis(1),
        Duration::from_secs(10),
        false,
    ));
}

#[test]
fn should_start_delivery_on_timeout() {
    // #8 regression: not ready, but elapsed exceeded the timeout → start.
    assert!(should_start_delivery(
        false,
        Duration::from_secs(11),
        Duration::from_secs(10),
        false,
    ));
}

#[test]
fn should_start_delivery_on_shutdown() {
    // Shutting down forces a final start (child already exited) so init-time
    // registration and post-loop cleanup run, even if never ready and still
    // inside the timeout.
    assert!(should_start_delivery(
        false,
        Duration::from_millis(1),
        Duration::from_secs(10),
        true,
    ));
}

#[test]
fn should_not_start_delivery_before_ready_or_timeout() {
    // #8 regression: not ready and still inside the timeout window while
    // running → must NOT start yet (the old reader could start too early).
    assert!(!should_start_delivery(
        false,
        Duration::from_secs(1),
        Duration::from_secs(10),
        false,
    ));
}

#[test]
fn should_refresh_snapshot_only_after_throttle_elapsed() {
    let throttle = Duration::from_millis(100);
    // Fresh chunk right after a refresh: defer (mark dirty), don't re-render.
    assert!(!should_refresh_snapshot(Duration::from_millis(0), throttle));
    assert!(!should_refresh_snapshot(
        Duration::from_millis(99),
        throttle
    ));
    // At/after the throttle window: render now (leading edge).
    assert!(should_refresh_snapshot(
        Duration::from_millis(100),
        throttle
    ));
    assert!(should_refresh_snapshot(
        Duration::from_millis(250),
        throttle
    ));
}

#[test]
fn csi_is_dsr_cpr_matches_only_the_cursor_position_query() {
    assert!(csi_is_dsr_cpr(b"\x1b[6n"));
    // A CPR reply, a private DSR, and a bare DSR must not match.
    assert!(!csi_is_dsr_cpr(b"\x1b[6;1R"));
    assert!(!csi_is_dsr_cpr(b"\x1b[?6n"));
    assert!(!csi_is_dsr_cpr(b"\x1b[n"));
}

fn filter_modes(chunks: &[&[u8]]) -> Vec<u8> {
    let mut f = OutputModeFilter::default();
    let mut out = Vec::new();
    for c in chunks {
        f.filter(c, &mut out);
    }
    out
}

#[test]
fn filter_dec_private_modes_drops_only_targeted_params() {
    // Whole-prefix regression (#15): a targeted param anywhere in the list
    // must be dropped without losing the others, in either order.
    assert_eq!(filter_dec_private_modes(b"\x1b[?9001;25h"), b"\x1b[?25h");
    assert_eq!(filter_dec_private_modes(b"\x1b[?25;9001h"), b"\x1b[?25h");
    assert_eq!(
        filter_dec_private_modes(b"\x1b[?1004;2004h"),
        b"\x1b[?2004h"
    );
    assert_eq!(filter_dec_private_modes(b"\x1b[?9001;1004h"), b"");
    assert_eq!(
        filter_dec_private_modes(b"\x1b[?9001;1004;25h"),
        b"\x1b[?25h"
    );
    assert_eq!(filter_dec_private_modes(b"\x1b[?25;9001l"), b"\x1b[?25l");
}

#[test]
fn output_mode_filter_drops_win32_and_focus_mode_sets() {
    // ESC[?9001h ESC[?1004h "hi" ESC[6n — mode-sets dropped, DSR passes.
    let input = b"\x1b[?9001h\x1b[?1004h hi \x1b[6n";
    assert_eq!(filter_modes(&[input]), b" hi \x1b[6n");
}

#[test]
fn output_mode_filter_passes_other_sequences_and_text() {
    let input = b"\x1b[31mred\x1b[0m\x1b[2J plain";
    assert_eq!(filter_modes(&[input]), input);
}

#[test]
fn output_mode_filter_keeps_other_modes_in_a_combined_set() {
    // #15: mixed sets keep non-targeted modes and drop targeted ones,
    // regardless of parameter order.
    assert_eq!(filter_modes(&[b"\x1b[?9001;25h"]), b"\x1b[?25h");
    assert_eq!(filter_modes(&[b"\x1b[?25;9001h"]), b"\x1b[?25h");
    assert_eq!(filter_modes(&[b"\x1b[4h\x1b[?25h"]), b"\x1b[4h\x1b[?25h");
}

#[test]
fn output_mode_filter_handles_sequence_split_across_reads() {
    // A combined set split mid-sequence still filters per-parameter.
    assert_eq!(filter_modes(&[b"\x1b[?25;90", b"01h"]), b"\x1b[?25h");
    // The pure Win32-input set split mid-sequence is still fully dropped.
    assert_eq!(filter_modes(&[b"\x1b[?90", b"01h", b"X"]), b"X");
}

#[test]
fn output_mode_filter_drops_mode_reset_too() {
    assert_eq!(filter_modes(&[b"\x1b[?9001l\x1b[?1004lY"]), b"Y");
}

#[test]
fn output_mode_filter_take_dsr_is_one_shot() {
    let mut f = OutputModeFilter::default();
    let mut out = Vec::new();
    f.filter(b"\x1b[6n", &mut out);
    // DSR passes through to the outer terminal...
    assert_eq!(out, b"\x1b[6n");
    // ...and is latched exactly once.
    assert!(f.take_dsr());
    assert!(!f.take_dsr());
}

#[test]
fn output_mode_filter_strips_title_osc_split_across_reads() {
    assert_eq!(
        filter_modes(&[b"before\x1b]2;Clau", b"de Code\x07after"]),
        b"beforeafter"
    );
    assert_eq!(
        filter_modes(&[b"\x1b", b"]1", b";icon\x1b", b"\\text"]),
        b"text"
    );
}

#[test]
fn output_mode_filter_passthrough_keeps_tool_title() {
    // title_mode `off`: the tool's own OSC 0/2 title must reach the terminal
    // intact, including across a read split, while DSR tracking still works.
    let mut f = OutputModeFilter::default();
    f.set_passthrough_titles(true);
    let mut out = Vec::new();
    f.filter(b"before\x1b]2;Clau", &mut out);
    f.filter(b"de Code\x07after", &mut out);
    assert_eq!(out, b"before\x1b]2;Claude Code\x07after".to_vec());
}

#[test]
fn output_mode_filter_preserves_non_title_osc_and_tracks_its_boundary() {
    let chunks: &[&[u8]] = &[b"\x1b]8;;https://exam", b"ple.test\x1b\\link"];
    assert_eq!(filter_modes(chunks), chunks.concat());

    let mut f = OutputModeFilter::default();
    let mut out = Vec::new();
    f.filter(chunks[0], &mut out);
    assert!(!f.title_write_safe());
    f.filter(chunks[1], &mut out);
    assert!(f.title_write_safe());
}

#[test]
fn output_mode_filter_defers_title_across_split_utf8() {
    let mut f = OutputModeFilter::default();
    let mut out = Vec::new();
    f.filter(&[0xe2], &mut out);
    assert!(!f.title_write_safe());
    f.filter(&[0x94], &mut out);
    assert!(!f.title_write_safe());
    f.filter(&[0x80], &mut out);
    assert!(f.title_write_safe());
    assert_eq!(out, "─".as_bytes());
}

#[test]
fn output_mode_filter_title_only_read_preserves_pending_utf8() {
    let mut f = OutputModeFilter::default();
    let mut out = Vec::new();
    f.filter(&[0xe2, 0x94], &mut out);
    assert!(!f.title_write_safe());
    f.filter(b"\x1b]2;Claude Code\x07", &mut out);
    assert!(!f.title_write_safe());
    f.filter(&[0x80], &mut out);
    assert!(f.title_write_safe());
}

#[test]
fn output_mode_filter_tracks_other_split_escape_boundaries() {
    let mut f = OutputModeFilter::default();
    let mut out = Vec::new();

    f.filter(b"\x1bPpayload", &mut out);
    assert!(!f.title_write_safe());
    f.filter(b"\x1b\\", &mut out);
    assert!(f.title_write_safe());

    f.filter(b"\x1bN", &mut out);
    assert!(!f.title_write_safe());
    f.filter(b"x", &mut out);
    assert!(f.title_write_safe());

    f.filter(b"\x1b(", &mut out);
    assert!(!f.title_write_safe());
    f.filter(b"B", &mut out);
    assert!(f.title_write_safe());

    assert_eq!(out, b"\x1bPpayload\x1b\\\x1bNx\x1b(B");
}

#[test]
fn build_title_escape_label_mode_formats_osc_1_and_2() {
    use crate::shared::TitleMode;
    // Label mode keeps the [tool] tag; assert exact OSC framing.
    let esc = build_title_escape("alpha", "listening", "claude", TitleMode::Label, None);
    let icon = status_icon("listening");
    let title = format!("{} alpha [claude]", icon);
    assert_eq!(esc, format!("\x1b]1;{}\x07\x1b]2;{}\x07", title, title));
    assert!(esc.starts_with("\x1b]1;"));
    assert!(esc.contains("\x07\x1b]2;"));
    assert!(esc.ends_with('\x07'));
}

#[test]
fn build_title_escape_uses_status_icon() {
    use crate::shared::TitleMode;
    // Different statuses must change the embedded icon.
    let listening = build_title_escape("a", "listening", "claude", TitleMode::Label, None);
    let blocked = build_title_escape("a", "blocked", "claude", TitleMode::Label, None);
    assert_ne!(listening, blocked);
}

#[test]
fn build_title_escape_combined_appends_child_and_drops_tool() {
    use crate::shared::TitleMode;
    // Combined mode: `{icon} name - {child}`, no `[tool]` tag.
    let icon = status_icon("active");
    let esc = build_title_escape(
        "luna",
        "active",
        "codex",
        TitleMode::Combined,
        Some("⠋ Working"),
    );
    let title = format!("{} luna - ⠋ Working", icon);
    assert_eq!(esc, format!("\x1b]1;{}\x07\x1b]2;{}\x07", title, title));
    assert!(!esc.contains("[codex]"), "combined mode drops the tool tag");
}

#[test]
fn build_title_escape_combined_without_child_is_icon_name() {
    use crate::shared::TitleMode;
    // No child title → just `{icon} name`, no dangling separator, no tag.
    let icon = status_icon("active");
    let esc = build_title_escape("luna", "active", "codex", TitleMode::Combined, None);
    let title = format!("{} luna", icon);
    assert_eq!(esc, format!("\x1b]1;{}\x07\x1b]2;{}\x07", title, title));
}

#[test]
fn note_user_keystroke_cursor_is_noop_and_returns_false() {
    let target = PtyTarget::AdhocCommand("cursor".to_string());
    let state = Arc::new(RwLock::new(ScreenState {
        approval: true,
        ..ScreenState::default()
    }));
    let calls = std::cell::Cell::new(0);
    let publish = |_a: bool| calls.set(calls.get() + 1);
    // cursor name: must not clear approval, must not publish, returns false.
    let cleared = note_user_keystroke(&target, &state, &publish);
    assert!(!cleared);
    assert!(state.read().unwrap().approval, "cursor approval untouched");
    assert_eq!(calls.get(), 0, "cursor keystroke must not publish");
}

#[test]
fn note_user_keystroke_clears_approval_for_non_cursor() {
    let target = PtyTarget::Known(Tool::Claude);
    let state = Arc::new(RwLock::new(ScreenState {
        approval: true,
        ..ScreenState::default()
    }));
    let calls = std::cell::Cell::new(0);
    let publish = |a: bool| {
        assert!(!a, "keystroke publishes the cleared (false) edge");
        calls.set(calls.get() + 1);
    };
    let cleared = note_user_keystroke(&target, &state, &publish);
    assert!(cleared, "a standing approval was cleared");
    assert!(!state.read().unwrap().approval, "approval cleared");
    assert_eq!(calls.get(), 1, "cleared edge published once");
}

#[test]
fn note_user_keystroke_no_publish_when_not_blocked() {
    let target = PtyTarget::Known(Tool::Claude);
    let state = Arc::new(RwLock::new(ScreenState::default())); // approval=false
    let calls = std::cell::Cell::new(0);
    let publish = |_a: bool| calls.set(calls.get() + 1);
    // No approval was showing, so nothing is cleared and the Windows caller
    // must not request a tracker-clear (which would wipe the scrape buffer).
    let cleared = note_user_keystroke(&target, &state, &publish);
    assert!(
        !cleared,
        "no standing approval means no tracker clear requested"
    );
    assert_eq!(calls.get(), 0, "no edge to publish when already clear");
}

#[test]
fn clear_injected_approval_state_cursor_returns_false() {
    let target = PtyTarget::AdhocCommand("cursor".to_string());
    let state = Arc::new(RwLock::new(ScreenState {
        approval: true,
        ..ScreenState::default()
    }));
    let publish = |_a: bool| panic!("cursor must not publish");
    assert!(!clear_injected_approval_state(&target, &state, &publish));
    assert!(state.read().unwrap().approval, "cursor approval untouched");
}

#[test]
fn clear_injected_approval_state_clears_when_blocked() {
    let target = PtyTarget::Known(Tool::Claude);
    let state = Arc::new(RwLock::new(ScreenState {
        approval: true,
        ..ScreenState::default()
    }));
    let calls = std::cell::Cell::new(0);
    let publish = |a: bool| {
        assert!(!a);
        calls.set(calls.get() + 1);
    };
    assert!(clear_injected_approval_state(&target, &state, &publish));
    assert!(!state.read().unwrap().approval);
    assert_eq!(calls.get(), 1);
}

#[test]
fn clear_injected_approval_state_noop_when_not_blocked() {
    let target = PtyTarget::Known(Tool::Claude);
    let state = Arc::new(RwLock::new(ScreenState::default()));
    let publish = |_a: bool| panic!("must not publish when nothing to clear");
    assert!(!clear_injected_approval_state(&target, &state, &publish));
}

// build_early_launch_context is portable (env::var/fs::read_to_string/thread::sleep
// all work identically on Windows), so these run on every platform rather than
// being Unix-only. Each test clears the env vars it touches before asserting so a
// panic can't leak state into later tests; #[serial] additionally prevents these
// from interleaving with each other.
use serial_test::serial;

fn clear_launch_context_env() {
    // SAFETY: tests are #[serial].
    unsafe {
        std::env::remove_var("HCOM_PROCESS_ID");
        std::env::remove_var("KITTY_LISTEN_ON");
        std::env::remove_var("WEZTERM_PANE");
        std::env::remove_var("TMUX_PANE");
        std::env::remove_var("KITTY_WINDOW_ID");
        std::env::remove_var("ZELLIJ_PANE_ID");
        std::env::remove_var("HCOM_LAUNCHED_PRESET");
        std::env::remove_var("HERDR_PANE_ID");
    }
}

#[test]
#[serial]
fn build_early_launch_context_empty_when_no_env_vars_set() {
    clear_launch_context_env();
    let json = build_early_launch_context();
    clear_launch_context_env();
    assert_eq!(json, "{}");
}

#[test]
#[serial]
fn build_early_launch_context_captures_kitty_listen_on() {
    clear_launch_context_env();
    // SAFETY: test is #[serial].
    unsafe {
        std::env::set_var("KITTY_LISTEN_ON", "unix:/tmp/kitty.sock");
    }
    let json = build_early_launch_context();
    clear_launch_context_env();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["kitty_listen_on"], "unix:/tmp/kitty.sock");
    assert!(parsed.get("pane_id").is_none());
}

#[test]
#[serial]
fn build_early_launch_context_prefers_first_pane_id_var_in_priority_order() {
    clear_launch_context_env();
    // SAFETY: test is #[serial].
    unsafe {
        std::env::set_var("WEZTERM_PANE", "wezterm-pane");
        std::env::set_var("TMUX_PANE", "tmux-pane");
    }
    let json = build_early_launch_context();
    clear_launch_context_env();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["pane_id"], "wezterm-pane");
}

#[test]
#[serial]
fn build_early_launch_context_ignores_multiplexer_only_vars_when_absent() {
    // TMUX_PANE/ZELLIJ_PANE_ID never being set on Windows must degrade to
    // simply absent fields, not an error — same as on Unix outside a
    // multiplexer. This is the "no platform branching needed" behavior.
    clear_launch_context_env();
    // SAFETY: test is #[serial].
    unsafe {
        std::env::set_var("KITTY_WINDOW_ID", "kitty-window-1");
    }
    let json = build_early_launch_context();
    clear_launch_context_env();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["pane_id"], "kitty-window-1");
}

#[test]
#[serial]
fn build_early_launch_context_preset_pane_id_env_wins_over_generic_vars() {
    clear_launch_context_env();
    // SAFETY: test is #[serial].
    unsafe {
        std::env::set_var("HCOM_LAUNCHED_PRESET", "herdr");
        std::env::set_var("HERDR_PANE_ID", "w2:p1A");
        std::env::set_var("WEZTERM_PANE", "0");
    }
    let json = build_early_launch_context();
    clear_launch_context_env();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["pane_id"], "w2:p1A");
}

#[test]
#[serial]
fn build_early_launch_context_known_preset_rejects_foreign_fallback() {
    clear_launch_context_env();
    // SAFETY: test is #[serial].
    unsafe {
        std::env::set_var("HCOM_LAUNCHED_PRESET", "herdr");
        std::env::set_var("WEZTERM_PANE", "4");
    }
    let json = build_early_launch_context();
    clear_launch_context_env();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("pane_id").is_none());
}
