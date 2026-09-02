# Cursor CLI sessionEnd + idle follow-up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep a live `cursor-agent` PTY on the hcom bus across `sessionEnd`, alias a second Cursor session UUID onto the same process, attach idle packets on a healthy Stop without a `status==completed` gate, and delete every session alias when the instance actually stops.

**Architecture:** Cursor identity is process-lifetime. `cursor-sessionend` logs and returns (never `finalize_session`). Dual UUID is Cursor-only in `bind_session_to_process` Path 1b plus `rebind_session` (keep aliases). Idle delivery stays two-step (`<hcom>` inject → `followup_message`). Stop timeout 30s so the installer rewrites stale 15s `hooks.json`. Teardown cascade uses `DELETE session_bindings WHERE instance_name = ?`. No `init_hook_context` change, no parse-fail env fallback, no new reaper.

**Tech Stack:** Rust, rusqlite, `serial_test` for Cursor env tests, `cargo test --locked --lib`.

**Spec:** `docs/superpowers/specs/2026-08-29-cursor-cli-sessionend-and-idle-followup-design.md`

---

## File map

| File | Responsibility |
|---|---|
| `src/hooks/cursor.rs` | sessionEnd no-op; Stop without status gate; stop timeout 30; verify requires 30; sessionStart keep aliases |
| `src/instance_binding.rs` | Path 1b: do not `exit:session_switch` when process-bound `tool == "cursor"` |
| `src/db/instances.rs` | `finalize_instance_stop`: delete all session bindings for the name |
| `src/instance_lifecycle.rs` | `mark_dead_instances`: same cascade |
| `skills/hcom-agent-messaging/references/cross-tool.md` | Cursor process-lifetime + opportunistic reap |
| `plugin/hcom/skills/hcom-agent-messaging/references/cross-tool.md` | Same text (published copy) |

Do not edit `src/hooks/common.rs` `init_hook_context`, `src/hooks/claude.rs` production code, or add env-fallback on parse fail.

---

### Task 1: Stop hook timeout 30 + verify fails on 15

**Files:**
- Modify: `src/hooks/cursor.rs` (`HOOK_TIMEOUT_SECS`, `expected_hook`, `verify_hooks_at`, tests at end of `mod tests`)

This is the migration trigger: launcher only rewrites when `verify_cursor_hooks_installed` is false (`src/launcher.rs` Cursor branch).

- [ ] **Step 1: Write the failing test**

Append in `src/hooks/cursor.rs` `mod tests` (same `cursor_test_env` + `#[serial]` pattern):

```rust
    #[test]
    #[serial]
    fn verify_rejects_fifteen_second_stop_timeout() {
        let (_dir, workspace, _guard) = cursor_test_env();
        let hooks_path = workspace.join(".cursor/hooks.json");
        std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
        try_setup_cursor_hooks(false).unwrap();
        let mut root: Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks_path).unwrap()).unwrap();
        for entry in root["hooks"]["stop"].as_array_mut().unwrap() {
            if entry["command"] == build_cursor_hook_command("cursor-stop") {
                entry["timeout"] = json!(15);
            }
        }
        std::fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&root).unwrap(),
        )
        .unwrap();
        assert!(!verify_cursor_hooks_installed(false));
    }

    #[test]
    #[serial]
    fn setup_writes_stop_timeout_thirty() {
        let (_dir, workspace, _guard) = cursor_test_env();
        try_setup_cursor_hooks(false).unwrap();
        let root: Value = serde_json::from_str(
            &std::fs::read_to_string(workspace.join(".cursor/hooks.json")).unwrap(),
        )
        .unwrap();
        let stop = root["hooks"]["stop"].as_array().unwrap();
        let hcom = stop
            .iter()
            .find(|h| h["command"] == build_cursor_hook_command("cursor-stop"))
            .unwrap();
        assert_eq!(hcom["timeout"], json!(30));
        assert!(hcom["loop_limit"].is_null());
        for event in ["sessionStart", "sessionEnd", "preToolUse", "postToolUse"] {
            let entries = root["hooks"][event].as_array().unwrap();
            let hcom = entries
                .iter()
                .find(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("cursor-"))
                })
                .unwrap();
            assert_eq!(hcom["timeout"], json!(15), "{event}");
        }
        assert!(verify_cursor_hooks_installed(false));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --lib verify_rejects_fifteen_second_stop_timeout setup_writes_stop_timeout_thirty -- --test-threads=1`

