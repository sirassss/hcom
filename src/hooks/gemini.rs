//! Gemini CLI hook handlers for hcom.
//!
//! Lifecycle: SessionStart → BeforeAgent → [BeforeTool → AfterTool]* → AfterAgent → SessionEnd

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::bootstrap;
use crate::db::{HcomDb, InstanceRow};
use crate::hooks::common;
use crate::hooks::{HookPayload, HookResult};
use crate::instance_binding;
use crate::instance_lifecycle as lifecycle;
use crate::instances;
use crate::log;
use crate::shared::context::HcomContext;
use crate::shared::{ST_BLOCKED, ST_LISTENING};

/// Derive Gemini CLI transcript path from session_id.
///
/// Gemini's ChatRecordingService isn't initialized at SessionStart, so
/// transcript_path is empty. This derives it from session_id by searching
/// the Gemini chats directory.
///
pub fn derive_gemini_transcript_path(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }

    let session_prefix = session_id.split('-').next().unwrap_or("");
    if session_prefix.is_empty() {
        return None;
    }

    let gemini_base = crate::runtime_env::gemini_family_config_dir();
    let gemini_tmp = gemini_base.join("tmp");
    if !gemini_tmp.exists() {
        return None;
    }

    // Search for session-*-{prefix}*.json recursively in chats/ dirs
    let pattern = format!("session-*-{}*.json", session_prefix);
    find_newest_matching_file(&gemini_tmp, &pattern)
}

/// Recursively search for files matching a glob pattern under chats/ directories.
/// Returns the most recently modified match.
fn find_newest_matching_file(base: &Path, pattern: &str) -> Option<String> {
    let mut best: Option<(String, std::time::SystemTime)> = None;
    let mut dirs_to_visit = vec![base.to_path_buf()];

    while let Some(dir) = dirs_to_visit.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            dirs_to_visit.push(path.clone());
            if path.file_name().is_some_and(|n| n == "chats") {
                check_chat_dir(&path, pattern, &mut best);
            }
        }
    }

    best.map(|(path, _)| path)
}

/// Check a chats/ directory for matching session files, updating `best` if newer.
fn check_chat_dir(
    chat_dir: &Path,
    pattern: &str,
    best: &mut Option<(String, std::time::SystemTime)>,
) {
    let entries = match std::fs::read_dir(chat_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !matches_session_pattern(name, pattern) {
            continue;
        }
        let mtime = match path.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let dominated = best.as_ref().is_some_and(|(_, bt)| mtime <= *bt);
        if !dominated {
            *best = Some((path.to_string_lossy().to_string(), mtime));
        }
    }
}

/// Check if a filename matches the session pattern "session-*-{prefix}*.json".
fn matches_session_pattern(filename: &str, pattern: &str) -> bool {
    // pattern is "session-*-{prefix}*.json"
    // Extract prefix from pattern
    let prefix = pattern
        .strip_prefix("session-*-")
        .and_then(|s| s.strip_suffix("*.json"))
        .unwrap_or("");

    filename.starts_with("session-") && filename.ends_with(".json") && filename.contains(prefix)
}

/// Try to capture transcript_path from payload if not already set.
///
/// Gemini's ChatRecordingService isn't initialized at SessionStart,
/// so transcript_path is empty. It becomes available at BeforeAgent/AfterAgent.
fn try_capture_transcript_path(db: &HcomDb, instance_name: &str, payload: &HookPayload) {
    let instance = match db.get_instance_full(instance_name) {
        Ok(Some(data)) => data,
        _ => return,
    };

    // Re-derive if stored path doesn't exist (e.g. after kill/resume, Gemini
    // creates a new transcript file with a different timestamp prefix).
    if !instance.transcript_path.is_empty()
        && std::path::Path::new(&instance.transcript_path).exists()
    {
        return;
    }

    let transcript_path = payload.transcript_path.clone().or_else(|| {
        let session_id = instance.session_id.as_deref().unwrap_or("");
        if session_id.is_empty() {
            None
        } else {
            let derived = derive_gemini_transcript_path(session_id).unwrap_or_default();
            if derived.is_empty() {
                None
            } else {
                Some(derived)
            }
        }
    });

    if let Some(tp) = transcript_path {
        let mut updates = serde_json::Map::new();
        updates.insert("transcript_path".into(), Value::String(tp));
        instances::update_instance_position(db, instance_name, &updates);
    }
}

/// Resolve instance using process binding or session binding.
fn resolve_instance_gemini(db: &HcomDb, payload: &HookPayload) -> Option<InstanceRow> {
    instance_binding::resolve_instance_from_binding(db, payload.session_id.as_deref(), None)
}

/// Resolve the directory value to record on this hook fire.
///
/// For antigravity, `ctx.cwd` is agy's internal config dir (~/.gemini/...), not
/// the user's project. Prefer agy's own `workspacePaths[0]` from stdin instead.
/// Returns None when no reliable directory is available — callers should leave
/// the existing `instances.directory` field untouched rather than clobber it
/// with agy's internal cwd (otherwise resume launches in the wrong folder).
fn resolve_hook_directory(payload: &HookPayload, ctx: &HcomContext) -> Option<String> {
    if payload.tool == "antigravity" {
        return payload
            .raw
            .get("workspacePaths")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
    }
    Some(ctx.cwd.to_string_lossy().to_string())
}

