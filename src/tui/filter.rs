//! Shared message/event detail tier + filter for both TUI viewports.
//!
//! Pure logic: no ratatui, no SQLite. This replaces four divergent predicates
//! (`eject::message_matches_filter` / `eject::event_matches_filter` /
//! `messages::msg_matches` / `messages::event_matches`) with one gate that the
//! inline and vertical render paths both call. Task 3 migrates those callers;
//! until then everything here is an unused building block.
#![allow(dead_code)] // consumers land in task 3

use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::tui::model::{ActivityKind, Event, EventKind, Message, MessageScope, format_time};
use crate::tui::state::DataState;

// ── FeedItem ────────────────────────────────────────────────────────

/// A borrowed timeline item: a chat message or an activity/tool event.
/// Deliberately not `Debug` — `Message` and `Event` are not `Debug` either.
pub enum FeedItem<'a> {
    Msg(&'a Message),
    Ev(&'a Event),
}

impl<'a> FeedItem<'a> {
    pub fn time(&self) -> f64 {
        match self {
            FeedItem::Msg(m) => m.time,
            FeedItem::Ev(e) => e.time,
        }
    }

    /// Monotonic DB row id (message `event_id` / event `row_id`).
    pub fn row_id(&self) -> u64 {
        match self {
            FeedItem::Msg(m) => m.event_id,
            FeedItem::Ev(e) => e.row_id,
        }
    }

    fn variant_rank(&self) -> u8 {
        match self {
            FeedItem::Msg(_) => 0,
            FeedItem::Ev(_) => 1,
        }
    }

    /// Deterministic ordering key: time (via `total_cmp`), then DB row id, then
    /// variant. A total order even for equal timestamps with out-of-order ids.
    pub fn order_cmp(&self, other: &FeedItem<'_>) -> Ordering {
        self.time()
            .total_cmp(&other.time())
            .then(self.row_id().cmp(&other.row_id()))
            .then(self.variant_rank().cmp(&other.variant_rank()))
    }
}

// ── MsgTier ─────────────────────────────────────────────────────────

/// Detail tier the user cycles with `v`. Density stops being implicit state.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum MsgTier {
    /// Messages only.
    #[default]
    Compact,
    /// Messages + tool-call events (one line each).
    Normal,
    /// + lifecycle/activity events.
    Verbose,
}

impl MsgTier {
    /// `Compact → Normal → Verbose → Compact`.
    pub fn next(self) -> Self {
        match self {
            MsgTier::Compact => MsgTier::Normal,
            MsgTier::Normal => MsgTier::Verbose,
            MsgTier::Verbose => MsgTier::Compact,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MsgTier::Compact => "compact",
            MsgTier::Normal => "normal",
            MsgTier::Verbose => "verbose",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "compact" => Some(MsgTier::Compact),
            "normal" => Some(MsgTier::Normal),
            "verbose" => Some(MsgTier::Verbose),
            _ => None,
        }
    }
}

/// Which items a tier is allowed to show, before any [`MsgFilter`]. Messages
/// pass at every tier; `Tool` events need `Normal`, `Activity` needs `Verbose`.
pub fn tier_admits(item: &FeedItem, tier: MsgTier) -> bool {
    match item {
        FeedItem::Msg(_) => true,
        FeedItem::Ev(e) => match e.kind {
            EventKind::Tool => matches!(tier, MsgTier::Normal | MsgTier::Verbose),
            EventKind::Activity(_) => tier == MsgTier::Verbose,
        },
    }
}

// ── MsgFilter ───────────────────────────────────────────────────────

/// One parsed struct drives all filtering. Structured `key:value` fields plus
/// leftover free `text`, plus the roster `agents` selection (§4).
#[derive(Clone, Default)]
pub struct MsgFilter {
    pub tag: Option<String>,
    pub thread: Option<String>,
    /// `"*"` = explicitly addressed (mentions scope), any other value = a name.
    pub to: Option<String>,
    pub from: Option<String>,
    /// Leftover tokens joined with a single space.
    pub text: String,
    /// From roster selection; not part of `to_query()`.
    pub agents: BTreeSet<String>,
}

fn is_field_key(k: &str) -> bool {
    matches!(k, "tag" | "thread" | "to" | "from")
}

