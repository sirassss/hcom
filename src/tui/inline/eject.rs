use std::collections::{HashMap, VecDeque};
use std::io::{self, Stdout};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;

use crate::tui::app::DataState;
use crate::tui::filter::{self, FeedItem, MsgFilter, MsgTier};
use crate::tui::model::*;
use crate::tui::render::messages::{
    build_waterlines, event_line, format_message, lifecycle_run_line,
};
use crate::tui::render::text::highlight_spans;
use crate::tui::theme::{Theme, palette};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReplayReason {
    FilterChange,
    Resize,
}

/// One queued replay row: a real item, or a collapsed lifecycle run.
enum ReplayRow {
    Item(EjectItem),
    LifecycleRun {
        agent: String,
        time: f64,
        count: usize,
    },
}

/// Everything the replay separator needs, captured when a filter/tier change
/// starts a replay — so it can render scope, tier, conditions and the count
/// even when there are zero matching items.
struct PendingSep {
    tier: MsgTier,
    filter: MsgFilter,
    /// Conditions pre-rendered with identity resolution at capture time.
    conds: String,
    limit: usize,
    matched: usize,
    total: usize,
}

pub struct Ejector {
    last_event_row_id: u64,
    last_msg_id: u64,
    replay_items: VecDeque<ReplayRow>,
    replay_lines: VecDeque<Line<'static>>,
    replay_emitted_any: bool,
    replay_lines_per_tick: usize,
    was_replaying: bool,
    replay_reason: ReplayReason,
    /// Read-receipt waterlines keyed by display identity, refreshed each cycle.
    waterlines: HashMap<String, u64>,
    banner_emitted: bool,
    /// `(tier, filter, effective timeline limit)` captured when a filter/tier
    /// change starts a replay, so the separator can name scope, tier and the
    /// active conditions even when there are zero matching items.
    pending_filter_separator: Option<PendingSep>,
}

impl Ejector {
    pub fn new() -> Self {
        Self {
            last_event_row_id: 0,
            last_msg_id: 0,
            replay_items: VecDeque::new(),
            replay_lines: VecDeque::new(),
            replay_emitted_any: false,
            replay_lines_per_tick: std::env::var("HCOM_TUI_REPLAY_LINES_PER_TICK")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(200),
            was_replaying: false,
            replay_reason: ReplayReason::Resize,
            waterlines: HashMap::new(),
            banner_emitted: false,
            pending_filter_separator: None,
        }
    }

    /// Reset incremental watermarks and replay state.
    fn reset(&mut self) {
        self.last_event_row_id = 0;
        self.last_msg_id = 0;
        self.replay_items.clear();
        self.replay_lines.clear();
        self.replay_emitted_any = false;
        self.was_replaying = false;
        self.banner_emitted = false;
        self.pending_filter_separator = None;
    }

    fn refresh_waterlines(&mut self, data: &DataState) {
        self.waterlines = build_waterlines(data);
    }

    /// Queue a full replay of the loaded window at the current tier + filter.
    /// Replay is emitted gradually (line-bounded) to avoid post-resize stalls.
    pub fn begin_replay(
        &mut self,
        data: &DataState,
        tier: MsgTier,
        f: &MsgFilter,
        reason: ReplayReason,
    ) {
        self.reset();
        self.replay_reason = reason;
        self.pending_filter_separator = if reason == ReplayReason::FilterChange {
            let (matched, total) = filter::counts(data, tier, f);
            Some(PendingSep {
                tier,
                filter: f.clone(),
                conds: f.describe_with(&|n| data.resolve_display_name(n)),
                limit: data.timeline_limit,
                matched,
                total,
            })
        } else {
            None
        };
        self.refresh_waterlines(data);

        // Share ordering and admission with the vertical viewport. Clone the
        // matched items into the owned queue so it never borrows DataState
        // across a reload, then collapse lifecycle runs before line-budget
        // chunking (same rule as the vertical pane).
        let items: Vec<EjectItem> = filter::collect_items(data, tier, f)
            .into_iter()
            .map(|it| match it {
                FeedItem::Ev(e) => EjectItem::Ev(e.clone()),
                FeedItem::Msg(m) => EjectItem::Msg(m.clone()),
            })
            .collect();
        let had_items = !items.is_empty();
        self.replay_items = group_eject_rows(items);
        // A filter/tier change keeps a pending replay even with zero items so
        // the separator, empty-state line and live marker still drain.
        self.was_replaying = reason == ReplayReason::FilterChange || had_items;

        // Snapshot watermarks from the whole loaded window (not just matches) so
        // a row that stays in-window during replay is emitted exactly once
        // afterwards.
        self.last_event_row_id = data.events.iter().map(|e| e.row_id).max().unwrap_or(0);
        self.last_msg_id = data.messages.iter().map(|m| m.event_id).max().unwrap_or(0);
    }

