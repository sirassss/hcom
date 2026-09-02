# Cursor CLI: live `sessionEnd` must not unregister + idle follow-up must attach the packet

**Date:** 2026-08-29
**Issue:** `docs/issues/2026-08-27-cursor-agent-cli-sessionend-and-followup-miss.md`
**Status:** Plan written at docs/superpowers/plans/2026-08-29-cursor-cli-sessionend-and-idle-followup.md

Two integration bugs in the same class. Fixing one does not fix the other. Bug A is a prerequisite for Bug B: Stop follow-up is useless if the instance is already `exit:completed`.

**Scope:** hcom + Cursor CLI hooks only (`src/hooks/cursor.rs`, Cursor-gated bind in `src/instance_binding.rs`, stop/prune cascade in `src/db/instances.rs` + `src/instance_lifecycle.rs`, `~/.cursor/hooks.json` via the existing installer). Not agent-ops, not Cursor IDE GUI, not Cursor product inbox-in-turn, not in-turn poll like Claude Stop, **not** a Claude identity change in `init_hook_context`.

---

## Problem

Cursor Agent CLI does not keep a Stop-hook poll the way Claude does. Inbox is still `hcom.db`. Idle delivery is intentionally two steps: PTY inject of only `<hcom>` → turn ends → `cursor-stop` puts the real packet in `followup_message`. A miss is step 2 failing, or identity already dead — not a missing queue.

### A — `sessionEnd` unregisters a live PTY

`uvx hcom cursor-agent` joins (`bindings: hooks, pty`). The herdr pane and PID stay up; later turns still run. `hcom list` shows `(not participating)` then Recently stopped, `By: session` / `Reason: exit:completed`. This is not `hcom stop` / `hcom kill`.

Root cause: hcom treats one name as one session UUID. Cursor CLI exposes two UUIDs on the same process (PTY bind vs transcript `primary_session`). `handle_sessionend` in `src/hooks/cursor.rs` always calls `finalize_session` (`src/hooks/common.rs`) with no pidtrack check. A `sessionEnd reason=completed` on one UUID deletes the instance while the process is still alive.

Evidence from `review-zilo` (2026-08-27): `sessionend` with `validated=false` while `pidtrack_recorded` still had the pane; then `identity.resolve.process_binding_expired`. Logs also showed `historical_process_rejected` / `historical_root_hook_rejected` between the two UUIDs. Cursor hook dispatch does **not** call `init_hook_context`; those lines are treated as shared-identity noise, not as a reason to change Claude’s path. Dual-UUID for Cursor is fixed in `bind_session_to_process` + not finalizing on `sessionEnd`.

Expected: identity lives until `hcom stop`, `hcom kill`, or the process / PTY actually exits (observed on the next hcom CLI process — see Teardown below).

### B — Idle follow-up miss

`handle_stop` already returns `followup_message`, but only when `payload.status == "completed"`. Installed `stop.timeout` is 15s. Cursor may kill the hook or hand empty stdin → `cursor.parse_error` `EOF while parsing a value` → no follow-up. The agent is taught that a prompt of only `<hcom>` means end the turn without ACK. If Stop does not attach the packet, the worker sits idle and needs a human nudge.

This is not “hcom lacks an inbox.” Queue stays in `hcom.db` until ACK. ACK must mean “Cursor had a healthy Stop payload and we flushed follow-up,” not “we wrote bytes after a broken stdin.”

---

## Goals

1. A live Cursor CLI PTY stays `◉ listening` across `sessionEnd` (any `reason`, including `completed`).
2. Two session UUIDs on the same live Cursor `process_id` are one instance. No session-switch retire of that Cursor name. Claude identity code stays byte-for-byte unchanged.
3. One `hcom send` to an idle Cursor attaches a full packet on a **healthy** Stop (`followup_message`) without a second human nudge.
4. Real teardown still works: `hcom stop`, `hcom kill`, process / PTY death. All session aliases for that instance are deleted, not only the canonical `session_id`.

## Non-goals

- agent-ops / kaban / settle identity (`kali` vs `myapp-kali`)
- Cursor IDE GUI
- In-turn poll like Claude Stop (the harness does not support it)
- Putting the message body in the PTY inject (idle wake stays `<hcom>` only)
- Raising `HCOM_TIMEOUT` (10800) on the Cursor stop hook — that hook does not block on `hcom poll`
- Changing `init_hook_context.historical_process_rejected` for all tools
- Parse-fail env fallback (`HCOM_PROCESS_ID` after empty stdin) in this change set — measure miss rate after A + healthy Stop follow-up + 30s timeout; add only if still non-zero
- Skill text that says “remember to ACK” as the primary fix
- A new background reaper daemon

