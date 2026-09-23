//! Start command: `hcom start [--name <agent-id>] [--as <name>] [--orphan <name|pid>]`
//!
//! Runs inside an already-running tool session rather than launching a new one.
//! Used for adhoc/manual setup, identity rebinding, and orphan recovery:
//! - Bare start: detect vanilla tool or create adhoc instance
//! - `--name <agent-id>`: register a subagent (a router-level global flag, not
//!   parsed by `StartArgs` — resolved in `run()` via `flags.name`)
//! - `--orphan`: recover orphaned PTY process
//! - `--as`: rebind session identity

use anyhow::{Result, bail};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::bootstrap;
use crate::claude_actor;
use crate::config::HcomConfig;
use crate::db::{HcomDb, InstanceRow};
use crate::identity;
use crate::instance_binding;
use crate::instance_lifecycle as lifecycle;
use crate::instance_names;
use crate::instances;
use crate::log::log_info;
use crate::paths;
use crate::pidtrack;
use crate::relay;
use crate::router::GlobalFlags;
use crate::shared::constants::ST_ACTIVE;
use crate::shared::context::HcomContext;

/// Parsed arguments for `hcom start`.
#[derive(clap::Parser, Debug)]
#[command(name = "start", about = "Start hcom participation")]
pub struct StartArgs {
    /// Rebind to a different instance name
    #[arg(long = "as")]
    pub as_name: Option<String>,
    /// Recover orphaned PTY process by name or PID
    #[arg(long)]
    pub orphan: Option<String>,
}

/// Run the start command.
pub fn run(argv: &[String], flags: &GlobalFlags) -> Result<i32> {
    // Filter out global flags already consumed by the router (start, --name X, --go)
    let mut filtered = vec!["start".to_string()];
    let mut skip_next = false;
    for arg in argv {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "start" | "--go" => continue,
            "--name" => {
                skip_next = true;
                continue;
            }
            _ => filtered.push(arg.clone()),
        }
    }

    use clap::Parser;
    let start_args = match StartArgs::try_parse_from(&filtered) {
        Ok(a) => a,
        Err(e) => {
            e.print().ok();
            return Ok(if e.use_stderr() { 1 } else { 0 });
        }
    };

    let orphan_target = start_args.orphan;
    let rebind_target = start_args.as_name;

    let db = HcomDb::open()?;
    let hcom_dir = paths::hcom_dir();

    let ctx = HcomContext::from_os();
    let verified_actor = claude_actor::resolve_env_actor(&db).map_err(anyhow::Error::new)?;
    if let (Some(actor), Some(name)) = (verified_actor.as_ref(), flags.name.as_deref()) {
        claude_actor::ensure_explicit_matches(&db, actor, name).map_err(anyhow::Error::new)?;
    }

    let requested_name = flags
        .name
        .as_deref()
        .map(|name| identity::resolve_display_name(&db, name).unwrap_or_else(|| name.to_string()));

    // A verified child actor can only promote/use its existing row. It cannot
    // rebind or recover another identity, and it does not need --name.
    if let Some(actor) = verified_actor.as_ref()
        && let Some(actor_row) = db.get_instance_full(&actor.name)?
        && instances::is_subagent_instance(&actor_row)
    {
        if rebind_target.is_some() {
            println!("[HCOM] Subagents cannot use --as. End your turn.");
            return Ok(1);
        }
        if orphan_target.is_some() {
            println!("[HCOM] Subagents cannot use --orphan. End your turn.");
            return Ok(1);
        }
        return start_subagent(&db, &actor_row);
    }

    // Without a capability, retain the ordinary manual fallback. A direct
    // indexed child lookup supports the documented --name <agent-id> form
    // without scanning duplicated parent JSON.
    let subagent_via_name = if verified_actor.is_none() {
        requested_name
            .as_deref()
            .and_then(|id| detect_subagent(&db, id))
    } else {
        None
    };
    let subagent_via_as = if verified_actor.is_none() {
        rebind_target
            .as_deref()
            .and_then(|id| detect_subagent(&db, id))
    } else {
        None
    };

    if subagent_via_as.is_some() || (subagent_via_name.is_some() && rebind_target.is_some()) {
        println!("[HCOM] Subagents cannot change identity. End your turn.");
        return Ok(1);
    }

    if let Some(orphan) = orphan_target {
        return start_from_orphan(&db, &hcom_dir, &orphan, &ctx);
    }

    if let Some(rebind) = rebind_target {
        let current_name = verified_actor
            .as_ref()
            .map(|actor| actor.name.as_str())
            .or(requested_name.as_deref());
        return start_rebind(&db, &rebind, &ctx, current_name);
    }

    if let Some(subagent) = subagent_via_name {
        return start_subagent(&db, &subagent);
    }

    // A verified root actor stays the root even while children exist.
    let effective_name = verified_actor
        .as_ref()
        .map(|actor| actor.name.as_str())
        .or(requested_name.as_deref());
    start_bare(&db, &hcom_dir, &ctx, effective_name)
}