Expected: FAIL — `verify_rejects_fifteen_second_stop_timeout` still true (timeout only `is_some()`), and/or stop timeout is 15 not 30.

- [ ] **Step 3: Write minimal implementation**

In `src/hooks/cursor.rs`:

```rust
const HOOK_TIMEOUT_SECS: u64 = 15;
const STOP_HOOK_TIMEOUT_SECS: u64 = 30;
```

In `expected_hook`, after inserting `timeout`:

```rust
        (
            "timeout".to_string(),
            json!(if event == "stop" {
                STOP_HOOK_TIMEOUT_SECS
            } else {
                HOOK_TIMEOUT_SECS
            }),
        ),
```

In `verify_hooks_at`, replace `entry.get("timeout").and_then(Value::as_u64).is_some()` with:

```rust
                    && entry.get("timeout").and_then(Value::as_u64)
                        == Some(if *event == "stop" {
                            STOP_HOOK_TIMEOUT_SECS
                        } else {
                            HOOK_TIMEOUT_SECS
                        })
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --lib cursor::tests -- --test-threads=1`

Expected: PASS (including existing setup tests).

- [ ] **Step 5: Commit**

```bash
git add src/hooks/cursor.rs
git commit -m "$(cat <<'EOF'
fix(cursor): require 30s stop hook timeout

Stale 15s hooks.json fails verify so the next cursor-agent spawn rewrites it.
EOF
)"
```

---

### Task 2: `handle_sessionend` must not unregister

**Files:**
- Modify: `src/hooks/cursor.rs` (`handle_sessionend` ~616–626, tests)

- [ ] **Step 1: Write the failing test**

Add a helper next to `cursor_test_env` and the test (needs `HcomDb`, `HcomContext`, `HookPayload`):

```rust
    fn seed_cursor_row(name: &str, session_id: &str, process_id: &str) {
        let db = HcomDb::open().unwrap();
        crate::instance_binding::initialize_instance_in_position_file(
            &db,
            name,
            Some(session_id),
            None,
            None,
            None,
            None,
            Some("cursor"),
            false,
            None,
            None,
            None,
            None,
            None,
        );
        db.rebind_session(session_id, name).unwrap();
        db.set_process_binding(process_id, session_id, name).unwrap();
    }

    #[test]
    #[serial]
    fn sessionend_completed_keeps_instance() {
        let (_dir, _workspace, _guard) = cursor_test_env();
        crate::config::Config::init();
        unsafe {
            std::env::set_var("HCOM_PROCESS_ID", "proc-zilo");
        }
        seed_cursor_row("zilo", "sess-a", "proc-zilo");
        let db = HcomDb::open().unwrap();
        let ctx = HcomContext::from_os();
        let raw = json!({
            "session_id": "sess-a",
            "conversation_id": "sess-a",
            "reason": "completed"
        });
        let payload = HookPayload::from_cursor_native("cursor-sessionend", raw);
        let out = handle_sessionend(&db, &ctx, &payload);
        assert_eq!(out, json!({}));
        let row = db.get_instance_full("zilo").unwrap().expect("row deleted");
        assert_ne!(row.status, crate::shared::ST_INACTIVE);
        assert!(!row.status_context.starts_with("exit:"));
    }
```

