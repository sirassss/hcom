# Cursor CLI: live `sessionEnd` must not unregister + idle follow-up must attach the packet

**Date:** 2026-08-29
**Issue:** `docs/issues/2026-08-27-cursor-agent-cli-sessionend-and-followup-miss.md`
**Status:** Design approved in brainstorming; awaiting spec review before the implementation plan

Two integration bugs in the same class. Fixing one does not fix the other. Bug A is a prerequisite for Bug B: Stop follow-up is useless if the instance is already `exit:completed`.

**Scope:** hcom + Cursor CLI hooks only (`src/hooks/cursor.rs`, shared identity bind in `src/hooks/common.rs` / `src/instance_binding.rs`, `~/.cursor/hooks.json` install). Not agent-ops, not Cursor IDE GUI, not Cursor product inbox-in-turn, not in-turn poll like Claude Stop.

---

## Problem

Cursor Agent CLI does not keep a Stop-hook poll the way Claude does. Inbox is still `hcom.db`. Idle delivery is intentionally two steps: PTY inject of only `<hcom>` → turn ends → `cursor-stop` puts the real packet in `followup_message`. A miss is step 2 failing, or identity already dead — not a missing queue.

### A — `sessionEnd` unregisters a live PTY

`uvx hcom cursor-agent` joins (`bindings: hooks, pty`). The herdr pane and PID stay up; later turns still run. `hcom list` shows `(not participating)` then Recently stopped, `By: session` / `Reason: exit:completed`. This is not `hcom stop` / `hcom kill`.

Root cause: hcom treats one name as one session UUID. Cursor CLI exposes two UUIDs on the same process (PTY bind vs transcript `primary_session`). `handle_sessionend` in `src/hooks/cursor.rs` always calls `finalize_session` (`src/hooks/common.rs`) with no pidtrack check. A `sessionEnd reason=completed` on one UUID deletes the instance while the process is still alive.

Evidence from `review-zilo` (2026-08-27): `sessionend` with `validated=false` while `pidtrack_recorded` still had the pane; then `identity.resolve.process_binding_expired`; `historical_process_rejected` / `historical_root_hook_rejected` between the two UUIDs.

Expected: identity lives until `hcom stop`, `hcom kill`, or the process / PTY actually exits.

### B — Idle follow-up miss

`handle_stop` already returns `followup_message`, but only when `payload.status == "completed"`. Installed `stop.timeout` is 15s. Cursor may kill the hook or hand empty stdin → `cursor.parse_error` `EOF while parsing a value` → no follow-up. The agent is taught that a prompt of only `<hcom>` means end the turn without ACK. If Stop does not attach the packet, the worker sits idle and needs a human nudge.

This is not “hcom lacks an inbox.” Queue stays in `hcom.db` until ACK after a successful stdout write.

---

## Goals

1. A live Cursor CLI PTY stays `◉ listening` across `sessionEnd reason=completed`.
2. Two session UUIDs on the same live `process_id` are one instance. No historical reject between them. No session-switch retire.
3. One `hcom send` to an idle Cursor attaches a full packet on Stop (`followup_message`) without a second human nudge.
4. Real teardown still works: `hcom stop`, `hcom kill`, process / PTY death.

## Non-goals

- agent-ops / kaban / settle identity (`kali` vs `myapp-kali`)
- Cursor IDE GUI
- In-turn poll like Claude Stop (the harness does not support it)
- Putting the message body in the PTY inject (idle wake stays `<hcom>` only)
- Raising `HCOM_TIMEOUT` (10800) on the Cursor stop hook — that hook does not block on poll
- Skill text that says “remember to ACK” as the primary fix

---

## Decisions