    pub fn is_replaying(&self) -> bool {
        !self.replay_items.is_empty()
            || !self.replay_lines.is_empty()
            || self.pending_filter_separator.is_some()
    }

    /// Eject new events/messages to terminal scrollback via insert_before.
    pub fn eject_new(
        &mut self,
        data: &DataState,
        tier: MsgTier,
        f: &MsgFilter,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        self.refresh_waterlines(data);
        if self.is_replaying() {
            let replay_done = self.eject_replay_chunk(data, f, terminal)?;
            if replay_done && self.was_replaying {
                self.was_replaying = false;
            }
            return Ok(());
        }

        let items = self.collect_new_items(data, tier, f);
        if items.is_empty() {
            return Ok(());
        }

        // Live grouping is batch-local: collapse only within this emission.
        let rows = group_eject_rows(items);
        let query = query_of(f);
        self.eject_lines(data, &rows, query.as_deref(), terminal)
    }

    /// Scan for rows past the watermark and advance it. Captures the watermark
    /// before scanning so every row is compared against the pre-batch value,
    /// then advances each watermark to the max id seen — filtered-out rows
    /// included — so a non-monotonic time-sorted vector never skips a new row
    /// and an empty batch never lowers the watermark.
    fn collect_new_items(
        &mut self,
        data: &DataState,
        tier: MsgTier,
        f: &MsgFilter,
    ) -> Vec<EjectItem> {
        let old_ev_wm = self.last_event_row_id;
        let old_msg_wm = self.last_msg_id;
        let mut max_ev = old_ev_wm;
        let mut max_msg = old_msg_wm;
        let mut items: Vec<EjectItem> = Vec::new();
        for ev in data.events.iter() {
            max_ev = max_ev.max(ev.row_id);
            if ev.row_id > old_ev_wm && filter::passes(&FeedItem::Ev(ev), tier, f, data) {
                items.push(EjectItem::Ev(ev.clone()));
            }
        }
        for msg in data.messages.iter() {
            max_msg = max_msg.max(msg.event_id);
            if msg.event_id > old_msg_wm && filter::passes(&FeedItem::Msg(msg), tier, f, data) {
                items.push(EjectItem::Msg(msg.clone()));
            }
        }
        self.last_event_row_id = max_ev;
        self.last_msg_id = max_msg;
        // Same total order as replay and the vertical pane — events are
        // collected first, so a plain time sort would put an event ahead of an
        // older message sharing its timestamp.
        items.sort_by(|a, b| a.as_feed().order_cmp(&b.as_feed()));
        items
    }

