//! Track hcom-launched process PIDs for orphan detection and recovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::instance_lifecycle as lifecycle;

const PIDFILE_NAME: &str = ".tmp/launched_pids.json";

/// Tracked process entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidEntry {
    pub tool: String,
    pub names: Vec<String>,
    pub launched_at: f64,
    #[serde(default)]
    pub directory: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub process_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal_preset: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kitty_listen_on: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub zellij_session_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub notify_port: u16,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub inject_port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// PID namespace `pid` was observed in (see
    /// [`crate::sys::process::current_pid_namespace`]). Empty for entries
    /// written before this field existed, or on platforms without PID
    /// namespaces; such entries read as "liveness unknown" and are never pruned.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pid_namespace: String,
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

/// Orphan process info (enriched with PID).
#[derive(Debug, Clone, Default)]
pub struct OrphanProcess {
    pub pid: u32,
    pub tool: String,
    pub names: Vec<String>,
    pub directory: String,
    pub process_id: String,
    pub terminal_preset: String,
    pub pane_id: String,
    pub terminal_id: String,
    pub kitty_listen_on: String,
    pub zellij_session_name: String,
    pub session_id: String,
    pub notify_port: u16,
    pub inject_port: u16,
    pub tag: String,
    /// See [`PidEntry::pid_namespace`].
    pub pid_namespace: String,
}

impl From<(u32, &PidEntry)> for OrphanProcess {
    fn from((pid, entry): (u32, &PidEntry)) -> Self {
        Self {
            pid,
            tool: entry.tool.clone(),
            names: entry.names.clone(),
            directory: entry.directory.clone(),
            process_id: entry.process_id.clone(),
            terminal_preset: entry.terminal_preset.clone(),
            pane_id: entry.pane_id.clone(),
            terminal_id: entry.terminal_id.clone(),
            kitty_listen_on: entry.kitty_listen_on.clone(),
            zellij_session_name: entry.zellij_session_name.clone(),
            session_id: entry.session_id.clone(),
            notify_port: entry.notify_port,
            inject_port: entry.inject_port,
            tag: entry.tag.clone(),
            pid_namespace: entry.pid_namespace.clone(),
        }
    }
}

/// Resolve the pidfile path from hcom_dir.
fn pidfile_path(hcom_dir: &Path) -> PathBuf {
    hcom_dir.join(PIDFILE_NAME)
}

/// Check if a process is alive. See [`crate::sys::process::is_alive`].
pub fn is_alive(pid: u32) -> bool {
    crate::sys::process::is_alive(pid)
}

/// The namespace to stamp on a PID this process observed itself.
fn current_namespace() -> String {
    crate::sys::process::current_pid_namespace()
        .unwrap_or_default()
        .to_string()
}

/// Whether this entry's PID is one we can still see. `None` for a PID recorded
/// in a namespace we cannot inspect — never evidence for removing the entry.
fn entry_liveness(pid: u32, entry: &PidEntry) -> Option<bool> {
    crate::sys::process::is_alive_in(pid, Some(entry.pid_namespace.as_str()))
}

/// Read raw pidfile data.
fn read_raw(hcom_dir: &Path) -> HashMap<String, PidEntry> {
    match std::fs::read_to_string(pidfile_path(hcom_dir)) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write pidfile data atomically (temp + rename).
fn write_raw(hcom_dir: &Path, data: &HashMap<String, PidEntry>) {
    if let Ok(content) = serde_json::to_string(data) {
        crate::paths::atomic_write(&pidfile_path(hcom_dir), &content);
    }
}

/// Parameters for recording a launched process.
#[derive(Debug)]
pub struct PidRecord<'a> {
    pub hcom_dir: &'a Path,
    pub pid: u32,
    pub tool: &'a str,
    pub name: &'a str,
    pub directory: &'a str,
    pub process_id: &'a str,
    pub terminal_preset: &'a str,
    pub pane_id: &'a str,
    pub terminal_id: &'a str,
    pub kitty_listen_on: &'a str,
    pub zellij_session_name: &'a str,
    pub session_id: &'a str,
    pub notify_port: u16,
    pub inject_port: u16,
    pub tag: &'a str,
    /// Where this PID was observed. `None` means the caller spawned or hosts
    /// the process itself, so its own namespace describes it. `Some(marker)`
    /// carries a namespace someone else recorded — including `Some("")`, which
    /// says "unknown" and must not be quietly upgraded to ours.
    pub pid_namespace: Option<&'a str>,
}