---

## Decisions

| Topic | Choice |
|---|---|
| When to unregister | Never on Cursor `sessionEnd`, **any** `reason`. Observed in the wild: `completed` with `validated=false` on a live PTY. Other values (`error`, `abort`, …) are not enumerated by Cursor in-repo; do not build an allowlist without measuring. Last-breath is PID death, not this hook. |
| Dual UUID | Same live Cursor `process_id` = same agent. Second UUID is an alias / rebind. Gate on `tool == "cursor"` (bound instance / placeholder row), not a shared `init_hook_context` relaxation. |
| Identity blast radius | Claude path unchanged. No new `init_hook_context` tests for live-process alias. |
| Idle delivery | Keep two steps. Inject `<hcom>` only. Healthy Stop attaches the full packet automatically. No human steer. |
| Stop `status` gate | Drop it. Unread → always `followup_message`. Empty queue → `{}`, no loop. |
| Parse / empty stdin | Log. **Do not ACK.** Do not add env-based resolve in this change set. Queue stays for the next healthy Stop (double-delivery over loss). |
| `stop.timeout` | 30s for `cursor-stop` only. Other Cursor hooks stay 15s. Cost that can exceed 15s: `notify_hook_instance_with_db` + sqlite lock under load; Cursor then kills the hook and truncates stdin/stdout. This is not poll. |
| `loop_limit` | Keep `null`. Looping is gated by unread. |
| Teardown / stale window | `mark_dead_instances` (`src/instance_lifecycle.rs`) runs at the start of **every** `hcom` CLI process (`src/main.rs`). Not a realtime watcher. If no `hcom` command runs on the host, a dead Cursor can stay `listening` until the next invocation. `hcom send` / `hcom list` themselves start a process, so they reap before dispatch. Accept this window; do not add a daemon. |
| Alias cleanup | Stop/prune must delete **all** `session_bindings` for the instance name (`delete_session_bindings_for_instance`), not `WHERE session_id = ?` on the canonical id only. |
| hooks.json migration | Next `hcom cursor-agent` spawn: `verify_cursor_hooks_installed` must fail if stop timeout is not 30, then `try_setup_cursor_hooks` rewrites. Idle existing agents keep 15s until that spawn or `hcom hooks add cursor`. |

---

## Architecture

Cursor CLI identity is **process-lifetime**, not conversation-lifetime. Claude stays **generation-lifetime** (primary `session_id` + `historical_root_hook_rejected`).

- **Canonical key (Cursor):** `process_id` (`process_bindings`).
- **`session_id` (Cursor):** alias. A second UUID on the same live Cursor process rebinds onto the existing instance. `session_bindings` may hold both UUIDs pointing at one name.
- **`sessionEnd`:** log and return. Do not call `finalize_session` or `soft_finalize_session`.
- **Idle path unchanged:** `delivery` injects `<hcom>` → Cursor runs a short turn → healthy `cursor-stop` returns `{ "followup_message": "<hcom>…packet…</hcom>" }` → ACK after flush → Cursor auto-continues.
- **Unregister:** `stop_instance` from `hcom stop` / `hcom kill`; PID death via `mark_dead_instances` on the next hcom CLI process. No new teardown channel.

---

## Components

### 1. `src/hooks/cursor.rs` — `handle_sessionend`

If an instance resolves, log that Cursor `sessionEnd` is ignored (`reason` included in the log). Do not call `finalize_session`. Return `{}`.

Do not gate on pidtrack, `validated`, or `reason`. An extra pidtrack check would still finalize a false `completed` if pidtrack lagged. Real death is `mark_dead_instances` / `hcom stop` / `hcom kill`.

### 2. `src/hooks/cursor.rs` — `handle_stop`

Remove the `status == "completed"` early return. After `set_status(listening)` and notify:

- unread → `{ "followup_message": prepared.formatted }` + `DeliveryAck`
- else → `{}`

On this **healthy** path (JSON parsed), ACK is committed only after stdout JSON is flushed successfully (existing `dispatch_cursor_hook_native` order). Keep that order.

`prepare_pending_messages` does not reserve the unread set. Two overlapping Stop processes can both read the same unread and both emit follow-up. Accept a possible double Cursor follow-up; `last_event_id` advance makes a second ACK idempotent for the queue. Do not add a new lock unless a test shows duplicate user-visible packets after a single send.

### 3. Parse-fail path — **deferred**

Today: stdin parse fail → log `cursor.parse_error` → return 0, no stdout, no ACK. **Keep that.** Do not resolve via `HCOM_PROCESS_ID` in this change set.

