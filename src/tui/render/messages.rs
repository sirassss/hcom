use std::collections::HashMap;

use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::tui::app::{App, DataState};
use crate::tui::filter::{self, FeedItem};
use crate::tui::model::*;
use crate::tui::render::text::{fmt_agent, highlight_spans, render_body};
use crate::tui::theme::{Theme, palette};

/// Read-receipt waterlines keyed by device-qualified display identity, so a
/// `✓` lookup on a resolved recipient name matches local and remote agents
/// the same way. Shared by the vertical pane and inline replay.
pub(crate) fn build_waterlines(data: &DataState) -> HashMap<String, u64> {
    data.agents
        .iter()
        .chain(data.remote_agents.iter())
        .filter_map(|a| a.last_event_id.map(|id| (a.display_name(), id)))
        .collect()
}

const DISPLAY_LIMIT: usize = 5000; // vertical mode loads up to 5000 from DB

use super::truncate_display;

pub fn render_messages(frame: &mut Frame, area: Rect, app: &App) -> usize {
    if let Some(ref cr) = app.ui.command_result {
        return render_command_output(frame, area, &cr.output, app.ui.msg_scroll);
    }

    // One shared collect/filter path for the whole pane.
    let mut items = filter::collect_items(&app.data, app.ui.msg_tier, &app.ui.msg_filter);
    // Newest DISPLAY_LIMIT after filtering; counts stay uncapped.
    if items.len() > DISPLAY_LIMIT {
        items.drain(..items.len() - DISPLAY_LIMIT);
    }

    if items.is_empty() {
        let empty = vec![
            Line::raw(""),
            Line::from(Span::styled(
                empty_state_label(app),
                Style::default().fg(palette::FG_DIM),
            )),
        ];
        frame.render_widget(Paragraph::new(empty), area);
        return 0;
    }

    let query = app.active_search_query();
    let resolve_name = |name: &str| app.data.resolve_display_name(name);
    let wl = build_waterlines(&app.data);
    let cursor_agent = cursor_agent_name(app);
    let verbose = app.ui.msg_tier == filter::MsgTier::Verbose;

    let mut lines: Vec<Line> = Vec::new();
    let mut prev_group: Option<(String, String)> = None;
    for row in group_lifecycle(&items) {
        match row {
            Row::LifecycleRun { agent, time, count } => {
                prev_group = None;
                lines.push(lifecycle_run_line(
                    &resolve_name(&agent),
                    count,
                    &format_time(time),
                    area.width,
                ));
            }
            Row::Item(FeedItem::Ev(ev)) => {
                let time_str = format_time(ev.time);
                let same = prev_group
                    .as_ref()
                    .is_some_and(|(a, t)| a == &ev.agent && t == &time_str);
                prev_group = Some((ev.agent.clone(), time_str.clone()));
                lines.push(event_line(
                    ev,
                    &time_str,
                    same,
                    true,
                    area.width,
                    query,
                    &resolve_name,
                ));
                // Tool details are one line; lifecycle sub-lines only in Verbose.
                if verbose && matches!(ev.kind, EventKind::Activity(_)) {
                    push_sub_lines(&mut lines, ev, query);
                }
            }
            Row::Item(FeedItem::Msg(msg)) => {
                prev_group = None;
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                let mut ml = format_message(msg, area.width, query, &resolve_name, Some(&wl));
                // Blue │ margin for messages involving the cursor agent.
                if involves_agent(app, msg, cursor_agent.as_deref())
                    && let Some(first) = ml.first_mut()
                    && let Some(s0) = first.spans.first_mut()
                {
                    *s0 = Span::styled(" \u{2502}", Style::default().fg(palette::BLUE));
                }
                lines.extend(ml);
                lines.push(Line::raw(""));
            }
        }
    }

    render_scrolled(frame, area, lines, app.ui.msg_scroll)
}

/// Empty-pane wording, matching the inline replay separator (spec §1).
fn empty_state_label(app: &App) -> &'static str {
    use crate::tui::filter::MsgTier;
    if !app.ui.msg_filter.is_empty() {
        "  No matches in recent window"
    } else if app.ui.msg_tier == MsgTier::Compact {
        "  No messages in recent window"
    } else {
        "  No activity in recent window"
    }
}