/// Handle Gemini SessionStart hook.
///
/// HCOM-launched: bind session_id, inject bootstrap if not announced.
/// Vanilla: show hcom hint.
fn handle_sessionstart(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    if ctx.process_id.is_none() {
        // Vanilla instance - show hint
        return HookResult::Allow {
            additional_context: Some(format!(
                "[hcom available - run '{} start' to participate]",
                crate::runtime_env::build_hcom_command()
            )),
            system_message: None,
            delivery_ack: None,
        };
    }

    let session_id = match payload.session_id.as_deref() {
        Some(sid) => sid,
        None => return hook_noop(),
    };

    let instance_name =
        instance_binding::bind_session_to_process(db, session_id, ctx.process_id.as_deref());

    log::log_info(
        "hooks",
        "gemini.sessionstart.bind",
        &format!(
            "instance={:?} session_id={} process_id={:?}",
            instance_name, session_id, ctx.process_id,
        ),
    );

    // Orphaned PTY: process_id exists but no binding (e.g., after session clear)
    let instance_name = match instance_name {
        Some(name) => name,
        None => {
            if let Some(ref pid) = ctx.process_id {
                // Preserve the actual tool identity: agy reuses the gemini hook
                // family but must not be recorded as "gemini" in the instance row.
                let orphan_tool = if ctx.tool == crate::tool::Tool::Antigravity {
                    "antigravity"
                } else {
                    "gemini"
                };
                match instance_binding::create_orphaned_pty_identity(
                    db,
                    session_id,
                    Some(pid.as_str()),
                    orphan_tool,
                ) {
                    Some(name) => {
                        log::log_info(
                            "hooks",
                            "gemini.sessionstart.orphan_created",
                            &format!("instance={} process_id={} tool={}", name, pid, orphan_tool),
                        );
                        name
                    }
                    None => return hook_noop(),
                }
            } else {
                return hook_noop();
            }
        }
    };

    let _ = db.rebind_instance_session(&instance_name, session_id);

    // agy fires PreInvocation before every model call, not just at process start.
    // Once the instance is initialized on the same session, skip the setup
    // side-effects below but refresh its bootstrap: agy's ephemeralMessage is
    // not persisted in the conversation trajectory.
    //
    // re-running set_status("listening","start") mid-turn would briefly clobber an
    // in-progress delivery status (e.g. "active:deliver:sender") set by the previous
    // turn boundary.
    let instance = db.get_instance_full(&instance_name).ok().flatten();
    let already_bootstrapped = instance.as_ref().is_some_and(|inst| {
        inst.name_announced != 0 && inst.session_id.as_deref() == Some(session_id)
    });
    if already_bootstrapped {
        if let Some(inst) = instance.as_ref()
            && bootstrap::is_antigravity_tool(&inst.tool)
        {
            let tag = inst.tag.as_deref().unwrap_or("");
            let hcom_config = crate::config::HcomConfig::load(None).unwrap_or_default();
            let recurring = bootstrap::get_bootstrap(
                db,
                &ctx.hcom_dir,
                &instance_name,
                &inst.tool,
                ctx.is_background,
                ctx.is_launched,
                &ctx.notes,
                tag,
                crate::relay::is_relay_enabled(&hcom_config),
                ctx.background_name.as_deref(),
            );
            return HookResult::Allow {
                additional_context: Some(recurring),
                system_message: None,
                delivery_ack: None,
            };
        }
        return hook_noop();
    }

    // Capture launch context
    instance_binding::capture_and_store_launch_context(db, &instance_name);

    let mut updates = serde_json::Map::new();
    if let Some(dir) = resolve_hook_directory(payload, ctx) {
        updates.insert("directory".into(), Value::String(dir));
    }
    if let Some(ref tp) = payload.transcript_path {
        updates.insert("transcript_path".into(), Value::String(tp.clone()));
    }
    if !updates.is_empty() {
        instances::update_instance_position(db, &instance_name, &updates);
    }
    lifecycle::set_status(
        db,
        &instance_name,
        ST_LISTENING,
        "start",
        Default::default(),
    );

    crate::runtime_env::set_terminal_title(&instance_name);

    // Auto-spawn relay-worker now that an instance is active
    crate::relay::worker::ensure_worker(true);

    // Gemini: SessionStart additionalContext is hidden after /clear — bootstrap stays in BeforeAgent.
    // Antigravity: initial bootstrap path (agy ignores GEMINI_SYSTEM_MD).
    // Later PreInvocation hooks take the recurring path above.
    if let Ok(Some(inst)) = db.get_instance_full(&instance_name)
        && bootstrap::is_antigravity_tool(&inst.tool)
        && let Some(bootstrap) =
            common::inject_bootstrap_once(db, ctx, &instance_name, &inst, &inst.tool)
    {
        return HookResult::Allow {
            additional_context: Some(bootstrap),
            system_message: None,
            delivery_ack: None,
        };
    }

    hook_noop()
}

/// Handle BeforeAgent hook - fires after user submits prompt.
///
/// Fallback bootstrap if SessionStart injection failed.
/// Also delivers pending messages and binds session_id for fresh instances.
fn handle_beforeagent(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_instance_gemini(db, payload) {
        Some(inst) => inst,
        None => return hook_noop(),
    };

    let instance_name = &instance.name;

    // Keep directory current — but for agy, prefer workspacePaths over the
    // hook subprocess cwd (which is agy's internal config dir, not the project).
    if let Some(dir) = resolve_hook_directory(payload, ctx) {
        let mut dir_updates = serde_json::Map::new();
        dir_updates.insert("directory".into(), Value::String(dir));
        instances::update_instance_position(db, instance_name, &dir_updates);
    }

    // Bind session_id if instance doesn't have one (fresh instance after /clear)
    if instance.session_id.is_none()
        && let Some(ref sid) = payload.session_id
    {
        log::log_info(
            "hooks",
            "gemini.beforeagent.bind_session",
            &format!("instance={} session_id={}", instance_name, sid),
        );
        let mut sid_updates = serde_json::Map::new();
        sid_updates.insert("session_id".into(), Value::String(sid.clone()));
        instances::update_instance_position(db, instance_name, &sid_updates);
        if let Err(e) = db.rebind_session(sid, instance_name) {
            log::log_warn(
                "hooks",
                "gemini.rebind_failed",
                &format!("rebind_session failed for {instance_name}: {e}"),
            );
        }
        if let Some(ref pid) = ctx.process_id
            && let Err(e) = db.set_process_binding(pid, sid, instance_name)
        {
            log::log_warn(
                "hooks",
                "gemini.process_binding_failed",
                &format!("set_process_binding failed for {instance_name}: {e}"),
            );
        }
    }

    try_capture_transcript_path(db, instance_name, payload);

    let is_agy = bootstrap::is_antigravity_tool(&instance.tool);
    let assembled = common::assemble_gemini_family_lifecycle_outputs(
        db,
        ctx,
        &instance,
        is_agy,
        common::GeminiFamilyLifecycleOpts {
            allow_wake_no_pending: true,
            set_status_on_empty: true,
        },
    );

    if let Some(wake_only) = assembled.early_wake_context {
        return HookResult::Allow {
            additional_context: Some(wake_only),
            system_message: None,
            delivery_ack: None,
        };
    }

    if assembled.parts.is_empty() {
        return hook_noop();
    }

    let combined = assembled.parts.join("\n\n---\n\n");
    HookResult::Allow {
        additional_context: Some(combined),
        system_message: None,
        delivery_ack: assembled.delivery_ack,
    }
}