Rationale: write-after-parse-fail is not “Cursor consumed follow-up.” ACK on that path would swallow the queue. Env inheritance into the hook subprocess is unverified. Ship 1, 2, 4, 5, 6 first; if production logs still show parse-fail misses at 30s timeout, add best-effort stdout **without ACK** as a follow-up change.

### 4. `src/hooks/cursor.rs` — hook install

- `HOOK_TIMEOUT_SECS` stays 15 for sessionStart / beforeSubmitPrompt / preToolUse / postToolUse / sessionEnd.
- `STOP_HOOK_TIMEOUT_SECS = 30` for `stop` only.

`expected_hook("stop", …)` sets `timeout: 30` and `loop_limit: null`. `verify_hooks_at` must require the hcom stop entry’s timeout **== 30** (not merely `is_some()`). That is the migration trigger: a 15s file fails verify → launcher (`src/launcher.rs` Cursor branch) calls `try_setup_cursor_hooks`. Idempotent; non-hcom entries preserved.

Do not hand-edit `~/.cursor/hooks.json` except via that installer / `hcom hooks add cursor`.

### 5. `src/instance_binding.rs` — Cursor dual UUID

Cursor `sessionStart` already calls `bind_session_to_process`. Path 3 (no canonical session, placeholder from `process_id`) already rebinds a new UUID onto the live name.

Guard Path 1b (`exit:session_switch`) **when the process-bound instance `tool` is `cursor`**: do not retire that name just because a second session UUID appeared on the same `process_id`. Keep the existing name, add/rebind the session alias.

Do **not** change Path 1b for Claude or other tools. Do **not** change `init_hook_context` historical reject.

### 6. Alias cascade on stop / dead-PID mark

Today `finalize_instance_stop` (`src/db/instances.rs`) and `mark_dead_instances` (`src/instance_lifecycle.rs`) delete `session_bindings` by the instance’s canonical `session_id` only. Extra alias rows leak.

Change both to `delete_session_bindings_for_instance(name)` (already used from `hcom start` reclaim / bind session-switch). Also keep deleting `process_bindings` by `instance_name` (already done).

---

## Data flow

### Live `sessionEnd` (must not unregister)

Cursor fires `sessionEnd` (e.g. `reason=completed`) → `cursor-sessionend` resolves via `process_id` or session alias → log warn → instance stays `◉ listening`.

### Dual UUID on one Cursor process

Spawn binds UUID-A + `process_id`. A later Cursor hook uses UUID-B. `bind_session_to_process` sees a live Cursor process binding → alias UUID-B onto the same name. No session-switch retire.

### Idle delivery (two steps, automatic)

`hcom send` while idle → PTY inject `<hcom>` only → Cursor starts a turn → healthy `cursor-stop` (30s timeout) → unread → stdout follow-up → ACK after flush → Cursor auto-follows-up. Empty unread → `{}` → no loop.

Broken stdin Stop: log, no ACK, no env fallback. Next healthy Stop delivers (may duplicate the packet in the model once).

### Real unregister

`hcom stop` / `hcom kill` → `stop_instance` + cascade alias delete.

Process dies idle → row can stay `listening` until the next `hcom` CLI process runs `mark_dead_instances` (then cascade alias delete). `sessionEnd` never takes this path.

---

## Error handling

| Situation | Behavior |
|---|---|
| Cursor `sessionEnd` any `reason`, PID/pane alive | Do not unregister. Log `reason`. Instance stays listening. |
| Cursor `sessionEnd` empty stdin / parse fail | Handler never runs. Log `cursor.parse_error`. Exit 0. No finalize. |
| `cursor-stop` empty stdin / truncated JSON | Log parse_error. **No ACK. No env fallback.** Queue stays. |
| Stop cannot resolve an instance | `{}`, no follow-up, no ACK. Queue stays. |
| Stop `status` is not `completed` | Still attach unread if any (healthy parse). |
| No unread | `{}`. Cursor does not loop. |
| Stdout write of `followup_message` fails | No ACK. Queue stays. |
| `hcom stop` / `hcom kill` / next-CLI dead-PID mark | `stop_instance` / `mark_dead_instances` + delete all session aliases. |
| Claude hook UUID ≠ primary | Unchanged `historical_root_hook_rejected`. |

No in-turn poll. No `hcom listen` inside the agent turn.

---

## Testing

Unit tests are the merge gate. PTY tests prove A+B on a real Cursor CLI when the environment has one.

### Unit (CI: `cargo test --locked`)

