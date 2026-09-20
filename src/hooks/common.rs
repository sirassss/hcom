//! Shared hook functions — deliver, poll, bind, bootstrap, finalize.

use std::collections::BTreeSet;
use std::io::Read;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::params;
use serde_json::Value;

use crate::bootstrap;
use crate::db::{HcomDb, InstanceRow, Message};
use crate::identity;
use crate::instance_lifecycle as lifecycle;
use crate::instances;
use crate::log;
use crate::messages;
use crate::shared::constants::MAX_MESSAGES_PER_DELIVERY;
use crate::shared::context::HcomContext;
use crate::shared::{ST_ACTIVE, ST_INACTIVE, ST_LISTENING};

/// Run a hook handler with panic safety.
///
/// Catches panics in the handler closure, logs them, and returns the fallback
/// value instead of crashing the host process. Used by all tool dispatchers.
pub(crate) fn dispatch_with_panic_guard<R>(
    tool: &str,
    hook_name: &str,
    fallback: R,
    f: impl FnOnce() -> R,
) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            log::log_error(
                "hooks",
                &format!("{tool}.dispatch.panic"),
                &format!("hook={hook_name}"),
            );
            fallback
        }
    }
}

/// Commands auto-approved in tool permission rules (Claude/Gemini/Codex settings).
///
/// Included: read-only queries, messaging, and session lifecycle commands that
/// agents need to run without user approval prompts.
/// Excluded: `stop`, `kill`, `run`, `reset` — these are destructive or
/// admin-level and require explicit user approval.
pub(crate) const SAFE_HCOM_COMMANDS: &[&str] = &[
    "send",
    "start",
    "help",
    "--help",
    "-h",
    "list",
    "events",
    "listen",
    "relay",
    "config",
    "transcript",
    "archive",
    "bundle",
    "status",
    "term",
    "hooks",
    "--version",
    "-v",
    "--new-terminal",
];

/// Pre-gate check: should hooks proceed?
///
///
/// - HCOM-launched (process_id or is_launched) → always proceed
/// - Otherwise: check if DB has any instances → if not, skip (exit 0, empty output)
///
/// This prevents outputting hints/errors when hcom is installed but not actively used.
pub fn hook_gate_check(ctx: &HcomContext, db: &HcomDb) -> bool {
    if ctx.process_id.is_some() || ctx.is_launched {
        return true;
    }
    // Check if any instances exist — distinguish "no rows" from DB error
    match db
        .conn()
        .query_row("SELECT 1 FROM instances LIMIT 1", [], |_| Ok(()))
    {
        Ok(()) => true,
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        Err(e) => {
            log::log_warn(
                "hooks",
                "gate.db_error",
                &format!("hook gate DB check failed: {e}, proceeding anyway"),
            );
            true // On DB error, proceed rather than silently disabling hooks
        }
    }
}

/// Convert a db::Message to a serde_json::Value object.
pub(crate) fn message_to_value(m: &Message) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("from".into(), Value::String(m.from.clone()));
    obj.insert("message".into(), Value::String(m.text.clone()));
    if let Some(ref intent) = m.intent {
        obj.insert("intent".into(), Value::String(intent.clone()));
    }
    if let Some(ref thread) = m.thread {
        obj.insert("thread".into(), Value::String(thread.clone()));
    }
    if let Some(id) = m.event_id {
        obj.insert("event_id".into(), serde_json::json!(id));
    }
    if let Some(ref ts) = m.timestamp {
        obj.insert("timestamp".into(), Value::String(ts.clone()));
    }
    if let Some(ref delivered_to) = m.delivered_to {
        obj.insert("delivered_to".into(), serde_json::json!(delivered_to));
    }
    if let Some(ref bundle_id) = m.bundle_id {
        obj.insert("bundle_id".into(), Value::String(bundle_id.clone()));
    }
    Value::Object(obj)
}

/// Load config hints string (from instance-level or global config).
/// Call once per hook invocation and pass to format functions.
pub(crate) fn load_config_hints() -> String {
    crate::config::HcomConfig::load(None)
        .map(|c| c.hints.clone())
        .unwrap_or_default()
}

/// Build instance-data lookup function for message formatting.
pub(crate) fn make_instance_lookup(db: &HcomDb) -> impl Fn(&str) -> Option<Value> + '_ {
    |name: &str| db.get_instance(name).ok().flatten()
}

/// Build a tip-tracking callback for hook message formatting.
pub(crate) fn make_tip_checker(db: &HcomDb) -> impl Fn(&str, &str) -> (bool, Box<dyn Fn()>) + '_ {
    move |instance_name: &str, tip_key: &str| {
        let seen = crate::core::tips::has_seen_tip(db, instance_name, tip_key);
        let db_path = db.path().to_path_buf();
        let instance_name = instance_name.to_string();
        let tip_key = tip_key.to_string();
        let mark = Box::new(move || {
            if let Ok(mark_db) = HcomDb::open_at(&db_path) {
                crate::core::tips::mark_tip_seen(&mark_db, &instance_name, &tip_key);
            }
        }) as Box<dyn Fn()>;
        (seen, mark)
    }
}

/// Prepared delivery — messages formatted but cursor not yet advanced.
///
/// Used by tools that need to ensure stdout write succeeds before committing.
pub struct PreparedDelivery {
    pub messages: Vec<Value>,
    pub formatted: String,
    pub ack: super::DeliveryAck,
}

/// Options for [`assemble_gemini_family_lifecycle_outputs`].
pub(crate) struct GeminiFamilyLifecycleOpts {
    /// BeforeAgent only: return wake-only context when agy has no pending messages.
    pub allow_wake_no_pending: bool,
    /// Set instance status to active/prompt when there are no pending messages.
    /// Should be true only for BeforeAgent; false for AfterTool (which fires mid-turn
    /// after every tool call and must not overwrite the current in-progress status).
    pub set_status_on_empty: bool,
}

/// Combined lifecycle hook text + optional deferred ack / early wake-only return.
pub(crate) struct GeminiFamilyLifecycleOutput {
    pub parts: Vec<String>,
    pub delivery_ack: Option<super::DeliveryAck>,
    pub early_wake_context: Option<String>,
}

