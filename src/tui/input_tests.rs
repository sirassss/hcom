use super::*;
use crate::tui::app::App;
use crate::tui::model::Agent;

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(code, KeyModifiers::NONE);
}

fn ctrl(app: &mut App, c: char) {
    app.handle_key(KeyCode::Char(c), KeyModifiers::CONTROL);
}

fn make_agent(name: &str) -> Agent {
    crate::tui::test_helpers::make_test_agent(name, 60.0)
}

fn test_app() -> App {
    crate::config::Config::init();
    let mut app = App::new();
    app.rpc_client = None;
    app.data.agents = vec![make_agent("nova"), make_agent("luna")];
    app.data.remote_agents.clear();
    app.data.stopped_agents.clear();
    app.data.orphans.clear();
    app.ui.mode = InputMode::Navigate;
    app.ui.overlay = None;
    app.ui.msg_filter = crate::tui::filter::MsgFilter::default();
    app.ui.msg_tier = crate::tui::filter::MsgTier::default();
    app.ui.input.clear();
    app.ui.input_cursor = 0;
    app.ui.cursor = 0;
    app
}

fn flash_text(app: &App) -> String {
    app.ui
        .flash
        .as_ref()
        .map(|f| f.text.clone())
        .unwrap_or_default()
}

#[test]
fn search_overlay_enter_commits_parsed_filter_and_replay_flags() {
    let mut app = test_app();
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('n'));
    key(&mut app, KeyCode::Char('o'));
    key(&mut app, KeyCode::Enter);

    assert_eq!(app.ui.mode, InputMode::Navigate);
    assert!(app.ui.overlay.is_none());
    assert_eq!(app.ui.msg_filter.text, "no");
    assert!(app.ui.inline_filter_changed);
    assert!(!app.ui.needs_clear_replay);
    assert!(
        !app.ui.needs_resize,
        "filter replay must preserve scrollback"
    );
}

#[test]
fn search_overlay_prefills_committed_query_and_preserves_agents_on_commit() {
    let mut app = test_app();
    app.ui.msg_filter = crate::tui::filter::MsgFilter::parse("from:ligo hi");
    app.ui.msg_filter.agents.insert("nova".into());

    key(&mut app, KeyCode::Char('/'));
    assert_eq!(app.ui.overlay.as_ref().unwrap().input, "from:ligo hi");

    // Commit unchanged: structured field + text survive, and so does the
    // roster selection that was never in the overlay string.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.ui.msg_filter.from.as_deref(), Some("ligo"));
    assert_eq!(app.ui.msg_filter.text, "hi");
    assert!(app.ui.msg_filter.agents.contains("nova"));
}

#[test]
fn search_overlay_escape_keeps_committed_filter() {
    let mut app = test_app();
    app.ui.msg_filter = crate::tui::filter::MsgFilter::parse("keep me");

    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('x')); // draft edit
    key(&mut app, KeyCode::Esc);

    // Draft discarded; the committed filter is untouched (changed semantics).
    assert!(app.ui.overlay.is_none());
    assert_eq!(app.ui.msg_filter.text, "keep me");
}

#[test]
fn v_cycles_detail_tier_and_flags_replay() {
    use crate::tui::filter::MsgTier;
    let mut app = test_app();
    assert_eq!(app.ui.msg_tier, MsgTier::Compact);
    key(&mut app, KeyCode::Char('v'));
    assert_eq!(app.ui.msg_tier, MsgTier::Normal);
    assert!(app.ui.inline_filter_changed || app.ui.view_mode != ViewMode::Inline);
    assert!(!app.ui.needs_resize, "tier replay must preserve scrollback");
    key(&mut app, KeyCode::Char('v'));
    key(&mut app, KeyCode::Char('v'));
    assert_eq!(app.ui.msg_tier, MsgTier::Compact);
}

#[test]
fn esc_cascade_clears_text_then_tokens_then_agents_without_touching_tier() {
    use crate::tui::filter::MsgTier;
    let mut app = test_app();
    app.ui.msg_tier = MsgTier::Verbose;
    app.ui.msg_filter = crate::tui::filter::MsgFilter::parse("tag:review free text");
    app.ui.msg_filter.agents.insert("nova".into());

    key(&mut app, KeyCode::Esc); // stage 1: free text
    assert_eq!(app.ui.msg_filter.text, "");
    assert_eq!(app.ui.msg_filter.tag.as_deref(), Some("review"));

    key(&mut app, KeyCode::Esc); // stage 2: structured tokens
    assert_eq!(app.ui.msg_filter.tag, None);
    assert!(app.ui.msg_filter.agents.contains("nova"));

    key(&mut app, KeyCode::Esc); // stage 3: roster selection
    assert!(app.ui.msg_filter.agents.is_empty());

    assert_eq!(app.ui.msg_tier, MsgTier::Verbose); // never cleared
}

