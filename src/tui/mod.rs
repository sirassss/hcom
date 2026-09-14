pub mod actions;
pub mod app;
pub mod commands;
pub mod data;
pub mod db;
pub mod filter;
pub mod inline;
pub mod input;
pub mod model;
pub mod render;
pub mod rpc;
pub mod rpc_async;
pub mod state;
pub mod status;
pub mod theme;

use std::io::{Write, stdout};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

use self::app::App;
use self::inline::eject::ReplayReason;
use self::model::ViewMode;

/// Clear the inline viewport area and reset scroll region.
/// Uses current terminal size — only valid when terminal hasn't been resized
/// since the viewport was created (viewport switches, quit).
fn clear_viewport_area(viewport_height: u16) -> Result<()> {
    let mut out = stdout();
    let (_, rows) = crossterm::terminal::size()?;
    let viewport_start = rows.saturating_sub(viewport_height);
    write!(out, "\x1b[r")?;
    write!(out, "\x1b[{};1H", viewport_start + 1)?;
    write!(out, "\x1b[J")?;
    out.flush()?;
    Ok(())
}

/// Clean up after a resize: clear the entire visible screen so replay doesn't
/// duplicate previously ejected content. Content already in the terminal's
/// scrollback buffer (scrolled off-screen) is unreachable without \x1b[3J and
/// left as-is to preserve pre-TUI shell history.
fn clear_for_resize(viewport_height: u16) -> Result<()> {
    let mut out = stdout();
    let (_, new_rows) = crossterm::terminal::size()?;
    let new_start = new_rows.saturating_sub(viewport_height);

    write!(out, "\x1b[r")?; // reset scroll region
    write!(out, "\x1b[3J")?; // clear scrollback buffer
    // Clear only the area above the inline viewport. Avoid a full-screen visible
    // wipe, which causes an obvious flash before replay redraws.
    for row in 1..=new_start {
        write!(out, "\x1b[{};1H\x1b[2K", row)?;
    }
    write!(out, "\x1b[{};1H", new_start + 1)?; // park at new viewport start
    out.flush()?;
    Ok(())
}

fn prepare_vertical_viewport(app: &mut App) {
    app.ui.switch_viewport = false;
    app.ui.view_mode = ViewMode::Vertical;
    app.source.set_timeline_limit(5000);
    app.reload_data_force();
    app.ui.msg_scroll = 0;
}

fn prepare_inline_viewport(app: &mut App) {
    app.ui.switch_viewport = false;
    app.ui.view_mode = ViewMode::Inline;
    app.source.set_timeline_limit(200);
    app.reload_data_force();
    app.ui.msg_scroll = 0;
    // A different tier/filter may have been selected in vertical mode. Replace
    // any queued inline snapshot using the freshly loaded inline window.
    app.ui.trigger_inline_replay();
}

/// Main event loop. Separated from run() so cleanup always runs regardless
/// of errors. `in_alt_screen` tracks whether we entered the alternate screen
/// so the caller can tear it down.
fn run_app(app: &mut App, viewport_height: u16, in_alt_screen: &mut bool) -> Result<()> {
    loop {
        match app.ui.view_mode {
            ViewMode::Inline => {
                *in_alt_screen = false;
                let backend = CrosstermBackend::new(stdout());
                let mut terminal = match Terminal::with_options(
                    backend,
                    TerminalOptions {
                        viewport: Viewport::Inline(viewport_height),
                    },
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "failed to initialize inline viewport: {}\n\
                             Try running in a terminal with full ANSI support.",
                            e
                        ));
                    }
                };

                app.run(&mut terminal, viewport_height)?;
                drop(terminal);

                if app.ui.should_quit {
                    break;
                }

                if app.ui.needs_resize {
                    app.ui.needs_resize = false;
                    app.ui.needs_clear_replay = true;
                    app.ejector.begin_replay(
                        &app.data,
                        app.ui.msg_tier,
                        &app.ui.msg_filter,
                        ReplayReason::Resize,
                    );
                    continue;
                }

                // Viewport switch (no resize) — terminal size unchanged.
                clear_viewport_area(viewport_height)?;
                prepare_vertical_viewport(app);
            }

            ViewMode::Vertical => {
                execute!(
                    stdout(),
                    EnterAlternateScreen,
                    EnableMouseCapture,
                    EnableBracketedPaste
                )?;
                *in_alt_screen = true;

                let backend = CrosstermBackend::new(stdout());
                let mut terminal = Terminal::new(backend)?;

                app.run(&mut terminal, viewport_height)?;

                drop(terminal);
                execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
                *in_alt_screen = false;

                if app.ui.should_quit {
                    break;
                }

                // After leaving alt screen, the main buffer is restored.
                // Re-enable bracketed paste — some terminals lose it on alt screen exit.
                execute!(stdout(), EnableBracketedPaste)?;
                // Clear the viewport area so the new inline terminal starts clean.
                clear_viewport_area(viewport_height)?;
                prepare_inline_viewport(app);
            }
        }
    }
    Ok(())
}