Add `use crate::db::HcomDb;` and `use crate::hooks::HookPayload;` in the test module if not already in scope (`super::*` already has HookPayload via the parent module — `HookPayload` is `crate::hooks::HookPayload`; cursor.rs uses it. `HcomDb` is already imported at crate level in cursor.rs).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --lib sessionend_completed_keeps_instance -- --test-threads=1`

Expected: FAIL — `row deleted` or status `inactive` / `exit:completed` because `finalize_session` ran.

- [ ] **Step 3: Write minimal implementation**

Replace `handle_sessionend` with:

```rust
fn handle_sessionend(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> Value {
    if let Some(instance) = resolved_instance(db, ctx, payload) {
        let reason = payload
            .raw
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        log::log_warn(
            "hooks",
            "cursor.sessionend.ignored",
            &format!(
                "instance={} reason={} (process-lifetime; not unregistering)",
                instance.name, reason
            ),
        );
    }
    json!({})
}
```

Do not call `finalize_session` or `soft_finalize_session`. Do not branch on `reason`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked --lib sessionend_completed_keeps_instance -- --test-threads=1`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hooks/cursor.rs
git commit -m "$(cat <<'EOF'
fix(cursor): ignore sessionEnd instead of finalize_session

Cursor CLI sessionEnd is a conversation UUID event, not process death.
EOF
)"
```

---

### Task 3: Stop follow-up without `status==completed`

**Files:**
- Modify: `src/hooks/cursor.rs` (`handle_stop` ~594–614)

- [ ] **Step 1: Write the failing tests**

```rust
    fn insert_broadcast(db: &HcomDb, from: &str, text: &str) {
        db.log_event(
            "message",
            from,
            &json!({"from": from, "text": text, "scope": "broadcast"}),
        )
        .unwrap();
    }

    #[test]
    #[serial]
    fn stop_followup_without_completed_status() {
        let (_dir, _workspace, _guard) = cursor_test_env();
        crate::config::Config::init();
        unsafe {
            std::env::set_var("HCOM_PROCESS_ID", "proc-kali");
        }
        seed_cursor_row("kali", "sess-k", "proc-kali");
        let db = HcomDb::open().unwrap();
        insert_broadcast(&db, "ops", "task for kali");
        let ctx = HcomContext::from_os();
        let payload = HookPayload::from_cursor_native(
            "cursor-stop",
            json!({"session_id": "sess-k", "conversation_id": "sess-k"}),
        );
        let (out, ack) = handle_stop(&db, &ctx, &payload);
        assert!(
            out.get("followup_message")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains("task for kali")),
            "{out}"
        );
        assert!(ack.is_some());
    }

    #[test]
    #[serial]
    fn stop_empty_queue_has_no_followup() {
        let (_dir, _workspace, _guard) = cursor_test_env();
        crate::config::Config::init();
        unsafe {
            std::env::set_var("HCOM_PROCESS_ID", "proc-idle");
        }
        seed_cursor_row("idle", "sess-i", "proc-idle");
        let db = HcomDb::open().unwrap();
        let ctx = HcomContext::from_os();
        let payload = HookPayload::from_cursor_native(
            "cursor-stop",
            json!({"session_id": "sess-i", "status": "completed"}),
        );
        let (out, ack) = handle_stop(&db, &ctx, &payload);
        assert_eq!(out, json!({}));
        assert!(ack.is_none());
    }
```

Keep the existing dispatch order in `dispatch_cursor_hook_native` (ACK only after `to_writer` + `flush`). Do not ACK on the parse-error branch (already returns 0 before handlers). Do not add `HCOM_PROCESS_ID` fallback on parse fail.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --lib stop_followup_without_completed_status stop_empty_queue_has_no_followup -- --test-threads=1`

Expected: `stop_followup_without_completed_status` FAIL (empty `{}` because status gate). Empty-queue test should already PASS.

- [ ] **Step 3: Write minimal implementation**

In `handle_stop`, delete:

```rust
    if payload.raw.get("status").and_then(Value::as_str) != Some("completed") {
        return (json!({}), None);
    }
```

Leave `set_status(listening)`, `notify_hook_instance_with_db`, and `prepare_pending_messages` as they are.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --lib stop_followup_without_completed_status stop_empty_queue_has_no_followup -- --test-threads=1`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hooks/cursor.rs
git commit -m "$(cat <<'EOF'
fix(cursor): attach followup_message even when status is not completed

Idle wakes often end Stop without status=completed; unread must still ship.
EOF
)"
```