/// Handle AfterAgent hook - fires when agent turn completes.
fn handle_afteragent(db: &HcomDb, _ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_instance_gemini(db, payload) {
        Some(inst) => inst,
        None => return hook_noop(),
    };

    if payload.tool == "antigravity" || instance.tool == "antigravity" {
        // Antigravity PostInvocation fires after each model invocation, including
        // between tool calls. Stop/turn-end is the reliable idle boundary.
        return hook_noop();
    }

    lifecycle::set_status(db, &instance.name, ST_LISTENING, "", Default::default());
    common::notify_hook_instance_with_db(db, &instance.name);

    hook_noop()
}

/// Handle BeforeTool hook - fires before tool execution.
fn handle_beforetool(db: &HcomDb, _ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_instance_gemini(db, payload) {
        Some(inst) => inst,
        None => return hook_noop(),
    };

    let tool_name = if payload.tool_name.is_empty() {
        "unknown"
    } else {
        &payload.tool_name
    };
    let status_tool = if instance.tool.is_empty() {
        payload.tool.as_str()
    } else {
        instance.tool.as_str()
    };
    common::update_tool_status(
        db,
        &instance.name,
        status_tool,
        tool_name,
        &payload.tool_input,
    );

    hook_noop()
}

/// Handle AfterTool hook - fires after tool execution.
///
/// Bootstrap injection and message delivery via additionalContext.
fn handle_aftertool(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_instance_gemini(db, payload) {
        Some(inst) => inst,
        None => return hook_noop(),
    };

    let is_agy = bootstrap::is_antigravity_tool(&instance.tool);
    if is_agy {
        // Antigravity PostToolUse cannot inject model context. Preparing delivery here would
        // attach a delivery_ack that dispatch would commit even though serialization emits `{}`.
        return hook_noop();
    }

    let assembled = common::assemble_gemini_family_lifecycle_outputs(
        db,
        ctx,
        &instance,
        is_agy,
        common::GeminiFamilyLifecycleOpts {
            allow_wake_no_pending: false,
            set_status_on_empty: false,
        },
    );

    if assembled.parts.is_empty() {
        return hook_noop();
    }

    let combined = assembled.parts.join("\n\n---\n\n");
    HookResult::Allow {
        additional_context: Some(combined),
        system_message: None,
        delivery_ack: assembled.delivery_ack,
    }
}

/// Handle Notification hook - fires on approval prompts, etc.
fn handle_notification(db: &HcomDb, _ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_instance_gemini(db, payload) {
        Some(inst) => inst,
        None => return hook_noop(),
    };

    if payload.notification_type.as_deref() == Some("ToolPermission") {
        lifecycle::set_status(
            db,
            &instance.name,
            ST_BLOCKED,
            "approval",
            Default::default(),
        );
    }

    hook_noop()
}

/// Handle SessionEnd hook - fires when a session ends.
fn handle_sessionend(db: &HcomDb, _ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_instance_gemini(db, payload) {
        Some(inst) => inst,
        None => return hook_noop(),
    };

    let is_agy = payload.tool == "antigravity" || instance.tool == "antigravity";
    let reason = if is_agy {
        crate::hooks::antigravity::sessionend_reason(&payload.raw)
    } else {
        payload
            .raw
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };
    let turn_idle_skip =
        is_agy && crate::hooks::antigravity::stop_should_skip_soft_finalize(&payload.raw);

    if turn_idle_skip {
        lifecycle::set_status(db, &instance.name, ST_LISTENING, "", Default::default());
        common::notify_hook_instance_with_db(db, &instance.name);
        return hook_noop();
    }

    // agy always soft-finalizes: this `Stop` is a loop-end signal on a *live* process
    // (agy has no SessionEnd hook), so hard-deleting would strand a still-running agent
    // that's about to take another turn. Real teardown is the PTY exit. Other gemini
    // tools have a genuine SessionEnd == process death, so they hard-finalize.
    if is_agy {
        common::soft_finalize_session(db, &instance.name, &reason, None, false);
    } else {
        common::finalize_session(db, &instance.name, &reason, None);
    }

    hook_noop()
}

/// No-op hook result: allow with no additional context.
fn hook_noop() -> HookResult {
    HookResult::Allow {
        additional_context: None,
        system_message: None,
        delivery_ack: None,
    }
}

/// Gemini hook handler name → function dispatch.
fn get_handler(hook_name: &str) -> Option<fn(&HcomDb, &HcomContext, &HookPayload) -> HookResult> {
    match hook_name {
        "gemini-sessionstart" => Some(handle_sessionstart),
        "gemini-beforeagent" => Some(handle_beforeagent),
        "gemini-afteragent" => Some(handle_afteragent),
        "gemini-beforetool" => Some(handle_beforetool),
        "gemini-aftertool" => Some(handle_aftertool),
        "gemini-notification" => Some(handle_notification),
        "gemini-sessionend" => Some(handle_sessionend),
        _ => None,
    }
}