/// Inner TUI lifecycle — everything after raw mode is enabled.
fn run_inner(viewport_height: u16) -> Result<()> {
    execute!(stdout(), EnableBracketedPaste)?;

    // Save current title (push stack) and set hcom title
    {
        let mut out = stdout();
        let _ = write!(out, "\x1b[22;0t\x1b]0;hcom\x07");
        let _ = out.flush();
    }

    let mut app = App::new();

    // Auto-spawn relay-worker if relay is configured
    crate::relay::worker::ensure_worker(true);

    // HCOM_TUI_FULLSCREEN=1 starts directly in alternate screen (fullscreen) mode,
    // bypassing inline viewport which requires cursor position queries.
    if std::env::var("HCOM_TUI_FULLSCREEN").as_deref() == Ok("1") {
        prepare_vertical_viewport(&mut app);
    }

    let mut in_alt_screen = false;

    let result = run_app(&mut app, viewport_height, &mut in_alt_screen);

    if in_alt_screen {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
    let _ = execute!(stdout(), DisableBracketedPaste);

    // Restore terminal title (pop stack, or reset to empty)
    {
        let mut out = stdout();
        let _ = write!(out, "\x1b[23;0t"); // xterm pop title
        let _ = out.flush();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::DataSource;
    use crate::tui::model::Agent;
    use crate::tui::state::DataState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::cell::Cell;
    use std::rc::Rc;

    struct DummySource {
        snapshot: DataState,
        timeline_limit: Rc<Cell<usize>>,
    }

    impl DummySource {
        fn new(snapshot: DataState, timeline_limit: Rc<Cell<usize>>) -> Self {
            Self {
                snapshot,
                timeline_limit,
            }
        }
    }

    impl DataSource for DummySource {
        fn load(&mut self) -> DataState {
            let mut snapshot = self.snapshot.clone();
            snapshot.timeline_limit = self.timeline_limit.get();
            snapshot
        }

        fn load_if_changed(&mut self) -> Option<DataState> {
            Some(self.snapshot.clone())
        }

        fn load_all_stopped(&mut self) -> Vec<Agent> {
            self.snapshot.stopped_agents.clone()
        }

        fn set_timeline_limit(&mut self, limit: usize) {
            self.timeline_limit.set(limit);
        }
    }

    fn make_test_app(limit: Rc<Cell<usize>>) -> App {
        crate::config::Config::init();
        let mut app = App::new();
        app.rpc_client = None;
        let data = DataState::empty();
        app.data = data.clone();
        app.source = Box::new(DummySource::new(data, limit));
        app
    }

    fn render_once(app: &mut App) {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render::render(frame, app)).unwrap();
    }

    #[test]
    fn inline_transition_sets_vertical_mode_and_limit() {
        let limit = Rc::new(Cell::new(0));
        let mut app = make_test_app(limit.clone());
        app.ui.view_mode = ViewMode::Inline;
        app.ui.switch_viewport = true;

        render_once(&mut app);
        prepare_vertical_viewport(&mut app);

        assert_eq!(app.ui.view_mode, ViewMode::Vertical);
        assert_eq!(app.ui.msg_scroll, 0);
        assert!(!app.ui.switch_viewport);
        assert_eq!(limit.get(), 5000);
        assert_eq!(app.data.timeline_limit, 5000);
    }

    #[test]
    fn vertical_transition_resets_inline_state() {
        let limit = Rc::new(Cell::new(0));
        let mut app = make_test_app(limit.clone());
        app.ui.view_mode = ViewMode::Vertical;
        app.ui.msg_scroll = 7;
        app.ui.switch_viewport = true;
        app.ui.msg_tier = crate::tui::filter::MsgTier::Verbose;
        app.ui.msg_filter = crate::tui::filter::MsgFilter::parse("from:nova");

        render_once(&mut app);
        prepare_inline_viewport(&mut app);

        assert_eq!(app.ui.view_mode, ViewMode::Inline);
        assert_eq!(app.ui.msg_scroll, 0);
        assert!(!app.ui.switch_viewport);
        assert_eq!(limit.get(), 200);
        assert_eq!(app.data.timeline_limit, 200);
        assert_eq!(app.ui.msg_tier, crate::tui::filter::MsgTier::Verbose);
        assert_eq!(app.ui.msg_filter.from.as_deref(), Some("nova"));
        assert!(
            app.ui.inline_filter_changed,
            "replace the old inline replay"
        );
        assert!(!app.ui.needs_resize);
        assert!(!app.ui.needs_clear_replay);
    }

    fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render::render(frame, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn vertical_header_and_footer_show_scope_tier_and_chips() {
        let limit = Rc::new(Cell::new(0));
        let mut app = make_test_app(limit);
        app.data.agents = vec![crate::tui::test_helpers::make_test_agent("nova", 60.0)];
        app.ui.view_mode = ViewMode::Vertical;
        app.ui.msg_tier = crate::tui::filter::MsgTier::Normal;
        app.ui.msg_filter = crate::tui::filter::MsgFilter::parse("tag:review to:bigboss");

        let out = render_to_string(&mut app, 120, 24);
        assert!(out.contains("recent (limit"), "scope missing:\n{out}");
        assert!(out.contains("normal"), "tier chip missing:\n{out}");
        assert!(out.contains("tag:review"), "tag chip missing:\n{out}");
        assert!(out.contains("to:bigboss"), "to chip missing:\n{out}");
        // Footer advertises the v tier shortcut.
        assert!(
            out.contains("tier:normal"),
            "footer tier hint missing:\n{out}"
        );
    }

    #[test]
    fn inline_status_keeps_count_visible_with_long_unicode_filter() {
        let mut app = make_test_app(Rc::new(Cell::new(0)));
        app.ui.view_mode = ViewMode::Inline;
        app.data.agents = vec![crate::tui::test_helpers::make_test_agent("nova", 60.0)];
        app.ui.msg_filter.text = "界".repeat(80);

        for width in [60, 80] {
            let out = render_to_string(&mut app, width, 24);
            assert!(out.contains("[0/0]"), "count clipped at {width}:\n{out}");
        }
    }

    #[test]
    fn help_popup_documents_new_filter_keys() {
        let limit = Rc::new(Cell::new(0));
        let mut app = make_test_app(limit);
        app.data.agents = vec![crate::tui::test_helpers::make_test_agent("nova", 60.0)];
        app.ui.help_open = true;

        let out = render_to_string(&mut app, 80, 40);
        assert!(out.contains("compact / normal / verbose"), "\n{out}");
        assert!(out.contains("to:<bigboss>"), "\n{out}");
        assert!(out.contains("recent window"), "\n{out}");
    }
}

#[cfg(test)]
pub mod test_helpers {
    use crate::tui::model::{Agent, AgentStatus, Tool, epoch_now};

    /// Create a test Agent with sensible defaults. `age_secs` controls how old the agent appears.
    pub fn make_test_agent(name: &str, age_secs: f64) -> Agent {
        let now = epoch_now();
        Agent {
            name: name.into(),
            tool: Tool::Claude,
            status: AgentStatus::Active,
            status_context: "working".into(),
            status_detail: String::new(),
            created_at: now - age_secs,
            status_time: now,
            last_heartbeat: now,
            has_tcp: true,
            directory: "/tmp".into(),
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
}

/// Restore terminal state — called from both normal cleanup and the panic hook.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    let _ = execute!(stdout(), DisableBracketedPaste);
    // Restore title (pop stack)
    let mut out = stdout();
    let _ = write!(out, "\x1b[23;0t");
    let _ = out.flush();
}

/// Entry point for the Rust TUI. Called from main.rs (default when no args).
pub fn run() -> Result<()> {
    // Panic hook is installed below (restore_terminal + original_hook).

    // Install panic hook that restores terminal state before printing panic info.
    // Without this, a panic leaves the terminal in raw mode and unusable.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let viewport_height = std::env::var("HCOM_INLINE_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(13u16);

    enable_raw_mode()?;
    let result = run_inner(viewport_height);
    let _ = disable_raw_mode();
    println!();

    result
}