---

### Task 4: Cursor dual-UUID bind (no session-switch retire) + keep aliases

**Files:**
- Modify: `src/instance_binding.rs` Path 1b ~504–532
- Modify: `src/hooks/cursor.rs` `handle_sessionstart` (~516) — `rebind_instance_session` wipes all aliases; switch to `rebind_session`
- Test: `src/instance_binding.rs` `mod tests` next to `test_bind_session_path1b_session_switch_marks_old_inactive`

Path 3 already rebinds a new UUID onto a process placeholder. Path 1b retires a live non-placeholder when the incoming session already has another canonical name. Gate the retire on `tool != "cursor"`.

`handle_sessionstart` currently:

```rust
    let _ = db.rebind_instance_session(&instance_name, session_id);
```

`rebind_instance_session` deletes **all** `session_bindings` for that name. For Cursor aliases, use `rebind_session` instead (upsert one session_id, keep the other).

- [ ] **Step 1: Write the failing tests**

In `src/instance_binding.rs` tests (copy the Path 1b fixture; add `"tool"`):

```rust
    #[test]
    fn test_bind_session_path1b_cursor_keeps_process_instance() {
        crate::config::Config::init();
        let (db, path) = setup_test_db();
        let now = now_epoch_i64();

        let mut canonical_data = serde_json::Map::new();
        canonical_data.insert("name".into(), serde_json::json!("miso"));
        canonical_data.insert("session_id".into(), serde_json::json!("sid-789"));
        canonical_data.insert("tool".into(), serde_json::json!("cursor"));
        canonical_data.insert("created_at".into(), serde_json::json!(now));
        canonical_data.insert("status".into(), serde_json::json!("listening"));
        db.save_instance_named("miso", &canonical_data).unwrap();
        db.rebind_session("sid-789", "miso").unwrap();

        let mut ph_data = serde_json::Map::new();
        ph_data.insert("name".into(), serde_json::json!("temp"));
        ph_data.insert("session_id".into(), serde_json::json!("sid-old"));
        ph_data.insert("tool".into(), serde_json::json!("cursor"));
        ph_data.insert("created_at".into(), serde_json::json!(now));
        ph_data.insert("status".into(), serde_json::json!("listening"));
        db.save_instance_named("temp", &ph_data).unwrap();
        db.rebind_session("sid-old", "temp").unwrap();
        db.set_process_binding("pid-123", "sid-old", "temp")
            .unwrap();

        let result = bind_session_to_process(&db, "sid-789", Some("pid-123"));
        assert_eq!(result, Some("miso".to_string()));

        let placeholder = db.get_instance_full("temp").unwrap().unwrap();
        assert_ne!(placeholder.status_context, "exit:session_switch");
        assert_ne!(placeholder.status, ST_INACTIVE);

        cleanup(path);
    }

    #[test]
    fn test_bind_session_path1b_non_cursor_still_session_switches() {
        crate::config::Config::init();
        let (db, path) = setup_test_db();
        let now = now_epoch_i64();

        let mut canonical_data = serde_json::Map::new();
        canonical_data.insert("name".into(), serde_json::json!("miso"));
        canonical_data.insert("session_id".into(), serde_json::json!("sid-789"));
        canonical_data.insert("tool".into(), serde_json::json!("claude"));
        canonical_data.insert("created_at".into(), serde_json::json!(now));
        canonical_data.insert("status".into(), serde_json::json!("listening"));
        db.save_instance_named("miso", &canonical_data).unwrap();
        db.rebind_session("sid-789", "miso").unwrap();

        let mut ph_data = serde_json::Map::new();
        ph_data.insert("name".into(), serde_json::json!("temp"));
        ph_data.insert("session_id".into(), serde_json::json!("sid-old"));
        ph_data.insert("tool".into(), serde_json::json!("claude"));
        ph_data.insert("created_at".into(), serde_json::json!(now));
        ph_data.insert("status".into(), serde_json::json!("listening"));
        db.save_instance_named("temp", &ph_data).unwrap();
        db.rebind_session("sid-old", "temp").unwrap();
        db.set_process_binding("pid-123", "sid-old", "temp")
            .unwrap();

        let result = bind_session_to_process(&db, "sid-789", Some("pid-123"));
        assert_eq!(result, Some("miso".to_string()));
        let placeholder = db.get_instance_full("temp").unwrap().unwrap();
        assert_eq!(placeholder.status, ST_INACTIVE);
        assert_eq!(placeholder.status_context, "exit:session_switch");

        cleanup(path);
    }
```