| Topic | Choice |
|---|---|
| When to unregister | Never on Cursor `sessionEnd`. Only `hcom stop` / `hcom kill` / process or PTY death. `sessionEnd` is a conversation-UUID event, not “user quit the CLI.” |
| Dual UUID | Same live `process_id` = same agent. Second UUID is an alias / rebind, not a new identity and not a historical reject. |
| Identity blast radius | Shared: `init_hook_context` in `common.rs` must not `historical_process_rejected` when the incoming `session_id` differs but `process_id` is bound to a **live** instance. Claude’s `historical_root_hook_rejected` in `claude.rs` **stays** for hooks whose `session_id` is not the instance primary **and** that are not this live-process alias case (different process, or dead process). |
| Idle delivery | Keep two steps. Inject `<hcom>` only. Stop attaches the full `<hcom>…</hcom>` packet automatically. No human steer. |
| Stop `status` gate | Drop it. Unread → always `followup_message`. Empty queue → `{}`, no loop. |
| Parse / empty stdin | Log; do not ACK. If `HCOM_PROCESS_ID` (or equivalent env) can resolve the instance and unread exists, still write `followup_message`. |
| `stop.timeout` | 30s for `cursor-stop` only. Other Cursor hooks stay 15s. |
| `loop_limit` | Keep `null` (unlimited). Looping is gated by unread, not by a numeric cap. |

---

## Architecture

Cursor CLI identity is **process-lifetime**, not conversation-lifetime.

- **Canonical key:** `process_id` (`HCOM_PROCESS_ID` / `process_bindings`).
- **`session_id`:** alias. A second UUID on the same live process rebinds onto the existing instance. `session_bindings` may hold both UUIDs pointing at one name. `process_bindings.session_id` may move to the newer UUID.
- **`sessionEnd`:** log and return. Do not call `finalize_session`. Do not `soft_finalize_session` either — that would mark `ST_INACTIVE` / `exit:…` and make `hcom list` look stopped while the PTY is live.
- **Idle path unchanged:** `delivery` injects `<hcom>` → Cursor runs a short turn → `cursor-stop` returns `{ "followup_message": "<hcom>…packet…</hcom>" }` → Cursor auto-continues. UI “follows-up” is correct CLI behavior.
- **Unregister:** existing `stop_instance` from `hcom stop`, `hcom kill`, and pidtrack / process-death cleanup. No new teardown channel.

This matches Antigravity’s “Stop is not process death” lesson without copying AGY’s soft-finalize (which still looks stopped in the roster).

---

## Components

### 1. `src/hooks/cursor.rs` — `handle_sessionend`

If an instance resolves, log that Cursor `sessionEnd` is ignored while the process is the source of truth. Do not call `finalize_session`. Return `{}`.

Do not gate this on pidtrack liveness, `validated`, or stdin quality. Those extra predicates were considered and rejected: Cursor `sessionEnd` is never “user quit.” Real death is observed elsewhere. An extra pidtrack check would still finalize a false `completed` if pidtrack lagged.

### 2. `src/hooks/cursor.rs` — `handle_stop`

Remove the `status == "completed"` early return. After `set_status(listening)` and notify:

- unread → `{ "followup_message": prepared.formatted }` + `DeliveryAck`
- else → `{}`

ACK is committed only after stdout JSON is flushed successfully (existing `dispatch_cursor_hook_native` order). Keep that order.

### 3. `src/hooks/cursor.rs` — `dispatch_cursor_hook_native` parse failure

Today: `serde_json::from_reader(stdin)` fails → log `cursor.parse_error` → return 0 with no stdout body.

Change for **`cursor-stop` only**: after parse fail, still open DB, `HcomContext::from_os()`, resolve via `process_id` / `HCOM_INSTANCE_NAME`. If unread exists, write the same `followup_message` JSON and ACK only after a successful write. If resolve fails or queue is empty, log and return 0. Never ACK on parse fail without a successful write.

Other hooks keep today’s fail-open (log, exit 0, no mutation).

### 4. `src/hooks/cursor.rs` — hook install

Split timeout:

- `HOOK_TIMEOUT_SECS` stays 15 for sessionStart / beforeSubmitPrompt / preToolUse / postToolUse / sessionEnd.
- `STOP_HOOK_TIMEOUT_SECS = 30` for `stop` only.

`expected_hook("stop", …)` sets `timeout: 30` and `loop_limit: null`. `verify_hooks_at` must require stop timeout == 30 (not merely `is_some()`). `try_setup_cursor_hooks` remaining idempotent; non-hcom entries preserved.

Existing machines get 30s on the next hook install / `hcom` setup path that already rewrites hcom Cursor hooks. Do not hand-edit `~/.cursor/hooks.json` in this work except via that installer.