/// Name of the agent under the cursor (local base, remote display, stopped
/// base), used only for the blue involvement margin.
fn cursor_agent_name(app: &App) -> Option<String> {
    match app.cursor_target() {
        CursorTarget::Agent(idx) => Some(app.data.agents[idx].name.clone()),
        CursorTarget::RemoteAgent(idx) => Some(app.data.remote_agents[idx].display_name()),
        CursorTarget::StoppedAgent(idx) => Some(app.data.stopped_agents[idx].name.clone()),
        _ => None,
    }
}

/// Identity-resolved, like the roster filter condition (spec §4): a message
/// addressed to `review-tomo` marks the cursor row for agent `tomo`.
fn involves_agent(app: &App, msg: &Message, name: Option<&str>) -> bool {
    name.is_some_and(|n| {
        app.same_agent(&msg.sender, n) || msg.recipients.iter().any(|r| app.same_agent(r, n))
    })
}

// ── Shared feed rendering helpers ───────────────────────────────────

/// Render a single event (tool or activity) as a Line with right-aligned time.
pub(crate) fn event_line(
    ev: &Event,
    time_str: &str,
    same: bool,
    show_agent: bool,
    width: u16,
    query: Option<&str>,
    resolve_name: &dyn Fn(&str) -> String,
) -> Line<'static> {
    let right_time: Vec<Span> = if same {
        vec![Span::raw("       ")] // 5 (time) + 2 (margin)
    } else {
        vec![
            Span::styled(format!(" {}", time_str), Theme::dim()),
            Span::raw("  "),
        ]
    };
    let right_w: usize = right_time.iter().map(|s| s.width()).sum();

    let agent_display = resolve_name(&ev.agent);
    let agent_col_w = unicode_width::UnicodeWidthStr::width(agent_display.as_str()).max(4) + 1;

    match ev.kind {
        EventKind::Tool => {
            let tc = tool_color(&ev.tool);
            let mut spans = vec![Span::raw("  ")];
            if show_agent {
                spans.push(if same {
                    Span::raw(" ".repeat(agent_col_w))
                } else {
                    Span::styled(
                        fmt_agent(&agent_display, agent_col_w),
                        Style::default().fg(palette::FG_DIM),
                    )
                });
            }
            spans.push(Span::styled(
                format!("{} ", ev.tool),
                Style::default().fg(tc),
            ));
            let prefix_w: usize = spans.iter().map(|s| s.width()).sum();
            let margin = 2usize;
            // Take the first detail line only, shorten a long absolute path for
            // file tools, then clip to the columns that remain.
            let full_avail = (width as usize).saturating_sub(prefix_w + margin);
            let first_line = ev.detail.lines().next().unwrap_or("");
            let shortened = shorten_tool_detail(&ev.tool, first_line);
            let detail_text = truncate_display(&shortened, full_avail);
            spans.extend(highlight_spans(
                vec![Span::styled(detail_text, Style::default().fg(palette::FG))],
                query,
            ));
            let left_w: usize = spans.iter().map(|s| s.width()).sum();
            if left_w + right_w <= width as usize {
                let pad = (width as usize).saturating_sub(left_w + right_w);
                spans.push(Span::raw(" ".repeat(pad)));
                spans.extend(right_time);
            }
            Line::from(spans)
        }
        EventKind::Activity(kind) => {
            let (icon, color) = match kind {
                ActivityKind::Started => ("\u{25b6}", palette::GREEN),
                ActivityKind::Active => ("\u{25b6}", palette::GREEN),
                ActivityKind::Listening => ("\u{25c9}", palette::CYAN),
                ActivityKind::Stopped => ("\u{25cb}", palette::FG_DIM),
                ActivityKind::Blocked => ("\u{25a0}", palette::RED),
                ActivityKind::StateChange => ("\u{25c6}", palette::YELLOW),
            };
            let detail = if kind == ActivityKind::Active {
                format!("active: {}", ev.detail)
            } else {
                ev.detail.clone()
            };
            let mut spans = vec![Span::raw("  ")];
            if show_agent {
                spans.push(if same {
                    Span::raw(" ".repeat(agent_col_w))
                } else {
                    Span::styled(
                        fmt_agent(&agent_display, agent_col_w),
                        Style::default().fg(palette::FG_DIM),
                    )
                });
            }
            spans.push(Span::styled(
                format!("{} ", icon),
                Style::default().fg(color),
            ));
            let prefix_w: usize = spans.iter().map(|s| s.width()).sum();
            let margin = 2usize;
            let full_avail = (width as usize).saturating_sub(prefix_w + margin);
            let detail_text = truncate_display(&detail, full_avail);
            spans.extend(highlight_spans(
                vec![Span::styled(detail_text, Style::default().fg(color))],
                query,
            ));
            let left_w: usize = spans.iter().map(|s| s.width()).sum();
            if left_w + right_w <= width as usize {
                let pad = (width as usize).saturating_sub(left_w + right_w);
                spans.push(Span::raw(" ".repeat(pad)));
                spans.extend(right_time);
            }
            Line::from(spans)
        }
    }
}

