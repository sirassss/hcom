//! Launch-context capture plus process/session binding for instance records.
//!
//! This module owns the hook-facing identity handshake:
//! launch metadata capture, placeholder/canonical binding, and instance-row
//! initialization for newly launched or recovered sessions.

use crate::db::{HcomDb, InstanceRow};
use crate::instance_names::{PLACEHOLDER_CONTEXT, PLACEHOLDER_STATUS};
use crate::instances::update_instance_position;
use crate::shared::time::{now_epoch_f64, now_epoch_i64};
use crate::shared::{ST_INACTIVE, ST_LISTENING};

/// Result of binding a session when the caller requires every existing owner
/// to belong to one tool family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCheckedBind {
    Bound(String),
    Unbound,
    Rejected,
}

/// Persist terminal launch metadata without clobbering other launch_context fields.
///
/// The launcher owns the authoritative preset decision. launch_context is only
/// for late-bound metadata such as pane_id, terminal_id, and env snapshot.
pub fn persist_terminal_launch_context(
    db: &HcomDb,
    instance_name: &str,
    requested_preset: Option<&str>,
    effective_preset: &str,
    process_id: Option<&str>,
) {
    let mut ctx = db
        .get_instance_full(instance_name)
        .ok()
        .flatten()
        .and_then(|pos| pos.launch_context)
        .and_then(|json| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json).ok()
        })
        .unwrap_or_default();

    if let Some(pid) = process_id.filter(|v| !v.is_empty()) {
        ctx.insert("process_id".into(), serde_json::json!(pid));
    }
    if !effective_preset.is_empty() {
        ctx.insert(
            "terminal_preset_effective".into(),
            serde_json::json!(effective_preset),
        );
        // Legacy compatibility for older readers and migration logic.
        ctx.insert(
            "terminal_preset".into(),
            serde_json::json!(effective_preset),
        );
    }
    if let Some(requested) = requested_preset.filter(|v| !v.is_empty() && *v != "default") {
        ctx.insert(
            "terminal_preset_requested".into(),
            serde_json::json!(requested),
        );
    }

    let mut updates = serde_json::Map::new();
    updates.insert(
        "terminal_preset_requested".into(),
        serde_json::json!(
            requested_preset
                .filter(|v| !v.is_empty() && *v != "default")
                .unwrap_or("")
        ),
    );
    updates.insert(
        "terminal_preset_effective".into(),
        serde_json::json!(effective_preset),
    );
    updates.insert(
        "launch_context".into(),
        serde_json::json!(serde_json::to_string(&ctx).unwrap_or_else(|_| "{}".to_string())),
    );
    update_instance_position(db, instance_name, &updates);
}

/// Capture environment context and store it for the instance.
///
/// Captures git branch, terminal program, tty, and relevant env vars.
pub fn capture_and_store_launch_context(db: &HcomDb, instance_name: &str) {
    let new_ctx = capture_context();

    // Preserve fields from prior context that can't be recaptured in hook env
    let preserve_keys = [
        "pane_id",
        "terminal_id",
        "kitty_listen_on",
        "process_id",
        "terminal_preset_effective",
    ];
    let mut ctx = new_ctx;

    let missing: Vec<&str> = preserve_keys
        .iter()
        .filter(|k| launch_context_value_missing(ctx.get(**k)))
        .copied()
        .collect();

    if !missing.is_empty()
        && let Ok(Some(pos)) = db.get_instance_full(instance_name)
        && let Some(old_json) = &pos.launch_context
        && let Ok(old_ctx) = serde_json::from_str::<serde_json::Value>(old_json)
    {
        for k in &missing {
            if let Some(val) = old_ctx.get(*k)
                && !launch_context_value_missing(Some(val))
            {
                ctx.insert(k.to_string(), val.clone());
            }
        }
    }

    let json = serde_json::to_string(&ctx).unwrap_or_else(|_| "{}".to_string());
    let mut updates = serde_json::Map::new();
    updates.insert("launch_context".into(), serde_json::json!(json));
    update_instance_position(db, instance_name, &updates);
}

/// "Missing" for the preserve-from-prior-context check. Treats absent, JSON
/// null, and empty strings as missing. Non-string non-null values (numbers,
/// objects, arrays) are considered present even though every preserved field
/// is currently a string — this is intentionally conservative so a future
/// non-string preserved field doesn't silently get clobbered by re-capture.
fn launch_context_value_missing(value: Option<&serde_json::Value>) -> bool {
    match value {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        Some(_) => false,
    }
}