In `src/hooks/cursor.rs` tests:

```rust
    #[test]
    #[serial]
    fn sessionstart_second_uuid_keeps_first_session_binding() {
        let (_dir, _workspace, _guard) = cursor_test_env();
        crate::config::Config::init();
        unsafe {
            std::env::set_var("HCOM_PROCESS_ID", "proc-dual");
        }
        seed_cursor_row("dual", "uuid-a", "proc-dual");
        let db = HcomDb::open().unwrap();
        let ctx = HcomContext::from_os();
        let payload = HookPayload::from_cursor_native(
            "cursor-sessionstart",
            json!({"session_id": "uuid-b", "conversation_id": "uuid-b"}),
        );
        let _ = handle_sessionstart(&db, &ctx, &payload);
        assert_eq!(
            db.get_session_binding("uuid-a").unwrap(),
            Some("dual".to_string())
        );
        assert_eq!(
            db.get_session_binding("uuid-b").unwrap(),
            Some("dual".to_string())
        );
        assert!(db.get_instance_full("dual").unwrap().is_some());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --lib test_bind_session_path1b_cursor_keeps_process_instance sessionstart_second_uuid_keeps_first_session_binding -- --test-threads=1`

Expected: FAIL — cursor Path 1b still `exit:session_switch`; sessionstart drops `uuid-a` binding.

- [ ] **Step 3: Write minimal implementation**

In `src/instance_binding.rs` Path 1b `else` (the `exit:session_switch` block), wrap the retire in:

```rust
            } else {
                let skip_cursor_retire = placeholder_data
                    .as_ref()
                    .is_some_and(|row| row.tool == "cursor");
                if skip_cursor_retire {
                    crate::log::log_info(
                        "binding",
                        "bind_canonical.cursor_alias",
                        &format!("keeping {ph_name} for process; aliasing {canonical_name} session"),
                    );
                } else {
                    // existing migrate-fail log + set_status ST_INACTIVE +
                    // delete_session_bindings_for_instance(ph_name)
                }
            }
```

Keep `set_process_binding(pid, session_id, canonical_name)` and `return Some(canonical_name)` as today so Claude/other tools unchanged.

In `src/hooks/cursor.rs` `handle_sessionstart`, replace:

```rust
    let _ = db.rebind_instance_session(&instance_name, session_id);
```

with:

```rust
    if let Err(e) = db.rebind_session(session_id, &instance_name) {
        log::log_warn(
            "hooks",
            "cursor.sessionstart.rebind_session",
            &format!("instance={instance_name} err={e}"),
        );
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```
cargo test --locked --lib test_bind_session_path1b_cursor_keeps_process_instance test_bind_session_path1b_non_cursor_still_session_switches test_bind_session_path1b_session_switch_marks_old_inactive sessionstart_second_uuid_keeps_first_session_binding -- --test-threads=1
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/instance_binding.rs src/hooks/cursor.rs
git commit -m "$(cat <<'EOF'
fix(cursor): alias a second session UUID on the live process

Skip session-switch retire for tool=cursor and keep prior session_bindings.
EOF
)"
```

---

### Task 5: Delete every session alias on stop and dead-PID mark

**Files:**
- Modify: `src/db/instances.rs` `finalize_instance_stop` ~425–434
- Modify: `src/instance_lifecycle.rs` `mark_dead_instances` ~813–822
- Test: `src/db/instances.rs` `#[cfg(test)]` and `src/instance_lifecycle.rs` `#[cfg(test)]`