// ── Lifecycle run collapsing ───────────────────────────────────────

/// One row of feed output: a real item, or a collapsed lifecycle run.
pub(crate) enum Row<'a> {
    Item(&'a FeedItem<'a>),
    LifecycleRun {
        agent: String,
        time: f64,
        count: usize,
    },
}

/// Absolute-minute bucket for a timestamp (`floor(time / 60)`), not the
/// repeated `HH:MM` label — two different dates with the same clock time do
/// not merge.
fn minute_bucket(t: f64) -> i64 {
    (t / 60.0).floor() as i64
}

/// Collapse a run of 3+ consecutive lifecycle events sharing an owner and an
/// absolute minute into one summary row. Runs of 1–2 stay as plain items. A
/// message, tool event, different agent or minute breaks the run. Grouping
/// happens after filtering/sorting and before any line-budget chunking.
pub(crate) fn group_lifecycle<'a>(items: &'a [FeedItem<'a>]) -> Vec<Row<'a>> {
    let is_life =
        |it: &FeedItem| matches!(it, FeedItem::Ev(e) if matches!(e.kind, EventKind::Activity(_)));
    let mut rows = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if is_life(&items[i]) {
            let FeedItem::Ev(head) = &items[i] else {
                unreachable!()
            };
            let bucket = minute_bucket(head.time);
            let mut j = i + 1;
            while j < items.len() {
                match &items[j] {
                    FeedItem::Ev(e)
                        if matches!(e.kind, EventKind::Activity(_))
                            && e.agent == head.agent
                            && minute_bucket(e.time) == bucket =>
                    {
                        j += 1;
                    }
                    _ => break,
                }
            }
            if j - i >= 3 {
                rows.push(Row::LifecycleRun {
                    agent: head.agent.clone(),
                    time: head.time,
                    count: j - i,
                });
            } else {
                rows.extend(items[i..j].iter().map(Row::Item));
            }
            i = j;
        } else {
            rows.push(Row::Item(&items[i]));
            i += 1;
        }
    }
    rows
}

/// `agent  · N status changes ·` with a right-aligned dim time.
pub(crate) fn lifecycle_run_line(
    agent_display: &str,
    count: usize,
    time_str: &str,
    width: u16,
) -> Line<'static> {
    let agent_col_w = unicode_width::UnicodeWidthStr::width(agent_display).max(4) + 1;
    let right = format!(" {}  ", time_str);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(
            fmt_agent(agent_display, agent_col_w),
            Style::default().fg(palette::FG_DIM),
        ),
        Span::styled(
            format!("\u{00b7} {} status changes \u{00b7}", count),
            Theme::dim(),
        ),
    ];
    let left_w: usize = spans.iter().map(|s| s.width()).sum();
    let right_w = unicode_width::UnicodeWidthStr::width(right.as_str());
    let pad = (width as usize).saturating_sub(left_w + right_w);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Theme::dim()));
    Line::from(spans)
}

/// Push sub_lines (stopped snapshot details etc.) as indented dim lines.
/// The last sub_line starting with "hcom " is styled as an actionable command.
/// Sub-lines are searchable (spec §3), so the hit is highlighted here too —
/// the inline replay already does.
fn push_sub_lines(lines: &mut Vec<Line<'static>>, ev: &Event, query: Option<&str>) {
    for sub in &ev.sub_lines {
        let style = if sub.starts_with("hcom ") {
            Style::default().fg(palette::CYAN)
        } else {
            Style::default().fg(palette::FG_DIM)
        };
        lines.push(Line::from(highlight_spans(
            vec![Span::styled(format!("        {}", sub), style)],
            query,
        )));
    }
}