#[test]
fn a_adds_all_local_agents_to_the_filter() {
    let mut app = test_app();
    key(&mut app, KeyCode::Char('a'));
    assert!(app.ui.msg_filter.agents.contains("nova"));
    assert!(app.ui.msg_filter.agents.contains("luna"));
}

#[test]
fn shift_b_toggles_the_coordinator_and_preserves_other_conditions() {
    let mut app = test_app();
    app.bigboss = "bigboss".into();
    app.ui.msg_filter = crate::tui::filter::MsgFilter::parse("tag:review thread:t from:ligo hi");
    app.ui.msg_filter.agents.insert("nova".into());

    key(&mut app, KeyCode::Char('B'));
    assert_eq!(app.ui.msg_filter.to.as_deref(), Some("bigboss"));
    // other conditions untouched
    assert_eq!(app.ui.msg_filter.tag.as_deref(), Some("review"));
    assert_eq!(app.ui.msg_filter.thread.as_deref(), Some("t"));
    assert_eq!(app.ui.msg_filter.from.as_deref(), Some("ligo"));
    assert_eq!(app.ui.msg_filter.text, "hi");
    assert!(app.ui.msg_filter.agents.contains("nova"));

    // second press removes it (identifies the same coordinator)
    key(&mut app, KeyCode::Char('B'));
    assert_eq!(app.ui.msg_filter.to, None);
}

#[test]
fn shift_b_replaces_a_different_to_target() {
    let mut app = test_app();
    app.bigboss = "chief".into();
    app.ui.msg_filter.to = Some("someone-else".into());

    key(&mut app, KeyCode::Char('B'));
    assert_eq!(app.ui.msg_filter.to.as_deref(), Some("chief"));
}

#[test]
fn shift_b_is_literal_in_compose_and_search() {
    let mut app = test_app();
    app.bigboss = "chief".into();

    // Compose: 'B' is text, no filter change.
    key(&mut app, KeyCode::Char('m'));
    key(&mut app, KeyCode::Char('B'));
    assert!(app.ui.input.contains('B'));
    assert_eq!(app.ui.msg_filter.to, None);

    // Search overlay: 'B' edits the draft, no commit.
    let mut app = test_app();
    app.bigboss = "chief".into();
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('B'));
    assert_eq!(app.ui.overlay.as_ref().unwrap().input, "B");
    assert_eq!(app.ui.msg_filter.to, None);
}

#[test]
fn compose_at_without_target_inserts_literal_char() {
    let mut app = test_app();
    app.data.agents.clear();
    app.ui.mode = InputMode::Compose;
    app.ui.cursor = 0;

    key(&mut app, KeyCode::Char('@'));

    assert_eq!(app.ui.input, "@");
    assert_eq!(app.ui.input_cursor, 1);
}

#[test]
fn paste_in_navigate_enters_compose_and_strips_newlines() {
    let mut app = test_app();
    app.handle_paste("hello\nthere\r!");

    assert_eq!(app.ui.mode, InputMode::Compose);
    assert_eq!(app.ui.input, "hellothere!");
    assert_eq!(app.ui.input_cursor, app.ui.input.len());
}

#[test]
fn message_key_prefills_mentions_for_selected_agents() {
    let mut app = test_app();
    app.ui.msg_filter.agents.insert("luna".into());
    app.ui.msg_filter.agents.insert("nova".into());

    key(&mut app, KeyCode::Char('m'));

    assert_eq!(app.ui.mode, InputMode::Compose);
    assert_eq!(app.ui.input, "@luna @nova ");
    assert_eq!(app.ui.input_cursor, app.ui.input.len());
}

#[test]
fn broadcast_key_sets_all_target() {
    let mut app = test_app();

    key(&mut app, KeyCode::Char('b'));

    assert_eq!(app.ui.mode, InputMode::Compose);
    assert_eq!(app.ui.input, "");
}

#[test]
fn kill_key_opens_inline_confirm_and_enter_confirms() {
    let mut app = test_app();

    key(&mut app, KeyCode::Char('k'));

    let confirm = app.ui.confirm.as_ref().expect("confirm");
    assert!(confirm.is_inline_agent_action());
    assert!(matches!(confirm.action, ConfirmAction::KillAgents(_)));
    assert_eq!(confirm.text, "Kill nova?");

    key(&mut app, KeyCode::Enter);

    assert!(app.ui.confirm.is_none());
    assert!(flash_text(&app).contains("Kill failed"));
}

#[test]
fn fork_key_opens_inline_confirm_and_escape_cancels() {
    let mut app = test_app();

    key(&mut app, KeyCode::Char('f'));

    let confirm = app.ui.confirm.as_ref().expect("confirm");
    assert!(confirm.is_inline_agent_action());
    assert!(matches!(confirm.action, ConfirmAction::ForkAgents(_)));
    assert_eq!(confirm.text, "Fork nova?");

    key(&mut app, KeyCode::Esc);

    assert!(app.ui.confirm.is_none());
    assert_eq!(flash_text(&app), "");
}

