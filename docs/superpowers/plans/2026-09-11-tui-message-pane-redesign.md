# TUI Message Pane Redesign Implementation Plan

**Status:** implemented 2026-09-11 (Tasks 1–6 + 8; Task 7 tier persistence
deliberately deferred — Compact on every launch, per the task's own opt-out).
**Date:** 2026-09-11
**Spec:** [design](../specs/2026-09-11-tui-message-pane-redesign-design.md)

**Goal:** Give both viewports an explicit Compact/Normal/Verbose tier and one shared filter
over the loaded recent timeline, with readable messages and a coordinator shortcut.

**Review decision:** The user chose recent-window filtering on 2026-09-11. Removing FTS
therefore intentionally removes all-history search from the pane. Keep the existing raw-row
limits (inline 200, vertical 5000, environment override) and show that scope in the UI.

**Architecture:** filter.rs owns borrowed FeedItem, MsgTier, MsgFilter, identity-aware
matching, collection and counts. UI state has one committed filter. Both render paths consume
the same gate; command output and replay separators are presentation, not feed items.

**Execution:** Follow the tasks in order. Each task ends with a compiling crate and passing
relevant tests. Tasks 1–2 introduce unused building blocks; Task 3 migrates their consumers
atomically. Do not expose a tier control that only works in one viewport, or delete state
fields while old render/count/replay code still reads them. Agent delegation is not required.

**Validation:** Rust tests live in existing test modules. Use DataState::empty() and
crate::tui::test_helpers::make_test_agent(name, age_secs); neither DataState nor Agent
implements Default. Message and Event currently do not implement Debug, so FeedItem must
not derive Debug unless those models are deliberately updated too. Use the real
input.rs helpers test_app() and key(), and db.rs helper setup_conn().

## Review corrections incorporated

| Original problem | Required correction |
|------------------|---------------------|
| FTS deletion silently shrinks history to the inline window | Explicit recent scope, limits and empty-state wording, per user decision |
| to:* accepts broadcasts with nonempty delivered_to | Classify explicit addressing using MessageScope |
| Empty delivery treated as missing historical data | Preserve delivery_known; known-empty never falls back |
| Base-name lookup aliases local and remote agents | Resolve storage/action/display names with device suffix preserved |
| Esc test clears text and tokens together | Three distinct clearing stages; overlay cancel preserves committed state |
| Partial migrations cannot compile and temporarily break selection | Migrate state, both viewports, counts, actions and FTS deletion together |
| Replay changes assume old scrollback can be redrawn | Append labelled replay; handle zero matches, pending lines and scan watermarks |
| Config examples use nonexistent get/set/KNOWN_KEYS and omit persistence paths | Cover HcomConfig and both config.rs mapping implementations plus CLI defaults |
| Formatting tests only check string helpers | Verify actual line output, width, receipts, grouping and replay chunks |
| Release rebuild assumed harmless for TUI-only changes | Use an isolated target directory for every manual build |

## Files and ownership

| File | Responsibility |
|------|----------------|
| src/tui/filter.rs | New pure gate, parsed filter, borrowed items, collection/counts |
| src/tui/model.rs | Message.thread and Message.delivery_known |
| src/tui/state.rs | Shared name/tag resolver, msg_tier/msg_filter, effective timeline limit metadata |
| src/tui/db.rs | Parse added fields; report effective limit; remove TUI FTS queries |
| src/tui/data.rs | Remove DataSource::search_timeline; optional preference seam |
| src/tui/actions.rs | Remove update_search; migrate selection pruning/action completion |
| src/tui/app.rs | State constructors, active_search_query, replay call sites, coordinator startup load |
| src/tui/input.rs | Tier/shortcut/filter edit/selection/Esc behavior and existing action readers |
| src/tui/mod.rs | Viewport preparation, replay caller, module declaration, fixtures |
| src/tui/inline/eject.rs | Shared gate, count adapter, replay chrome, watermarks, line emission |
| src/tui/render/messages.rs | Unified dispatch and shared message/event formatting |
| src/tui/render/text.rs | Highlight/width helpers only where required by matching/formatting tests |
| src/tui/render/mod.rs | Inline and vertical headers, scope/count/chips, footer, help |
| src/tui/render/agents.rs | Selected checkmarks use msg_filter.agents |
| src/config.rs | HcomConfig field/default/load/get/set/validation, TOML and environment mappings |
| src/commands/config.rs | CLI mapping/default/help metadata and validation/write/read tests |

## Task 1: Message metadata and agent identity

- [ ] Extend Message with thread: Option<String> and delivery_known: bool. Keep delivered
  as Vec<String> to minimize churn. In parse_message_row, thread is a JSON string or None;
  delivery_known is true exactly when delivered_to is an array, including an empty array.
  Absent, null, and non-array values are unknown. Remove dead-code allowances once consumed.
- [ ] Update every Message literal, using rg to find them rather than assuming two fixtures.
  Tests for broadcasts must set scope = Broadcast explicitly; clearing recipients alone
  leaves a Mentions fixture inconsistent.
- [ ] Add a shared roster lookup in DataState, used by tag_of and resolve_display_name.
  Search local, remote and loaded stopped rosters. Match storage name, action_name and
  display_name; normalize case while preserving remote device identity.
- [ ] For remote agents, raw storage identity is base:device and display/action identity may
  be tag-base:device. An unqualified base resolves locally and must not alias a remote agent.
  Unknown names compare literally (case-insensitive) and keep their raw display.
  Resolve known aliases through actual roster records; never strip arbitrary tag prefixes.
  Prefer exact storage identity over display aliases if names collide.
- [ ] Test thread present/missing/null/non-string; delivery missing/null/empty/populated;
  local and remote agents sharing a base, two remote devices, tagged/stopped/unknown agents.
  Verify lookup precedence cannot attach a local agent's tag or receipt to a remote record.
- [ ] Run just test tui::db and just test tui::model, plus focused identity tests in state.rs.
  Run cargo check --locked before committing the task.

## Task 2: Shared tier, filter and collection

- [ ] Declare pub mod filter in src/tui/mod.rs. Add FeedItem<'a> with borrowed Msg/Ev,
  MsgTier (Default = Compact), next(), as_str(), from_str(), and tier_admits().
  Provide a deterministic ordering key: time via total_cmp, DB row ID, then variant.
- [ ] Add MsgFilter with tag/thread/to/from: Option<String>, text: String and
  agents: BTreeSet<String>. Parse whitespace tokens using split_once(':'); recognize only
  exact lowercase keys, nonempty values, last duplicate wins. Unknown/empty tokens remain text.
  Join leftover tokens with one space; no quoting/escaping language is introduced.
- [ ] Add to_query() excluding roster selection, has_query(), has_tokens(), and is_empty().
  Round-trip only query fields; do not assert parse(to_query()) preserves agents.
- [ ] Implement matches() and passes() according to spec §3. Conditions AND together;
  selected agents OR together inside their condition. Empty agents means no restriction.
  thread: and to: exclude events. tag: refers to the sender/event owner, not recipients.
- [ ] to:X uses recorded delivered recipients when delivery_known is true, even when empty.
  Only unknown delivery may fall back to explicit mentions; unknown broadcasts do not
  prove delivery to arbitrary X. to:* admits Mentions with at least one explicit recipient,
  independent of delivery, and rejects all Broadcast rows.
- [ ] The roster condition matches sender or explicit recipients through identity lookup.
  Non-system Broadcast rows pass the roster condition. Selection remains an addressing
  filter; it is not the same predicate as recorded delivery used by to:X.
- [ ] Free text uses Unicode lowercase substring matching against the original fields
  enumerated in spec §3, including resolved event names, reply ID, intent badge, activity
  label and formatted time. Do not concatenate fields to manufacture cross-field hits.
  Keep lowercase normalization out of inner loops where practical.
- [ ] Add collect_items(data, tier, filter) returning all matching borrowed items in
  chronological order, and a shared count helper. Count tier-admitted total and matched
  before display caps or grouping. The pure gate must not depend on ratatui or SQLite;
  use model-level formatting for time/activity labels where needed.

Required tests:

- [ ] Tier admission matrix for messages, Tool and every Activity variant; tier cycle,
  valid/invalid string parsing and Compact default.
- [ ] Parser unknown/empty/duplicate tokens, whitespace, empty input and query round-trip.
- [ ] Broadcast with recorded bigboss matches to:bigboss but fails to:*.
  Known-empty delivery with mentions of bigboss fails to:bigboss.
  Unknown delivery with explicit bigboss mention passes; unknown broadcast fails.
- [ ] from/tag/thread/to individually and together; case variants; roster conditions;
  local versus tagged remote identities, including a same-base collision.
- [ ] Literal eliver, deliver:bono, 2026-09-10, Đồng/đồng, resolved event display name,
  request/req badge, reply ID, thread, lifecycle label and event sub-line.
  FTS can match whole dates/phrases; do not write a test claiming it never does.
- [ ] Shared collection order for equal timestamps and out-of-order IDs; filter before cap;
  counts independent of cap and collapsed lines. Default Compact count is messages only.
- [ ] Run just test tui::filter and focused state tests, then cargo check --locked.

## Task 3: Integrate both viewports and remove legacy state atomically

This task is one migration/commit. Implement the following substeps before the green-build
checkpoint; do not commit the intermediate field deletion as a standalone working change.

- [ ] Add UiState.msg_tier and msg_filter; migrate both App construction sites. Delete
  selected, search_filter, eject_filter and show_events after repointing all readers.
  DataState reports the effective timeline row limit used by load_all, with a deterministic
  fixture value, so headers do not reparse the environment or guess scope.
- [ ] Migrate ui.selected readers throughout src/tui, especially actions.rs pruning/removal,
  compose mention prefill, kill/stop/resume target resolution, tag/action menus, roster
  checkmarks and existing tests. Do not rename unrelated Confirm.selected/palette state.
  Keep current action eligibility and remote isolation.
- [ ] In Navigate mode, v cycles the tier, resets scroll and triggers inline replay.
  Enter/space changes selection only for agent rows; group headers still expand/collapse.
  a adds all local agents and triggers replay if the set changes. Selection never changes tier.
- [ ] Open Search with msg_filter.to_query(), placing the cursor at a valid end position.
  Treat edits as a draft. Enter parses and commits, preserving agents; Escape cancels the
  draft without clearing the committed filter. Remove live-search/update calls from typing,
  paste, delete, cursor movement and cancel paths.
- [ ] Outside overlays, Esc clears text first, then the four structured fields, then agents,
  skipping empty stages. Preserve existing higher-priority dismissal paths. Each actual
  filter mutation resets msg_scroll and triggers replay; do not clear the tier.
- [ ] Update App::active_search_query (in app.rs, not actions.rs) to return committed
  msg_filter.text only when nonempty. All highlighting uses this free-text value.
- [ ] Preserve filter and tier across prepare_inline_viewport/prepare_vertical_viewport.
  Retain their 200/5000 limit behavior and reload the source after a limit change.
  If reload prunes selected names or changes identity/display metadata that affects an
  active query, invalidate any queued replay and rebuild against the new snapshot.
- [ ] Update actual Ejector APIs: begin_replay and eject_new (there is no replay_all).
  Replace the old filter/text arguments with tier and &MsgFilter at all call sites in
  app.rs AND mod.rs. Migrate pending_filter_separator, filter_separator_lines and
  filtered_counts to the same filter/tier/count contract; remove the old FTS shortcut.
- [ ] Clone admitted items into the existing owned replay queue. FeedItem stays borrowed;
  the queue must not borrow DataState across reloads. Share ordering with collect_items.
  Replace queued items/lines and formatting group state on a new replay.
- [ ] Keep a pending-replay state even with zero items so the separator, empty message and
  live marker are drained. Include scope, tier and all active conditions in the separator.
  Preserve ReplayReason::Resize versus FilterChange and existing line-budget emission.
- [ ] Fix incremental scan progress: capture old message/event watermarks before iterating;
  compare all rows against those old values; afterwards advance each watermark to the
  maximum of its old value and all scanned IDs, including excluded rows. Never decrease a
  watermark on an empty batch or advance it during a time-sorted loop.
  Replay snapshots watermarks from the whole loaded window.
- [ ] Replace the vertical three-way dispatch with command-result override or the shared
  collect/filter/render path. Retain scrolling, blue cursor-related margin, waterlines and
  empty states. Always include agent identity on the first row of an event group.
- [ ] Remove render_agent_feed, render_all_messages, render_events_timeline and local
  FeedItem/msg_matches/event_matches/search count branches once their behavior is migrated.
  Apply DISPLAY_LIMIT only after filtering, retaining newest items; counts stay uncapped.
- [ ] Migrate render/mod.rs consumers now, including render_header, render_messages_separator,
  has_active_filter, input hints and display_count_str. Use the shared scope/count contract
  in both viewports. No code may keep consulting a deleted field until a later task.
- [ ] Remove update_search and every caller; DataState.search_results and all initializers;
  DbDataSource::search_timeline, load_timeline_search and DataSource::search_timeline in
  data.rs, including dummy implementations/imports/tests. Keep the shared FTS schema and
  all non-TUI users. No independent text prefilter may bypass passes().

Required integration tests:

- [ ] Exercise actual replay collection/emission helpers, not just passes() again.
  Default Compact emits messages and no tool/lifecycle rows; Normal and Verbose add only
  their allowed rows. Both viewport collectors admit identical IDs for the same snapshot.
- [ ] All filter mutations (v, Enter/space, a, each Esc stage, Search commit and pruning)
  invalidate inline replay. Group expansion, Search typing/cancel, Compose v/B do not.
  Adapt existing Search-cancel tests to the deliberately changed semantics.
- [ ] Check Search prefill, preserved roster selection, full three-press Esc cascade,
  unchanged tier, empty commit, literal punctuation, and viewport switching.
- [ ] Zero-match replay still emits filter context, empty state and live marker exactly
  once. Changing filters during a multi-chunk replay cannot emit stale queued items.
- [ ] Replay followed by new live rows has no duplicate IDs; filtered rows advance progress.
  A time-ordered vector with IDs [12, 11] emits both when the old watermark is 10.
- [ ] Seed a temporary DB with an old matching message outside the recent limit: it is
  intentionally absent. An in-window match is present. Check effective override, count
  scope and no all-history fallback; test default viewport limits using DummySource.
- [ ] Preserve existing compose/action tests, especially
  selected_local_agent_does_not_match_remote_with_same_base_name.

Verification before commit:

~~~bash
cargo check --locked
just test tui
rg -n 'ui\.(selected|search_filter|eject_filter|show_events)|search_results|update_search|search_timeline|load_timeline_search|events_fts' src/tui
~~~

The rg command should find no legacy executable code; exit 1 means no matches. Inspect any
remaining explanatory comments. Do not delete unrelated non-TUI events_fts code.

## Task 4: Shared readable formatting and bounded replay groups

- [ ] Reuse format_message, push_msg_header, event_line and shared run/line helpers.
  Do not fork formatting implementations between vertical and inline.
- [ ] Keep named recipients as individual spans so each retains its own highlight and ✓.
  Render the first two resolved names, then +N. Use all for Broadcast; a malformed
  Mentions row with no recipients must not be relabelled as a broadcast.
- [ ] Reserve dim time plus two trailing columns before clipping the header. Use Unicode
  display widths, saturating arithmetic, and width-safe clipping for long names/badges.
  Preserve intent and reply-to labels when space permits; never overflow/panic at width 0–2.
- [ ] Wrap body to the available width with two-space hanging indent. Maintain one blank
  line between message blocks, including live messages arriving in separate batches.
  Preserve yellow, body-in-header system messages.
- [ ] Make build_waterlines accessible to shared formatting and key it with the same
  device-qualified identity resolver. Pass it into inline format_message instead of None.
  A receipt is based on waterline >= event_id, not membership in delivered_to, and inline
  receipts reflect the snapshot at emission/replay time.
- [ ] Tool events render a single line in Normal and Verbose. Take only the first detail
  line, shorten long absolute paths for file tools, and clip to available columns.
  Do not accidentally shorten a Bash command merely because it starts with '/'.
  Do not emit Tool sub_lines in either tier.
- [ ] Group consecutive admitted lifecycle events by canonical owner and absolute minute
  floor(time / 60). Three or more become one summary; one/two stay plain. A message, tool,
  different agent or minute breaks the run. Group before line-budget chunking in replay.
- [ ] In live inline output group only within the current emission batch and emit promptly.
  Do not buffer a whole minute or promise to merge already emitted lines. Reset prefix/run
  state at replay boundaries. Retain lifecycle sub_lines only for uncollapsed Verbose rows.
- [ ] Keep substring matching on original detail/sub-lines even when output is shortened.
  Reuse safe highlight logic; test lowercase expansion (e.g. İ) and UTF-8 boundaries if
  extending it to align with to_lowercase matching.

Required output tests:

- [ ] Assert actual styled line output for a reply with four recipients (two names +2),
  individual ✓ spans, long tagged remote names, intent and right-aligned time.
- [ ] Check wrapped Vietnamese/CJK/emoji body, long unbroken words and narrow widths with
  a ratatui TestBackend or pure line builder; all rendered rows fit their available width.
- [ ] Check system message, empty/multiline tool detail and file path shortening.
- [ ] Check lifecycle runs of 1/2/3, interruptions, different dates with the same HH:MM,
  and a run crossing a replay line-budget boundary.
- [ ] Check inline and vertical shared formatting plus the documented live-batch exception.
- [ ] Run just test tui before committing.

## Task 5: Coordinator config and B shortcut

- [ ] Add bigboss to HcomConfig, its Default, get_field(), set_field(), normalization/
  validation and load_from_sources string list. Default is "bigboss"; empty means default;
  reject whitespace-containing values and "*". Tagged and device-qualified names are valid.
- [ ] In src/config.rs add TOML_KEY_MAP entry ("bigboss", "preferences.bigboss") and
  FIELD_TO_ENV entry ("bigboss", "HCOM_BIGBOSS").
- [ ] In src/commands/config.rs update CONFIG_KEYS with HCOM_BIGBOSS,
  toml_path_for_key("bigboss"), config_get's default and setter validation.
  Both config modules maintain mappings; editing only one causes read/write disagreement.
  Keep the normal CLI spelling hcom config bigboss <name>.
- [ ] Load HcomConfig::load(None) once in production App startup; use its bigboss value
  or a safe default through the existing configuration-error behavior. Fixture constructors
  initialize a deterministic "bigboss" without reading host config.
- [ ] B in Navigate removes to: when it identifies the configured coordinator; otherwise
  replaces it with that coordinator. Use shared identity/case rules, preserve other fields,
  reset scroll and trigger replay.
- [ ] Test get_field/set_field, defaults/empty/invalid values, TOML load/save/reload, environment
  precedence, CLI read default, CLI normalized key/mapping and config --info metadata.
  Use temporary config paths and existing environment guards.
- [ ] Test B twice, replacing another to:, case/alias variants and preserving text/agents/
  tag/thread/from. Test B is literal in Compose and Search.
- [ ] Run focused config and CLI config tests, just test tui, then just test before commit.
  A getter/setter-only test is insufficient to establish persistence works.

## Task 6: Chips, scope and help

Task 3 already made headers compile with the new count model. This task finishes presentation.

- [ ] Render tag/thread/to/from, sorted agent selections and quoted free text as chips in
  both viewport headers. Include recent (limit N), count and current tier in inline replay
  separators. Resolve selection display names using the same identity resolver.
- [ ] Reserve count space before clipping chips; avoid pushing it off the right edge.
  No condition: [total]. Any condition: [matched/total]. Count within the current tier and
  loaded window; append showing N only when presentation caps the matched items.
- [ ] Footer says v tier:compact/normal/verbose. Update existing help entries in place,
  retaining other navigation/action controls and the scrollable help layout.
- [ ] Explain '/' recent-window scope and grammar, Search Enter/cancel, B coordinator
  shortcut, selection's continued action-target role, three-stage Esc, and inline replay's
  append-only behavior. Mention HCOM_TUI_TIMELINE_LIMIT and differing viewport defaults.
- [ ] Verify header/footer/help output at normal and narrow widths, filtered/unfiltered/
  empty windows and presentation cap. Check count tests use messages-only total in Compact.
- [ ] Run focused render tests and just test tui before committing.

## Task 7: Optional tier persistence

This task may be explicitly deferred in the implementation report. Compact on every launch
is acceptable. Core behavior and tests must not depend on persisted host preferences.

- [ ] Use kv key tui.msg_tier. Parse compact/normal/verbose, defaulting to Compact for
  missing/invalid values. A failed write must not prevent the tier change.
- [ ] Add a small injectable preference seam (for example default read/write methods on
  DataSource) so UI tests never read/write the real preference.
- [ ] Use the existing read-only TUI connection for reads. Writes require a separate writable
  HcomDb connection to the same configured DB, through HcomDb::kv_set(key, Some(value));
  HcomDb::kv_get returns Result<Option<String>>. Do not disable query_only on the TUI
  datasource or invent set_kv/get_kv free functions.
- [ ] Load once in production startup; fixture constructors stay Compact. Save only when v
  actually changes the tier, not on every reload/render. No other filter state persists.
- [ ] Test restart/load through a temporary preference store, missing/invalid values and
  write failure with an injected failing store. Existing kv helper tests need not be repeated.
- [ ] Run affected tests and just test tui before committing; report whether this task shipped.

## Task 8: Final verification and handoff

- [ ] Run just ci. Report any environmental skip/failure accurately; do not claim a green
  suite from unexecuted example commands.
- [ ] For manual verification build to a separate target directory and run that binary.
  Do not assume the hcom on PATH is this build, or replace target/release/hcom, which may
  be used by active hooks even when only src/tui changed.

~~~bash
cargo build --locked --target-dir /tmp/hcom-tui-review-build
/tmp/hcom-tui-review-build/debug/hcom
~~~

- [ ] Exercise default Compact, v cycle, roster filtering in every tier, Search commit/cancel,
  each Esc stage, B, no matches, long/wrapped messages, resize during replay and viewport
  switches. Observe current labelled replay; old scrollback is expected to remain.
- [ ] Verify recent-only scope: old out-of-window matches stay absent, and in-window eliver
  finds deliver:bono. A broadcast with recorded delivery to bigboss matches B, but never to:*.
- [ ] For config/persistence write tests use temporary HCOM_DIR or automated fixtures.
  Manual observation of a live room does not require sending messages or changing its config.
- [ ] Inspect the final diff and git status. Stage only task files with explicit paths and
  keep unrelated work out of commits. Report implemented behavior, verification and the
  optional persistence decision.
