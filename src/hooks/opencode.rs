//! OpenCode hook handlers — argv-based lifecycle management (start, status, read, stop).

use std::time::Instant;

use serde_json::Value;

use crate::bootstrap;
use crate::db::HcomDb;
use crate::instance_binding;
use crate::instance_lifecycle as lifecycle;
use crate::instances;
use crate::log::{log_error, log_info};
use crate::shared::ST_LISTENING;
use crate::shared::context::HcomContext;

use super::common;
use super::common::finalize_session;

/// Extract `--flag value` from argv. Returns None if not found.
fn parse_flag(argv: &[String], flag: &str) -> Option<String> {
    argv.iter()
        .position(|a| a == flag)
        .and_then(|i| argv.get(i + 1))
        .cloned()
}

/// Check if a bare flag exists in argv (no value).
fn has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|a| a == flag)
}

/// Extract `--flag value` or `--flag=value` from argv.
fn parse_value_arg(argv: &[String], flags: &[&str]) -> Option<String> {
    for (idx, token) in argv.iter().enumerate() {
        for flag in flags {
            if token == flag {
                return argv.get(idx + 1).cloned();
            }
            let prefix = format!("{flag}=");
            if let Some(value) = token.strip_prefix(&prefix)
                && !value.is_empty()
            {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn parse_launch_model(raw: &str) -> Option<Value> {
    let (provider_id, model_id) = raw.split_once('/')?;
    if provider_id.is_empty() || model_id.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "providerID": provider_id,
        "modelID": model_id,
    }))
}

fn launch_agent_and_model_from_args(launch_args: Option<&str>) -> (Option<String>, Option<Value>) {
    let Some(raw_args) = launch_args.filter(|value| !value.is_empty()) else {
        return (None, None);
    };
    let argv: Vec<String> = match serde_json::from_str(raw_args) {
        Ok(args) => args,
        Err(_) => return (None, None),
    };

    let agent = parse_value_arg(&argv, &["--agent"]);
    let model =
        parse_value_arg(&argv, &["--model", "-m"]).and_then(|value| parse_launch_model(&value));
    (agent, model)
}

fn launch_agent_and_model(db: &HcomDb, instance_name: &str) -> (Option<String>, Option<Value>) {
    db.get_instance_full(instance_name)
        .ok()
        .flatten()
        .map(|instance| launch_agent_and_model_from_args(instance.launch_args.as_deref()))
        .unwrap_or((None, None))
}

/// Upsert plugin notify endpoint in DB.
fn upsert_plugin_notify_endpoint(db: &HcomDb, instance_name: &str, port: u16) {
    if let Err(e) = db.upsert_notify_endpoint(instance_name, "plugin", port) {
        log_error(
            "native",
            "opencode.register_notify_fail",
            &format!(
                "Failed to register plugin notify port for {}: {}",
                instance_name, e
            ),
        );
    }
}

/// Send TCP wake to ALL of an instance's wake endpoints.
///
/// Used by status handler when instance becomes listening.
/// Wakes every registered wake kind (pty, hook, plugin, listen variants,
/// events_wait). The inject endpoint is excluded — it speaks RPC, not wake.
fn notify_all_endpoints(db: &HcomDb, instance_name: &str) {
    crate::notify::wake(db, instance_name, &[]);
}

fn instance_tool(db: &HcomDb, instance_name: &str) -> String {
    db.get_instance_full(instance_name)
        .ok()
        .flatten()
        .map(|instance| instance.tool)
        .filter(|tool| tool == "opencode" || tool == "kilo")
        .unwrap_or_else(|| "opencode".to_string())
}

/// Get the OpenCode-family SQLite database path for an instance tool.
///
/// Both apps use XDG_DATA_HOME. Kilo additionally supports `KILO_DB`, which
/// may be absolute or relative to Kilo's data directory.
fn get_family_db_path(tool: &str) -> Option<String> {
    crate::runtime_env::opencode_family_db_path(tool)
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
fn get_opencode_db_path() -> Option<String> {
    get_family_db_path("opencode")
}

/// Handle opencode-start: bind session to process, set listening status.
///
/// Called by OpenCode plugin on session.created event.
/// Expects: hcom opencode-start --session-id <id> [--notify-port <port>]
///
/// Returns JSON: {"name": "<instance>", "session_id": "<id>", "bootstrap": "..."}
fn handle_start(ctx: &HcomContext, db: &HcomDb, argv: &[String]) -> (i32, String) {
    let session_id = match parse_flag(argv, "--session-id") {
        Some(sid) => sid,
        None => return (0, r#"{"error":"Missing --session-id"}"#.to_string()),
    };

    let notify_port: Option<u16> = parse_flag(argv, "--notify-port").and_then(|s| s.parse().ok());

    let process_id = match &ctx.process_id {
        Some(pid) => pid.clone(),
        None => return (0, r#"{"error":"HCOM_PROCESS_ID not set"}"#.to_string()),
    };

    // Re-binding detection: session already bound (compaction or reconnect)
    if let Ok(Some(existing_name)) = db.get_session_binding(&session_id) {
        let tool = instance_tool(db, &existing_name);
        let mut rebind_updates = serde_json::Map::new();
        rebind_updates.insert("name_announced".into(), serde_json::json!(false));
        rebind_updates.insert("session_id".into(), serde_json::json!(&session_id));

        if let Some(db_path) = get_family_db_path(&tool) {
            rebind_updates.insert("transcript_path".into(), serde_json::json!(db_path));
        }

        instances::update_instance_position(db, &existing_name, &rebind_updates);
        lifecycle::set_status(
            db,
            &existing_name,
            ST_LISTENING,
            "start",
            Default::default(),
        );

        let hcom_config = crate::config::HcomConfig::load(None).unwrap_or_default();
        let bootstrap_text = bootstrap::get_bootstrap(
            db,
            &ctx.hcom_dir,
            &existing_name,
            &tool,
            ctx.is_background,
            ctx.is_launched,
            &ctx.notes,
            &hcom_config.tag,
            crate::relay::is_relay_enabled(&hcom_config),
            ctx.background_name.as_deref(),
        );

        if let Some(port) = notify_port {
            upsert_plugin_notify_endpoint(db, &existing_name, port);
        }

        log_info(
            "hooks",
            "opencode-start.rebind",
            &format!("instance={} session_id={}", existing_name, session_id),
        );

        let (launch_agent, launch_model) = launch_agent_and_model(db, &existing_name);
        let mut result = serde_json::json!({
            "name": existing_name,
            "session_id": session_id,
        });
        result["bootstrap"] = Value::String(bootstrap_text);
        if let Some(agent) = launch_agent {
            result["agent"] = Value::String(agent);
        }
        if let Some(model) = launch_model {
            result["model"] = model;
        }
        return (0, serde_json::to_string(&result).unwrap_or_default());
    }

    // Normal binding path
    let instance_name =
        match instance_binding::bind_session_to_process(db, &session_id, Some(&process_id)) {
            Some(name) => name,
            None => {
                return (
                    0,
                    r#"{"error":"No instance bound to this process"}"#.to_string(),
                );
            }
        };
    let tool = instance_tool(db, &instance_name);

    // Rebind session and initialize
    if let Err(e) = db.rebind_instance_session(&instance_name, &session_id) {
        log_error(
            "hooks",
            "hook.error",
            &format!("hook=opencode-start op=rebind_session err={}", e),
        );
    }

    // Initialize last_event_id BEFORE set_status() — set_status triggers
    // `crate::notify::wake` which TCP-wakes the plugin's
    // deliverPendingToIdle(). If last_event_id is still 0, ALL historical
    // events get delivered.
    if let Ok(Some(existing)) = db.get_instance_full(&instance_name)
        && existing.last_event_id == 0
    {
        let launch_event_id: Option<i64> = std::env::var("HCOM_LAUNCH_EVENT_ID")
            .ok()
            .and_then(|s| s.parse().ok());
        let current_max = db.get_last_event_id();
        let new_id = match launch_event_id {
            Some(lei) if lei <= current_max => lei,
            _ => current_max,
        };
        let mut id_updates = serde_json::Map::new();
        id_updates.insert("last_event_id".into(), serde_json::json!(new_id));
        instances::update_instance_position(db, &instance_name, &id_updates);
    }

    lifecycle::set_status(
        db,
        &instance_name,
        ST_LISTENING,
        "start",
        Default::default(),
    );

    // Capture launch context (preserves pane_id/terminal_preset from Rust PTY)
    instance_binding::capture_and_store_launch_context(db, &instance_name);

    // Update instance position
    let mut updates = serde_json::Map::new();
    updates.insert("session_id".into(), serde_json::json!(&session_id));
    if let Some(db_path) = get_family_db_path(&tool) {
        updates.insert("transcript_path".into(), serde_json::json!(db_path));
    }
    if !ctx.cwd.as_os_str().is_empty() {
        updates.insert(
            "directory".into(),
            serde_json::json!(ctx.cwd.to_string_lossy()),
        );
    }
    instances::update_instance_position(db, &instance_name, &updates);

    // Register TCP notify endpoint
    if let Some(port) = notify_port {
        upsert_plugin_notify_endpoint(db, &instance_name, port);
    }

    // Build bootstrap text
    let tag = db
        .get_instance_full(&instance_name)
        .ok()
        .flatten()
        .and_then(|d| d.tag.clone())
        .unwrap_or_default();

    let hcom_config = crate::config::HcomConfig::load(None).unwrap_or_default();
    let relay_enabled = crate::relay::is_relay_enabled(&hcom_config);
    // Use config tag as fallback when instance has no tag
    let effective_tag = if tag.is_empty() {
        &hcom_config.tag
    } else {
        &tag
    };
    let bootstrap_text = bootstrap::get_bootstrap(
        db,
        &ctx.hcom_dir,
        &instance_name,
        &tool,
        ctx.is_background,
        ctx.is_launched,
        &ctx.notes,
        effective_tag,
        relay_enabled,
        ctx.background_name.as_deref(),
    );

    // Auto-spawn relay-worker now that an instance is active
    crate::relay::worker::ensure_worker(true);

    let (launch_agent, launch_model) = launch_agent_and_model(db, &instance_name);
    let mut response = serde_json::json!({
        "name": instance_name,
        "session_id": session_id,
    });
    response["bootstrap"] = Value::String(bootstrap_text);
    if let Some(agent) = launch_agent {
        response["agent"] = Value::String(agent);
    }
    if let Some(model) = launch_model {
        response["model"] = model;
    }
    (0, serde_json::to_string(&response).unwrap_or_default())
}

/// Handle opencode-status: update instance status.
///
/// Called by OpenCode plugin on session.status and session.idle events.
/// Expects: hcom opencode-status --name <name> --status <status> [--context <ctx>] [--detail <d>]
fn handle_status(db: &HcomDb, argv: &[String]) -> (i32, String) {
    let name = match parse_flag(argv, "--name") {
        Some(n) => n,
        None => return (0, r#"{"error":"Missing --name or --status"}"#.to_string()),
    };
    let status = match parse_flag(argv, "--status") {
        Some(s) => s,
        None => return (0, r#"{"error":"Missing --name or --status"}"#.to_string()),
    };

    let context = parse_flag(argv, "--context").unwrap_or_default();
    let detail = parse_flag(argv, "--detail").unwrap_or_default();

    lifecycle::set_status(
        db,
        &name,
        &status,
        &context,
        lifecycle::StatusUpdate {
            detail: &detail,
            ..Default::default()
        },
    );

    // Wake delivery thread if instance is now listening
    if status == ST_LISTENING {
        notify_all_endpoints(db, &name);
    }

    (0, r#"{"ok":true}"#.to_string())
}

/// Handle opencode-read: fetch pending messages, check, format, or ack.
///
/// Modes:
/// - Default: Return pending messages as JSON array (does NOT advance cursor)
/// - --format: Return formatted text (same format as Claude/Gemini delivery)
/// - --check: Return "true" or "false" string
/// - --ack --up-to <id>: Advance cursor to explicit event_id
/// - --ack (no --up-to): Advance cursor to max pending event_id (legacy)
fn handle_read(db: &HcomDb, argv: &[String]) -> (i32, String) {
    let name = match parse_flag(argv, "--name") {
        Some(n) => n,
        None => return (0, r#"{"error":"Missing --name"}"#.to_string()),
    };

    let format_mode = has_flag(argv, "--format");
    let check_mode = has_flag(argv, "--check");
    let ack_mode = has_flag(argv, "--ack");

    // Fetch unread messages (without advancing cursor)
    let raw_messages = db.get_unread_messages(&name);

    // Convert db::Message to serde_json::Value
    let messages: Vec<Value> = raw_messages.iter().map(common::message_to_value).collect();

    if format_mode {
        if messages.is_empty() {
            return (0, String::new());
        }
        let deliver = common::limit_delivery_messages(&messages);
        let formatted = common::format_messages_json_for_instance(db, &deliver, &name);
        return (0, formatted);
    }

    if ack_mode {
        let up_to = parse_flag(argv, "--up-to");
        if let Some(up_to_str) = up_to {
            // Explicit ack position
            let ack_id: i64 = match up_to_str.parse() {
                Ok(id) => id,
                Err(_) => {
                    return (
                        0,
                        serde_json::json!({"error": format!("Invalid --up-to: {}", up_to_str)})
                            .to_string(),
                    );
                }
            };
            let mut updates = serde_json::Map::new();
            updates.insert("last_event_id".into(), serde_json::json!(ack_id));
            instances::update_instance_position(db, &name, &updates);
            return (0, serde_json::json!({"acked_to": ack_id}).to_string());
        }
        // Legacy: ack all pending
        if messages.is_empty() {
            return (0, r#"{"acked":0}"#.to_string());
        }
        let last_id = messages
            .iter()
            .filter_map(|m| m.get("event_id").and_then(|v| v.as_i64()))
            .max()
            .unwrap_or(0);
        // Fallback: when all event_ids are 0, use db max
        let ack_id = if last_id > 0 {
            last_id
        } else {
            db.get_last_event_id()
        };
        if ack_id > 0 {
            let mut updates = serde_json::Map::new();
            updates.insert("last_event_id".into(), serde_json::json!(ack_id));
            instances::update_instance_position(db, &name, &updates);
        }
        return (0, serde_json::json!({"acked": messages.len()}).to_string());
    }

    if check_mode {
        return (
            0,
            if messages.is_empty() { "false" } else { "true" }.to_string(),
        );
    }

    // Default: return raw JSON array
    (
        0,
        serde_json::to_string(&messages).unwrap_or_else(|_| "[]".to_string()),
    )
}

/// Handle opencode-stop: finalize session and clean up instance.
///
/// Called by OpenCode plugin on session.deleted event.
/// Expects: hcom opencode-stop --name <name> [--reason <reason>]
fn handle_stop(db: &HcomDb, argv: &[String]) -> (i32, String) {
    let name = match parse_flag(argv, "--name") {
        Some(n) => n,
        None => return (0, r#"{"error":"Missing --name"}"#.to_string()),
    };
    let reason = parse_flag(argv, "--reason").unwrap_or_else(|| "unknown".to_string());

    finalize_session(db, &name, &reason, None);

    (0, r#"{"ok":true}"#.to_string())
}

/// Dispatch an OpenCode hook by name.
///
/// Returns (exit_code, stdout_output).
/// All OpenCode hooks return exit 0 (no blocking behavior).
pub fn dispatch_opencode_hook(hook_name: &str, argv: &[String]) -> (i32, String) {
    let start = Instant::now();

    // Build context
    let ctx = HcomContext::from_os();

    // Ensure hcom directories exist before opening DB.
    // On clean HOME/HCOM_DIR the DB parent dir won't exist yet.
    crate::paths::ensure_hcom_directories_at(&ctx.hcom_dir);

    // Open DB (includes schema migration/compat)
    let db = match HcomDb::open() {
        Ok(db) => db,
        Err(e) => {
            log_error(
                "hooks",
                "hook.error",
                &format!("hook={} op=db_open err={}", hook_name, e),
            );
            return (
                0,
                serde_json::json!({"error": format!("DB open failed: {}", e)}).to_string(),
            );
        }
    };

    // Pre-gate: non-participants with empty DB → exit 0, no output
    if !common::hook_gate_check(&ctx, &db) {
        return (0, String::new());
    }

    // Strip hook name from argv to get handler args
    // argv comes as: ["opencode-start", "--session-id", "abc", ...]
    let handler_argv: Vec<String> = if !argv.is_empty() && argv[0] == hook_name {
        argv[1..].to_vec()
    } else {
        argv.to_vec()
    };

    let handler_start = Instant::now();
    let hook_name_owned = hook_name.to_string();

    let (exit_code, output) = common::dispatch_with_panic_guard(
        "opencode",
        &hook_name_owned,
        (
            0,
            serde_json::json!({"error": "internal panic"}).to_string(),
        ),
        || match hook_name_owned.as_str() {
            "opencode-start" => handle_start(&ctx, &db, &handler_argv),
            "opencode-status" => handle_status(&db, &handler_argv),
            "opencode-read" => handle_read(&db, &handler_argv),
            "opencode-stop" => handle_stop(&db, &handler_argv),
            _ => (
                0,
                serde_json::json!({"error": format!("Unknown OpenCode hook: {}", hook_name_owned)})
                    .to_string(),
            ),
        },
    );

    let handler_ms = handler_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    log_info(
        "hooks",
        "opencode.dispatch.timing",
        &format!(
            "hook={} handler_ms={:.2} total_ms={:.2} exit_code={}",
            hook_name, handler_ms, total_ms, exit_code
        ),
    );

    (exit_code, output)
}

/// Embedded hcom.ts plugin source (compiled into the binary).
pub const PLUGIN_SOURCE: &str = include_str!("../opencode_plugin/hcom.ts");

const PLUGIN_FILENAME: &str = "hcom.ts";

fn current_home_dir() -> std::path::PathBuf {
    crate::runtime_env::user_home().unwrap_or_default()
}

/// Resolve the user config home (`XDG_CONFIG_HOME`, else `~/.config` on
/// Unix/macOS or `%APPDATA%` on Windows), falling back to `~/.config` if
/// unresolvable.
fn xdg_config_home() -> String {
    crate::runtime_env::user_config_home()
        .unwrap_or_else(|| current_home_dir().join(".config"))
        .to_string_lossy()
        .into_owned()
}

/// Get the canonical plugin install directory for an OpenCode-family app.
///
/// Uses the XDG global plugin dir in the default HOME-backed case, and a
/// project-local `.<app>/plugins/` dir when HCOM_DIR points at a project root.
fn plugin_dir_for_app(app: &str) -> std::path::PathBuf {
    let tool_root = crate::runtime_env::tool_config_root();
    let home = current_home_dir();
    if tool_root == home {
        std::path::PathBuf::from(xdg_config_home())
            .join(app)
            .join("plugins")
    } else {
        tool_root.join(format!(".{app}")).join("plugins")
    }
}

pub fn get_opencode_plugin_dir() -> std::path::PathBuf {
    plugin_dir_for_app("opencode")
}

/// Get the canonical install path for the hcom.ts plugin.
pub fn get_opencode_plugin_path() -> std::path::PathBuf {
    get_opencode_plugin_dir().join(PLUGIN_FILENAME)
}

pub fn get_kilo_plugin_path() -> std::path::PathBuf {
    plugin_dir_for_app("kilo").join(PLUGIN_FILENAME)
}

/// Scan all directories where hcom.ts plugin might exist.
///
/// Checks both plugin/ and plugins/ under the XDG global location and the
/// project-local tool_config_root() location when applicable.
fn scan_plugin_dirs(app: &str) -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    let xdg_base = std::path::PathBuf::from(xdg_config_home()).join(app);
    candidates.push(xdg_base.join("plugin"));
    candidates.push(xdg_base.join("plugins"));

    let config_dir_env = if app == "kilo" {
        "KILO_CONFIG_DIR"
    } else {
        "OPENCODE_CONFIG_DIR"
    };
    if let Ok(custom_dir) = std::env::var(config_dir_env) {
        let custom_base = std::path::PathBuf::from(custom_dir);
        candidates.push(custom_base.join("plugin"));
        candidates.push(custom_base.join("plugins"));
    }

    let tool_root = crate::runtime_env::tool_config_root();
    let home = current_home_dir();
    if tool_root != home {
        let tool_base = tool_root.join(format!(".{app}"));
        candidates.push(tool_base.join("plugin"));
        candidates.push(tool_base.join("plugins"));
    }

    let mut deduped = Vec::new();
    for dir in candidates.into_iter().filter(|d| d.exists()) {
        if !deduped.contains(&dir) {
            deduped.push(dir);
        }
    }
    deduped
}

/// Check if hcom.ts plugin is installed in any plugin directory for an app.
fn verify_plugin_installed(app: &str) -> bool {
    if plugin_matches_source(&plugin_dir_for_app(app).join(PLUGIN_FILENAME)) {
        return true;
    }
    scan_plugin_dirs(app)
        .iter()
        .map(|d| d.join(PLUGIN_FILENAME))
        .any(|path| plugin_matches_source(&path))
}

pub fn verify_opencode_plugin_installed() -> bool {
    verify_plugin_installed("opencode")
}

pub fn verify_kilo_plugin_installed() -> bool {
    verify_plugin_installed("kilo")
}

/// Install the hcom.ts plugin to the canonical plugin directory.
///
/// Creates the canonical app plugin dir if needed.
/// Writes the embedded plugin source directly (no file copy needed).
fn install_plugin(app: &str) -> std::io::Result<bool> {
    let target_dir = plugin_dir_for_app(app);
    let target = target_dir.join(PLUGIN_FILENAME);

    std::fs::create_dir_all(&target_dir)?;

    // Remove stale symlinks before writing
    if target.is_symlink() || target.exists() {
        std::fs::remove_file(&target)?;
    }

    std::fs::write(&target, PLUGIN_SOURCE)?;
    Ok(true)
}

pub fn install_opencode_plugin() -> std::io::Result<bool> {
    install_plugin("opencode")
}

pub fn install_kilo_plugin() -> std::io::Result<bool> {
    install_plugin("kilo")
}

/// Remove hcom.ts from ALL plugin directories for an app.
///
/// Checks all candidate directories directly (without filtering by dir existence)
/// to avoid missing stale plugins when path resolution differs between install/remove.
fn remove_plugin(app: &str) -> std::io::Result<()> {
    let mut paths = vec![plugin_dir_for_app(app).join(PLUGIN_FILENAME)];

    // Build candidate paths from all known locations (skip dir-exists filter
    // that scan_plugin_dirs uses — a dir might not show as existing due to
    // mount/symlink differences but the file inside might still be reachable).
    let xdg_base = std::path::PathBuf::from(xdg_config_home()).join(app);
    for sub in &["plugin", "plugins"] {
        let p = xdg_base.join(sub).join(PLUGIN_FILENAME);
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    let config_dir_env = if app == "kilo" {
        "KILO_CONFIG_DIR"
    } else {
        "OPENCODE_CONFIG_DIR"
    };
    if let Ok(custom_dir) = std::env::var(config_dir_env) {
        let custom_base = std::path::PathBuf::from(custom_dir);
        for sub in &["plugin", "plugins"] {
            let p = custom_base.join(sub).join(PLUGIN_FILENAME);
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    let tool_root = crate::runtime_env::tool_config_root();
    let home = current_home_dir();
    if tool_root != home {
        let tool_base = tool_root.join(format!(".{app}"));
        for sub in &["plugin", "plugins"] {
            let p = tool_base.join(sub).join(PLUGIN_FILENAME);
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    for p in paths {
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

pub fn remove_opencode_plugin() -> std::io::Result<()> {
    remove_plugin("opencode")
}

pub fn remove_kilo_plugin() -> std::io::Result<()> {
    remove_plugin("kilo")
}

fn plugin_matches_source(path: &std::path::Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(content) => content == PLUGIN_SOURCE,
        Err(_) => false,
    }
}

/// Ensure the hcom.ts plugin is installed and up to date.
///
/// Used by the launcher for auto-install on first launch.
pub fn ensure_plugin_installed(app: &str) -> std::io::Result<bool> {
    if verify_plugin_installed(app) {
        return Ok(true);
    }
    install_plugin(app)
}

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
