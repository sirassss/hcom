# TUI Dead-Agent Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** TUI đang mở loại local dead-PID agent khỏi live roster trong khoảng hai giây, dùng cleanup atomic và an toàn khi race.

**Architecture:** Startup và TUI dùng cùng reaper, dựa vào PID death. Reaper reuse DB finalization transaction; TUI gọi maintenance qua DataSource mỗi một giây trước reload. Fixture/remote data sources mặc định no-op.

**Tech Stack:** Rust, rusqlite, existing HcomDb transaction helpers, ratatui/crossterm.

**Spec:** [2026-09-13-codex-plugin-skill-and-session-lifecycle-design.md](../specs/2026-09-13-codex-plugin-skill-and-session-lifecycle-design.md)

## Global Constraints

- Không thêm `SessionEnd` handler hoặc thay đổi nghĩa của `Stop` trong change này.
- Không thêm daemon/background watcher.
- Không phụ thuộc Herdr pane-close API để đảm bảo correctness.
- Không thay đổi lifecycle của remote hoặc PID-less agents.
- Không xóa instance nếu PID vẫn sống, kể cả khi tab Herdr chỉ bị detach.
- Reason dùng `exit:dead_process`; detector là `startup` hoặc `tui`.
- Không stop/kill agent thật nếu chưa có explicit owner go-ahead; automated tests dùng injected process state và isolated DB.

---

## File map

| File | Change |
|---|---|
| `src/db/instances.rs` | Reuse/extend atomic finalization with PID and identity guards |
| `src/instance_lifecycle.rs` | Reaper result, detector, conditional cleanup and tests |
| `src/main.rs` | Preserve startup call, error logging if return contract changes |
| `src/tui/data.rs` | Default no-op maintenance interface |
| `src/tui/db.rs` | Maintenance using retained local DB handle |
| `src/tui/app.rs` | Independent one-second cadence before reload |
| `src/pidtrack.rs` | Only if audit shows unknown/error can be treated as dead |

### Task 1: Make dead-process cleanup atomic and retryable

**Interfaces:** Keep `mark_dead_instances(db: &HcomDb) -> i32` as startup-compatible wrapper. Add `reconcile_dead_instances(db: &HcomDb, detector: DeadProcessDetector) -> anyhow::Result<usize>` and `DeadProcessDetector::{Startup, Tui}`. Tests inject a PID probe via private `reconcile_dead_instances_with_probe`; production uses existing process checker after its error semantics are audited.

- [ ] **Step 1: Add failing regression fixtures in lifecycle/db tests.** Use existing `setup_test_db()` and isolated `HcomDb::open_raw`. Insert active/listening/live/remote/PID-less/launching/inactive rows, session aliases, process bindings, notify endpoint and subscription. Inject `Alive`, `Dead`, `Unknown`; expected removed set contains only active/listening dead local rows. Query life events and require one stopped event per successful identity cleanup.
- [ ] **Step 2: Add race and rollback regressions before implementation.** Read candidate, replace same name with another `created_at`, then finalize stale candidate → false/no-op. Repeat with same identity but updated PID/status/session. Force event INSERT failure with a temporary SQLite trigger raising ABORT; entire delete/cleanup must roll back. Remove trigger and retry → one cleanup/event. Execute PTY finalizer then reaper and reversed order, each yielding one event.
- [ ] **Step 3: Run `cargo test instance_lifecycle` and `cargo test db::instances`.** New detector/atomic tests fail on old reaper; preserve existing finalization tests.
- [ ] **Step 4: Reuse the existing transaction rather than separate cleanup statements.** `HcomDb::finalize_instance_stop(name, created_at, session_id, agent_id, event_data)` already deletes via identity CAS, cleans bindings/endpoints/subscriptions and inserts one life event. Extend its internal implementation with an optional expected PID/status guard for dead-process callers while keeping existing public callers unchanged. Conditional DELETE must include the observed PID/status along with identity:

```sql
DELETE FROM instances
WHERE name = ? AND created_at = ?
  AND session_id IS ? AND agent_id IS ?
  AND pid IS ? AND status = ?
```

Recheck liveness immediately before conditional finalization; unknown/permission/check errors preserve row. Preserve the full current snapshot. Event data retains the schema used by `hooks/common.rs` and adds detector alongside `reason: "exit:dead_process"`. Count only `Ok(true)`; `Ok(false)` is a normal race; errors log and leave state retryable. Audit OS PID reuse limitations and preserve existing process identity checks where available.
- [ ] **Step 5: Replace reaper's separate cleanup/event/delete sequence.** Startup wrapper invokes detector Startup and logs errors without aborting hcom. TUI will call result-returning function. Search `rg -n 'exit:reboot|mark_dead_instances' src tests docs` and update semantic references tied to this reaper, preserving historical issue descriptions.
- [ ] **Step 6: Run lifecycle/db tests and commit:** `fix(lifecycle): finalize dead processes atomically with identity guards`.