/// Serialize hook stdout JSON.
///
/// Gemini lifecycle hooks: `hookSpecificOutput.additionalContext`.
/// Antigravity PreInvocation/PostInvocation (mapped from gemini-sessionstart, -beforeagent,
/// -afteragent) use `injectSteps[*].ephemeralMessage` per agy hooks docs.
/// Antigravity PostToolUse (mapped from gemini-aftertool) cannot inject — must return `{}`,
/// so additional_context from those handlers is dropped.
/// Tool permission hooks: Antigravity uses flat `decision: deny` per agy-hooks.md.
fn serialize_hook_result(tool: &str, hook_name: &str, result: &HookResult) -> Option<Value> {
    let is_agy = tool == "antigravity";
    match result {
        // Note: system_message on Allow is unused for Gemini (not part of Gemini hook schema)
        HookResult::Allow {
            additional_context, ..
        } => {
            if let Some(ctx) = additional_context {
                if is_agy {
                    // agy hook events are PreInvocation / PostInvocation / PostToolUse / Stop.
                    // Only the Invocation events accept injectSteps.
                    let supports_inject = matches!(
                        hook_name,
                        "gemini-sessionstart" | "gemini-beforeagent" | "gemini-afteragent"
                    );
                    if !supports_inject {
                        // PostToolUse expects exactly `{}` — emitting anything else risks agy
                        // discarding sibling hooks' output in the same phase.
                        return Some(serde_json::json!({}));
                    }
                    return Some(serde_json::json!({
                        "injectSteps": [ { "ephemeralMessage": ctx } ]
                    }));
                }
                let event_name = match hook_name {
                    "gemini-sessionstart" => "SessionStart",
                    "gemini-beforeagent" => "BeforeAgent",
                    "gemini-afteragent" => "AfterAgent",
                    "gemini-beforetool" => "BeforeTool",
                    "gemini-aftertool" => "AfterTool",
                    "gemini-notification" => "Notification",
                    "gemini-sessionend" => "SessionEnd",
                    _ => hook_name,
                };
                Some(serde_json::json!({
                    "decision": "allow",
                    "hookSpecificOutput": {
                        "hookEventName": event_name,
                        "additionalContext": ctx,
                    }
                }))
            } else if is_agy {
                // agy noop: emit valid JSON rather than empty stdout. PreToolUse REQUIRES a
                // decision field — `{}` is interpreted as deny and breaks all tool calls.
                // Stop also requires a decision field.
                if matches!(hook_name, "gemini-beforetool" | "gemini-sessionend") {
                    Some(serde_json::json!({ "decision": "allow" }))
                } else {
                    Some(serde_json::json!({}))
                }
            } else {
                None
            }
        }
        HookResult::Block { reason, .. } => {
            if is_agy {
                Some(serde_json::json!({
                    "decision": "deny",
                    "reason": reason,
                }))
            } else {
                Some(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                }))
            }
        }
        HookResult::UpdateInput { updated_input } => {
            Some(serde_json::json!({ "updatedInput": updated_input }))
        }
    }
}

/// Whether stdin/context should route through Antigravity payload parsing.
///
/// Order: `ANTIGRAVITY_AGENT` env → exclude `GEMINI_CLI` gemini → Antigravity schema fallback.
pub(crate) fn detect_antigravity_payload(ctx: &HcomContext, stdin_json: &Value) -> (bool, bool) {
    if ctx.tool == crate::tool::Tool::Antigravity {
        return (true, false);
    }
    if ctx.tool == crate::tool::Tool::Gemini {
        return (false, false);
    }
    if stdin_json.get("toolCall").is_some() {
        return (true, true);
    }
    if stdin_json
        .get("conversationId")
        .and_then(|v| v.as_str())
        .is_some()
        && [
            "workspacePaths",
            "artifactDirectoryPath",
            "invocationNum",
            "executionNum",
            "fullyIdle",
            "terminationReason",
        ]
        .iter()
        .any(|key| stdin_json.get(*key).is_some())
    {
        return (true, true);
    }
    (false, false)
}

/// Main entry point for Gemini hooks — called by router.
///
/// Reads stdin JSON, builds HookPayload + HcomContext, dispatches to handler.
/// Prints JSON to stdout (additionalContext for Gemini to inject).
///
pub fn dispatch_gemini_hook(hook_name: &str) -> i32 {
    let start = Instant::now();

    // Build context from environment
    let ctx = HcomContext::from_os();

    // Read stdin JSON
    let stdin_json: Value = match serde_json::from_reader(std::io::stdin().lock()) {
        Ok(v) => v,
        Err(_) => Value::Object(Default::default()),
    };

    let (is_agy, agy_fallback) = detect_antigravity_payload(&ctx, &stdin_json);
    if agy_fallback {
        static AGY_FALLBACK_LOGGED: std::sync::Once = std::sync::Once::new();
        AGY_FALLBACK_LOGGED.call_once(|| {
            log::log_warn(
                "hooks",
                "gemini.agy_detect_fallback",
                "Antigravity payload detected via toolCall without ANTIGRAVITY_AGENT; set env at launch",
            );
        });
    }

    let payload = if is_agy {
        HookPayload::from_antigravity(stdin_json, hook_name)
    } else {
        HookPayload::from_gemini(stdin_json)
    };

    // Pre-gate: skip BeforeAgent for non-participants
    if !ctx.is_launched && hook_name == "gemini-beforeagent" {
        let sid = match payload.session_id.as_deref() {
            Some(sid) => sid,
            None => return 0,
        };
        // Quick DB check for session binding
        if let Ok(db) = HcomDb::open() {
            if db.get_session_binding(sid).ok().flatten().is_none() {
                return 0;
            }
        } else {
            return 0;
        }
    }

    // Ensure hcom directories exist
    let init_start = Instant::now();
    if !crate::paths::ensure_hcom_directories() {
        return 0;
    }
    let init_ms = init_start.elapsed().as_secs_f64() * 1000.0;

    // Open DB (includes schema migration/compat check)
    let db = match HcomDb::open() {
        Ok(db) => db,
        Err(e) => {
            log::log_error("hooks", "gemini.db.error", &format!("{}", e));
            return 0;
        }
    };

    // Pre-gate: non-participants with empty DB → exit 0, no output
    if !common::hook_gate_check(&ctx, &db) {
        return 0;
    }

    // Get handler
    let handler = match get_handler(hook_name) {
        Some(h) => h,
        None => {
            log::log_error(
                "hooks",
                "gemini.dispatch.unknown",
                &format!("Unknown Gemini hook: {}", hook_name),
            );
            return 0;
        }
    };

    // Execute handler
    let handler_start = Instant::now();
    let result = common::dispatch_with_panic_guard(
        "gemini",
        hook_name,
        HookResult::Allow {
            additional_context: None,
            system_message: None,
            delivery_ack: None,
        },
        || handler(&db, &ctx, &payload),
    );

    let handler_ms = handler_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;

    log::log_info(
        "hooks",
        "gemini.dispatch.timing",
        &format!(
            "hook={} init_ms={:.2} handler_ms={:.2} total_ms={:.2} exit_code={}",
            hook_name,
            init_ms,
            handler_ms,
            total_ms,
            result.exit_code()
        ),
    );

    // Output result JSON to stdout
    let exit_code = result.exit_code();
    let output_json = serialize_hook_result(&payload.tool, hook_name, &result);
    if let Some(json) = output_json {
        let mut stdout = std::io::stdout().lock();
        if serde_json::to_writer(&mut stdout, &json).is_ok()
            && stdout.flush().is_ok()
            && let HookResult::Allow {
                delivery_ack: Some(ack),
                ..
            } = &result
        {
            common::commit_delivery_ack(&db, ack);
        }
    }

    exit_code
}