    fn eject_replay_chunk(
        &mut self,
        data: &DataState,
        f: &MsgFilter,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<bool> {
        let width = terminal.size()?.width;
        let mut out_lines: Vec<Line<'static>> = Vec::new();

        if let Some(banner) = self.take_banner_lines(width as usize) {
            out_lines.extend(banner);
        }
        let sep = self.pending_filter_separator.take();
        if let Some(ref s) = sep {
            out_lines.extend(filter_separator_lines(s, width));
        }

        // Borrows `data`, not `self`, so it survives the `replay_items` mutation.
        let resolve_name = resolver(data);
        let wl = self.waterlines.clone();
        let query = query_of(f);

        // Fill replay_lines from items until we have enough for one chunk.
        while self.replay_lines.len() < self.replay_lines_per_tick {
            let Some(row) = self.replay_items.pop_front() else {
                break;
            };
            let pad = self.replay_emitted_any && matches!(row, ReplayRow::Item(EjectItem::Msg(_)));
            let item_lines =
                format_row_lines(&row, width, pad, &resolve_name, query.as_deref(), Some(&wl));
            self.replay_lines.extend(item_lines);
            self.replay_emitted_any = true;
        }

        let n = self.replay_lines.len().min(self.replay_lines_per_tick);
        let replay_done = self.replay_items.is_empty() && self.replay_lines.len() <= n;
        if n > 0 {
            out_lines.extend(self.replay_lines.drain(..n));
        }

        if replay_done && self.was_replaying && self.replay_reason == ReplayReason::FilterChange {
            // Zero-match replays still get an explicit empty-state line.
            if !self.replay_emitted_any
                && let Some(ref s) = sep
            {
                out_lines.push(Line::raw(""));
                out_lines.push(Line::from(Span::styled(
                    format!("  {}", empty_state_text(s.tier, &s.filter)),
                    Style::default().fg(palette::FG_DIM),
                )));
            }
            out_lines.extend(live_marker_lines(width));
        }

        if out_lines.is_empty() {
            return Ok(replay_done);
        }
        emit_lines(&out_lines, terminal)?;
        Ok(replay_done)
    }

    fn take_banner_lines(&mut self, width: usize) -> Option<Vec<Line<'static>>> {
        if self.banner_emitted {
            return None;
        }
        self.banner_emitted = true;
        let style = Style::default()
            .fg(palette::ORANGE)
            .add_modifier(Modifier::BOLD);

        let noise = |offset: usize, w: usize| -> String {
            let chars = ['░', '▒', '▓'];
            (0..w).map(|i| chars[(i + offset) % 3]).collect()
        };

        let label = " hcom ";
        let label_w = 6;
        let left_w = width.saturating_sub(label_w) / 2;
        let right_w = width.saturating_sub(label_w + left_w);

        let lines = vec![Line::from(vec![
            Span::styled(noise(0, left_w), style),
            Span::styled(label, style),
            Span::styled(noise(left_w + label_w, right_w), style),
        ])];
        Some(lines)
    }

    /// Eject command output lines into scrollback with a label separator.
    pub fn eject_command_output(
        &self,
        label: &str,
        output: &[String],
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        let width = terminal.size()?.width;
        let mut lines: Vec<Line> = Vec::new();

        lines.push(Line::raw(""));
        lines.push(separator(&format!("! {}", label), palette::MAGENTA, width));

        for line in output {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(line.clone(), Style::default().fg(palette::FG)),
            ]));
        }
        lines.push(Line::raw(""));

        emit_lines(&lines, terminal)
    }

    fn eject_lines(
        &mut self,
        data: &DataState,
        rows: &VecDeque<ReplayRow>,
        query: Option<&str>,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let width = terminal.size()?.width;
        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(banner) = self.take_banner_lines(width as usize) {
            lines.extend(banner);
        }
        let resolve_name = resolver(data);
        let mut item_lines: Vec<Line<'static>> = Vec::new();
        for row in rows {
            item_lines.extend(format_row_lines(
                row,
                width,
                !item_lines.is_empty(),
                &resolve_name,
                query,
                Some(&self.waterlines),
            ));
        }
        lines.extend(item_lines);

        emit_lines(&lines, terminal)
    }
}

/// Insert lines into scrollback via insert_before.
fn emit_lines(lines: &[Line], terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let height = lines.len() as u16;
    terminal.insert_before(height, |buf| {
        for (i, line) in lines.iter().enumerate() {
            if (i as u16) < buf.area.height {
                let row = Rect::new(buf.area.x, buf.area.y + i as u16, buf.area.width, 1);
                line.render(row, buf);
            }
        }
    })?;
    Ok(())
}

/// The one resolver both eject paths hand to formatting: identical rules to
/// the vertical pane, so a name renders the same in scrollback and in the pane.
fn resolver(data: &DataState) -> impl Fn(&str) -> String + '_ {
    |name: &str| data.resolve_display_name(name)
}

/// Committed free-text as a highlight query, `None` when empty.
fn query_of(f: &MsgFilter) -> Option<String> {
    (!f.text.is_empty()).then(|| f.text.clone())
}