### 5. Shared identity — `src/hooks/common.rs` `init_hook_context`

Today: if lineage is unknown and `process_bindings.session_id != incoming session_id`, log `init_hook_context.historical_process_rejected` and return no instance.

Change: if `process_id` is bound to an instance that still exists (row present, not already deleted by `stop_instance`), treat the incoming `session_id` as an alias of that instance. Do not reject. Caller bind paths (`bind_session_to_process` / `rebind_session`) attach the new UUID.

Still reject when there is no live process binding, or when transcript lineage is **ambiguous** (multiple owners) — that branch is unchanged.

### 6. `src/instance_binding.rs` — `bind_session_to_process`

Cursor `sessionStart` already prefers `bind_session_to_process`. Path 3 (no canonical session, placeholder from `process_id`) already rebinds a new UUID onto the live name.

Guard Path 1b (session switch / `exit:session_switch`): do **not** retire the process-bound instance when the “new” canonical is just a second UUID for the same live `process_id`. Same `process_id` → keep the existing name, add/rebind session alias, do not `ST_INACTIVE` the placeholder name.

### 7. Claude generation gate — `src/hooks/claude.rs`

**Do not remove** `claude.historical_root_hook_rejected`. Compact/resume still must not apply live hooks to a retired generation.

Interaction with (5): Claude SessionStart that shares a live `process_id` with a different UUID will now resolve the instance in `init_hook_context` instead of returning `None`. Claude’s own dispatch still no-ops non-primary-generation hooks except SessionEnd. That is intended. Add a regression test so a Claude hook whose `session_id` ≠ instance primary **and** whose `process_id` is missing or bound to a **different** dead session still rejects.

---

## Data flow

### Live `sessionEnd` (must not unregister)

Cursor fires `sessionEnd reason=completed` → `cursor-sessionend` resolves the instance via `process_id` or session alias → log warn → instance stays `◉ listening`. Later `hcom send` and `cursor-stop` still resolve.

### Dual UUID on one process

Spawn binds UUID-A + `process_id`. A later hook uses UUID-B. `init_hook_context` / `bind_session_to_process` see a live process binding → alias UUID-B onto the same name. No `historical_*_rejected`. No session-switch retire. Both UUIDs resolve to the same name.

### Idle delivery (two steps, automatic)

`hcom send` while idle → PTY inject `<hcom>` only → Cursor starts a turn → `cursor-stop` (30s timeout) → unread → stdout `{ "followup_message": "<hcom>…packet…</hcom>" }` → ACK after flush → Cursor auto-follows-up. Human does not type or nudge. Empty unread → `{}` → no loop.

### Real unregister

`hcom stop` / `hcom kill` / process or PTY death (pidtrack) → `stop_instance` as today. `sessionEnd` never takes this path.

---

## Error handling

| Situation | Behavior |
|---|---|
| Cursor `sessionEnd` while PID/pane alive | Do not unregister. Log. Instance stays listening. |
| Cursor `sessionEnd` with empty stdin / parse fail | Do not `finalize_session` (handler never runs on parse fail today; keep it that way). Log `cursor.parse_error`. Exit 0. |
| `cursor-stop` empty stdin / truncated JSON | Log parse_error. **No ACK.** If env resolves an instance and unread exists, still write `followup_message`. Queue stays if write fails. |
| Stop cannot resolve an instance | `{}`, no follow-up, no ACK. Queue stays. |
| Stop `status` is not `completed` (aborted, error, missing) | Still attach unread if any. |
| No unread | `{}`. Cursor does not loop. |
| Stdout write of `followup_message` fails | No ACK. Queue stays for a later Stop. |
| `hcom stop` / `hcom kill` / process death | `stop_instance` as today. |
| Claude hook UUID ≠ primary, not a live same-`process_id` alias | Keep `historical_root_hook_rejected`. |

No in-turn poll. No `hcom listen` inside the agent turn.

---

## Testing

Unit tests are the merge gate. PTY tests prove A+B on a real Cursor CLI when the environment has one.

### Unit (CI: `cargo test --locked`)

New tests next to existing `#[cfg(test)]` in `src/hooks/cursor.rs` and `src/hooks/common.rs` (and bind tests in `src/instance_binding.rs` if Path 1b needs a case):