/// Shared beforeagent/aftertool output assembly for Gemini and Antigravity.
pub(crate) fn assemble_gemini_family_lifecycle_outputs(
    db: &HcomDb,
    ctx: &HcomContext,
    instance: &InstanceRow,
    is_agy: bool,
    opts: GeminiFamilyLifecycleOpts,
) -> GeminiFamilyLifecycleOutput {
    let instance_name = &instance.name;
    let mut parts: Vec<String> = Vec::new();
    let mut delivery_ack = None;

    if is_agy {
        // agy gets one short anti-stall preamble before each delivery (see
        // ANTIGRAVITY_DELIVERY_ACTION). On an empty wake it gets nothing and
        // simply ends its turn — no discovery prompt is needed.
        if let Some(prepared) = prepare_pending_messages(db, instance_name) {
            parts.push(bootstrap::ANTIGRAVITY_DELIVERY_ACTION.to_string());
            parts.push(prepared.formatted);
            delivery_ack = Some(prepared.ack);
        } else if opts.allow_wake_no_pending && instance.name_announced != 0 {
            return GeminiFamilyLifecycleOutput {
                parts: vec![],
                delivery_ack: None,
                early_wake_context: None,
            };
        }
        if let Some(bootstrap) =
            inject_bootstrap_once(db, ctx, instance_name, instance, &instance.tool)
        {
            parts.push(bootstrap);
        }
    } else {
        if let Some(bootstrap) =
            inject_bootstrap_once(db, ctx, instance_name, instance, &instance.tool)
        {
            parts.push(bootstrap);
        }
        if let Some(prepared) = prepare_pending_messages(db, instance_name) {
            parts.push(prepared.formatted);
            delivery_ack = Some(prepared.ack);
        } else if opts.set_status_on_empty {
            lifecycle::set_status(db, instance_name, ST_ACTIVE, "prompt", Default::default());
        }
    }

    GeminiFamilyLifecycleOutput {
        parts,
        delivery_ack,
        early_wake_context: None,
    }
}

pub(crate) fn limit_delivery_messages(messages: &[Value]) -> Vec<Value> {
    if messages.len() > MAX_MESSAGES_PER_DELIVERY {
        messages[..MAX_MESSAGES_PER_DELIVERY].to_vec()
    } else {
        messages.to_vec()
    }
}

pub(crate) fn format_messages_json_for_instance(
    db: &HcomDb,
    messages: &[Value],
    instance_name: &str,
) -> String {
    let get_instance_data = make_instance_lookup(db);
    let hints = load_config_hints();
    let get_config_hints = || hints.clone();
    let tip_checker = make_tip_checker(db);
    messages::format_messages_json(
        messages,
        instance_name,
        &get_instance_data,
        &get_config_hints,
        Some(&tip_checker),
    )
}

pub(crate) fn format_hook_messages_for_instance(
    db: &HcomDb,
    messages: &[Value],
    instance_name: &str,
) -> String {
    let get_instance_data = make_instance_lookup(db);
    let hints = load_config_hints();
    let get_config_hints = || hints.clone();
    messages::format_hook_messages(
        messages,
        instance_name,
        &get_instance_data,
        &get_config_hints,
        None,
    )
}

/// Prepare pending messages for delivery without committing cursor advance.
///
/// Returns formatted text + ack token. Caller must call `commit_delivery_ack`
/// after the output is successfully written (e.g. stdout flush).
pub fn prepare_pending_messages(db: &HcomDb, instance_name: &str) -> Option<PreparedDelivery> {
    let raw_messages = db.get_unread_messages(instance_name);
    prepare_raw_messages(db, instance_name, raw_messages)
}

/// Commit a deferred delivery ack — advance cursor and set status.
pub fn commit_delivery_ack(db: &HcomDb, ack: &super::DeliveryAck) {
    let mut updates = serde_json::Map::new();
    updates.insert("last_event_id".into(), serde_json::json!(ack.last_event_id));
    if ack.mark_announced {
        updates.insert("name_announced".into(), serde_json::json!(true));
    }
    instances::update_instance_position(db, &ack.instance_name, &updates);

    lifecycle::set_status(
        db,
        &ack.instance_name,
        ST_ACTIVE,
        &ack.status_context,
        lifecycle::StatusUpdate {
            msg_ts: &ack.msg_ts,
            ..Default::default()
        },
    );
}

