//! Instance table accessors and row conversion.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{HcomDb, chrono_now_iso, subscriptions};
use crate::shared::constants::ST_LISTENING;
use crate::shared::time::now_epoch_i64;

/// Instance status info
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceStatus {
    pub status: String,
    pub detail: String,
    pub last_event_id: i64,
}

/// Full instance row from the instances table.
#[derive(Debug, Clone)]
pub struct InstanceRow {
    pub name: String,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub parent_name: Option<String>,
    pub agent_id: Option<String>,
    pub tag: Option<String>,
    pub last_event_id: i64,
    pub last_stop: i64,
    pub status: String,
    pub status_time: i64,
    pub last_seen: i64,
    pub status_context: String,
    pub status_detail: String,
    pub directory: String,
    pub created_at: f64,
    pub transcript_path: String,
    pub tool: String,
    pub background: i64,
    pub background_log_file: String,
    pub tcp_mode: i64,
    pub wait_timeout: Option<i64>,
    pub subagent_timeout: Option<i64>,
    pub hints: Option<String>,
    pub origin_device_id: Option<String>,
    pub pid: Option<i64>,
    /// PID namespace `pid` was observed in (see
    /// [`crate::sys::process::current_pid_namespace`]). `None` for rows written
    /// before this column existed, or on platforms without PID namespaces.
    pub pid_namespace: Option<String>,
    pub launch_args: Option<String>,
    pub terminal_preset_requested: Option<String>,
    pub terminal_preset_effective: Option<String>,
    pub launch_context: Option<String>,
    pub name_announced: i64,
    pub idle_since: Option<String>,
}

impl InstanceRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get("name")?,
            session_id: row
                .get::<_, Option<String>>("session_id")?
                .filter(|s| !s.is_empty()),
            parent_session_id: row
                .get::<_, Option<String>>("parent_session_id")?
                .filter(|s| !s.is_empty()),
            parent_name: row
                .get::<_, Option<String>>("parent_name")?
                .filter(|s| !s.is_empty()),
            agent_id: row
                .get::<_, Option<String>>("agent_id")?
                .filter(|s| !s.is_empty()),
            tag: row
                .get::<_, Option<String>>("tag")?
                .filter(|s| !s.is_empty()),
            last_event_id: row.get::<_, Option<i64>>("last_event_id")?.unwrap_or(0),
            last_stop: row.get::<_, Option<i64>>("last_stop")?.unwrap_or(0),
            status: row
                .get::<_, Option<String>>("status")?
                .unwrap_or_else(|| "inactive".into()),
            status_time: row.get::<_, Option<i64>>("status_time")?.unwrap_or(0),
            last_seen: row.get::<_, Option<i64>>("last_seen")?.unwrap_or(0),
            status_context: row
                .get::<_, Option<String>>("status_context")?
                .unwrap_or_default(),
            status_detail: row
                .get::<_, Option<String>>("status_detail")?
                .unwrap_or_default(),
            directory: row
                .get::<_, Option<String>>("directory")?
                .unwrap_or_default(),
            created_at: row.get::<_, Option<f64>>("created_at")?.unwrap_or(0.0),
            transcript_path: row
                .get::<_, Option<String>>("transcript_path")?
                .unwrap_or_default(),
            tool: row
                .get::<_, Option<String>>("tool")?
                .unwrap_or_else(|| "claude".into()),
            background: row.get::<_, Option<i64>>("background")?.unwrap_or(0),
            background_log_file: row
                .get::<_, Option<String>>("background_log_file")?
                .unwrap_or_default(),
            tcp_mode: row.get::<_, Option<i64>>("tcp_mode")?.unwrap_or(0),
            wait_timeout: row.get::<_, Option<i64>>("wait_timeout")?,
            subagent_timeout: row.get::<_, Option<i64>>("subagent_timeout")?,
            hints: row
                .get::<_, Option<String>>("hints")?
                .filter(|s| !s.is_empty()),
            origin_device_id: row
                .get::<_, Option<String>>("origin_device_id")?
                .filter(|s| !s.is_empty()),
            pid: row.get::<_, Option<i64>>("pid")?,
            pid_namespace: row
                .get::<_, Option<String>>("pid_namespace")?
                .filter(|s| !s.is_empty()),
            launch_args: row
                .get::<_, Option<String>>("launch_args")?
                .filter(|s| !s.is_empty()),
            terminal_preset_requested: row
                .get::<_, Option<String>>("terminal_preset_requested")?
                .filter(|s| !s.is_empty()),
            terminal_preset_effective: row
                .get::<_, Option<String>>("terminal_preset_effective")?
                .filter(|s| !s.is_empty()),
            launch_context: row
                .get::<_, Option<String>>("launch_context")?
                .filter(|s| !s.is_empty()),
            name_announced: row.get::<_, Option<i64>>("name_announced")?.unwrap_or(0),
            idle_since: row
                .get::<_, Option<String>>("idle_since")?
                .filter(|s| !s.is_empty()),
        })
    }
}