impl<'a> PidRecord<'a> {
    /// Create with required fields, defaulting optional ones.
    pub fn new(
        hcom_dir: &'a Path,
        pid: u32,
        tool: &'a str,
        name: &'a str,
        directory: &'a str,
    ) -> Self {
        Self {
            hcom_dir,
            pid,
            tool,
            name,
            directory,
            process_id: "",
            terminal_preset: "",
            pane_id: "",
            terminal_id: "",
            kitty_listen_on: "",
            zellij_session_name: "",
            session_id: "",
            notify_port: 0,
            inject_port: 0,
            tag: "",
            pid_namespace: None,
        }
    }
}

/// Record a launched process PID.
pub fn record_pid(rec: &PidRecord<'_>) {
    let PidRecord {
        hcom_dir,
        pid,
        tool,
        name,
        directory,
        process_id,
        terminal_preset,
        pane_id,
        terminal_id,
        kitty_listen_on,
        zellij_session_name,
        session_id,
        notify_port,
        inject_port,
        tag,
        pid_namespace,
    } = rec;
    // `None` = ours to stamp; `Some("")` = the source knows it does not know,
    // which stays unknown rather than being relabelled with our namespace.
    let source_namespace = match pid_namespace {
        None => Some(current_namespace()),
        Some(marker) if !marker.is_empty() => Some((*marker).to_string()),
        Some(_) => None,
    };
    let mut data = read_raw(hcom_dir);
    let key = pid.to_string();

    if let Some(entry) = data.get_mut(&key) {
        // Append name if not already present
        if !entry.names.contains(&name.to_string()) {
            entry.names.push(name.to_string());
        }
        // Fill in fields that are empty
        if !process_id.is_empty() && entry.process_id.is_empty() {
            entry.process_id = process_id.to_string();
        }
        if !terminal_preset.is_empty() && entry.terminal_preset.is_empty() {
            entry.terminal_preset = terminal_preset.to_string();
        }
        if !pane_id.is_empty() && entry.pane_id.is_empty() {
            entry.pane_id = pane_id.to_string();
        }
        if !terminal_id.is_empty() && entry.terminal_id.is_empty() {
            entry.terminal_id = terminal_id.to_string();
        }
        if !kitty_listen_on.is_empty() && entry.kitty_listen_on.is_empty() {
            entry.kitty_listen_on = kitty_listen_on.to_string();
        }
        if !zellij_session_name.is_empty() && entry.zellij_session_name.is_empty() {
            entry.zellij_session_name = zellij_session_name.to_string();
        }
        if !session_id.is_empty() && entry.session_id.is_empty() {
            entry.session_id = session_id.to_string();
        }
        if *notify_port != 0 && entry.notify_port == 0 {
            entry.notify_port = *notify_port;
        }
        if *inject_port != 0 && entry.inject_port == 0 {
            entry.inject_port = *inject_port;
        }
        if !tag.is_empty() && entry.tag.is_empty() {
            entry.tag = tag.to_string();
        }
        if entry.pid_namespace.is_empty()
            && let Some(ns) = source_namespace
        {
            entry.pid_namespace = ns;
        }
    } else {
        data.insert(
            key,
            PidEntry {
                tool: tool.to_string(),
                names: vec![name.to_string()],
                launched_at: crate::shared::time::now_epoch_f64(),
                directory: directory.to_string(),
                process_id: process_id.to_string(),
                terminal_preset: terminal_preset.to_string(),
                pane_id: pane_id.to_string(),
                terminal_id: terminal_id.to_string(),
                kitty_listen_on: kitty_listen_on.to_string(),
                zellij_session_name: zellij_session_name.to_string(),
                session_id: session_id.to_string(),
                notify_port: *notify_port,
                inject_port: *inject_port,
                tag: tag.to_string(),
                pid_namespace: source_namespace.unwrap_or_default(),
            },
        );
    }

    write_raw(hcom_dir, &data);
}