/// Resolve a live child row directly by agent_id (or by its exact row name).
fn detect_subagent(db: &HcomDb, check_id: &str) -> Option<InstanceRow> {
    let name = db
        .get_instance_by_agent_id(check_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| check_id.to_string());
    let row = db.get_instance_full(&name).ok().flatten()?;
    row.parent_name.as_ref().filter(|name| !name.is_empty())?;
    Some(row)
}

/// Promote an existing dormant child row into active hcom participation.
fn start_subagent(db: &HcomDb, info: &InstanceRow) -> Result<i32> {
    let parent_name = info.parent_name.as_deref().unwrap_or("");
    if parent_name.is_empty() || info.agent_id.as_deref().unwrap_or("").is_empty() {
        bail!(
            "Subagent row '{}' is missing parent/agent identity",
            info.name
        );
    }

    let was_announced = info.name_announced != 0;
    lifecycle::set_status(db, &info.name, ST_ACTIVE, "tool:start", Default::default());
    instance_binding::capture_and_store_launch_context(db, &info.name);

    log_info(
        "lifecycle",
        "start.subagent",
        &format!(
            "name={} parent={} agent_id={} announced={}",
            info.name,
            parent_name,
            info.agent_id.as_deref().unwrap_or(""),
            was_announced
        ),
    );

    if was_announced {
        println!("hcom already started for {}", info.name);
        return Ok(0);
    }

    let bootstrap = bootstrap::get_subagent_bootstrap(&info.name, parent_name);
    if !bootstrap.is_empty() {
        println!("{bootstrap}");
    }
    let mut updates = serde_json::Map::new();
    updates.insert("name_announced".into(), serde_json::json!(true));
    instances::update_instance_position(db, &info.name, &updates);

    Ok(0)
}

/// Reclaim a surviving row only when its process/session evidence is consistent.
fn orphan_can_reuse_name(
    db: &HcomDb,
    preferred_name: &str,
    orphan: &pidtrack::OrphanProcess,
) -> Result<bool> {
    // A different session/process owner is stronger evidence than the name in
    // an old pidfile. Even minting here would steal its session via rebind.
    if !orphan.session_id.is_empty()
        && let Some(owner) = db.get_session_binding(&orphan.session_id)?
        && owner != preferred_name
    {
        bail!("Orphan session is already owned by '{}'.", owner);
    }
    let bound_name = db
        .get_process_binding_full(&orphan.process_id)?
        .map(|(_, name)| name);
    if let Some(owner) = bound_name.as_deref()
        && owner != preferred_name
    {
        bail!("Orphan process is already owned by '{}'.", owner);
    }
    if preferred_name.is_empty() || !identity::is_valid_base_name(preferred_name) {
        return Ok(false);
    }
    let Some(row) = db.get_instance_full(preferred_name)? else {
        return Ok(true);
    };
    let same_session = !orphan.session_id.is_empty()
        && row.session_id.as_deref() == Some(orphan.session_id.as_str());
    let same_process = bound_name.as_deref() == Some(preferred_name);
    if same_process
        && row.session_id.as_deref().is_some_and(|sid| !sid.is_empty())
        && !orphan.session_id.is_empty()
        && !same_session
    {
        bail!(
            "Orphan identity '{}' has moved to another session.",
            preferred_name
        );
    }
    if !same_session && !same_process {
        return Ok(false);
    }
    if row.tool != orphan.tool {
        bail!(
            "Orphan identity '{}' belongs to a different tool.",
            preferred_name
        );
    }
    if let Some(pid) = row.pid {
        let same_pid = pid == i64::from(orphan.pid)
            && row.pid_namespace.as_deref().unwrap_or("") == orphan.pid_namespace;
        if !same_pid
            && u32::try_from(pid)
                .ok()
                .and_then(|pid| crate::sys::process::is_alive_in(pid, row.pid_namespace.as_deref()))
                != Some(false)
        {
            bail!(
                "Orphan identity '{}' still has another owner (or its liveness is unknown).",
                preferred_name
            );
        }
    } else if !same_process && db.has_process_binding_for_instance(preferred_name) {
        bail!(
            "Orphan identity '{}' has a different process binding.",
            preferred_name
        );
    }
    Ok(true)
}

