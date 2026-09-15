//! Claude Code hook handler, settings management, and subagent lifecycle.
//!
//! Claude lifecycle, tool, failure, permission, and subagent hooks.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

use crate::bootstrap;
use crate::config::HcomConfig;
use crate::db::{HcomDb, InstanceRow};
use crate::hooks::common;
use crate::hooks::family;
use crate::hooks::{DeliveryAck, HookPayload};
use crate::instance_binding;
use crate::instance_lifecycle as lifecycle;
use crate::instance_names;
use crate::instances;
use crate::log;
use crate::messages;
use crate::paths;
use crate::shared::context::HcomContext;
use crate::shared::{ST_ACTIVE, ST_BLOCKED, ST_INACTIVE, ST_LISTENING};

const HOOK_SESSIONSTART: &str = "sessionstart";
const HOOK_USERPROMPTSUBMIT: &str = "userpromptsubmit";
const HOOK_PRE: &str = "pre";
const HOOK_POST: &str = "post";
const HOOK_POST_FAILURE: &str = "post-failure";
const HOOK_NOTIFY: &str = "notify";
const HOOK_PERMISSION_REQUEST: &str = "permission-request";
const HOOK_PERMISSION_DENIED: &str = "permission-denied";
const HOOK_SUBAGENT_START: &str = "subagent-start";
const HOOK_SUBAGENT_STOP: &str = "subagent-stop";
const HOOK_SESSIONEND: &str = "sessionend";
const HOOK_STOP_FAILURE: &str = "stop-failure";
const HOOK_POLL: &str = "poll";

fn is_subagent_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Agent" | "Task")
}

fn is_shell_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Bash" | "PowerShell")
}