/// Find an executable in PATH.
fn find_in_path(name: &str) -> Option<PathBuf> {
    crate::terminal::which_bin(name).map(PathBuf::from)
}

/// Minimum supported Gemini CLI version (hooksConfig.enabled schema).
pub const GEMINI_MIN_VERSION: (u32, u32, u32) = (0, 26, 0);

/// Get installed Gemini CLI version without subprocess.
///
/// Resolves the gemini binary symlink and reads version from package.json.
/// Returns (major, minor, patch) or None if not found/parseable.
///
pub fn get_gemini_version() -> Option<(u32, u32, u32)> {
    let gemini_path = find_in_path("gemini")?;
    let real_path = std::fs::canonicalize(&gemini_path).ok()?;

    // package.json is in same dir as dist/ for npm installs
    let mut package_json = real_path.parent()?.join("package.json");
    if !package_json.exists() {
        // Try parent (dist/index.js -> package.json at package root)
        package_json = real_path.parent()?.parent()?.join("package.json");
    }
    #[cfg(windows)]
    if !package_json.exists() {
        // npm on Windows installs a `gemini.cmd`/`.ps1` shim directly in the
        // global bin dir (e.g. `%APPDATA%\npm\`) rather than a symlink into
        // the package, so canonicalize() just resolves the shim itself. The
        // package always lives in that same dir's `node_modules\@google\gemini-cli`.
        package_json = real_path
            .parent()?
            .join("node_modules")
            .join("@google")
            .join("gemini-cli")
            .join("package.json");
    }
    if !package_json.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&package_json).ok()?;
    let data: serde_json::Value = serde_json::from_str(&content).ok()?;
    let version_str = data.get("version")?.as_str()?;
    let parts: Vec<&str> = version_str.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        // Handle versions like "0.24.0-beta.1"
        let patch_str = parts[2].split('-').next()?;
        let patch = patch_str.parse().ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}

/// Check if installed Gemini version supports hcom hooks (>= 0.26.0).
///
/// Returns True if version detected and >= 0.26.0, or if version can't be
/// detected (optimistic fallback). False only if version detected AND too old.
pub fn is_gemini_version_supported() -> bool {
    match get_gemini_version() {
        Some(v) => v >= GEMINI_MIN_VERSION,
        None => true, // Can't determine — allow optimistically
    }
}

/// Safe hcom commands for Gemini auto-approval permission patterns.
use super::common::SAFE_HCOM_COMMANDS;

/// Hook configuration: (hook_type, matcher, command_suffix, timeout, description).
const GEMINI_HOOK_CONFIGS: &[(&str, &str, &str, u32, &str)] = &[
    (
        "SessionStart",
        "*",
        "gemini-sessionstart",
        5000,
        "Connect to hcom network",
    ),
    (
        "BeforeAgent",
        "*",
        "gemini-beforeagent",
        5000,
        "Deliver pending messages",
    ),
    (
        "AfterAgent",
        "*",
        "gemini-afteragent",
        5000,
        "Signal ready for messages",
    ),
    (
        "BeforeTool",
        ".*",
        "gemini-beforetool",
        5000,
        "Track tool execution",
    ),
    (
        "AfterTool",
        ".*",
        "gemini-aftertool",
        5000,
        "Deliver messages after tools",
    ),
    (
        "Notification",
        "ToolPermission",
        "gemini-notification",
        5000,
        "Track approval prompts",
    ),
    (
        "SessionEnd",
        "*",
        "gemini-sessionend",
        5000,
        "Disconnect from hcom",
    ),
];

/// Build all legacy permission patterns (both hcom and uvx hcom) for removal from tools.allowed.
fn build_all_permission_patterns() -> Vec<String> {
    let mut patterns = Vec::new();
    for prefix in &["hcom", "uvx hcom"] {
        for cmd in SAFE_HCOM_COMMANDS {
            patterns.push(format!("run_shell_command({} {})", prefix, cmd));
        }
    }
    patterns
}

fn gemini_config_dir() -> PathBuf {
    crate::runtime_env::gemini_family_config_dir()
}

/// Get path to Gemini policies directory.
///
/// Respects GEMINI_CLI_HOME env var, then falls back to:
/// If HCOM_DIR is set (sandbox), uses HCOM_DIR parent.
/// Otherwise uses global (~/.gemini/policies/).
fn get_gemini_policies_path() -> PathBuf {
    gemini_config_dir().join("policies")
}

/// Build policy TOML content for hcom.toml.
///
/// Uses commandPrefix array to allow all safe hcom commands in a single rule.
/// Matches the Codex pattern of a separate, self-contained permission file.
fn build_gemini_policy() -> String {
    let prefix = crate::runtime_env::build_hcom_command();
    let command_prefixes: Vec<String> = SAFE_HCOM_COMMANDS
        .iter()
        .map(|cmd| format!("  \"{} {}\"", prefix, cmd))
        .collect();

    format!(
        "# hcom integration - auto-approve safe commands\n\
         [[rule]]\n\
         toolName = \"run_shell_command\"\n\
         commandPrefix = [\n\
         {},\n\
         ]\n\
         decision = \"allow\"\n\
         priority = 300\n",
        command_prefixes.join(",\n")
    )
}

/// Set up Gemini policy file for auto-approval.
fn setup_gemini_policy() -> bool {
    let policies_dir = get_gemini_policies_path();
    let policy_file = policies_dir.join("hcom.toml");
    let policy_content = build_gemini_policy();

    // Check if already configured correctly
    if policy_file.exists()
        && let Ok(existing) = std::fs::read_to_string(&policy_file)
        && existing == policy_content
    {
        return true;
    }

    let _ = std::fs::create_dir_all(&policies_dir);
    crate::paths::atomic_write(&policy_file, &policy_content)
}

/// Remove hcom policy file.
fn remove_gemini_policy() -> bool {
    let policy_file = get_gemini_policies_path().join("hcom.toml");
    if policy_file.exists() {
        std::fs::remove_file(&policy_file).is_ok()
    } else {
        true
    }
}

/// Remove policy from a specific policies directory path.
fn remove_policy_from_path(policies_dir: &Path) -> bool {
    let policy_file = policies_dir.join("hcom.toml");
    if policy_file.exists() {
        std::fs::remove_file(&policy_file).is_ok()
    } else {
        true
    }
}