1. `handle_sessionend` does not delete the instance / does not call `finalize_session` behavior (row still present, status not `exit:completed`).
2. Two `session_id`s, one live `process_id` → one instance; no `historical_process_rejected`.
3. `handle_stop` with unread and `status` omitted or not `"completed"` → `followup_message` present.
4. `handle_stop` with empty queue → no `followup_message`.
5. Stop parse-fail path: no ACK; with `HCOM_PROCESS_ID` + unread → stdout contains `followup_message`.
6. `try_setup_cursor_hooks`: `hooks.stop[].timeout == 30` for the hcom command; other hcom hooks timeout 15; `loop_limit` is `null`; custom commands preserved.
7. Existing `finalize_session` / `stop_instance` tests still pass (`hcom stop` still deletes).
8. Claude: UUID mismatch **without** a live same-`process_id` alias still historical-rejects (`src/hooks/claude.rs` existing test `test_historical_root_hooks_are_rejected_before_dispatch` plus a case that a live same-process alias is not rejected in `init_hook_context`).

### PTY (`tests/test_pty_delivery.rs` or a Cursor-focused ignored test)

If Cursor CLI is available in the PTY job:

- Spawn `hcom cursor-agent` → idle several minutes → `hcom list` still `◉ listening`.
- One `hcom send` while idle → a `deliver:` / follow-up containing the packet; no second nudge.
- `hcom stop` afterwards → instance gone.

If the job has no Cursor CLI or the test is flaky, keep it `#[ignore]` and document the manual commands in this spec. Do not block merge on an environment that cannot run Cursor.

### Manual acceptance (from the issue)

- Idle several minutes still `◉ listening`.
- `hcom send` injects; no need for `hcom start --as` again.
- No `historical_root_hook_rejected` / `historical_process_rejected` between two UUIDs on the same `process_id`.
- Early `sessionEnd` does not become `exit:completed` while PID is alive.
- `hcom stop` / process death still unregisters.
- One idle `hcom send` → follow-up has the packet; worker ACK + work without a second nudge.
- Empty unread → Stop does not loop.

---

## Alternatives considered

1. **Cursor-only lifecycle (no `common.rs` change).** Smallest blast radius. Rejected in brainstorming: dual-UUID historical reject lives in shared `init_hook_context`; Cursor spawn evidence included those log lines. Shared live-process alias is in scope.
2. **AGY-style `soft_finalize_session`.** Keeps the row but marks inactive / `exit:…`. Roster would still look stopped until the next turn. Rejected: the invariant is “listening until stop/kill/process death,” not “soft-stopped until rebind.”
3. **Guard `sessionEnd` on pidtrack / `validated=false` / empty stdin.** More predicates, still finalizes a “real-looking” `completed` on a live PTY. Rejected: Cursor `sessionEnd` is never quit.
4. **Inject the full packet into the PTY and skip `followup_message`.** Breaks the existing Cursor idle contract (`tests/test_pty_delivery.rs`, `skills/.../cross-tool.md`). Rejected.
5. **Keep `status==completed` and only raise timeout.** Still drops follow-up on aborted/error Stop. Rejected.
6. **`stop.timeout` 60s or all Cursor hooks at 30s.** Extra latency on every Stop hang; other hooks do not need it. Chose 30s on stop only.

---

## File touch list (for the plan)

| File | Change |
|---|---|
| `src/hooks/cursor.rs` | `handle_sessionend`, `handle_stop`, parse-fail stop fallback, stop timeout 30, verify |
| `src/hooks/common.rs` | live `process_id` alias instead of `historical_process_rejected` |
| `src/instance_binding.rs` | do not session-switch-retire same live `process_id` |
| `src/hooks/claude.rs` | tests only, unless a tiny dispatch comment; do not remove historical root gate |
| `tests/test_pty_delivery.rs` | optional ignored/manual Cursor idle+follow-up case |
| `skills/hcom-agent-messaging/references/cross-tool.md` (and plugin copy if that is the published skill) | one-line: Cursor identity is process-lifetime; idle follow-up is automatic |

Do not change agent-ops, kaban, or Cursor product docs beyond the hcom cross-tool note.