### Task 2: Add data-source maintenance with a retained database

**Files:** `src/tui/data.rs`, `src/tui/db.rs`; tests beside the DB-backed data source.

**Interfaces:** Extend `DataSource` with:

```rust
fn reconcile_dead_instances(&mut self) -> anyhow::Result<usize> {
    Ok(0)
}
```

DbDataSource overrides this using Task 1 reaper and detector Tui; fixture implementations remain no-op.

- [ ] **Step 1: Write a fixture-source test that calls maintenance and observes no mutation; DB-source test removes a dead local row and returns 1.** Subsequent call returns 0. Check that data-source reload sees committed deletion even if it caches previous DB revision.
- [ ] **Step 2: Run `cargo test tui::db`; new maintenance test fails before implementation.** Inspect existing DbDataSource connection/reopen behavior and do not open a DB on every frame.
- [ ] **Step 3: Implement maintenance with the retained local DB handle.** If connection unavailable, return error, keep TUI source alive and retry opening only at next maintenance cadence. Invoke the lifecycle function with detector Tui. Make load-cache invalidation observe the successful local write; never issue PID checks for remote/fixture-only data.
- [ ] **Step 4: Run `cargo test tui` and commit:** `feat(tui): expose local lifecycle maintenance through the data source`.

### Task 3: Run one-second maintenance before TUI reload

**Files:** `src/tui/app.rs`; timing tests beside app module.

**Interfaces:** Track `last_reconcile: Instant` independently of `last_reload`; one-second interval. Add a small testable due predicate if needed; use supplied Instants in tests, no real sleep.

- [ ] **Step 1: Add fake-source event recording.** Simulate ticks at 0ms, 350ms, 999ms, 1000ms and 2000ms. Expected: no early maintenance, one call at each due boundary, maintenance before load/redraw, successful deletion forces fresh roster; failure keeps UI alive and retries next second. Cover viewport switch/re-enter without creating duplicate maintenance loops.
- [ ] **Step 2: Run `cargo test tui`; new schedule test fails before timer integration.** Test observable call ordering, not just a copied arithmetic expression.
- [ ] **Step 3: Insert cadence before existing data reload section.** Implement this control flow with current app helpers:

```rust
if last_reconcile.elapsed() >= Duration::from_secs(1) {
    last_reconcile = std::time::Instant::now();
    match self.source.reconcile_dead_instances() {
        Ok(n) if n > 0 => {
            self.reload_data();
            last_reload = std::time::Instant::now();
            dirty = true;
        }
        Ok(_) => {}
        Err(error) => crate::log::log_warn("tui", "dead_process_reconcile", &error.to_string()),
    }
}
```

Keep normal 120–350ms reload and input/RPC timing. Prefer one maintenance pass per due tick; no catch-up loop after pause. Slow DB errors must not become continuous retries or new per-frame database opens.
- [ ] **Step 4: Run `cargo test tui`, `cargo test instance_lifecycle`, then full `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.** Only broaden again if new failures or edits require it.
- [ ] **Step 5: Commit:** `fix(tui): reconcile dead local agents before periodic reload`.

### Task 4: Record end-to-end behavior

**Files:** spec acceptance section and related issue.

- [ ] **Step 1:** Use isolated database/process fixtures to measure dead-row removal from a running TUI without restart. Validate active and listening, no removal for live process or detached terminal, one stopped event with detector tui. Target <=2 seconds under normal responsive DB/UI conditions; record timings and distinguish fixture coverage from real terminal evidence.
- [ ] **Step 2:** If owner authorizes a live process-group termination test, read HOST.md and use a designated visible test agent. Observe confirmed PID death and roster removal, then a detach-only case with live PID retained. Do not kill an existing working agent to exercise this criterion. If permission is absent, keep this manual criterion unverified.
- [ ] **Step 3:** Record tests, version/environment, timing, and any skipped manual criterion. Note stopped history may retain a snapshot even though live roster no longer contains the agent. Commit docs: `docs: record dead-agent reconciliation verification`.

## Self-review coverage

Atomic cleanup/idempotency/cascades/reason/detector/error safety → Task 1; retained DB and remote/fixture isolation → Task 2; cadence/order/retry/timing → Task 3; live death versus detach and visible roster acceptance → Task 4. Plugin installation/customization work is independent and remains in the companion plan.