/// Get running hcom processes not accounted for by active instances.
///
/// Auto-prunes dead PIDs from the file. If `active_pids` is provided,
/// also prunes PIDs that are now active from the file and filters them
/// from the result. Foreign/unknown namespaces stay on disk but are never
/// returned: callers use this list to kill or adopt local processes.
pub fn get_orphan_processes(
    hcom_dir: &Path,
    active_pids: Option<&std::collections::HashSet<u32>>,
) -> Vec<OrphanProcess> {
    let mut data = read_raw(hcom_dir);
    let original_len = data.len();
    let mut result = Vec::new();
    data.retain(|pid_str, entry| {
        let Ok(pid) = pid_str.parse::<u32>() else {
            return false;
        };
        match entry_liveness(pid, entry) {
            None => true, // Preserve metadata without offering an unsafe PID.
            Some(false) => false,
            Some(true) => {
                if active_pids.is_some_and(|active| active.contains(&pid)) {
                    return false;
                }
                result.push(OrphanProcess::from((pid, &*entry)));
                true
            }
        }
    });
    if data.len() != original_len {
        write_raw(hcom_dir, &data);
    }

    result
}

/// Look up a tracked entry by PID regardless of liveness. Read-only — unlike
/// [`get_orphan_processes`], never prunes or writes. For an explicit,
/// user-targeted `hcom kill <pid>`: the caller supplied the exact PID, so an
/// unknown-liveness entry (no `pid_namespace` on record) is still a valid kill
/// target, just one the automatic orphan scan won't surface on its own.
pub fn get_tracked_entry(hcom_dir: &Path, pid: u32) -> Option<OrphanProcess> {
    let data = read_raw(hcom_dir);
    data.get(&pid.to_string())
        .map(|entry| OrphanProcess::from((pid, entry)))
}

/// Remove a PID from tracking (after kill).
pub fn remove_pid(hcom_dir: &Path, pid: u32) {
    let mut data = read_raw(hcom_dir);
    let key = pid.to_string();
    if data.remove(&key).is_some() {
        write_raw(hcom_dir, &data);
    }
}