/// Capture launch context snapshot.
fn capture_context() -> serde_json::Map<String, serde_json::Value> {
    let mut ctx = serde_json::Map::new();

    // Git branch
    let git_branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    ctx.insert("git_branch".into(), serde_json::json!(git_branch));

    // TTY
    let tty = std::process::Command::new("tty")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    ctx.insert("tty".into(), serde_json::json!(tty));

    // Env vars (only include if set)
    let env_keys = [
        "TERM_PROGRAM",
        "TERM_SESSION_ID",
        "WINDOWID",
        "ITERM_SESSION_ID",
        "KITTY_WINDOW_ID",
        "KITTY_PID",
        "KITTY_LISTEN_ON",
        "ALACRITTY_WINDOW_ID",
        "WEZTERM_PANE",
        "PTYXIS_PROFILE",
        "PTYXIS_VERSION",
        "GNOME_TERMINAL_SCREEN",
        "KONSOLE_DBUS_WINDOW",
        "TERMINATOR_UUID",
        "TILIX_ID",
        "GUAKE_TAB_UUID",
        "WT_SESSION",
        "ConEmuHWND",
        "TMUX_PANE",
        "STY",
        "ZELLIJ_SESSION_NAME",
        "ZELLIJ_PANE_ID",
        "SSH_TTY",
        "SSH_CONNECTION",
        "WSL_DISTRO_NAME",
        "VSCODE_PID",
        "CURSOR_AGENT",
        "INSIDE_EMACS",
        "NVIM_LISTEN_ADDRESS",
        "CODESPACE_NAME",
        "GITPOD_WORKSPACE_ID",
        "CLOUD_SHELL",
        "REPL_ID",
    ];
    let mut env_map = serde_json::Map::new();
    for key in &env_keys {
        if let Ok(val) = std::env::var(key)
            && !val.is_empty()
        {
            env_map.insert((*key).to_string(), serde_json::json!(val));
        }
    }
    ctx.insert("env".into(), serde_json::Value::Object(env_map));

    // The launcher already resolved the effective preset; record it so
    // child agents can inherit it (see commands::launch). Pane IDs are
    // late-bound from the env vars the preset declares.
    if let Ok(preset_name) = std::env::var("HCOM_LAUNCHED_PRESET")
        && !preset_name.is_empty()
    {
        ctx.insert(
            "terminal_preset_effective".into(),
            serde_json::json!(preset_name),
        );

        if let Some(pane_id_env) = crate::config::get_merged_preset_pane_id_env(&preset_name)
            && let Ok(pane_id) = std::env::var(pane_id_env)
            && !pane_id.is_empty()
        {
            ctx.insert("pane_id".into(), serde_json::json!(pane_id));
        }
    }

    // Process ID for kitty close-by-env matching
    if let Ok(pid) = std::env::var("HCOM_PROCESS_ID")
        && !pid.is_empty()
    {
        ctx.insert("process_id".into(), serde_json::json!(pid));

        // Terminal ID from parent's stdout capture
        let id_file = crate::paths::hcom_dir()
            .join(".tmp")
            .join("terminal_ids")
            .join(&pid);
        if id_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&id_file) {
                let terminal_id = content.trim().to_string();
                if !terminal_id.is_empty() {
                    if std::env::var("HCOM_LAUNCHED_PRESET").as_deref() == Ok("zellij")
                        && let Some(pane_id) = zellij_pane_id_from_terminal_id(&terminal_id)
                    {
                        ctx.insert("pane_id".into(), serde_json::json!(pane_id));
                    }
                    ctx.insert("terminal_id".into(), serde_json::json!(terminal_id));
                }
            }
            let _ = std::fs::remove_file(&id_file);
        }
    }

    ctx
}