impl MsgFilter {
    /// Split on whitespace. A token `key:value` with an exact lowercase key and
    /// a non-empty value sets that field (last duplicate wins); everything else
    /// — unknown keys, empty values, bare words — joins back into `text`. No
    /// quoting or escaping. `agents` is never touched here.
    pub fn parse(input: &str) -> MsgFilter {
        let mut f = MsgFilter::default();
        let mut leftover: Vec<&str> = Vec::new();
        for tok in input.split_whitespace() {
            match tok.split_once(':') {
                Some((key, val)) if !val.is_empty() && is_field_key(key) => {
                    let slot = match key {
                        "tag" => &mut f.tag,
                        "thread" => &mut f.thread,
                        "to" => &mut f.to,
                        "from" => &mut f.from,
                        _ => unreachable!(),
                    };
                    *slot = Some(val.to_string());
                }
                _ => leftover.push(tok),
            }
        }
        f.text = leftover.join(" ");
        f
    }

    /// Reconstruct the `/` overlay string. Excludes `agents` by design.
    pub fn to_query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = &self.tag {
            parts.push(format!("tag:{v}"));
        }
        if let Some(v) = &self.thread {
            parts.push(format!("thread:{v}"));
        }
        if let Some(v) = &self.to {
            parts.push(format!("to:{v}"));
        }
        if let Some(v) = &self.from {
            parts.push(format!("from:{v}"));
        }
        if !self.text.is_empty() {
            parts.push(self.text.clone());
        }
        parts.join(" ")
    }

    /// Any structured `key:value` field is set.
    pub fn has_tokens(&self) -> bool {
        self.tag.is_some() || self.thread.is_some() || self.to.is_some() || self.from.is_some()
    }

    /// Anything searchable was typed (structured field or free text).
    pub fn has_query(&self) -> bool {
        self.has_tokens() || !self.text.is_empty()
    }

    /// Nothing at all — no query and no roster selection.
    pub fn is_empty(&self) -> bool {
        !self.has_query() && self.agents.is_empty()
    }

    /// Active conditions as ` · `-joined chips: `tag:x`, `thread:y`, `to:z`,
    /// `from:w`, `agent:a,b` (names run through `resolve`, then sorted), then
    /// `"free text"`. Empty when the filter is empty.
    pub fn describe_with(&self, resolve: &dyn Fn(&str) -> String) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = &self.tag {
            parts.push(format!("tag:{v}"));
        }
        if let Some(v) = &self.thread {
            parts.push(format!("thread:{v}"));
        }
        if let Some(v) = &self.to {
            parts.push(format!("to:{v}"));
        }
        if let Some(v) = &self.from {
            parts.push(format!("from:{v}"));
        }
        if !self.agents.is_empty() {
            let mut names: Vec<String> = self.agents.iter().map(|a| resolve(a)).collect();
            names.sort();
            parts.push(format!("agent:{}", names.join(",")));
        }
        if !self.text.is_empty() {
            parts.push(format!("\"{}\"", self.text));
        }
        parts.join(" \u{00b7} ")
    }

    /// [`describe_with`] with no identity resolution (raw roster names).
    pub fn describe(&self) -> String {
        self.describe_with(&|s| s.to_string())
    }

    /// All present conditions AND together (spec §3). Selected agents OR
    /// together inside their own condition; an empty `agents` set imposes no
    /// restriction. `thread:` and `to:` exclude events outright.
    pub fn matches(&self, item: &FeedItem, data: &DataState) -> bool {
        if (self.thread.is_some() || self.to.is_some()) && matches!(item, FeedItem::Ev(_)) {
            return false;
        }

        if let Some(tag) = &self.tag {
            let owner = owner_name(item);
            if !data
                .tag_of(owner)
                .is_some_and(|t| t.eq_ignore_ascii_case(tag))
            {
                return false;
            }
        }

        if let Some(from) = &self.from {
            let owner = owner_name(item);
            // A bare query may match the base on any device. An explicitly
            // qualified query must keep its device suffix significant.
            let base_match = !from.contains(':') && base(owner).eq_ignore_ascii_case(from);
            if !name_eq(data, owner, from) && !base_match {
                return false;
            }
        }

        if let Some(thread) = &self.thread {
            match item {
                FeedItem::Msg(m) => match &m.thread {
                    Some(t) if ci_contains(t, thread) => {}
                    _ => return false,
                },
                FeedItem::Ev(_) => return false,
            }
        }

        if let Some(to) = &self.to {
            match item {
                FeedItem::Msg(m) if to_matches(data, m, to) => {}
                _ => return false,
            }
        }

        if !self.agents.is_empty() && !self.agent_cond(item, data) {
            return false;
        }

        if !self.text.is_empty() && !self.text_matches(item, data) {
            return false;
        }

        true
    }

    /// "Involves this agent": sender or explicit recipient for a message
    /// (non-system broadcasts always pass), event owner for an event. This is
    /// an addressing predicate — not the recorded-delivery predicate `to:X`
    /// uses.
    fn agent_cond(&self, item: &FeedItem, data: &DataState) -> bool {
        match item {
            FeedItem::Ev(e) => self.agents.iter().any(|a| name_eq(data, a, &e.agent)),
            FeedItem::Msg(m) => {
                if m.scope == MessageScope::Broadcast && !m.is_system() {
                    return true;
                }
                self.agents.iter().any(|a| {
                    name_eq(data, a, &m.sender) || m.recipients.iter().any(|r| name_eq(data, a, r))
                })
            }
        }
    }

    /// Case-insensitive Unicode substring over the fields enumerated in spec
    /// §3, each checked independently — never concatenated.
    fn text_matches(&self, item: &FeedItem, data: &DataState) -> bool {
        let q = self.text.to_lowercase();
        let hit = |s: &str| s.to_lowercase().contains(&q);
        match item {
            FeedItem::Msg(m) => {
                hit(&m.body)
                    || hit(&m.sender)
                    || hit(&data.resolve_display_name(&m.sender))
                    || m.recipients
                        .iter()
                        .any(|r| hit(r) || hit(&data.resolve_display_name(r)))
                    || m.intent.as_deref().is_some_and(&hit)
                    || m.intent.as_deref().map(intent_badge).is_some_and(&hit)
                    || m.thread.as_deref().is_some_and(&hit)
                    || m.reply_to.is_some_and(|id| hit(&id.to_string()))
                    || hit(&format_time(m.time))
            }
            FeedItem::Ev(e) => {
                hit(&e.tool)
                    || hit(&e.detail)
                    || hit(&e.agent)
                    || hit(&data.resolve_display_name(&e.agent))
                    || e.sub_lines.iter().any(|l| hit(l))
                    || hit(&activity_label(e))
                    || hit(&format_time(e.time))
            }
        }
    }
}