/// Prepare raw messages into a PreparedDelivery without committing cursor/status.
///
/// Cursor advance and status update are deferred to `commit_delivery_ack`.
fn prepare_raw_messages(
    db: &HcomDb,
    instance_name: &str,
    raw_messages: Vec<Message>,
) -> Option<PreparedDelivery> {
    if raw_messages.is_empty() {
        return None;
    }

    let messages: Vec<Value> = raw_messages.iter().map(message_to_value).collect();
    let deliver = limit_delivery_messages(&messages);
    let formatted = format_messages_json_for_instance(db, &deliver, instance_name);

    let sender = deliver
        .first()
        .and_then(|m| m.get("from").and_then(|v| v.as_str()))
        .unwrap_or("unknown");
    let sender_display = identity::get_display_name(db, sender);
    let last_id = deliver
        .last()
        .and_then(|m| m.get("event_id").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    let msg_ts = deliver
        .last()
        .and_then(|m| m.get("timestamp").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    Some(PreparedDelivery {
        messages: deliver,
        formatted,
        ack: super::DeliveryAck {
            instance_name: instance_name.to_string(),
            last_event_id: last_id,
            status_context: format!("deliver:{}", sender_display),
            msg_ts,
            mark_announced: false,
        },
    })
}

/// Fetch unread messages, update cursor, set delivery status.
///
/// Returns (delivered_messages, formatted_json). Empty vec and None if no messages.
/// Callers that need additional formatting can use the returned messages vec.
///
pub fn deliver_pending_messages(db: &HcomDb, instance_name: &str) -> (Vec<Value>, Option<String>) {
    let raw_messages = db.get_unread_messages(instance_name);
    let Some(prepared) = prepare_raw_messages(db, instance_name, raw_messages) else {
        return (vec![], None);
    };
    commit_delivery_ack(db, &prepared.ack);
    (prepared.messages, Some(prepared.formatted))
}

/// Result of [`poll_messages`].
pub struct PollResult {
    /// True if a message was delivered (Stop/SubagentStop should be blocked
    /// so Claude sees `output` on its next turn instead of ending).
    pub delivered: bool,
    /// `{"decision":"block","reason":...}` when `delivered`, else `None`.
    pub output: Option<Value>,
    pub timed_out: bool,
    /// Deferred cursor/status commit. Caller must call `commit_delivery_ack`
    /// only after `output` has been successfully written to stdout — never
    /// before, since Claude only reads `output` on exit 0 and a premature
    /// commit would advance the cursor past a message Claude never saw.
    pub ack: Option<super::DeliveryAck>,
}

/// Stop hook polling loop — NOT used by main PTY path.
///
/// Runs for: headless instances, vanilla tool instances, subagent polling.
/// Main PTY path bypasses this (HCOM_PTY_MODE=1, PTY wrapper handles injection).
///
/// Uses select() on a TCP socket for efficient wake-on-message delivery.
/// Senders call `crate::notify::wake` (kind=`hook`) to wake the select().
///
/// Always exits 0: Claude ignores stdout JSON on exit 2 for Stop/SubagentStop
/// (stderr-only feedback), so a delivered message must go out as exit 0 +
/// `{"decision":"block"}` or Claude never sees it.
pub fn poll_messages(
    db: &HcomDb,
    instance_name: &str,
    timeout_secs: u64,
    is_background: bool,
) -> PollResult {
    match poll_messages_inner(db, instance_name, timeout_secs, is_background) {
        Ok(result) => result,
        Err(e) => {
            log::log_error(
                "hooks",
                "hook.error",
                &format!("hook=poll_messages err={}", e),
            );
            PollResult {
                delivered: false,
                output: None,
                timed_out: false,
                ack: None,
            }
        }
    }
}

fn poll_messages_inner(
    db: &HcomDb,
    instance_name: &str,
    timeout_secs: u64,
    is_background: bool,
) -> Result<PollResult> {
    // Check instance exists
    let instance_data = db
        .get_instance_full(instance_name)
        .context("DB error checking instance")?;
    if instance_data.is_none() {
        return Ok(PollResult {
            delivered: false,
            output: None,
            timed_out: false,
            ack: None,
        });
    }

    // Setup TCP notification socket
    let (notify_server, tcp_mode) = setup_tcp_notification(instance_name);
    let notify_port = notify_server
        .as_ref()
        .and_then(|s| s.local_addr().ok())
        .map(|a| a.port());

    // Register TCP mode
    let mut updates = serde_json::Map::new();
    updates.insert("tcp_mode".into(), serde_json::json!(tcp_mode));
    instances::update_instance_position(db, instance_name, &updates);

    // Register hook notify endpoint
    if let Some(port) = notify_port {
        register_hook_notify_port(db, instance_name, port);
    }

    // Set listening status
    lifecycle::set_status(db, instance_name, ST_LISTENING, "", Default::default());

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    let result = poll_loop(
        db,
        instance_name,
        timeout,
        start,
        is_background,
        notify_server.as_ref(),
    );

    // Cleanup: close socket, remove notify endpoint
    drop(notify_server);
    delete_hook_notify_endpoint(db, instance_name);

    result
}

fn poll_loop(
    db: &HcomDb,
    instance_name: &str,
    timeout: Duration,
    start: Instant,
    is_background: bool,
    notify_server: Option<&TcpListener>,
) -> Result<PollResult> {
    let empty = || PollResult {
        delivered: false,
        output: None,
        timed_out: false,
        ack: None,
    };
    let mut waited = false;
    while start.elapsed() < timeout {
        // Check if instance still exists (stopped = row deleted)
        let instance_data = db.get_instance_full(instance_name)?;
        if instance_data.is_none() {
            return Ok(empty());
        }

        // Poll for messages BEFORE select to catch transition gap
        let raw_messages = db.get_unread_messages(instance_name);
        if !raw_messages.is_empty() {
            // Orphan detection: don't deliver if parent died.
            // Only check after we've waited at least once — on the first iteration stdin
            // may legitimately be closed (e.g. subprocess invocation via `input=...`).
            if waited && !is_background && check_stdin_closed() {
                return Ok(empty());
            }

            if let Some(prepared) = prepare_raw_messages(db, instance_name, raw_messages) {
                // Do NOT commit the ack here — the caller must only advance
                // the cursor after `output` is actually flushed to stdout.
                // Claude discards stdout JSON on exit 2, so this must be
                // reported via exit 0 + decision:block for Claude to see it.
                let output = serde_json::json!({
                    "decision": "block",
                    "reason": prepared.formatted,
                });
                return Ok(PollResult {
                    delivered: true,
                    output: Some(output),
                    timed_out: false,
                    ack: Some(prepared.ack),
                });
            }
        }

        // Calculate remaining time
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            break;
        }
        let remaining = timeout - elapsed;

        // TCP select for notifications (or fallback poll). Relay imports
        // (pull.rs) call `crate::notify::wake_all` after every batch, so the
        // TCP wake fires as soon as remote events land — no separate relay
        // polling needed.
        let wait_time = if notify_server.is_some() {
            Duration::from_secs(remaining.as_secs().min(30))
        } else {
            Duration::from_millis(remaining.as_millis().min(100) as u64)
        };

        if let Some(server) = notify_server {
            // Block until a wake-up connection arrives instead of busy-looping
            if crate::sys::net::wait_readable(server, wait_time) {
                // Drain all pending connections
                if let Err(e) = server.set_nonblocking(true) {
                    log::log_warn(
                        "hooks",
                        "poll.nonblocking_failed",
                        &format!("set_nonblocking failed: {e}, skipping drain"),
                    );
                } else {
                    while let Ok((conn, _)) = server.accept() {
                        drop(conn);
                    }
                }
            }
        } else {
            std::thread::sleep(wait_time);
        }

        waited = true;

        // Update heartbeat (also re-asserts tcp_mode=1 for self-healing)
        let _ = db.update_heartbeat(instance_name);
    }

    // Timeout reached
    Ok(PollResult {
        delivered: false,
        output: None,
        timed_out: true,
        ack: None,
    })
}

/// Check if stdin is closed (orphan detection heuristic).
///
/// Piped stdin (normal for hook subprocess invocation) always gets POLLHUP
/// after the payload is consumed — this is NOT an orphan signal. Only check
/// POLLERR (broken pipe) and POLLNVAL (fd was closed/invalidated).
///
fn check_stdin_closed() -> bool {
    crate::sys::io::stdin_appears_broken()
}

/// Create TCP server socket for instant message wake notifications.
fn setup_tcp_notification(instance_name: &str) -> (Option<TcpListener>, bool) {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => {
            listener.set_nonblocking(true).unwrap_or(());
            (Some(listener), true)
        }
        Err(e) => {
            log::log_error(
                "hooks",
                "hook.error",
                &format!("hook=tcp_notification instance={} err={}", instance_name, e),
            );
            (None, false)
        }
    }
}

/// Register hook notify port in DB.
fn register_hook_notify_port(db: &HcomDb, instance_name: &str, port: u16) {
    if let Err(e) = db.upsert_notify_endpoint(instance_name, "hook", port) {
        log::log_warn(
            "native",
            "hooks.register_notify_fail",
            &format!(
                "Failed to register hook notify port for {}: {}",
                instance_name, e
            ),
        );
    }
}

/// Remove hook notify endpoint from DB.
fn delete_hook_notify_endpoint(db: &HcomDb, instance_name: &str) {
    let _ = db.conn().execute(
        "DELETE FROM notify_endpoints WHERE instance = ? AND kind = 'hook'",
        params![instance_name],
    );
}