/// Get path to Gemini settings file.
///
/// Respects GEMINI_CLI_HOME env var, then falls back to:
/// If HCOM_DIR is set (sandbox), uses HCOM_DIR parent.
/// Otherwise uses global (~/.gemini/settings.json).
pub fn get_gemini_settings_path() -> PathBuf {
    gemini_config_dir().join("settings.json")
}

/// Load Gemini settings from JSON file.
fn load_gemini_settings(path: &Path) -> Option<serde_json::Map<String, Value>> {
    let content = std::fs::read_to_string(path).ok()?;
    let val: Value = serde_json::from_str(&content).ok()?;
    val.as_object().cloned()
}

/// Check if a hook dict is an hcom hook.
fn is_hcom_hook(hook: &Value) -> bool {
    let command = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let name = hook.get("name").and_then(|v| v.as_str()).unwrap_or("");
    // Check for hcom-related patterns
    command.contains("hcom")
        || name.contains("hcom-")
        || command.contains("${HCOM")
        || command.contains("$HCOM")
}

/// Set hooksConfig.enabled = true and clean up legacy hooks.enabled.
fn set_hooks_enabled(settings: &mut serde_json::Map<String, Value>) {
    // Ensure hooksConfig exists and set enabled
    if !settings.contains_key("hooksConfig") {
        settings.insert("hooksConfig".into(), serde_json::json!({}));
    }
    if let Some(hc) = settings
        .get_mut("hooksConfig")
        .and_then(|v| v.as_object_mut())
    {
        hc.insert("enabled".into(), Value::Bool(true));
    }

    // Clean up legacy hooks.enabled
    if let Some(hooks) = settings.get_mut("hooks").and_then(|v| v.as_object_mut())
        && hooks.get("enabled").and_then(|v| v.as_bool()).is_some()
    {
        hooks.remove("enabled");
        if hooks.is_empty() {
            settings.remove("hooks");
        }
    }
}

/// Check if hooksConfig.enabled is set.
fn is_hooks_enabled(settings: &serde_json::Map<String, Value>) -> bool {
    settings
        .get("hooksConfig")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        == Some(true)
}

/// Remove hcom hooks from Gemini settings dict (in-place).
///
/// Only removes hcom-specific hooks, preserving user hooks.
fn remove_hcom_hooks_from_settings(settings: &mut serde_json::Map<String, Value>) {
    if let Some(hooks_val) = settings.get_mut("hooks")
        && let Some(hooks) = hooks_val.as_object_mut()
    {
        let hook_types: Vec<String> = hooks.keys().cloned().collect();
        for hook_type in hook_types {
            if let Some(matchers) = hooks.get_mut(&hook_type).and_then(|v| v.as_array_mut()) {
                let mut updated = Vec::new();
                for matcher in matchers.iter() {
                    if let Some(matcher_obj) = matcher.as_object() {
                        if let Some(hook_list) = matcher_obj.get("hooks").and_then(|v| v.as_array())
                        {
                            let non_hcom: Vec<Value> = hook_list
                                .iter()
                                .filter(|h| !is_hcom_hook(h))
                                .cloned()
                                .collect();
                            if !non_hcom.is_empty() {
                                let mut new_matcher = matcher_obj.clone();
                                new_matcher.insert("hooks".into(), Value::Array(non_hcom));
                                updated.push(Value::Object(new_matcher));
                            } else if !matcher_obj.contains_key("hooks") {
                                updated.push(matcher.clone());
                            }
                            // else: had only hcom hooks — drop
                        } else {
                            updated.push(matcher.clone());
                        }
                    } else {
                        updated.push(matcher.clone());
                    }
                }
                if updated.is_empty() {
                    hooks.remove(&hook_type);
                } else {
                    hooks.insert(hook_type, Value::Array(updated));
                }
            }
        }

        // Clean up legacy hooks.enabled
        if hooks.get("enabled").and_then(|v| v.as_bool()).is_some() {
            hooks.remove("enabled");
        }

        if hooks.is_empty() {
            settings.remove("hooks");
        }
    }

    // Remove hcom permission patterns from tools.allowed
    if let Some(tools) = settings.get_mut("tools").and_then(|v| v.as_object_mut())
        && let Some(allowed) = tools.get_mut("allowed").and_then(|v| v.as_array_mut())
    {
        let all_patterns = build_all_permission_patterns();
        allowed.retain(|v| {
            v.as_str()
                .map(|s| !all_patterns.iter().any(|p| p == s))
                .unwrap_or(true)
        });
        if allowed.is_empty() {
            tools.remove("allowed");
        }
    }
}

/// Ensure hooksConfig.enabled = true, migrating from legacy hooks.enabled if needed.
///
/// Call this on any hcom gemini command to auto-fix settings.
/// Skips mutation if Gemini version < 0.26.0.
pub fn ensure_hooks_enabled() -> bool {
    let version = get_gemini_version();
    if let Some(v) = version
        && v < GEMINI_MIN_VERSION
    {
        return false;
    }

    let settings_path = get_gemini_settings_path();
    if !settings_path.exists() {
        return true; // setup_gemini_hooks will handle it
    }

    let mut settings = match load_gemini_settings(&settings_path) {
        Some(s) => s,
        None => serde_json::Map::new(),
    };

    let needs_migration = settings
        .get("hooks")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        .is_some();

    if is_hooks_enabled(&settings) && !needs_migration {
        return true;
    }

    set_hooks_enabled(&mut settings);

    let json_str = serde_json::to_string_pretty(&Value::Object(settings)).unwrap_or_default();
    crate::paths::atomic_write(&settings_path, &json_str)
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum VerifyFailReason {
    #[error("settings.json missing, empty, or not parseable as JSON")]
    SettingsUnreadableOrEmpty,
    #[error("neither tools.enableHooks nor legacy enableHooks is true")]
    EnableHooksMissing,
    #[error("hooksConfig.enabled is not true")]
    HooksConfigDisabled,
    #[error("'hooks' key missing or not an object")]
    HooksKeyMissing,
    #[error("hook type '{0}' missing or empty")]
    HookTypeMissing(String),
    #[error("hcom hook command '{cmd_suffix}' not found under hook type '{hook_type}'")]
    HookCommandMissing {
        hook_type: String,
        cmd_suffix: String,
    },
    #[error("hook type '{0}': hcom entry has 'type' != \"command\"")]
    HookTypeFieldNotCommand(String),
    #[error("hook type '{hook_type}' name mismatch: expected {expected:?}, got {actual:?}")]
    HookNameMismatch {
        hook_type: String,
        expected: String,
        actual: String,
    },
    #[error("hook type '{hook_type}' matcher mismatch: expected {expected:?}, got {actual:?}")]
    HookMatcherMismatch {
        hook_type: String,
        expected: String,
        actual: String,
    },
    #[error(
        "hook type '{hook_type}' has no numeric 'timeout' field (canonical): expected a numeric timeout for a canonically-bounded hook"
    )]
    HookTimeoutMissing { hook_type: String },
    #[error("duplicate hcom hook entry for hook type '{0}'")]
    HookDuplicated(String),
    #[error("policy file missing: {}", .0.display())]
    PermissionsPolicyMissing(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(
        "Gemini CLI version {}.{}.{} is too old (need >= {}.{}.{})",
        detected.0, detected.1, detected.2, required.0, required.1, required.2
    )]
    VersionUnsupported {
        detected: (u32, u32, u32),
        required: (u32, u32, u32),
    },
    #[error("JSON serialization failed: {0}")]
    SerializationFailed(#[from] serde_json::Error),
    #[error("atomic write to {} failed: {source}", path.display())]
    AtomicWriteFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("post-write verify failed for {}: {reason}", path.display())]
    PostWriteVerifyFailed {
        path: PathBuf,
        #[source]
        reason: VerifyFailReason,
    },
}