/// Recover orphaned PTY process by PID or name.
fn start_from_orphan(
    db: &HcomDb,
    hcom_dir: &std::path::Path,
    target: &str,
    _ctx: &HcomContext,
) -> Result<i32> {
    let active_pids: HashSet<u32> = db
        .iter_instances_full()?
        .iter()
        .filter_map(|inst| inst.pid.map(|p| p as u32))
        .collect();
    let orphans = pidtrack::get_orphan_processes(hcom_dir, Some(&active_pids));

    if orphans.is_empty() {
        bail!("No orphan processes found.");
    }

    // Match by PID or name
    let orphan = if let Ok(pid) = target.parse::<u32>() {
        match orphans.iter().find(|o| o.pid == pid) {
            Some(o) => o,
            None => bail!("Orphan PID {} not found.", pid),
        }
    } else {
        let matches: Vec<_> = orphans
            .iter()
            .filter(|o| o.names.contains(&target.to_string()))
            .collect();
        match matches.len() {
            0 => bail!("Orphan '{}' not found.", target),
            1 => matches[0],
            _ => {
                let pids: Vec<String> = matches.iter().map(|m| m.pid.to_string()).collect();
                bail!(
                    "Multiple orphans match '{}' (PIDs: {}). Use --orphan <pid>.",
                    target,
                    pids.join(", ")
                );
            }
        }
    };

    let pid = orphan.pid;

    if orphan.process_id.is_empty() {
        bail!(
            "Orphan PID {} has no process_id and cannot be recovered.",
            pid
        );
    }

    let preferred_name = orphan.names.last().cloned().unwrap_or_default();
    // Match launch lock order: name-generation lock, then DB write lock.
    let name_lock = instance_names::lock_name_generation(db)?;
    // Hold ownership stable between the checks and the recovery writes.
    let transaction =
        rusqlite::Transaction::new_unchecked(db.conn(), rusqlite::TransactionBehavior::Immediate)?;
    let can_reuse = orphan_can_reuse_name(db, &preferred_name, orphan)?;
    let name = if can_reuse {
        preferred_name
    } else {
        instance_names::reserve_generated_name_locked(db)?
    };

    // Core DB registration
    pidtrack::recover_single_orphan_to_db(db, orphan, &name).map_err(anyhow::Error::msg)?;
    transaction.commit()?;
    drop(name_lock);
    // Other connections can now see the restored binding and status.
    crate::notify::wake(db, &name, crate::notify::WakeKind::DELIVERY_LOOPS);

    // Recovery may run in a management pane. Do not rename the caller's TTY;
    // the recovered PTY refreshes its own title through its delivery loop.

    db.log_event(
        "life",
        &name,
        &json!({
            "action": "started",
            "by": "cli",
            "reason": "orphan_recover",
            "orphan_pid": pid,
        }),
    )
    .ok();

    pidtrack::remove_pid(hcom_dir, pid);

    println!("[hcom:{}]", name);
    if can_reuse {
        println!("Recovered orphan PID {} as '{}'.", pid, name);
    } else {
        println!(
            "Recovered orphan PID {} as new identity '{}' (name conflict/unavailable).",
            pid, name
        );
    }

    log_info(
        "start",
        "orphan.recovered",
        &format!("name={} pid={} tool={}", name, pid, orphan.tool),
    );

    Ok(0)
}

#[derive(Debug, Clone)]
struct ChildLink {
    name: String,
    parent_name: Option<String>,
}