/// Inject bootstrap text if not already announced.
///
/// Idempotent — checks name_announced flag and only injects once
/// per instance lifecycle. Returns bootstrap text if injection needed,
/// None if already announced.
///
pub fn inject_bootstrap_once(
    db: &HcomDb,
    ctx: &HcomContext,
    instance_name: &str,
    instance_data: &InstanceRow,
    tool: &str,
) -> Option<String> {
    if instance_data.name_announced != 0 {
        return None;
    }

    let tag = instance_data.tag.as_deref().unwrap_or("");
    let hcom_config = crate::config::HcomConfig::load(None).unwrap_or_default();
    let relay_enabled = crate::relay::is_relay_enabled(&hcom_config);

    let bootstrap_text = bootstrap::get_bootstrap(
        db,
        &ctx.hcom_dir,
        instance_name,
        tool,
        ctx.is_background,
        ctx.is_launched,
        &ctx.notes,
        tag,
        relay_enabled,
        ctx.background_name.as_deref(),
    );

    // Mark as announced
    let mut updates = serde_json::Map::new();
    updates.insert("name_announced".into(), serde_json::json!(true));
    instances::update_instance_position(db, instance_name, &updates);

    Some(bootstrap_text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptOwnerResolution {
    Owner(String),
    Ambiguous(Vec<String>),
    Unknown,
}

/// Resolve Claude ownership from bounded, structured transcript/session evidence.
///
/// Only envelope metadata is inspected. Ordinary message content, summaries,
/// tool output, bootstrap text, and `[hcom:name]` markers are intentionally out
/// of scope for lineage resolution.
pub(crate) fn resolve_claude_transcript_owner(
    db: &HcomDb,
    transcript_path: &str,
    incoming_session_id: Option<&str>,
) -> Result<TranscriptOwnerResolution> {
    const MAX_BYTES: usize = 512 * 1024;
    const MAX_RECORDS: usize = 2048;

    let mut owners = BTreeSet::new();
    let mut structured_session_ids = BTreeSet::new();

    let incoming_is_validated = match incoming_session_id.filter(|value| !value.is_empty()) {
        Some(session_id) => db.get_validated_claude_session_owner(session_id)?.is_some(),
        None => false,
    };
    if incoming_is_validated && let Some(session_id) = incoming_session_id {
        // A hook-provided incoming ID is only self-authenticating after a
        // trusted SessionStart or prior structured-lineage validation.
        structured_session_ids.insert(session_id.to_string());
    }

    if !transcript_path.is_empty() {
        owners.extend(db.get_instances_by_transcript_path(transcript_path)?);

        match std::fs::File::open(transcript_path) {
            Ok(file) => {
                // Head-biased by design: fork ancestry is copied into the first
                // records, and SessionStart must never stall on a huge transcript.
                let mut input = Vec::with_capacity(MAX_BYTES + 1);
                file.take((MAX_BYTES + 1) as u64).read_to_end(&mut input)?;
                let input_is_truncated = input.len() > MAX_BYTES;
                input.truncate(MAX_BYTES);
                for line in input
                    .split_inclusive(|byte| *byte == b'\n')
                    .take(MAX_RECORDS)
                {
                    // The bounded read may end in the middle of a UTF-8 code
                    // point or JSON record. Ignore only that incomplete tail
                    // rather than failing after valid earlier rows.
                    if input_is_truncated && !line.ends_with(b"\n") {
                        break;
                    }
                    let Ok(line) = std::str::from_utf8(line) else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    for session_id in [
                        value.get("sessionId").and_then(Value::as_str),
                        value.get("session_id").and_then(Value::as_str),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|value| !value.is_empty())
                    {
                        // Claude rewrites top-level IDs to the new fork UUID.
                        // Until that incoming binding is validated, the same ID
                        // cannot prove its own ownership. Different top-level
                        // IDs remain useful structured ancestry evidence.
                        if incoming_session_id != Some(session_id) || incoming_is_validated {
                            structured_session_ids.insert(session_id.to_string());
                        }
                    }
                    if let Some(session_id) = value
                        .get("message")
                        .and_then(|message| message.get("session_id"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        // Envelope message.session_id is independent structured
                        // provenance and may legitimately equal the current ID.
                        structured_session_ids.insert(session_id.to_string());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    for session_id in structured_session_ids {
        if let Some(owner) = db.get_session_binding(&session_id)? {
            owners.insert(owner);
        }
    }

    Ok(match owners.len() {
        0 => TranscriptOwnerResolution::Unknown,
        1 => TranscriptOwnerResolution::Owner(owners.into_iter().next().unwrap()),
        _ => TranscriptOwnerResolution::Ambiguous(owners.into_iter().collect()),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeIdentityEvidence {
    pub process_binding: Option<(Option<String>, String)>,
    pub process_session_id: Option<String>,
    pub process_owner: Option<String>,
    pub session_owner: Option<String>,
    pub validated_session_owner: Option<String>,
    pub owners_disagree: bool,
    pub lineage_scanned: bool,
    pub lineage: TranscriptOwnerResolution,
}

/// Load the identity facts shared by SessionStart and ordinary Claude hooks.
///
/// The caller supplies only the lineage-scan policy; owner selection remains
/// local to each resolution path.
pub(crate) fn load_claude_identity_evidence(
    db: &HcomDb,
    process_id: Option<&str>,
    session_id: &str,
    transcript_path: &str,
    should_scan_lineage: impl FnOnce(&ClaudeIdentityEvidence) -> bool,
) -> Result<ClaudeIdentityEvidence> {
    let process_binding = match process_id.filter(|value| !value.is_empty()) {
        Some(process_id) => db.get_process_binding_full(process_id)?,
        None => None,
    };
    let process_session_id = process_binding
        .as_ref()
        .and_then(|(session_id, _)| session_id.clone());
    let process_owner = process_binding
        .as_ref()
        .map(|(_, instance_name)| instance_name.clone());
    let session_owner = if session_id.is_empty() {
        None
    } else {
        db.get_session_binding(session_id)?
    };
    let validated_session_owner = if session_id.is_empty() {
        None
    } else {
        db.get_validated_claude_session_owner(session_id)?
    };
    let owners_disagree = matches!(
        (&process_owner, &session_owner),
        (Some(process_owner), Some(session_owner)) if process_owner != session_owner
    );

    let mut evidence = ClaudeIdentityEvidence {
        process_binding,
        process_session_id,
        process_owner,
        session_owner,
        validated_session_owner,
        owners_disagree,
        lineage_scanned: false,
        lineage: TranscriptOwnerResolution::Unknown,
    };
    evidence.lineage_scanned = should_scan_lineage(&evidence);
    if evidence.lineage_scanned {
        evidence.lineage = resolve_claude_transcript_owner(
            db,
            transcript_path,
            (!session_id.is_empty()).then_some(session_id),
        )?;
    }
    Ok(evidence)
}

/// Initialize instance context from hook data via binding lookup.
///
/// Structured session/transcript identity wins over a conflicting process
/// binding. Transcript scanning stays off the common hot path: it runs only
/// when the session is unbound or its binding has not yet been validated.
///
/// Returns (instance_name, metadata_updates, is_matched_resume).
pub fn init_hook_context(
    db: &HcomDb,
    ctx: &HcomContext,
    session_id: &str,
    transcript_path: &str,
) -> (Option<String>, serde_json::Map<String, Value>, bool) {
    let start = Instant::now();
    let evidence = match load_claude_identity_evidence(
        db,
        ctx.process_id.as_deref(),
        session_id,
        transcript_path,
        |evidence| {
            let binding_needs_validation = evidence.session_owner.is_some()
                && evidence.validated_session_owner.as_ref() != evidence.session_owner.as_ref();
            evidence.session_owner.is_none() || binding_needs_validation
        },
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            log::log_warn(
                "hooks",
                "init_hook_context.identity_evidence_error",
                &format!(
                    "session_id={} transcript_path={} process_id={:?} err={}",
                    session_id, transcript_path, ctx.process_id, error
                ),
            );
            return (None, serde_json::Map::new(), false);
        }
    };
    let evidence_ms = start.elapsed().as_secs_f64() * 1000.0;
    let historical_process_binding = evidence
        .process_session_id
        .as_deref()
        .filter(|bound_session_id| !bound_session_id.is_empty())
        .is_some_and(|bound_session_id| bound_session_id != session_id);

    let instance_name = if let Some(validated_owner) = evidence.validated_session_owner.clone() {
        Some(validated_owner)
    } else if evidence.lineage_scanned {
        match &evidence.lineage {
            TranscriptOwnerResolution::Owner(owner) => Some(owner.clone()),
            TranscriptOwnerResolution::Ambiguous(owners) => {
                log::log_warn(
                    "hooks",
                    "init_hook_context.identity_ambiguous",
                    &format!(
                        "session_id={} transcript_path={} process_id={:?} process_owner={:?} session_owner={:?} transcript_owners={:?}",
                        session_id,
                        transcript_path,
                        ctx.process_id,
                        evidence.process_owner,
                        evidence.session_owner,
                        owners,
                    ),
                );
                None
            }
            TranscriptOwnerResolution::Unknown => {
                if evidence.session_owner.is_some() {
                    log::log_warn(
                        "hooks",
                        "init_hook_context.unvalidated_session_rejected",
                        &format!(
                            "session_id={} transcript_path={} process_id={:?} process_owner={:?} session_owner={:?}",
                            session_id,
                            transcript_path,
                            ctx.process_id,
                            evidence.process_owner,
                            evidence.session_owner,
                        ),
                    );
                    None
                } else if historical_process_binding {
                    log::log_warn(
                        "hooks",
                        "init_hook_context.historical_process_rejected",
                        &format!(
                            "session_id={} transcript_path={} process_id={:?} process_session_id={:?} process_owner={:?}",
                            session_id,
                            transcript_path,
                            ctx.process_id,
                            evidence.process_session_id,
                            evidence.process_owner,
                        ),
                    );
                    None
                } else {
                    evidence.process_owner.clone()
                }
            }
        }
    } else {
        evidence
            .session_owner
            .clone()
            .or_else(|| evidence.process_owner.clone())
    };

    let Some(name) = instance_name else {
        log::log_info(
            "hooks",
            "init_hook_context.timing",
            &format!(
                "evidence_ms={:.2} total_ms={:.2} result=no_instance owners_disagree={}",
                evidence_ms,
                start.elapsed().as_secs_f64() * 1000.0,
                evidence.owners_disagree
            ),
        );
        return (None, serde_json::Map::new(), false);
    };

    let mut updates = serde_json::Map::new();
    updates.insert(
        "directory".into(),
        Value::String(ctx.cwd.to_string_lossy().to_string()),
    );
    if !transcript_path.is_empty() {
        updates.insert(
            "transcript_path".into(),
            Value::String(transcript_path.to_string()),
        );
    }
    if ctx.is_background
        && let Some(ref bg_name) = ctx.background_name
    {
        updates.insert("background".into(), serde_json::json!(true));
        let log_file = ctx.hcom_dir.join(".tmp").join("logs").join(bg_name);
        updates.insert(
            "background_log_file".into(),
            Value::String(log_file.to_string_lossy().to_string()),
        );
    }

    let instance = db.get_instance_full(&name).ok().flatten();
    let is_matched_resume = !session_id.is_empty()
        && instance
            .as_ref()
            .is_some_and(|data| data.session_id.as_deref() == Some(session_id));

    if is_matched_resume
        && matches!(&evidence.lineage, TranscriptOwnerResolution::Owner(owner) if owner == &name)
        && evidence.session_owner.as_deref() == Some(name.as_str())
        && let Err(error) = db.mark_claude_session_validated(session_id, &name)
    {
        log::log_warn(
            "hooks",
            "init_hook_context.validation_cache_write_failed",
            &format!("session_id={} owner={} err={}", session_id, name, error),
        );
    }

    if evidence.lineage_scanned
        && matches!(&evidence.lineage, TranscriptOwnerResolution::Owner(owner) if owner == &name)
        && !is_matched_resume
    {
        log::log_warn(
            "hooks",
            "init_hook_context.unpromoted_lineage_rejected",
            &format!(
                "session_id={} owner={} primary_session={:?} total_ms={:.2}",
                session_id,
                name,
                instance.as_ref().and_then(|row| row.session_id.as_deref()),
                start.elapsed().as_secs_f64() * 1000.0,
            ),
        );
        return (None, serde_json::Map::new(), false);
    }

    log::log_info(
        "hooks",
        "init_hook_context.timing",
        &format!(
            "instance={} evidence_ms={:.2} total_ms={:.2} validated={} owners_disagree={}",
            name,
            evidence_ms,
            start.elapsed().as_secs_f64() * 1000.0,
            evidence.validated_session_owner.is_some(),
            evidence.owners_disagree,
        ),
    );

    (Some(name), updates, is_matched_resume)
}

/// Wake an instance's hook poll loop via TCP connection.
///
/// Best-effort: opens DB, finds hook wake endpoint, sends brief TCP connect.
/// Wraps `crate::notify::wake` with kind=`hook` for the hook poll path —
/// PTY/listen wakes go through `crate::notify::wake` directly.
///
pub fn notify_hook_instance(instance_name: &str) {
    if let Ok(db) = HcomDb::open() {
        notify_hook_instance_with_db(&db, instance_name);
    }
}

/// Wake hook poll loop with an existing DB handle.
pub fn notify_hook_instance_with_db(db: &HcomDb, instance_name: &str) {
    crate::notify::wake(db, instance_name, &[crate::notify::WakeKind::Hook]);
}

/// Chỗ đặt trước lý do dừng, để một tiến trình `hcom` khác chạy
/// `mark_dead_instances` (main.rs:72, chạy ở MỌI lệnh) không ghi đè bằng
/// "exit:dead_process" khi nó thấy PID chết trước lúc ta kịp ghi life event.
///
/// Value là `<reason>|<initiated_by>|<created_at>`: tên agent là từ CVCV và
/// được tái sử dụng, nên một key sót lại từ lần crash trước không được phép
/// gán lý do cho một instance khác trùng tên. `session_id` không đủ để phân
/// biệt: `hcom r <name>` resume không-fork giữ nguyên `session_id` cũ
/// (`prior_session_id`, resume.rs), nên một claim mồ côi từ lần chạy trước
/// (kill thua PTY/SessionEnd bỏ dở, xoá row mà không tiêu thụ claim) vẫn khớp
/// session_id của instance mới sau resume. `created_at` là duy nhất theo
/// vòng đời instance nên dùng nó làm khoá đối chiếu thay vì session_id.
fn stop_reason_key(name: &str) -> String {
    format!("stop_reason:{name}")
}

pub(crate) fn claim_stop_reason(
    db: &HcomDb,
    name: &str,
    created_at: f64,
    initiated_by: &str,
    reason: &str,
) {
    let value = format!("{reason}|{initiated_by}|{created_at}");
    // A delayed stopper must not overwrite a newer lifetime's claim.
    let _ = db.conn().execute(
        "INSERT OR REPLACE INTO kv (key, value)
         SELECT ?, ? WHERE EXISTS (
             SELECT 1 FROM instances WHERE name = ? AND created_at = ?
         )",
        params![stop_reason_key(name), value, name, created_at],
    );
}

/// Read a matching claim without consuming it. The winning stop transaction
/// deletes it together with the instance; failed or competing writers can retry.
pub(crate) fn read_stop_reason(
    db: &HcomDb,
    name: &str,
    created_at: f64,
) -> Option<(String, String)> {
    let key = stop_reason_key(name);
    let raw = db.kv_get(&key).ok().flatten()?;
    // splitn(3): created_at là phần cuối và không được phép bị cắt tiếp.
    let mut parts = raw.splitn(3, '|');
    let reason = parts.next()?;
    let initiated_by = parts.next()?;
    let claimed_created_at: f64 = parts.next()?.parse().ok()?;
    if claimed_created_at == created_at {
        Some((reason.to_string(), initiated_by.to_string()))
    } else {
        None
    }
}

/// Stop instance: log snapshot, clean bindings, delete row.
///
/// Handles: snapshot capture, session/process/notify/subscription cleanup,
/// life event logging, and instance deletion.
pub fn stop_instance(
    db: &HcomDb,
    instance_name: &str,
    initiated_by: &str,
    reason: &str,
) -> StopOutcome {
    stop_instance_inner(db, instance_name, initiated_by, reason, false, 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    Stopped,
    AlreadyStopped,
    RetryableError(String),
}

pub(crate) fn stop_placeholder_instance(
    db: &HcomDb,
    instance_name: &str,
    initiated_by: &str,
    reason: &str,
) -> StopOutcome {
    stop_instance_inner(db, instance_name, initiated_by, reason, true, 0)
}

/// Max recursion depth for subagent cleanup. Prevents stack overflow if DB
/// corruption creates a parent_session_id cycle.
const MAX_STOP_DEPTH: u32 = 10;

fn child_instance_names(db: &HcomDb, column: &str, value: &str) -> Result<Vec<String>> {
    let sql = match column {
        "parent_session_id" => "SELECT name FROM instances WHERE parent_session_id = ?",
        "parent_name" => "SELECT name FROM instances WHERE parent_name = ?",
        _ => anyhow::bail!("unsupported child relationship: {column}"),
    };
    let mut stmt = db.conn().prepare(sql)?;
    let rows = stmt.query_map(params![value], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn stop_instance_inner(
    db: &HcomDb,
    instance_name: &str,
    initiated_by: &str,
    reason: &str,
    placeholder: bool,
    depth: u32,
) -> StopOutcome {
    if depth >= MAX_STOP_DEPTH {
        log::log_warn(
            "core",
            "stop_instance.max_depth",
            &format!(
                "Recursion limit ({}) reached stopping {}; possible cycle",
                MAX_STOP_DEPTH, instance_name
            ),
        );
        return StopOutcome::RetryableError(format!(
            "recursion limit reached while stopping {instance_name}"
        ));
    }

    let instance_data = match db.get_instance_full(instance_name) {
        Ok(Some(data)) => data,
        Ok(None) => return StopOutcome::AlreadyStopped,
        Err(e) => {
            return StopOutcome::RetryableError(format!(
                "could not read instance {instance_name}: {e}"
            ));
        }
    };

    // Đặt chỗ lý do trước khi có bất kỳ tín hiệu nào được gửi. Từ đây trở đi
    // PID có thể chết bất cứ lúc nào, và mark_dead_instances của một tiến trình
    // hcom khác có thể thắng cuộc ghi.
    claim_stop_reason(
        db,
        instance_name,
        instance_data.created_at,
        initiated_by,
        reason,
    );

    // Kill headless processes (background=true)
    let pid = instance_data.pid;
    let is_headless = instance_data.background != 0;
    if let Some(pid_val) = pid {
        let pid_u32 = pid_val as u32;
        if is_headless {
            // Graceful-then-forceful group kill: terminate_group (Unix: SIGTERM;
            // Windows: forceful process-tree kill) → poll up to 2s for exit →
            // kill_group (Unix: SIGKILL; Windows: tree kill again). The poll also
            // waits out Windows' asynchronous TerminateProcess.
            use crate::sys::process::GroupSignal;
            if crate::sys::process::terminate_group(pid_u32) == GroupSignal::Sent {
                let mut dead = false;
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(100));
                    if !crate::sys::process::is_alive(pid_u32) {
                        dead = true;
                        break;
                    }
                }
                if !dead {
                    crate::sys::process::kill_group(pid_u32);
                }
            }
            // NotFound/PermissionDenied from initial signal is fine — process already gone or foreign
        } else {
            // Track surviving PTY processes in pidtrack
            let alive = crate::sys::process::is_alive(pid_u32);
            if alive {
                let hcom_dir = crate::paths::hcom_dir();

                let ti = crate::terminal::resolve_terminal_info(
                    instance_data.terminal_preset_effective.as_deref(),
                    instance_data.launch_context.as_deref(),
                );
                let terminal_preset = ti.preset_name;
                let pane_id = ti.pane_id;
                let mut proc_id = ti.process_id;
                let terminal_id = ti.terminal_id;
                let kitty_listen_on = ti.kitty_listen_on;
                let zellij_session_name = ti.zellij_session_name;
                // Fallback: process_bindings table
                if proc_id.is_empty()
                    && let Ok(mut stmt) = db
                        .conn()
                        .prepare("SELECT process_id FROM process_bindings WHERE instance_name = ?")
                    && let Ok(val) =
                        stmt.query_row(params![instance_name], |row| row.get::<_, String>(0))
                {
                    proc_id = val;
                }
                // Grab notify/inject ports before DB cleanup deletes them
                let mut notify_port: u16 = 0;
                let mut inject_port: u16 = 0;
                if let Ok(mut stmt) = db
                    .conn()
                    .prepare("SELECT kind, port FROM notify_endpoints WHERE instance = ?")
                    && let Ok(rows) = stmt.query_map(params![instance_name], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                {
                    for row in rows.flatten() {
                        match row.0.as_str() {
                            "pty" => notify_port = row.1 as u16,
                            "inject" => inject_port = row.1 as u16,
                            _ => {}
                        }
                    }
                }

                crate::pidtrack::record_pid(&crate::pidtrack::PidRecord {
                    hcom_dir: &hcom_dir,
                    pid: pid_val as u32,
                    tool: &instance_data.tool,
                    name: instance_name,
                    directory: &instance_data.directory,
                    process_id: &proc_id,
                    terminal_preset: &terminal_preset,
                    pane_id: &pane_id,
                    terminal_id: &terminal_id,
                    kitty_listen_on: &kitty_listen_on,
                    zellij_session_name: &zellij_session_name,
                    session_id: instance_data.session_id.as_deref().unwrap_or(""),
                    notify_port,
                    inject_port,
                    tag: instance_data.tag.as_deref().unwrap_or(""),
                    // This pid comes from the instance row, not from a process
                    // this hook spawned: a sandboxed hcom running the stop path
                    // for a host agent must not relabel the host's marker as
                    // its own. An empty marker stays unknown.
                    pid_namespace: Some(instance_data.pid_namespace.as_deref().unwrap_or("")),
                });
                log::log_info(
                    "stop",
                    "pidtrack_recorded",
                    &format!(
                        "pid={} instance={} preset={} pane_id={}",
                        pid_val, instance_name, terminal_preset, pane_id
                    ),
                );
            }
        }
    }

    // Capture wake ports BEFORE cleanup deletes them; we'll fire wakes after
    // delete so any remaining listeners see the row is gone.
    let wake_ports = crate::notify::snapshot_wake_ports(db, instance_name);

    // Prepare snapshot before delete (preserves data for transcript access)
    // Use Option values directly so None serializes as JSON null
    let snapshot = serde_json::json!({
        "name": instance_name,
        "transcript_path": instance_data.transcript_path,
        "session_id": instance_data.session_id,
        "tool": instance_data.tool,
        "directory": instance_data.directory,
        "parent_name": instance_data.parent_name,
        "parent_session_id": instance_data.parent_session_id,
        "tag": instance_data.tag,
        "wait_timeout": instance_data.wait_timeout,
        "subagent_timeout": instance_data.subagent_timeout,
        "hints": instance_data.hints,
        "pid": instance_data.pid,
        "created_at": instance_data.created_at,
        "last_seen": instance_data.last_seen,
        "background": instance_data.background,
        "agent_id": instance_data.agent_id,
        "name_announced": instance_data.name_announced,
        "launch_args": instance_data.launch_args,
        "origin_device_id": instance_data.origin_device_id,
        "background_log_file": instance_data.background_log_file,
        "last_event_id": instance_data.last_event_id,
    });

    // Snapshot both child sets before deleting the parent. Only the teardown
    // winner processes them, but it still needs relationships that may be
    // cascaded or otherwise obscured by the parent deletion.
    let session_subagents = match instance_data.session_id.as_deref() {
        Some(session_id) => match child_instance_names(db, "parent_session_id", session_id) {
            Ok(children) => children,
            Err(e) => {
                return StopOutcome::RetryableError(format!(
                    "could not enumerate session children of {instance_name}: {e}"
                ));
            }
        },
        None => Vec::new(),
    };
    let native_children = match child_instance_names(db, "parent_name", instance_name) {
        Ok(children) => children,
        Err(e) => {
            return StopOutcome::RetryableError(format!(
                "could not enumerate native children of {instance_name}: {e}"
            ));
        }
    };

    // Finish children first while the parent row keeps the teardown retryable.
    // Concurrent callers may repeat this work; every child has its own atomic
    // event/delete gate.
    for sub_name in session_subagents {
        if let StopOutcome::RetryableError(error) = stop_instance_inner(
            db,
            &sub_name,
            initiated_by,
            "parent_stopped",
            false,
            depth + 1,
        ) {
            log::log_warn(
                "hooks",
                "finalize.child_stop_incomplete",
                &format!("parent={instance_name} child={sub_name} err={error}"),
            );
            return StopOutcome::RetryableError(format!(
                "could not stop child {sub_name}: {error}"
            ));
        }
    }

    // Native subagent rows carry session_id=NULL and inherit the root session
    // as parent_session_id, so only parent_name links nested children. A row
    // already stopped via the session set is a no-op here.
    for child in native_children {
        if let StopOutcome::RetryableError(error) =
            stop_instance_inner(db, &child, initiated_by, "parent_stopped", false, depth + 1)
        {
            log::log_warn(
                "hooks",
                "finalize.child_stop_incomplete",
                &format!("parent={instance_name} child={child} err={error}"),
            );
            return StopOutcome::RetryableError(format!("could not stop child {child}: {error}"));
        }
    }

    // Publish the winner's pre-delete snapshot in the same transaction that
    // deletes the row and its control-plane state. Event failure rolls the
    // deletion back, so another invocation can retry the whole teardown.
    let mut event_data = serde_json::json!({
        "action": "stopped",
        "by": initiated_by,
        "reason": reason,
        "snapshot": snapshot,
    });
    if placeholder {
        event_data["placeholder"] = serde_json::json!(true);
    }
    // ponytail: direct row deletion (e.g. PTY exit) can leave one orphan claim
    // per name. A new lifetime's claim overwrites it, and the created_at check
    // prevents a stale reason from being used. Keep cleanup transactional here;
    // add a lifetime-checked orphan sweep if retained kv rows become significant.
    match db.finalize_instance_stop(
        instance_name,
        instance_data.created_at,
        instance_data.session_id.as_deref(),
        instance_data.agent_id.as_deref(),
        &event_data,
    ) {
        Ok(true) => {}
        Ok(false) => return StopOutcome::AlreadyStopped,
        Err(e) => {
            log::log_warn(
                "hooks",
                "finalize.transaction_failed",
                &format!("instance={instance_name} err={e}"),
            );
            return StopOutcome::RetryableError(format!(
                "could not finalize stop for {instance_name}: {e}"
            ));
        }
    }

    // Capabilities are scoped to the deleted actor. Root teardown also
    // revokes every child token in the shared Claude session and drops
    // outstanding stop-claim correlation records.
    let _ = db.revoke_claude_actor_capabilities_for_instance(instance_name);
    if let Some(ref session_id) = instance_data.session_id {
        let _ = db.revoke_claude_actor_capabilities_for_session(session_id);
        let _ = db.kv_delete_prefix(&format!("subagent_stop_inflight:{session_id}:"));
    }

    // Notify remaining listeners AFTER delete (so they see the row is gone)
    crate::notify::wake_ports(&wake_ports, crate::notify::WAKE_TARGETED_MS);

    // Trigger relay push (best-effort)
    crate::relay::spawn_background_push();
    StopOutcome::Stopped
}

/// Soft session end for Antigravity: mark inactive without deleting the `instances` row.
///
/// agy has no process-death hook — its hook set is only PreToolUse/PostToolUse/
/// PreInvocation/PostInvocation/Stop. We synthesize "SessionEnd" from `Stop`, which
/// fires when an *execution loop* terminates, NOT when the process dies: the agy
/// editor stays alive and routinely runs more turns after a `Stop` (observed in the
/// wild — instances soft-stopped here go straight back to listening/active). So the
/// hook path must never hard-delete: doing so would strand a still-running agent.
/// agy's real teardown is the PTY exit (`cleanup_antigravity_pty_exit`), which sees
/// the inactive status and preserves the row for `hcom r`.
///
/// Clears session bindings (and process bindings unless `keep_process_binding`),
/// and logs a stopped life event with snapshot, but does not delete the instance row.
///
/// OMP soft-stop passes `keep_process_binding: true` so the live process can rebind
/// via `bind_session_to_process` on the next turn. Antigravity passes `false`.
pub fn soft_finalize_session(
    db: &HcomDb,
    instance_name: &str,
    reason: &str,
    updates: Option<&serde_json::Map<String, Value>>,
    keep_process_binding: bool,
) {
    log::log_info(
        "hooks",
        "sessionend.soft",
        &format!("instance={} reason={}", instance_name, reason),
    );

    lifecycle::set_status(
        db,
        instance_name,
        ST_INACTIVE,
        &format!("exit:{}", reason),
        Default::default(),
    );

    if let Some(updates) = updates {
        instances::update_instance_position(db, instance_name, updates);
    }

    let instance_data = match db.get_instance_full(instance_name) {
        Ok(Some(data)) => data,
        _ => return,
    };

    let snapshot = serde_json::json!({
        "name": instance_name,
        "transcript_path": instance_data.transcript_path,
        "session_id": instance_data.session_id,
        "tool": instance_data.tool,
        "directory": instance_data.directory,
        "parent_name": instance_data.parent_name,
        "parent_session_id": instance_data.parent_session_id,
        "tag": instance_data.tag,
        "wait_timeout": instance_data.wait_timeout,
        "subagent_timeout": instance_data.subagent_timeout,
        "hints": instance_data.hints,
        "pid": instance_data.pid,
        "created_at": instance_data.created_at,
        "last_seen": instance_data.last_seen,
        "background": instance_data.background,
        "agent_id": instance_data.agent_id,
        "name_announced": instance_data.name_announced,
        "launch_args": instance_data.launch_args,
        "origin_device_id": instance_data.origin_device_id,
        "background_log_file": instance_data.background_log_file,
        "last_event_id": instance_data.last_event_id,
    });

    if let Some(ref session_id) = instance_data.session_id {
        let _ = db.conn().execute(
            "DELETE FROM session_bindings WHERE session_id = ?",
            params![session_id],
        );
        if !keep_process_binding {
            let _ = db.conn().execute(
                "DELETE FROM process_bindings WHERE session_id = ?",
                params![session_id],
            );
        }
    }

    let _ = db.delete_notify_endpoints(instance_name);
    if !keep_process_binding {
        let _ = db.conn().execute(
            "DELETE FROM process_bindings WHERE instance_name = ?",
            params![instance_name],
        );
    }
    let _ = db.cleanup_subscriptions(instance_name);

    if let Err(e) = db.log_life_event(
        instance_name,
        "stopped",
        "session",
        &format!("exit:{}", reason),
        Some(snapshot),
    ) {
        log::log_warn(
            "hooks",
            "sessionend.soft.life_event_failed",
            &format!("log_life_event failed for {instance_name}: {e}"),
        );
    }
}

/// Set inactive status, persist updates, and stop instance.
///
/// Common to Claude and Gemini SessionEnd handlers. Catches all errors
/// internally — callers don't need error handling.
///
pub fn finalize_session(
    db: &HcomDb,
    instance_name: &str,
    reason: &str,
    updates: Option<&serde_json::Map<String, Value>>,
) {
    log::log_info(
        "hooks",
        "sessionend",
        &format!("instance={} reason={}", instance_name, reason),
    );

    // Set inactive status
    lifecycle::set_status(
        db,
        instance_name,
        ST_INACTIVE,
        &format!("exit:{}", reason),
        Default::default(),
    );

    // Persist metadata updates
    if let Some(updates) = updates {
        instances::update_instance_position(db, instance_name, updates);
    }

    // Full stop_instance chain: snapshot, cleanup bindings, log, delete
    stop_instance(db, instance_name, "session", &format!("exit:{}", reason));
}

/// Update instance status for tool execution.
///
/// Calls extract_tool_detail for tool-specific detail formatting,
/// then sets status to active with tool context.
///
pub fn update_tool_status(
    db: &HcomDb,
    instance_name: &str,
    tool: &str,
    tool_name: &str,
    tool_input: &Value,
) {
    let detail = super::family::extract_tool_detail(tool, tool_name, tool_input);
    lifecycle::set_status(
        db,
        instance_name,
        ST_ACTIVE,
        &format!("tool:{}", tool_name),
        lifecycle::StatusUpdate {
            detail: &detail,
            ..Default::default()
        },
    );
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
