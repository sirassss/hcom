use crate::tui::filter::{MsgFilter, MsgTier};
use crate::tui::model::{
    Agent, CommandResult, Event, Flash, InputMode, LaunchState, Message, OrphanProcess, Overlay,
    RelayPopupState, ViewMode,
};

/// Deterministic timeline-row limit used by `DataState::empty()` fixtures so
/// header scope text never has to reparse the environment. Matches the vertical
/// default; production `load_all` reports the real effective limit instead.
pub const DEFAULT_TIMELINE_LIMIT: usize = 5000;

#[derive(Clone)]
pub struct DataState {
    pub agents: Vec<Agent>,
    pub remote_agents: Vec<Agent>,
    pub stopped_agents: Vec<Agent>,
    pub orphans: Vec<OrphanProcess>,
    pub messages: Vec<Message>,
    pub events: Vec<Event>,
    pub relay_enabled: bool,
    /// Canonical effective relay state. All UI render branches should switch
    /// on this rather than on `relay_enabled` + raw KV to avoid the false-green
    /// / disabled-but-showing-ok class of bugs. Raw underlying signals (status
    /// KV, last_error, heartbeat age, pid) are intentionally not held here —
    /// they're only meaningful via the derivation, and the JSON output exposes
    /// them under `raw` for forensics.
    pub relay_health: crate::relay::RelayHealth,
    /// Effective timeline row limit that produced `messages`/`events` (the
    /// viewport default or `HCOM_TUI_TIMELINE_LIMIT`). Headers read this rather
    /// than re-deriving scope.
    pub timeline_limit: usize,
}

impl DataState {
    pub fn empty() -> Self {
        Self {
            agents: vec![],
            remote_agents: vec![],
            stopped_agents: vec![],
            orphans: vec![],
            messages: vec![],
            events: vec![],
            relay_enabled: false,
            relay_health: crate::relay::RelayHealth::NotConfigured,
            timeline_limit: DEFAULT_TIMELINE_LIMIT,
        }
    }

    /// Find the roster agent whose identity matches `name`, searching local,
    /// remote and loaded stopped rosters. Matches raw storage identity first
    /// (local base name, or `base:device` for a remote), then display/action
    /// aliases (`tag-base`, `tag-base:device`). All comparisons are
    /// case-insensitive. An unqualified base never matches a device-qualified
    /// remote agent, and exact storage identity wins over a display alias when
    /// two records would otherwise both match.
    fn resolve_agent(&self, name: &str) -> Option<&Agent> {
        let roster = || {
            self.agents
                .iter()
                .chain(self.remote_agents.iter())
                .chain(self.stopped_agents.iter())
        };
        let ci_eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

        // Pass 1: exact storage identity.
        let storage_hit = roster().find(|a| match &a.device_name {
            Some(dev) => ci_eq(&format!("{}:{}", a.name, dev), name),
            None => ci_eq(&a.name, name),
        });
        if storage_hit.is_some() {
            return storage_hit;
        }

        // Pass 2: tagged display / action aliases.
        roster().find(|a| ci_eq(&a.display_name(), name) || ci_eq(&a.action_name(), name))
    }

    /// Tag of the roster agent matching `name`, or `None` for an unknown name
    /// or an untagged agent. Reflects the currently loaded roster only — an old
    /// agent absent from every roster has no tag.
    #[allow(dead_code)] // consumed by MsgFilter in task 2
    pub fn tag_of(&self, name: &str) -> Option<String> {
        self.resolve_agent(name)
            .map(|a| a.tag.clone())
            .filter(|t| !t.is_empty())
    }

    /// Resolve a raw agent name to its tag-name display format.
    /// Falls back to the raw name if no matching agent is found.
    pub fn resolve_display_name(&self, name: &str) -> String {
        self.resolve_agent(name)
            .map(|a| a.display_name())
            .unwrap_or_else(|| name.to_string())
    }
}

pub struct UiState {
    pub cursor: usize,
    pub cursor_name: Option<String>,
    /// Detail tier for both viewports. Cycled with `v`; never touched by filter
    /// changes.
    pub msg_tier: MsgTier,
    /// The one committed filter shared by both viewports (structured tokens,
    /// free text, and the roster `agents` selection).
    pub msg_filter: MsgFilter,
    pub input: String,
    pub input_cursor: usize,
    /// First visible line in multi-line compose input.
    pub input_scroll: usize,
    pub flash: Option<Flash>,
    pub tick: u64,
    pub launch: LaunchState,
    pub should_quit: bool,
    pub switch_viewport: bool,
    pub msg_scroll: usize,
    pub scroll_max: usize,
    pub help_open: bool,
    pub help_scroll: u16,
    pub confirm: Option<Confirm>,
    pub mode: InputMode,
    pub command_result: Option<CommandResult>,
    pub view_mode: ViewMode,
    pub relay_popup: Option<RelayPopupState>,
    pub relay_text_until: Option<std::time::Instant>,
    /// Whether the last observed snapshot's relay health was Connected.
    /// Drives the "relay connected" flash on the not-Connected → Connected
    /// edge. Started as false (we haven't seen any snapshot yet, so the
    /// first Connected snapshot triggers the flash, which is what we want).
    pub last_relay_was_connected: bool,
    pub remote_expanded: bool,
    pub stopped_expanded: bool,
    pub show_all_stopped: bool,
    pub orphans_expanded: bool,
    pub inline_filter_changed: bool,
    pub needs_resize: bool,
    pub needs_clear_replay: bool,
    pub overlay: Option<Overlay>,
    pub pending_eject_cmd: bool,
    /// Terminal width, updated each render frame. Used by input handlers for wrap calculations.
    pub term_width: u16,
}