fn snapshot_child_links(db: &HcomDb, session_id: Option<&str>) -> Result<Vec<ChildLink>> {
    let Some(session_id) = session_id.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut stmt = db
        .conn()
        .prepare("SELECT name, parent_name FROM instances WHERE parent_session_id = ?")?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| {
        Ok(ChildLink {
            name: row.get(0)?,
            parent_name: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn restore_child_links_after_root_rebind(
    db: &HcomDb,
    links: &[ChildLink],
    session_id: &str,
    old_root: &str,
    new_root: &str,
) -> Result<()> {
    db.with_immediate_transaction(|txn| {
        for link in links {
            let parent_name = match link.parent_name.as_deref() {
                Some(parent) if parent == old_root => Some(new_root),
                other => other,
            };
            txn.execute(
                "UPDATE instances SET parent_session_id = ?, parent_name = ? WHERE name = ?",
                rusqlite::params![session_id, parent_name, &link.name],
            )?;
        }
        Ok(())
    })
}

/// Rebind session identity (`--as <name>`), preserving last_event_id and any
/// live Claude child hierarchy owned by the current root actor.
fn start_rebind(
    db: &HcomDb,
    rebind_target: &str,
    ctx: &HcomContext,
    explicit_name: Option<&str>,
) -> Result<i32> {
    let hcom_dir = paths::hcom_dir();

    // Resolve the target name
    let target_name = identity::resolve_display_name_or_stopped(db, rebind_target)
        .unwrap_or_else(|| rebind_target.to_string());

    // Guard: refuse to reclaim a subagent slot. Subagents share their parent's
    // session_id, so `hcom start --as <subagent_name>` from inside a subagent
    // bash would rebind session_bindings[parent_sid] to the subagent name,
    // clobbering the parent's identity. `--as` is documented for top-level
    // restartable identities (compaction/resume/clear), not for subagent
    // lifecycle — which has its own SubagentStart bootstrap path.
    if db.was_subagent_name(&target_name) {
        eprintln!(
            "Error: '{target_name}' is a subagent slot; cannot be reclaimed with --as.\n\
             Subagents register via 'hcom start --name <agent-id>' in the SubagentStart context. If your session ended, stop working and end your turn."
        );
        return Ok(1);
    }

    let explicit_current_name = explicit_name.unwrap_or("");

    // Resolve session_id from process binding or existing instance
    let mut session_id: Option<String> = None;
    if let Some(ref process_id) = ctx.process_id
        && let Ok(Some((sid, _))) = db.get_process_binding_full(process_id)
    {
        session_id = sid.filter(|s| !s.is_empty());
    }
    if session_id.is_none()
        && !explicit_current_name.is_empty()
        && let Ok(Some(current_data)) = db.get_instance_full(explicit_current_name)
    {
        session_id = current_data.session_id.filter(|s| !s.is_empty());
    }
    if session_id.is_none() {
        // Direct Claude and Codex sessions have no hcom process binding. Their
        // native ids are definitive and are also what their hooks report.
        session_id = resolve_vanilla_session_id(ctx);
    }
    let current_name = if !explicit_current_name.is_empty() {
        explicit_current_name.to_string()
    } else if let Some(ref sid) = session_id {
        db.get_session_binding(sid)?.unwrap_or_default()
    } else {
        String::new()
    };
    let child_links = snapshot_child_links(db, session_id.as_deref())?;

    let target_meta = load_rebind_target_metadata(db, &target_name).ok();
    if let Some(ref meta) = target_meta {
        ensure_rebind_compatible(&target_name, meta, ctx)?;
    }

    // Preserve last_event_id from target (cursor preservation)
    let mut last_event_id = target_meta.as_ref().map(|m| m.last_event_id);
    let target_data = db.get_instance_full(&target_name)?;

    // Final fallback: use current max to avoid re-delivering old messages
    if last_event_id.is_none() {
        last_event_id = Some(db.get_last_event_id());
    }

    // Skip delete for remote instances (origin_device_id)
    if let Some(ref td) = target_data
        && (td.origin_device_id.is_none() || td.origin_device_id.as_deref() == Some(""))
        && let Err(e) = db.delete_instance(&target_name)
    {
        eprintln!("[hcom] warn: delete_instance failed for {target_name}: {e}");
    }

    // Clean up target's bindings
    if let Err(e) = db.delete_process_bindings_for_instance(&target_name) {
        eprintln!("[hcom] warn: delete_process_bindings failed for {target_name}: {e}");
    }
    if let Err(e) = db.delete_session_bindings_for_instance(&target_name) {
        eprintln!("[hcom] warn: delete_session_bindings failed for {target_name}: {e}");
    }

    // Delete old identity if different from target
    if !current_name.is_empty()
        && current_name != target_name
        && let Err(e) = db.delete_instance(&current_name)
    {
        eprintln!("[hcom] warn: delete_instance failed for {current_name}: {e}");
    }

    // Create fresh instance with the target name.
    let tool = if ctx.process_id.is_some() || session_id.is_some() {
        ctx.tool.as_str()
    } else {
        "adhoc"
    };
    let cwd_override = ctx.cwd.to_string_lossy().to_string();
    instance_binding::initialize_instance_in_position_file(
        db,
        &target_name,
        session_id.as_deref(),
        None, // parent_session_id
        None, // parent_name
        None, // agent_id
        None, // transcript_path
        Some(tool),
        false, // background
        None,  // tag
        None,  // wait_timeout
        None,  // subagent_timeout
        None,  // hints
        Some(&cwd_override),
    );

    if let Some(ref sid) = session_id {
        let old_root = if current_name.is_empty() {
            target_name.as_str()
        } else {
            current_name.as_str()
        };
        restore_child_links_after_root_rebind(db, &child_links, sid, old_root, &target_name)?;
        if old_root != target_name {
            db.rebind_claude_root_actor_state(sid, old_root, &target_name)?;
        }
    }

    // Restore cursor position + mark as announced
    {
        let mut updates = serde_json::Map::new();
        if let Some(eid) = last_event_id {
            updates.insert("last_event_id".into(), serde_json::json!(eid));
        }
        updates.insert("name_announced".into(), serde_json::json!(1));
        if let Err(e) = db.update_instance_fields(&target_name, &updates) {
            eprintln!("[hcom] warn: update_instance_fields failed for {target_name}: {e}");
        }
    }

    // Create bindings
    if let Some(ref sid) = session_id {
        if let Err(e) = db.set_session_binding(sid, &target_name) {
            eprintln!("[hcom] warn: set_session_binding failed for {target_name}: {e}");
        } else if ctx.tool == crate::tool::Tool::Claude
            && let Err(e) = db.mark_claude_session_validated(sid, &target_name)
        {
            // The cache still names the identity being replaced, and it is keyed
            // by session generation, so it does not expire on its own. Left
            // stale, every hook for this session resolves to no_instance: no
            // status, no delivery, and the reclaimed row is flagged
            // launch_failed ~30s later while the session is alive and bound.
            eprintln!("[hcom] warn: mark_claude_session_validated failed for {target_name}: {e}");
        }
    }
    if let Some(ref process_id) = ctx.process_id {
        let sid = session_id.as_deref().unwrap_or("");
        if let Err(e) = db.set_process_binding(process_id, sid, &target_name) {
            eprintln!("[hcom] warn: set_process_binding failed for {target_name}: {e}");
        }

        // Migrate notify endpoints before notify so wake reaches correct port
        if !current_name.is_empty()
            && current_name != target_name
            && let Err(e) = db.migrate_notify_endpoints(&current_name, &target_name)
        {
            eprintln!("[hcom] warn: migrate_notify_endpoints failed: {e}");
        }

        crate::notify::wake(db, &target_name, crate::notify::WakeKind::DELIVERY_LOOPS);
    }

    crate::runtime_env::set_terminal_title(&target_name);

    // Print bootstrap
    let hcom_config = HcomConfig::load(None).unwrap_or_else(|_| {
        let mut c = HcomConfig::default();
        c.normalize();
        c
    });

    let bootstrap_text = bootstrap::get_bootstrap(
        db,
        &hcom_dir,
        &target_name,
        tool,
        false,
        false,
        &ctx.notes,
        &hcom_config.tag,
        relay::is_relay_enabled(&hcom_config),
        None,
    );

    println!("[hcom:{}]", target_name);
    println!("{}", bootstrap_text);
    // Same reason as bare start: keep the new name visible in a tailed snapshot.
    println!("[hcom:{}]", target_name);

    log_info(
        "start",
        "rebind.complete",
        &format!("from={} to={}", current_name, target_name),
    );

    Ok(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RebindTargetMetadata {
    tool: String,
    directory: String,
    last_event_id: i64,
}

fn ensure_rebind_compatible(
    target_name: &str,
    meta: &RebindTargetMetadata,
    ctx: &HcomContext,
) -> Result<()> {
    let current_tool = ctx.tool.as_str();
    if !meta.tool.is_empty() && meta.tool != current_tool {
        bail!(
            "Refusing to reclaim '{target_name}': latest identity used tool '{}' but current session is '{}'",
            meta.tool,
            current_tool
        );
    }

    let current_dir = ctx.cwd.to_string_lossy();
    if !meta.directory.is_empty() && !same_path(&meta.directory, &current_dir) {
        bail!(
            "Refusing to reclaim '{target_name}': latest identity used directory '{}' but current session is '{}'",
            meta.directory,
            current_dir
        );
    }

    Ok(())
}

fn same_path(left: &str, right: &str) -> bool {
    normalize_path_for_compare(left) == normalize_path_for_compare(right)
}

fn normalize_path_for_compare(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// Load rebind metadata from the live row first, then the latest stopped snapshot.
fn load_rebind_target_metadata(db: &HcomDb, name: &str) -> Result<RebindTargetMetadata> {
    if let Some(inst) = db.get_instance_full(name)? {
        return Ok(RebindTargetMetadata {
            tool: inst.tool,
            directory: inst.directory,
            last_event_id: inst.last_event_id,
        });
    }

    let mut stmt = db.conn().prepare(
        "SELECT data FROM events WHERE type='life' AND instance=? ORDER BY id DESC LIMIT 10",
    )?;

    let rows: Vec<String> = stmt
        .query_map(rusqlite::params![name], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    for data_str in &rows {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(data_str)
            && data.get("action").and_then(|v| v.as_str()) == Some("stopped")
            && let Some(snapshot) = data.get("snapshot")
        {
            return Ok(RebindTargetMetadata {
                tool: snapshot
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                directory: snapshot
                    .get("directory")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                last_event_id: snapshot
                    .get("last_event_id")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            });
        }
    }

    bail!("No rebind metadata found for '{}'", name)
}

/// Resolve the Claude session id visible to a CLI invocation.
///
/// Claude sets `CLAUDE_CODE_SESSION_ID` in Bash and PowerShell subprocesses,
/// and it matches the `session_id` passed to hooks.
fn resolve_claude_session_id(env: &HashMap<String, String>) -> Option<String> {
    env.get("CLAUDE_CODE_SESSION_ID")
        .filter(|value| !value.is_empty())
        .cloned()
}

/// Resolve a native session id exposed to shell commands by a direct tool run.
fn resolve_vanilla_session_id(ctx: &HcomContext) -> Option<String> {
    match ctx.tool {
        crate::tool::Tool::Claude => resolve_claude_session_id(&ctx.raw_env),
        crate::tool::Tool::Codex => ctx.codex_thread_id.clone(),
        _ => None,
    }
}

/// Path C: Bare start — detect tool or create adhoc instance.
fn start_bare(
    db: &HcomDb,
    hcom_dir: &std::path::Path,
    ctx: &HcomContext,
    explicit_name: Option<&str>,
) -> Result<i32> {
    let explicit_name = explicit_name
        .map(|name| identity::resolve_display_name(db, name).unwrap_or_else(|| name.to_string()));
    let explicit_name = explicit_name.as_deref();

    // Skip vanilla detection if --name is provided with an existing instance
    let has_valid_identity = explicit_name
        .and_then(|n| db.get_instance_full(n).ok().flatten())
        .is_some();

    let vanilla_session_id = resolve_vanilla_session_id(ctx);
    // Only native session identity supports hooks in a manually started tool.
    // Other manual starts use ordinary adhoc participation.
    if !has_valid_identity && !ctx.is_launched && vanilla_session_id.is_some() {
        let vanilla_tool = ctx.tool;
        if !vanilla_tool.hooks().is_empty() && !vanilla_tool.verify_hooks_installed(false) {
            // Tools whose hooks ship as a plugin are never installed as a side
            // effect: doing so would shell out to that tool's CLI, which clones
            // a marketplace over the network, and `hcom start` is a request to
            // join the bus, not a request to change the machine. Report and let
            // the user decide — the same contract `ensure_hooks_installed` in
            // src/launcher.rs follows.
            if vanilla_tool.hooks_ship_as_plugin() {
                eprintln!(
                    "hcom hooks are not installed for {}.\n\
                     Messages will not be delivered automatically.\n  \
                     Install:  hcom hooks add {}",
                    vanilla_tool.as_str(),
                    vanilla_tool.as_str()
                );
                return Ok(1);
            }
            println!("Installing {} hooks...", vanilla_tool.as_str());
            let include_perms = crate::config::load_config_snapshot().core.auto_approve;
            match vanilla_tool.try_setup_hooks(include_perms) {
                Ok(()) => {
                    println!(
                        "\nRestart {} to enable automatic message delivery.",
                        vanilla_tool.spec().label
                    );
                    println!("Then run: hcom start");
                }
                Err(error) if error.is_empty() => {
                    eprintln!(
                        "Failed to install hooks. Run: hcom hooks add {}",
                        vanilla_tool.as_str()
                    );
                }
                Err(error) => {
                    eprintln!(
                        "Failed to install {} hooks: {error}\nRun: hcom hooks add {}",
                        vanilla_tool.as_str(),
                        vanilla_tool.as_str()
                    );
                }
            }
            return Ok(1);
        }
    }

    let tool = if ctx.process_id.is_some() || vanilla_session_id.is_some() {
        ctx.tool.as_str()
    } else {
        "adhoc"
    };

    if explicit_name.is_none()
        && let Some(ref session_id) = vanilla_session_id
        && let Some(bound_name) = db.get_session_binding(session_id)?
    {
        // Only hcom writes session bindings, so a row keyed by this session's
        // own id is trusted identity evidence. Heal bindings created by older
        // versions before returning the existing row.
        if ctx.tool == crate::tool::Tool::Claude {
            db.mark_claude_session_validated(session_id, &bound_name)?;
        }
        println!("hcom already started for {bound_name}");
        return Ok(0);
    }

    // Resolve or generate name
    let name = if let Some(n) = explicit_name {
        n.to_string()
    } else {
        instance_names::generate_unique_name(db)?
    };

    // Remote instances are relay mirrors. Starting them remotely is intentionally
    // unsupported because the useful remote lifecycle operations are launch/resume/kill.
    if let Ok(Some(ref existing)) = db.get_instance_full(&name)
        && crate::instances::is_remote_instance(existing)
    {
        bail!("Remote start is not supported for '{name}'. Start it on the owning device instead.");
    }

    // Check if already exists and active (only for explicit names —
    // generate_unique_name creates a placeholder row we must skip past)
    if explicit_name.is_some()
        && let Ok(Some(existing)) = db.get_instance_full(&name)
        && existing.status != "stopped"
    {
        println!("hcom already started for {}", name);
        return Ok(0);
    }

    instance_binding::initialize_instance_in_position_file(
        db,
        &name,
        vanilla_session_id.as_deref(),
        None, // parent_session_id
        None, // parent_name
        None, // agent_id
        None, // transcript_path
        Some(tool),
        false, // background
        None,  // tag
        None,  // wait_timeout
        None,  // subagent_timeout
        None,  // hints
        None,  // cwd_override
    );

    if let Some(ref session_id) = vanilla_session_id {
        db.set_session_binding(session_id, &name)?;
        if ctx.tool == crate::tool::Tool::Claude {
            db.mark_claude_session_validated(session_id, &name)?;
        }
    }

    // Bind process if we have a process_id
    if let Some(ref process_id) = ctx.process_id
        && let Err(e) = db.set_process_binding(process_id, "", &name)
    {
        eprintln!("[hcom] warn: set_process_binding failed for {name}: {e}");
    }

    // Print bootstrap
    let hcom_config = HcomConfig::load(None).unwrap_or_else(|e| {
        eprintln!("[hcom] warn: config load failed, using defaults: {e}");
        let mut c = HcomConfig::default();
        c.normalize();
        c
    });

    let bootstrap_text = bootstrap::get_bootstrap(
        db,
        hcom_dir,
        &name,
        tool,
        false,
        ctx.is_launched,
        &ctx.notes,
        &hcom_config.tag,
        relay::is_relay_enabled(&hcom_config),
        None,
    );

    println!("[hcom:{}]", name);
    println!("{}", bootstrap_text);
    // Repeated deliberately: the header above sits on top of a long bootstrap, so
    // `hcom start | tail -n` shows none of it. A caller that cannot see its own
    // name re-runs start, which is one way duplicate identities appear.
    println!("[hcom:{}]", name);

    // Log
    db.log_event(
        "life",
        &name,
        &json!({
            "action": "started",
            "tool": tool,
            "name": name,
        }),
    )
    .ok();

    Ok(0)
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