/// Format a message (header + body) as lines with right-aligned time.
pub(crate) fn format_message(
    msg: &Message,
    width: u16,
    query: Option<&str>,
    resolve_name: &dyn Fn(&str) -> String,
    waterlines: Option<&HashMap<String, u64>>,
) -> Vec<Line<'static>> {
    let mut header_spans: Vec<Span> = vec![Span::raw("  ")];
    push_msg_header(&mut header_spans, msg, query, resolve_name, waterlines);

    // Right-align time
    let time_str = format!(" {}  ", format_time(msg.time));
    let left_w: usize = header_spans.iter().map(|s| s.width()).sum();
    let time_w = Span::raw(&time_str).width();
    let pad = (width as usize).saturating_sub(left_w + time_w);
    header_spans.push(Span::raw(" ".repeat(pad)));
    header_spans.push(Span::styled(time_str, Theme::dim()));

    let mut lines = vec![Line::from(header_spans)];
    if !msg.is_system() {
        lines.extend(render_body(&msg.body, width as usize, query));
    }
    lines
}

fn push_msg_header(
    spans: &mut Vec<Span<'static>>,
    msg: &Message,
    query: Option<&str>,
    resolve_name: &dyn Fn(&str) -> String,
    waterlines: Option<&HashMap<String, u64>>,
) {
    if msg.is_system() {
        spans.extend(highlight_spans(
            vec![Span::styled(
                msg.body.clone(),
                Style::default().fg(palette::YELLOW),
            )],
            query,
        ));
    } else {
        spans.extend(highlight_spans(
            vec![Span::styled(
                resolve_name(&msg.sender),
                Style::default().fg(palette::FG),
            )],
            query,
        ));
        spans.push(Span::styled(" \u{2192} ", Theme::dim()));

        if msg.scope == MessageScope::Broadcast {
            spans.push(Span::styled("all", Theme::dim()));
        } else if msg.recipients.is_empty() {
            // Malformed Mentions row with no recipients — never relabel as "all".
            spans.push(Span::styled("?", Theme::dim()));
        } else {
            // First two resolved names as their own spans (each keeps its ✓ and
            // highlight), then a `+N` for the rest.
            let shown = msg.recipients.len().min(2);
            for (i, r) in msg.recipients.iter().take(shown).enumerate() {
                if i > 0 {
                    spans.push(Span::styled(", ", Theme::dim()));
                }
                let display = resolve_name(r);
                spans.extend(highlight_spans(
                    vec![Span::styled(
                        display.clone(),
                        Style::default().fg(palette::FG),
                    )],
                    query,
                ));
                if let Some(wl) = waterlines
                    && wl.get(&display).is_some_and(|&w| w >= msg.event_id)
                {
                    spans.push(Span::styled(" \u{2713}", Theme::delivery()));
                }
            }
            if msg.recipients.len() > shown {
                spans.push(Span::styled(
                    format!(" +{}", msg.recipients.len() - shown),
                    Theme::dim(),
                ));
            }
        }

        // Intent badge
        if let Some(ref intent) = msg.intent {
            let (label, color) = match intent.as_str() {
                "request" => ("req", palette::ORANGE),
                "ack" => ("ack", palette::FG_DIM),
                _ => (intent.as_str(), palette::FG_DIM),
            };
            spans.push(Span::styled(
                format!(" [{}]", label),
                Style::default().fg(color),
            ));
        }

        // Reply-to reference
        if let Some(id) = msg.reply_to {
            spans.push(Span::styled(
                format!(" \u{21b5}{}", id),
                Style::default().fg(palette::FG_DIM),
            ));
        }
    }
}