/// Re-register a single orphan into the DB.
///
/// Creates instance row, sets PID/directory, creates process/session bindings,
/// and sets status to listening so the PTY delivery gate can inject messages.
/// Does NOT log events, print output, or remove from pidtrack — caller handles those.
///
/// Returns `Err` if the critical instance INSERT fails. Caller must leave the
/// pidtrack entry intact on failure so recovery can be retried later.
pub fn recover_single_orphan_to_db(
    db: &crate::db::HcomDb,
    orphan: &OrphanProcess,
    instance_name: &str,
) -> Result<(), String> {
    use crate::shared::constants::ST_LISTENING;

    let now = crate::shared::time::now_epoch_i64();

    // Create instance row — this is the critical step; fail = abort recovery
    db.conn()
        .execute(
            "INSERT OR IGNORE INTO instances (name, tool, status, status_context, created_at) VALUES (?1, ?2, 'inactive', 'new', ?3)",
            rusqlite::params![instance_name, orphan.tool, now],
        )
        .map_err(|e| format!("failed to insert instance '{}': {}", instance_name, e))?;

    // Update PID and directory
    // The orphan's pid was observed by whoever recorded it in the pidfile, not
    // by us. Copy that marker across rather than stamping our own namespace.
    let mut updates = serde_json::Map::new();
    updates.insert("pid".into(), serde_json::json!(orphan.pid));
    updates.insert(
        "pid_namespace".into(),
        serde_json::json!(orphan.pid_namespace),
    );
    if !orphan.directory.is_empty() {
        updates.insert("directory".into(), serde_json::json!(orphan.directory));
    }
    if !orphan.terminal_preset.is_empty() {
        updates.insert(
            "terminal_preset_effective".into(),
            serde_json::json!(orphan.terminal_preset),
        );
    }
    let mut launch_context = serde_json::Map::new();
    if !orphan.process_id.is_empty() {
        launch_context.insert("process_id".into(), serde_json::json!(orphan.process_id));
    }
    if !orphan.pane_id.is_empty() {
        launch_context.insert("pane_id".into(), serde_json::json!(orphan.pane_id));
    }
    if !orphan.terminal_id.is_empty() {
        launch_context.insert("terminal_id".into(), serde_json::json!(orphan.terminal_id));
    }
    if !orphan.kitty_listen_on.is_empty() {
        launch_context.insert(
            "kitty_listen_on".into(),
            serde_json::json!(orphan.kitty_listen_on),
        );
    }
    if !orphan.zellij_session_name.is_empty() {
        launch_context.insert(
            "env".into(),
            serde_json::json!({ "ZELLIJ_SESSION_NAME": orphan.zellij_session_name }),
        );
    }
    if !launch_context.is_empty() {
        updates.insert(
            "launch_context".into(),
            serde_json::json!(
                serde_json::to_string(&launch_context).unwrap_or_else(|_| "{}".to_string())
            ),
        );
    }
    db.update_instance_fields(instance_name, &updates)
        .map_err(|e| format!("failed to restore process fields: {e}"))?;

    // Create process binding
    if !orphan.process_id.is_empty() {
        let sid = if orphan.session_id.is_empty() {
            None
        } else {
            Some(orphan.session_id.as_str())
        };
        db.set_process_binding(&orphan.process_id, sid.unwrap_or(""), instance_name)
            .map_err(|e| format!("failed to set process binding: {}", e))?;
    }

    // Create session binding
    if !orphan.session_id.is_empty() {
        db.rebind_session(&orphan.session_id, instance_name)
            .map_err(|e| format!("failed to rebind session: {}", e))?;
        let mut sid_update = serde_json::Map::new();
        sid_update.insert("session_id".into(), serde_json::json!(orphan.session_id));
        db.update_instance_fields(instance_name, &sid_update)
            .map_err(|e| format!("failed to restore session fields: {e}"))?;
    }

    // Restore notify endpoints
    if orphan.notify_port != 0 {
        db.register_notify_port(instance_name, orphan.notify_port)
            .map_err(|e| format!("failed to register notify port: {}", e))?;
    }
    if orphan.inject_port != 0 {
        db.register_inject_port(instance_name, orphan.inject_port)
            .map_err(|e| format!("failed to register inject port: {}", e))?;
    }

    // Set listening so PTY delivery gate allows message injection
    lifecycle::try_set_status(
        db,
        instance_name,
        ST_LISTENING,
        "recovered",
        Default::default(),
    )
    .map_err(|e| format!("failed to restore listening status: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn make_temp_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tmp")).unwrap();
        dir
    }

    fn rec<'a>(dir: &'a Path, pid: u32, tool: &'a str, name: &'a str) -> PidRecord<'a> {
        PidRecord::new(dir, pid, tool, name, "/tmp")
    }

    #[test]
    fn test_record_and_read() {
        let dir = make_temp_dir();
        record_pid(&PidRecord {
            hcom_dir: dir.path(),
            pid: 12345,
            tool: "claude",
            name: "luna",
            directory: "/tmp",
            process_id: "pid-1",
            terminal_preset: "kitty",
            pane_id: "pane-1",
            terminal_id: "term-1",
            kitty_listen_on: "/tmp/kitty.sock",
            zellij_session_name: "wise-kangaroo",
            session_id: "sess-1",
            notify_port: 8080,
            inject_port: 8081,
            tag: "test-tag",
            pid_namespace: None,
        });

        let data = read_raw(dir.path());
        assert_eq!(data.len(), 1);
        let entry = data.get("12345").unwrap();
        assert_eq!(entry.tool, "claude");
        assert_eq!(entry.names, vec!["luna"]);
        assert_eq!(entry.process_id, "pid-1");
        assert_eq!(entry.terminal_preset, "kitty");
        assert_eq!(entry.pane_id, "pane-1");
        assert_eq!(entry.terminal_id, "term-1");
        assert_eq!(entry.kitty_listen_on, "/tmp/kitty.sock");
        assert_eq!(entry.zellij_session_name, "wise-kangaroo");
        assert_eq!(entry.session_id, "sess-1");
        assert_eq!(entry.notify_port, 8080);
        assert_eq!(entry.inject_port, 8081);
    }

    #[test]
    fn test_record_appends_name() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), 12345, "claude", "luna"));
        record_pid(&rec(dir.path(), 12345, "claude", "nova"));

        let data = read_raw(dir.path());
        let entry = data.get("12345").unwrap();
        assert_eq!(entry.names, vec!["luna", "nova"]);
    }

    #[test]
    fn test_record_fills_empty_fields() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), 12345, "claude", "luna"));
        record_pid(&PidRecord {
            process_id: "pid-1",
            terminal_preset: "kitty",
            zellij_session_name: "wise-kangaroo",
            notify_port: 8080,
            ..rec(dir.path(), 12345, "claude", "luna")
        });

        let data = read_raw(dir.path());
        let entry = data.get("12345").unwrap();
        assert_eq!(entry.process_id, "pid-1");
        assert_eq!(entry.terminal_preset, "kitty");
        assert_eq!(entry.zellij_session_name, "wise-kangaroo");
        assert_eq!(entry.notify_port, 8080);
    }

    #[test]
    fn test_record_does_not_overwrite_existing_fields() {
        let dir = make_temp_dir();
        record_pid(&PidRecord {
            process_id: "pid-1",
            terminal_preset: "kitty",
            ..rec(dir.path(), 12345, "claude", "luna")
        });
        record_pid(&PidRecord {
            process_id: "pid-2",
            terminal_preset: "wezterm",
            ..rec(dir.path(), 12345, "claude", "luna")
        });

        let data = read_raw(dir.path());
        let entry = data.get("12345").unwrap();
        assert_eq!(entry.process_id, "pid-1");
        assert_eq!(entry.terminal_preset, "kitty");
    }

    #[test]
    fn test_remove_pid() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), 12345, "claude", "luna"));
        record_pid(&rec(dir.path(), 67890, "gemini", "nova"));

        remove_pid(dir.path(), 12345);
        let data = read_raw(dir.path());
        assert_eq!(data.len(), 1);
        assert!(data.contains_key("67890"));
        assert!(!data.contains_key("12345"));
    }

    #[test]
    fn test_remove_nonexistent_pid() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), 12345, "claude", "luna"));
        remove_pid(dir.path(), 99999);
        let data = read_raw(dir.path());
        assert_eq!(data.len(), 1);
    }

    #[test]
    fn test_orphan_prunes_dead_pids() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), 99999999, "claude", "dead"));

        let orphans = get_orphan_processes(dir.path(), None);
        let data = read_raw(dir.path());
        assert!(!data.contains_key("99999999"));
        assert!(orphans.iter().all(|o| o.pid != 99999999));
    }

    // Linux/Android only - see the note on the matching test in commands/kill.rs.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn test_unknown_namespace_orphan_excluded_but_still_tracked_by_pid() {
        let dir = make_temp_dir();
        record_pid(&PidRecord {
            pid_namespace: Some(""), // explicit "I don't know" marker
            ..rec(dir.path(), 99999999, "claude", "dead")
        });

        // Unknown liveness: never offered as an automatic orphan...
        let orphans = get_orphan_processes(dir.path(), None);
        assert!(orphans.iter().all(|o| o.pid != 99999999));
        // ...but a caller targeting this exact PID can still find it, and it
        // was never pruned from disk (liveness genuinely unknown, not dead).
        let data = read_raw(dir.path());
        assert!(data.contains_key("99999999"));
        assert!(get_tracked_entry(dir.path(), 99999999).is_some());
        assert!(get_tracked_entry(dir.path(), 424242).is_none());
    }

    #[test]
    fn test_orphan_active_pids_pruned() {
        let dir = make_temp_dir();
        let our_pid = std::process::id();
        record_pid(&rec(dir.path(), our_pid, "claude", "luna"));

        let mut active = HashSet::new();
        active.insert(our_pid);

        let orphans = get_orphan_processes(dir.path(), Some(&active));
        // Our PID is active — should be filtered from results AND pruned from file
        assert!(orphans.is_empty());
        let data = read_raw(dir.path());
        assert!(!data.contains_key(&our_pid.to_string()));
    }

    /// Writes an entry the way another PID namespace would have: same shape,
    /// a marker this process cannot match, and a PID number that is dead here.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn record_foreign(dir: &Path, pid: u32, name: &str) {
        record_pid(&rec(dir, pid, "claude", name));
        let mut data = read_raw(dir);
        data.get_mut(&pid.to_string()).unwrap().pid_namespace = "pid:[foreign]".to_string();
        write_raw(dir, &data);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn orphan_scan_keeps_entries_from_a_foreign_namespace() {
        let dir = make_temp_dir();
        record_foreign(dir.path(), 99_999_998, "host_agent");
        record_pid(&rec(dir.path(), 99_999_999, "claude", "ours_and_dead"));

        let orphans = get_orphan_processes(dir.path(), None);

        let data = read_raw(dir.path());
        assert!(
            data.contains_key("99999998"),
            "a pid we cannot inspect is not evidence of death"
        );
        assert!(!data.contains_key("99999999"), "our own dead pid is pruned");
        assert!(
            orphans.is_empty(),
            "foreign PIDs must not reach kill/adopt callers"
        );
        assert_eq!(data["99999998"].pid_namespace, "pid:[foreign]");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn orphan_scan_does_not_offer_foreign_or_unknown_live_pid_collisions() {
        let dir = make_temp_dir();
        let pid = std::process::id();
        record_foreign(dir.path(), pid, "host_agent");
        for namespace in ["pid:[foreign]", ""] {
            let mut data = read_raw(dir.path());
            data.get_mut(&pid.to_string()).unwrap().pid_namespace = namespace.into();
            write_raw(dir.path(), &data);
            assert!(get_orphan_processes(dir.path(), None).is_empty());
            assert_eq!(
                read_raw(dir.path())[&pid.to_string()].pid_namespace,
                namespace
            );
        }
        let mut data = read_raw(dir.path());
        data.get_mut(&pid.to_string()).unwrap().pid_namespace = current_namespace();
        write_raw(dir.path(), &data);
        let local = get_orphan_processes(dir.path(), None);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].pid, pid);
    }

    /// Bare PID numbers are only unique inside one namespace, so an active
    /// local PID must not delete a foreign entry that happens to share its
    /// number along with all of its recovery metadata.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn active_pid_pruning_cannot_delete_a_colliding_foreign_entry() {
        let dir = make_temp_dir();
        let our_pid = std::process::id();
        record_foreign(dir.path(), our_pid, "host_agent");

        let mut active = HashSet::new();
        active.insert(our_pid);
        let orphans = get_orphan_processes(dir.path(), Some(&active));

        assert!(orphans.is_empty(), "still hidden from this listing");
        let data = read_raw(dir.path());
        assert!(
            data.contains_key(&our_pid.to_string()),
            "but its metadata survives in the pidfile"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn adoption_copies_the_orphan_marker_instead_of_stamping_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::HcomDb::open_at(&dir.path().join("test.db")).unwrap();
        let orphan = OrphanProcess {
            pid: 99_999_998,
            tool: "claude".into(),
            names: vec!["kume".into()],
            pid_namespace: "pid:[foreign]".into(),
            ..Default::default()
        };

        recover_single_orphan_to_db(&db, &orphan, "kume").unwrap();

        let row = db.get_instance_full("kume").unwrap().unwrap();
        assert_eq!(row.pid, Some(99_999_998));
        assert_eq!(row.pid_namespace.as_deref(), Some("pid:[foreign]"));
    }

    #[test]
    fn recording_a_pid_we_spawned_stamps_our_namespace() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), 12345, "claude", "luna"));

        assert_eq!(
            read_raw(dir.path()).get("12345").unwrap().pid_namespace,
            crate::sys::process::current_pid_namespace().unwrap_or_default()
        );
    }

    /// The stop path records a pid it read out of the instance row, not one it
    /// spawned. A sandboxed hcom running it for a host agent must leave the
    /// host's marker alone.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn recording_a_pid_from_another_source_keeps_that_source_marker() {
        let dir = make_temp_dir();

        record_pid(&PidRecord {
            pid_namespace: Some("pid:[foreign]"),
            ..rec(dir.path(), 4_194_305, "claude", "host_agent")
        });
        // An unknown source stays unknown instead of being relabelled as ours.
        record_pid(&PidRecord {
            pid_namespace: Some(""),
            ..rec(dir.path(), 4_194_306, "claude", "legacy_row")
        });

        let data = read_raw(dir.path());
        assert_eq!(data.get("4194305").unwrap().pid_namespace, "pid:[foreign]");
        assert_eq!(data.get("4194306").unwrap().pid_namespace, "");

        // A second record from the same unknown source must not fill it in.
        record_pid(&PidRecord {
            pid_namespace: Some(""),
            ..rec(dir.path(), 4_194_306, "claude", "legacy_row")
        });
        assert_eq!(
            read_raw(dir.path()).get("4194306").unwrap().pid_namespace,
            ""
        );
    }

    #[test]
    fn orphan_scan_drops_keys_that_are_not_pids() {
        let dir = make_temp_dir();
        record_pid(&rec(dir.path(), std::process::id(), "claude", "luna"));
        let mut data = read_raw(dir.path());
        let junk = data.values().next().unwrap().clone();
        data.insert("not-a-pid".to_string(), junk);
        write_raw(dir.path(), &data);

        get_orphan_processes(dir.path(), None);

        assert!(!read_raw(dir.path()).contains_key("not-a-pid"));
    }

    #[test]
    fn test_empty_pidfile() {
        let dir = make_temp_dir();
        let orphans = get_orphan_processes(dir.path(), None);
        assert!(orphans.is_empty());
    }

    #[test]
    fn test_is_alive_current_process() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn test_is_alive_dead_process() {
        assert!(!is_alive(99999999));
    }

    #[test]
    fn test_recover_single_orphan_returns_error_on_db_failure() {
        // DB without init_db → no instances table → INSERT fails
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = crate::db::HcomDb::open_raw(&db_path).unwrap();
        // Deliberately NOT calling db.init_db()

        let orphan = OrphanProcess {
            pid: std::process::id(),
            tool: "claude".into(),
            names: vec!["luna".into()],
            directory: "/tmp".into(),
            process_id: "pid-1".into(),
            terminal_preset: String::new(),
            pane_id: String::new(),
            terminal_id: String::new(),
            kitty_listen_on: String::new(),
            zellij_session_name: String::new(),
            session_id: String::new(),
            notify_port: 0,
            inject_port: 0,
            tag: String::new(),
            pid_namespace: current_namespace(),
        };

        let result = recover_single_orphan_to_db(&db, &orphan, "luna");
        assert!(
            result.is_err(),
            "expected error when DB has no instances table"
        );
    }
}