/// `recent (limit N) · tier · <conditions> · [matched/total]` for the replay
/// separator.
fn filter_separator_lines(s: &PendingSep, width: u16) -> Vec<Line<'static>> {
    let scope = format!("recent (limit {}) \u{00b7} {}", s.limit, s.tier.as_str());
    let count = format!(
        " \u{00b7} {}",
        if s.filter.is_empty() {
            format!("[{}]", s.total)
        } else {
            format!("[{}/{}]", s.matched, s.total)
        }
    );
    // `separator` never truncates — it just overflows the line, dropping
    // whatever sits at the end. So the count is reserved first and the
    // conditions (then, on a very narrow terminal, the scope) absorb the clip.
    let w = |t: &str| unicode_width::UnicodeWidthStr::width(t);
    let budget = (width as usize).saturating_sub(SEPARATOR_CHROME_W + w(&count));
    let mut label = crate::tui::render::truncate_display(&scope, budget);
    if !s.conds.is_empty() {
        const SEP: &str = " \u{00b7} ";
        let avail = budget.saturating_sub(w(&label) + w(SEP));
        let conds = crate::tui::render::truncate_display(&s.conds, avail);
        if !conds.is_empty() {
            label.push_str(SEP);
            label.push_str(&conds);
        }
    }
    label.push_str(&count);
    let color = if s.filter.is_empty() {
        palette::FG_DIM
    } else {
        palette::CYAN
    };

    vec![
        Line::raw(""),
        separator(&label, color, width),
        Line::raw(""),
    ]
}

/// Empty-state wording for a zero-match replay (spec §1).
fn empty_state_text(tier: MsgTier, f: &MsgFilter) -> &'static str {
    if !f.is_empty() {
        "No matches in recent window"
    } else if tier == MsgTier::Compact {
        "No messages in recent window"
    } else {
        "No activity in recent window"
    }
}

fn live_marker_lines(width: u16) -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        separator("live", palette::GREEN, width),
        Line::raw(""),
    ]
}

/// Collapse lifecycle runs in an already-sorted item vector (spec §7). Live
/// grouping is batch-local — this is only ever handed one emission batch or one
/// full replay snapshot.
fn group_eject_rows(items: Vec<EjectItem>) -> VecDeque<ReplayRow> {
    let mut rows = VecDeque::new();
    let mut it = items.into_iter().peekable();
    while let Some(cur) = it.next() {
        match &cur {
            EjectItem::Ev(e) if matches!(e.kind, EventKind::Activity(_)) => {
                let agent = e.agent.clone();
                let minute = (e.time / 60.0).floor() as i64;
                let first_time = e.time;
                let mut buf = vec![cur];
                while let Some(EjectItem::Ev(n)) = it.peek() {
                    if matches!(n.kind, EventKind::Activity(_))
                        && n.agent == agent
                        && (n.time / 60.0).floor() as i64 == minute
                    {
                        buf.push(it.next().unwrap());
                    } else {
                        break;
                    }
                }
                if buf.len() >= 3 {
                    rows.push_back(ReplayRow::LifecycleRun {
                        agent,
                        time: first_time,
                        count: buf.len(),
                    });
                } else {
                    rows.extend(buf.into_iter().map(ReplayRow::Item));
                }
            }
            _ => rows.push_back(ReplayRow::Item(cur)),
        }
    }
    rows
}

fn format_row_lines(
    row: &ReplayRow,
    width: u16,
    pad_message: bool,
    resolve_name: &dyn Fn(&str) -> String,
    query: Option<&str>,
    waterlines: Option<&HashMap<String, u64>>,
) -> Vec<Line<'static>> {
    match row {
        ReplayRow::LifecycleRun { agent, time, count } => {
            vec![lifecycle_run_line(
                &resolve_name(agent),
                *count,
                &format_time(*time),
                width,
            )]
        }
        ReplayRow::Item(EjectItem::Ev(ev)) => {
            let time_str = format_time(ev.time);
            let mut lines = vec![event_line(
                ev,
                &time_str,
                false,
                true,
                width,
                query,
                resolve_name,
            )];
            // Tool details are one line; lifecycle sub-lines survive only for an
            // uncollapsed row (which is always Verbose — Activity is Verbose-only).
            if matches!(ev.kind, EventKind::Activity(_)) {
                for sub in &ev.sub_lines {
                    lines.push(Line::from(highlight_spans(
                        vec![Span::styled(
                            format!("        {}", sub),
                            Style::default().fg(palette::FG_DIM),
                        )],
                        query,
                    )));
                }
            }
            lines
        }
        ReplayRow::Item(EjectItem::Msg(msg)) => {
            let mut lines = Vec::new();
            if pad_message {
                lines.push(Line::raw(""));
            }
            lines.extend(format_message(msg, width, query, resolve_name, waterlines));
            lines
        }
    }
}

// ── Formatting ───────────────────────────────────────────────────

/// Leading `"  ── "` plus the trailing space around [`separator`]'s label.
const SEPARATOR_CHROME_W: usize = 6;