/// Shorten a long absolute path to `…/parent/file` for file-oriented tools.
/// A Bash/shell command is left alone even when it starts with `/`.
fn shorten_tool_detail(tool: &str, detail: &str) -> String {
    const FILE_TOOLS: &[&str] = &[
        "Read",
        "Edit",
        "Write",
        "write_file",
        "apply_patch",
        "replace",
        "NotebookEdit",
    ];
    if !FILE_TOOLS.contains(&tool)
        || !detail.starts_with('/')
        || detail.contains(char::is_whitespace)
    {
        return detail.to_string();
    }
    let parts: Vec<&str> = detail.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        return detail.to_string();
    }
    format!(
        "\u{2026}/{}/{}",
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

/// Resolve tool name to display color.
fn tool_color(tool: &str) -> Color {
    match tool {
        "Edit" | "Write" | "write_file" | "apply_patch" | "replace" => palette::YELLOW,
        "Bash" | "shell" | "run_shell_command" => palette::TEAL,
        "Grep" | "WebSearch" | "grep_search" | "search_file_content" | "google_web_search" => {
            palette::MAGENTA
        }
        _ => palette::BLUE,
    }
}

/// Short count string for the panel heading and inline separator. `[42]`
/// unfiltered, `[3/42]` with any condition active; tier-relative and computed
/// before the display cap (spec §1). Appends ` showing N` when the vertical
/// pane's `DISPLAY_LIMIT` actually truncates the matched set.
pub fn display_count_str(app: &App) -> String {
    let (matched, total) = filter::counts(&app.data, app.ui.msg_tier, &app.ui.msg_filter);
    let mut s = if app.ui.msg_filter.is_empty() {
        format!("[{}]", total)
    } else {
        format!("[{}/{}]", matched, total)
    };
    let shown = if app.ui.msg_filter.is_empty() {
        total
    } else {
        matched
    };
    if shown > DISPLAY_LIMIT {
        s.push_str(&format!(" showing {}", DISPLAY_LIMIT));
    }
    s
}

fn render_command_output(
    frame: &mut Frame,
    area: Rect,
    output: &[String],
    msg_scroll: usize,
) -> usize {
    let mut lines: Vec<Line> = Vec::new();
    for line in output {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(line.clone(), Style::default().fg(palette::FG)),
        ]));
    }

    render_scrolled(frame, area, lines, msg_scroll)
}