Inside `finalize_instance_stop` the work runs on transaction `tx`. Do **not** call `HcomDb::delete_session_bindings_for_instance` (it uses `self.conn` outside the txn). Use SQL on `tx`.

- [ ] **Step 1: Write the failing tests**

In `src/db/instances.rs` tests (follow `cleanup_test_db` / `open_at` pattern already in that file):

```rust
    #[test]
    fn finalize_instance_stop_deletes_all_session_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("hcom.db");
        let db = HcomDb::open_at(&db_path).unwrap();
        db.conn()
            .execute(
                "INSERT INTO instances (name, session_id, tool, status, status_context, status_time, created_at, last_event_id)
                 VALUES ('zilo', 'uuid-a', 'cursor', 'listening', 'start', 0, 1.0, 0)",
                [],
            )
            .unwrap();
        db.rebind_session("uuid-a", "zilo").unwrap();
        db.rebind_session("uuid-b", "zilo").unwrap();
        let won = db
            .finalize_instance_stop("zilo", 1.0, Some("uuid-a"), None, &serde_json::json!({"action":"stopped"}))
            .unwrap();
        assert!(won);
        assert_eq!(db.get_session_binding("uuid-a").unwrap(), None);
        assert_eq!(db.get_session_binding("uuid-b").unwrap(), None);
        assert!(db.get_instance_full("zilo").unwrap().is_none());
    }
```

In `src/instance_lifecycle.rs` tests, reuse `setup_test_db` / `default_instance` / `cleanup`:

```rust
    #[test]
    fn mark_dead_deletes_all_session_aliases() {
        let (db, path) = setup_test_db();
        let mut row = default_instance();
        row.name = "deadc".into();
        row.tool = "cursor".into();
        row.status = ST_LISTENING.into();
        row.pid = Some(1_000_000_007); // not a live process
        row.created_at = 1.0;
        db.save_instance_named("deadc", &{
            let mut m = serde_json::Map::new();
            m.insert("name".into(), serde_json::json!("deadc"));
            m.insert("tool".into(), serde_json::json!("cursor"));
            m.insert("status".into(), serde_json::json!(ST_LISTENING));
            m.insert("pid".into(), serde_json::json!(1_000_000_007));
            m.insert("created_at".into(), serde_json::json!(1.0));
            m
        })
        .unwrap();
        db.rebind_session("uuid-a", "deadc").unwrap();
        db.rebind_session("uuid-b", "deadc").unwrap();
        let n = mark_dead_instances(&db);
        assert!(n >= 1);
        assert_eq!(db.get_session_binding("uuid-a").unwrap(), None);
        assert_eq!(db.get_session_binding("uuid-b").unwrap(), None);
        cleanup(path);
    }
```

If `save_instance_named` ignores `pid`, set pid with `db.conn().execute("UPDATE instances SET pid = ? WHERE name = 'deadc'", ...)` after insert. Check the saved row’s `pid` in the debugger/test if the first run does not mark dead.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --lib finalize_instance_stop_deletes_all_session_aliases mark_dead_deletes_all_session_aliases`

Expected: FAIL — `uuid-b` binding still present.

- [ ] **Step 3: Write minimal implementation**

In `finalize_instance_stop`, replace the `if let Some(session_id)` block that deletes `session_bindings WHERE session_id = ?` with:

```rust
            tx.execute(
                "DELETE FROM session_bindings WHERE instance_name = ?",
                params![name],
            )?;
            if let Some(session_id) = session_id {
                tx.execute(
                    "DELETE FROM process_bindings WHERE session_id = ?",
                    params![session_id],
                )?;
            }
```

Keep the existing `DELETE FROM process_bindings WHERE instance_name = ?`.

In `mark_dead_instances`, replace the `if let Some(ref session_id)` session_bindings delete with:

```rust
        let _ = db.delete_session_bindings_for_instance(&inst.name);
        if let Some(ref session_id) = inst.session_id {
            let _ = db.conn().execute(
                "DELETE FROM process_bindings WHERE session_id = ?",
                rusqlite::params![session_id],
            );
        }
