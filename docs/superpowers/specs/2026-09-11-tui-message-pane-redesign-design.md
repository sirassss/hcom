# TUI message pane redesign — design

**Status:** implemented 2026-09-11 (tier persistence §2 deferred — optional by design)
**Date:** 2026-09-11
**Owner:** siras

---

## Problem

The TUI has two viewports and **`Inline` is the default** (`src/tui/app.rs:63`). Inline renders
into terminal scrollback via `src/tui/inline/eject.rs`; `Vertical` (reached with `\`) renders the
pane in `src/tui/render/messages.rs`. Each has its own, differently-broken filter path. Three
defects:

1. **Everything is ejected unconditionally in inline mode.** `prepare_inline_viewport`
   (`src/tui/mod.rs:78`) sets `eject_filter = None`, and the predicates treat `None` as
   pass-all:

   ```rust
   // eject.rs:428
   let agent_ok = match filter { None => true, Some(names) => names.contains(&ev.agent) };
   ```

   So every tool call, every `◉ listening`, every `▶ active: deliver:x` lands in scrollback with
   no user action and no way to turn it off. This is the reported symptom ("I selected nothing and
   still see bash lines") and it requires no user error to explain.

   The vertical pane has a second, smaller instance of the same root cause — *density is implicit
   state*. `render_messages` silently switches between `render_all_messages` (messages only),
   `render_agent_feed` (messages + tool calls + lifecycle), and `render_events_timeline` (dead —
   nothing ever sets `show_events = true`). Selecting a roster agent (`space` / `enter` / `a`)
   flips the whole pane into the noisy feed with no in-pane indicator; the only exit is an
   undocumented `Esc` cascade.

2. **Filter silently drops matches, differently in each viewport.** When a query is active,
   `actions.rs:51` calls `search_timeline`, which is an **FTS5 quoted-phrase search**:

   ```rust
   // db.rs:835-840
   let fts_query = format!("\"{}\"", stripped);
   conn.prepare("SELECT … JOIN events_fts ON events_fts.rowid = e.id WHERE events_fts MATCH ?")
   ```

   FTS5 phrase matching is not literal substring matching: mid-word queries such as `eliver`
   can miss `deliver:bono`, and punctuation is treated according to token boundaries. Whole
   punctuated phrases and dates can still match; the defect is not that they never match.
   When FTS results exist the client-side text filter is
   bypassed entirely (`eject.rs:106` passes `eff_text = &None`), so anything FTS misses is gone
   rather than caught by the fallback.

   The fallbacks are also narrow and inconsistent:
   - inline `message_matches_filter` (eject.rs:462) searches only `body` + `sender`
   - vertical `msg_matches` (messages.rs:~595) searches `body` + `sender` + raw `recipients`

   Neither searches intent, thread, reply-to, or the **resolved** display name (`tag-name`) the
   user actually sees.

3. **Formatting is noisy.** Every empty-detail `bash` / `read` and every lifecycle change is
   its own full line. The proportion of noise has not been measured.

The `Message` model also drops the `thread` field on parse, `Message.delivered` is parsed but
unused (`#[allow(dead_code)]`), and there is no notion of an agent `tag` at the message/event
level — so filtering by tag, thread, or true delivery is impossible today.

## Goals

- One mental model for the pane: **it always shows messages at the current detail tier; tag,
  thread, to, from, and agent are all stacked filters on top.**
- A detail tier the user controls explicitly, defaulting to the most compact.
- A literal substring filter that never silently drops a match within the loaded recent
  window; the window and any display truncation must be visible.
- Readable message formatting.
- Quick read of "what is being sent to the floor-lead".
- Delete the dead `show_events` / `render_events_timeline` code.

## Non-goals

- No DB schema change.
- No change to message delivery, hooks, or the CLI `hcom events` command.
- No recipient-expansion UI for `+N` (deferred).
- No all-history search in this pane. The user explicitly chose recent-window filtering
  during review on 2026-09-11; removing the old FTS history search is intentional.
- Preserve append-only inline scrollback: filter/tier changes append a labelled replay and
  affect subsequent output. Previously emitted terminal history is not erased or rewritten.

---

## Design

### 1. One filter+tier module, shared by both viewports

The core of this change is a single decision function that **both** viewports call, replacing
four divergent predicates (`eject.rs::event_matches_filter`, `eject.rs::message_matches_filter`,
`messages.rs::msg_matches`, `messages.rs::event_matches`).

New module `src/tui/filter.rs`:

```rust
pub enum MsgTier { Compact, Normal, Verbose }   // default Compact
pub struct MsgFilter { /* §3 */ }

/// The single gate for timeline items (not command output or UI chrome).
pub fn passes(item: &FeedItem, tier: MsgTier, f: &MsgFilter, data: &DataState) -> bool {
    tier_admits(item, tier) && f.matches(item, data)
}
```

`tier_admits` is the fix for defect 1 — density stops being implicit state and becomes one
explicit parameter that both viewports read from `ui.msg_tier`.

**Inline (`inline/eject.rs`)** — the default viewport, and the one the user is actually looking at:

- `begin_replay` and `eject_new` swap their `event_matches_filter` / `message_matches_filter` calls
  for `filter::passes(...)`.
- Both currently take `(filter: &Option<BTreeSet<String>>, text_filter: &Option<String>)`; replace
  that pair with `(tier: MsgTier, f: &MsgFilter)`. `ui.eject_filter` (the `BTreeSet` of agent
  names) folds into `MsgFilter.agents`, so `eject_filter` is deleted.
- The FTS shortcut at `eject.rs:105` and `eject.rs:475` is removed along with `search_results`
  (§3), so the source is always `data.events` / `data.messages` with `passes` applied — no more
  bypassed text filter.

**Vertical (`render/messages.rs`)** — collapse the three-way branch to one path:

```rust
fn render_message_pane(frame, area, app) -> usize {
    if let Some(cr) = &app.ui.command_result { return render_command_output(...); } // unchanged
    let items = collect_items(&app.data, app.ui.msg_tier, &app.ui.msg_filter);
    render_lines(frame, area, items, app.ui.msg_tier, app)
}
```

- **Delete:** `render_agent_feed`, `render_events_timeline`, `UiState.show_events`,
  `UiState.eject_filter`, and the `render/mod.rs:757‑760` label branch reading `show_events`.
- **Keep:** `FeedItem { Msg, Ev }` (promote to `filter.rs` so both viewports share it),
  `render_scrolled`, `tool_color`, `format_time`, `build_waterlines`, the blue `│` margin.
- `collect_items` lives in `filter.rs` and is shared with inline replay and counts. It merges
  messages + events, applies `passes`, and sorts by `(time, database row id, item kind)` for
  deterministic ties. Compute counts before presentation truncation. Keep the newest
  `DISPLAY_LIMIT` matched items for vertical display; inline replay uses its loaded window.
  The gate covers timeline items; banners, separators and command output are UI chrome.

**Window and count contract (review decision):**

- Preserve current loading limits: inline defaults to the latest 200 raw timeline rows,
  vertical to 5000, overridden by `HCOM_TUI_TIMELINE_LIMIT`. `load_timeline` selects the
  combined message/status/life window before splitting it. A busy activity window can
  therefore contain few or no messages in Compact; do not imply a 200-message history.
- Both viewports use the same matching rules, but different default windows can yield
  different results after a viewport switch. Preserve filter/tier and force the existing
  reload when the source limit changes. Do not change the loader to scan for older matches.
- Header and inline replay separator identify scope as `recent (limit 200)` or the effective
  limit. `total` counts loaded items admitted by the current tier, before other filters;
  `matched` counts those passing every condition, before truncation or grouping.
  Show `[matched/total]` only if a condition is active; otherwise `[total]`.
- If presentation truncates matches, append `showing N`. Collapsed lifecycle runs still
  count their underlying items. A filter/tier change with zero matches must still emit an
  inline separator and `No matches in recent window`, followed by the live marker.
  Without active filters use `No messages in recent window` for empty Compact, or
  `No activity in recent window` for empty Normal/Verbose. This does not assert an empty DB.

**Inline state invariants:**

- Migrate `filtered_counts`, `pending_filter_separator`, `filter_separator_lines`, and all
  `begin_replay`/`eject_new` callers in both `app.rs` and `mod.rs`, not only item predicates.
- Replay cancellation replaces pending items/lines when filter, tier, or width changes.
  Finish pending chrome even if there are no matching replay items; `is_replaying` must
  not strand an empty-result separator.
- Incremental watermarks are scan progress, not the last displayed row. Compare each
  candidate against the watermark captured before the batch, then advance to the maximum
  of the old watermark and IDs in the full scanned batch, including filtered-out items.
  Time-sorted vectors can have
  non-monotonic IDs; updating the watermark inside the loop can skip a new row.
- Snapshot replay watermarks from all loaded candidates, not just matches. New rows that
  remain in the recent window during replay are emitted once afterwards. Inline history
  retains its existing bounded-window limitation for rows that age out before a reload.

### 2. Detail tiers

```rust
enum MsgTier { Compact, Normal, Verbose }   // default Compact
```

| tier | shows |
|------|-------|
| `Compact` | messages only |
| `Normal` | messages + tool-call events (one line each) |
| `Verbose` | + lifecycle/activity events (`listening`, `active: …`, `started`, `stopped`) |

- Key **`v`** cycles `Compact → Normal → Verbose → Compact` in Navigate mode, in both
  viewports. Compose and overlays keep `v` and `B` as input characters. In inline mode a
  tier change triggers a labelled replay (`trigger_inline_replay`) at the new density;
  older scrollback remains visible. Changing a filter must not also reset the tier.
- Persist to the `kv` table (key `tui.msg_tier`) via `HcomDb::kv_set(key, Some(value))`;
  load on startup, default `Compact` when absent or unparseable. Use a separate writable
  connection for saving: the TUI datasource sets `PRAGMA query_only=ON`. Keep preferences
  injectable for tests and do not load host preferences in fixture constructors. Persistence
  is optional; a tier that resets to `Compact` each launch is acceptable behaviour.
- Footer shows `v tier:compact`.
- `EventKind` classification for the Normal/Verbose split: `Tool` → Normal; `Activity(_)` →
  Verbose. Reuse the existing `EventKind` enum, no new field.

### 3. Filter grammar

One parsed struct drives all filtering. Entered through the existing `/` `Search` overlay — no
new input mode.

```
/tag:review thread:hcom-skill to:bigboss from:ligo  free text here
```

The leading `/` opens the overlay; it is not part of the string passed to `parse`.

```rust
struct MsgFilter {
    tag:    Option<String>,
    thread: Option<String>,
    to:     Option<String>,   // "*" = explicitly addressed (mentions scope)
    from:   Option<String>,
    text:   String,           // leftover tokens joined with ' '
    agents: BTreeSet<String>, // from roster selection, see §4
}
```

**Parse** (`MsgFilter::parse(input: &str) -> MsgFilter`):

- Split on whitespace. A token matching `^(tag|thread|to|from):(.+)$` sets that field (last one
  wins). Everything else is joined back with a single space into `text`.
- A malformed token (`tag:` with empty value, unknown key) is treated as plain text — never an
  error.

**Match** (`MsgFilter::matches(&self, item: &FeedItem, data: &DataState) -> bool`) — all present
conditions AND together:

- `tag`   — `data.tag_of(name).eq_ignore_ascii_case(tag)` where `name` is `msg.sender` /
  `ev.agent`. (`tag_of` — see §5.)
- `thread` — messages only; `msg.thread` contains `thread` (ci). An event never matches a
  `thread:` filter → events drop out when `thread:` is set.
- `to`    — messages only; **matches on `delivered` (`delivered_to`), not `recipients`**. A
  broadcast reaches bigboss without mentioning it, so matching `recipients` (the `mentions`
  array) would silently hide exactly the messages the user asked to see. This mirrors what the
  CLI equivalent does (`json_each(msg_delivered_to)`).
  - `to:X` → any name in `msg.delivered` equals `X` under the shared identity rules (§5).
  - Preserve whether `delivered_to` was an array in the original JSON with
    `Message.delivery_known: bool`. An explicit empty array means nobody was recorded as
    reached; it must not activate a legacy fallback. Absent/null/non-array means unknown.
  - Legacy fallback, only when delivery is unknown: match explicit `recipients`. Unknown
    broadcasts do not match arbitrary `to:X`; there is no evidence that X was reached.
    Document that legacy explicit addressing is approximate, and `delivered_to` itself is
    routing metadata, not proof of a read acknowledgment.
  - `to:*` → `scope == MessageScope::Mentions && !recipients.is_empty()` (explicitly
    addressed, not broadcast), regardless of delivery tracking. Broadcast classification
    uses `scope`, not an empty mentions array.
- `from`  — `msg.sender` / `ev.agent` equals `from` (ci, base or full).
- `text`  — case-insensitive Unicode substring (`str::to_lowercase`, **not** FTS) over, for a
  message: `body`, raw/resolved `sender`, every raw/resolved recipient, `intent` and its
  displayed badge (`request`/`req`), `thread`, decimal `reply_to`, formatted time; for an
  event: `tool`, `detail`, raw/resolved `agent`, `sub_lines`, activity label, formatted time.
  Match each field independently; do not create substring matches across field boundaries.
  Empty `text` always matches. Lowercasing is not Unicode normalization or full case folding.

**Remove** `app.data.search_results`, the `is_fts_search` branch in `render_all_messages`, and
any FTS query wiring that feeds it. The existing `matches_search` already uses `to_lowercase`
plus `contains`; consolidate that behavior in the shared matcher and remove the old helper
if it has no remaining caller. Highlighting consumes only the committed free-text query.

**Header** renders scope and active conditions as chips plus the count:
`recent (limit 200) · tag:review · thread:hcom-skill · to:bigboss · agent:bono,hana · [3/47]`
`[matched/total]` when any condition is active, `[total]` otherwise, using §1's tier-relative
counts before presentation truncation. Include `from:` and free text when present. Reserve
space for the count and clip long chips by display width.

**Search editing and `Esc`:** `/` pre-fills `msg_filter.to_query()`. Edits are a draft;
Enter parses and commits it while preserving `agents`. No filtering or replay per keystroke.
Escape in Search cancels the draft and preserves the committed filter (an intentional
change from today's cancel-clears-search behavior). Outside overlays, successive Escape
presses clear free `text`, then all `key:value` fields, then roster selection. Skip empty
stages. Each actual change resets message scroll and triggers inline replay. Preserve
existing help/confirmation/command-output dismissal precedence.

### 4. Selection becomes a filter, not a mode

- `space` / `enter` on a roster agent toggles its `Agent::action_name()` in `MsgFilter.agents`. The pane does
  **not** change tier or switch views — it just adds an "involves this agent" condition
  (sender or explicitly addressed recipient; for events the event owner). Use the shared
  identity resolver below, not raw string equality against a tagged remote action name.
- `a` adds all local agents; `Esc` (per the cascade) clears them.
- Roster shows `[x]` next to each agent in `MsgFilter.agents`.
- `MsgFilter.agents` empty → no agent condition (all messages).
- Broadcasts (`scope == MessageScope::Broadcast`, non-system) always pass the agent condition — they are
  addressed to everyone including the selected agents.
- Selection remains the target set for compose, kill/stop, resume, and other existing roster
  actions. Migrate every `ui.selected` reader, including `actions.rs`; do not change action
  eligibility or local/remote target isolation. Group-header toggles do not change filters.
- Preserve the existing pruning of selections absent from all loaded rosters, but replay if
  pruning changes the filter. `a` must also replay when it changes the set. Viewport switches
  preserve the entire filter and tier.

### 5. Model changes

- **`Message.thread: Option<String>`** — in `parse_message_row` (`src/tui/db.rs`) read
  `json.get("thread")` as string; absent/null/non-string → `None`. Update every `Message`
  literal found by `rg`, including test fixtures.
- **Shared agent identity lookup in `state.rs`:** use local, remote, and loaded stopped
  rosters. Recognize raw storage names (`luna`, `luna:BOXE`), `action_name()`, and tagged
  `display_name()` (`review-luna:BOXE`). Preserve device suffixes; unqualified `luna` must
  not match remote `luna:BOXE`. Do not infer identity by stripping arbitrary hyphens.
  Comparisons accept case variants; exact storage identity takes precedence over display
  aliases if names collide. `tag_of` and `resolve_display_name` use this resolver;
  unknown names retain their raw display and have no tag. Tags reflect the current loaded
  roster, not historical tags; an old agent absent from that roster cannot match `tag:`.
- **`Message.delivery_known: bool`** — parsed as described in §3, alongside `thread`.
- **`Message.delivered`** — already parsed from `delivered_to` but marked `#[allow(dead_code)]`.
  The `to:` filter (§3) makes it live; drop the attribute.
- No `Event` change — it already carries `agent`; tag is derived via `tag_of`.

### 6. `bigboss` config key

- New global config key **`bigboss`**, default `"bigboss"`. Add to `HcomConfig` (not the
  separate runtime `Config`), its `Default`, `get_field`, `set_field`, and `load_from_sources`
  string-field list. Add `TOML_KEY_MAP` mapping to `preferences.bigboss`, `FIELD_TO_ENV`
  mapping to `HCOM_BIGBOSS`. Also update `src/commands/config.rs::CONFIG_KEYS`, its separate
  `toml_path_for_key` mapping, `config_get` default and write-path validation.
  Set via `hcom config bigboss <name>`; persist in `config.toml`. Follow existing
  env → TOML → default precedence. Reject whitespace-containing values and `*`; empty
  config resets to the default. Names can include tags and remote device suffixes.
- The TUI loads `HcomConfig::load(None)` at startup into `App`. Test constructors use
  deterministic defaults; do not introduce host configuration reads into fixtures.
- Key **`B`** toggles the `to:<bigboss>` token in the active filter — adds it if absent, removes
  it if it names the same coordinator under the shared identity rules. A different existing
  `to:` is replaced with the coordinator; other conditions survive. Purely a shortcut for
  typing `/to:<name>`; the chip and count update like any other filter.

### 7. Message formatting

```
bono → ligo  [ack] ↵4832                              23:01
  No dissent. Consensus text matches my vote. H2: keep
  AGENTS.md as pointer for vanilla Codex only.

ligo → bigboss, bono +2  [req]                        23:02
  @bigboss – consensus closed, 4/4 (ligo·bono·hana·sumo)…
```

- **Header:** `sender → r1, r2 +N  [intent] ↵reply_to` … right-aligned dim `time` (2-space
  trailing margin). `+N` when more than two recipients (list the first two, then `+N`). `all`
  for broadcast scope. `✓` after a recipient whose waterline is at least `event_id`
  (existing logic). Names are resolved (`tag-name`, with device suffix for remote agents).
  Keep recipient spans separate so each retains its own receipt/highlight. A malformed
  Mentions row with no recipients uses an unknown-recipient label, not `all`.
- **Intent badge:** `[req]` orange, `[ack]` dim, other dim — unchanged from `push_msg_header`.
- **Body:** wrapped to the available display columns after margin/indent, hanging-indented
  2 spaces, one blank line between messages. Reserve time width before truncating a long
  header, using Unicode display width and saturating arithmetic at narrow widths.
  System messages: yellow, body-in-header, no separate body line (unchanged).
- **Tool event (Normal/Verbose):** `agent  ⟩ bash  <command, first line only>` /
  `agent  ⟩ edit  <path shortened .../…/file>`; colour from `tool_color`. Consecutive events
  from the same agent within the same minute share the agent prefix (blank it on rows after the
  first — the existing `prev_group` trick).
- **Lifecycle event (Verbose):** `agent  ◉ listening` / `agent  ▶ active: deliver:x`. Collapse a
  run of consecutive lifecycle events from the same agent within the same absolute minute
  (`floor(timestamp / 60)`, not the repeated `HH:MM` label) into one line:
  `agent  · 3 status changes ·` when the run is longer than 2; otherwise render them plainly.
- Group only after filtering and sorting. A message, tool event, different agent, or minute
  breaks a lifecycle run. During replay, group before splitting into line-budget chunks.
  Live inline grouping is limited to the current emission batch: already emitted lines
  cannot be collapsed retroactively. Emit live events promptly without waiting a minute.
  This is an explicit presentation difference from the vertical snapshot.
- `Normal` never emits event `sub_lines`. `Verbose` may show them for uncollapsed lifecycle
  events; a collapsed run shows only its summary. Tool details are one line in both tiers.
  Filter matching still searches original detail/sub-lines even when presentation truncates.
- Reuse `format_message`/`event_line` and shared line-building helpers in both viewports.
  Pass waterlines to inline formatting too; existing inline calls pass `None` and therefore
  do not show read receipts. Inline receipts are snapshots at emission/replay time.

### 8. Keybindings & help

Add to `render_help` (`src/tui/render/mod.rs` ~L1755):

```
v            detail: compact / normal / verbose
/            filter: text + tag: thread: to: from:
B            toggle "to:<bigboss>" filter
enter/space  filter by agent (does not change detail)
a            filter by all agents
esc          clear: text → filter → agent
```

Footer (Navigate mode, no overlay): add `v tier:<t>` next to the existing `? help · \ view`.

---

## Files touched

| file | change |
|------|--------|
| `src/tui/filter.rs` | **new** — `MsgTier`, `MsgFilter` + `parse`/`matches`, `FeedItem`, `tier_admits`, `passes`, shared collection/counts |
| `src/tui/inline/eject.rs` | `begin_replay` / `eject_new` call shared gate; remove private predicates/FTS shortcut; migrate counts, separators, replay state/watermarks and formatting |
| `src/tui/render/messages.rs` | collapse dispatch; delete old view-specific renderers/predicates; consume shared collection/counts; formatting |
| `src/tui/state.rs` | `msg_tier`, `msg_filter`, identity lookup and effective timeline limit metadata; remove `selected`, `show_events`, `eject_filter`, `search_filter`, `search_results` |
| `src/tui/input.rs` | `v` cycles tier; `B` toggles bigboss token; `/` overlay commits a parsed `MsgFilter`; `space`/`a`/`esc` operate on `msg_filter.agents`; drop `show_events` / `eject_filter` refs |
| `src/tui/model.rs` | `Message.thread` and `delivery_known` |
| `src/tui/db.rs` | parse message metadata, expose effective recent limit; delete `load_timeline_search` + `search_timeline` |
| `src/tui/data.rs` | delete `DataSource::search_timeline`; optional preference seam |
| `src/tui/actions.rs` | remove `update_search`; migrate selection pruning and action completion; replay on filter-changing prune |
| `src/tui/render/mod.rs` | header chips + count; footer tier; help text; remove `show_events` label branch |
| `src/config.rs` | `HcomConfig.bigboss`, load/default/get/set/validation, TOML/environment mappings |
| `src/commands/config.rs` | CLI bigboss metadata, TOML mapping, default, validation and persistence tests |
| `src/tui/app.rs` | load `bigboss`; migrate both constructors, highlighting and replay callers |
| `src/tui/mod.rs` | `prepare_*_viewport` no longer resets `eject_filter` |
| `src/tui/render/agents.rs` | selection checkmarks use `msg_filter.agents` |
| `src/tui/render/text.rs` | width/highlight helpers if required for consistent Unicode formatting |

## Testing

- `MsgFilter::parse` — tokens extracted, leftover text joined, malformed token → text, last
  duplicate wins.
- `MsgFilter::matches` — each token kind; AND of several; base vs full name; Unicode
  (`Đồng thuận` matched by `đồng`); `deliver:bono` matched by `eliver` (the case FTS5 loses);
  `thread:` excludes events.
- **`to:` regression guard** — a broadcast (`recipients` empty, `delivered = ["bigboss","ligo"]`)
  **must** match `to:bigboss`. This is the bug the original spec would have shipped.
  Plus: known-empty delivery never falls back; unknown delivery uses explicit mentions
  only; unknown broadcasts do not match arbitrary `to:X`; `to:*` excludes broadcasts even
  when their delivered array is populated.
- `tier_admits` — Compact excludes all events; Normal keeps `Tool`, drops `Activity`; Verbose
  keeps both. Messages always admitted at every tier.
- **inline default regression guard** — exercise actual replay/incremental collection:
  Compact emits messages and no events. Gate-only tests do not prove callers use it.
- Replay: zero matches still emits context/empty/live lines; new rows after replay appear
  once; changing filters cancels pending chunks; time-sorted IDs `[12, 11]` both emit when
  the old watermark is 10, and rejected rows still advance progress.
- `parse_message_row` — thread string/missing/null/non-string; delivery array presence.
- Identity: tagged/untagged/unknown/stopped and same-base local/remote agents on two devices.
- UI: draft Search prefill/commit/cancel, text → tokens → agents Escape sequence, `a`/pruning
  replay, viewport preservation, and unchanged compose/action target behavior.
- Window/counts: out-of-window match intentionally absent, limit override, tier-relative
  counts before filtering cap/grouping, scope shown in both viewports.
- Formatting: actual line sets with `reply_to`, four recipients and independent receipts,
  Unicode wrapped body, narrow width and system messages; tool one-line output; lifecycle
  run boundaries across messages, minutes, dates and replay chunks.
- Config: default/validation, CLI and HcomConfig TOML round-trip, environment precedence,
  `B` preserves other conditions. Optional persistence uses an injected temporary store.

## Open questions

None. Recent-window filtering was explicitly selected by the user during review. Tier
persistence remains an optional final implementation task and must be reported if deferred.