/// Separator line: `  ── label ──────────`
fn separator(label: &str, color: Color, width: u16) -> Line<'static> {
    let prefix = "\u{2500}\u{2500} ";
    let label_display = format!("{} ", label);
    let prefix_w = unicode_width::UnicodeWidthStr::width(prefix);
    let label_w = unicode_width::UnicodeWidthStr::width(label_display.as_str());
    let fill_len = (width as usize).saturating_sub(2 + prefix_w + label_w);

    Line::from(vec![
        Span::raw("  "),
        Span::styled(prefix.to_string(), Theme::separator()),
        Span::styled(label_display, Style::default().fg(color)),
        Span::styled("\u{2500}".repeat(fill_len), Theme::separator()),
    ])
}

enum EjectItem {
    Ev(Event),
    Msg(Message),
}

impl EjectItem {
    /// Borrow as the shared feed item, so ordering has exactly one definition.
    fn as_feed(&self) -> FeedItem<'_> {
        match self {
            EjectItem::Ev(e) => FeedItem::Ev(e),
            EjectItem::Msg(m) => FeedItem::Msg(m),
        }
    }

    #[cfg(test)]
    fn row_id(&self) -> u64 {
        match self {
            EjectItem::Ev(e) => e.row_id,
            EjectItem::Msg(m) => m.event_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::{ActivityKind, MessageScope, SenderKind};

    fn data_with(messages: Vec<Message>, events: Vec<Event>) -> DataState {
        let mut d = DataState::empty();
        d.messages = messages;
        d.events = events;
        d
    }

    fn msg(id: u64, body: &str) -> Message {
        Message {
            event_id: id,
            sender: "a".into(),
            recipients: vec![],
            body: body.into(),
            time: id as f64,
            delivered: vec![],
            scope: MessageScope::Broadcast,
            sender_kind: SenderKind::Instance,
            intent: None,
            reply_to: None,
            thread: None,
            delivery_known: false,
        }
    }

    fn ev(id: u64, kind: EventKind) -> Event {
        ev_at(id, id as f64, "a", kind)
    }

    fn ev_at(id: u64, time: f64, agent: &str, kind: EventKind) -> Event {
        Event {
            row_id: id,
            agent: agent.into(),
            time,
            kind,
            tool: "Bash".into(),
            detail: "x".into(),
            sub_lines: vec![],
        }
    }

    fn replay_ids(ej: &Ejector) -> Vec<u64> {
        let mut v: Vec<u64> = ej
            .replay_items
            .iter()
            .filter_map(|r| match r {
                ReplayRow::Item(i) => Some(i.row_id()),
                ReplayRow::LifecycleRun { .. } => None,
            })
            .collect();
        v.sort_unstable();
        v
    }

    fn run_counts(ej: &Ejector) -> Vec<usize> {
        ej.replay_items
            .iter()
            .filter_map(|r| match r {
                ReplayRow::LifecycleRun { count, .. } => Some(*count),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn begin_replay_admits_by_tier_and_matches_vertical_collector() {
        let d = data_with(
            vec![msg(1, "hello")],
            vec![
                ev(2, EventKind::Tool),
                ev(3, EventKind::Activity(ActivityKind::Listening)),
            ],
        );
        let f = MsgFilter::default();

        for tier in [MsgTier::Compact, MsgTier::Normal, MsgTier::Verbose] {
            let mut ej = Ejector::new();
            ej.begin_replay(&d, tier, &f, ReplayReason::FilterChange);
            let mut vertical: Vec<u64> = filter::collect_items(&d, tier, &f)
                .iter()
                .map(|i| i.row_id())
                .collect();
            vertical.sort_unstable();
            assert_eq!(replay_ids(&ej), vertical, "inline vs vertical at {tier:?}");
        }

        let mut ej = Ejector::new();
        ej.begin_replay(&d, MsgTier::Compact, &f, ReplayReason::FilterChange);
        assert_eq!(replay_ids(&ej), vec![1], "Compact = messages only");
        ej.begin_replay(&d, MsgTier::Normal, &f, ReplayReason::FilterChange);
        assert_eq!(replay_ids(&ej), vec![1, 2], "Normal adds the tool row");
        ej.begin_replay(&d, MsgTier::Verbose, &f, ReplayReason::FilterChange);
        assert_eq!(replay_ids(&ej), vec![1, 2, 3], "Verbose adds lifecycle");
    }

    #[test]
    fn zero_match_filter_change_keeps_a_pending_replay() {
        let d = data_with(vec![msg(1, "hello")], vec![]);
        let f = MsgFilter::parse("nomatch");
        let mut ej = Ejector::new();
        ej.begin_replay(&d, MsgTier::Compact, &f, ReplayReason::FilterChange);

        assert!(ej.replay_items.is_empty());
        assert!(ej.was_replaying, "empty result must still drain chrome");
        assert!(
            ej.is_replaying(),
            "pending separator keeps is_replaying true"
        );
    }

    #[test]
    fn collect_new_items_handles_non_monotonic_ids_and_advances_watermark() {
        // Time-ordered vector, ids [12, 11]; old watermark 10.
        let d = data_with(vec![msg(12, "later id"), msg(11, "earlier id")], vec![]);
        let mut ej = Ejector::new();
        ej.last_msg_id = 10;

        let got = ej.collect_new_items(&d, MsgTier::Compact, &MsgFilter::default());
        let mut ids: Vec<u64> = got.iter().map(|i| i.row_id()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![11, 12], "both rows past the old watermark emit");
        assert_eq!(ej.last_msg_id, 12, "watermark advances to the max id seen");

        // A second scan with no new rows must not lower or re-emit.
        let again = ej.collect_new_items(&d, MsgTier::Compact, &MsgFilter::default());
        assert!(again.is_empty());
        assert_eq!(ej.last_msg_id, 12);
    }

    // Regression: live eject sorted by time alone while events were collected
    // first, so an event tied with an older message jumped ahead of it — the
    // replay and vertical paths order by (time, row id, variant).
    #[test]
    fn collect_new_items_orders_ties_by_row_id() {
        let mut m = msg(11, "message first");
        m.time = 100.0;
        let d = data_with(vec![m], vec![ev_at(12, 100.0, "a", EventKind::Tool)]);

        let mut ej = Ejector::new();
        let got = ej.collect_new_items(&d, MsgTier::Normal, &MsgFilter::default());
        let ids: Vec<u64> = got.iter().map(|i| i.row_id()).collect();
        assert_eq!(ids, vec![11, 12], "tied timestamps must order by row id");

        // And it agrees with the shared collector used by replay/vertical.
        let shared: Vec<u64> = filter::collect_items(&d, MsgTier::Normal, &MsgFilter::default())
            .iter()
            .map(|i| i.row_id())
            .collect();
        assert_eq!(ids, shared);
    }

    #[test]
    fn collect_new_items_advances_watermark_past_filtered_out_rows() {
        let d = data_with(vec![msg(5, "keep"), msg(6, "drop")], vec![]);
        let mut ej = Ejector::new();
        let got = ej.collect_new_items(&d, MsgTier::Compact, &MsgFilter::parse("keep"));
        assert_eq!(got.len(), 1);
        // id 6 was filtered out but still moves the watermark forward.
        assert_eq!(ej.last_msg_id, 6);
    }

    #[test]
    fn begin_replay_collapses_lifecycle_runs_and_breaks_on_boundaries() {
        let life = |id: u64, t: f64, agent: &str| {
            ev_at(id, t, agent, EventKind::Activity(ActivityKind::StateChange))
        };
        // agent a: 4 lifecycle events inside minute 0 → collapses to one run.
        // agent b: 1 lifecycle event → stays plain.
        // agent a: 2 more lifecycle events in minute 1 → stay plain (run < 3).
        let d = data_with(
            vec![],
            vec![
                life(1, 0.0, "a"),
                life(2, 10.0, "a"),
                life(3, 20.0, "a"),
                life(4, 30.0, "a"),
                life(5, 40.0, "b"),
                life(6, 61.0, "a"),
                life(7, 62.0, "a"),
            ],
        );
        let mut ej = Ejector::new();
        ej.begin_replay(
            &d,
            MsgTier::Verbose,
            &d_filter(),
            ReplayReason::FilterChange,
        );

        assert_eq!(
            run_counts(&ej),
            vec![4],
            "the 4-event same-minute run collapses"
        );
        // ids 5 (agent b) and 6,7 (agent a, next minute, run of 2) stay plain.
        assert_eq!(replay_ids(&ej), vec![5, 6, 7]);
    }

    #[test]
    fn a_message_breaks_a_lifecycle_run() {
        let life =
            |id: u64, t: f64| ev_at(id, t, "a", EventKind::Activity(ActivityKind::StateChange));
        let mut d = data_with(
            vec![{
                let mut m = msg(3, "interrupt");
                m.time = 15.0;
                m
            }],
            vec![life(1, 0.0), life(2, 10.0), life(4, 20.0), life(5, 30.0)],
        );
        d.messages[0].time = 15.0;
        let mut ej = Ejector::new();
        ej.begin_replay(
            &d,
            MsgTier::Verbose,
            &d_filter(),
            ReplayReason::FilterChange,
        );

        // The message at t=15 splits the minute-0 run into 2 + 2 — neither collapses.
        assert!(run_counts(&ej).is_empty());
        assert_eq!(replay_ids(&ej), vec![1, 2, 3, 4, 5]);
    }

    fn sep_text(s: &PendingSep, width: u16) -> String {
        filter_separator_lines(s, width)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.to_string())
            .collect()
    }

    fn pending(conds: &str, matched: usize, total: usize) -> PendingSep {
        PendingSep {
            tier: MsgTier::Compact,
            filter: MsgFilter::parse("x"),
            conds: conds.to_string(),
            limit: 200,
            matched,
            total,
        }
    }

    // Regression: the count was appended after the conditions and `separator`
    // does not truncate, so a long filter pushed `[matched/total]` off the right
    // edge. The conditions are the part that may be clipped.
    #[test]
    fn separator_keeps_the_count_when_conditions_are_long() {
        let wide = "thread:some-really-long-thread-name \u{00b7} from:review-tomo \u{00b7} \"a long free text query\"";
        let cjk = "thread:\u{4f1a}\u{8b70}\u{8a18}\u{9332}\u{4f1a}\u{8b70}\u{8a18}\u{9332}\u{4f1a}\u{8b70}\u{8a18}\u{9332} \u{00b7} from:\u{30ec}\u{30d3}\u{30e5}\u{30fc}";
        for conds in [wide, cjk] {
            for width in [40u16, 60, 80] {
                let s = pending(conds, 3, 42);
                let line = &filter_separator_lines(&s, width)[1];
                let text: String = line.spans.iter().map(|sp| sp.content.to_string()).collect();
                assert!(
                    text.contains("[3/42]"),
                    "count dropped at width {width}: {text:?}"
                );
                let w: usize = line.spans.iter().map(|sp| sp.width()).sum();
                assert!(w <= width as usize, "separator overflows {width}: {w}");
            }
        }
    }

    #[test]
    fn separator_keeps_short_conditions_intact() {
        let text = sep_text(&pending("from:nova", 1, 9), 80);
        assert!(text.contains("from:nova"), "{text:?}");
        assert!(text.contains("[1/9]"), "{text:?}");
        assert!(text.contains("recent (limit 200)"), "{text:?}");
    }

    // Regression: the old name_map used bare names for tagged remote agents,
    // causing incorrect identities and missing read receipts in inline mode.
    #[test]
    fn inline_replay_resolves_names_like_the_vertical_pane() {
        let mut d = DataState::empty();
        let mut remote = crate::tui::test_helpers::make_test_agent("nova", 10.0);
        remote.tag = "dev".into();
        remote.device_name = Some("BOXE".into());
        remote.last_event_id = Some(9);
        d.remote_agents = vec![remote];
        let mut local = crate::tui::test_helpers::make_test_agent("ligo", 10.0);
        local.tag = "dev".into();
        local.last_event_id = Some(9);
        d.agents = vec![local];

        let mut ej = Ejector::new();
        ej.refresh_waterlines(&d);
        let wl = ej.waterlines.clone();

        // A message addressed to the remote agent's raw storage identity.
        let mut m = msg(5, "hi");
        m.recipients = vec!["nova:BOXE".into(), "ligo".into()];
        m.scope = MessageScope::Mentions;
        let row = ReplayRow::Item(EjectItem::Msg(m));

        let resolve = resolver(&d);
        let text: String = format_row_lines(&row, 120, false, &resolve, None, Some(&wl))
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.to_string())
            .collect();

        assert!(
            text.contains("dev-nova:BOXE"),
            "remote recipient must render its resolved identity: {text:?}"
        );
        assert!(
            text.matches('\u{2713}').count() == 2,
            "both recipients are at/behind the waterline: {text:?}"
        );
    }

    fn d_filter() -> MsgFilter {
        MsgFilter::default()
    }
}