// ── gate + collection ───────────────────────────────────────────────

/// The single gate for timeline items (not command output or UI chrome).
pub fn passes(item: &FeedItem, tier: MsgTier, f: &MsgFilter, data: &DataState) -> bool {
    tier_admits(item, tier) && f.matches(item, data)
}

/// Every item passing tier + filter, in deterministic chronological order. No
/// display cap and no grouping — callers apply those on top.
pub fn collect_items<'a>(data: &'a DataState, tier: MsgTier, f: &MsgFilter) -> Vec<FeedItem<'a>> {
    let mut items: Vec<FeedItem<'a>> = Vec::new();
    for m in &data.messages {
        let it = FeedItem::Msg(m);
        if passes(&it, tier, f, data) {
            items.push(it);
        }
    }
    for e in &data.events {
        let it = FeedItem::Ev(e);
        if passes(&it, tier, f, data) {
            items.push(it);
        }
    }
    items.sort_by(|a, b| a.order_cmp(b));
    items
}

/// `(matched, total)` within the current tier, before any display cap or
/// lifecycle collapsing. `total` counts tier-admitted items; `matched` also
/// applies the filter. With no query and no agent selection, `matched == total`
/// (and in `Compact` that is the message count).
pub fn counts(data: &DataState, tier: MsgTier, f: &MsgFilter) -> (usize, usize) {
    let mut total = 0usize;
    let mut matched = 0usize;
    let mut tally = |it: FeedItem| {
        if tier_admits(&it, tier) {
            total += 1;
            if f.matches(&it, data) {
                matched += 1;
            }
        }
    };
    for m in &data.messages {
        tally(FeedItem::Msg(m));
    }
    for e in &data.events {
        tally(FeedItem::Ev(e));
    }
    (matched, total)
}

// ── helpers ─────────────────────────────────────────────────────────

fn owner_name<'a>(item: &'a FeedItem) -> &'a str {
    match item {
        FeedItem::Msg(m) => &m.sender,
        FeedItem::Ev(e) => &e.agent,
    }
}

/// Everything before the first `:` — the device-suffix-stripped base name.
fn base(s: &str) -> &str {
    s.split(':').next().unwrap_or(s)
}