// This is deliberately a protocol detector, not a security boundary. A
// standalone hcom command token anywhere in a child shell command is enough to
// instrument it. Bias toward false positives: adding actor identity to a
// command that merely mentions hcom is harmless, while missing a wrapped hcom
// invocation can silently attribute it to the shared root Claude session.
static RE_HCOM_COMMAND_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)(?:^|[\s;&|('"`])(?:\$\{?HCOM\}?|(?:[^\s;&|()'"`]+/)*hcom)(?:$|[\s;&|)('"`])"#,
    )
    .unwrap()
});

fn visibly_invokes_hcom(command: &str) -> bool {
    RE_HCOM_COMMAND_TOKEN.is_match(command)
}

fn child_visibly_invokes_hcom(payload: &HookPayload) -> bool {
    payload
        .raw
        .get("agent_id")
        .and_then(Value::as_str)
        .is_some_and(|agent_id| !agent_id.is_empty())
        && is_shell_tool(&payload.tool_name)
        && payload
            .tool_input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(visibly_invokes_hcom)
}

fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Handle a Claude hook — entry point from router.
///
/// Reads JSON from stdin, builds context, dispatches to appropriate handler.
/// Returns exit code (0 = success/non-participant, 2 = message delivered).
///
pub fn dispatch_claude_hook(hook_type: &str) -> i32 {
    let start = Instant::now();

    // Read stdin JSON
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        log::log_error(
            "hooks",
            "claude.stdin_error",
            &format!("hook={} err={}", hook_type, e),
        );
        return 0;
    }

    let raw: Value = match serde_json::from_slice(&input) {
        Ok(v) => v,
        Err(e) => {
            log::log_error(
                "hooks",
                "claude.parse_error",
                &format!("hook={} err={}", hook_type, e),
            );
            return 0;
        }
    };

    // Open DB (includes schema migration/compat check)
    let db = match HcomDb::open() {
        Ok(db) => db,
        Err(e) => {
            log::log_warn(
                "hooks",
                "claude.db_error",
                &format!("hook={} err={}", hook_type, e),
            );
            return 0;
        }
    };

    let ctx = HcomContext::from_os();
    let mut payload = HookPayload::from_claude(raw);

    // Keep ordinary nonparticipants completely silent. The sole exception is
    // a visible hcom attempt from a child: let it reach routing so it receives
    // the actionable "start hcom in the parent first" denial even with an
    // otherwise empty database.
    if !(common::hook_gate_check(&ctx, &db)
        || (hook_type == HOOK_PRE && child_visibly_invokes_hcom(&payload)))
    {
        return 0;
    }

    let (exit_code, stdout, delivery_ack, timing) = common::dispatch_with_panic_guard(
        "claude",
        hook_type,
        (0, String::new(), None, DispatchTiming::default()),
        || route_claude_hook(&db, &ctx, hook_type, &mut payload),
    );

    // Output result
    if !stdout.is_empty() {
        let mut writer = std::io::stdout().lock();
        if let Err(e) = write_hook_output(&db, &mut writer, &stdout, delivery_ack.as_ref()) {
            log::log_error(
                "hooks",
                "claude.stdout_error",
                &format!("hook={} err={}", hook_type, e),
            );
        }
    }

    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    let tool_name = payload.tool_name.as_str();
    log::log_info(
        "hooks",
        "claude.dispatch.timing",
        &timing.format(hook_type, tool_name, exit_code, total_ms),
    );

    exit_code
}

fn write_hook_output(
    db: &HcomDb,
    writer: &mut impl std::io::Write,
    stdout: &str,
    delivery_ack: Option<&DeliveryAck>,
) -> std::io::Result<()> {
    writer.write_all(stdout.as_bytes())?;
    writer.flush()?;
    if let Some(ack) = delivery_ack {
        common::commit_delivery_ack(db, ack);
    }
    Ok(())
}

/// A wake is only ever the exact `<hcom>` marker.  In particular, do not
/// consume a user's draft when terminal injection and typing overlap.
fn is_bare_hcom_wake(payload: &HookPayload) -> bool {
    payload
        .raw
        .get("prompt")
        .and_then(Value::as_str)
        .is_some_and(|prompt| prompt.trim() == "<hcom>")
}

/// Prevent an injected wake with no accompanying context from reaching Claude.
/// Claude displays `reason` to the user without adding it to model context.
fn block_bare_hcom_wake(reason: &str) -> (i32, String, Option<DeliveryAck>) {
    let output = serde_json::json!({
        "decision": "block",
        "reason": reason,
        "suppressOriginalPrompt": true,
    });
    (0, output.to_string(), None)
}

fn block_unroutable_bare_hcom_wake() -> (i32, String, Option<DeliveryAck>) {
    // A delivery race, session switch, stale/missing binding, conflicting
    // identity evidence, or a wake landing in a subagent view can all arrive
    // here. None of the available signals can tell those apart reliably
    // (launch environment is replayed by Claude), so say so explicitly.
    block_bare_hcom_wake(
        "hcom received a wake but could not prepare delivery context for this Claude session. Another hook may already have delivered the message, or session attribution may have failed (for example, a switched or unbound session). No action is needed unless you were expecting a message; then return to the agent's main session or retry with `hcom kill <name> && hcom r <name>`. To join this session to hcom, run `hcom start`.",
    )
}

/// Per-stage timing collected during dispatch,
#[derive(Default)]
struct DispatchTiming {
    init_ms: Option<f64>,
    session_ms: Option<f64>,
    resolve_ms: Option<f64>,
    bind_ms: Option<f64>,
    handler_ms: Option<f64>,
    subagent_check_ms: Option<f64>,
    task_ms: Option<f64>,
    instance: Option<String>,
    context: Option<&'static str>,
    result: Option<&'static str>,
}

impl DispatchTiming {
    /// Format timing fields as key=value pairs for log line.
    fn format(&self, hook_type: &str, tool_name: &str, exit_code: i32, total_ms: f64) -> String {
        let instance = self.instance.as_deref();
        let mut parts = vec![format!("hook={}", hook_type)];
        if !tool_name.is_empty() {
            parts.push(format!("tool={}", tool_name));
        }
        if let Some(name) = instance {
            parts.push(format!("instance={}", name));
        }
        if let Some(v) = self.init_ms {
            parts.push(format!("init_ms={:.2}", v));
        }
        if let Some(v) = self.session_ms {
            parts.push(format!("session_ms={:.2}", v));
        }
        if let Some(v) = self.resolve_ms {
            parts.push(format!("resolve_ms={:.2}", v));
        }
        if let Some(v) = self.bind_ms {
            parts.push(format!("bind_ms={:.2}", v));
        }
        if let Some(v) = self.handler_ms {
            parts.push(format!("handler_ms={:.2}", v));
        }
        if let Some(v) = self.subagent_check_ms {
            parts.push(format!("subagent_check_ms={:.2}", v));
        }
        if let Some(v) = self.task_ms {
            parts.push(format!("task_ms={:.2}", v));
        }
        parts.push(format!("total_ms={:.2}", total_ms));
        parts.push(format!("exit_code={}", exit_code));
        if let Some(ctx) = self.context {
            parts.push(format!("context={}", ctx));
        }
        if let Some(r) = self.result {
            parts.push(format!("result={}", r));
        }
        parts.join(" ")
    }
}

/// Core dispatcher — routes to appropriate handler.
///
/// Returns (exit_code, stdout_string, deferred delivery ack, timing).
fn route_claude_hook(
    db: &HcomDb,
    ctx: &HcomContext,
    hook_type: &str,
    payload: &mut HookPayload,
) -> (i32, String, Option<DeliveryAck>, DispatchTiming) {
    let dispatch_start = Instant::now();
    let mut timing = DispatchTiming::default();

    // Ensure directories and init DB
    if !paths::ensure_hcom_directories() {
        return (0, String::new(), None, timing);
    }

    timing.init_ms = Some(dispatch_start.elapsed().as_secs_f64() * 1000.0);

    // Correct session_id (fork bug workaround)
    let session_start = Instant::now();
    let use_fork_env_session = should_use_fork_env_session_id(db, ctx, &payload.raw);
    let session_id = get_real_session_id(
        &payload.raw,
        ctx.claude_env_file.as_deref(),
        use_fork_env_session,
    );

    // Update payload in place so downstream handlers don't need another raw clone.
    payload.session_id = Some(session_id.clone());
    if let Some(obj) = payload.raw.as_object_mut() {
        obj.insert("session_id".into(), Value::String(session_id.clone()));
    }
    timing.session_ms = Some(session_start.elapsed().as_secs_f64() * 1000.0);

    // SessionStart — no instance resolution needed
    if hook_type == HOOK_SESSIONSTART {
        let handler_start = Instant::now();
        let result = handle_sessionstart(
            db,
            ctx,
            &session_id,
            payload.transcript_path.as_deref(),
            &payload.raw,
        );
        timing.handler_ms = Some(handler_start.elapsed().as_secs_f64() * 1000.0);
        return (result.0, result.1, None, timing);
    }

    // Claude identifies the hook's actor with agent_id. All actors share the
    // root session_id. Actor routing must therefore happen before any lifecycle handling.
    let raw_agent_id = payload
        .raw
        .get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if let Some(agent_id) = raw_agent_id {
        // Every Claude subagent carries agent_id on its hooks regardless of
        // whether its root ever ran `hcom start` — that's a property of
        // Claude's hook schema, not of hcom participation. Act only if the
        // shared session_id actually has an hcom root binding; otherwise this
        // whole branch (including the `hcom start --name ...` hint at
        // SubagentStart) must be a silent no-op, same as any other
        // nonparticipant hook.
        let root_name = match db.get_session_binding(&session_id) {
            Ok(Some(root_name)) => root_name,
            Ok(None) => {
                if hook_type == HOOK_PRE && child_visibly_invokes_hcom(payload) {
                    let output = serde_json::json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason":
                                "This Claude subagent cannot join hcom before its parent. Run `hcom start` in the parent Claude session first, then spawn a new subagent.",
                        }
                    });
                    return (0, output.to_string(), None, timing);
                }
                if hook_type == HOOK_USERPROMPTSUBMIT && is_bare_hcom_wake(payload) {
                    let (code, stdout, ack) = block_unroutable_bare_hcom_wake();
                    return (code, stdout, ack, timing);
                }
                return (0, String::new(), None, timing);
            }
            Err(_) => {
                if hook_type == HOOK_USERPROMPTSUBMIT && is_bare_hcom_wake(payload) {
                    let (code, stdout, ack) = block_unroutable_bare_hcom_wake();
                    return (code, stdout, ack, timing);
                }
                return (0, String::new(), None, timing);
            }
        };
        let (validated_root, _, root_is_primary) = common::init_hook_context(
            db,
            ctx,
            &session_id,
            payload.transcript_path.as_deref().unwrap_or(""),
        );
        if validated_root.as_deref() != Some(root_name.as_str()) || !root_is_primary {
            if matches!(
                hook_type,
                HOOK_POST | HOOK_POST_FAILURE | HOOK_PERMISSION_DENIED
            ) {
                revoke_shell_actor_for_hook(db, &session_id, payload, Some(&agent_id));
            }
            timing.result = Some("historical_session");
            log::log_warn(
                "hooks",
                "claude.historical_subagent_hook_rejected",
                &format!(
                    "hook={} session_id={} root={}",
                    hook_type, session_id, root_name
                ),
            );
            if hook_type == HOOK_USERPROMPTSUBMIT && is_bare_hcom_wake(payload) {
                let (code, stdout, ack) = block_unroutable_bare_hcom_wake();
                return (code, stdout, ack, timing);
            }
            return (0, String::new(), None, timing);
        }
        let subagent_check_start = Instant::now();
        let (exit_code, stdout, delivery_ack) = route_subagent_actor_hook(
            db,
            hook_type,
            payload,
            &agent_id,
            &session_id,
            &root_name,
            &mut timing,
        );
        timing.subagent_check_ms = Some(subagent_check_start.elapsed().as_secs_f64() * 1000.0);
        return (exit_code, stdout, delivery_ack, timing);
    }

    // ---- Root/main-thread hook (no agent_id) from here on ----

    // Capability revocation is keyed by (session_id, tool_use_id) alone, so it
    // must not depend on identity resolution: a hook we cannot attribute would
    // otherwise leave the shell actor's `hcom send` capability granted forever.
    if matches!(
        hook_type,
        HOOK_POST | HOOK_POST_FAILURE | HOOK_PERMISSION_DENIED
    ) {
        revoke_shell_actor_for_hook(db, &session_id, payload, None);
    }

    let tool_name = payload.tool_name.as_str();
    // Resolve parent instance
    let resolve_start = Instant::now();
    let (instance_name, updates, _is_matched_resume) = common::init_hook_context(
        db,
        ctx,
        &session_id,
        payload.transcript_path.as_deref().unwrap_or(""),
    );
    timing.resolve_ms = Some(resolve_start.elapsed().as_secs_f64() * 1000.0);

    // Participation requires a trusted session/process binding. Shell output
    // can quote another participant's marker and is not identity evidence.
    let Some(ref instance_name) = instance_name else {
        timing.result = Some("no_instance");
        if hook_type == HOOK_USERPROMPTSUBMIT && is_bare_hcom_wake(payload) {
            let (code, stdout, ack) = block_unroutable_bare_hcom_wake();
            return (code, stdout, ack, timing);
        }
        return (0, String::new(), None, timing);
    };

    let instance_data = match db.get_instance_full(instance_name) {
        Ok(Some(data)) => data,
        _ => {
            timing.result = Some("no_instance_data");
            if hook_type == HOOK_USERPROMPTSUBMIT && is_bare_hcom_wake(payload) {
                let (code, stdout, ack) = block_unroutable_bare_hcom_wake();
                return (code, stdout, ack, timing);
            }
            return (0, String::new(), None, timing);
        }
    };

    let is_primary_generation = instance_data.session_id.as_deref() == Some(session_id.as_str());
    if !is_primary_generation {
        timing.instance = Some(instance_name.clone());
        timing.result = Some("historical_session");
        log::log_warn(
            "hooks",
            "claude.historical_root_hook_rejected",
            &format!(
                "hook={} session_id={} instance={} primary_session={:?}",
                hook_type, session_id, instance_name, instance_data.session_id
            ),
        );
        if hook_type == HOOK_SESSIONEND {
            let handler_start = Instant::now();
            let (code, stdout) =
                handle_sessionend(db, instance_name, &session_id, &payload.raw, &updates);
            timing.handler_ms = Some(handler_start.elapsed().as_secs_f64() * 1000.0);
            return (code, stdout, None, timing);
        }
        if hook_type == HOOK_USERPROMPTSUBMIT && is_bare_hcom_wake(payload) {
            let (code, stdout, ack) = block_unroutable_bare_hcom_wake();
            return (code, stdout, ack, timing);
        }
        return (0, String::new(), None, timing);
    }

    if hook_type == HOOK_PRE && is_subagent_tool(tool_name) {
        let task_start = Instant::now();
        common::update_tool_status(db, instance_name, "claude", tool_name, &payload.tool_input);
        timing.task_ms = Some(task_start.elapsed().as_secs_f64() * 1000.0);
        return (0, String::new(), None, timing);
    }
    if hook_type == HOOK_POST && is_subagent_tool(tool_name) {
        let task_start = Instant::now();
        let stdout = end_task(db, instance_name, &payload.raw).unwrap_or_default();
        timing.task_ms = Some(task_start.elapsed().as_secs_f64() * 1000.0);
        return (0, stdout, None, timing);
    }

    if hook_type == HOOK_USERPROMPTSUBMIT {
        // A missing completion hook is uncertain lifecycle, not identity.
        // Only mark old child rows stale for display; never delete or reroute them.
        mark_stale_subagents(db, &session_id);
        // Fall through to parent handler for PTY message delivery
    }

    // Dispatch to handler
    timing.instance = Some(instance_name.clone());
    let handler_start = Instant::now();
    let (exit_code, stdout, delivery_ack) = match hook_type {
        HOOK_PRE => {
            let (code, stdout) = handle_pretooluse(db, payload, instance_name, &session_id, None);
            (code, stdout, None)
        }
        HOOK_POST => handle_posttooluse(db, ctx, payload, instance_name, &instance_data, &updates),
        HOOK_POST_FAILURE => {
            let (code, stdout) = handle_tool_failure(db, payload, instance_name, &session_id, None);
            (code, stdout, None)
        }
        HOOK_PERMISSION_DENIED => {
            let (code, stdout) =
                handle_permission_denied(db, payload, instance_name, &session_id, None);
            (code, stdout, None)
        }
        HOOK_STOP_FAILURE => {
            let (code, stdout) = handle_stop_failure(db, payload, instance_name);
            (code, stdout, None)
        }
        HOOK_POLL => handle_poll(db, ctx, instance_name, &instance_data),
        HOOK_NOTIFY => {
            let (code, stdout) = handle_notify(db, payload, instance_name, &updates);
            (code, stdout, None)
        }
        HOOK_PERMISSION_REQUEST => {
            let (code, stdout) = handle_permission_request(db, payload, instance_name, &updates);
            (code, stdout, None)
        }
        HOOK_USERPROMPTSUBMIT => {
            handle_userpromptsubmit(db, ctx, payload, instance_name, &updates, &instance_data)
        }
        HOOK_SESSIONEND => {
            let (code, stdout) =
                handle_sessionend(db, instance_name, &session_id, &payload.raw, &updates);
            (code, stdout, None)
        }
        _ => (0, String::new(), None),
    };
    timing.handler_ms = Some(handler_start.elapsed().as_secs_f64() * 1000.0);

    (exit_code, stdout, delivery_ack, timing)
}

/// Route a hook whose payload carries a non-empty `agent_id` — i.e. one that
/// fired inside a subagent's own execution context. Claude's verified
/// `agent_id`, not parent display state, is the actor signal.
///
/// Returns (exit_code, stdout, delivery_ack).
fn route_subagent_actor_hook(
    db: &HcomDb,
    hook_type: &str,
    payload: &HookPayload,
    agent_id: &str,
    session_id: &str,
    root_name: &str,
    timing: &mut DispatchTiming,
) -> (i32, String, Option<DeliveryAck>) {
    timing.context = Some("subagent");

    if matches!(
        hook_type,
        HOOK_POST | HOOK_POST_FAILURE | HOOK_PERMISSION_DENIED
    ) {
        revoke_shell_actor_for_hook(db, session_id, payload, Some(agent_id));
    }

    if hook_type == HOOK_SUBAGENT_START {
        return handle_subagent_start(db, payload, agent_id, session_id, root_name, timing);
    }

    // SubagentStop resolves its own row and handles a missing row safely.
    if hook_type == HOOK_SUBAGENT_STOP {
        return subagent_stop(db, session_id, &payload.raw);
    }

    // Every other subagent hook requires a known actor row. A missing row is
    // not proof that the hook belongs to the root, so fail closed.
    let Ok(Some(subagent_instance)) = db.get_instance_by_agent_id(agent_id) else {
        timing.result = Some("unknown_subagent_actor");
        return (0, String::new(), None);
    };
    refresh_subagent_last_seen(db, &subagent_instance);

    let tool_name = payload.tool_name.as_str();

    // A subagent's own Agent/Task calls just update its status; the child it
    // spawns still attaches to the root (see `handle_subagent_start`), not to
    // this acting subagent.
    if hook_type == HOOK_PRE && is_subagent_tool(tool_name) {
        common::update_tool_status(
            db,
            &subagent_instance,
            "claude",
            tool_name,
            &payload.tool_input,
        );
        return (0, String::new(), None);
    }
    if hook_type == HOOK_POST && is_subagent_tool(tool_name) {
        let stdout = end_task(db, &subagent_instance, &payload.raw).unwrap_or_default();
        return (0, stdout, None);
    }

    match hook_type {
        HOOK_PRE => {
            let (code, stdout) =
                handle_pretooluse(db, payload, &subagent_instance, session_id, Some(agent_id));
            (code, stdout, None)
        }
        HOOK_POST => {
            revoke_shell_actor_for_hook(db, session_id, payload, Some(agent_id));
            if is_shell_tool(tool_name) {
                subagent_posttooluse(db, &subagent_instance)
            } else {
                (0, String::new(), None)
            }
        }
        HOOK_POST_FAILURE => {
            let (code, stdout) =
                handle_tool_failure(db, payload, &subagent_instance, session_id, Some(agent_id));
            (code, stdout, None)
        }
        HOOK_PERMISSION_DENIED => {
            let (code, stdout) = handle_permission_denied(
                db,
                payload,
                &subagent_instance,
                session_id,
                Some(agent_id),
            );
            (code, stdout, None)
        }
        HOOK_STOP_FAILURE => {
            let (code, stdout) = handle_stop_failure(db, payload, &subagent_instance);
            (code, stdout, None)
        }
        HOOK_PERMISSION_REQUEST => {
            let (code, stdout) =
                handle_permission_request(db, payload, &subagent_instance, &Default::default());
            (code, stdout, None)
        }
        HOOK_USERPROMPTSUBMIT if is_bare_hcom_wake(payload) => block_unroutable_bare_hcom_wake(),
        _ => (0, String::new(), None),
    }
}

fn handle_subagent_start(
    db: &HcomDb,
    payload: &HookPayload,
    agent_id: &str,
    session_id: &str,
    root_name: &str,
    timing: &mut DispatchTiming,
) -> (i32, String, Option<DeliveryAck>) {
    let agent_type = payload
        .raw
        .get("agent_type")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    // Every subagent — however deeply Claude nests it in its own hierarchy —
    // attaches directly to the root hcom instance. Claude's hook schema gives
    // no reliable way to correlate a nested Agent/Task call with the specific
    // child that issued it (the same prompt_id can be reused by an
    // interleaved sibling, see history of this function), so root attribution
    // is the only thing knowable without guessing.
    let effective_agent_type = if agent_type.is_empty() {
        "task"
    } else {
        agent_type
    };
    let Some(name) = ensure_subagent_row(db, root_name, session_id, agent_id, effective_agent_type)
    else {
        timing.result = Some("subagent_row_allocation_failed");
        return (0, String::new(), None);
    };
    refresh_subagent_last_seen(db, &name);

    let stdout = build_subagent_start_output(&payload.raw, root_name)
        .and_then(|output| serde_json::to_string(&output).ok())
        .unwrap_or_default();
    (0, stdout, None)
}

/// Return the session id carried directly by the hook payload.
fn hook_payload_session_id(raw: &Value) -> &str {
    let hook_session_id = raw
        .get("session_id")
        .or_else(|| raw.get("sessionId"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if !hook_session_id.is_empty() {
        return hook_session_id;
    }
    raw.get("session")
        .and_then(|session| {
            session
                .get("session_id")
                .or_else(|| session.get("sessionId"))
                .and_then(|value| value.as_str())
        })
        .unwrap_or("")
}

fn session_id_from_claude_env_file(env_file: Option<&str>) -> Option<String> {
    let path = Path::new(env_file?);
    let parts: Vec<&str> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    let index = parts.iter().position(|part| *part == "session-env")?;
    let candidate = *parts.get(index + 1)?;
    (candidate.len() == 36
        && candidate
            .chars()
            .filter(|character| *character == '-')
            .count()
            == 4)
        .then(|| candidate.to_string())
}

fn is_fresh_claude_process_placeholder(
    db: &HcomDb,
    process_session_id: Option<&str>,
    process_owner: Option<&str>,
    expected_session_id: Option<&str>,
) -> bool {
    process_session_id.is_none()
        && process_owner.is_some_and(|owner| {
            db.get_instance_full(owner)
                .ok()
                .flatten()
                .is_some_and(|instance| {
                    let resumed_generation = expected_session_id.is_some_and(|session_id| {
                        instance.session_id.as_deref() == Some(session_id)
                    });
                    (instance.session_id.is_none() || resumed_generation)
                        && (resumed_generation
                            || instances::is_launching_placeholder(&instance)
                            || (instance.status == ST_BLOCKED
                                && instance.status_context == "launch_blocked")
                            || (instance.status == ST_LISTENING
                                && matches!(
                                    instance.status_context.as_str(),
                                    "start" | "ready_observed" | "launch_blocked_cleared"
                                )))
                })
        })
}

/// `HCOM_IS_FORK` is persistent session environment, so it cannot by itself
/// authorize replacing the hook payload's session id. The env-file workaround
/// is allowed for the original fresh hcom-fork placeholder, or on later
/// non-SessionStart hooks only when this exact worker is already bound to the
/// env-file generation.
fn should_use_fork_env_session_id(db: &HcomDb, ctx: &HcomContext, raw: &Value) -> bool {
    if !ctx.is_fork {
        return false;
    }
    let Some(env_session_id) = session_id_from_claude_env_file(ctx.claude_env_file.as_deref())
    else {
        return false;
    };
    let Some(process_id) = ctx.process_id.as_deref() else {
        return false;
    };
    let Ok(Some((process_session_id, process_owner))) = db.get_process_binding_full(process_id)
    else {
        return false;
    };
    let fresh_placeholder = is_fresh_claude_process_placeholder(
        db,
        process_session_id.as_deref(),
        Some(process_owner.as_str()),
        None,
    );
    let source = raw.get("source").and_then(Value::as_str).unwrap_or("");
    if matches!(source, "fork" | "startup" | "resume") {
        return fresh_placeholder;
    }
    process_session_id.as_deref() == Some(env_session_id.as_str())
}

/// Get the effective session id, applying Claude Code's `--fork-session`
/// workaround only after the worker binding proves the env file belongs to
/// this launch/generation.
fn get_real_session_id(
    raw: &Value,
    env_file: Option<&str>,
    allow_fork_env_override: bool,
) -> String {
    let hook_session_id = hook_payload_session_id(raw);
    if allow_fork_env_override && let Some(candidate) = session_id_from_claude_env_file(env_file) {
        log::log_info(
            "hooks",
            "get_real_session_id.from_env_file",
            &format!(
                "candidate={} hook_session_id={}",
                candidate, hook_session_id
            ),
        );
        return candidate;
    }
    hook_session_id.to_string()
}

/// Handle SessionStart: bind session, inject bootstrap.
fn handle_sessionstart(
    db: &HcomDb,
    ctx: &HcomContext,
    session_id: &str,
    transcript_path: Option<&str>,
    raw: &Value,
) -> (i32, String) {
    let source = raw.get("source").and_then(Value::as_str).unwrap_or("");
    let process_id = ctx.process_id.as_deref();
    let transcript_path = transcript_path.unwrap_or("");
    let evidence = match common::load_claude_identity_evidence(
        db,
        process_id,
        session_id,
        transcript_path,
        |evidence| {
            let fresh_process_placeholder = is_fresh_claude_process_placeholder(
                db,
                evidence.process_session_id.as_deref(),
                evidence.process_owner.as_deref(),
                Some(session_id),
            );
            should_scan_sessionstart_lineage(
                ctx.is_fork && fresh_process_placeholder,
                source,
                evidence.session_owner.is_some(),
                evidence.validated_session_owner.is_some(),
                evidence.owners_disagree,
            )
        },
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            log::log_warn(
                "hooks",
                "sessionstart.identity_evidence_error",
                &format!(
                    "source={} session_id={} transcript_path={} process_id={:?} err={} mutated=false",
                    source, session_id, transcript_path, process_id, error
                ),
            );
            return (0, String::new());
        }
    };
    let fresh_process_placeholder = is_fresh_claude_process_placeholder(
        db,
        evidence.process_session_id.as_deref(),
        evidence.process_owner.as_deref(),
        Some(session_id),
    );
    let fresh_hcom_fork_launch = ctx.is_fork && fresh_process_placeholder;
    let native_lineage_source = matches!(source, "fork" | "startup" | "resume");

    log::log_info(
        "hooks",
        "sessionstart.entry",
        &format!(
            "source={} session_id={} transcript_path={} process_id={:?} process_binding={:?} process_owner={:?} process_session_id={:?} session_owner={:?} validated_session_owner={:?} transcript_owners={:?} hcom_is_fork={} fresh_process_placeholder={} fresh_hcom_fork_launch={}",
            source,
            session_id,
            transcript_path,
            process_id,
            evidence.process_binding,
            evidence.process_owner,
            evidence.process_session_id,
            evidence.session_owner,
            evidence.validated_session_owner,
            evidence.lineage,
            ctx.is_fork,
            fresh_process_placeholder,
            fresh_hcom_fork_launch,
        ),
    );

    // Compaction recovery: re-inject bootstrap.
    if source == "compact"
        && !session_id.is_empty()
        && let Some(output) = handle_compact_recovery(db, ctx, session_id, process_id)
    {
        return (0, serde_json::to_string(&output).unwrap_or_default());
    }

    // Vanilla instance - show hint.
    if process_id.is_none() || session_id.is_empty() {
        let hcom_cmd = crate::runtime_env::build_hcom_command();
        let output = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": format!("[hcom available - run '{} start' to participate]", hcom_cmd),
            }
        });
        return (0, serde_json::to_string(&output).unwrap_or_default());
    }

    // Resolve the selected owner and log vocabulary first; the mutation and
    // bootstrap workflow below is intentionally shared by cache and lineage.
    let selected_owner = if let Some(owner) = evidence.validated_session_owner.as_deref() {
        Some((
            owner,
            "sessionstart.validated_bind_failed",
            "sessionstart.validated_identity_selected",
        ))
    } else if evidence.lineage_scanned {
        match &evidence.lineage {
            common::TranscriptOwnerResolution::Owner(owner) => Some((
                owner.as_str(),
                "sessionstart.lineage_bind_failed",
                "sessionstart.identity_selected",
            )),
            common::TranscriptOwnerResolution::Ambiguous(owners) => {
                log::log_warn(
                    "hooks",
                    "sessionstart.identity_ambiguous",
                    &format!(
                        "session_id={} source={} transcript_path={} process_id={} process_owner={:?} session_owner={:?} transcript_owners={:?} mutated=false",
                        session_id,
                        source,
                        transcript_path,
                        process_id.unwrap_or_default(),
                        evidence.process_owner,
                        evidence.session_owner,
                        owners,
                    ),
                );
                return (0, String::new());
            }
            common::TranscriptOwnerResolution::Unknown
                if native_lineage_source && !fresh_process_placeholder =>
            {
                log::log_warn(
                    "hooks",
                    "sessionstart.lineage_unknown",
                    &format!(
                        "session_id={} source={} transcript_path={} process_id={} process_owner={:?} process_session={:?} fresh_process_placeholder={} mutated=false",
                        session_id,
                        source,
                        transcript_path,
                        process_id.unwrap_or_default(),
                        evidence.process_owner,
                        evidence.process_session_id,
                        fresh_process_placeholder,
                    ),
                );
                return (0, String::new());
            }
            common::TranscriptOwnerResolution::Unknown => None,
        }
    } else {
        None
    };

    let process_id = process_id.unwrap();
    if let Some((owner, bind_failed_event, selected_event)) = selected_owner {
        if let Err(error) = bind_lineage_owner(
            db,
            owner,
            session_id,
            transcript_path,
            process_id,
            evidence.process_owner.as_deref(),
        ) {
            log::log_error(
                "hooks",
                bind_failed_event,
                &format!(
                    "session_id={} source={} owner={} err={} mutated=false",
                    session_id, source, owner, error
                ),
            );
            return (0, String::new());
        }
        log::log_info(
            "hooks",
            selected_event,
            &format!(
                "session_id={} source={} final_owner={} mutated=true",
                session_id, source, owner
            ),
        );
        crate::relay::worker::ensure_worker(true);
        return bootstrap_for_existing_owner(db, ctx, owner);
    }

    let result_output = match bind_and_bootstrap(db, ctx, session_id, transcript_path, process_id) {
        Ok(result) => result,
        Err(error) => {
            log::log_error(
                "hooks",
                "bind.fail",
                &format!("hook=sessionstart err={}", error),
            );
            return (0, String::new());
        }
    };

    crate::relay::worker::ensure_worker(true);
    let stdout = result_output
        .map(|value| serde_json::to_string(&value).unwrap_or_default())
        .unwrap_or_default();
    (0, stdout)
}

fn bind_lineage_owner(
    db: &HcomDb,
    owner: &str,
    session_id: &str,
    transcript_path: &str,
    process_id: &str,
    process_owner: Option<&str>,
) -> Result<(), String> {
    match db.attach_claude_generation(
        owner,
        session_id,
        transcript_path,
        process_id,
        process_owner,
    ) {
        Ok(true) => {
            log::log_info(
                "hooks",
                "sessionstart.stale_process_binding_deleted",
                &format!("process_id={} selected_owner={}", process_id, owner),
            );
        }
        Ok(false) if process_owner != Some(owner) => {
            log::log_info(
                "hooks",
                "sessionstart.foreign_process_binding_preserved",
                &format!(
                    "process_id={} process_owner={:?} selected_owner={}",
                    process_id, process_owner, owner
                ),
            );
        }
        Ok(false) => {}
        Err(error) => return Err(error.to_string()),
    }

    lifecycle::set_status(db, owner, ST_LISTENING, "start", Default::default());
    Ok(())
}

fn bootstrap_for_existing_owner(db: &HcomDb, ctx: &HcomContext, owner: &str) -> (i32, String) {
    let Some(instance) = db.get_instance_full(owner).ok().flatten() else {
        return (0, String::new());
    };
    let bootstrap_text = bootstrap::get_bootstrap(
        db,
        &ctx.hcom_dir,
        owner,
        "claude",
        ctx.is_background,
        ctx.is_launched,
        &ctx.notes,
        instance.tag.as_deref().unwrap_or(""),
        false,
        ctx.background_name.as_deref(),
    );
    let output = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": bootstrap_text,
        }
    });
    (0, serde_json::to_string(&output).unwrap_or_default())
}

fn should_scan_sessionstart_lineage(
    fresh_hcom_fork_launch: bool,
    source: &str,
    has_session_owner: bool,
    has_validated_session_owner: bool,
    owners_disagree: bool,
) -> bool {
    !fresh_hcom_fork_launch
        && !has_validated_session_owner
        && (matches!(source, "fork" | "startup" | "resume")
            || !has_session_owner
            || owners_disagree)
}

/// Handle compaction recovery (source=compact).
fn handle_compact_recovery(
    db: &HcomDb,
    ctx: &HcomContext,
    session_id: &str,
    process_id: Option<&str>,
) -> Option<Value> {
    let instance_name = db
        .get_session_binding(session_id)
        .ok()
        .flatten()
        .or_else(|| {
            process_id.and_then(|pid| instance_binding::resolve_process_binding(db, Some(pid)))
        })?;

    let bootstrap = if process_id.is_some() {
        // hcom-launched: inject full bootstrap
        let inst = db.get_instance_full(&instance_name).ok()??;
        let tag = inst.tag.as_deref().unwrap_or("");
        bootstrap::get_bootstrap(
            db,
            &ctx.hcom_dir,
            &instance_name,
            "claude",
            ctx.is_background,
            ctx.is_launched,
            &ctx.notes,
            tag,
            false,
            ctx.background_name.as_deref(),
        )
    } else {
        // Vanilla: need rebind
        let mut updates = serde_json::Map::new();
        updates.insert("name_announced".into(), serde_json::json!(false));
        instances::update_instance_position(db, &instance_name, &updates);
        format!(
            "[HCOM RECOVERY] You were participating in hcom as '{}'. \
             Run this command now to continue: hcom start --as {}",
            instance_name, instance_name
        )
    };

    Some(serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": bootstrap,
        }
    }))
}

/// Bind session to process and inject bootstrap for hcom-launched instances.
fn bind_and_bootstrap(
    db: &HcomDb,
    ctx: &HcomContext,
    session_id: &str,
    transcript_path: &str,
    process_id: &str,
) -> Result<Option<Value>, String> {
    let mut instance_name =
        instance_binding::bind_session_to_process(db, session_id, Some(process_id));

    // Orphaned PTY: process_id exists but no binding (e.g., after /clear)
    if instance_name.is_none() {
        instance_name = instance_binding::create_orphaned_pty_identity(
            db,
            session_id,
            Some(process_id),
            "claude",
        );
        log::log_info(
            "hooks",
            "sessionstart.orphan_created",
            &format!("instance={:?} process_id={}", instance_name, process_id),
        );
    }

    let instance_name = instance_name.ok_or("no instance after bind")?;
    let instance = db
        .get_instance_full(&instance_name)
        .map_err(|e| e.to_string())?
        .ok_or("instance not found after bind")?;

    db.attach_claude_generation(
        &instance_name,
        session_id,
        transcript_path,
        process_id,
        Some(&instance_name),
    )
    .map_err(|error| error.to_string())?;

    // Capture launch context
    instance_binding::capture_and_store_launch_context(db, &instance_name);

    lifecycle::set_status(
        db,
        &instance_name,
        ST_LISTENING,
        "start",
        Default::default(),
    );

    crate::runtime_env::set_terminal_title(&instance_name);

    let is_resume = instance.name_announced != 0;
    let tag = instance.tag.as_deref().unwrap_or("");
    let bootstrap_text = bootstrap::get_bootstrap(
        db,
        &ctx.hcom_dir,
        &instance_name,
        "claude",
        ctx.is_background,
        ctx.is_launched,
        &ctx.notes,
        tag,
        false,
        ctx.background_name.as_deref(),
    );

    let result = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": bootstrap_text,
        }
    });

    if !is_resume {
        let mut updates = serde_json::Map::new();
        updates.insert("name_announced".into(), serde_json::json!(true));
        instances::update_instance_position(db, &instance_name, &updates);
        paths::increment_flag_counter("instance_count");
    }

    Ok(Some(result))
}

/// PostToolUse Task: deliver freeze-period messages for `instance_name` — the
/// acting instance's own row.
///
/// Returns Option<String> — JSON stdout if messages were delivered.
/// Dispatcher writes this to stdout before returning exit code.
fn end_task(db: &HcomDb, instance_name: &str, raw: &Value) -> Option<String> {
    // Since Claude Code 2.1.198, Agent/Task calls background by default: this
    // PostToolUse fires immediately with tool_response.status="async_launched"
    // when the call is merely dispatched to the background, not when the
    // subagent actually finishes. Treating that as completion would emit a
    // false "Subagents have finished and are no longer active" summary and
    // advance last_event_id before the subagent's freeze window is over, so
    // skip delivery rather than assume anything about how/when completion is
    // reported — root-scoped messages still flow through the root's own
    // interleaved PostToolUse hooks regardless (see route_claude_hook).
    let is_async_launch = raw
        .get("tool_response")
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        == Some("async_launched");
    if is_async_launch {
        return None;
    }

    let instance_data = match db.get_instance_full(instance_name) {
        Ok(Some(data)) => data,
        _ => return None,
    };

    let freeze_event_id = instance_data.last_event_id;
    let (last_event_id, stdout) = deliver_freeze_messages(db, instance_name, freeze_event_id);

    let mut updates = serde_json::Map::new();
    updates.insert("last_event_id".into(), serde_json::json!(last_event_id));
    instances::update_instance_position(db, instance_name, &updates);

    stdout
}

/// Deliver messages from Task freeze period.
///
/// Returns (last_event_id, Option<stdout_json>). Caller writes stdout.
fn deliver_freeze_messages(
    db: &HcomDb,
    instance_name: &str,
    freeze_event_id: i64,
) -> (i64, Option<String>) {
    let events = match db.get_events_since(freeze_event_id, Some("message"), None) {
        Ok(events) => events,
        Err(_) => return (freeze_event_id, None),
    };

    if events.is_empty() {
        return (freeze_event_id, None);
    }

    let last_id = events
        .iter()
        .filter_map(|e| e.get("id").and_then(|v| v.as_i64()))
        .max()
        .unwrap_or(freeze_event_id);

    // Get subagents for message filtering
    let subagent_rows: Vec<(String, Option<String>)> = db
        .conn()
        .prepare("SELECT name, agent_id FROM instances WHERE parent_name = ?")
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params![instance_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    let subagent_names: Vec<&str> = subagent_rows.iter().map(|(n, _)| n.as_str()).collect();

    // Filter messages
    let mut subagent_msgs: Vec<Value> = Vec::new();
    let mut parent_msgs: Vec<Value> = Vec::new();

    for event in &events {
        let event_data = match event.get("data") {
            Some(d) => d,
            None => continue,
        };
        let sender_name = event_data
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let text = event_data
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let msg = serde_json::json!({
            "timestamp": timestamp,
            "from": sender_name,
            "message": text,
        });

        if subagent_names.contains(&sender_name) {
            subagent_msgs.push(msg);
        } else if !subagent_names.is_empty()
            && subagent_names.iter().any(|name| {
                match messages::should_deliver_message(event_data, name, sender_name) {
                    Ok(v) => v,
                    Err(e) => {
                        log::log_warn(
                            "hooks",
                            "claude.should_deliver_message",
                            &format!("target={} sender={} err={}", name, sender_name, e),
                        );
                        false
                    }
                }
            })
        {
            if !subagent_msgs.contains(&msg) {
                subagent_msgs.push(msg);
            }
        } else {
            match messages::should_deliver_message(event_data, instance_name, sender_name) {
                Ok(true) => parent_msgs.push(msg),
                Ok(false) => {}
                Err(e) => {
                    log::log_warn(
                        "hooks",
                        "claude.should_deliver_message",
                        &format!("target={} sender={} err={}", instance_name, sender_name, e),
                    );
                }
            }
        }
    }

    let mut all_relevant: Vec<Value> = Vec::new();
    all_relevant.extend(subagent_msgs);
    all_relevant.extend(parent_msgs);
    all_relevant.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        ta.cmp(tb)
    });

    if all_relevant.is_empty() {
        return (last_id, None);
    }

    let formatted: Vec<String> = all_relevant
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                m.get("from").and_then(|v| v.as_str()).unwrap_or("?"),
                m.get("message").and_then(|v| v.as_str()).unwrap_or("")
            )
        })
        .collect();

    let subagent_list = if subagent_rows.is_empty() {
        "none".to_string()
    } else {
        subagent_rows
            .iter()
            .map(|(name, agent_id)| {
                if let Some(aid) = agent_id {
                    format!("{} (agent_id: {})", name, aid)
                } else {
                    name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    let summary = format!(
        "[Task tool completed - Message history during Task tool]\n\
         Subagents: {}\n\
         The following {} message(s) occurred:\n\n\
         {}\n\n\
         [End of message history. Subagents have finished and are no longer active.]",
        subagent_list,
        all_relevant.len(),
        formatted.join("\n"),
    );

    let output = serde_json::json!({
        "systemMessage": "[Task subagent messages shown to instance]",
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": summary,
        },
    });

    (
        last_id,
        Some(serde_json::to_string(&output).unwrap_or_default()),
    )
}

/// Resolve the tool use id shared by PreToolUse and its terminal hook.
fn tool_use_id(payload: &HookPayload) -> Option<&str> {
    payload
        .raw
        .get("tool_use_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
}

/// PreToolUse: status tracking plus a verified actor capability for shell tools.
fn handle_pretooluse(
    db: &HcomDb,
    payload: &HookPayload,
    instance_name: &str,
    session_id: &str,
    agent_id: Option<&str>,
) -> (i32, String) {
    let tool_name = payload.tool_name.as_str();
    let tool_input = &payload.tool_input;

    // Skip status update for Claude's internal memory operations.
    if tool_name == "Edit" || tool_name == "Write" {
        let detail = family::extract_tool_detail("claude", tool_name, tool_input);
        if detail.contains("session-memory/") {
            return (0, String::new());
        }
    }

    common::update_tool_status(db, instance_name, "claude", tool_name, tool_input);

    if !is_shell_tool(tool_name) {
        return (0, String::new());
    }

    // Root shell commands already resolve through the Claude session/process
    // binding. Only an in-process subagent needs a capability to distinguish
    // it from the root that owns the shared session.
    let Some(agent_id) = agent_id else {
        return (0, String::new());
    };

    // Only hcom invocations need actor identity. Every other shell command is
    // left byte-for-byte untouched; its hook event is already attributed by
    // Claude's agent_id.
    let (Some(tool_use_id), Some(command)) = (
        tool_use_id(payload),
        tool_input.get("command").and_then(|value| value.as_str()),
    ) else {
        return (0, String::new());
    };
    if !visibly_invokes_hcom(command) {
        return (0, String::new());
    }

    // Thread a verified actor capability into this visible hcom command so the
    // CLI resolves the exact child. If issuance fails, run uninstrumented and
    // let the CLI's normal explicit-name rules produce the actionable error.

    let token = match db.issue_claude_actor_capability(
        session_id,
        tool_use_id,
        Some(agent_id),
        instance_name,
    ) {
        Ok(token) => token,
        Err(error) => {
            log::log_error(
                "hooks",
                "claude.actor.issue_failed",
                &format!(
                    "session_id={} tool_use_id={} actor={} err={}",
                    session_id, tool_use_id, instance_name, error
                ),
            );
            return (0, String::new());
        }
    };

    let mut updated_input = tool_input.clone();
    let Some(object) = updated_input.as_object_mut() else {
        return (0, String::new());
    };
    let command = if tool_name == "PowerShell" {
        format!(
            "$env:{} = {}; $env:{} = {}\n{}",
            crate::claude_actor::ENV_VAR,
            powershell_single_quote(&token),
            crate::claude_actor::SESSION_ENV_VAR,
            powershell_single_quote(session_id),
            command
        )
    } else {
        let token = crate::tools::args_common::shell_quote(&token);
        let actor_session = crate::tools::args_common::shell_quote(session_id);
        format!(
            "export {}={} {}={}\n{}",
            crate::claude_actor::ENV_VAR,
            token,
            crate::claude_actor::SESSION_ENV_VAR,
            actor_session,
            command
        )
    };
    object.insert("command".into(), Value::String(command));

    let output = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": updated_input,
        }
    });
    (0, serde_json::to_string(&output).unwrap_or_default())
}

fn revoke_shell_actor_for_hook(
    db: &HcomDb,
    session_id: &str,
    payload: &HookPayload,
    agent_id: Option<&str>,
) {
    if !is_shell_tool(&payload.tool_name) {
        return;
    }
    let Some(tool_use_id) = tool_use_id(payload) else {
        return;
    };
    if let Err(error) = db.revoke_claude_actor_capability(session_id, tool_use_id, agent_id) {
        log::log_warn(
            "hooks",
            "claude.actor.revoke_failed",
            &format!(
                "session_id={} tool_use_id={} err={}",
                session_id, tool_use_id, error
            ),
        );
    }
}

fn handle_tool_failure(
    db: &HcomDb,
    payload: &HookPayload,
    instance_name: &str,
    session_id: &str,
    agent_id: Option<&str>,
) -> (i32, String) {
    revoke_shell_actor_for_hook(db, session_id, payload, agent_id);
    let error = payload
        .raw
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("tool failed");
    lifecycle::set_status(
        db,
        instance_name,
        ST_ACTIVE,
        &format!("tool_failed:{}", payload.tool_name),
        lifecycle::StatusUpdate {
            detail: error,
            tool_name: &payload.tool_name,
            tool_use_id: tool_use_id(payload).unwrap_or(""),
            ..Default::default()
        },
    );
    (0, String::new())
}

fn handle_permission_denied(
    db: &HcomDb,
    payload: &HookPayload,
    instance_name: &str,
    session_id: &str,
    agent_id: Option<&str>,
) -> (i32, String) {
    revoke_shell_actor_for_hook(db, session_id, payload, agent_id);
    let reason = payload
        .raw
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("permission denied");
    lifecycle::set_status(
        db,
        instance_name,
        ST_BLOCKED,
        &format!("denied:{}", payload.tool_name),
        lifecycle::StatusUpdate {
            detail: reason,
            tool_name: &payload.tool_name,
            tool_use_id: tool_use_id(payload).unwrap_or(""),
            ..Default::default()
        },
    );
    (0, String::new())
}

fn handle_stop_failure(db: &HcomDb, payload: &HookPayload, instance_name: &str) -> (i32, String) {
    let error = payload
        .raw
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let detail = payload
        .raw
        .get("error_details")
        .and_then(|value| value.as_str())
        .unwrap_or(error);
    lifecycle::set_status(
        db,
        instance_name,
        ST_INACTIVE,
        &format!("failure:{}", error),
        lifecycle::StatusUpdate {
            detail,
            ..Default::default()
        },
    );
    (0, String::new())
}

/// Parent PostToolUse: bootstrap, messages, vanilla binding.
fn handle_posttooluse(
    db: &HcomDb,
    ctx: &HcomContext,
    payload: &HookPayload,
    instance_name: &str,
    instance_data: &InstanceRow,
    updates: &serde_json::Map<String, Value>,
) -> (i32, String, Option<DeliveryAck>) {
    let tool_name = payload.tool_name.as_str();
    let mut outputs: Vec<Value> = Vec::new();
    let mut delivery_ack = None;

    // Clear blocked status if tool completed
    if instance_data.status == ST_BLOCKED {
        lifecycle::set_status(
            db,
            instance_name,
            ST_ACTIVE,
            &format!("approved:{}", tool_name),
            lifecycle::StatusUpdate {
                tool_name,
                tool_use_id: tool_use_id(payload).unwrap_or(""),
                ..Default::default()
            },
        );
    }

    // Shell-specific: persist updates and check bootstrap
    if is_shell_tool(tool_name) {
        if !updates.is_empty() {
            instances::update_instance_position(db, instance_name, updates);
        }
        if let Some(output) = inject_bootstrap_if_needed(db, ctx, instance_name, instance_data) {
            outputs.push(output);
        }
    }

    // Message delivery for ALL tools (parent only)
    if let Some((output, ack)) = get_posttooluse_messages(db, instance_name) {
        outputs.push(output);
        delivery_ack = Some(ack);
    }

    if !outputs.is_empty() {
        let combined = combine_posttooluse_outputs(&outputs);
        return (
            0,
            serde_json::to_string(&combined).unwrap_or_default(),
            delivery_ack,
        );
    }

    (0, String::new(), None)
}

/// Defensive fallback bootstrap injection at PostToolUse.
fn inject_bootstrap_if_needed(
    db: &HcomDb,
    ctx: &HcomContext,
    instance_name: &str,
    instance_data: &InstanceRow,
) -> Option<Value> {
    let bootstrap = common::inject_bootstrap_once(db, ctx, instance_name, instance_data, "claude")?;

    paths::increment_flag_counter("instance_count");

    Some(serde_json::json!({
        "systemMessage": "[HCOM info shown to instance]",
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": bootstrap,
        },
    }))
}

/// Check for unread messages to deliver at PostToolUse.
fn get_posttooluse_messages(db: &HcomDb, instance_name: &str) -> Option<(Value, DeliveryAck)> {
    let prepared = common::prepare_pending_messages(db, instance_name)?;
    let model_context =
        common::format_messages_json_for_instance(db, &prepared.messages, instance_name);

    // Claude needs user-facing display in addition to model context
    let user_display =
        common::format_hook_messages_for_instance(db, &prepared.messages, instance_name);

    Some((
        serde_json::json!({
            "systemMessage": user_display,
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": model_context,
            },
        }),
        prepared.ack,
    ))
}

/// Combine multiple PostToolUse outputs with \n\n---\n\n separator.
fn combine_posttooluse_outputs(outputs: &[Value]) -> Value {
    if outputs.len() == 1 {
        return outputs[0].clone();
    }

    let system_msgs: Vec<&str> = outputs
        .iter()
        .filter_map(|o| o.get("systemMessage").and_then(|v| v.as_str()))
        .collect();
    let combined_system = if system_msgs.is_empty() {
        None
    } else {
        Some(system_msgs.join(" + "))
    };

    let contexts: Vec<&str> = outputs
        .iter()
        .filter_map(|o| {
            o.get("hookSpecificOutput")
                .and_then(|h| h.get("additionalContext"))
                .and_then(|v| v.as_str())
        })
        .collect();
    let combined_context = contexts.join("\n\n---\n\n");

    let mut result = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": combined_context,
        }
    });

    if let Some(sys) = combined_system {
        result
            .as_object_mut()
            .unwrap()
            .insert("systemMessage".into(), Value::String(sys));
    }

    result
}

/// Poll hook: message delivery when Claude goes idle.
fn handle_poll(
    db: &HcomDb,
    ctx: &HcomContext,
    instance_name: &str,
    instance_data: &InstanceRow,
) -> (i32, String, Option<DeliveryAck>) {
    log::log_info(
        "hooks",
        "stop.enter",
        &format!(
            "instance={} is_headless={} pty_mode={}",
            instance_name, ctx.is_background, ctx.is_pty_mode
        ),
    );

    // PTY mode: exit immediately, PTY wrapper handles injection
    if ctx.is_pty_mode {
        lifecycle::set_status(db, instance_name, ST_LISTENING, "", Default::default());
        common::notify_hook_instance_with_db(db, instance_name);
        return (0, String::new(), None);
    }

    // Non-PTY: poll for messages
    let wait_timeout = instance_data.wait_timeout;
    let timeout = wait_timeout.unwrap_or_else(HcomConfig::effective_timeout);

    // Persist effective timeout
    let mut updates = serde_json::Map::new();
    updates.insert("wait_timeout".into(), serde_json::json!(timeout));
    instances::update_instance_position(db, instance_name, &updates);

    let result = common::poll_messages(db, instance_name, timeout as u64, ctx.is_background);

    if result.timed_out {
        lifecycle::set_status(
            db,
            instance_name,
            ST_INACTIVE,
            "exit:timeout",
            Default::default(),
        );
    }

    let stdout = result
        .output
        .map(|v| serde_json::to_string(&v).unwrap_or_default())
        .unwrap_or_default();
    // Always exit 0: Claude ignores stdout JSON on exit 2 for Stop, so a
    // delivered message must go out as exit 0 + decision:block, with the ack
    // committed by write_hook_output only after that stdout is flushed.
    (0, stdout, result.ack)
}

/// Parent UserPromptSubmit: fallback bootstrap, PTY mode message delivery.
fn handle_userpromptsubmit(
    db: &HcomDb,
    ctx: &HcomContext,
    payload: &HookPayload,
    instance_name: &str,
    updates: &serde_json::Map<String, Value>,
    instance_data: &InstanceRow,
) -> (i32, String, Option<DeliveryAck>) {
    let name_announced = instance_data.name_announced != 0;
    let mut contexts = Vec::new();
    let mut system_message = None;
    let mut delivery_ack = None;

    // Persist updates
    if !updates.is_empty() {
        instances::update_instance_position(db, instance_name, updates);
    }

    // Bootstrap fallback (rarely fires). Do not return early: a resumed PTY can
    // have both an unannounced bootstrap and pending messages on its first wake.
    if !name_announced
        && ctx.is_launched
        && let Some(bootstrap_text) =
            common::inject_bootstrap_once(db, ctx, instance_name, instance_data, "claude")
    {
        contexts.push(bootstrap_text);
        paths::increment_flag_counter("instance_count");
    }

    // PTY mode: deliver messages
    if ctx.is_pty_mode
        && let Some(prepared) = common::prepare_pending_messages(db, instance_name)
    {
        let user_display =
            common::format_hook_messages_for_instance(db, &prepared.messages, instance_name);
        let model_context =
            common::format_messages_json_for_instance(db, &prepared.messages, instance_name);

        system_message = Some(user_display);
        contexts.push(model_context);
        delivery_ack = Some(prepared.ack);
    }

    if !contexts.is_empty() {
        let mut output = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": contexts.join("\n\n---\n\n"),
            },
        });
        if let Some(message) = system_message {
            output
                .as_object_mut()
                .unwrap()
                .insert("systemMessage".into(), Value::String(message));
        }
        if delivery_ack.is_none() {
            lifecycle::set_status(db, instance_name, ST_ACTIVE, "prompt", Default::default());
        }
        return (
            0,
            serde_json::to_string(&output).unwrap_or_default(),
            delivery_ack,
        );
    }

    lifecycle::set_status(db, instance_name, ST_ACTIVE, "prompt", Default::default());
    if is_bare_hcom_wake(payload) {
        return block_bare_hcom_wake(
            "This hcom wake arrived after another hook handled the queued delivery. Nothing is pending; no action is needed.",
        );
    }
    (0, String::new(), None)
}

/// Parent PermissionRequest: mark instance blocked immediately on approval UI.
fn handle_permission_request(
    db: &HcomDb,
    payload: &HookPayload,
    instance_name: &str,
    updates: &serde_json::Map<String, Value>,
) -> (i32, String) {
    if !updates.is_empty() {
        instances::update_instance_position(db, instance_name, updates);
    }

    let detail = family::extract_tool_detail("claude", &payload.tool_name, &payload.tool_input);
    lifecycle::set_status(
        db,
        instance_name,
        ST_BLOCKED,
        "approval",
        lifecycle::StatusUpdate {
            detail: &detail,
            tool_name: &payload.tool_name,
            tool_use_id: tool_use_id(payload).unwrap_or(""),
            ..Default::default()
        },
    );
    (0, String::new())
}

/// Parent Notification: map Claude notification types to hcom lifecycle state.
fn handle_notify(
    db: &HcomDb,
    payload: &HookPayload,
    instance_name: &str,
    updates: &serde_json::Map<String, Value>,
) -> (i32, String) {
    if !updates.is_empty() {
        instances::update_instance_position(db, instance_name, updates);
    }

    let message = payload
        .raw
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match payload.notification_type.as_deref() {
        Some("idle_prompt") => {
            lifecycle::set_status(db, instance_name, ST_LISTENING, "", Default::default());
            common::notify_hook_instance_with_db(db, instance_name);
            return (0, String::new());
        }
        Some("permission_prompt") => {
            // PermissionRequest owns blocked state and carries tool detail.
            return (0, String::new());
        }
        Some("elicitation_dialog") => {
            lifecycle::set_status(
                db,
                instance_name,
                ST_BLOCKED,
                "approval",
                Default::default(),
            );
            return (0, String::new());
        }
        Some("auth_success") => return (0, String::new()),
        Some(other) => {
            log::log_warn(
                "hooks",
                "claude.notify.unknown_type",
                &format!("instance={} notification_type={}", instance_name, other),
            );
        }
        None => {}
    }

    // Back-compat fallback for older Claude payloads that only include free-form text.
    if message == "Claude is waiting for your input" {
        lifecycle::set_status(db, instance_name, ST_LISTENING, "", Default::default());
        common::notify_hook_instance_with_db(db, instance_name);
        return (0, String::new());
    }
    if message.starts_with("Claude needs your permission") {
        lifecycle::set_status(
            db,
            instance_name,
            ST_BLOCKED,
            "approval",
            Default::default(),
        );
        return (0, String::new());
    }
    (0, String::new())
}

/// Parent SessionEnd: finalize session and stop instance.
fn handle_sessionend(
    db: &HcomDb,
    instance_name: &str,
    session_id: &str,
    raw: &Value,
    updates: &serde_json::Map<String, Value>,
) -> (i32, String) {
    let reason = raw
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let is_primary = db
        .get_instance_full(instance_name)
        .ok()
        .flatten()
        .and_then(|instance| instance.session_id)
        .as_deref()
        == Some(session_id);
    if !is_primary {
        log::log_warn(
            "hooks",
            "sessionend.historical_ignored",
            &format!(
                "instance={} incoming_session_id={} reason={} primary_mutation=false",
                instance_name, session_id, reason
            ),
        );
        cleanup_sessionend_scoped_state(db, session_id);
        return (0, String::new());
    }

    common::finalize_session(
        db,
        instance_name,
        reason,
        if updates.is_empty() {
            None
        } else {
            Some(updates)
        },
    );

    cleanup_sessionend_scoped_state(db, session_id);

    (0, String::new())
}

fn cleanup_sessionend_scoped_state(db: &HcomDb, session_id: &str) {
    // The lineage validation cache is per session generation and nothing else
    // ever deletes it, so an ended generation would leak a kv row forever. A
    // generation that comes back is re-validated by its next SessionStart.
    if let Err(error) = db.clear_claude_session_validation(session_id) {
        log::log_warn(
            "hooks",
            "sessionend.validation_cache_cleanup_failed",
            &format!("session_id={} err={}", session_id, error),
        );
    }

    if let Err(error) = db.revoke_claude_actor_capabilities_for_session(session_id) {
        log::log_warn(
            "hooks",
            "sessionend.actor_cleanup_failed",
            &format!("session_id={} err={}", session_id, error),
        );
    }

    // Root session is over: no further SubagentStop invocations can fire for
    // it (see `subagent_stop_inflight_key`). Inflight stop records are
    // transient and self-recover when their owning process dies, but
    // sweeping them here avoids retaining unreachable session data.
    for (label, prefix) in [(
        "subagent_stop_inflight",
        subagent_stop_inflight_prefix(session_id),
    )] {
        if let Err(e) = db.kv_delete_prefix(&prefix) {
            log::log_warn(
                "hooks",
                "sessionend.kv_cleanup_failed",
                &format!("kind={} session_id={} err={}", label, session_id, e),
            );
        }
    }
}

/// Refresh a child row's last-seen lease without changing its identity or
/// lifecycle ownership. Every hook emitted inside a child calls this.
fn refresh_subagent_last_seen(db: &HcomDb, instance_name: &str) {
    let _ = db.conn().execute(
        "UPDATE instances SET last_seen = ?
         WHERE name = ? AND parent_name IS NOT NULL",
        rusqlite::params![crate::shared::time::now_epoch_i64(), instance_name],
    );
}

/// Mark quiet child rows stale for display only. Staleness never authorizes
/// routing and never blocks start, stop, send, or listen.
fn mark_stale_subagents(db: &HcomDb, session_id: &str) {
    let timeout = db
        .get_session_binding(session_id)
        .ok()
        .flatten()
        .and_then(|name| db.get_instance_full(&name).ok().flatten())
        .and_then(|row| row.subagent_timeout)
        .unwrap_or_else(|| {
            HcomConfig::load(None)
                .ok()
                .map(|config| config.subagent_timeout)
                .unwrap_or(120)
        });
    let cutoff = crate::shared::time::now_epoch_i64().saturating_sub(timeout.saturating_mul(2));

    let _ = db.conn().execute(
        "UPDATE instances
         SET status = ?, status_context = 'subagent:stale',
             status_detail = 'No recent child hook; lifecycle uncertain'
         WHERE parent_session_id = ?
           AND last_seen > 0 AND last_seen < ?
           AND status_context != 'subagent:stale'",
        rusqlite::params![ST_INACTIVE, session_id, cutoff],
    );
}

/// SubagentStart: surface agent_id and root parent to subagent.
fn build_subagent_start_output(raw: &Value, root_name: &str) -> Option<Value> {
    let agent_id = raw.get("agent_id").and_then(|v| v.as_str())?;
    if agent_id.is_empty() {
        return None;
    }

    let hcom_cmd = crate::runtime_env::build_hcom_command();
    let additional_context = format!(
        "Your agent ID: {agent_id}\n\
         Your hcom parent: {root_name}\n\
         To use hcom, first run: {hcom_cmd} start --name {agent_id}"
    );

    Some(serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SubagentStart",
            "additionalContext": additional_context,
        }
    }))
}

/// Allocate (or adopt) an `instances` row for this subagent at SubagentStart,
/// so it's visible in the TUI and addressable by `hcom send` without any
/// change to the subagent's own context. The row stays dormant
/// (status_context=`subagent:dormant`, name_announced=0) until SubagentStop
/// activates it or the subagent runs `hcom start --name <agent_id>`.
///
/// Idempotent: calls into `allocate_subagent_instance`, which returns the
/// existing row's name if one already exists for this agent_id.
///
/// `parent_instance` (the row's `parent_name`) is always the root hcom
/// instance — every subagent, at any depth Claude nests it, attaches
/// directly to root; see `handle_subagent_start`. `root_session_id` is
/// always the real Claude session_id (the root's own), never a subagent's,
/// because subagent rows never get their own `session_id` (see
/// `allocate_subagent_instance`) — it is the only value the
/// `parent_session_id` FK column (`REFERENCES instances(session_id)`) can
/// ever validly point at.
/// Allocate this subagent's `instances` row and return its stable name.
fn ensure_subagent_row(
    db: &HcomDb,
    parent_instance: &str,
    root_session_id: &str,
    agent_id: &str,
    agent_type: &str,
) -> Option<String> {
    let parent_data = match db.get_instance_full(parent_instance) {
        Ok(Some(data)) => data,
        _ => return None,
    };
    let parent_tag = parent_data.tag.as_deref();

    let alloc = instance_names::SubagentAllocation {
        agent_id,
        agent_type,
        parent_name: parent_instance,
        parent_session_id: Some(root_session_id),
        parent_tag,
        status: ST_INACTIVE,
        status_context: Some("subagent:dormant"),
    };

    match instance_names::allocate_subagent_instance(db, &alloc) {
        Ok(name) => {
            log::log_info(
                "hooks",
                "subagent.row.ensured",
                &format!(
                    "name={} parent={} agent_id={} type={}",
                    name, parent_instance, agent_id, agent_type
                ),
            );
            Some(name)
        }
        Err(e) => {
            log::log_warn(
                "hooks",
                "subagent.row.alloc_failed",
                &format!("agent_id={} err={}", agent_id, e),
            );
            None
        }
    }
}

/// Deterministic, bounded fingerprint of a raw hook payload for use inside a
/// SQLite key. Payloads can carry unbounded fields (e.g. a transcript
/// excerpt), so the payload itself must never be embedded directly into a
/// key — hash it instead. Not for cryptographic use, just content-addressing
/// two invocations as "the same bytes" or not.
fn hash_raw_payload(raw: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(raw.to_string().as_bytes());
    let hash = hasher.finalize();
    hash.iter().map(|b| format!("{b:02x}")).take(16).collect()
}

/// kv-table key claiming exactly-once processing for *one specific*
/// SubagentStop invocation — not the agent_id alone, and not `prompt_id`
/// alone either. Claude legitimately re-fires SubagentStop for the same
/// agent_id (and, as far as we've established, possibly the same
/// `prompt_id` too — SubagentStop is explicitly designed to fire again after
/// a `decision:"block"` delivery within the same agent prompt/continuation,
/// and we have not confirmed Claude changes `prompt_id` for that re-fire) across the
/// message-poll loop and on resume, so a claim keyed on either alone risks
/// wrongly suppressing a later, genuinely different stop. The full raw
/// payload is hashed instead: two hook registrations firing the *identical*
/// event (e.g. hcom installed in both global and repo Claude settings, seen
/// live) produce byte-identical stdin and collapse onto the same key, while
/// any real difference between invocations (delivered message content,
/// background/transcript state) changes the hash and gets its own key.
/// `prompt_id` is folded in only as a readable, non-load-bearing component.
fn subagent_stop_inflight_key(root_session_id: &str, agent_id: &str, raw: &Value) -> String {
    let prompt_id = raw.get("prompt_id").and_then(|v| v.as_str()).unwrap_or("");
    format!(
        "subagent_stop_inflight:{root_session_id}:{agent_id}:{prompt_id}:{}",
        hash_raw_payload(raw)
    )
}

/// kv-table key prefix covering every SubagentStop claim for one root
/// session. See `handle_sessionend`.
fn subagent_stop_inflight_prefix(root_session_id: &str) -> String {
    format!("subagent_stop_inflight:{root_session_id}:")
}

/// Transient claim that prevents concurrent duplicate SubagentStop hooks from
/// polling or tearing down the same actor at the same time. Persisting the PID
/// plus its start identity makes a claim recoverable after SIGKILL without
/// mistaking a reused PID for the original owner.
#[derive(Deserialize, Serialize)]
struct SubagentStopOwner {
    owner_token: String,
    pid: u32,
    process_start: String,
}

struct SubagentStopClaim<'a> {
    db: &'a HcomDb,
    key: String,
    value: String,
}

enum SubagentStopClaimResult<'a> {
    Acquired(SubagentStopClaim<'a>),
    Duplicate,
    RetryableError(String),
}

impl<'a> SubagentStopClaim<'a> {
    fn acquire(
        db: &'a HcomDb,
        root_session_id: &str,
        agent_id: &str,
        raw: &Value,
    ) -> SubagentStopClaimResult<'a> {
        let key = subagent_stop_inflight_key(root_session_id, agent_id, raw);
        let pid = std::process::id();
        let process_start = match crate::sys::process::identity(pid) {
            Some(identity) => identity,
            None => {
                log::log_warn(
                    "hooks",
                    "subagent_stop.claim_identity_failed",
                    &format!("pid={pid}"),
                );
                return SubagentStopClaimResult::RetryableError(
                    "could not identify the stop-hook process".to_string(),
                );
            }
        };
        let owner = SubagentStopOwner {
            owner_token: uuid::Uuid::new_v4().to_string(),
            pid,
            process_start,
        };
        let value = match serde_json::to_string(&owner) {
            Ok(value) => value,
            Err(e) => {
                return SubagentStopClaimResult::RetryableError(format!(
                    "could not encode stop ownership: {e}"
                ));
            }
        };

        // A claim can disappear or change between INSERT, read, and stale-owner
        // replacement. Retry those benign CAS races locally before asking
        // Claude to keep the subagent alive and invoke SubagentStop again.
        for _ in 0..3 {
            match db.conn().execute(
                "INSERT OR IGNORE INTO kv (key, value) VALUES (?, ?)",
                rusqlite::params![key, value],
            ) {
                Ok(1) => {
                    return SubagentStopClaimResult::Acquired(Self { db, key, value });
                }
                Ok(_) => {}
                Err(e) => {
                    log::log_warn(
                        "hooks",
                        "subagent_stop.claim_insert_failed",
                        &format!("key={key} err={e}"),
                    );
                    return SubagentStopClaimResult::RetryableError(format!(
                        "could not write stop ownership: {e}"
                    ));
                }
            }

            let old_value = match db.kv_get(&key) {
                Ok(Some(value)) => value,
                Ok(None) => continue,
                Err(e) => {
                    log::log_warn(
                        "hooks",
                        "subagent_stop.claim_read_failed",
                        &format!("key={key} err={e}"),
                    );
                    return SubagentStopClaimResult::RetryableError(format!(
                        "could not read stop ownership: {e}"
                    ));
                }
            };
            if serde_json::from_str::<SubagentStopOwner>(&old_value)
                .is_ok_and(|old| crate::sys::process::has_identity(old.pid, &old.process_start))
            {
                return SubagentStopClaimResult::Duplicate;
            }

            // Replace a dead owner's record only if it is still the exact
            // value we inspected. Another contender may recover it first.
            match db.conn().execute(
                "UPDATE kv SET value = ? WHERE key = ? AND value = ?",
                rusqlite::params![value, key, old_value],
            ) {
                Ok(1) => {
                    return SubagentStopClaimResult::Acquired(Self { db, key, value });
                }
                Ok(_) => continue,
                Err(e) => {
                    log::log_warn(
                        "hooks",
                        "subagent_stop.claim_replace_failed",
                        &format!("key={key} err={e}"),
                    );
                    return SubagentStopClaimResult::RetryableError(format!(
                        "could not replace stale stop ownership: {e}"
                    ));
                }
            }
        }

        SubagentStopClaimResult::RetryableError(
            "stop ownership changed repeatedly during acquisition".to_string(),
        )
    }
}

impl Drop for SubagentStopClaim<'_> {
    fn drop(&mut self) {
        let _ = self.db.conn().execute(
            "DELETE FROM kv WHERE key = ? AND value = ?",
            rusqlite::params![self.key, self.value],
        );
    }
}

/// SubagentStop: message polling using agent_id, cleanup on exit.
///
/// Always exits 0: Claude ignores stdout JSON on exit 2 for SubagentStop
/// (stderr-only feedback), so `decision:"block"` must go out as exit 0 for
/// Claude to see the delivered message and keep the subagent alive.
fn block_subagent_stop(error: impl std::fmt::Display) -> (i32, String, Option<DeliveryAck>) {
    let output = serde_json::json!({
        "decision": "block",
        "reason": format!("hcom could not finish SubagentStop: {error}. Please stop again."),
    });
    (0, output.to_string(), None)
}

fn subagent_stop(
    db: &HcomDb,
    root_session_id: &str,
    raw: &Value,
) -> (i32, String, Option<DeliveryAck>) {
    let agent_id = match raw.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return (0, String::new(), None),
    };

    // Claim before reading the row. Otherwise duplicate hook processes can all
    // observe the row, wait for the first claim to be released, and then each
    // perform a second teardown against their stale copy of that row.
    let _stop_claim = match SubagentStopClaim::acquire(db, root_session_id, agent_id, raw) {
        SubagentStopClaimResult::Acquired(claim) => claim,
        SubagentStopClaimResult::Duplicate => return (0, String::new(), None),
        SubagentStopClaimResult::RetryableError(error) => return block_subagent_stop(error),
    };

    // Query subagent instance by agent_id
    let row: Option<(String, String, Option<String>, i64)> = match db.conn().query_row(
        "SELECT name, transcript_path, parent_name, name_announced \
             FROM instances WHERE agent_id = ?",
        rusqlite::params![agent_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        },
    ) {
        Ok(row) => Some(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return block_subagent_stop(format!("could not read subagent state: {e}")),
    };

    let Some((subagent_name, existing_transcript, parent_name, name_announced)) = row else {
        return (0, String::new(), None);
    };
    refresh_subagent_last_seen(db, &subagent_name);

    // Store transcript_path if not already set
    if existing_transcript.is_empty()
        && let Some(tp) = raw.get("agent_transcript_path").and_then(|v| v.as_str())
        && !tp.is_empty()
    {
        let mut updates = serde_json::Map::new();
        updates.insert("transcript_path".into(), Value::String(tp.to_string()));
        instances::update_instance_position(db, &subagent_name, &updates);
    }

    // Idle gate: a dormant subagent (never opted in via `hcom start`, never
    // had a message delivered) only wakes for *direct* mentions. Broadcasts
    // are visible to its row but are not enough to keep it alive — that
    // would break the "no message in → no keep-alive" contract, since
    // SubagentStart now puts every subagent in the broadcast recipient set.
    let dormant = name_announced == 0;
    let has_direct = dormant && db.has_direct_unread(&subagent_name);
    if dormant && !has_direct {
        lifecycle::set_status(
            db,
            &subagent_name,
            ST_INACTIVE,
            "exit:idle",
            Default::default(),
        );
        match common::stop_instance(db, &subagent_name, "subagent", "idle") {
            common::StopOutcome::RetryableError(error) => return block_subagent_stop(error),
            common::StopOutcome::Stopped | common::StopOutcome::AlreadyStopped => {}
        }
        return (0, String::new(), None);
    }

    // Activation: dormant + a direct mention pending. Build the bootstrap
    // text now but DO NOT flip `name_announced` yet — only mark the row
    // announced once `poll_messages` actually returns a delivery. Otherwise
    // a transient poll failure (orphan stdin closed, row deleted mid-loop)
    // would burn the one-shot bootstrap with nothing to show for it.
    let activation_bootstrap = if dormant {
        let parent_display = match parent_name.as_deref() {
            Some(s) if !s.is_empty() => s,
            _ => {
                log::log_warn(
                    "hooks",
                    "subagent.activation.parent_missing",
                    &format!("name={subagent_name} agent_id={agent_id}"),
                );
                ""
            }
        };
        let bs = bootstrap::get_subagent_bootstrap(&subagent_name, parent_display);
        if bs.is_empty() { None } else { Some(bs) }
    } else {
        None
    };

    // Resolve timeout: parent override > global config
    let timeout = parent_name
        .as_ref()
        .and_then(|pn| db.get_instance_full(pn).ok().flatten())
        .and_then(|pd| pd.subagent_timeout)
        .unwrap_or_else(|| {
            HcomConfig::load(None)
                .ok()
                .map(|c| c.subagent_timeout)
                .unwrap_or(120)
        }) as u64;

    let mut result = common::poll_messages(db, &subagent_name, timeout, false);

    // On first-activation delivery, prepend the subagent bootstrap to the
    // `reason` field so Claude injects both as a single user message on the
    // subagent's next turn. `name_announced` is only flipped once the caller
    // commits `result.ack` after this stdout is actually flushed — if
    // delivery never lands (crash, orphan, timeout) the bootstrap is
    // preserved for the next SubagentStop instead of being burned.
    let stdout = match (&result.output, activation_bootstrap.as_deref()) {
        (Some(Value::Object(obj)), Some(bs)) => {
            let mut munged = obj.clone();
            let existing_reason = munged
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let combined = if existing_reason.is_empty() {
                bs.to_string()
            } else {
                format!("{bs}\n\n{existing_reason}")
            };
            munged.insert("reason".into(), Value::String(combined));
            if let Some(ack) = result.ack.as_mut() {
                ack.mark_announced = true;
            }
            serde_json::to_string(&Value::Object(munged)).unwrap_or_default()
        }
        (Some(v), _) => serde_json::to_string(v).unwrap_or_default(),
        (None, _) => String::new(),
    };

    if !result.delivered {
        let reason = if result.timed_out {
            "timeout"
        } else {
            "task_completed"
        };
        lifecycle::set_status(
            db,
            &subagent_name,
            ST_INACTIVE,
            &format!("exit:{}", reason),
            Default::default(),
        );
        match common::stop_instance(db, &subagent_name, "subagent", reason) {
            common::StopOutcome::RetryableError(error) => return block_subagent_stop(error),
            common::StopOutcome::Stopped | common::StopOutcome::AlreadyStopped => {}
        }
    }

    // Always exit 0: Claude consumes decision:block stdout only on exit 0.
    // The dispatcher commits this deferred ack after stdout is flushed.
    (0, stdout, result.ack)
}

/// Subagent PostToolUse: message delivery for subagents running hcom commands.
///
/// Returns (exit_code, stdout).
fn subagent_posttooluse(db: &HcomDb, subagent_name: &str) -> (i32, String, Option<DeliveryAck>) {
    if db.get_instance_full(subagent_name).ok().flatten().is_none() {
        return (0, String::new(), None);
    }

    // Message delivery
    let Some(prepared) = common::prepare_pending_messages(db, subagent_name) else {
        return (0, String::new(), None);
    };
    let formatted =
        common::format_messages_json_for_instance(db, &prepared.messages, subagent_name);

    let output = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": formatted,
        }
    });
    (
        0,
        serde_json::to_string(&output).unwrap_or_default(),
        Some(prepared.ack),
    )
}

//
// Manages hook installation in ~/.claude/settings.json.

use super::common::SAFE_HCOM_COMMANDS;

/// Hook configuration: (hook_type, matcher, command_suffix, timeout_secs).
/// Single source of truth — all hook properties derived from this.
pub(crate) const CLAUDE_HOOK_CONFIGS: &[(&str, &str, &str, Option<u64>)] = &[
    ("SessionStart", "", "sessionstart", None),
    ("UserPromptSubmit", "", "userpromptsubmit", None),
    (
        "PreToolUse",
        "Bash|PowerShell|Agent|Task|Write|Edit",
        "pre",
        None,
    ),
    ("PostToolUse", "", "post", Some(86400)),
    ("PostToolUseFailure", "", "post-failure", None),
    ("Stop", "", "poll", Some(86400)),
    ("StopFailure", "", "stop-failure", None),
    ("PermissionRequest", "", "permission-request", None),
    ("PermissionDenied", "", "permission-denied", None),
    ("SubagentStart", "", "subagent-start", None),
    ("SubagentStop", "", "subagent-stop", Some(86400)),
    ("Notification", "", "notify", None),
    ("SessionEnd", "", "sessionend", None),
];

/// Hook command suffixes for pattern detection.
pub(crate) const CLAUDE_HOOK_COMMANDS: &[&str] = &[
    "sessionstart",
    "userpromptsubmit",
    "pre",
    "post",
    "post-failure",
    "poll",
    "stop-failure",
    "permission-request",
    "permission-denied",
    "subagent-start",
    "subagent-stop",
    "notify",
    "sessionend",
];

/// Claude hook types for cleanup iteration.
const CLAUDE_HOOK_TYPES: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "PermissionRequest",
    "PermissionDenied",
    "SubagentStart",
    "SubagentStop",
    "Notification",
    "SessionEnd",
];

// Static regexes for hot-path hook command detection
static RE_HCOM_COMMANDS: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = CLAUDE_HOOK_COMMANDS.join("|");
    Regex::new(&format!(r"\bhcom\s+({})\b", pattern)).unwrap()
});
static RE_HCOM_CLAUDE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bhcom\s+claude-").unwrap());
static RE_UVX_HCOM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\buvx\s+hcom\s+claude-").unwrap());
static RE_HCOM_ACTIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bHCOM_ACTIVE.*hcom\.py").unwrap());
static RE_HCOM_PY_COMMANDS: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = CLAUDE_HOOK_COMMANDS.join("|");
    Regex::new(&format!(r#"hcom\.py["']?\s+({})\b"#, pattern)).unwrap()
});
static RE_SH_HCOM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"sh\s+-c.*hcom").unwrap());

/// Resolve the Claude config directory.
///
/// Priority: CLAUDE_CONFIG_DIR env var → tool_config_root()/.claude
fn claude_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    paths::get_project_root().join(".claude")
}

/// Get path to Claude settings.json.
///
/// Respects CLAUDE_CONFIG_DIR env var, then falls back to:
/// - HCOM_DIR set → project_root is HCOM_DIR parent → {parent}/.claude/settings.json
/// - Otherwise → ~/.hcom parent = ~ → ~/.claude/settings.json
pub fn get_claude_settings_path() -> PathBuf {
    claude_config_dir().join("settings.json")
}

/// Load and parse Claude settings.json. Returns None on error or missing file.
pub fn load_claude_settings(settings_path: &Path) -> Option<Value> {
    let content = std::fs::read_to_string(settings_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Build a hook command that silently exits 0 when hcom is not installed.
///
/// Claude already executes hook commands through a shell, so this command keeps
/// all shell logic inline instead of spawning another `sh -c`. It's self-contained:
/// it tries `$HCOM` (set in settings.json's env block, when present) or bare `hcom`,
/// and falls back to `uvx hcom` when that's not on PATH — the plugin manifest path
/// has no env block, so a uvx-only install must still resolve. When neither
/// resolves (e.g. after `brew uninstall hcom`), the hook exits 0 instead of
/// emitting a "command not found" error inside the tool.
pub(crate) fn build_hook_entry_command(cmd_suffix: &str) -> String {
    build_hook_entry_command_with(cmd_suffix, "", "exit 0")
}

/// Same self-resolving guard as [`build_hook_entry_command`], generalized for
/// callers whose command needs more than a bare `exec $cmd <suffix>`. Its real
/// consumers are the static Antigravity plugin manifest
/// (`plugin/hcom-agy/hooks/hooks.json`) and the test in `src/hooks/plugin.rs`
/// that pins it: that manifest both prefixes an env var to route the shared
/// `gemini-*` handler and, on two events, must still emit a decision JSON when
/// hcom is missing rather than plain `exit 0`. `src/hooks/antigravity.rs`
/// itself does not call this — its live installer has its own `hook_sh_cmd`.
pub(crate) fn build_hook_entry_command_with(
    cmd_suffix: &str,
    env_prefix: &str,
    on_missing: &str,
) -> String {
    // Claude runs hook commands through a POSIX shell on every platform
    // (Git Bash on Windows), so the same command works everywhere. Try
    // `${HCOM:-hcom}` first, fall back to `uvx hcom` if that's not on PATH,
    // then the `command -v` guard runs `on_missing` if neither is.
    format!(
        "cmd=${{HCOM:-hcom}}; command -v \"${{cmd%% *}}\" >/dev/null 2>&1 || cmd=\"uvx hcom\"; command -v \"${{cmd%% *}}\" >/dev/null 2>&1 && {}exec $cmd {} || {}",
        env_prefix, cmd_suffix, on_missing
    )
}

/// Format a single Claude permission pattern: `Bash(prefix cmd:*)`.
fn format_claude_permission(prefix: &str, cmd: &str) -> String {
    let suffix = if cmd.starts_with('-') { "" } else { ":*" };
    format!("Bash({} {}{})", prefix, cmd, suffix)
}

/// Format a single Claude permission pattern for the PowerShell tool:
/// `PowerShell(prefix cmd:*)`.
///
/// Claude Code uses the Bash tool on Windows when Git for Windows is present,
/// and falls back to a separate PowerShell tool (same rule syntax as Bash)
/// otherwise, so both patterns are installed to cover either case.
fn format_claude_powershell_permission(prefix: &str, cmd: &str) -> String {
    let suffix = if cmd.starts_with('-') { "" } else { ":*" };
    format!("PowerShell({} {}{})", prefix, cmd, suffix)
}

/// Build permission patterns for installation using detected prefix.
fn build_claude_permissions() -> Vec<String> {
    let prefix = crate::runtime_env::build_hcom_command();
    let mut patterns: Vec<String> = SAFE_HCOM_COMMANDS
        .iter()
        .map(|cmd| format_claude_permission(&prefix, cmd))
        .chain(
            SAFE_HCOM_COMMANDS
                .iter()
                .map(|cmd| format_claude_powershell_permission(&prefix, cmd)),
        )
        .collect();
    patterns.extend(claude_actor_permission_patterns());
    patterns
}

/// Permission rules for the trusted actor prelude injected into subagent
/// shell calls. Claude parses compound commands at shell boundaries, so these
/// rules cover only the environment-setting statements; every original
/// command remains subject to its own normal allow/ask/deny decision.
fn claude_actor_permission_patterns() -> Vec<String> {
    vec![
        format!(
            "Bash(export {}=* {}=*)",
            crate::claude_actor::ENV_VAR,
            crate::claude_actor::SESSION_ENV_VAR
        ),
        format!("PowerShell($env:{} = *)", crate::claude_actor::ENV_VAR),
        format!(
            "PowerShell($env:{} = *)",
            crate::claude_actor::SESSION_ENV_VAR
        ),
    ]
}

/// Legacy commands that were once in SAFE_HCOM_COMMANDS or auto-approved.
/// Kept here so removal cleans up permissions from older installs.
const LEGACY_HCOM_COMMANDS: &[&str] = &["daemon"];

/// Build ALL permission patterns (both "hcom" and "uvx hcom" prefixes) for removal.
fn build_all_claude_permission_patterns() -> Vec<String> {
    let mut patterns = Vec::new();
    for prefix in &["hcom", "uvx hcom"] {
        for cmd in SAFE_HCOM_COMMANDS.iter().chain(LEGACY_HCOM_COMMANDS.iter()) {
            patterns.push(format_claude_permission(prefix, cmd));
            patterns.push(format_claude_powershell_permission(prefix, cmd));
        }
    }
    patterns.extend(claude_actor_permission_patterns());
    patterns
}

/// Check if a hook command string matches any hcom hook pattern.
fn is_hcom_hook_command(command: &str) -> bool {
    // Env var patterns: ${HCOM} or %HCOM%
    if command.contains("${HCOM}")
        || command.contains("$HCOM")
        || command.contains("%HCOM%")
        || command.contains("${HCOM:-")
    {
        return true;
    }

    // Standard patterns: hcom <hook_command>
    if RE_HCOM_COMMANDS.is_match(command) {
        return true;
    }

    // Tool prefix pattern: hcom claude-
    if RE_HCOM_CLAUDE.is_match(command) {
        return true;
    }

    // uvx pattern: uvx hcom claude-
    if RE_UVX_HCOM.is_match(command) {
        return true;
    }

    // Legacy patterns
    if RE_HCOM_ACTIVE.is_match(command) {
        return true;
    }
    if command.contains(r#"IF "%HCOM_ACTIVE%""#) {
        return true;
    }
    if RE_HCOM_PY_COMMANDS.is_match(command) {
        return true;
    }
    if RE_SH_HCOM.is_match(command) {
        return true;
    }

    false
}

/// Remove all hcom hooks from a Claude settings dictionary (in-place).
///
/// Scans all hook types and removes hooks whose command matches hcom patterns.
/// Also removes HCOM from env and hcom permission patterns from permissions.allow.
/// Returns true if any hooks/env/permissions were removed.
fn remove_hcom_hooks_from_settings(settings: &mut Value) -> bool {
    let mut removed_any = false;

    let obj = match settings.as_object_mut() {
        Some(o) => o,
        None => return false,
    };

    // Process each hook type (if hooks section exists)
    if let Some(hooks) = obj.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        for event in CLAUDE_HOOK_TYPES {
            let event_matchers = match hooks.get_mut(*event) {
                Some(v) => v,
                None => continue,
            };

            let matchers = match event_matchers.as_array_mut() {
                Some(a) => a,
                None => {
                    // Malformed event value (not a list) — skip and preserve
                    continue;
                }
            };

            let mut updated_matchers = Vec::new();
            for matcher in matchers.iter() {
                let matcher_obj = match matcher.as_object() {
                    Some(o) => o,
                    None => {
                        // Malformed matcher — preserve as-is
                        updated_matchers.push(matcher.clone());
                        continue;
                    }
                };

                let hooks_field = match matcher_obj.get("hooks") {
                    Some(v) => match v.as_array() {
                        Some(a) => a,
                        None => {
                            // Malformed hooks field — preserve
                            updated_matchers.push(matcher.clone());
                            continue;
                        }
                    },
                    None => {
                        // No hooks field — preserve matcher
                        updated_matchers.push(matcher.clone());
                        continue;
                    }
                };

                // Filter out hcom hooks
                let non_hcom_hooks: Vec<&Value> = hooks_field
                    .iter()
                    .filter(|hook| {
                        let command = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                        !is_hcom_hook_command(command)
                    })
                    .collect();

                if non_hcom_hooks.len() < hooks_field.len() {
                    removed_any = true;
                }

                // Only keep matcher if it has non-hcom hooks remaining
                if !non_hcom_hooks.is_empty() {
                    let mut matcher_copy = matcher.clone();
                    matcher_copy["hooks"] =
                        Value::Array(non_hcom_hooks.into_iter().cloned().collect());
                    updated_matchers.push(matcher_copy);
                }
                // If all hooks were hcom, drop the entire matcher
            }

            if updated_matchers.is_empty() {
                hooks.remove(*event);
            } else {
                hooks.insert(event.to_string(), Value::Array(updated_matchers));
            }
        }
    }

    // Remove HCOM from env section
    if let Some(env) = obj.get_mut("env").and_then(|v| v.as_object_mut()) {
        if env.remove("HCOM").is_some() {
            removed_any = true;
        }
        if env.is_empty() {
            obj.remove("env");
        }
    }

    // Remove hcom permission patterns
    if let Some(perms) = obj.get_mut("permissions").and_then(|v| v.as_object_mut()) {
        if let Some(allow) = perms.get_mut("allow").and_then(|v| v.as_array_mut()) {
            let all_patterns = build_all_claude_permission_patterns();
            let original_len = allow.len();
            allow.retain(|p| {
                let s = p.as_str().unwrap_or("");
                !all_patterns.iter().any(|pat| pat == s)
            });
            if allow.len() < original_len {
                removed_any = true;
            }
            if allow.is_empty() {
                perms.remove("allow");
            }
        }
        if perms.is_empty() {
            obj.remove("permissions");
        }
    }

    removed_any
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum VerifyFailReason {
    #[error("settings.json missing or not parseable as JSON")]
    SettingsUnreadable,
    #[error("'hooks' key missing or not an object")]
    HooksKeyMissing,
    #[error("hook type '{0}' missing or empty")]
    HookTypeMissing(String),
    #[error("hcom hook command '{cmd_suffix}' not found under hook type '{hook_type}'")]
    HookCommandMissing {
        hook_type: String,
        cmd_suffix: String,
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
    #[error("HCOM env var not set in settings.json")]
    HcomEnvMissing,
    #[error("'permissions.allow' missing or not an array")]
    PermissionsAllowMissing,
    #[error("required permission pattern not present: {0}")]
    PermissionMissing(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
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

/// Set up hcom hooks in Claude settings.json.
///
/// - Removes existing hcom hooks first (clean slate)
/// - Adds all hooks from CLAUDE_HOOK_CONFIGS
/// - Sets HCOM environment variable
/// - Optionally adds permission patterns
/// - Uses atomic write for concurrent safety
///
/// Installation does not grant workspace trust. Interactive Claude sessions
/// hold back settings-file hooks, including these user-level hooks, until the
/// directory or an ancestor is trusted. `hcom claude -b` uses `-p`, where this
/// settings-file trust gate does not apply. See:
/// https://code.claude.com/docs/en/hooks#workspace-trust
///
/// If hooks are installed but the session never binds, check Claude's trust
/// prompt and debug log before reinstalling. Tool-permission bypass flags do
/// not accept workspace trust. In the reconstructed Claude source,
/// `showSetupScreens` also skips the trust dialog when `IS_DEMO` is set; in an
/// untrusted directory this can leave hooks disabled without a visible prompt.
/// This internal environment variable is not a supported trust mechanism.
///
/// hcom leaves trust acceptance to Claude's dialog rather than coupling this
/// installer to Claude's internal trust-state schema. PTY launches can report
/// a stalled prompt through `launch_blocked`; ordinary `hcom claude` launches
/// use the user's terminal directly, without that watcher.
pub fn try_setup_claude_hooks(include_permissions: bool) -> Result<(), SetupError> {
    let settings_path = get_claude_settings_path();
    if let Some(parent) = settings_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut settings =
        load_claude_settings(&settings_path).unwrap_or_else(|| serde_json::json!({}));

    // Normalize hooks dict
    if !settings.get("hooks").is_some_and(|v| v.is_object()) {
        settings["hooks"] = serde_json::json!({});
    }

    // Remove existing hcom hooks
    remove_hcom_hooks_from_settings(&mut settings);

    for &(hook_type, matcher, cmd_suffix, timeout) in CLAUDE_HOOK_CONFIGS {
        // Initialize or normalize hook_type to array
        if !settings["hooks"]
            .get(hook_type)
            .is_some_and(|v| v.is_array())
        {
            settings["hooks"][hook_type] = serde_json::json!([]);
        }

        let mut hook_entry = serde_json::json!({
            "type": "command",
            "command": build_hook_entry_command(cmd_suffix),
        });

        if let Some(t) = timeout {
            hook_entry["timeout"] = serde_json::json!(t);
        }

        let mut hook_dict = serde_json::json!({
            "hooks": [hook_entry],
        });

        if !matcher.is_empty() {
            hook_dict["matcher"] = Value::String(matcher.to_string());
        }

        settings["hooks"][hook_type]
            .as_array_mut()
            .unwrap()
            .push(hook_dict);
    }

    // Set $HCOM environment variable
    if !settings.get("env").is_some_and(|v| v.is_object()) {
        settings["env"] = serde_json::json!({});
    }
    settings["env"]["HCOM"] = Value::String(crate::runtime_env::build_hcom_command());
    // Remove stale HCOM_DIR from settings
    if let Some(env) = settings["env"].as_object_mut() {
        env.remove("HCOM_DIR");
    }

    // Handle permission patterns
    if include_permissions {
        if !settings.get("permissions").is_some_and(|v| v.is_object()) {
            settings["permissions"] = serde_json::json!({});
        }
        if !settings["permissions"]
            .get("allow")
            .is_some_and(|v| v.is_array())
        {
            settings["permissions"]["allow"] = serde_json::json!([]);
        }
        if let Some(allow) = settings["permissions"]["allow"].as_array_mut() {
            for pattern in build_claude_permissions() {
                if !allow.iter().any(|p| p.as_str() == Some(&pattern)) {
                    allow.push(Value::String(pattern));
                }
            }
        }
    } else {
        // Remove hcom permissions if disabled
        if let Some(perms) = settings
            .get_mut("permissions")
            .and_then(|v| v.as_object_mut())
        {
            if let Some(allow) = perms.get_mut("allow").and_then(|v| v.as_array_mut()) {
                let hcom_perms = build_all_claude_permission_patterns();
                allow.retain(|p| {
                    let s = p.as_str().unwrap_or("");
                    !hcom_perms.iter().any(|pat| pat == s)
                });
                if allow.is_empty() {
                    perms.remove("allow");
                }
            }
            if perms.is_empty() {
                settings.as_object_mut().unwrap().remove("permissions");
            }
        }
    }

    let json_str =
        serde_json::to_string_pretty(&settings).map_err(SetupError::SerializationFailed)?;

    paths::atomic_write_io(&settings_path, &json_str).map_err(|e| {
        SetupError::AtomicWriteFailed {
            path: settings_path.clone(),
            source: e,
        }
    })?;

    // Re-read from disk: catches truncation, FS-layer corruption, and
    // concurrent overwrite by another process between rename and verify.
    verify_claude_hooks_inner(Some(&settings_path), include_permissions).map_err(|reason| {
        SetupError::PostWriteVerifyFailed {
            path: settings_path,
            reason,
        }
    })?;

    Ok(())
}

pub fn setup_claude_hooks(include_permissions: bool) -> bool {
    try_setup_claude_hooks(include_permissions).is_ok()
}

/// Verify hcom hooks are installed in Claude settings. Every hook that
/// carries a timeout in `CLAUDE_HOOK_CONFIGS` must have a numeric `timeout`
/// field — the value itself is not checked, so user edits still pass.
pub fn verify_claude_hooks_installed(
    settings_path: Option<&Path>,
    check_permissions: bool,
) -> bool {
    verify_claude_hooks_inner(settings_path, check_permissions).is_ok()
}

fn verify_claude_hooks_inner(
    settings_path: Option<&Path>,
    check_permissions: bool,
) -> Result<(), VerifyFailReason> {
    let default_path = get_claude_settings_path();
    let path = settings_path.unwrap_or(&default_path);

    let settings = load_claude_settings(path).ok_or(VerifyFailReason::SettingsUnreadable)?;

    let hooks = settings
        .get("hooks")
        .and_then(|v| v.as_object())
        .ok_or(VerifyFailReason::HooksKeyMissing)?;

    for &(hook_type, expected_matcher, cmd_suffix, expected_timeout) in CLAUDE_HOOK_CONFIGS {
        let hook_matchers = match hooks.get(hook_type).and_then(|v| v.as_array()) {
            Some(a) if !a.is_empty() => a,
            _ => return Err(VerifyFailReason::HookTypeMissing(hook_type.to_string())),
        };

        let mut hcom_hook_found = false;
        for matcher_dict in hook_matchers {
            let matcher_obj = match matcher_dict.as_object() {
                Some(o) => o,
                None => continue,
            };
            let hooks_list = match matcher_obj.get("hooks").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => continue,
            };

            for hook in hooks_list {
                let command = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let has_hcom =
                    command.contains("${HCOM}") || command.to_lowercase().contains("hcom");
                if has_hcom && command.contains(cmd_suffix) {
                    if hcom_hook_found {
                        return Err(VerifyFailReason::HookDuplicated(hook_type.to_string()));
                    }

                    if expected_timeout.is_some()
                        && hook.get("timeout").and_then(|v| v.as_u64()).is_none()
                    {
                        return Err(VerifyFailReason::HookTimeoutMissing {
                            hook_type: hook_type.to_string(),
                        });
                    }

                    let actual_matcher = matcher_obj
                        .get("matcher")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if actual_matcher != expected_matcher {
                        return Err(VerifyFailReason::HookMatcherMismatch {
                            hook_type: hook_type.to_string(),
                            expected: expected_matcher.to_string(),
                            actual: actual_matcher.to_string(),
                        });
                    }

                    hcom_hook_found = true;
                }
            }
        }

        if !hcom_hook_found {
            return Err(VerifyFailReason::HookCommandMissing {
                hook_type: hook_type.to_string(),
                cmd_suffix: cmd_suffix.to_string(),
            });
        }
    }

    if settings.get("env").and_then(|v| v.get("HCOM")).is_none() {
        return Err(VerifyFailReason::HcomEnvMissing);
    }

    if check_permissions {
        let allow = settings
            .get("permissions")
            .and_then(|v| v.get("allow"))
            .and_then(|v| v.as_array())
            .ok_or(VerifyFailReason::PermissionsAllowMissing)?;
        for pattern in build_claude_permissions() {
            if !allow.iter().any(|p| p.as_str() == Some(&pattern)) {
                return Err(VerifyFailReason::PermissionMissing(pattern));
            }
        }
    }

    Ok(())
}

/// Remove hcom hooks from a specific settings path. Returns true on success.
fn remove_hooks_from_settings_path(settings_path: &Path) -> bool {
    if !settings_path.exists() {
        return true;
    }

    let mut settings = match load_claude_settings(settings_path) {
        Some(s) => s,
        // An empty file is fine — nothing to strip. A file with bytes we cannot
        // parse is not: hcom's entries may still be in there, so reporting
        // success would tell the user the migration finished while both hook
        // sets keep firing. Never rewrite a file we failed to understand.
        None => {
            let empty = std::fs::read_to_string(settings_path)
                .map(|c| c.trim().is_empty())
                .unwrap_or(false);
            if !empty {
                crate::log::log_warn(
                    "hooks",
                    "claude.strip.unparseable",
                    &format!(
                        "left {} untouched: not valid JSON, so any hcom entries remain",
                        settings_path.display()
                    ),
                );
                return false;
            }
            return true;
        }
    };

    if !settings.is_object() {
        crate::log::log_warn(
            "hooks",
            "claude.strip.not_an_object",
            &format!(
                "left {} untouched: root is not a JSON object",
                settings_path.display()
            ),
        );
        return false;
    }

    remove_hcom_hooks_from_settings(&mut settings);

    let json_str = match serde_json::to_string_pretty(&settings) {
        Ok(s) => s,
        Err(_) => return false,
    };

    paths::atomic_write(settings_path, &json_str)
}

/// Remove hcom hooks from Claude settings.
///
/// Cleans both global (~/.claude/settings.json) and local (HCOM_DIR-based) paths.
/// Only removes hcom-specific hooks, not the whole file.
pub fn remove_claude_hooks() -> bool {
    let global_path = dirs::home_dir()
        .map(|h| h.join(".claude").join("settings.json"))
        .unwrap_or_default();
    let env_path = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("settings.json"));
    let local_path = get_claude_settings_path();

    let global_ok = remove_hooks_from_settings_path(&global_path);
    let env_ok = match env_path {
        Some(ref p) if *p != global_path => remove_hooks_from_settings_path(p),
        _ => true,
    };
    let local_ok = if local_path != global_path && Some(&local_path) != env_path.as_ref() {
        remove_hooks_from_settings_path(&local_path)
    } else {
        true
    };

    global_ok && env_ok && local_ok
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