fn zellij_pane_id_from_terminal_id(terminal_id: &str) -> Option<String> {
    terminal_id
        .strip_prefix("terminal_")
        .filter(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
        .map(|suffix| suffix.to_string())
}

fn is_true_launch_placeholder(data: Option<&InstanceRow>) -> bool {
    let Some(data) = data else {
        return false;
    };
    if data.session_id.is_some() {
        return false;
    }

    if crate::instances::is_launching_placeholder(data) {
        return true;
    }

    // OpenCode and other PTY-backed tools can be marked ready/listening by the PTY
    // watcher before their later session-start hook binds a session id. Those rows
    // are still launch placeholders, but no longer match the stricter "pending/new"
    // display predicate. Keep this narrow: active no-session rows are real work, not
    // launch placeholders.
    data.status == ST_LISTENING
        && matches!(
            data.status_context.as_str(),
            "start" | "ready_observed" | "launch_blocked_cleared"
        )
}

fn migrate_placeholder_notify(db: &HcomDb, placeholder_name: &str, canonical_name: &str) -> bool {
    match db.migrate_notify_endpoints(placeholder_name, canonical_name) {
        Ok(()) => true,
        Err(e) => {
            crate::log::log_error("binding", "placeholder.migrate_endpoints", &format!("{e}"));
            false
        }
    }
}

/// Delete a true launch placeholder row. Notify endpoints remain on the canonical instance.
fn delete_true_placeholder_instance(db: &HcomDb, placeholder_name: &str) {
    match db.delete_instance(placeholder_name) {
        Ok(true) => {}
        Ok(false) => {
            crate::log::log_info(
                "binding",
                "placeholder.delete_missing",
                &format!("placeholder={placeholder_name}"),
            );
        }
        Err(e) => {
            crate::log::log_error("binding", "placeholder.delete", &format!("{e}"));
        }
    }
}

/// Carry runtime state the PTY wrapper wrote onto the placeholder row over to the
/// canonical instance before the placeholder is deleted. The OS `pid` and terminal
/// `launch_context` (pane_id) are written once at spawn under the launch name
/// (`src/pty/mod.rs`); without migrating them, `hcom kill <canonical>` finds no pid
/// and can't close the terminal pane.
fn migrate_placeholder_runtime_state(
    db: &HcomDb,
    canonical_name: &str,
    placeholder_data: Option<&InstanceRow>,
) {
    let Some(ph) = placeholder_data else {
        return;
    };
    // The PTY wrapper observed this pid, not us: SessionStart can run in a
    // different PID namespace than the process that spawned the tool. Carry the
    // placeholder's recorded namespace across with the pid rather than stamping
    // our own, or the canonical row would claim a pid we cannot actually see.
    if let Some(pid) = ph.pid
        && u32::try_from(pid).is_ok()
    {
        let updates = serde_json::Map::from_iter([
            ("pid".to_string(), serde_json::json!(pid)),
            (
                "pid_namespace".to_string(),
                serde_json::json!(ph.pid_namespace.clone().unwrap_or_default()),
            ),
        ]);
        if let Err(e) = db.update_instance_fields(canonical_name, &updates) {
            crate::log::log_error("binding", "placeholder.migrate_pid", &format!("{e}"));
        }
    }
    if let Some(ref ctx) = ph.launch_context
        && let Err(e) = db.store_launch_context(canonical_name, ctx)
    {
        crate::log::log_error(
            "binding",
            "placeholder.migrate_launch_context",
            &format!("{e}"),
        );
    }
}

fn delete_true_placeholder_if_migrated(
    db: &HcomDb,
    placeholder_name: &str,
    canonical_name: &str,
    placeholder_data: Option<&InstanceRow>,
) {
    if is_true_launch_placeholder(placeholder_data) {
        // Move pid/launch_context to the canonical row before dropping the placeholder
        // so the restored agent stays killable and its pane closeable.
        migrate_placeholder_runtime_state(db, canonical_name, placeholder_data);
        delete_true_placeholder_instance(db, placeholder_name);
    }
}

/// Path 2: after restore_stopped bind, merge notify ports and drop the launch placeholder.
fn retire_true_placeholder_after_canonical_bind(
    db: &HcomDb,
    placeholder_name: Option<&String>,
    canonical_name: &str,
    placeholder_data: Option<&InstanceRow>,
) {
    let Some(ph_name) = placeholder_name else {
        return;
    };
    if ph_name == canonical_name {
        return;
    }

    if !migrate_placeholder_notify(db, ph_name, canonical_name) {
        return;
    }

    delete_true_placeholder_if_migrated(db, ph_name, canonical_name, placeholder_data);
}

/// Recreate a missing instance row from an active placeholder (resume after stop/kill).
fn recreate_instance_from_placeholder(
    db: &HcomDb,
    target_name: &str,
    session_id: &str,
    ph: Option<&InstanceRow>,
) {
    if db.get_instance_full(target_name).ok().flatten().is_some() {
        return;
    }
    let Some(ph) = ph else {
        return;
    };
    initialize_instance_in_position_file(
        db,
        target_name,
        Some(session_id),
        ph.parent_session_id.as_deref(),
        ph.parent_name.as_deref(),
        ph.agent_id.as_deref(),
        (!ph.transcript_path.is_empty()).then_some(ph.transcript_path.as_str()),
        Some(ph.tool.as_str()),
        ph.background != 0,
        ph.tag.as_deref(),
        None,
        None,
        ph.hints.as_deref(),
        Some(ph.directory.as_str()),
    );
}

/// Bind session_id to canonical instance for process_id.
/// Handles 4 paths: canonical exists (with placeholder merge/switch), placeholder bind,
/// and two no-op paths.
pub fn bind_session_to_process(
    db: &HcomDb,
    session_id: &str,
    process_id: Option<&str>,
) -> Option<String> {
    if session_id.is_empty() {
        crate::log::log_info("binding", "bind_session_to_process.no_session_id", "");
        return None;
    }

    crate::log::log_info(
        "binding",
        "bind_session_to_process.entry",
        &format!("session_id={}, process_id={:?}", session_id, process_id),
    );

    // Find placeholder from process binding
    let (placeholder_name, placeholder_data) = if let Some(pid) = process_id {
        match db.get_process_binding(pid) {
            Ok(Some(name)) => {
                let data = match db.get_instance_full(&name) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[hcom] warn: get_instance_full failed for {name}: {e}");
                        None
                    }
                };
                (Some(name), data)
            }
            _ => (None, None),
        }
    } else {
        (None, None)
    };

    // Find canonical from session binding
    let canonical = match db.get_session_binding(session_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[hcom] warn: get_session_binding failed for {session_id}: {e}");
            None
        }
    };

    // Path 1: Canonical exists (session already bound)
    if let Some(ref canonical_name) = canonical {
        crate::log::log_info(
            "binding",
            "bind_session_to_process.canonical_exists",
            &format!(
                "canonical={}, placeholder={:?}",
                canonical_name, placeholder_name
            ),
        );

        recreate_instance_from_placeholder(
            db,
            canonical_name,
            session_id,
            placeholder_data.as_ref(),
        );

        // Reset last_stop on resume
        let now = now_epoch_i64();
        let mut resume_updates = serde_json::Map::new();
        resume_updates.insert("last_stop".into(), serde_json::json!(now));

        if let Some(ref ph_name) = placeholder_name
            && ph_name != canonical_name
        {
            let migrated = migrate_placeholder_notify(db, ph_name, canonical_name);

            if is_true_launch_placeholder(placeholder_data.as_ref()) {
                // Path 1a: True placeholder merge
                if let Some(ref ph_data) = placeholder_data {
                    if let Some(ref tag) = ph_data.tag {
                        resume_updates.insert("tag".into(), serde_json::json!(tag));
                    }
                    if ph_data.background != 0 {
                        resume_updates
                            .insert("background".into(), serde_json::json!(ph_data.background));
                    }
                    if let Some(ref args) = ph_data.launch_args {
                        resume_updates.insert("launch_args".into(), serde_json::json!(args));
                    }
                    // Reset status_context for ready event
                    if std::env::var("HCOM_LAUNCHED").as_deref() == Ok("1") {
                        resume_updates.insert("status_context".into(), serde_json::json!("new"));
                    }
                }

                if migrated {
                    delete_true_placeholder_if_migrated(
                        db,
                        ph_name,
                        canonical_name,
                        placeholder_data.as_ref(),
                    );
                }
            } else {
                // Path 1b: Session switch — retire the real old identity. Unlike a true
                // launch placeholder (deletion above stays gated on endpoint migration),
                // this is a live old identity: retire it regardless of migration outcome,
                // else a migrate failure leaves a duplicate active/listening row and a
                // stale session binding. Endpoints may remain imperfect on the old name,
                // but the delivery loop re-registers under the canonical name.
                let is_cursor = placeholder_data
                    .as_ref()
                    .is_some_and(|row| row.tool == "cursor");
                if is_cursor {
                    // Cursor issues a fresh session UUID on resume/session-switch but keeps
                    // the same OS process. Retiring the placeholder here would kill the
                    // still-live instance; alias the new session id onto it instead.
                    // The orphaned placeholder (e.g. "temp") is deliberately left listening
                    // with no process binding after this path fires; it is cleaned up later
                    // by the normal stop / dead-PID cascade (mark_dead_instances), not immediately.
                    crate::log::log_info(
                        "binding",
                        "bind_canonical.cursor_alias",
                        &format!("not retiring {ph_name}; process rebound to {canonical_name}"),
                    );
                } else {
                    if !migrated {
                        crate::log::log_info(
                            "binding",
                            "bind_canonical.session_switch_migrate_failed",
                            &format!("endpoints may remain on {ph_name}; retiring identity anyway"),
                        );
                    }
                    crate::instance_lifecycle::set_status(
                        db,
                        ph_name,
                        ST_INACTIVE,
                        "exit:session_switch",
                        Default::default(),
                    );
                    if let Err(e) = db.delete_session_bindings_for_instance(ph_name) {
                        crate::log::log_error(
                            "binding",
                            "bind_canonical.delete_session_bindings",
                            &format!("{e}"),
                        );
                    }
                }
            }
        }

        update_instance_position(db, canonical_name, &resume_updates);

        if let Some(pid) = process_id
            && let Err(e) = db.set_process_binding(pid, session_id, canonical_name)
        {
            crate::log::log_error(
                "binding",
                "bind_canonical.set_process_binding",
                &format!("{e}"),
            );
        }

        return Some(canonical_name.clone());
    }

    // Path 2: session_bindings CASCADE'd on delete — recover canonical name from life.stopped
    if canonical.is_none()
        && let Ok(Some(stopped_name)) = db.find_stopped_instance_by_session_id(session_id)
    {
        crate::log::log_info(
            "binding",
            "bind_session_to_process.restore_stopped",
            &format!("stopped_name={stopped_name}, session_id={session_id}"),
        );
        recreate_instance_from_placeholder(
            db,
            &stopped_name,
            session_id,
            placeholder_data.as_ref(),
        );

        if let Err(e) = db.clear_session_id_from_other_instances(session_id, &stopped_name) {
            crate::log::log_error("binding", "restore_stopped.clear_session", &format!("{e}"));
        }
        let mut updates = serde_json::Map::new();
        updates.insert("session_id".into(), serde_json::json!(session_id));
        update_instance_position(db, &stopped_name, &updates);
        if let Err(e) = db.rebind_session(session_id, &stopped_name) {
            crate::log::log_error("binding", "restore_stopped.rebind_session", &format!("{e}"));
        }
        if let Some(pid) = process_id
            && let Err(e) = db.set_process_binding(pid, session_id, &stopped_name)
        {
            crate::log::log_error(
                "binding",
                "restore_stopped.set_process_binding",
                &format!("{e}"),
            );
        }

        retire_true_placeholder_after_canonical_bind(
            db,
            placeholder_name.as_ref(),
            &stopped_name,
            placeholder_data.as_ref(),
        );

        return Some(stopped_name);
    }

    // Path 3: No canonical, but placeholder exists — bind session to placeholder
    if let Some(ref ph_name) = placeholder_name {
        crate::log::log_info(
            "binding",
            "bind_session_to_process.bind_placeholder",
            &format!("placeholder={}, session_id={}", ph_name, session_id),
        );

        if let Err(e) = db.clear_session_id_from_other_instances(session_id, ph_name) {
            crate::log::log_error("binding", "bind_placeholder.clear_session", &format!("{e}"));
        }

        let mut updates = serde_json::Map::new();
        updates.insert("session_id".into(), serde_json::json!(session_id));
        update_instance_position(db, ph_name, &updates);

        if let Err(e) = db.rebind_session(session_id, ph_name) {
            crate::log::log_error(
                "binding",
                "bind_placeholder.rebind_session",
                &format!("{e}"),
            );
        }
        if let Some(pid) = process_id
            && let Err(e) = db.set_process_binding(pid, session_id, ph_name)
        {
            crate::log::log_error(
                "binding",
                "bind_placeholder.set_process_binding",
                &format!("{e}"),
            );
        }

        return Some(ph_name.clone());
    }

    crate::log::log_info("binding", "bind_session_to_process.return_none", "");
    None
}

