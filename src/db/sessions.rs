//! Session and process binding helpers.

use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, Transaction, params};

use super::HcomDb;
use crate::shared::time::now_epoch_f64;

const CLAUDE_LINEAGE_VALIDATION_PREFIX: &str = "claude_lineage_validated:";

fn claude_lineage_validation_key(session_id: &str) -> String {
    format!("{CLAUDE_LINEAGE_VALIDATION_PREFIX}{session_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeProcessBindingClassification {
    Missing,
    SelectedOwner,
    ForeignLive,
    ForeignStale,
}

fn classify_claude_process_binding(
    txn: &Transaction<'_>,
    process_id: &str,
    selected_owner: &str,
) -> Result<ClaudeProcessBindingClassification> {
    if process_id.is_empty() {
        return Ok(ClaudeProcessBindingClassification::Missing);
    }
    let binding = txn
        .query_row(
            "SELECT session_id, instance_name FROM process_bindings WHERE process_id = ?",
            params![process_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((bound_session_id, bound_instance)) = binding else {
        return Ok(ClaudeProcessBindingClassification::Missing);
    };
    if bound_instance == selected_owner {
        return Ok(ClaudeProcessBindingClassification::SelectedOwner);
    }

    let stale = match bound_session_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(bound_session_id) => {
            let bound_primary = txn
                .query_row(
                    "SELECT session_id FROM instances WHERE name = ?",
                    params![&bound_instance],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            let bound_alias_owner = txn
                .query_row(
                    "SELECT instance_name FROM session_bindings WHERE session_id = ?",
                    params![bound_session_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            bound_primary.as_deref() != Some(bound_session_id)
                || bound_alias_owner.as_deref() != Some(bound_instance.as_str())
        }
        None => true,
    };
    Ok(if stale {
        ClaudeProcessBindingClassification::ForeignStale
    } else {
        ClaudeProcessBindingClassification::ForeignLive
    })
}

fn claude_children_for_session(txn: &Transaction<'_>, session_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        txn.prepare_cached("SELECT name FROM instances WHERE parent_session_id = ? ORDER BY name")?;
    let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
static TEST_MIGRATE_NOTIFY_FAIL: AtomicBool = AtomicBool::new(false);

impl HcomDb {
    /// Delete process binding (for cleanup)
    pub fn delete_process_binding(&self, process_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM process_bindings WHERE process_id = ?",
            params![process_id],
        )?;
        Ok(())
    }

    /// Get process binding to check for name changes
    ///
    /// Returns:
    /// - Ok(Some(instance_name)) if binding exists
    /// - Ok(None) if binding not found
    /// - Err if database error occurs
    pub fn get_process_binding(&self, process_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT instance_name FROM process_bindings WHERE process_id = ?")?;

        match stmt.query_row(params![process_id], |row| row.get::<_, String>(0)) {
            Ok(name) => Ok(Some(name)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get process binding with session_id. Returns (session_id, instance_name).
    pub fn get_process_binding_full(
        &self,
        process_id: &str,
    ) -> Result<Option<(Option<String>, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, instance_name FROM process_bindings WHERE process_id = ?",
        )?;

        match stmt.query_row(params![process_id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Return all instance names whose current transcript path exactly matches.
    ///
    /// Duplicate matches are returned deliberately so identity resolution can
    /// reject them as ambiguous instead of silently selecting one row.
    pub fn get_instances_by_transcript_path(&self, transcript_path: &str) -> Result<Vec<String>> {
        if transcript_path.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT name FROM instances WHERE transcript_path = ?")?;
        let rows = stmt.query_map(params![transcript_path], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Migrate notify endpoints from old instance to new instance.
    ///
    /// Per-kind merge with **source-wins on conflict**: `old_name` is the freshly
    /// launched/live process (placeholder or rebinding pid), so its `pty`/`inject`
    /// ports are authoritative and overwrite any stale ports on `new_name` left by a
    /// crashed or incompletely-cleaned prior process. Kinds only present on `new_name`
    /// (e.g. the canonical `plugin` registered during opencode-start) are preserved.
    pub fn migrate_notify_endpoints(&self, old_name: &str, new_name: &str) -> Result<()> {
        if old_name == new_name {
            return Ok(());
        }

        #[cfg(test)]
        if TEST_MIGRATE_NOTIFY_FAIL.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("test_injected_migrate_notify_fail"));
        }

        // A savepoint provides the same all-or-nothing merge while allowing
        // callers to include endpoint migration in a larger identity-binding
        // transaction.
        self.conn
            .execute_batch("SAVEPOINT hcom_migrate_notify_endpoints")?;
        let merge = (|| -> Result<()> {
            // Drop target rows for kinds the source will bring (source wins), keeping
            // target-only kinds like `plugin`.
            self.conn.execute(
                "DELETE FROM notify_endpoints
             WHERE instance = ?2
               AND kind IN (SELECT kind FROM notify_endpoints WHERE instance = ?1)",
                params![old_name, new_name],
            )?;
            // Move the source's (now conflict-free) rows onto the target.
            self.conn.execute(
                "UPDATE notify_endpoints SET instance = ?2 WHERE instance = ?1",
                params![old_name, new_name],
            )?;
            Ok(())
        })();

        if let Err(error) = merge {
            let _ = self
                .conn
                .execute_batch("ROLLBACK TO hcom_migrate_notify_endpoints");
            let _ = self
                .conn
                .execute_batch("RELEASE hcom_migrate_notify_endpoints");
            return Err(error);
        }
        self.conn
            .execute_batch("RELEASE hcom_migrate_notify_endpoints")?;

        Ok(())
    }

    /// Get last_event_id for an instance (cursor position for message delivery).
    ///
    /// Returns 0 if instance not found or on error.
    pub fn get_cursor(&self, name: &str) -> i64 {
        match self.get_instance_status(name) {
            Ok(Some(status)) => status.last_event_id,
            Ok(None) => 0, // No instance found
            Err(e) => {
                crate::log::log_error("db", "get_cursor.get_instance_status", &format!("{e}"));
                0
            }
        }
    }

    /// Check if instance has a session binding (session_id is set and non-empty).
    /// Used by OpenCode delivery thread to skip PTY injection when plugin is active.
    pub fn has_session(&self, name: &str) -> bool {
        match self.conn.query_row(
            "SELECT session_id FROM instances WHERE name = ?",
            params![name],
            |row| row.get::<_, String>(0),
        ) {
            Ok(sid) => !sid.is_empty(),
            _ => false,
        }
    }

    /// Check if there are pending (unread) messages for an instance.
    ///
    /// Lightweight check — parses only the JSON `data` column (skipping full
    /// Message construction) and returns on the first matching row.
    pub fn has_pending(&self, name: &str) -> bool {
        let last_event_id = match self.get_instance_status(name) {
            Ok(Some(status)) => status.last_event_id,
            // No instance row (e.g. a launch placeholder deleted after restore_stopped
            // rebinds to the canonical name) ⇒ no recipient ⇒ nothing pending. Falling
            // back to cursor 0 here would treat the entire channel backlog as unread and
            // replay a stale broadcast into the resumed session.
            Ok(None) => return false,
            Err(e) => {
                crate::log::log_error("db", "has_pending.get_instance_status", &format!("{e}"));
                return false;
            }
        };

        let mut stmt = match self
            .conn
            .prepare_cached("SELECT data FROM events WHERE id > ? AND type = 'message'")
        {
            Ok(s) => s,
            Err(e) => {
                crate::log::log_error("db", "has_pending.prepare", &format!("{e}"));
                return false;
            }
        };

        let rows = match stmt.query_map(params![last_event_id], |row| row.get::<_, String>(0)) {
            Ok(r) => r,
            Err(e) => {
                crate::log::log_error("db", "has_pending.query", &format!("{e}"));
                return false;
            }
        };

        for data in rows.flatten() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                && Self::should_deliver_to(&json, name)
            {
                return true;
            }
        }
        false
    }

    /// Diagnostic-only: (min_id, max_id, count) of pending message events for
    /// an instance, or None if nothing is pending. Not used on the delivery
    /// hot path — for logging at `delivery.gate_pass`.
    pub fn pending_event_range(&self, name: &str) -> Option<(i64, i64, i64)> {
        let last_event_id = self.get_cursor(name);

        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT id, data FROM events WHERE id > ? AND type = 'message' ORDER BY id",
            )
            .ok()?;
        let rows = stmt
            .query_map(params![last_event_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .ok()?;

        let mut min_id = i64::MAX;
        let mut max_id = i64::MIN;
        let mut count = 0i64;
        for (id, data) in rows.flatten() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                && Self::should_deliver_to(&json, name)
            {
                min_id = min_id.min(id);
                max_id = max_id.max(id);
                count += 1;
            }
        }
        (count > 0).then_some((min_id, max_id, count))
    }

    /// Get instance name bound to session_id, or None if not bound.
    pub fn get_session_binding(&self, session_id: &str) -> Result<Option<String>> {
        if session_id.is_empty() {
            return Ok(None);
        }
        match self.conn.query_row(
            "SELECT instance_name FROM session_bindings WHERE session_id = ?",
            params![session_id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(name) => Ok(Some(name)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Create or update session binding.
    /// Returns error if session_id is already bound to a different instance.
    pub fn set_session_binding(&self, session_id: &str, instance_name: &str) -> Result<()> {
        if session_id.is_empty() || instance_name.is_empty() {
            return Ok(());
        }

        // Check for existing binding to different instance
        if let Some(existing) = self.get_session_binding(session_id)?
            && existing != instance_name
        {
            bail!(
                "Session {}... already bound to {}, cannot bind to {}",
                &session_id[..session_id.len().min(8)],
                existing,
                instance_name
            );
        }

        let now = now_epoch_f64();

        self.conn.execute(
            "INSERT INTO session_bindings (session_id, instance_name, created_at)
             VALUES (?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
                 instance_name = excluded.instance_name,
                 created_at = excluded.created_at",
            params![session_id, instance_name, now],
        )?;
        Ok(())
    }

    /// Clear session_id from any instance except exclude_instance.
    pub fn clear_session_id_from_other_instances(
        &self,
        session_id: &str,
        exclude_instance: &str,
    ) -> Result<()> {
        if session_id.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "UPDATE instances SET session_id = NULL WHERE session_id = ? AND name != ?",
            params![session_id, exclude_instance],
        )?;
        Ok(())
    }

    /// Explicitly rebind session to a different instance.
    pub fn rebind_session(&self, session_id: &str, new_instance_name: &str) -> Result<()> {
        if session_id.is_empty() || new_instance_name.is_empty() {
            return Ok(());
        }
        self.clear_session_id_from_other_instances(session_id, new_instance_name)?;
        self.upsert_session_binding(session_id, new_instance_name)
    }

    /// Internal helper: unconditional upsert of session binding.
    fn upsert_session_binding(&self, session_id: &str, instance_name: &str) -> Result<()> {
        let now = now_epoch_f64();
        self.conn.execute(
            "INSERT INTO session_bindings (session_id, instance_name, created_at)
             VALUES (?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
                 instance_name = excluded.instance_name,
                 created_at = excluded.created_at",
            params![session_id, instance_name, now],
        )?;
        Ok(())
    }

    /// Delete session binding.
    pub fn delete_session_binding(&self, session_id: &str) -> Result<()> {
        if session_id.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "DELETE FROM session_bindings WHERE session_id = ?",
            params![session_id],
        )?;
        Ok(())
    }

    /// Delete all session bindings for an instance.
    pub fn delete_session_bindings_for_instance(&self, instance_name: &str) -> Result<()> {
        if instance_name.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "DELETE FROM session_bindings WHERE instance_name = ?",
            params![instance_name],
        )?;
        Ok(())
    }

    /// Atomically rebind instance to new session.
    pub fn rebind_instance_session(&self, instance_name: &str, session_id: &str) -> Result<()> {
        if instance_name.is_empty() || session_id.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "DELETE FROM session_bindings WHERE instance_name = ?",
            params![instance_name],
        )?;
        self.conn.execute(
            "UPDATE instances SET session_id = NULL WHERE session_id = ? AND name != ?",
            params![session_id, instance_name],
        )?;
        self.upsert_session_binding(session_id, instance_name)?;
        Ok(())
    }

    /// Check if instance has a session binding (hooks active).
    pub fn has_session_binding(&self, instance_name: &str) -> bool {
        if instance_name.is_empty() {
            return false;
        }
        self.conn
            .query_row(
                "SELECT 1 FROM session_bindings WHERE instance_name = ? LIMIT 1",
                params![instance_name],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Check if instance has a process binding (hcom-launched).
    pub fn has_process_binding_for_instance(&self, instance_name: &str) -> bool {
        if instance_name.is_empty() {
            return false;
        }
        self.conn
            .query_row(
                "SELECT 1 FROM process_bindings WHERE instance_name = ? LIMIT 1",
                params![instance_name],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Set process binding (map process_id -> instance/session).
    /// Set process binding. Empty session_id is stored as NULL.
    pub fn set_process_binding(
        &self,
        process_id: &str,
        session_id: &str,
        instance_name: &str,
    ) -> Result<()> {
        let now = now_epoch_f64();
        // Normalize empty string to NULL
        let sid: Option<&str> = if session_id.is_empty() {
            None
        } else {
            Some(session_id)
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO process_bindings (process_id, session_id, instance_name, updated_at)
             VALUES (?, ?, ?, ?)",
            params![process_id, sid, instance_name, now],
        )?;
        Ok(())
    }

    /// Return the owner cached after Claude transcript lineage was validated.
    ///
    /// The cache is only trusted while it agrees with the live session binding;
    /// a later rebind automatically invalidates a stale cache entry.
    pub fn get_validated_claude_session_owner(&self, session_id: &str) -> Result<Option<String>> {
        if session_id.is_empty() {
            return Ok(None);
        }

        let Some(owner) = self.kv_get(&claude_lineage_validation_key(session_id))? else {
            return Ok(None);
        };
        if self.get_session_binding(session_id)?.as_deref() == Some(owner.as_str()) {
            Ok(Some(owner))
        } else {
            Ok(None)
        }
    }

    /// Mark a Claude session binding as validated by a trusted SessionStart or
    /// bounded structured transcript lineage.
    pub fn mark_claude_session_validated(
        &self,
        session_id: &str,
        instance_name: &str,
    ) -> Result<()> {
        if session_id.is_empty() || instance_name.is_empty() {
            return Ok(());
        }
        if self.get_session_binding(session_id)?.as_deref() != Some(instance_name) {
            bail!(
                "cannot validate Claude session {} for non-owner {}",
                session_id,
                instance_name
            );
        }
        self.kv_set(
            &claude_lineage_validation_key(session_id),
            Some(instance_name),
        )
    }

    /// Remove cached Claude lineage validation for a session generation.
    pub fn clear_claude_session_validation(&self, session_id: &str) -> Result<()> {
        if session_id.is_empty() {
            return Ok(());
        }
        self.kv_set(&claude_lineage_validation_key(session_id), None)
    }

    /// Attach and promote a Claude session generation without deleting older
    /// aliases. Identity writes and validation-cache updates are atomic.
    ///
    /// Only the process row that delivered this SessionStart may be refreshed.
    /// A live foreign process row is preserved; a genuinely stale conflicting
    /// row is deleted. Returns true when that stale row was removed.
    pub fn attach_claude_generation(
        &self,
        instance_name: &str,
        session_id: &str,
        transcript_path: &str,
        process_id: &str,
        _process_owner: Option<&str>,
    ) -> Result<bool> {
        if instance_name.is_empty() || session_id.is_empty() {
            return Ok(false);
        }

        let now = now_epoch_f64();
        let validation_key = claude_lineage_validation_key(session_id);
        self.with_immediate_transaction(|txn| {
            let old_primary_session = txn
                .query_row(
                    "SELECT session_id FROM instances WHERE name = ?",
                    params![instance_name],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    anyhow::anyhow!("Claude lineage owner {} does not exist", instance_name)
                })?
                .filter(|old_session_id| old_session_id != session_id);

            // parent_session_id has an immediate FK to instances.session_id.
            // Detach known children before changing the root key, then restore
            // them to the promoted generation inside the same transaction.
            let children = match old_primary_session.as_deref() {
                Some(old_session_id) => claude_children_for_session(txn, old_session_id)?,
                None => Vec::new(),
            };
            for child in &children {
                txn.execute(
                    "UPDATE instances SET parent_session_id = NULL
                     WHERE name = ? AND parent_session_id = ?",
                    params![child, old_primary_session.as_deref()],
                )?;
            }

            // The incoming generation may currently be (incorrectly) primary
            // on another root. Its children reference that primary key through
            // an immediate FK, so detach them before clearing the displaced
            // root. They remain addressable by name/agent id but no longer
            // claim ancestry under the repaired session generation.
            let displaced_owner = txn
                .query_row(
                    "SELECT name FROM instances WHERE session_id = ? AND name != ? LIMIT 1",
                    params![session_id, instance_name],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if displaced_owner.is_some() {
                txn.execute(
                    "UPDATE instances SET parent_session_id = NULL
                     WHERE parent_session_id = ?",
                    params![session_id],
                )?;
            }

            txn.execute(
                "UPDATE instances SET session_id = NULL WHERE session_id = ? AND name != ?",
                params![session_id, instance_name],
            )?;
            txn.execute(
                "INSERT INTO session_bindings (session_id, instance_name, created_at)
                 VALUES (?, ?, ?)
                 ON CONFLICT(session_id) DO UPDATE SET
                     instance_name = excluded.instance_name,
                     created_at = excluded.created_at",
                params![session_id, instance_name, now],
            )?;
            txn.execute(
                "UPDATE instances
                 SET session_id = ?,
                     transcript_path = CASE WHEN ? = '' THEN transcript_path ELSE ? END
                 WHERE name = ?",
                params![session_id, transcript_path, transcript_path, instance_name],
            )?;
            for child in &children {
                txn.execute(
                    "UPDATE instances SET parent_session_id = ?
                     WHERE name = ? AND parent_session_id IS NULL",
                    params![session_id, child],
                )?;
            }
            txn.execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)",
                params![validation_key, instance_name],
            )?;

            let classification = classify_claude_process_binding(txn, process_id, instance_name)?;
            match classification {
                ClaudeProcessBindingClassification::SelectedOwner => {
                    txn.execute(
                        "UPDATE process_bindings
                         SET session_id = ?, updated_at = ?
                         WHERE process_id = ? AND instance_name = ?",
                        params![session_id, now, process_id, instance_name],
                    )?;
                }
                ClaudeProcessBindingClassification::ForeignStale => {
                    txn.execute(
                        "DELETE FROM process_bindings WHERE process_id = ?",
                        params![process_id],
                    )?;
                }
                ClaudeProcessBindingClassification::Missing
                | ClaudeProcessBindingClassification::ForeignLive => {}
            }

            Ok(matches!(
                classification,
                ClaudeProcessBindingClassification::ForeignStale
            ))
        })
    }

    /// Delete all process bindings for an instance.
    pub fn delete_process_bindings_for_instance(&self, instance_name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM process_bindings WHERE instance_name = ?",
            params![instance_name],
        )?;
        Ok(())
    }
}

#[cfg(test)]
impl HcomDb {
    pub fn set_test_migrate_notify_fail(fail: bool) {
        TEST_MIGRATE_NOTIFY_FAIL.store(fail, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