#[test]
fn fork_key_ignores_tools_without_fork_support() {
    let mut app = test_app();
    app.data.agents[0].tool = Tool::Gemini;

    key(&mut app, KeyCode::Char('f'));

    assert!(app.ui.confirm.is_none());
}

#[test]
fn stopped_agent_uses_resume_not_kill_or_fork() {
    let mut app = test_app();
    app.data.agents.clear();
    let mut stopped = make_agent("nova");
    stopped.status = AgentStatus::Inactive;
    app.data.stopped_agents = vec![stopped];
    app.ui.view_mode = ViewMode::Vertical;
    app.ui.stopped_expanded = true;
    app.ui.cursor = 1; // stopped header is row 0

    key(&mut app, KeyCode::Char('k'));
    assert!(app.ui.confirm.is_none());
    key(&mut app, KeyCode::Char('f'));
    assert!(app.ui.confirm.is_none());

    key(&mut app, KeyCode::Char('r'));
    let confirm = app.ui.confirm.as_ref().expect("resume confirm");
    assert!(matches!(confirm.action, ConfirmAction::ResumeAgents(_)));
    assert_eq!(confirm.text, "Resume nova?");
}

#[test]
fn remote_agent_targets_use_display_name_for_kill_and_tag() {
    let mut app = test_app();
    app.data.agents.clear();
    let mut remote = make_agent("nova");
    remote.device_name = Some("BOXE".into());
    app.data.remote_agents = vec![remote];
    app.ui.remote_expanded = true;
    app.ui.cursor = 1; // remote header is row 0

    assert_eq!(app.resolve_kill_targets(), vec!["nova:BOXE"]);
    assert_eq!(app.resolve_tag_targets(), vec!["nova:BOXE"]);
    assert!(app.resolve_fork_targets().is_empty());
}

#[test]
fn selected_local_agent_does_not_match_remote_with_same_base_name() {
    let mut app = test_app();
    app.data.agents = vec![make_agent("nova")];
    let mut remote = make_agent("nova");
    remote.device_name = Some("BOXE".into());
    app.data.remote_agents = vec![remote];
    app.ui.msg_filter.agents.insert("nova".into());

    assert_eq!(app.resolve_kill_targets(), vec!["nova"]);
    assert_eq!(app.resolve_tag_targets(), vec!["nova"]);
}

#[test]
fn relay_status_from_inline_sets_pending_eject() {
    let mut app = test_app();
    ctrl(&mut app, 'r');
    let popup = app.ui.relay_popup.as_mut().expect("relay popup");
    popup.cursor = 1; // status

    key(&mut app, KeyCode::Enter);

    assert!(app.ui.relay_popup.is_none());
    assert_eq!(app.ui.mode, InputMode::Navigate);
    assert!(app.ui.pending_eject_cmd);
    let cr = app.ui.command_result.as_ref().expect("command result");
    assert_eq!(cr.label, "relay status");
    assert!(
        cr.output[0].contains("rpc client unavailable"),
        "expected rpc unavailable error, got {:?}",
        cr.output
    );
}

#[test]
fn ctrl_w_deletes_word_in_compose() {
    let mut app = test_app();
    app.ui.mode = InputMode::Compose;
    app.ui.input = "hello world foo".into();
    app.ui.input_cursor = app.ui.input.len();

    ctrl(&mut app, 'w');
    assert_eq!(app.ui.input, "hello world ");

    ctrl(&mut app, 'w');
    assert_eq!(app.ui.input, "hello ");

    ctrl(&mut app, 'w');
    assert_eq!(app.ui.input, "");
}

#[test]
fn ctrl_u_deletes_to_start_in_compose() {
    let mut app = test_app();
    app.ui.mode = InputMode::Compose;
    app.ui.input = "hello world".into();
    app.ui.input_cursor = 5;

    ctrl(&mut app, 'u');
    assert_eq!(app.ui.input, " world");
    assert_eq!(app.ui.input_cursor, 0);
}

#[test]
fn tab_from_compose_preserves_input() {
    let mut app = test_app();
    app.ui.mode = InputMode::Compose;
    app.ui.input = "my message".into();
    app.ui.input_cursor = app.ui.input.len();

    key(&mut app, KeyCode::Tab);

    assert_eq!(app.ui.mode, InputMode::Launch);
    assert_eq!(app.ui.input, "my message");
}

#[test]
fn altgr_char_not_intercepted_as_ctrl() {
    let mut app = test_app();
    app.ui.mode = InputMode::Compose;

    // AltGr+Q on German keyboard → '@' with Ctrl+Alt modifiers
    app.handle_key(
        KeyCode::Char('@'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );

    // Should NOT be intercepted by any Ctrl handler — should reach compose input
    assert_eq!(app.ui.mode, InputMode::Compose);
    assert_eq!(app.ui.input, "@");
    assert_eq!(app.ui.input_cursor, 1);
}