/// Shell wrapper for a single hcom hook subcommand (`gemini-beforeagent`, etc.).
///
/// Silently no-ops (exit 0) when hcom isn't on PATH.
///
/// Gemini CLI's `getShellConfiguration()` (packages/core/src/utils/shell-utils.ts)
/// runs hook commands via PowerShell on Windows (`-NoProfile -NonInteractive -Command`),
/// never a POSIX shell, and via a POSIX shell (e.g. `bash -c`) elsewhere. The command
/// string built here is PowerShell-native on Windows and POSIX `sh -c` elsewhere to
/// match what actually executes it.
fn hook_command(hcom_cmd: &str, cmd_suffix: &str) -> String {
    let bin = hcom_cmd.split_whitespace().next().unwrap_or("hcom");
    if cfg!(windows) {
        // hcom_cmd/cmd_suffix are always fixed hcom invocations (never user input),
        // so no quoting is needed here; if that ever changes, note that PowerShell
        // still expands `$variables` and backticks in bare (unquoted) script text.
        format!(
            "if (Get-Command {bin} -ErrorAction SilentlyContinue) {{ {hcom_cmd} {cmd_suffix} }} else {{ exit 0 }}"
        )
    } else {
        format!(
            "sh -c 'command -v {bin} >/dev/null 2>&1 && exec {hcom_cmd} {cmd_suffix} || exit 0'"
        )
    }
}

/// Set up hcom hooks in Gemini settings.json.
///
/// - Removes existing hcom hooks first (clean slate)
/// - Adds all hooks from GEMINI_HOOK_CONFIGS
/// - Uses atomic write for safety
pub fn try_setup_gemini_hooks(include_permissions: bool) -> Result<(), SetupError> {
    // Guard: block only if version detected AND too old
    if let Some(v) = get_gemini_version()
        && v < GEMINI_MIN_VERSION
    {
        return Err(SetupError::VersionUnsupported {
            detected: v,
            required: GEMINI_MIN_VERSION,
        });
    }

    let settings_path = get_gemini_settings_path();
    if let Some(parent) = settings_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut settings = load_gemini_settings(&settings_path).unwrap_or_default();

    // Remove existing hcom hooks (clean slate)
    remove_hcom_hooks_from_settings(&mut settings);

    // Ensure tools.enableHooks = true
    if !settings.contains_key("tools") {
        settings.insert("tools".into(), serde_json::json!({}));
    }
    if let Some(tools) = settings.get_mut("tools").and_then(|v| v.as_object_mut()) {
        tools.insert("enableHooks".into(), Value::Bool(true));

        // Clean up legacy tools.allowed entries (migrated to policy engine)
        if let Some(allowed) = tools.get_mut("allowed").and_then(|v| v.as_array_mut()) {
            let all_patterns = build_all_permission_patterns();
            allowed.retain(|v| {
                v.as_str()
                    .map(|s| !all_patterns.iter().any(|p| p == s))
                    .unwrap_or(true)
            });
            if allowed.is_empty() {
                tools.remove("allowed");
            }
        }
    }

    // Handle permissions via policy engine (~/.gemini/policies/hcom.toml)
    if include_permissions {
        setup_gemini_policy();
    } else {
        remove_gemini_policy();
    }

    let hcom_cmd = crate::runtime_env::build_hcom_command();

    // Set hooksConfig.enabled
    set_hooks_enabled(&mut settings);

    // Ensure hooks dict exists
    if !settings.contains_key("hooks") || !settings["hooks"].is_object() {
        settings.insert("hooks".into(), serde_json::json!({}));
    }

    // Add hook entries
    if let Some(hooks) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        for &(hook_type, matcher, cmd_suffix, timeout, description) in GEMINI_HOOK_CONFIGS {
            let hook_name = format!("hcom-{}", hook_type.to_lowercase());
            let hook_entry = serde_json::json!({
                "matcher": matcher,
                "hooks": [{
                    "name": hook_name,
                    "type": "command",
                    "command": hook_command(&hcom_cmd, cmd_suffix),
                    "timeout": timeout,
                    "description": description,
                }]
            });

            if !hooks.contains_key(hook_type) || !hooks[hook_type].is_array() {
                hooks.insert(hook_type.into(), Value::Array(Vec::new()));
            }
            if let Some(arr) = hooks.get_mut(hook_type).and_then(|v| v.as_array_mut()) {
                arr.push(hook_entry);
            }
        }
    }

    let json_str = serde_json::to_string_pretty(&Value::Object(settings))
        .map_err(SetupError::SerializationFailed)?;

    crate::paths::atomic_write_io(&settings_path, &json_str).map_err(|e| {
        SetupError::AtomicWriteFailed {
            path: settings_path.clone(),
            source: e,
        }
    })?;

    // Re-read from disk: catches truncation, FS-layer corruption, and
    // concurrent overwrite by another process between rename and verify.
    verify_hooks_at(&settings_path, include_permissions).map_err(|reason| {
        SetupError::PostWriteVerifyFailed {
            path: settings_path,
            reason,
        }
    })?;

    Ok(())
}

