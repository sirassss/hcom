use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};

pub(crate) mod compose;

use crate::tui::app::{App, Confirm, ConfirmAction};
use crate::tui::input::compose::parse_outbound_message;
use crate::tui::model::*;
use crate::tui::rpc_async::RpcOp;
use crate::tui::theme::Theme;

impl App {
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // AltGr on non-US keyboards sends Ctrl+Alt — exclude from Ctrl shortcut matching
        // so characters like @ (AltGr+Q on German) and \ (AltGr on many layouts) pass through.
        let ctrl =
            modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT);

        // ── Global shortcuts (always fire) ──
        if ctrl && matches!(code, KeyCode::Char('d') | KeyCode::Char('c')) {
            self.ui.should_quit = true;
            return;
        }

        if self.ui.confirm.is_some() {
            self.handle_confirm(code);
            return;
        }

        if self.ui.help_open {
            match code {
                KeyCode::Up => {
                    self.ui.help_scroll = self.ui.help_scroll.saturating_sub(1);
                }
                KeyCode::Down => {
                    self.ui.help_scroll = self.ui.help_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    self.ui.help_scroll = self.ui.help_scroll.saturating_sub(5);
                }
                KeyCode::PageDown => {
                    self.ui.help_scroll = self.ui.help_scroll.saturating_add(5);
                }
                _ => {
                    self.ui.help_open = false;
                    self.ui.help_scroll = 0;
                }
            }
            return;
        }

        if code == KeyCode::Char('r') && ctrl {
            self.ui.relay_popup = Some(RelayPopupState::new(self.data.relay_enabled));
            self.ui.mode = InputMode::Relay;
            return;
        }

        // Ctrl+A: cursor to start (overlay, compose, or launch field)
        if code == KeyCode::Char('a') && ctrl {
            if self.ui.mode == InputMode::Launch && self.ui.launch.editing.is_some() {
                self.ui.launch.edit_cursor = 0;
            } else if let Some(ref mut overlay) = self.ui.overlay {
                overlay.cursor = 0;
            } else if self.ui.mode == InputMode::Compose {
                self.ui.input_cursor = 0;
            }
            return;
        }

        // Ctrl+E: cursor to end (overlay, compose, or launch field)
        if code == KeyCode::Char('e') && ctrl {
            if self.ui.mode == InputMode::Launch && self.ui.launch.editing.is_some() {
                if let Some(field) = self.ui.launch.editing {
                    self.ui.launch.edit_cursor = self.ui.launch.field_value(field).len();
                }
            } else if let Some(ref mut overlay) = self.ui.overlay {
                overlay.cursor = overlay.input.len();
            } else if self.ui.mode == InputMode::Compose {
                self.ui.input_cursor = self.ui.input.len();
            }
            return;
        }

        // Ctrl+W: delete word back (active text input)
        if code == KeyCode::Char('w') && ctrl {
            if self.ui.mode == InputMode::Launch && self.ui.launch.editing.is_some() {
                self.ui.launch.delete_word();
            } else if let Some(ref mut overlay) = self.ui.overlay {
                delete_word_back(&mut overlay.input, &mut overlay.cursor);
                if overlay.kind == OverlayKind::Search {
                    self.ui.msg_scroll = 0;
                }
                if let Some(ref mut p) = overlay.palette {
                    p.filter(&overlay.input);
                }
            } else if self.ui.mode == InputMode::Compose {
                delete_word_back(&mut self.ui.input, &mut self.ui.input_cursor);
            }
            return;
        }

        // Ctrl+U: delete to start of line (active text input)
        if code == KeyCode::Char('u') && ctrl {
            if self.ui.mode == InputMode::Launch && self.ui.launch.editing.is_some() {
                self.ui.launch.delete_to_start();
            } else if let Some(ref mut overlay) = self.ui.overlay {
                delete_to_start(&mut overlay.input, &mut overlay.cursor);
                if overlay.kind == OverlayKind::Search {
                    self.ui.msg_scroll = 0;
                }
                if let Some(ref mut p) = overlay.palette {
                    p.filter(&overlay.input);
                }
            } else if self.ui.mode == InputMode::Compose {
                delete_to_start(&mut self.ui.input, &mut self.ui.input_cursor);
            }
            return;
        }

        if code == KeyCode::Char('s') && ctrl {
            self.ui.show_all_stopped = !self.ui.show_all_stopped;
            if self.ui.show_all_stopped {
                if self.ui.view_mode == ViewMode::Inline {
                    self.ui.switch_viewport = true;
                }
                self.ui.stopped_expanded = true;
                self.data.stopped_agents = self.source.load_all_stopped();
                self.ui.flash = Some(Flash::new(
                    "Showing all stopped".into(),
                    Theme::flash_info(),
                ));
            } else {
                self.reload_data_force();
                self.ui.flash = Some(Flash::new(
                    "Showing recent stopped".into(),
                    Theme::flash_info(),
                ));
            }
            return;
        }

        // Ctrl+O in Launch mode: open config
        if code == KeyCode::Char('o') && ctrl && self.ui.mode == InputMode::Launch {
            let cmd = "config".to_string();
            let is_inline = self.ui.view_mode == ViewMode::Inline;
            if let Err(e) = self.enqueue_rpc(RpcOp::Command { cmd: cmd.clone() }) {
                self.ui.command_result = Some(CommandResult {
                    label: cmd,
                    output: vec![format!("Error: {}", e)],
                });
                if is_inline {
                    self.ui.pending_eject_cmd = true;
                } else {
                    self.ui.mode = InputMode::CommandOutput;
                }
            } else {
                self.ui.command_result = Some(CommandResult {
                    label: cmd,
                    output: vec!["(running...)".into()],
                });
                if is_inline {
                    self.ui.flash =
                        Some(Flash::new("Running config...".into(), Theme::flash_info()));
                } else {
                    self.ui.mode = InputMode::CommandOutput;
                }
            }
            self.ui.launch = LaunchState::new();
            self.ui.msg_scroll = 0;
            return;
        }

        // ── Mode dispatch ──
        match self.ui.mode {
            InputMode::Relay => self.handle_relay(code),
            InputMode::CommandOutput => self.handle_command_output(code),
            InputMode::Launch => self.handle_launch_inline(code),
            InputMode::Compose => self.handle_compose(code),
            InputMode::Navigate => self.handle_navigate(code),
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        let clean: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        if self.ui.mode == InputMode::Relay {
            if let Some(ref mut popup) = self.ui.relay_popup
                && popup.editing_token
            {
                for c in clean.chars() {
                    insert_at(&mut popup.token_input, &mut popup.token_cursor, c);
                }
            }
        } else if self.ui.mode == InputMode::Launch && self.ui.launch.editing.is_some() {
            for c in clean.chars() {
                self.ui.launch.insert_char(c);
            }
        } else if self.ui.mode == InputMode::Navigate {
            if let Some(ref mut overlay) = self.ui.overlay {
                // Paste into active overlay
                for c in clean.chars() {
                    insert_at(&mut overlay.input, &mut overlay.cursor, c);
                }
                if overlay.kind == OverlayKind::Search {
                    self.ui.msg_scroll = 0;
                }
                if let Some(ref mut p) = overlay.palette {
                    p.filter(&overlay.input);
                }
            } else {
                // Navigate with no overlay → enter Compose and paste
                self.ui.mode = InputMode::Compose;
                for c in clean.chars() {
                    insert_at(&mut self.ui.input, &mut self.ui.input_cursor, c);
                }
            }
        } else if self.ui.mode == InputMode::Compose {
            for c in clean.chars() {
                insert_at(&mut self.ui.input, &mut self.ui.input_cursor, c);
            }
        }
    }

    pub fn handle_mouse(&mut self, kind: MouseEventKind) {
        if self.ui.help_open {
            match kind {
                MouseEventKind::ScrollUp => {
                    self.ui.help_scroll = self.ui.help_scroll.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    self.ui.help_scroll = self.ui.help_scroll.saturating_add(3);
                }
                _ => {}
            }
            return;
        }
        if self.ui.mode == InputMode::Launch && self.ui.launch.options_cursor.is_some() {
            return;
        }
        match kind {
            MouseEventKind::ScrollUp => {
                self.ui.msg_scroll = self.ui.msg_scroll.saturating_add(3);
            }
            MouseEventKind::ScrollDown => {
                self.ui.msg_scroll = self.ui.msg_scroll.saturating_sub(3);
            }
            _ => {}
        }
    }

    fn handle_confirm(&mut self, code: KeyCode) {
        if self
            .ui
            .confirm
            .as_ref()
            .is_some_and(Confirm::is_inline_agent_action)
        {
            match code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(confirm) = self.ui.confirm.take() {
                        self.execute_confirm(confirm.action);
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.ui.confirm = None;
                }
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Left | KeyCode::Right => {
                if let Some(ref mut c) = self.ui.confirm {
                    c.selected = !c.selected;
                }
            }
            KeyCode::Enter => {
                let confirm = self.ui.confirm.take().unwrap();
                match confirm.action {
                    ConfirmAction::OrphanAction(pid) => {
                        if confirm.selected {
                            self.recover_orphan(pid);
                        } else {
                            self.execute_confirm(ConfirmAction::KillOrphan(pid));
                        }
                    }
                    _ => {
                        if confirm.selected {
                            self.execute_confirm(confirm.action);
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.ui.confirm = None;
            }
            KeyCode::Char('n')
                if !matches!(
                    self.ui.confirm.as_ref().map(|c| &c.action),
                    Some(ConfirmAction::OrphanAction(_))
                ) =>
            {
                // n = "No" shortcut, not applicable to OrphanAction (Kill/Recover)
                self.ui.confirm = None;
            }
            KeyCode::Char('y') => {
                // y = "Yes" shortcut, not applicable to OrphanAction
                if let Some(confirm) = self.ui.confirm.as_ref()
                    && !matches!(confirm.action, ConfirmAction::OrphanAction(_))
                {
                    let confirm = self.ui.confirm.take().unwrap();
                    self.execute_confirm(confirm.action);
                }
            }
            _ => {}
        }
    }

    // ── Navigate mode ─────────────────────────────────────────────

    fn handle_navigate(&mut self, code: KeyCode) {
        // If overlay is active, give it first crack at text-input keys
        if self.ui.overlay.is_some() && self.handle_overlay_key(code) {
            return;
        }

        match code {
            // MOVEMENT — always work, no conditions
            KeyCode::Up if self.ui.cursor > 0 => {
                self.ui.cursor -= 1;
            }
            KeyCode::Down if self.ui.cursor + 1 < self.total_visible_rows() => {
                self.ui.cursor += 1;
            }
            KeyCode::Left if self.ui.view_mode == ViewMode::Inline && self.ui.cursor > 0 => {
                self.ui.cursor -= 1;
            }
            KeyCode::Right
                if self.ui.view_mode == ViewMode::Inline
                    && self.ui.cursor + 1 < self.total_visible_rows() =>
            {
                self.ui.cursor += 1;
            }
            KeyCode::Home => {
                if self.ui.view_mode == ViewMode::Inline {
                    self.ui.cursor = 0;
                } else {
                    self.ui.msg_scroll = self.ui.scroll_max;
                }
            }
            KeyCode::End => {
                if self.ui.view_mode == ViewMode::Inline {
                    let total = self.total_visible_rows();
                    if total > 0 {
                        self.ui.cursor = total - 1;
                    }
                } else {
                    self.ui.msg_scroll = 0;
                }
            }
            KeyCode::PageUp => {
                self.ui.msg_scroll = self.ui.msg_scroll.saturating_add(5);
            }
            KeyCode::PageDown => {
                self.ui.msg_scroll = self.ui.msg_scroll.saturating_sub(5);
            }

            // DETAIL TIER
            KeyCode::Char('v') => {
                self.ui.msg_tier = self.ui.msg_tier.next();
                self.ui.msg_scroll = 0;
                self.ui.trigger_inline_replay();
            }

            // SELECTION — folds into msg_filter.agents; a set change replays
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.toggle_select_at_cursor() {
                    self.ui.msg_scroll = 0;
                    self.ui.trigger_inline_replay();
                }
            }
            KeyCode::Char('a') => {
                let before = self.ui.msg_filter.agents.len();
                for agent in &self.data.agents {
                    self.ui.msg_filter.agents.insert(agent.name.clone());
                }
                if self.ui.msg_filter.agents.len() != before {
                    self.ui.msg_scroll = 0;
                    self.ui.trigger_inline_replay();
                }
            }
            KeyCode::Esc => {
                // Cascade: overlay → free text → structured tokens → roster.
                if self.ui.overlay.is_some() {
                    // Shouldn't reach here (overlay handles its own Esc) but be safe
                    self.cancel_overlay();
                } else if self.clear_filter_stage() {
                    self.ui.msg_scroll = 0;
                    self.ui.trigger_inline_replay();
                }
            }

            // ACTIONS — operate on resolve_targets() (selection or cursor)
            KeyCode::Char('k') => {
                let names = self.resolve_kill_targets();
                if !names.is_empty() {
                    let text = if names.len() == 1 {
                        format!("Kill {}?", names[0])
                    } else {
                        format!("Kill {} agents?", names.len())
                    };
                    self.ui.confirm =
                        Some(Confirm::new(text, ConfirmAction::KillAgents(names), false));
                }
            }
            KeyCode::Char('f') => {
                let names = self.resolve_fork_targets();
                if !names.is_empty() {
                    let text = if names.len() == 1 {
                        format!("Fork {}?", names[0])
                    } else {
                        format!("Fork {} agents?", names.len())
                    };
                    self.ui.confirm =
                        Some(Confirm::new(text, ConfirmAction::ForkAgents(names), true));
                }
            }
            KeyCode::Char('r') => {
                let names = self.resolve_resume_targets();
                if !names.is_empty() {
                    let text = if names.len() == 1 {
                        format!("Resume {}?", names[0])
                    } else {
                        format!("Resume {} agents?", names.len())
                    };
                    self.ui.confirm =
                        Some(Confirm::new(text, ConfirmAction::ResumeAgents(names), true));
                }
            }
            KeyCode::Char('t') => {
                let names = self.resolve_tag_targets();
                if !names.is_empty() {
                    // Pre-fill with common tag if all targets share one
                    let tags: std::collections::HashSet<&str> = names
                        .iter()
                        .filter_map(|n| self.find_agent_by_action_name(n))
                        .map(|a| a.tag.as_str())
                        .collect();
                    let common_tag = if tags.len() == 1 {
                        tags.into_iter().next().unwrap().to_string()
                    } else {
                        String::new()
                    };
                    self.ui.overlay = Some(Overlay::with(OverlayKind::Tag, names, common_tag));
                }
            }

            // COMPOSE ENTRY
            KeyCode::Char('m') => {
                // Message: pre-fill @mentions for selected or cursor agent
                self.ui.mode = InputMode::Compose;
                self.ui.input.clear();
                self.ui.input_cursor = 0;
                if !self.ui.msg_filter.agents.is_empty() {
                    for name in self.ui.msg_filter.agents.iter() {
                        self.ui.input.push_str(&format!("@{} ", name));
                    }
                } else if let Some(name) = self.cursor_display_name() {
                    self.ui.input = format!("@{} ", name);
                }
                self.ui.input_cursor = self.ui.input.len();
            }
            KeyCode::Char('b') => {
                // Broadcast: compose with no @mentions (sends to all)
                self.ui.mode = InputMode::Compose;
                self.ui.input.clear();
                self.ui.input_cursor = 0;
            }
            KeyCode::Char('B') => {
                // Toggle `to:<coordinator>`: drop it when the current `to:`
                // already names the coordinator (shared identity rules),
                // otherwise set/replace it. Other conditions survive.
                let coord = self.bigboss.clone();
                let already = self
                    .ui
                    .msg_filter
                    .to
                    .as_deref()
                    .is_some_and(|t| self.same_agent(t, &coord));
                self.ui.msg_filter.to = if already { None } else { Some(coord) };
                self.ui.msg_scroll = 0;
                self.ui.trigger_inline_replay();
            }

            // OVERLAYS
            KeyCode::Char('/') => {
                // Prefill with the committed query; edits are a draft until Enter.
                let mut ov = Overlay::new(OverlayKind::Search);
                ov.input = self.ui.msg_filter.to_query();
                ov.cursor = ov.input.len();
                self.ui.overlay = Some(ov);
                self.ui.msg_scroll = 0;
            }
            KeyCode::Char('!') => {
                let suggestions = self.build_command_suggestions();
                let palette = CommandPalette::new(suggestions);
                self.ui.overlay = Some(Overlay::command_with_palette(palette));
            }

            // VIEWS
            KeyCode::Tab => {
                self.ui.mode = InputMode::Launch;
                self.ui.launch.options_cursor = None;
                self.ui.msg_scroll = 0;
            }
            KeyCode::Char('\\') => {
                self.ui.switch_viewport = true;
            }
            // INFO
            KeyCode::Char('?') => {
                self.ui.help_open = true;
            }

            _ => {}
        }
    }

    /// Act on the current cursor target: agent rows toggle the roster filter,
    /// group headers expand/collapse, an orphan row opens its chooser. Returns
    /// `true` only when the `msg_filter.agents` set actually changed.
    fn toggle_select_at_cursor(&mut self) -> bool {
        let name = match self.cursor_target() {
            CursorTarget::Agent(idx) => self.data.agents[idx].name.clone(),
            CursorTarget::RemoteAgent(idx) => self.data.remote_agents[idx].display_name(),
            CursorTarget::StoppedAgent(idx) => self.data.stopped_agents[idx].name.clone(),
            CursorTarget::RemoteHeader => {
                self.ui.remote_expanded = !self.ui.remote_expanded;
                return false;
            }
            CursorTarget::StoppedHeader => {
                self.ui.stopped_expanded = !self.ui.stopped_expanded;
                return false;
            }
            CursorTarget::OrphanHeader => {
                self.ui.orphans_expanded = !self.ui.orphans_expanded;
                return false;
            }
            CursorTarget::Orphan(idx) => {
                let pid = self.data.orphans[idx].pid;
                self.ui.confirm = Some(Confirm::new(
                    format!("PID {}", pid),
                    ConfirmAction::OrphanAction(pid),
                    true, // default to Recover (right)
                ));
                return false;
            }
            CursorTarget::None => return false,
        };
        if !self.ui.msg_filter.agents.remove(&name) {
            self.ui.msg_filter.agents.insert(name);
        }
        self.ui.msg_scroll = 0;
        true
    }

    /// Clear the first non-empty filter stage (free text → structured tokens →
    /// roster selection), skipping empty stages. Returns whether anything
    /// changed. The tier is never touched.
    fn clear_filter_stage(&mut self) -> bool {
        let f = &mut self.ui.msg_filter;
        if !f.text.is_empty() {
            f.text.clear();
            true
        } else if f.has_tokens() {
            f.tag = None;
            f.thread = None;
            f.to = None;
            f.from = None;
            true
        } else if !f.agents.is_empty() {
            f.agents.clear();
            true
        } else {
            false
        }
    }

    /// Display name of the agent at cursor (local, remote, or stopped). None for headers/orphans.
    fn cursor_display_name(&self) -> Option<String> {
        match self.cursor_target() {
            CursorTarget::Agent(idx) => Some(self.data.agents[idx].display_name()),
            CursorTarget::RemoteAgent(idx) => Some(self.data.remote_agents[idx].display_name()),
            CursorTarget::StoppedAgent(idx) => Some(self.data.stopped_agents[idx].display_name()),
            _ => None,
        }
    }

    /// Resolve selectable target names by action capability.
    fn resolve_action_targets(&self, can_apply: impl Fn(&Agent) -> bool) -> Vec<String> {
        if !self.ui.msg_filter.agents.is_empty() {
            return self
                .all_agents()
                .filter(|agent| {
                    can_apply(agent) && self.ui.msg_filter.agents.contains(&agent.action_name())
                })
                .map(Agent::action_name)
                .collect();
        }

        self.cursor_agent()
            .filter(|agent| can_apply(agent))
            .map(|agent| vec![agent.action_name()])
            .unwrap_or_default()
    }

    fn resolve_kill_targets(&self) -> Vec<String> {
        self.resolve_action_targets(Agent::can_kill)
    }

    fn resolve_fork_targets(&self) -> Vec<String> {
        self.resolve_action_targets(Agent::can_fork_from_tui)
    }

    fn resolve_resume_targets(&self) -> Vec<String> {
        self.resolve_action_targets(Agent::can_resume)
    }

    fn resolve_tag_targets(&self) -> Vec<String> {
        self.resolve_action_targets(Agent::can_tag)
    }

    fn all_agents(&self) -> impl Iterator<Item = &Agent> {
        self.data
            .agents
            .iter()
            .chain(self.data.remote_agents.iter())
            .chain(self.data.stopped_agents.iter())
    }

    fn cursor_agent(&self) -> Option<&Agent> {
        match self.cursor_target() {
            CursorTarget::Agent(idx) => self.data.agents.get(idx),
            CursorTarget::RemoteAgent(idx) => self.data.remote_agents.get(idx),
            CursorTarget::StoppedAgent(idx) => self.data.stopped_agents.get(idx),
            _ => None,
        }
    }

    fn find_agent_by_action_name(&self, name: &str) -> Option<&Agent> {
        self.all_agents().find(|agent| agent.action_name() == name)
    }

    pub(crate) fn cursor_action_availability(&self) -> ActionAvailability {
        if !self.ui.msg_filter.agents.is_empty() {
            return ActionAvailability {
                kill: !self.resolve_kill_targets().is_empty(),
                fork: !self.resolve_fork_targets().is_empty(),
                resume: !self.resolve_resume_targets().is_empty(),
                tag: !self.resolve_tag_targets().is_empty(),
            };
        }

        match self.cursor_agent() {
            Some(agent) => ActionAvailability {
                kill: agent.can_kill(),
                fork: agent.can_fork_from_tui(),
                resume: agent.can_resume(),
                tag: agent.can_tag(),
            },
            None => ActionAvailability::default(),
        }
    }

    // ── Overlay handling (Search / Command / Tag on Navigate) ─────

    /// Handle a key while an overlay is active. Returns true if consumed.
    fn handle_overlay_key(&mut self, code: KeyCode) -> bool {
        let has_palette = self
            .ui
            .overlay
            .as_ref()
            .is_some_and(|o| o.kind == OverlayKind::Command && o.palette.is_some());

        match code {
            KeyCode::Char(c) => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                insert_at(&mut overlay.input, &mut overlay.cursor, c);
                if overlay.kind == OverlayKind::Search {
                    self.ui.msg_scroll = 0;
                }
                if let Some(ref mut p) = overlay.palette {
                    p.filter(&overlay.input);
                }
                true
            }
            KeyCode::Backspace => {
                let is_empty = self.ui.overlay.as_ref().unwrap().input.is_empty();
                if is_empty {
                    self.cancel_overlay();
                } else {
                    let overlay = self.ui.overlay.as_mut().unwrap();
                    delete_back(&mut overlay.input, &mut overlay.cursor);
                    if overlay.kind == OverlayKind::Search {
                        self.ui.msg_scroll = 0;
                    }
                    if let Some(ref mut p) = overlay.palette {
                        p.filter(&overlay.input);
                    }
                }
                true
            }
            KeyCode::Down if has_palette => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                overlay.palette.as_mut().unwrap().cursor_down();
                true
            }
            KeyCode::Up if has_palette => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                overlay.palette.as_mut().unwrap().cursor_up();
                true
            }
            KeyCode::Tab if has_palette => {
                // Fill input from highlighted suggestion without executing
                let overlay = self.ui.overlay.as_mut().unwrap();
                if let Some(cmd) = overlay
                    .palette
                    .as_ref()
                    .and_then(|p| p.selected())
                    .map(|s| s.command.clone())
                {
                    overlay.input = cmd;
                    overlay.cursor = overlay.input.len();
                }
                if let Some(ref mut p) = overlay.palette {
                    p.cursor = None;
                    p.filter(&overlay.input);
                }
                true
            }
            KeyCode::Left => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                cursor_left(&overlay.input, &mut overlay.cursor);
                true
            }
            KeyCode::Right => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                cursor_right(&overlay.input, &mut overlay.cursor);
                true
            }
            KeyCode::Home => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                overlay.cursor = 0;
                true
            }
            KeyCode::End => {
                let overlay = self.ui.overlay.as_mut().unwrap();
                overlay.cursor = overlay.input.len();
                true
            }
            KeyCode::Enter => {
                // If palette item highlighted, fill input before committing
                if has_palette {
                    let overlay = self.ui.overlay.as_mut().unwrap();
                    if let Some(cmd) = overlay
                        .palette
                        .as_ref()
                        .and_then(|p| p.selected())
                        .map(|s| s.command.clone())
                    {
                        overlay.input = cmd;
                        overlay.cursor = overlay.input.len();
                    }
                }
                self.commit_overlay();
                true
            }
            KeyCode::Esc => {
                self.cancel_overlay();
                true
            }
            // Navigation keys pass through to Navigate
            _ => false,
        }
    }

    fn commit_overlay(&mut self) {
        let overlay = match self.ui.overlay.take() {
            Some(o) => o,
            None => return,
        };

        match overlay.kind {
            OverlayKind::Search => {
                // Parse + commit; the roster selection survives a re-parse.
                let agents = std::mem::take(&mut self.ui.msg_filter.agents);
                self.ui.msg_filter = crate::tui::filter::MsgFilter::parse(&overlay.input);
                self.ui.msg_filter.agents = agents;
                self.ui.msg_scroll = 0;
                self.ui.trigger_inline_replay();
            }
            OverlayKind::Command => {
                if !overlay.input.is_empty() {
                    let cmd = overlay.input;
                    let is_inline = self.ui.view_mode == ViewMode::Inline;
                    if let Err(e) = self.enqueue_rpc(RpcOp::Command { cmd: cmd.clone() }) {
                        self.ui.command_result = Some(CommandResult {
                            label: cmd,
                            output: vec![format!("Error: {}", e)],
                        });
                        if is_inline {
                            self.ui.pending_eject_cmd = true;
                        } else {
                            self.ui.mode = InputMode::CommandOutput;
                        }
                    } else {
                        self.ui.command_result = Some(CommandResult {
                            label: cmd,
                            output: vec!["(running...)".into()],
                        });
                        if is_inline {
                            self.ui.flash =
                                Some(Flash::new("Running...".into(), Theme::flash_info()));
                        } else {
                            self.ui.mode = InputMode::CommandOutput;
                        }
                    }
                    self.ui.msg_scroll = 0;
                }
            }
            OverlayKind::Tag => {
                let tag = overlay.input.trim().to_string();
                let targets = overlay.targets;
                if !targets.is_empty() {
                    for name in &targets {
                        if let Err(e) = self.enqueue_rpc(RpcOp::Tag {
                            name: name.clone(),
                            tag: tag.clone(),
                        }) {
                            self.ui.flash =
                                Some(Flash::new(format!("Tag failed: {}", e), Theme::flash_err()));
                            break;
                        }
                    }
                    if self.ui.flash.is_none() {
                        let label = if targets.len() == 1 {
                            format!("Tagging {}", targets[0])
                        } else {
                            format!("Tagging {} agents", targets.len())
                        };
                        self.ui.flash = Some(Flash::new(label, Theme::flash_info()));
                    }
                }
            }
        }
    }

    fn cancel_overlay(&mut self) {
        // Search: the draft is discarded and the committed filter is left
        // untouched — an intentional change from the old cancel-clears
        // behaviour. Other overlays have nothing to roll back.
        self.ui.overlay = None;
    }

    // ── Command palette suggestions ─────────────────────────────

    fn build_command_suggestions(&self) -> Vec<CommandSuggestion> {
        let mut s = Vec::new();

        // Collect targeted agent action names: selected agents, or cursor agent if none selected.
        let mut targeted: Vec<&Agent> = Vec::new();
        if !self.ui.msg_filter.agents.is_empty() {
            for agent in self.all_agents() {
                if self.ui.msg_filter.agents.contains(&agent.action_name()) {
                    targeted.push(agent);
                }
            }
        } else if let Some(agent) = self.cursor_agent() {
            targeted.push(agent);
        }

        // Per-agent commands for targeted agents go first
        for agent in &targeted {
            push_agent_command_suggestions(&mut s, agent);
        }

        // Static commands
        s.push(CommandSuggestion {
            command: "status".into(),
            description: "system status",
        });
        s.push(CommandSuggestion {
            command: "config".into(),
            description: "show configuration",
        });
        s.push(CommandSuggestion {
            command: "daemon status".into(),
            description: "daemon health",
        });
        s.push(CommandSuggestion {
            command: "hooks status".into(),
            description: "hook status",
        });
        s.push(CommandSuggestion {
            command: "archive".into(),
            description: "archive stopped",
        });
        s.push(CommandSuggestion {
            command: "reset".into(),
            description: "clear database",
        });

        // Per-agent commands for non-targeted agents
        for agent in self.all_agents() {
            let name = agent.action_name();
            if targeted.iter().any(|target| target.action_name() == name) {
                continue;
            }
            push_agent_command_suggestions(&mut s, agent);
        }

        s
    }

    // ── Compose mode ──────────────────────────────────────────────

    fn handle_compose(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.ui.input.clear();
                self.ui.input_cursor = 0;
                self.ui.input_scroll = 0;
                self.ui.mode = InputMode::Navigate;
            }
            KeyCode::Enter => {
                if !self.ui.input.is_empty() {
                    self.send_message();
                }
                self.ui.input.clear();
                self.ui.input_cursor = 0;
                self.ui.input_scroll = 0;
                self.ui.mode = InputMode::Navigate;
            }
            KeyCode::Tab => {
                self.ui.input_scroll = 0;
                self.ui.mode = InputMode::Launch;
                self.ui.launch.options_cursor = None;
                self.ui.msg_scroll = 0;
            }
            KeyCode::Backspace => {
                if self.ui.input.is_empty() {
                    self.ui.mode = InputMode::Navigate;
                } else {
                    delete_back(&mut self.ui.input, &mut self.ui.input_cursor);
                }
            }
            KeyCode::Left => cursor_left(&self.ui.input, &mut self.ui.input_cursor),
            KeyCode::Right => cursor_right(&self.ui.input, &mut self.ui.input_cursor),
            KeyCode::Up => {
                crate::tui::render::cursor_wrap_up(
                    &self.ui.input,
                    &mut self.ui.input_cursor,
                    self.ui.term_width,
                    4,
                );
            }
            KeyCode::Down => {
                crate::tui::render::cursor_wrap_down(
                    &self.ui.input,
                    &mut self.ui.input_cursor,
                    self.ui.term_width,
                    4,
                );
            }
            KeyCode::Home => self.ui.input_cursor = 0,
            KeyCode::End => self.ui.input_cursor = self.ui.input.len(),
            KeyCode::Char(c) => insert_at(&mut self.ui.input, &mut self.ui.input_cursor, c),
            _ => {}
        }
    }

    fn send_message(&mut self) {
        let (recipients, body) = parse_outbound_message(&self.ui.input);
        let recipients = if recipients.is_empty() {
            vec!["all".to_string()]
        } else {
            recipients
        };
        if let Err(e) = self.enqueue_rpc(RpcOp::Send {
            recipients,
            body,
            intent: None,
            reply_to: None,
        }) {
            self.ui.flash = Some(Flash::new(
                format!("Send failed: {}", e),
                Theme::flash_err(),
            ));
        } else {
            self.ui.flash = Some(Flash::new("Sending...".into(), Theme::flash_info()));
        }
    }

    // ── CommandOutput mode (vertical view only) ───────────────────

    fn handle_command_output(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.ui.mode = InputMode::Navigate;
                self.ui.command_result = None;
                self.ui.msg_scroll = 0;
            }
            KeyCode::PageUp => {
                self.ui.msg_scroll = self.ui.msg_scroll.saturating_add(5);
            }
            KeyCode::PageDown => {
                self.ui.msg_scroll = self.ui.msg_scroll.saturating_sub(5);
            }
            _ => {}
        }
    }

    // ── Launch mode (unchanged) ───────────────────────────────────

    fn handle_launch_inline(&mut self, code: KeyCode) {
        if self.ui.launch.editing.is_some() {
            match code {
                KeyCode::Esc => self.ui.launch.cancel_editing(),
                KeyCode::Enter => self.ui.launch.stop_editing(),
                // Up/Down: save and navigate away
                KeyCode::Up => self.ui.launch.cursor_up(),
                KeyCode::Down => self.ui.launch.cursor_down(),
                KeyCode::Home => self.ui.launch.edit_cursor = 0,
                KeyCode::End => {
                    if let Some(field) = self.ui.launch.editing {
                        self.ui.launch.edit_cursor = self.ui.launch.field_value(field).len();
                    }
                }
                KeyCode::Left => self.ui.launch.edit_cursor_left(),
                KeyCode::Right => self.ui.launch.edit_cursor_right(),
                KeyCode::Char(c) => self.ui.launch.insert_char(c),
                KeyCode::Backspace => self.ui.launch.delete_char(),
                _ => {}
            }
            return;
        }

        if self.ui.launch.options_cursor.is_some() {
            match code {
                KeyCode::Esc => {
                    self.ui.launch.options_cursor = None;
                }
                KeyCode::Up => self.ui.launch.cursor_up(),
                KeyCode::Down => self.ui.launch.cursor_down(),
                KeyCode::Left => self.ui.launch.adjust_left(),
                KeyCode::Right => self.ui.launch.adjust_right(),
                KeyCode::Enter if !self.ui.launch.is_text_field() => {
                    self.ui.launch.toggle_or_select();
                }
                KeyCode::Char(' ') => self.ui.launch.toggle_or_select(),
                KeyCode::Char(c) if self.ui.launch.is_text_field() => {
                    self.ui.launch.start_editing();
                    self.ui.launch.insert_char(c);
                }
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Esc => {
                if !self.ui.input.is_empty() {
                    self.ui.input.clear();
                    self.ui.input_cursor = 0;
                } else {
                    self.ui.launch = LaunchState::new();
                    self.ui.mode = InputMode::Navigate;
                }
            }

            KeyCode::Tab => {
                self.ui.launch = LaunchState::new();
                self.ui.mode = InputMode::Navigate;
            }

            KeyCode::Left if self.ui.input.is_empty() => {
                self.ui.launch.tool = self.ui.launch.tool.prev();
            }
            KeyCode::Right if self.ui.input.is_empty() => {
                self.ui.launch.tool = self.ui.launch.tool.next();
            }

            KeyCode::Up => {
                let fields = self.ui.launch.settings_fields();
                self.ui.launch.options_cursor = fields.last().copied();
            }
            KeyCode::Down => {
                let fields = self.ui.launch.settings_fields();
                self.ui.launch.options_cursor = fields.first().copied();
            }

            KeyCode::Home => self.ui.input_cursor = 0,
            KeyCode::End => self.ui.input_cursor = self.ui.input.len(),
            KeyCode::Left => cursor_left(&self.ui.input, &mut self.ui.input_cursor),
            KeyCode::Right => cursor_right(&self.ui.input, &mut self.ui.input_cursor),

            KeyCode::Enter => {
                let tool = self.ui.launch.tool.clone();
                let count = self.ui.launch.count;
                let tag = self.ui.launch.tag.clone();
                let headless = self.ui.launch.headless;
                let headless_pty = self.ui.launch.headless_pty;
                let terminal = self
                    .ui
                    .launch
                    .terminal_presets
                    .get(self.ui.launch.terminal)
                    .map(|s| s.as_str())
                    .unwrap_or("auto");
                let prompt = self.ui.input.clone();
                let tool_name = tool.name().to_string();

                if let Err(e) = self.enqueue_rpc(RpcOp::Launch {
                    tool,
                    count,
                    tag,
                    headless,
                    headless_pty,
                    terminal: terminal.into(),
                    prompt,
                }) {
                    self.ui.flash = Some(Flash::new(
                        format!("Launch failed: {}", e),
                        Theme::flash_err(),
                    ));
                } else {
                    self.ui.flash = Some(Flash::new(
                        format!("Launching {} {}", count, tool_name),
                        Theme::flash_info(),
                    ));
                }
                self.ui.input.clear();
                self.ui.input_cursor = 0;
                self.ui.launch = LaunchState::new();
                self.ui.mode = InputMode::Navigate;
            }

            KeyCode::Backspace => delete_back(&mut self.ui.input, &mut self.ui.input_cursor),
            KeyCode::Char(c) => insert_at(&mut self.ui.input, &mut self.ui.input_cursor, c),

            _ => {}
        }
    }

    // ── Relay mode (unchanged) ────────────────────────────────────

    fn handle_relay(&mut self, code: KeyCode) {
        let popup = match self.ui.relay_popup.as_mut() {
            Some(p) => p,
            None => return,
        };

        if popup.editing_token {
            match code {
                KeyCode::Esc => {
                    popup.editing_token = false;
                    popup.token_input.clear();
                    popup.token_cursor = 0;
                }
                KeyCode::Enter if !popup.token_input.is_empty() => {
                    let token = popup.token_input.clone();
                    let cmd = format!("relay connect {}", token);
                    if let Err(e) = self.enqueue_rpc(RpcOp::RelayConnect {
                        token: token.clone(),
                    }) {
                        self.ui.command_result = Some(CommandResult {
                            label: cmd,
                            output: vec![format!("Error: {}", e)],
                        });
                    } else {
                        self.ui.command_result = Some(CommandResult {
                            label: cmd,
                            output: vec!["(running...)".into()],
                        });
                    }
                    self.ui.relay_popup = None;
                    if self.ui.view_mode == ViewMode::Inline {
                        self.ui.pending_eject_cmd = true;
                        self.ui.mode = InputMode::Navigate;
                    } else {
                        self.ui.mode = InputMode::CommandOutput;
                        self.ui.msg_scroll = 0;
                    }
                }
                KeyCode::Left => cursor_left(&popup.token_input, &mut popup.token_cursor),
                KeyCode::Right => cursor_right(&popup.token_input, &mut popup.token_cursor),
                KeyCode::Backspace => delete_back(&mut popup.token_input, &mut popup.token_cursor),
                KeyCode::Char(c) => insert_at(&mut popup.token_input, &mut popup.token_cursor, c),
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Esc => {
                self.ui.relay_popup = None;
                self.ui.mode = InputMode::Navigate;
            }
            KeyCode::Up if popup.cursor > 0 => {
                popup.cursor -= 1;
            }
            KeyCode::Down if popup.cursor < RELAY_ACTIONS.len() as u8 => {
                popup.cursor += 1;
            }
            KeyCode::Enter | KeyCode::Char(' ') => match popup.cursor {
                0 => {
                    if popup.toggling {
                        return; // already in-flight
                    }
                    let new_state = !popup.enabled;
                    if let Err(e) = self.enqueue_rpc(RpcOp::RelayToggle { enable: new_state }) {
                        self.ui.flash = Some(Flash::new(
                            format!("Relay toggle failed: {}", e),
                            Theme::flash_err(),
                        ));
                    } else if let Some(p) = self.ui.relay_popup.as_mut() {
                        p.toggling = true;
                    }
                }
                action @ (1 | 2) => {
                    let (cmd, op): (String, RpcOp) = if action == 1 {
                        ("relay status".into(), RpcOp::RelayStatus)
                    } else {
                        ("relay new".into(), RpcOp::RelayNew)
                    };
                    let output = if let Err(e) = self.enqueue_rpc(op) {
                        vec![format!("Error: {}", e)]
                    } else {
                        vec!["(running...)".into()]
                    };
                    self.ui.command_result = Some(CommandResult { label: cmd, output });
                    self.ui.relay_popup = None;
                    if self.ui.view_mode == ViewMode::Inline {
                        self.ui.pending_eject_cmd = true;
                        self.ui.mode = InputMode::Navigate;
                    } else {
                        self.ui.mode = InputMode::CommandOutput;
                        self.ui.msg_scroll = 0;
                    }
                }
                3 => {
                    popup.editing_token = true;
                    popup.token_input.clear();
                    popup.token_cursor = 0;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn push_agent_command_suggestions(s: &mut Vec<CommandSuggestion>, agent: &Agent) {
    let name = agent.action_name();
    if !agent.is_stopped() {
        s.push(CommandSuggestion {
            command: format!("term {}", name),
            description: "view terminal",
        });
        s.push(CommandSuggestion {
            command: format!("term inject {} --enter", name),
            description: "send enter to terminal",
        });
    }
    s.push(CommandSuggestion {
        command: format!("transcript {} --last 1 --full", name),
        description: "last conversation (full)",
    });
    s.push(CommandSuggestion {
        command: format!("transcript {} --full", name),
        description: "full transcript",
    });
    if agent.can_resume() {
        s.push(CommandSuggestion {
            command: format!("r {}", name),
            description: "resume agent",
        });
    }
    if agent.can_fork_from_tui() {
        s.push(CommandSuggestion {
            command: format!("f {}", name),
            description: "fork agent",
        });
    }
    if agent.can_kill() {
        s.push(CommandSuggestion {
            command: format!("kill {}", name),
            description: "kill agent",
        });
    }
    if agent.can_tag() {
        s.push(CommandSuggestion {
            command: format!("config -i {} tag", name),
            description: "set tag",
        });
    }
}

#[cfg(test)]
mod tests {
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
        app.ui.msg_filter =
            crate::tui::filter::MsgFilter::parse("tag:review thread:t from:ligo hi");
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
}