impl UiState {
    /// Set flags to trigger an inline scrollback replay (filter/search change).
    /// No-op in vertical mode.
    pub fn trigger_inline_replay(&mut self) {
        if self.view_mode == ViewMode::Inline {
            self.inline_filter_changed = true;
            // Append the replay in the existing terminal on the next draw.
            // Only a real resize may recreate/clear the inline viewport.
        }
    }
}

pub struct Confirm {
    pub text: String,
    pub action: ConfirmAction,
    pub selected: bool, // true = yes
    pub expires_at: std::time::Instant,
}

impl Confirm {
    pub fn new(text: String, action: ConfirmAction, default_yes: bool) -> Self {
        Self {
            text,
            action,
            selected: default_yes,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10),
        }
    }

    pub fn is_expired(&self) -> bool {
        std::time::Instant::now() >= self.expires_at
    }

    pub fn is_inline_agent_action(&self) -> bool {
        matches!(
            self.action,
            ConfirmAction::KillAgents(_)
                | ConfirmAction::ForkAgents(_)
                | ConfirmAction::ResumeAgents(_)
        )
    }
}

pub enum ConfirmAction {
    KillAgents(Vec<String>),
    ForkAgents(Vec<String>),
    ResumeAgents(Vec<String>),
    KillOrphan(u32),
    /// Orphan chooser: selected=false → Kill, selected=true → Recover
    OrphanAction(u32),
}

#[cfg(test)]
mod tests {
    use super::DataState;
    use crate::tui::model::Agent;
    use crate::tui::test_helpers::make_test_agent;

    fn agent(name: &str, tag: &str, device: Option<&str>) -> Agent {
        let mut a = make_test_agent(name, 60.0);
        a.tag = tag.to_string();
        a.device_name = device.map(String::from);
        a
    }

    #[test]
    fn unqualified_base_resolves_local_not_remote() {
        let mut d = DataState::empty();
        d.agents.push(agent("luna", "", None));
        d.remote_agents.push(agent("luna", "review", Some("BOXE")));

        // Bare "luna" is the local agent; it never aliases the remote.
        assert_eq!(d.resolve_display_name("luna"), "luna");
        assert_eq!(d.tag_of("luna"), None);
        // The remote is reachable only through its device-qualified identity.
        assert_eq!(d.resolve_display_name("luna:BOXE"), "review-luna:BOXE");
        assert_eq!(d.tag_of("luna:BOXE").as_deref(), Some("review"));
    }

    #[test]
    fn two_remote_devices_stay_distinct() {
        let mut d = DataState::empty();
        d.remote_agents.push(agent("luna", "a", Some("BOXE")));
        d.remote_agents.push(agent("luna", "b", Some("CRAY")));

        assert_eq!(d.tag_of("luna:BOXE").as_deref(), Some("a"));
        assert_eq!(d.tag_of("luna:CRAY").as_deref(), Some("b"));
    }

    #[test]
    fn resolves_case_insensitively_and_through_display_alias() {
        let mut d = DataState::empty();
        d.agents.push(agent("Nova", "team", None));

        assert_eq!(d.tag_of("nova").as_deref(), Some("team"));
        assert_eq!(d.tag_of("NOVA").as_deref(), Some("team"));
        // Tagged display form also resolves back to the same record.
        assert_eq!(d.tag_of("team-nova").as_deref(), Some("team"));
    }

    #[test]
    fn stopped_roster_is_searched() {
        let mut d = DataState::empty();
        d.stopped_agents.push(agent("ligo", "old", None));
        assert_eq!(d.tag_of("ligo").as_deref(), Some("old"));
    }

    #[test]
    fn unknown_name_has_no_tag_and_keeps_raw_display() {
        let d = DataState::empty();
        assert_eq!(d.tag_of("ghost"), None);
        assert_eq!(d.resolve_display_name("ghost"), "ghost");
    }

    #[test]
    fn local_tag_never_attaches_to_remote_with_same_base() {
        let mut d = DataState::empty();
        d.agents.push(agent("bono", "local", None));
        d.remote_agents.push(agent("bono", "", Some("CRAY")));

        // The remote record is untagged; the local tag must not leak onto it.
        assert_eq!(d.tag_of("bono:CRAY"), None);
        assert_eq!(d.resolve_display_name("bono:CRAY"), "bono:CRAY");
        // And the local keeps its own tag.
        assert_eq!(d.tag_of("bono").as_deref(), Some("local"));
    }

    #[test]
    fn exact_storage_identity_wins_over_display_alias_collision() {
        let mut d = DataState::empty();
        // One agent literally named "team-nova"; another "nova" tagged "team"
        // whose display_name() is also "team-nova".
        d.agents.push(agent("team-nova", "", None));
        d.agents.push(agent("nova", "team", None));

        // Storage identity match must be preferred over the display alias.
        assert_eq!(d.tag_of("team-nova"), None);
    }
}