1. `handle_sessionend` does not delete the instance (row present, status not `exit:completed`), including `reason=completed`.
2. Two `session_id`s, one live Cursor `process_id` → one instance; Path 1b does not `exit:session_switch` that Cursor name.
3. Same as (2) but `tool=claude` (or non-cursor): Path 1b behavior **unchanged** (session-switch still allowed).
4. `handle_stop` with unread and `status` omitted or not `"completed"` → `followup_message` present.
5. `handle_stop` with empty queue → no `followup_message`.
6. Dispatch parse-fail: no ACK (cursor not advanced). Do **not** require env-fallback stdout in this change set.
7. Healthy Stop: failed stdout write → no ACK (if the test harness can simulate flush failure; otherwise assert ACK is only called after successful `to_writer`+`flush` in the dispatch function).
8. `try_setup_cursor_hooks`: hcom stop `timeout == 30`; other hcom hooks 15; `loop_limit` null; custom commands preserved. `verify_hooks_at` is false when stop timeout is 15.
9. `finalize_instance_stop` / `mark_dead_instances`: two `session_bindings` rows for one instance → both gone after stop/mark.
10. Existing `finalize_session` / `stop_instance` tests still pass.
11. Existing Claude `test_historical_root_hooks_are_rejected_before_dispatch` still passes. No new Claude live-alias case.

### PTY (`tests/test_pty_delivery.rs` or a Cursor-focused ignored test)

If Cursor CLI is available:

- Spawn `hcom cursor-agent` → idle several minutes → `hcom list` still `◉ listening`.
- One `hcom send` while idle → follow-up contains the packet; no second nudge.
- `hcom stop` afterwards → instance gone; no leftover `session_bindings` for that name.

If the job has no Cursor CLI, keep `#[ignore]` and use manual acceptance. Do not block merge.

### Manual acceptance

- Idle several minutes still `◉ listening`.
- `hcom send` injects; no `hcom start --as` again.
- Early `sessionEnd` does not become `exit:completed` while PID is alive.
- `hcom stop` / process death (then any `hcom` command) still unregisters.
- One idle `hcom send` → follow-up has the packet.
- Empty unread → Stop does not loop.
- After stop, `session_bindings` has no extra UUIDs for that name.

---

## Alternatives considered

1. **Relax `init_hook_context` for every tool (shared identity).** Chosen in the first brainstorm, **reversed after review-noze.** Cursor does not call that function. A tool-gated bind fix deletes the Claude blast radius. YAGNI.
2. **AGY-style `soft_finalize_session`.** Roster would look stopped. Rejected.
3. **Guard `sessionEnd` on pidtrack / `validated` / `reason` allowlist.** Rejected until Cursor’s real `reason` set is measured. Ignore all reasons.
4. **Inject the full packet into the PTY.** Breaks the existing Cursor idle contract. Rejected.
5. **Keep `status==completed` and only raise timeout.** Still drops aborted/error Stop. Rejected.
6. **`stop.timeout` 60s or all Cursor hooks at 30s.** Rejected; 30s on stop only, justified as notify + sqlite vs 15s kill, not poll.
7. **Parse-fail env fallback + ACK after write.** Rejected for this change set: write ≠ consumed; env unverified. Prefer no ACK and later healthy Stop.
8. **Background PID watcher.** Rejected. Opportunistic `mark_dead_instances` on each hcom CLI process is the existing contract; document the stale window instead of a new daemon.

---

## File touch list (for the plan)

| File | Change |
|---|---|
| `src/hooks/cursor.rs` | `handle_sessionend`, `handle_stop` (drop status gate), stop timeout 30, `verify_hooks_at` requires 30 |
| `src/instance_binding.rs` | Cursor-only: do not session-switch-retire same live `process_id` |
| `src/db/instances.rs` | `finalize_instance_stop`: delete all `session_bindings` for the instance name |
| `src/instance_lifecycle.rs` | `mark_dead_instances`: same cascade |
| `src/hooks/common.rs` | **No** `historical_process_rejected` behavior change |
| `src/hooks/claude.rs` | **No** production change |
| `tests/test_pty_delivery.rs` | optional ignored Cursor idle+follow-up case |
| `skills/hcom-agent-messaging/references/cross-tool.md` (and plugin copy) | Cursor identity is process-lifetime; idle follow-up is automatic; teardown is next hcom CLI process |

Do not change agent-ops, kaban, or Cursor product docs beyond the hcom cross-tool note.

---

## Review notes (2026-08-29)

- **review-rina:** APPROVE + alias cascade on prune/stop. Incorporated in component 6.
- **review-noze:** request-changes (stale window, cursor-gated identity, timeout justification, no ACK on parse-fail, defer env fallback, hooks.json migration, tests). Incorporated above. Shared `init_hook_context` change dropped.