/// Get instance by name. Returns full row as JSON or None.
/// Column list for instance SELECT queries. Must match instance_row_to_json index order.
pub(super) const INSTANCE_COLUMNS: &str =
    "name, session_id, parent_session_id, parent_name, tag, last_event_id,
     status, status_time, last_seen, status_context, status_detail, last_stop, directory,
     created_at, transcript_path, tcp_mode, wait_timeout, background,
     background_log_file, name_announced, agent_id,
     origin_device_id, hints, subagent_timeout, tool, launch_args,
     terminal_preset_requested, terminal_preset_effective,
     idle_since, pid, launch_context";

impl HcomDb {
    /// Get instance status by name
    ///
    /// Returns:
    /// - Ok(Some(status)) if instance exists
    /// - Ok(None) if instance not found
    /// - Err if database error occurs
    pub fn get_instance_status(&self, name: &str) -> Result<Option<InstanceStatus>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT name, status, status_detail, last_event_id
             FROM instances WHERE name = ?",
        )?;

        match stmt.query_row(params![name], |row| {
            Ok(InstanceStatus {
                status: row
                    .get::<_, String>(1)
                    .unwrap_or_else(|_| "unknown".to_string()),
                detail: row.get::<_, String>(2).unwrap_or_default(),
                last_event_id: row.get::<_, i64>(3).unwrap_or(0),
            })
        }) {
            Ok(status) => Ok(Some(status)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set instance status in the live `instances` row.
    pub fn set_status(&self, name: &str, status: &str, context: &str) -> Result<()> {
        let now = now_epoch_i64();

        // Update last_stop heartbeat when entering listening state
        if status == ST_LISTENING {
            self.conn.execute(
                "UPDATE instances SET status = ?, status_context = ?, status_time = ?, last_stop = ? WHERE name = ?",
                params![status, context, now, now, name],
            )?;
        } else {
            self.conn.execute(
                "UPDATE instances SET status = ?, status_context = ?, status_time = ? WHERE name = ?",
                params![status, context, now, name],
            )?;
        }

        Ok(())
    }

    /// Update gate blocking status WITHOUT logging a status event.
    ///
    /// Used for transient PTY gate states (tui:*) that shouldn't pollute the events table.
    /// Only updates the instance row; TUI reads this for display but no event is created.
    ///
    /// Args:
    ///   context: Gate context like "tui:not-ready", "tui:user-active", etc.
    ///   detail: Human-readable description like "user is typing"
    /// Preserve status_detail when it's "cmd:listen" — gate diagnostics must not
    /// overwrite the flag that blocks PTY injection during `hcom listen`.
    /// Returns false if a hook has already moved the instance out of listening.
    pub fn set_gate_status(&self, name: &str, context: &str, detail: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE instances SET status_context = ?,
                status_detail = CASE WHEN status_detail = 'cmd:listen' THEN status_detail ELSE ? END
             WHERE name = ? AND status = ?",
            params![context, detail, name, ST_LISTENING],
        )?;
        Ok(changed > 0)
    }

    /// Clear a gate-block context, but only if it is still the one we wrote.
    ///
    /// The delivery loop writes `tui:<reason>` contexts and later clears them,
    /// while hooks write their own (`tool:Bash`) to the same column. An
    /// unconditional clear erases whatever the hook put there; a read-then-
    /// clear still loses the race. Comparing in the WHERE clause is what makes
    /// this safe. `cmd:listen` details are preserved as in `set_gate_status`.
    ///
    /// Returns whether a row matched, so a caller can tell "cleared" from
    /// "someone else owns it now" — both mean the caller may drop its marker,
    /// but only an `Err` means it should keep it and retry.
    pub fn clear_gate_status_if(&self, name: &str, expected_context: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE instances SET status_context = '',
                status_detail = CASE WHEN status_detail = 'cmd:listen' THEN status_detail ELSE '' END
             WHERE name = ? AND status_context = ?",
            params![name, expected_context],
        )?;
        Ok(rows > 0)
    }

    /// Update instance PID after spawn, stamping the namespace it was observed
    /// in. The caller spawned this process, so its own namespace is the right
    /// one; a caller copying a PID observed elsewhere must use
    /// [`Self::update_instance_fields`] with an explicit `pid_namespace`.
    pub fn update_instance_pid(&self, name: &str, pid: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE instances SET pid = ?, pid_namespace = ? WHERE name = ?",
            params![
                pid as i64,
                crate::sys::process::current_pid_namespace().unwrap_or_default(),
                name
            ],
        )?;
        Ok(())
    }

    /// Store launch_context JSON (terminal preset, pane_id, env snapshot).
    /// Merges incoming keys into existing JSON, only filling fields that are
    /// currently missing or empty so late-bound PTY metadata can be persisted
    /// without clobbering richer hook-captured context.
    pub fn store_launch_context(&self, name: &str, context_json: &str) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            let existing_json: Option<String> = self
                .conn
                .query_row(
                    "SELECT launch_context FROM instances WHERE name = ?",
                    params![name],
                    |row| row.get(0),
                )
                .optional()?;

            let existing_json = existing_json.unwrap_or_default();
            if existing_json.is_empty() {
                self.conn.execute(
                    "UPDATE instances SET launch_context = ? WHERE name = ?",
                    params![context_json, name],
                )?;
                return Ok(());
            }

            let mut existing = match serde_json::from_str::<
                serde_json::Map<String, serde_json::Value>,
            >(&existing_json)
            {
                Ok(map) => map,
                Err(_) => return Ok(()),
            };
            let incoming = match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                context_json,
            ) {
                Ok(map) => map,
                Err(_) => return Ok(()),
            };

            let mut changed = false;
            for (key, value) in incoming {
                let should_fill = existing.get(&key).is_none_or(|current| {
                    current.is_null() || current.as_str().is_some_and(str::is_empty)
                });
                if should_fill {
                    existing.insert(key, value);
                    changed = true;
                }
            }

            if changed {
                self.conn.execute(
                    "UPDATE instances SET launch_context = ? WHERE name = ?",
                    params![
                        serde_json::to_string(&existing).unwrap_or_else(|_| "{}".to_string()),
                        name
                    ],
                )?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Get instance tag (for display name computation).
    /// Returns Some(tag) if tag is non-empty, None otherwise.
    pub fn get_instance_tag(&self, name: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT tag FROM instances WHERE name = ?",
                params![name],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .filter(|t| !t.is_empty())
    }

    /// Get current status and context for gate blocking logic
    ///
    /// Returns:
    /// - Ok(Some((status, context))) if instance exists
    /// - Ok(None) if instance not found
    /// - Err if database error occurs
    pub fn get_status(&self, name: &str) -> Result<Option<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT status, status_context FROM instances WHERE name = ?")?;

        match stmt.query_row(params![name], |row| {
            Ok((
                row.get::<_, String>(0)
                    .unwrap_or_else(|_| "unknown".to_string()),
                row.get::<_, String>(1).unwrap_or_default(),
            ))
        }) {
            Ok(status) => Ok(Some(status)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get transcript path for an instance
    ///
    /// Returns:
    /// - Ok(Some(path)) if instance exists and has non-empty transcript_path
    /// - Ok(None) if instance not found or transcript_path is empty
    /// - Err if database error occurs
    pub fn get_transcript_path(&self, name: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT transcript_path FROM instances WHERE name = ?")?;

        match stmt.query_row(params![name], |row| row.get::<_, String>(0)) {
            Ok(path) if !path.is_empty() => Ok(Some(path)),
            Ok(_) => Ok(None), // Empty path
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get instance snapshot for life event logging before deletion
    ///
    /// Returns:
    /// - Ok(Some(snapshot)) if instance exists
    /// - Ok(None) if instance not found
    /// - Err if database error occurs
    pub fn get_instance_snapshot(&self, name: &str) -> Result<Option<serde_json::Value>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT transcript_path, session_id, tool, directory, parent_name, tag,
                    wait_timeout, subagent_timeout, hints, pid, created_at, background,
                    agent_id, launch_args, origin_device_id, background_log_file, last_event_id
             FROM instances WHERE name = ?",
        )?;

        match stmt.query_row(params![name], |row| {
            Ok(serde_json::json!({
                "transcript_path": row.get::<_, String>(0).unwrap_or_default(),
                "session_id": row.get::<_, String>(1).unwrap_or_default(),
                "tool": row.get::<_, String>(2).unwrap_or_default(),
                "directory": row.get::<_, String>(3).unwrap_or_default(),
                "parent_name": row.get::<_, String>(4).unwrap_or_default(),
                "tag": row.get::<_, String>(5).unwrap_or_default(),
                "wait_timeout": row.get::<_, Option<i64>>(6).unwrap_or(None),
                "subagent_timeout": row.get::<_, Option<i64>>(7).unwrap_or(None),
                "hints": row.get::<_, String>(8).unwrap_or_default(),
                "pid": row.get::<_, Option<i64>>(9).unwrap_or(None),
                "created_at": row.get::<_, f64>(10).unwrap_or(0.0),
                "background": row.get::<_, i64>(11).unwrap_or(0),
                "agent_id": row.get::<_, String>(12).unwrap_or_default(),
                "launch_args": row.get::<_, String>(13).unwrap_or_default(),
                "origin_device_id": row.get::<_, String>(14).unwrap_or_default(),
                "background_log_file": row.get::<_, String>(15).unwrap_or_default(),
                "last_event_id": row.get::<_, i64>(16).unwrap_or(0),
            }))
        }) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete instance row from database
    pub fn delete_instance(&self, name: &str) -> Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM instances WHERE name = ?", params![name])?;
        Ok(rows > 0)
    }

    /// Atomically publish a stopped event and remove its live instance state.
    ///
    /// The instance delete is the ownership CAS. Cleanup and event insertion
    /// share its transaction, so an error restores the row for a later retry.
    pub fn finalize_instance_stop(
        &self,
        name: &str,
        created_at: f64,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        event_data: &serde_json::Value,
    ) -> Result<bool> {
        self.finalize_instance_stop_impl(name, created_at, session_id, agent_id, None, event_data)
    }

    /// Same as [`Self::finalize_instance_stop`], but the delete additionally
    /// requires the instance's current PID, PID namespace and status to
    /// still match the probed snapshot.
    ///
    /// A dead-process reaper reads a PID snapshot, probes it out-of-band, and
    /// only then decides to finalize — so identity (name/created_at/session/
    /// agent) alone isn't enough: the row could have been rebound or resumed
    /// with a *new* live PID under the same identity in between. The guard
    /// makes that race a no-op (`Ok(false)`) instead of deleting a live row.
    // Every parameter names a distinct part of the identity/liveness CAS;
    // bundling them into a struct would just move the same count elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_instance_stop_guarded(
        &self,
        name: &str,
        created_at: f64,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        expected_pid: Option<i64>,
        expected_pid_namespace: &str,
        expected_status: &str,
        event_data: &serde_json::Value,
    ) -> Result<bool> {
        self.finalize_instance_stop_impl(
            name,
            created_at,
            session_id,
            agent_id,
            Some((expected_pid, expected_pid_namespace, expected_status)),
            event_data,
        )
    }

    fn finalize_instance_stop_impl(
        &self,
        name: &str,
        created_at: f64,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        pid_status_guard: Option<(Option<i64>, &str, &str)>,
        event_data: &serde_json::Value,
    ) -> Result<bool> {
        let timestamp = chrono_now_iso();
        let data = serde_json::to_string(event_data)?;
        let mut event_id = None;

        let won = self.with_immediate_transaction(|tx| {
            let deleted = if let Some((expected_pid, expected_pid_namespace, expected_status)) =
                pid_status_guard
            {
                tx.execute(
                    "DELETE FROM instances
                     WHERE name = ? AND created_at = ?
                       AND session_id IS ? AND agent_id IS ?
                       AND pid IS ? AND COALESCE(pid_namespace, '') = ? AND status = ?",
                    params![
                        name,
                        created_at,
                        session_id,
                        agent_id,
                        expected_pid,
                        expected_pid_namespace,
                        expected_status
                    ],
                )?
            } else {
                tx.execute(
                    "DELETE FROM instances
                     WHERE name = ? AND created_at = ?
                       AND session_id IS ? AND agent_id IS ?",
                    params![name, created_at, session_id, agent_id],
                )?
            };
            if deleted == 0 {
                return Ok(false);
            }

            // Consume the claim only with the winning event/delete. A failed
            // event write rolls it back, and a later lifetime cannot lose its claim.
            tx.execute(
                "DELETE FROM kv WHERE key = ?",
                params![format!("stop_reason:{name}")],
            )?;

            // Defense-in-depth: `session_bindings.instance_name` already has
            // ON DELETE CASCADE from `instances`, so this is redundant here,
            // but kept for correctness independent of the FK and for any
            // path that doesn't go through the cascade.
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
            tx.execute(
                "DELETE FROM notify_endpoints WHERE instance = ?",
                params![name],
            )?;
            tx.execute(
                "DELETE FROM process_bindings WHERE instance_name = ?",
                params![name],
            )?;
            tx.execute(
                "DELETE FROM kv
                 WHERE key LIKE 'events_sub:%'
                   AND json_extract(value, '$.caller') = ?
                   AND COALESCE(json_extract(value, '$.delivery_only'), 0) != 1",
                params![name],
            )?;
            tx.execute(
                "INSERT INTO events (timestamp, type, instance, data)
                 VALUES (?, 'life', ?, ?)",
                params![timestamp, name, data],
            )?;
            event_id = Some(tx.last_insert_rowid());
            Ok(true)
        })?;

        // Subscription notifications are best-effort external effects. Run
        // them only after the stopped event and deletion are durable.
        if let Some(event_id) = event_id {
            subscriptions::process_logged_event(self, event_id, "life", name, event_data);
        }
        Ok(won)
    }

    /// Check whether `name`'s *current* identity is a subagent slot.
    ///
    /// Classification rules, in order:
    /// 1. If a live instance row exists, it defines the current identity:
    ///    non-empty `parent_name` → true (subagent); empty → false
    ///    (top-level, regardless of any historical subagent events).
    /// 2. Otherwise, consult the *most recent* `life.stopped` event: true
    ///    iff its snapshot has a non-empty `agent_id`.
    ///
    /// Older subagent history does not poison a name that has since been
    /// reused top-level (either live or via a more recent top-level stop).
    pub fn was_subagent_name(&self, name: &str) -> bool {
        if let Ok(Some(data)) = self.get_instance(name) {
            return data
                .get("parent_name")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty());
        }

        self.conn
            .query_row(
                "SELECT COALESCE(json_extract(data, '$.snapshot.agent_id'), '') != ''
                 FROM events
                 WHERE type = 'life'
                   AND instance = ?
                   AND json_extract(data, '$.action') = 'stopped'
                 ORDER BY id DESC LIMIT 1",
                params![name],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or(false)
    }

    /// Find the most recent stopped instance whose snapshot carries the given
    /// session_id. life.stopped events are the source of truth: they persist
    /// across the `session_bindings` cascade, so they're the right thing to
    /// consult when reclaiming hcom identity by UUID after stop/kill.
    pub fn find_stopped_instance_by_session_id(&self, session_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT instance FROM events
                 WHERE type = 'life'
                   AND json_extract(data, '$.action') = 'stopped'
                   AND json_extract(data, '$.snapshot.session_id') = ?
                 ORDER BY id DESC LIMIT 1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Convert a row from INSTANCE_COLUMNS SELECT to JSON.
    fn instance_row_to_json(row: &rusqlite::Row) -> rusqlite::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "name": row.get::<_, String>(0).unwrap_or_default(),
            "session_id": row.get::<_, Option<String>>(1).unwrap_or(None),
            "parent_session_id": row.get::<_, Option<String>>(2).unwrap_or(None),
            "parent_name": row.get::<_, Option<String>>(3).unwrap_or(None),
            "tag": row.get::<_, Option<String>>(4).unwrap_or(None),
            "last_event_id": row.get::<_, i64>(5).unwrap_or(0),
            "status": row.get::<_, String>(6).unwrap_or_default(),
            "status_time": row.get::<_, i64>(7).unwrap_or(0),
            "last_seen": row.get::<_, i64>(8).unwrap_or(0),
            "status_context": row.get::<_, String>(9).unwrap_or_default(),
            "status_detail": row.get::<_, String>(10).unwrap_or_default(),
            "last_stop": row.get::<_, i64>(11).unwrap_or(0),
            "directory": row.get::<_, Option<String>>(12).unwrap_or(None),
            "created_at": row.get::<_, f64>(13).unwrap_or(0.0),
            "transcript_path": row.get::<_, String>(14).unwrap_or_default(),
            "tcp_mode": row.get::<_, i64>(15).unwrap_or(0),
            "wait_timeout": row.get::<_, i64>(16).unwrap_or(86400),
            "background": row.get::<_, i64>(17).unwrap_or(0),
            "background_log_file": row.get::<_, String>(18).unwrap_or_default(),
            "name_announced": row.get::<_, i64>(19).unwrap_or(0),
            "agent_id": row.get::<_, Option<String>>(20).unwrap_or(None),
            "origin_device_id": row.get::<_, String>(21).unwrap_or_default(),
            "hints": row.get::<_, String>(22).unwrap_or_default(),
            "subagent_timeout": row.get::<_, Option<i64>>(23).unwrap_or(None),
            "tool": row.get::<_, String>(24).unwrap_or_default(),
            "launch_args": row.get::<_, String>(25).unwrap_or_default(),
            "terminal_preset_requested": row.get::<_, String>(26).unwrap_or_default(),
            "terminal_preset_effective": row.get::<_, String>(27).unwrap_or_default(),
            "idle_since": row.get::<_, String>(28).unwrap_or_default(),
            "pid": row.get::<_, Option<i64>>(29).unwrap_or(None),
            "launch_context": row.get::<_, String>(30).unwrap_or_default(),
        }))
    }

    pub fn get_instance(&self, name: &str) -> Result<Option<serde_json::Value>> {
        let sql = format!("SELECT {} FROM instances WHERE name = ?", INSTANCE_COLUMNS);
        let mut stmt = self.conn.prepare_cached(&sql)?;

        match stmt.query_row(params![name], Self::instance_row_to_json) {
            Ok(inst) => Ok(Some(inst)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Look up instance name by agent_id (Claude Code sends short IDs like 'a6d9caf').
    pub fn get_instance_by_agent_id(&self, agent_id: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT name FROM instances WHERE agent_id = ?",
            params![agent_id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(name) => Ok(Some(name)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Save (upsert) instance from a map of field names to values.
    pub fn save_instance(
        &self,
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool> {
        if data.is_empty() {
            return Ok(false);
        }

        let columns: Vec<&str> = data.keys().map(|k| k.as_str()).collect();
        for col in &columns {
            Self::validate_column(col)?;
        }
        let placeholders = vec!["?"; columns.len()].join(", ");
        let update_clause: String = columns
            .iter()
            .filter(|&&k| k != "name")
            .map(|k| format!("{} = excluded.{}", k, k))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "INSERT INTO instances ({}) VALUES ({}) ON CONFLICT(name) DO UPDATE SET {}",
            columns.join(", "),
            placeholders,
            update_clause
        );

        let values: Vec<Box<dyn rusqlite::types::ToSql>> = columns
            .iter()
            .map(|&col| -> Box<dyn rusqlite::types::ToSql> {
                let val = &data[col];
                match val {
                    serde_json::Value::String(s) => Box::new(s.clone()),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            Box::new(i)
                        } else if let Some(f) = n.as_f64() {
                            Box::new(f)
                        } else {
                            Box::new(val.to_string())
                        }
                    }
                    serde_json::Value::Bool(b) => Box::new(*b as i32),
                    serde_json::Value::Null => Box::new(rusqlite::types::Null),
                    _ => Box::new(val.to_string()),
                }
            })
            .collect();

        let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        self.conn.execute(&sql, refs.as_slice())?;
        Ok(true)
    }

    /// Update specific instance fields.
    pub fn update_instance(
        &self,
        name: &str,
        updates: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool> {
        if updates.is_empty() {
            return Ok(true);
        }

        let entries: Vec<(&String, &serde_json::Value)> = updates.iter().collect();
        for (k, _) in &entries {
            Self::validate_column(k)?;
        }

        let set_clause: String = entries
            .iter()
            .map(|(k, _)| format!("{} = ?", k))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!("UPDATE instances SET {} WHERE name = ?", set_clause);

        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = entries
            .iter()
            .map(|(_, val)| -> Box<dyn rusqlite::types::ToSql> {
                match val {
                    serde_json::Value::String(s) => Box::new(s.clone()),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            Box::new(i)
                        } else if let Some(f) = n.as_f64() {
                            Box::new(f)
                        } else {
                            Box::new(val.to_string())
                        }
                    }
                    serde_json::Value::Bool(b) => Box::new(*b as i32),
                    serde_json::Value::Null => Box::new(rusqlite::types::Null),
                    _ => Box::new(val.to_string()),
                }
            })
            .collect();
        values.push(Box::new(name.to_string()));

        let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        self.conn.execute(&sql, refs.as_slice())?;
        Ok(true)
    }

    /// Iterate all instances, returning Vec of JSON objects.
    pub fn iter_instances(&self) -> Result<Vec<serde_json::Value>> {
        let sql = format!(
            "SELECT {} FROM instances ORDER BY created_at DESC",
            INSTANCE_COLUMNS
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;

        let rows: Vec<serde_json::Value> = stmt
            .query_map([], Self::instance_row_to_json)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    /// Get full instance row by name.
    pub fn get_instance_full(&self, name: &str) -> Result<Option<InstanceRow>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM instances WHERE name = ?")?;
        match stmt.query_row(params![name], InstanceRow::from_row) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get all instance rows.
    pub fn iter_instances_full(&self) -> Result<Vec<InstanceRow>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM instances ORDER BY created_at DESC")?;
        let rows = stmt
            .query_map([], InstanceRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Save (INSERT OR REPLACE) an instance row.
    /// Uses a JSON Value map for flexible field specification.
    pub fn save_instance_named(
        &self,
        name: &str,
        data: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<bool> {
        let patched = Self::with_pid_namespace(data);
        let data = patched.as_ref().unwrap_or(data);

        // Build column list and values dynamically
        let mut cols = vec!["name"];
        let mut placeholders = vec!["?"];
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(name.to_string())];

        for (key, val) in data {
            if key == "name" {
                continue;
            }
            cols.push(Self::validate_column(key)?);
            placeholders.push("?");
            values.push(Self::json_value_to_sql(val));
        }

        let sql = format!(
            "INSERT OR REPLACE INTO instances ({}) VALUES ({})",
            cols.join(", "),
            placeholders.join(", ")
        );

        let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|b| b.as_ref()).collect();
        let rows = self.conn.execute(&sql, refs.as_slice())?;
        Ok(rows > 0)
    }

    /// Insert a reservation row for a generated instance name.
    /// Unlike save_instance_named, this never replaces an existing row.
    pub fn save_instance_reservation(
        &self,
        name: &str,
        data: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<bool> {
        let mut cols = vec!["name"];
        let mut placeholders = vec!["?"];
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(name.to_string())];

        for (key, val) in data {
            if key == "name" {
                continue;
            }
            cols.push(Self::validate_column(key)?);
            placeholders.push("?");
            values.push(Self::json_value_to_sql(val));
        }

        let sql = format!(
            "INSERT INTO instances ({}) VALUES ({})",
            cols.join(", "),
            placeholders.join(", ")
        );

        let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|b| b.as_ref()).collect();
        let rows = self.conn.execute(&sql, refs.as_slice())?;
        Ok(rows > 0)
    }

    /// Update specific fields on an instance row.
    /// Uses a JSON Value map for flexible field specification.
    pub fn update_instance_fields(
        &self,
        name: &str,
        updates: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let patched = Self::with_pid_namespace(updates);
        let updates = patched.as_ref().unwrap_or(updates);

        let mut set_parts = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        for (key, val) in updates {
            set_parts.push(format!("{} = ?", Self::validate_column(key)?));
            values.push(Self::json_value_to_sql(val));
        }

        values.push(Box::new(name.to_string()));

        let sql = format!(
            "UPDATE instances SET {} WHERE name = ?",
            set_parts.join(", ")
        );

        let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|b| b.as_ref()).collect();
        self.conn.execute(&sql, refs.as_slice())?;
        Ok(())
    }

    /// A PID is only interpretable together with the PID namespace it was
    /// observed in, so every write of `pid` carries its namespace and no later
    /// writer can leave the two out of sync. Returns a patched copy, or `None`
    /// when there is nothing to add: either the write does not touch `pid`, or
    /// the caller already supplied a `pid_namespace` because it is copying a
    /// PID some *other* process observed (see the placeholder migration in
    /// `crate::instance_binding`).
    ///
    /// Clearing `pid` clears the namespace with it.
    fn with_pid_namespace(
        data: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let pid = data.get("pid")?;
        if data.contains_key("pid_namespace") {
            return None;
        }
        let namespace = if pid.is_null() {
            ""
        } else {
            crate::sys::process::current_pid_namespace().unwrap_or_default()
        };
        let mut patched = data.clone();
        patched.insert("pid_namespace".into(), serde_json::json!(namespace));
        Some(patched)
    }

    /// Validate column name against SQL injection (whitelist of known columns).
    fn validate_column(key: &str) -> Result<&str> {
        const VALID_COLUMNS: &[&str] = &[
            "name",
            "session_id",
            "parent_session_id",
            "parent_name",
            "agent_id",
            "tag",
            "last_event_id",
            "last_stop",
            "status",
            "status_time",
            "last_seen",
            "status_context",
            "status_detail",
            "directory",
            "created_at",
            "transcript_path",
            "tool",
            "background",
            "background_log_file",
            "tcp_mode",
            "wait_timeout",
            "subagent_timeout",
            "hints",
            "origin_device_id",
            "pid",
            "pid_namespace",
            "launch_args",
            "launch_context",
            "name_announced",
            "idle_since",
            "terminal_preset_requested",
            "terminal_preset_effective",
        ];
        if VALID_COLUMNS.contains(&key) {
            Ok(key)
        } else {
            Err(anyhow::anyhow!("Invalid column name: {}", key))
        }
    }

    /// Convert a serde_json::Value to a boxed ToSql for dynamic SQL binding.
    fn json_value_to_sql(val: &serde_json::Value) -> Box<dyn rusqlite::types::ToSql> {
        match val {
            serde_json::Value::Null => Box::new(rusqlite::types::Null),
            serde_json::Value::Bool(b) => Box::new(if *b { 1i64 } else { 0i64 }),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Box::new(i)
                } else if let Some(f) = n.as_f64() {
                    Box::new(f)
                } else {
                    Box::new(rusqlite::types::Null)
                }
            }
            serde_json::Value::String(s) => Box::new(s.clone()),
            _ => Box::new(val.to_string()),
        }
    }

    /// Check if instance is idle (safe for PTY injection).
    /// Returns true only when status is "listening" AND detail is not "cmd:listen".
    /// The "cmd:listen" detail is set by `hcom listen` as its first operation,
    /// ensuring the gate blocks before any async setup (endpoint registration, etc.).
    pub fn is_idle(&self, name: &str) -> bool {
        match self.get_instance_status(name) {
            Ok(Some(s)) => s.status == ST_LISTENING && s.detail != "cmd:listen",
            _ => false,
        }
    }

    /// Update heartbeat timestamp and re-assert tcp_mode to prove instance is alive.
    ///
    /// Sets both last_stop (heartbeat) and tcp_mode=true atomically.
    /// Re-asserting tcp_mode on every heartbeat self-heals after DB resets,
    /// instance re-creation, or any state loss — the delivery thread is the
    /// source of truth for whether TCP delivery is active.
    pub fn update_heartbeat(&self, name: &str) -> Result<()> {
        let now = now_epoch_i64();

        self.conn.execute(
            "UPDATE instances SET last_stop = ?, tcp_mode = 1 WHERE name = ?",
            params![now, name],
        )?;
        Ok(())
    }

    /// Update instance position with tcp_mode flag
    pub fn update_tcp_mode(&self, name: &str, tcp_mode: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE instances SET tcp_mode = ? WHERE name = ?",
            params![tcp_mode as i32, name],
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "instances_tests.rs"]
mod tests;