pub fn setup_gemini_hooks(include_permissions: bool) -> bool {
    try_setup_gemini_hooks(include_permissions).is_ok()
}

/// Lenient: ignores hook timeout fields entirely. Timeouts are user-tunable.
/// Verify hcom hooks are installed in Gemini settings. Every hook must have a
/// numeric `timeout` field — the value itself is not checked, so user edits
/// still pass.
pub fn verify_gemini_hooks_installed(check_permissions: bool) -> bool {
    verify_hooks_at(&get_gemini_settings_path(), check_permissions).is_ok()
}

fn verify_hooks_at(settings_path: &Path, check_permissions: bool) -> Result<(), VerifyFailReason> {
    let settings =
        load_gemini_settings(settings_path).ok_or(VerifyFailReason::SettingsUnreadableOrEmpty)?;

    // Check tools.enableHooks or legacy enableHooks
    let enable_hooks = settings
        .get("tools")
        .and_then(|v| v.get("enableHooks"))
        .and_then(|v| v.as_bool())
        .or_else(|| settings.get("enableHooks").and_then(|v| v.as_bool()));
    if enable_hooks != Some(true) {
        return Err(VerifyFailReason::EnableHooksMissing);
    }

    // Check hooksConfig.enabled
    if !is_hooks_enabled(&settings) {
        return Err(VerifyFailReason::HooksConfigDisabled);
    }

    // Check all hook types
    let hooks = settings
        .get("hooks")
        .and_then(|v| v.as_object())
        .ok_or(VerifyFailReason::HooksKeyMissing)?;

    for &(hook_type, expected_matcher, cmd_suffix, _expected_timeout, _) in GEMINI_HOOK_CONFIGS {
        let hook_matchers = match hooks.get(hook_type).and_then(|v| v.as_array()) {
            Some(arr) if !arr.is_empty() => arr,
            _ => return Err(VerifyFailReason::HookTypeMissing(hook_type.to_string())),
        };

        let expected_name = format!("hcom-{}", hook_type.to_lowercase());
        let mut found = false;

        for matcher_dict in hook_matchers {
            let matcher_obj = match matcher_dict.as_object() {
                Some(o) => o,
                None => continue,
            };

            let actual_matcher = matcher_obj
                .get("matcher")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let matcher_hooks = match matcher_obj.get("hooks").and_then(|v| v.as_array()) {
                Some(h) => h,
                None => continue,
            };

            for hook in matcher_hooks {
                if is_hcom_hook(hook) {
                    if found {
                        return Err(VerifyFailReason::HookDuplicated(hook_type.to_string()));
                    }
                    if actual_matcher != expected_matcher {
                        return Err(VerifyFailReason::HookMatcherMismatch {
                            hook_type: hook_type.to_string(),
                            expected: expected_matcher.to_string(),
                            actual: actual_matcher.to_string(),
                        });
                    }
                    if hook.get("type").and_then(|v| v.as_str()) != Some("command") {
                        return Err(VerifyFailReason::HookTypeFieldNotCommand(
                            hook_type.to_string(),
                        ));
                    }
                    let actual_name = hook
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if actual_name != expected_name {
                        return Err(VerifyFailReason::HookNameMismatch {
                            hook_type: hook_type.to_string(),
                            expected: expected_name.clone(),
                            actual: actual_name,
                        });
                    }
                    if hook.get("timeout").and_then(|v| v.as_u64()).is_none() {
                        return Err(VerifyFailReason::HookTimeoutMissing {
                            hook_type: hook_type.to_string(),
                        });
                    }
                    let command = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    let has_hcom = command.contains("${HCOM}")
                        || command.to_ascii_lowercase().contains("hcom");
                    if !has_hcom || !command.contains(cmd_suffix) {
                        return Err(VerifyFailReason::HookCommandMissing {
                            hook_type: hook_type.to_string(),
                            cmd_suffix: cmd_suffix.to_string(),
                        });
                    }
                    found = true;
                }
            }
        }

        if !found {
            return Err(VerifyFailReason::HookCommandMissing {
                hook_type: hook_type.to_string(),
                cmd_suffix: cmd_suffix.to_string(),
            });
        }
    }

    // Check permissions via policy engine
    if check_permissions {
        let policy_file = get_gemini_policies_path().join("hcom.toml");
        if !policy_file.exists() {
            return Err(VerifyFailReason::PermissionsPolicyMissing(policy_file));
        }
    }

    Ok(())
}

/// Remove hcom hooks from Gemini settings (global + local).
///
/// Removes hooks from settings.json and policy file from policies/.
pub fn remove_gemini_hooks() -> bool {
    let global_path = dirs::home_dir()
        .map(|h| h.join(".gemini").join("settings.json"))
        .unwrap_or_default();
    let env_path = std::env::var("GEMINI_CLI_HOME")
        .ok()
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join(".gemini").join("settings.json"));
    let local_path = get_gemini_settings_path();

    let global_ok = remove_hooks_from_path(&global_path);
    let env_ok = match env_path {
        Some(ref p) if *p != global_path => remove_hooks_from_path(p),
        _ => true,
    };
    let local_ok = if local_path != global_path && Some(&local_path) != env_path.as_ref() {
        remove_hooks_from_path(&local_path)
    } else {
        true
    };

    // Remove policy files
    let global_policies = dirs::home_dir()
        .map(|h| h.join(".gemini").join("policies"))
        .unwrap_or_default();
    let env_policies = std::env::var("GEMINI_CLI_HOME")
        .ok()
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join(".gemini").join("policies"));
    let local_policies = get_gemini_policies_path();

    let global_policy_ok = remove_policy_from_path(&global_policies);
    let env_policy_ok = match env_policies {
        Some(ref p) if *p != global_policies => remove_policy_from_path(p),
        _ => true,
    };
    let local_policy_ok =
        if local_policies != global_policies && Some(&local_policies) != env_policies.as_ref() {
            remove_policy_from_path(&local_policies)
        } else {
            true
        };

    global_ok && env_ok && local_ok && global_policy_ok && env_policy_ok && local_policy_ok
}

fn remove_hooks_from_path(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let mut settings = match load_gemini_settings(path) {
        Some(s) => s,
        None => return true,
    };

    remove_hcom_hooks_from_settings(&mut settings);

    let json_str = serde_json::to_string_pretty(&Value::Object(settings)).unwrap_or_default();
    crate::paths::atomic_write(path, &json_str)
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;