/// Render lines with bottom-anchored scroll and an auto-hiding scrollbar.
/// Returns `max_scroll` so callers can clamp `msg_scroll`.
fn render_scrolled(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'_>>,
    scroll_from_bottom: usize,
) -> usize {
    let total = lines.len();
    let visible = area.height as usize;
    let max_scroll = total.saturating_sub(visible);
    let effective = scroll_from_bottom.min(max_scroll);
    let scroll_pos = max_scroll - effective;

    let paragraph = Paragraph::new(lines).scroll((scroll_pos.min(u16::MAX as usize) as u16, 0));
    frame.render_widget(paragraph, area);

    if total > visible {
        let mut state = ScrollbarState::new(total).position(scroll_pos);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_style(Style::default().fg(palette::FG_DIM)),
            area,
            &mut state,
        );
    }

    max_scroll
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::{ActivityKind, MessageScope, SenderKind};

    fn mk_msg(recipients: &[&str], scope: MessageScope) -> Message {
        Message {
            event_id: 1,
            sender: "bono".into(),
            recipients: recipients.iter().map(|s| s.to_string()).collect(),
            body: "hi".into(),
            time: 0.0,
            delivered: vec![],
            scope,
            sender_kind: SenderKind::Instance,
            intent: None,
            reply_to: None,
            thread: None,
            delivery_known: false,
        }
    }

    fn header_text(msg: &Message) -> String {
        let id = |s: &str| s.to_string();
        let lines = format_message(msg, 80, None, &id, None);
        lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn broadcast_header_says_all_and_empty_mentions_says_unknown() {
        assert!(header_text(&mk_msg(&[], MessageScope::Broadcast)).contains("→ all"));
        // Malformed Mentions row with no recipients must not be relabelled "all".
        let h = header_text(&mk_msg(&[], MessageScope::Mentions));
        assert!(h.contains("→ ?"), "got {h:?}");
        assert!(!h.contains("all"));
    }

    #[test]
    fn header_shows_first_two_recipients_then_plus_n() {
        let h = header_text(&mk_msg(
            &["ligo", "hana", "sumo", "kai"],
            MessageScope::Mentions,
        ));
        assert!(h.contains("ligo"));
        assert!(h.contains("hana"));
        assert!(h.contains("+2"), "got {h:?}");
        assert!(!h.contains("sumo"));
    }

    // Regression: the blue involvement margin compared raw strings, so a message
    // addressed to `review-tomo` did not mark the cursor row for agent `tomo` —
    // while the roster filter condition (same identity rules) did match it.
    #[test]
    fn blue_margin_follows_resolved_identity() {
        let mut app = App::new();
        let mut agent = crate::tui::test_helpers::make_test_agent("tomo", 5.0);
        agent.tag = "review".into();
        app.data.agents = vec![agent];

        let tagged = mk_msg(&["review-tomo"], MessageScope::Mentions);
        assert!(
            involves_agent(&app, &tagged, Some("tomo")),
            "tag-qualified recipient must mark its agent"
        );

        let mut from_agent = mk_msg(&["ligo"], MessageScope::Mentions);
        from_agent.sender = "review-tomo".into();
        assert!(involves_agent(&app, &from_agent, Some("tomo")));

        let other = mk_msg(&["ligo"], MessageScope::Mentions);
        assert!(!involves_agent(&app, &other, Some("tomo")));
    }

    fn life(id: u64, t: f64, agent: &str) -> Event {
        Event {
            row_id: id,
            agent: agent.into(),
            time: t,
            kind: EventKind::Activity(ActivityKind::StateChange),
            tool: String::new(),
            detail: "x".into(),
            sub_lines: vec![],
        }
    }

    fn tool(id: u64, t: f64) -> Event {
        Event {
            row_id: id,
            agent: "a".into(),
            time: t,
            kind: EventKind::Tool,
            tool: "Bash".into(),
            detail: "ls".into(),
            sub_lines: vec![],
        }
    }

    #[test]
    fn group_lifecycle_collapses_three_and_keeps_two() {
        let evs = [life(1, 0.0, "a"), life(2, 5.0, "a"), life(3, 10.0, "a")];
        let items: Vec<FeedItem> = evs.iter().map(FeedItem::Ev).collect();
        let rows = group_lifecycle(&items);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::LifecycleRun { count: 3, .. }));

        let two = [life(1, 0.0, "a"), life(2, 5.0, "a")];
        let items: Vec<FeedItem> = two.iter().map(FeedItem::Ev).collect();
        assert_eq!(group_lifecycle(&items).len(), 2);
        assert!(
            group_lifecycle(&items)
                .iter()
                .all(|r| matches!(r, Row::Item(_)))
        );
    }

    #[test]
    fn group_lifecycle_breaks_on_tool_agent_and_minute() {
        // tool event in the middle breaks the run into 2 + 1
        let evs = [
            life(1, 0.0, "a"),
            life(2, 5.0, "a"),
            tool(3, 7.0),
            life(4, 8.0, "a"),
        ];
        let items: Vec<FeedItem> = evs.iter().map(FeedItem::Ev).collect();
        assert!(
            group_lifecycle(&items)
                .iter()
                .all(|r| matches!(r, Row::Item(_)))
        );

        // different agent breaks
        let evs = [life(1, 0.0, "a"), life(2, 5.0, "b"), life(3, 10.0, "a")];
        let items: Vec<FeedItem> = evs.iter().map(FeedItem::Ev).collect();
        assert!(
            group_lifecycle(&items)
                .iter()
                .all(|r| matches!(r, Row::Item(_)))
        );

        // crossing a minute boundary breaks (same HH:MM label irrelevant)
        let evs = [life(1, 55.0, "a"), life(2, 58.0, "a"), life(3, 61.0, "a")];
        let items: Vec<FeedItem> = evs.iter().map(FeedItem::Ev).collect();
        assert!(
            group_lifecycle(&items)
                .iter()
                .all(|r| matches!(r, Row::Item(_)))
        );
    }

    #[test]
    fn shorten_tool_detail_only_touches_file_tool_paths() {
        assert_eq!(
            shorten_tool_detail("Edit", "/home/alam/workspaces/hcom/src/tui/db.rs"),
            "\u{2026}/tui/db.rs"
        );
        // Bash command starting with '/' is left intact.
        assert_eq!(
            shorten_tool_detail("Bash", "/usr/bin/env python -m pytest"),
            "/usr/bin/env python -m pytest"
        );
        // Short path unchanged.
        assert_eq!(shorten_tool_detail("Read", "/etc/hosts"), "/etc/hosts");
        // Relative path unchanged.
        assert_eq!(
            shorten_tool_detail("Read", "src/tui/db.rs"),
            "src/tui/db.rs"
        );
    }

    #[test]
    fn lifecycle_run_line_fits_width() {
        let l = lifecycle_run_line("agent", 4, "23:01", 40);
        let w: usize = l.spans.iter().map(|s| s.width()).sum();
        assert!(w <= 40, "run line width {w} exceeds 40");
        let text: String = l
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.contains("4 status changes"));
    }
}