/// Bind a session without allowing a hook from one tool to adopt another
/// tool's process, live session, or stopped-session identity.
///
/// The ownership checks and the existing binding operation share one SQLite
/// transaction. A disagreement therefore rolls back any mutation performed by
/// `bind_session_to_process`, including placeholder retirement and rebinding.
pub fn bind_session_to_process_for_tool(
    db: &HcomDb,
    session_id: &str,
    process_id: Option<&str>,
    expected_tool: &str,
    create_if_unbound: bool,
) -> ToolCheckedBind {
    let transaction = match db.conn().unchecked_transaction() {
        Ok(transaction) => transaction,
        Err(error) => {
            crate::log::log_error(
                "binding",
                "bind_for_tool.transaction",
                &format!("session_id={session_id} expected_tool={expected_tool} err={error}"),
            );
            return ToolCheckedBind::Rejected;
        }
    };

    let owner_matches = |kind: &str, name: &str, tool: Option<&str>| {
        if tool == Some(expected_tool) {
            return true;
        }
        crate::log::log_warn(
            "binding",
            "bind_for_tool.owner_rejected",
            &format!(
                "kind={kind} instance={name} actual_tool={tool:?} expected_tool={expected_tool} session_id={session_id} process_id={process_id:?}"
            ),
        );
        false
    };

    if let Some(pid) = process_id {
        match db.get_process_binding(pid) {
            Ok(Some(name)) => {
                let tool = db
                    .get_instance_full(&name)
                    .ok()
                    .flatten()
                    .map(|row| row.tool);
                if !owner_matches("process", &name, tool.as_deref()) {
                    return ToolCheckedBind::Rejected;
                }
            }
            Ok(None) => {}
            Err(error) => {
                crate::log::log_error(
                    "binding",
                    "bind_for_tool.process_owner",
                    &format!("process_id={pid} err={error}"),
                );
                return ToolCheckedBind::Rejected;
            }
        }
    }

    let session_owner = match db.get_session_binding(session_id) {
        Ok(owner) => owner,
        Err(error) => {
            crate::log::log_error(
                "binding",
                "bind_for_tool.session_owner",
                &format!("session_id={session_id} err={error}"),
            );
            return ToolCheckedBind::Rejected;
        }
    };
    if let Some(ref name) = session_owner {
        let tool = db
            .get_instance_full(name)
            .ok()
            .flatten()
            .map(|row| row.tool);
        if !owner_matches("session", name, tool.as_deref()) {
            return ToolCheckedBind::Rejected;
        }
    } else {
        match db.find_stopped_instance_by_session_id(session_id) {
            Ok(Some(name)) => {
                let tool = db
                    .conn()
                    .query_row(
                        "SELECT json_extract(data, '$.snapshot.tool') FROM events
                         WHERE type = 'life'
                           AND instance = ?1
                           AND json_extract(data, '$.action') = 'stopped'
                           AND json_extract(data, '$.snapshot.session_id') = ?2
                         ORDER BY id DESC LIMIT 1",
                        rusqlite::params![name, session_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten();
                if !owner_matches("stopped_session", &name, tool.as_deref()) {
                    return ToolCheckedBind::Rejected;
                }
            }
            Ok(None) => {}
            Err(error) => {
                crate::log::log_error(
                    "binding",
                    "bind_for_tool.stopped_owner",
                    &format!("session_id={session_id} err={error}"),
                );
                return ToolCheckedBind::Rejected;
            }
        }
    }

    let bound = bind_session_to_process(db, session_id, process_id).or_else(|| {
        if create_if_unbound {
            create_orphaned_pty_identity(db, session_id, process_id, expected_tool)
        } else {
            None
        }
    });
    if let Some(ref name) = bound {
        let tool = db
            .get_instance_full(name)
            .ok()
            .flatten()
            .map(|row| row.tool);
        if !owner_matches("bound", name, tool.as_deref()) {
            return ToolCheckedBind::Rejected;
        }
    }

    if let Err(error) = transaction.commit() {
        crate::log::log_error(
            "binding",
            "bind_for_tool.commit",
            &format!("session_id={session_id} expected_tool={expected_tool} err={error}"),
        );
        return ToolCheckedBind::Rejected;
    }

    match bound {
        Some(name) => ToolCheckedBind::Bound(name),
        None => ToolCheckedBind::Unbound,
    }
}

/// Rebind process/session after soft-finalize cleared bindings but left the
/// instance row (typically inactive). Used when `bind_session_to_process` finds
/// no process binding, but the caller still knows the instance name via
/// `HCOM_INSTANCE_NAME` in a live OMP process.
pub fn recover_process_binding_for_instance(
    db: &HcomDb,
    instance_name: &str,
    session_id: &str,
    process_id: &str,
) -> Option<String> {
    if instance_name.is_empty() || session_id.is_empty() || process_id.is_empty() {
        return None;
    }

    let instance = db.get_instance_full(instance_name).ok().flatten()?;

    if instance.tool != "omp" {
        return None;
    }
    if instance.status != ST_INACTIVE {
        return None;
    }

    if let Err(e) = db.clear_session_id_from_other_instances(session_id, instance_name) {
        crate::log::log_error("binding", "recover.clear_session", &format!("{e}"));
        return None;
    }

    let mut updates = serde_json::Map::new();
    updates.insert("session_id".into(), serde_json::json!(session_id));
    if let Err(e) = db.update_instance_fields(instance_name, &updates) {
        crate::log::log_error("binding", "recover.update_session_id", &format!("{e}"));
        return None;
    }

    if let Err(e) = db.rebind_session(session_id, instance_name) {
        crate::log::log_error("binding", "recover.rebind_session", &format!("{e}"));
        return None;
    }
    if let Err(e) = db.set_process_binding(process_id, session_id, instance_name) {
        crate::log::log_error("binding", "recover.set_process_binding", &format!("{e}"));
        return None;
    }

    crate::log::log_info(
        "binding",
        "recover_process_binding_for_instance",
        &format!(
            "instance={} session_id={} process_id={}",
            instance_name, session_id, process_id
        ),
    );
    Some(instance_name.to_string())
}

/// Initialize the DB row and default bindings for an instance identity.
///
/// This is the shared setup path used by launch, resume, and orphan recovery.
#[allow(clippy::too_many_arguments)]
pub fn initialize_instance_in_position_file(
    db: &HcomDb,
    instance_name: &str,
    session_id: Option<&str>,
    parent_session_id: Option<&str>,
    parent_name: Option<&str>,
    agent_id: Option<&str>,
    transcript_path: Option<&str>,
    tool: Option<&str>,
    background: bool,
    tag: Option<&str>,
    wait_timeout: Option<i64>,
    subagent_timeout: Option<i64>,
    hints: Option<&str>,
    cwd_override: Option<&str>,
) -> bool {
    let cwd = cwd_override.map(|s| s.to_string()).unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let is_launched = std::env::var("HCOM_LAUNCHED").as_deref() == Ok("1");

    match db.get_instance_full(instance_name) {
        Ok(Some(existing)) => {
            let mut updates = serde_json::Map::new();
            updates.insert("directory".into(), serde_json::json!(cwd));

            if let Some(sid) = session_id {
                updates.insert("session_id".into(), serde_json::json!(sid));
            }
            if let Some(psid) = parent_session_id {
                updates.insert("parent_session_id".into(), serde_json::json!(psid));
            }
            if let Some(pn) = parent_name {
                updates.insert("parent_name".into(), serde_json::json!(pn));
            }
            if let Some(aid) = agent_id {
                updates.insert("agent_id".into(), serde_json::json!(aid));
            }
            if let Some(tp) = transcript_path {
                updates.insert("transcript_path".into(), serde_json::json!(tp));
            }
            if let Some(t) = tool {
                updates.insert("tool".into(), serde_json::json!(t));
            }
            if let Some(t) = tag {
                updates.insert("tag".into(), serde_json::json!(t));
            }
            if background {
                updates.insert("background".into(), serde_json::json!(1));
            }

            let is_true_placeholder = existing.session_id.is_none();
            let is_pending_placeholder = is_true_placeholder
                && existing.status == PLACEHOLDER_STATUS
                && existing.status_context == PLACEHOLDER_CONTEXT;
            if existing.last_event_id == 0 && is_true_placeholder {
                let current_max = db.get_last_event_id();
                let launch_event_id = std::env::var("HCOM_LAUNCH_EVENT_ID")
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok());

                let eid = match launch_event_id {
                    Some(id) if id <= current_max => id,
                    _ => current_max,
                };
                updates.insert("last_event_id".into(), serde_json::json!(eid));
            }

            if is_launched {
                updates.insert("status_context".into(), serde_json::json!("new"));
            }

            if !updates.is_empty() {
                let _ = db.update_instance_fields(instance_name, &updates);
            }

            if is_pending_placeholder {
                auto_subscribe_defaults(db, instance_name, tool.unwrap_or(existing.tool.as_str()));
            }

            true
        }
        Ok(None) => {
            let now = now_epoch_f64();
            let current_max = db.get_last_event_id();
            let launch_event_id = std::env::var("HCOM_LAUNCH_EVENT_ID")
                .ok()
                .and_then(|s| s.parse::<i64>().ok());

            let initial_event_id = match launch_event_id {
                Some(id) if id <= current_max => id,
                _ => current_max,
            };

            let mut data = serde_json::Map::new();
            data.insert("name".into(), serde_json::json!(instance_name));
            data.insert("last_event_id".into(), serde_json::json!(initial_event_id));
            data.insert("directory".into(), serde_json::json!(cwd));
            data.insert("last_stop".into(), serde_json::json!(0));
            data.insert("created_at".into(), serde_json::json!(now));
            data.insert(
                "session_id".into(),
                match session_id {
                    Some(s) if !s.is_empty() => serde_json::json!(s),
                    _ => serde_json::Value::Null,
                },
            );
            data.insert("transcript_path".into(), serde_json::json!(""));
            data.insert("name_announced".into(), serde_json::json!(0));
            data.insert("tag".into(), serde_json::Value::Null);
            data.insert("status".into(), serde_json::json!(ST_INACTIVE));
            data.insert("status_time".into(), serde_json::json!(now_epoch_i64()));
            data.insert("status_context".into(), serde_json::json!("new"));
            data.insert("tool".into(), serde_json::json!(tool.unwrap_or("claude")));
            data.insert(
                "background".into(),
                serde_json::json!(if background { 1 } else { 0 }),
            );

            if let Some(t) = tag {
                data.insert("tag".into(), serde_json::json!(t));
            } else if (session_id.is_some() || parent_session_id.is_some() || is_launched)
                && let Ok(hcom_config) = crate::config::HcomConfig::load(None)
                && !hcom_config.tag.is_empty()
            {
                data.insert("tag".into(), serde_json::json!(hcom_config.tag));
            }

            // Resolve HCOM_TIMEOUT explicitly rather than leaving the column
            // unset — the schema's DEFAULT 86400 would otherwise silently
            // mask the configured value for every non-PTY instance (issue #71).
            let effective_wait_timeout =
                wait_timeout.unwrap_or_else(crate::config::HcomConfig::effective_timeout);
            data.insert(
                "wait_timeout".into(),
                serde_json::json!(effective_wait_timeout),
            );
            if let Some(st) = subagent_timeout {
                data.insert("subagent_timeout".into(), serde_json::json!(st));
            }
            if let Some(h) = hints {
                data.insert("hints".into(), serde_json::json!(h));
            }
            if let Some(psid) = parent_session_id {
                data.insert("parent_session_id".into(), serde_json::json!(psid));
            }
            if let Some(pn) = parent_name {
                data.insert("parent_name".into(), serde_json::json!(pn));
            }
            if let Some(aid) = agent_id {
                data.insert("agent_id".into(), serde_json::json!(aid));
            }
            if let Some(tp) = transcript_path {
                data.insert("transcript_path".into(), serde_json::json!(tp));
            }

            match db.save_instance_named(instance_name, &data) {
                Ok(true) => {
                    log_created_and_auto_subscribe(
                        db,
                        instance_name,
                        is_launched,
                        parent_session_id,
                        parent_name,
                        tool.unwrap_or(""),
                    );
                    true
                }
                _ => true,
            }
        }
        Err(_) => false,
    }
}

fn log_created_and_auto_subscribe(
    db: &HcomDb,
    instance_name: &str,
    is_launched: bool,
    parent_session_id: Option<&str>,
    parent_name: Option<&str>,
    tool: &str,
) {
    let launcher = std::env::var("HCOM_LAUNCHED_BY").unwrap_or_else(|_| "unknown".to_string());
    let event_data = serde_json::json!({
        "action": "created",
        "by": launcher,
        "is_hcom_launched": is_launched,
        "is_subagent": parent_session_id.is_some(),
        "parent_name": parent_name.unwrap_or(""),
    });
    let _ = db.log_event("life", instance_name, &event_data);
    auto_subscribe_defaults(db, instance_name, tool);
}

/// Create orphaned PTY identity — called when process binding exists but session_id
/// is fresh (e.g., after /clear). Generates new name, creates instance, binds it.
pub fn create_orphaned_pty_identity(
    db: &HcomDb,
    session_id: &str,
    process_id: Option<&str>,
    tool: &str,
) -> Option<String> {
    let name = match crate::instance_names::generate_unique_name(db) {
        Ok(n) => n,
        Err(e) => {
            crate::log::log_error(
                "instances",
                "create_orphaned_pty_identity.name_gen",
                &e.to_string(),
            );
            return None;
        }
    };

    let success = initialize_instance_in_position_file(
        db,
        &name,
        Some(session_id),
        None,
        None,
        None,
        None,
        Some(tool),
        false,
        None,
        None,
        None,
        None,
        None,
    );

    if !success {
        return None;
    }

    if let Err(e) = db.rebind_session(session_id, &name) {
        eprintln!("[hcom] warn: rebind_session failed for {name}: {e}");
    }
    if let Some(pid) = process_id
        && let Err(e) = db.set_process_binding(pid, session_id, &name)
    {
        eprintln!("[hcom] warn: set_process_binding failed for {name}: {e}");
    }

    Some(name)
}

/// Resolve instance name for a process_id via process_bindings.
pub fn resolve_process_binding(db: &HcomDb, process_id: Option<&str>) -> Option<String> {
    let pid = process_id?;
    db.get_process_binding(pid).ok()?
}

/// Resolve instance via process or session binding.
pub fn resolve_instance_from_binding(
    db: &HcomDb,
    session_id: Option<&str>,
    process_id: Option<&str>,
) -> Option<InstanceRow> {
    if let Some(pid) = process_id
        && let Ok(Some(name)) = db.get_process_binding(pid)
        && let Ok(Some(instance)) = db.get_instance_full(&name)
    {
        return Some(instance);
    }

    if let Some(sid) = session_id
        && let Some(name) = db.get_session_binding(sid).ok().flatten()
        && let Ok(Some(instance)) = db.get_instance_full(&name)
    {
        return Some(instance);
    }

    None
}

/// Auto-subscribe instance to default event subscriptions from config.
/// Called during instance creation.
fn auto_subscribe_eligible(tool: &str) -> bool {
    tool.parse::<crate::tool::Tool>()
        .is_ok_and(|tool| tool.spec().released)
}

fn auto_subscribe_defaults(db: &HcomDb, instance_name: &str, tool: &str) {
    if !auto_subscribe_eligible(tool) {
        return;
    }

    let _ = db.cleanup_subscriptions(instance_name);
    let _ = db.cleanup_thread_memberships_for_name_reuse(instance_name);
    let config = match crate::config::HcomConfig::load(None) {
        Ok(c) => c,
        Err(_) => return,
    };
    if config.auto_subscribe.is_empty() {
        return;
    }

    use std::collections::HashMap;

    let preset_to_flags: HashMap<&str, Vec<(&str, &str)>> = HashMap::from([
        ("collision", vec![("collision", "1")]),
        ("created", vec![("action", "created")]),
        ("stopped", vec![("action", "stopped")]),
        ("blocked", vec![("status", "blocked")]),
    ]);

    for preset in config
        .auto_subscribe
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        if let Some(flag_pairs) = preset_to_flags.get(preset) {
            let mut filters: HashMap<String, Vec<String>> = HashMap::new();
            for (key, val) in flag_pairs {
                filters
                    .entry(key.to_string())
                    .or_default()
                    .push(val.to_string());
            }
            let _ = crate::db::subscriptions::create_filter_subscription(
                db,
                &filters,
                &[],
                instance_name,
                false,
                None,
            );
        }
    }
}

/// Scoped guard that sets one env var and restores its prior value (or absence) on
/// drop. Callers using `set`/`unset` must be marked `#[serial]` (env var mutation is
/// process-global).
#[cfg(test)]
pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

#[cfg(test)]
impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: callers of this guard are marked #[serial].
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    pub(crate) fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: callers of this guard are marked #[serial].
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: callers of this guard are marked #[serial].
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[cfg(test)]
#[path = "instance_binding_tests.rs"]
mod tests;