```

Keep `DELETE FROM process_bindings WHERE instance_name = ?`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --lib finalize_instance_stop_deletes_all_session_aliases mark_dead_deletes_all_session_aliases`

Also: `cargo test --locked --lib test_finalize_session_calls_stop test_stop_instance_basic_cleanup`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/db/instances.rs src/instance_lifecycle.rs
git commit -m "$(cat <<'EOF'
fix: drop all session aliases when an instance stops

Canonical session_id delete left extra Cursor UUID rows in session_bindings.
EOF
)"
```

---

### Task 6: Cross-tool docs + spec status

**Files:**
- Modify: `skills/hcom-agent-messaging/references/cross-tool.md` Cursor bullet list (~58–67)
- Modify: `plugin/hcom/skills/hcom-agent-messaging/references/cross-tool.md` (same paragraph)
- Modify: spec status line in `docs/superpowers/specs/2026-08-29-cursor-cli-sessionend-and-idle-followup-design.md`

- [ ] **Step 1: Edit Cursor section** (both skill copies)

Replace the Cursor **Session binding** and **Message delivery** bullets with:

```markdown
- **Session binding**: On sessionStart. Identity is **process-lifetime** (`process_id`): `sessionEnd` does not unregister. A second session UUID on the same live Cursor process is an alias. Process death is reaped on the next `hcom` CLI process (`mark_dead_instances` in `main.rs`), not by a background watcher.
- **Message delivery**: Hook-based when hcom-launched. Active turn → body in postToolUse `additional_context`. Idle agent → PTY injects only `<hcom>`; a healthy `stop` hook puts the packet in `followup_message` (status need not be `completed`). Empty stdin on stop does not ACK; the next healthy stop re-delivers. `stop.timeout` is 30s (rewritten on next `hcom cursor-agent` spawn if still 15).
```

- [ ] **Step 2: Flip spec status**

Change the spec header **Status** to: `Plan written at docs/superpowers/plans/2026-08-29-cursor-cli-sessionend-and-idle-followup.md`

- [ ] **Step 3: Commit**

```bash
git add skills/hcom-agent-messaging/references/cross-tool.md plugin/hcom/skills/hcom-agent-messaging/references/cross-tool.md docs/superpowers/specs/2026-08-29-cursor-cli-sessionend-and-idle-followup-design.md
git commit -m "$(cat <<'EOF'
docs: Cursor process-lifetime identity and idle follow-up

EOF
)"
```

No failing test for markdown.

---

### Task 7: Regression sweep

**Files:** none new.

- [ ] **Step 1: Run Cursor + bind + DB + Claude historical tests**

```
cargo test --locked --lib cursor::tests -- --test-threads=1
cargo test --locked --lib test_bind_session_path1b
cargo test --locked --lib finalize_instance_stop_deletes_all_session_aliases mark_dead_deletes_all_session_aliases
cargo test --locked --lib test_historical_root_hooks_are_rejected_before_dispatch
cargo test --locked --lib test_finalize_session_calls_stop
```

Expected: all PASS. Claude historical reject unchanged.

- [ ] **Step 2: If anything fails, fix in the file that caused it; do not “relax” Claude tests.**

- [ ] **Step 3: Manual acceptance (not merge-blocking)**

On a machine with Cursor CLI + herdr:

1. `HCOM_TAG=manual uvx hcom cursor-agent` in this repo.
2. Idle a few minutes; `uvx hcom list` still `◉ listening`.
3. `uvx hcom send @manual-<name> -- ping` — one follow-up with packet, no second nudge.
4. `uvx hcom stop @manual-<name>` — instance gone; no leftover `session_bindings` for that name.

PTY job: do **not** add a new ignored test in this change set (spec allows skip if Cursor CLI is absent).

No commit unless Step 2 produced a fix.

---

## Out of scope (do not implement)

- `init_hook_context.historical_process_rejected` relaxation
- Parse-fail `HCOM_PROCESS_ID` env fallback
- Background PID daemon
- `HCOM_TIMEOUT` 10800 on Cursor stop
- agent-ops / kaban / Cursor IDE
