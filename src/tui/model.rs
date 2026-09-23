use crate::tui::theme::Theme;
use ratatui::style::Style;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewMode {
    Inline,   // agents only, no messages panel
    Vertical, // agents left, messages right
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputMode {
    Navigate,
    Compose,
    CommandOutput,
    Launch,
    Relay,
}

pub struct CommandResult {
    pub label: String,
    pub output: Vec<String>,
}

pub struct RelayPopupState {
    pub enabled: bool,
    pub toggling: bool, // true while relay toggle RPC is in-flight
    pub cursor: u8,     // 0=toggle, 1=status, 2=new, 3=connect
    pub editing_token: bool,
    pub token_input: String,
    pub token_cursor: usize,
}

impl RelayPopupState {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            toggling: false,
            cursor: 0,
            editing_token: false,
            token_input: String::new(),
            token_cursor: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AgentStatus {
    Active,
    Listening,
    Blocked,
    Launching,
    Inactive,
}

impl AgentStatus {
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Active => "\u{25b6}",
            Self::Listening => "\u{25c9}",
            Self::Blocked => "\u{25a0}",
            Self::Launching => "\u{25ce}",
            Self::Inactive => "\u{25cb}",
        }
    }

    pub fn style(&self) -> Style {
        match self {
            Self::Active => Theme::active(),
            Self::Listening => Theme::listening(),
            Self::Blocked => Theme::blocked(),
            Self::Launching => Theme::launching(),
            Self::Inactive => Theme::inactive(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tool {
    Claude,
    Gemini,
    Codex,
    OpenCode,
    Kilo,
    Pi,
    Omp,
    Antigravity,
    Cursor,
    Kimi,
    Copilot,
    Adhoc,
    /// Persisted value written by a newer or third-party integration.
    Unknown(String),
}

impl Tool {
    /// Convert to the canonical `crate::tool::Tool` for spec lookup.
    pub fn canonical(&self) -> Option<crate::tool::Tool> {
        match self {
            Self::Claude => Some(crate::tool::Tool::Claude),
            Self::Gemini => Some(crate::tool::Tool::Gemini),
            Self::Codex => Some(crate::tool::Tool::Codex),
            Self::OpenCode => Some(crate::tool::Tool::OpenCode),
            Self::Kilo => Some(crate::tool::Tool::Kilo),
            Self::Pi => Some(crate::tool::Tool::Pi),
            Self::Omp => Some(crate::tool::Tool::Omp),
            Self::Antigravity => Some(crate::tool::Tool::Antigravity),
            Self::Cursor => Some(crate::tool::Tool::Cursor),
            Self::Kimi => Some(crate::tool::Tool::Kimi),
            Self::Copilot => Some(crate::tool::Tool::Copilot),
            Self::Adhoc => Some(crate::tool::Tool::Adhoc),
            Self::Unknown(_) => None,
        }
    }

    /// Integration spec for known TUI tools.
    pub fn spec(&self) -> Option<&'static crate::integration_spec::IntegrationSpec> {
        self.canonical().map(crate::tool::Tool::spec)
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Unknown(raw) => raw,
            _ => {
                self.spec()
                    .expect("known TUI tool must have an integration spec")
                    .name
            }
        }
    }

    /// Cycle forward (for launch panel). Adhoc is not launchable.
    pub fn next(&self) -> Self {
        match self {
            Self::Claude => Self::Gemini,
            Self::Gemini => Self::Codex,
            Self::Codex => Self::OpenCode,
            Self::OpenCode => Self::Kilo,
            Self::Kilo => Self::Pi,
            Self::Pi => Self::Omp,
            Self::Omp => Self::Antigravity,
            Self::Antigravity => Self::Cursor,
            Self::Cursor => Self::Kimi,
            Self::Kimi => Self::Copilot,
            Self::Copilot => Self::Claude,
            Self::Adhoc => Self::Adhoc,
            Self::Unknown(raw) => Self::Unknown(raw.clone()),
        }
    }

    /// Cycle backward (for launch panel). Adhoc is not launchable.
    pub fn prev(&self) -> Self {
        match self {
            Self::Claude => Self::Copilot,
            Self::Gemini => Self::Claude,
            Self::Codex => Self::Gemini,
            Self::OpenCode => Self::Codex,
            Self::Antigravity => Self::Omp,
            Self::Omp => Self::Pi,
            Self::Pi => Self::Kilo,
            Self::Kilo => Self::OpenCode,
            Self::Cursor => Self::Antigravity,
            Self::Kimi => Self::Cursor,
            Self::Copilot => Self::Kimi,
            Self::Adhoc => Self::Adhoc,
            Self::Unknown(raw) => Self::Unknown(raw.clone()),
        }
    }
}

#[derive(Clone)]
pub struct Agent {
    pub name: String,
    pub tool: Tool,
    pub status: AgentStatus,
    pub status_context: String,
    pub status_detail: String,
    pub created_at: f64,
    pub status_time: f64,
    pub last_heartbeat: f64,
    pub has_tcp: bool,
    pub directory: String,
    pub tag: String,
    pub unread: usize,
    pub last_event_id: Option<u64>,
    pub device_name: Option<String>,
    pub sync_age: Option<String>,
    pub headless: bool,
    pub session_id: Option<String>,
    pub pid: Option<u32>,
    pub terminal_preset: Option<String>,
}

impl Agent {
    /// Full display name: `{tag}-{name}` if tagged, else `{name}`.
    /// Appends `:{device}` for remote agents.
    pub fn display_name(&self) -> String {
        let base = if self.tag.is_empty() {
            self.name.clone()
        } else {
            format!("{}-{}", self.tag, self.name)
        };
        if let Some(ref device) = self.device_name {
            format!("{}:{}", base, device)
        } else {
            base
        }
    }

    pub fn is_remote(&self) -> bool {
        self.device_name.is_some()
    }

    pub fn is_stopped(&self) -> bool {
        self.status == AgentStatus::Inactive
    }

    pub fn can_kill(&self) -> bool {
        !self.is_stopped()
    }

    pub fn can_resume(&self) -> bool {
        self.is_stopped() && self.tool.spec().is_some_and(|spec| spec.resume.is_some())
    }

    pub fn can_fork_from_tui(&self) -> bool {
        !self.is_remote()
            && !self.is_stopped()
            && self
                .tool
                .spec()
                .and_then(|spec| spec.resume)
                .is_some_and(|resume| resume.fork.is_some())
    }

    pub fn can_tag(&self) -> bool {
        true
    }

    pub fn action_name(&self) -> String {
        if self.is_remote() {
            self.display_name()
        } else {
            self.name.clone()
        }
    }

    pub fn context_display(&self) -> String {
        // Strip known internal prefixes for cleaner display
        let ctx = strip_context_prefix(&self.status_context);
        if self.status_detail.is_empty() {
            ctx.to_string()
        } else {
            format!("{}: {}", ctx, self.status_detail)
        }
    }

    /// Time since last status change (falls back to created_at).
    pub fn age_display(&self) -> String {
        let now = epoch_now();
        let base = if self.status_time > 0.0 {
            self.status_time
        } else {
            self.created_at
        };
        format_duration_short((now - base).max(0.0) as u64)
    }

    /// Age since creation (total session duration).
    pub fn created_display(&self) -> String {
        format_duration_short((epoch_now() - self.created_at).max(0.0) as u64)
    }

    /// True when PTY delivery is held by a TUI gate or an approval prompt.
    pub fn is_pty_blocked(&self) -> bool {
        crate::shared::is_delivery_paused_status_context(&self.status_context)
    }
}

/// Strip known internal prefixes from status_context for display.
/// e.g. "tool:Bash" → "Bash", "tui:not-ready" → "not-ready"
fn strip_context_prefix(ctx: &str) -> &str {
    if let Some(rest) = ctx.strip_prefix("exit:") {
        return match rest {
            "unknown" => "ended",
            "" => "ended",
            other => other,
        };
    }
    const PREFIXES: &[&str] = &[
        "tool:",
        "deliver:",
        "approved:",
        "denied:",
        "stale:",
        "tui:",
    ];
    for p in PREFIXES {
        if let Some(rest) = ctx.strip_prefix(p) {
            return rest;
        }
    }
    ctx
}

/// Format a duration in seconds as a short human-readable string (e.g. "now", "5s", "3m", "2h", "1d").
pub fn format_duration_short(secs: u64) -> String {
    crate::shared::time::format_age(secs as i64)
}

/// Current Unix epoch as f64.
pub fn epoch_now() -> f64 {
    crate::shared::time::now_epoch_f64()
}

/// Format timestamp as "HH:MM" in local timezone.
pub fn format_time(t: f64) -> String {
    use chrono::{Local, TimeZone};
    if let Some(dt) = Local.timestamp_opt(t as i64, 0).single() {
        return dt.format("%H:%M").to_string();
    }
    "--:--".into()
}

// ── Message ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MessageScope {
    Broadcast,
    Mentions,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SenderKind {
    External,
    Instance,
    System,
}

#[derive(Clone)]
pub struct Message {
    pub event_id: u64,
    pub sender: String,
    pub recipients: Vec<String>,
    pub body: String,
    pub time: f64,
    #[allow(dead_code)] // read in tests
    pub delivered: Vec<String>,
    #[allow(dead_code)] // read in tests
    pub scope: MessageScope,
    pub sender_kind: SenderKind,
    pub intent: Option<String>,
    pub reply_to: Option<u64>,
    /// Thread id from the message JSON. `None` when absent, null, or non-string.
    #[allow(dead_code)] // consumed by MsgFilter in task 2
    pub thread: Option<String>,
    /// True exactly when `delivered_to` was a JSON array (including an empty
    /// array). False means unknown (absent/null/non-array); the `to:` filter
    /// must never fall back to mentions when delivery is known-but-empty.
    #[allow(dead_code)] // consumed by MsgFilter in task 2
    pub delivery_known: bool,
}

impl Message {
    pub fn is_system(&self) -> bool {
        self.sender_kind == SenderKind::System
    }
}

// ── Unified Event (replaces ToolEvent + ActivityEvent) ───────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    Tool,
    Activity(ActivityKind),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ActivityKind {
    Started,
    Active,
    Listening,
    Stopped,
    Blocked,
    StateChange,
}

#[derive(Clone)]
pub struct Event {
    pub row_id: u64, // DB row id — monotonic, used as ejection watermark
    pub agent: String,
    pub time: f64,
    pub kind: EventKind,
    pub tool: String,           // for Tool events: "Read", "Edit", etc.
    pub detail: String,         // for Tool: target path; for Activity: description
    pub sub_lines: Vec<String>, // extra detail lines (e.g. stopped snapshot)
}

#[derive(Clone)]
pub struct OrphanProcess {
    pub pid: u32,
    pub tool: Tool,
    pub names: Vec<String>,
    pub launched_at: f64,
    pub directory: String,
}

impl OrphanProcess {
    pub fn age_display(&self) -> String {
        format_duration_short((epoch_now() - self.launched_at).max(0.0) as u64)
    }

    /// Display string for the names column (e.g. "nova, kira" or "—").
    pub fn names_display(&self) -> String {
        if self.names.is_empty() {
            "\u{2014}".into()
        } else {
            self.names.join(", ")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CursorTarget {
    None,
    Agent(usize),
    RemoteHeader,
    RemoteAgent(usize),
    StoppedHeader,
    StoppedAgent(usize),
    OrphanHeader,
    Orphan(usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionAvailability {
    pub kill: bool,
    pub fork: bool,
    pub resume: bool,
    pub tag: bool,
}

pub struct Flash {
    pub text: String,
    pub style: Style,
    pub expires_at: std::time::Instant,
}

impl Flash {
    pub fn new(text: String, style: Style) -> Self {
        Self {
            text,
            style,
            expires_at: std::time::Instant::now() + std::time::Duration::from_millis(1600),
        }
    }

    pub fn is_expired(&self) -> bool {
        std::time::Instant::now() >= self.expires_at
    }
}

pub const RELAY_ACTIONS: &[&str] = &["status", "new", "connect"];

// ── Overlay (Navigate-mode text inputs) ──────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayKind {
    Search,
    Command,
    Tag,
}

pub struct Overlay {
    pub kind: OverlayKind,
    pub input: String,
    pub cursor: usize,
    /// For Tag overlay: which agents are being tagged.
    pub targets: Vec<String>,
    /// For Command overlay: browsable suggestion palette.
    pub palette: Option<CommandPalette>,
}

impl Overlay {
    pub fn new(kind: OverlayKind) -> Self {
        Self {
            kind,
            input: String::new(),
            cursor: 0,
            targets: Vec::new(),
            palette: None,
        }
    }

    pub fn with(kind: OverlayKind, targets: Vec<String>, input: String) -> Self {
        let cursor = input.len();
        Self {
            kind,
            input,
            cursor,
            targets,
            palette: None,
        }
    }

    pub fn command_with_palette(palette: CommandPalette) -> Self {
        Self {
            kind: OverlayKind::Command,
            input: String::new(),
            cursor: 0,
            targets: Vec::new(),
            palette: Some(palette),
        }
    }
}

// ── Command Palette ─────────────────────────────────────────────

#[derive(Clone)]
pub struct CommandSuggestion {
    pub command: String,
    pub description: &'static str,
}

/// Browsable, filterable suggestion list for the Command overlay.
pub struct CommandPalette {
    pub all: Vec<CommandSuggestion>,
    /// Indices into `all` matching current input filter.
    pub filtered: Vec<usize>,
    /// Position within `filtered`. None = no highlight (free-text input).
    pub cursor: Option<usize>,
}

impl CommandPalette {
    pub fn new(all: Vec<CommandSuggestion>) -> Self {
        let filtered = (0..all.len()).collect();
        Self {
            all,
            filtered,
            cursor: None,
        }
    }

    /// Rebuild filtered indices based on input text (case-insensitive substring).
    pub fn filter(&mut self, input: &str) {
        let q = input.to_lowercase();
        self.filtered = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                q.is_empty()
                    || s.command.to_lowercase().contains(&q)
                    || s.description.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        // Clamp cursor into bounds
        if let Some(c) = self.cursor
            && c >= self.filtered.len()
        {
            self.cursor = if self.filtered.is_empty() {
                None
            } else {
                Some(self.filtered.len() - 1)
            };
        }
    }

    pub fn cursor_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        match self.cursor {
            None => self.cursor = Some(0),
            Some(c) if c + 1 < self.filtered.len() => self.cursor = Some(c + 1),
            _ => {}
        }
    }

    pub fn cursor_up(&mut self) {
        match self.cursor {
            Some(0) => self.cursor = None,
            Some(c) => self.cursor = Some(c - 1),
            None => {}
        }
    }

    pub fn selected(&self) -> Option<&CommandSuggestion> {
        self.cursor
            .and_then(|c| self.filtered.get(c))
            .and_then(|&idx| self.all.get(idx))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LaunchField {
    Tool,
    Count,
    Tag,
    Headless,
    Terminal,
}

pub struct LaunchState {
    pub tool: Tool,
    pub count: u8,
    pub options_cursor: Option<LaunchField>,
    pub tag: String,
    pub headless: bool,
    /// Claude-only: when headless, `true` keeps the default live PTY-backed
    /// session; `false` opts into `-p` print mode. Ignored for other
    /// tools (their only headless mode is the PTY wrapper).
    pub headless_pty: bool,
    pub terminal: usize,
    pub terminal_presets: Vec<String>,
    pub editing: Option<LaunchField>,
    pub edit_cursor: usize,
    pub edit_snapshot: Option<String>,
}

impl Default for LaunchState {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchState {
    pub fn new() -> Self {
        use crate::tui::db::{get_available_presets, read_launch_defaults};
        let defaults = read_launch_defaults();
        let presets = get_available_presets();
        let terminal_idx = presets
            .iter()
            .position(|p| p == &defaults.terminal)
            .unwrap_or(0);
        Self {
            tool: Tool::Claude,
            count: 1,
            options_cursor: None,
            tag: defaults.tag,
            headless: false,
            headless_pty: false,
            terminal: terminal_idx,
            terminal_presets: presets,
            editing: None,
            edit_cursor: 0,
            edit_snapshot: None,
        }
    }

    /// Height of the inline panel.
    pub fn panel_height(&self) -> u16 {
        // sep + tool + count + tag + headless + terminal. Headless is available
        // for every tool (claude additionally toggles print vs PTY headless).
        6
    }

    /// Ordered navigable fields in the settings area.
    pub fn settings_fields(&self) -> &'static [LaunchField] {
        // Headless applies to every tool. For non-claude tools it routes through
        // the PTY headless wrapper; claude's Headless field cycles off/print/pty.
        &[
            LaunchField::Tool,
            LaunchField::Count,
            LaunchField::Tag,
            LaunchField::Headless,
            LaunchField::Terminal,
        ]
    }

    /// Move cursor up. At top, wraps to None (input focus).
    pub fn cursor_up(&mut self) {
        // Auto-save tag when navigating away
        if self.editing.is_some() {
            self.stop_editing();
        }
        match self.options_cursor {
            None => {
                let fields = self.settings_fields();
                self.options_cursor = fields.last().copied();
            }
            Some(current) => {
                let fields = self.settings_fields();
                if let Some(pos) = fields.iter().position(|f| *f == current) {
                    if pos == 0 {
                        self.options_cursor = None;
                    } else {
                        self.options_cursor = Some(fields[pos - 1]);
                    }
                }
            }
        }
        self.auto_edit_text_field();
    }

    /// Move cursor down. At bottom, wraps to None (input focus).
    pub fn cursor_down(&mut self) {
        // Auto-save tag when navigating away
        if self.editing.is_some() {
            self.stop_editing();
        }
        match self.options_cursor {
            None => {
                let fields = self.settings_fields();
                self.options_cursor = fields.first().copied();
            }
            Some(current) => {
                let fields = self.settings_fields();
                if let Some(pos) = fields.iter().position(|f| *f == current) {
                    if pos + 1 >= fields.len() {
                        self.options_cursor = None;
                    } else {
                        self.options_cursor = Some(fields[pos + 1]);
                    }
                }
            }
        }
        self.auto_edit_text_field();
    }

    /// Auto-enter editing mode when landing on a text field (Tag).
    fn auto_edit_text_field(&mut self) {
        if self.is_text_field() && self.editing.is_none() {
            self.start_editing();
        }
    }

    pub fn adjust_left(&mut self) {
        match self.options_cursor {
            Some(LaunchField::Tool) => {
                self.tool = self.tool.prev();
                if self.tool != Tool::Claude {
                    self.headless_pty = false;
                }
            }
            Some(LaunchField::Count) if self.count > 1 => {
                self.count -= 1;
            }
            Some(LaunchField::Terminal) => {
                if self.terminal == 0 {
                    self.terminal = self.terminal_presets.len().saturating_sub(1);
                } else {
                    self.terminal -= 1;
                }
            }
            _ => {}
        }
    }

    pub fn adjust_right(&mut self) {
        match self.options_cursor {
            Some(LaunchField::Tool) => {
                self.tool = self.tool.next();
                if self.tool != Tool::Claude {
                    self.headless_pty = false;
                }
            }
            Some(LaunchField::Count) if self.count < 99 => {
                self.count += 1;
            }
            Some(LaunchField::Terminal) => {
                self.terminal = (self.terminal + 1) % self.terminal_presets.len();
            }
            _ => {}
        }
    }

    pub fn toggle_or_select(&mut self) {
        if self.options_cursor == Some(LaunchField::Headless) {
            if self.tool == Tool::Claude {
                // Cycle off → PTY headless (default) → print headless → off.
                // PTY is the default; print (`-p`) is the opt-in second stop
                // because it draws from a separate Agent SDK credit pool.
                (self.headless, self.headless_pty) = match (self.headless, self.headless_pty) {
                    (false, _) => (true, true),      // pty (default)
                    (true, true) => (true, false),   // print
                    (true, false) => (false, false), // off
                };
            } else {
                self.headless = !self.headless;
                self.headless_pty = false;
            }
        }
    }

    pub fn is_text_field(&self) -> bool {
        matches!(self.options_cursor, Some(LaunchField::Tag))
    }

    pub fn start_editing(&mut self) {
        if self.is_text_field() {
            let field = self.options_cursor.unwrap();
            let val = self.field_value(field);
            let len = val.len();
            let snapshot = val.to_string();
            self.editing = Some(field);
            self.edit_cursor = len;
            self.edit_snapshot = Some(snapshot);
        }
    }

    pub fn stop_editing(&mut self) {
        self.editing = None;
        self.edit_cursor = 0;
        self.edit_snapshot = None;
    }

    pub fn cancel_editing(&mut self) {
        if let (Some(field), Some(snapshot)) = (self.editing, self.edit_snapshot.take())
            && let Some(s) = self.field_value_mut(field)
        {
            *s = snapshot;
        }
        self.editing = None;
        self.edit_cursor = 0;
    }

    pub fn edit_cursor_left(&mut self) {
        if let Some(LaunchField::Tag) = self.editing {
            cursor_left(&self.tag, &mut self.edit_cursor);
        }
    }

    pub fn edit_cursor_right(&mut self) {
        if let Some(LaunchField::Tag) = self.editing {
            cursor_right(&self.tag, &mut self.edit_cursor);
        }
    }

    pub fn field_value(&self, field: LaunchField) -> &str {
        match field {
            LaunchField::Tag => &self.tag,
            _ => "",
        }
    }

    pub fn field_value_mut(&mut self, field: LaunchField) -> Option<&mut String> {
        match field {
            LaunchField::Tag => Some(&mut self.tag),
            _ => None,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if let Some(LaunchField::Tag) = self.editing {
            insert_at(&mut self.tag, &mut self.edit_cursor, c);
        }
    }

    pub fn delete_char(&mut self) {
        if let Some(LaunchField::Tag) = self.editing {
            delete_back(&mut self.tag, &mut self.edit_cursor);
        }
    }

    pub fn delete_word(&mut self) {
        if let Some(LaunchField::Tag) = self.editing {
            delete_word_back(&mut self.tag, &mut self.edit_cursor);
        }
    }

    pub fn delete_to_start(&mut self) {
        if let Some(LaunchField::Tag) = self.editing {
            crate::tui::model::delete_to_start(&mut self.tag, &mut self.edit_cursor);
        }
    }
}

// ── Shared text-input helpers ─────────────────────────────────────

/// Move cursor one grapheme cluster left.
pub fn cursor_left(s: &str, cursor: &mut usize) {
    if *cursor > 0 {
        let preceding = &s[..*cursor];
        if let Some(g) = preceding.graphemes(true).next_back() {
            *cursor -= g.len();
        }
    }
}

/// Move cursor one grapheme cluster right.
pub fn cursor_right(s: &str, cursor: &mut usize) {
    if *cursor < s.len() {
        let remaining = &s[*cursor..];
        if let Some(g) = remaining.graphemes(true).next() {
            *cursor += g.len();
        }
    }
}

/// Delete the grapheme cluster before the cursor.
pub fn delete_back(s: &mut String, cursor: &mut usize) {
    if *cursor > 0 {
        let preceding = &s[..*cursor];
        if let Some(g) = preceding.graphemes(true).next_back() {
            let start = *cursor - g.len();
            s.drain(start..*cursor);
            *cursor = start;
        }
    }
}

/// Delete the word before the cursor (Ctrl+W).
pub fn delete_word_back(s: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let before = &s[..*cursor];
    let trimmed = before.trim_end_matches(' ');
    if trimmed.is_empty() {
        s.drain(0..*cursor);
        *cursor = 0;
        return;
    }
    let word_start = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
    s.drain(word_start..*cursor);
    *cursor = word_start;
}

/// Delete everything before the cursor (Ctrl+U).
pub fn delete_to_start(s: &mut String, cursor: &mut usize) {
    if *cursor > 0 {
        s.drain(..*cursor);
        *cursor = 0;
    }
}

/// Insert a character at cursor position.
pub fn insert_at(s: &mut String, cursor: &mut usize, c: char) {
    s.insert(*cursor, c);
    *cursor += c.len_utf8();
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