fn ci_contains(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// Two names refer to the same agent under the shared identity rules: they
/// resolve to the same display identity, or (both unknown) compare equal
/// case-insensitively.
fn name_eq(data: &DataState, a: &str, b: &str) -> bool {
    data.resolve_display_name(a)
        .eq_ignore_ascii_case(&data.resolve_display_name(b))
}

fn intent_badge(intent: &str) -> &str {
    match intent {
        "request" => "req",
        "ack" => "ack",
        other => other,
    }
}

fn activity_label(e: &Event) -> String {
    match e.kind {
        EventKind::Activity(ActivityKind::Active) => format!("active: {}", e.detail),
        _ => e.detail.clone(),
    }
}

/// `to:X` (spec §3). `to:*` = explicitly addressed, rejecting every broadcast.
/// `to:X` uses recorded `delivered` when delivery is known (empty included, no
/// fallback); only unknown delivery falls back to explicit mentions, and an
/// unknown broadcast proves nothing.
fn to_matches(data: &DataState, m: &Message, to: &str) -> bool {
    if to == "*" {
        return m.scope == MessageScope::Mentions && !m.recipients.is_empty();
    }
    if m.delivery_known {
        m.delivered.iter().any(|d| name_eq(data, d, to))
    } else {
        m.scope == MessageScope::Mentions && m.recipients.iter().any(|r| name_eq(data, r, to))
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::SenderKind;
    use crate::tui::test_helpers::make_test_agent;

    fn mk_msg(
        id: u64,
        sender: &str,
        recipients: &[&str],
        body: &str,
        scope: MessageScope,
    ) -> Message {
        Message {
            event_id: id,
            sender: sender.into(),
            recipients: recipients.iter().map(|s| s.to_string()).collect(),
            body: body.into(),
            time: id as f64,
            delivered: vec![],
            scope,
            sender_kind: SenderKind::Instance,
            intent: None,
            reply_to: None,
            thread: None,
            delivery_known: false,
        }
    }

    fn mk_ev(id: u64, agent: &str, kind: EventKind, tool: &str, detail: &str) -> Event {
        Event {
            row_id: id,
            agent: agent.into(),
            time: id as f64,
            kind,
            tool: tool.into(),
            detail: detail.into(),
            sub_lines: vec![],
        }
    }

    fn tool_ev(id: u64, agent: &str) -> Event {
        mk_ev(id, agent, EventKind::Tool, "Bash", "ls -la")
    }

    fn life_ev(id: u64, agent: &str) -> Event {
        mk_ev(
            id,
            agent,
            EventKind::Activity(ActivityKind::Listening),
            "",
            "listening",
        )
    }

    // ── tier ────────────────────────────────────────────────────────

    #[test]
    fn tier_admission_matrix() {
        let m = mk_msg(1, "a", &[], "hi", MessageScope::Broadcast);
        let t = tool_ev(2, "a");
        let l = life_ev(3, "a");
        for tier in [MsgTier::Compact, MsgTier::Normal, MsgTier::Verbose] {
            assert!(
                tier_admits(&FeedItem::Msg(&m), tier),
                "msg at {:?}",
                tier.as_str()
            );
        }
        assert!(!tier_admits(&FeedItem::Ev(&t), MsgTier::Compact));
        assert!(tier_admits(&FeedItem::Ev(&t), MsgTier::Normal));
        assert!(tier_admits(&FeedItem::Ev(&t), MsgTier::Verbose));

        assert!(!tier_admits(&FeedItem::Ev(&l), MsgTier::Compact));
        assert!(!tier_admits(&FeedItem::Ev(&l), MsgTier::Normal));
        assert!(tier_admits(&FeedItem::Ev(&l), MsgTier::Verbose));
    }

    #[test]
    fn tier_every_activity_variant_is_verbose_only() {
        for k in [
            ActivityKind::Started,
            ActivityKind::Active,
            ActivityKind::Listening,
            ActivityKind::Stopped,
            ActivityKind::Blocked,
            ActivityKind::StateChange,
        ] {
            let e = mk_ev(1, "a", EventKind::Activity(k), "", "x");
            assert!(!tier_admits(&FeedItem::Ev(&e), MsgTier::Normal), "{k:?}");
            assert!(tier_admits(&FeedItem::Ev(&e), MsgTier::Verbose), "{k:?}");
        }
    }

    #[test]
    fn tier_cycle_and_string_round_trip() {
        assert_eq!(MsgTier::default(), MsgTier::Compact);
        assert_eq!(MsgTier::Compact.next(), MsgTier::Normal);
        assert_eq!(MsgTier::Normal.next(), MsgTier::Verbose);
        assert_eq!(MsgTier::Verbose.next(), MsgTier::Compact);
        for t in [MsgTier::Compact, MsgTier::Normal, MsgTier::Verbose] {
            assert_eq!(MsgTier::from_str(t.as_str()), Some(t));
        }
        assert_eq!(MsgTier::from_str("COMPACT"), None);
        assert_eq!(MsgTier::from_str(""), None);
        assert_eq!(MsgTier::from_str("loud"), None);
    }

    // ── parser ──────────────────────────────────────────────────────

    #[test]
    fn parse_extracts_fields_and_joins_leftover() {
        let f = MsgFilter::parse("tag:review thread:hcom-skill to:bigboss from:ligo  free  text");
        assert_eq!(f.tag.as_deref(), Some("review"));
        assert_eq!(f.thread.as_deref(), Some("hcom-skill"));
        assert_eq!(f.to.as_deref(), Some("bigboss"));
        assert_eq!(f.from.as_deref(), Some("ligo"));
        assert_eq!(f.text, "free text");
        assert!(f.has_tokens() && f.has_query() && !f.is_empty());
    }

    #[test]
    fn parse_malformed_and_unknown_tokens_are_text() {
        let f = MsgFilter::parse("tag: deliver:bono Tag:x to:");
        assert_eq!(f.tag, None);
        assert_eq!(f.to, None);
        assert_eq!(f.text, "tag: deliver:bono Tag:x to:");
    }

    #[test]
    fn parse_last_duplicate_wins_and_keeps_colons_in_value() {
        let f = MsgFilter::parse("to:luna:BOXE to:hana from:a from:b");
        assert_eq!(f.to.as_deref(), Some("hana"));
        assert_eq!(f.from.as_deref(), Some("b"));

        let g = MsgFilter::parse("to:luna:BOXE");
        assert_eq!(g.to.as_deref(), Some("luna:BOXE"));
    }

    #[test]
    fn parse_empty_and_whitespace_is_empty() {
        let f = MsgFilter::parse("   \t  ");
        assert!(f.is_empty());
        assert_eq!(f.to_query(), "");
    }

    #[test]
    fn query_round_trips_fields() {
        let src = "tag:review thread:t1 to:bigboss from:ligo some free text";
        let f = MsgFilter::parse(src);
        let g = MsgFilter::parse(&f.to_query());
        assert_eq!(f.tag, g.tag);
        assert_eq!(f.thread, g.thread);
        assert_eq!(f.to, g.to);
        assert_eq!(f.from, g.from);
        assert_eq!(f.text, g.text);
    }

    #[test]
    fn to_query_excludes_agents() {
        let mut f = MsgFilter::parse("from:ligo hello");
        f.agents.insert("bono".into());
        assert_eq!(f.to_query(), "from:ligo hello");
        let g = MsgFilter::parse(&f.to_query());
        assert!(g.agents.is_empty());
    }

    // ── to: ─────────────────────────────────────────────────────────

    #[test]
    fn to_name_uses_recorded_delivery_and_to_star_rejects_broadcast() {
        let data = DataState::empty();

        // Broadcast, but delivery recorded reaching bigboss.
        let mut bc = mk_msg(1, "ligo", &[], "all hands", MessageScope::Broadcast);
        bc.delivered = vec!["bigboss".into(), "hana".into()];
        bc.delivery_known = true;
        assert!(to_matches(&data, &bc, "bigboss"));
        assert!(!to_matches(&data, &bc, "*"), "broadcast never matches to:*");

        // Mentions with populated delivery still fails to:* only if not mentions
        // scope; here it IS mentions → to:* passes.
        let mut mn = mk_msg(2, "ligo", &["bono"], "hi", MessageScope::Mentions);
        mn.delivered = vec!["bono".into()];
        mn.delivery_known = true;
        assert!(to_matches(&data, &mn, "*"));
    }

    #[test]
    fn to_star_rejects_broadcast_even_with_populated_delivered() {
        let data = DataState::empty();
        let mut bc = mk_msg(1, "ligo", &[], "x", MessageScope::Broadcast);
        bc.delivered = vec!["a".into(), "b".into()];
        bc.delivery_known = true;
        assert!(!to_matches(&data, &bc, "*"));
    }

    #[test]
    fn to_known_empty_delivery_never_falls_back() {
        let data = DataState::empty();
        let mut m = mk_msg(1, "ligo", &["bigboss"], "hey", MessageScope::Mentions);
        m.delivered = vec![];
        m.delivery_known = true; // explicit empty array
        assert!(!to_matches(&data, &m, "bigboss"));
    }

    #[test]
    fn to_unknown_delivery_falls_back_to_mentions_only() {
        let data = DataState::empty();

        let m = mk_msg(1, "ligo", &["bigboss"], "hey", MessageScope::Mentions);
        assert!(!m.delivery_known);
        assert!(to_matches(&data, &m, "bigboss"));

        // Unknown broadcast proves nothing about arbitrary X.
        let bc = mk_msg(2, "ligo", &[], "all", MessageScope::Broadcast);
        assert!(!to_matches(&data, &bc, "bigboss"));
    }

    #[test]
    fn to_and_thread_exclude_events() {
        let data = DataState::empty();
        let e = tool_ev(1, "ligo");
        let ft = MsgFilter::parse("to:bigboss");
        let fth = MsgFilter::parse("thread:x");
        assert!(!ft.matches(&FeedItem::Ev(&e), &data));
        assert!(!fth.matches(&FeedItem::Ev(&e), &data));
    }

    // ── from / tag / thread / agents ───────────────────────────────

    #[test]
    fn from_matches_base_or_full_case_insensitively() {
        let data = DataState::empty();
        let m = mk_msg(1, "Luna:BOXE", &[], "hi", MessageScope::Broadcast);
        assert!(MsgFilter::parse("from:luna").matches(&FeedItem::Msg(&m), &data));
        assert!(MsgFilter::parse("from:LUNA:boxe").matches(&FeedItem::Msg(&m), &data));
        assert!(!MsgFilter::parse("from:hana").matches(&FeedItem::Msg(&m), &data));
    }

    #[test]
    fn qualified_from_keeps_local_and_remote_devices_distinct() {
        let mut data = DataState::empty();
        let mut remote = make_test_agent("luna", 60.0);
        remote.device_name = Some("BOXE".into());
        remote.tag = "review".into();
        data.remote_agents.push(remote);
        let f = MsgFilter::parse("from:luna:BOXE");
        for sender in ["luna", "luna:CRAY"] {
            let m = mk_msg(1, sender, &[], "hi", MessageScope::Broadcast);
            assert!(!f.matches(&FeedItem::Msg(&m), &data), "matched {sender}");
            assert!(!f.matches(&FeedItem::Ev(&tool_ev(2, sender)), &data));
        }
        let m = mk_msg(1, "luna:BOXE", &[], "hi", MessageScope::Broadcast);
        assert!(f.matches(&FeedItem::Msg(&m), &data));
        assert!(MsgFilter::parse("from:REVIEW-LUNA:boxe").matches(&FeedItem::Msg(&m), &data));
    }

    #[test]
    fn tag_refers_to_owner_not_recipients() {
        let mut data = DataState::empty();
        let mut a = make_test_agent("ligo", 60.0);
        a.tag = "review".into();
        data.agents.push(a);
        data.agents.push(make_test_agent("bono", 60.0)); // untagged

        let from_ligo = mk_msg(1, "ligo", &["bono"], "hi", MessageScope::Mentions);
        let from_bono = mk_msg(2, "bono", &["ligo"], "hi", MessageScope::Mentions);
        let f = MsgFilter::parse("tag:REVIEW");
        assert!(f.matches(&FeedItem::Msg(&from_ligo), &data));
        assert!(!f.matches(&FeedItem::Msg(&from_bono), &data));

        // events carry the tag of their owner too
        let ev = tool_ev(3, "ligo");
        assert!(f.matches(&FeedItem::Ev(&ev), &data));
    }

    #[test]
    fn thread_is_case_insensitive_substring_on_messages() {
        let data = DataState::empty();
        let mut m = mk_msg(1, "a", &[], "hi", MessageScope::Broadcast);
        m.thread = Some("HCOM-Skill-Review".into());
        assert!(MsgFilter::parse("thread:skill").matches(&FeedItem::Msg(&m), &data));
        assert!(!MsgFilter::parse("thread:deploy").matches(&FeedItem::Msg(&m), &data));

        let mut n = mk_msg(2, "a", &[], "hi", MessageScope::Broadcast);
        n.thread = None;
        assert!(!MsgFilter::parse("thread:skill").matches(&FeedItem::Msg(&n), &data));
    }

    #[test]
    fn conditions_and_together() {
        let mut data = DataState::empty();
        let mut a = make_test_agent("ligo", 60.0);
        a.tag = "review".into();
        data.agents.push(a);

        let mut m = mk_msg(
            1,
            "ligo",
            &["bigboss"],
            "consensus closed",
            MessageScope::Mentions,
        );
        m.delivered = vec!["bigboss".into()];
        m.delivery_known = true;
        m.thread = Some("hcom".into());

        assert!(
            MsgFilter::parse("tag:review thread:hcom to:bigboss from:ligo consensus")
                .matches(&FeedItem::Msg(&m), &data)
        );
        // one wrong condition fails the whole AND
        assert!(
            !MsgFilter::parse("tag:review thread:hcom to:bigboss from:ligo MISSING")
                .matches(&FeedItem::Msg(&m), &data)
        );
    }

    #[test]
    fn agents_condition_ors_and_respects_local_vs_remote_identity() {
        let mut data = DataState::empty();
        data.agents.push(make_test_agent("luna", 60.0)); // local
        let mut rem = make_test_agent("luna", 60.0);
        rem.tag = "review".into();
        rem.device_name = Some("BOXE".into());
        data.remote_agents.push(rem);

        let from_local = mk_msg(1, "luna", &["x"], "hi", MessageScope::Mentions);
        let from_remote = mk_msg(2, "luna:BOXE", &["x"], "hi", MessageScope::Mentions);

        let mut f = MsgFilter::default();
        f.agents.insert("luna".into());
        assert!(f.matches(&FeedItem::Msg(&from_local), &data));
        assert!(
            !f.matches(&FeedItem::Msg(&from_remote), &data),
            "bare luna must not match the remote"
        );

        let mut g = MsgFilter::default();
        g.agents.insert("review-luna:BOXE".into()); // action_name form
        assert!(g.matches(&FeedItem::Msg(&from_remote), &data));
        assert!(!g.matches(&FeedItem::Msg(&from_local), &data));
    }

    #[test]
    fn agents_condition_passes_nonsystem_broadcasts() {
        let mut data = DataState::empty();
        data.agents.push(make_test_agent("luna", 60.0));

        let bc = mk_msg(1, "someone-else", &[], "all hands", MessageScope::Broadcast);
        let mut f = MsgFilter::default();
        f.agents.insert("luna".into());
        assert!(f.matches(&FeedItem::Msg(&bc), &data));

        let mut sys = mk_msg(2, "system", &[], "sys note", MessageScope::Broadcast);
        sys.sender_kind = SenderKind::System;
        assert!(!f.matches(&FeedItem::Msg(&sys), &data));
    }

    #[test]
    fn to_regression_guard_full_matches() {
        // The exact scenario the original spec would have shipped broken.
        let data = DataState::empty();
        let mut bc = mk_msg(1, "ligo", &[], "fyi", MessageScope::Broadcast);
        bc.delivered = vec!["bigboss".into(), "ligo".into()];
        bc.delivery_known = true;

        assert!(MsgFilter::parse("to:bigboss").matches(&FeedItem::Msg(&bc), &data));
        assert!(!MsgFilter::parse("to:*").matches(&FeedItem::Msg(&bc), &data));
    }

    // ── free text ──────────────────────────────────────────────────

    #[test]
    fn text_literal_substring_where_fts_would_miss() {
        let data = DataState::empty();
        let m = mk_msg(
            1,
            "a",
            &[],
            "please run deliver:bono now",
            MessageScope::Broadcast,
        );
        assert!(MsgFilter::parse("eliver").matches(&FeedItem::Msg(&m), &data));
        assert!(MsgFilter::parse("deliver:bono").matches(&FeedItem::Msg(&m), &data));
    }

    #[test]
    fn text_matches_dates_unicode_badge_reply_thread_and_sublines() {
        let data = DataState::empty();

        let mut m = mk_msg(
            1,
            "a",
            &[],
            "shipped 2026-09-10 — Đồng thuận",
            MessageScope::Broadcast,
        );
        m.intent = Some("request".into());
        m.reply_to = Some(4832);
        m.thread = Some("hcom-skill".into());
        assert!(MsgFilter::parse("2026-09-10").matches(&FeedItem::Msg(&m), &data));
        assert!(MsgFilter::parse("đồng").matches(&FeedItem::Msg(&m), &data));
        assert!(
            MsgFilter::parse("req").matches(&FeedItem::Msg(&m), &data),
            "badge"
        );
        assert!(
            MsgFilter::parse("request").matches(&FeedItem::Msg(&m), &data),
            "raw intent"
        );
        assert!(
            MsgFilter::parse("4832").matches(&FeedItem::Msg(&m), &data),
            "reply id"
        );
        assert!(
            MsgFilter::parse("hcom-skill").matches(&FeedItem::Msg(&m), &data),
            "thread"
        );

        let mut e = mk_ev(
            2,
            "a",
            EventKind::Activity(ActivityKind::Stopped),
            "",
            "stopped by bigboss",
        );
        e.sub_lines = vec!["resume: hcom r a".into()];
        assert!(
            MsgFilter::parse("stopped").matches(&FeedItem::Ev(&e), &data),
            "lifecycle label"
        );
        assert!(
            MsgFilter::parse("resume:").matches(&FeedItem::Ev(&e), &data),
            "sub-line"
        );
    }

    #[test]
    fn text_matches_resolved_event_display_name() {
        let mut data = DataState::empty();
        let mut a = make_test_agent("luna", 60.0);
        a.tag = "review".into();
        data.agents.push(a);

        let e = tool_ev(1, "luna");
        // raw agent is "luna"; resolved display is "review-luna"
        assert!(MsgFilter::parse("review-luna").matches(&FeedItem::Ev(&e), &data));
    }

    #[test]
    fn text_does_not_manufacture_cross_field_hits() {
        let data = DataState::empty();
        // "bono" in sender, "hana" in body — "bonohana" must not match.
        let m = mk_msg(1, "bono", &[], "hana said hi", MessageScope::Broadcast);
        assert!(!MsgFilter::parse("bonohana").matches(&FeedItem::Msg(&m), &data));
    }

    #[test]
    fn empty_text_always_matches() {
        let data = DataState::empty();
        let m = mk_msg(1, "a", &[], "", MessageScope::Broadcast);
        assert!(MsgFilter::default().matches(&FeedItem::Msg(&m), &data));
    }

    // ── collection + counts ────────────────────────────────────────

    #[test]
    fn collect_orders_by_time_then_id_then_variant() {
        let mut data = DataState::empty();
        // equal timestamps, out-of-order ids
        let mut m1 = mk_msg(12, "a", &[], "later id", MessageScope::Broadcast);
        m1.time = 100.0;
        let mut m2 = mk_msg(11, "a", &[], "earlier id", MessageScope::Broadcast);
        m2.time = 100.0;
        data.messages = vec![m1, m2];
        let mut e = tool_ev(5, "a");
        e.time = 100.0;
        data.events = vec![e];

        let items = collect_items(&data, MsgTier::Verbose, &MsgFilter::default());
        let ids: Vec<u64> = items.iter().map(|i| i.row_id()).collect();
        // id 5 (ev) — but message variant sorts before event at equal (time,id);
        // here ids differ so pure id order: 5, 11, 12
        assert_eq!(ids, vec![5, 11, 12]);
    }

    #[test]
    fn collect_filters_before_any_cap_and_counts_are_uncapped() {
        let mut data = DataState::empty();
        for i in 1..=10 {
            data.messages
                .push(mk_msg(i, "a", &[], "keep", MessageScope::Broadcast));
        }
        for i in 11..=15 {
            data.messages
                .push(mk_msg(i, "b", &[], "drop", MessageScope::Broadcast));
        }

        let f = MsgFilter::parse("from:a");
        let items = collect_items(&data, MsgTier::Compact, &f);
        assert_eq!(items.len(), 10);

        let (matched, total) = counts(&data, MsgTier::Compact, &f);
        assert_eq!((matched, total), (10, 15));
    }

    #[test]
    fn describe_with_resolves_sorts_and_quotes() {
        let mut f = MsgFilter::parse("tag:review to:bigboss free text");
        f.agents.insert("z-one".into());
        f.agents.insert("a-two".into());
        let resolve = |n: &str| format!("R({n})");
        let d = f.describe_with(&resolve);
        assert!(d.contains("tag:review"));
        assert!(d.contains("to:bigboss"));
        // agent names resolved and sorted
        assert!(d.contains("agent:R(a-two),R(z-one)"), "got {d}");
        // free text quoted
        assert!(d.contains("\"free text\""));
        // empty filter → empty describe
        assert_eq!(MsgFilter::default().describe(), "");
    }

    #[test]
    fn compact_counts_are_messages_only() {
        let mut data = DataState::empty();
        data.messages
            .push(mk_msg(1, "a", &[], "hi", MessageScope::Broadcast));
        data.events.push(tool_ev(2, "a"));
        data.events.push(life_ev(3, "a"));

        assert_eq!(
            counts(&data, MsgTier::Compact, &MsgFilter::default()),
            (1, 1)
        );
        assert_eq!(
            counts(&data, MsgTier::Normal, &MsgFilter::default()),
            (2, 2)
        );
        assert_eq!(
            counts(&data, MsgTier::Verbose, &MsgFilter::default()),
            (3, 3)
        );
    }
}
