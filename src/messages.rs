//! Message operations — routing, scope computation, and delivery formatting.

use crate::shared::{MAX_MESSAGE_SIZE, SENDER, extract_mentions};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Precompiled regex for @[hcom-*] system notification mentions.
static SYSTEM_BRACKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@\[hcom-[a-z]+\]").unwrap());

/// Message scope: broadcast (everyone) or mentions (targeted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageScope {
    Broadcast,
    Mentions,
}

impl MessageScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageScope::Broadcast => "broadcast",
            MessageScope::Mentions => "mentions",
        }
    }
}

impl std::str::FromStr for MessageScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "broadcast" => Ok(MessageScope::Broadcast),
            "mentions" => Ok(MessageScope::Mentions),
            _ => Err(format!("invalid message scope: {s}")),
        }
    }
}

/// Message intent for envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageIntent {
    Request,
    Inform,
    Ack,
}

impl MessageIntent {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageIntent::Request => "request",
            MessageIntent::Inform => "inform",
            MessageIntent::Ack => "ack",
        }
    }
}

impl std::str::FromStr for MessageIntent {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "request" => Ok(MessageIntent::Request),
            "inform" => Ok(MessageIntent::Inform),
            "ack" => Ok(MessageIntent::Ack),
            _ => Err(format!("invalid message intent: {s}")),
        }
    }
}

/// Optional envelope fields for messages.
#[derive(Debug, Clone, Default)]
pub struct MessageEnvelope {
    pub intent: Option<MessageIntent>,
    pub reply_to: Option<String>,
    pub thread: Option<String>,
    pub bundle_id: Option<String>,
}

/// Relay metadata for cross-device messages.
#[derive(Debug, Clone)]
pub struct RelayMetadata {
    pub id: String,
    pub short: String,
}

/// Scope computation result.
#[derive(Debug, Clone)]
pub struct ScopeResult {
    pub scope: MessageScope,
    /// For Mentions scope: list of base names targeted.
    pub mentions: Vec<String>,
}

/// Read receipt for a sent message.
#[derive(Debug, Clone)]
pub struct ReadReceipt {
    pub id: i64,
    pub age: String,
    pub text: String,
    pub read_by: Vec<String>,
    pub total_recipients: usize,
}

/// Instance info for scope computation (name + optional tag).
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    pub name: String,
    pub tag: Option<String>,
}

impl InstanceInfo {
    /// Full display name: "{tag}-{name}" if tag, else just "{name}".
    pub fn full_name(&self) -> String {
        match &self.tag {
            Some(tag) if !tag.is_empty() => format!("{}-{}", tag, self.name),
            _ => self.name.clone(),
        }
    }
}

// validate_scope and validate_intent live in core::helpers — re-export for consumers.
pub use crate::core::helpers::{validate_intent, validate_scope};

/// Validate message content and size.
pub fn validate_message(message: &str) -> Result<(), String> {
    if message.is_empty() || message.trim().is_empty() {
        return Err("Message required".to_string());
    }

    // Reject control characters (except \n, \r, \t)
    for ch in message.chars() {
        if ('\x00'..='\x08').contains(&ch)
            || ('\x0B'..='\x0C').contains(&ch)
            || ('\x0E'..='\x1F').contains(&ch)
            || ('\u{0080}'..='\u{009F}').contains(&ch)
        {
            return Err("Message contains control characters".to_string());
        }
    }

    if message.len() > MAX_MESSAGE_SIZE {
        return Err(format!(
            "Message too large (max {} chars)",
            MAX_MESSAGE_SIZE
        ));
    }

    Ok(())
}

/// Format recipients list for display.
///
/// "luna, nova" or "luna, nova, kira (+2 more)" or "(none)"
pub fn format_recipients(delivered_to: &[String], max_show: usize) -> String {
    if delivered_to.is_empty() {
        return "(none)".to_string();
    }

    if delivered_to.len() > max_show {
        let shown: Vec<&str> = delivered_to[..max_show]
            .iter()
            .map(|s| s.as_str())
            .collect();
        let remaining = delivered_to.len() - max_show;
        format!("{} (+{} more)", shown.join(", "), remaining)
    } else {
        delivered_to.join(", ")
    }
}

/// Build the "unknown @mention" error string with a "Did you mean" hint when
/// an unmatched target (without `:`) has the same base name as a remote agent.
///
/// Without this hint, users hit `@zeli` → "non-existent" even though `zeli:ZOME`
/// is right there in the available list and only takes a colon-suffix to reach.
fn build_unmatched_error(unmatched: &[String], full_names: &[String]) -> String {
    let unmatched_display: Vec<String> = unmatched.iter().map(|t| format!("@{}", t)).collect();

    let mut suggestions: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for target in unmatched {
        if target.contains(':') {
            continue;
        }
        let target_lower = target.to_lowercase();
        for fn_ in full_names {
            if let Some((prefix, _device)) = fn_.split_once(':')
                && prefix.to_lowercase() == target_lower
                && seen.insert(fn_.clone())
            {
                suggestions.push(format!("@{}", fn_));
            }
        }
    }

    let mut msg = format!(
        "@mentions to non-existent or stopped agents (or you used '@' char for stuff that wasn't agent name): {}",
        unmatched_display.join(", "),
    );
    if !suggestions.is_empty() {
        msg.push_str(&format!("\nDid you mean: {}?", suggestions.join(", ")));
    }
    msg.push_str(&format!(
        "\nAvailable: {}",
        format_recipients(full_names, 30)
    ));
    msg
}

/// Match a target against instance names.
///
/// Resolution order:
/// 1. Exact base name
/// 2. Exact full display name ({tag}-{name})
/// 3. Exact tag group when the target ends in `-`
/// 4. Unique remote prefix when the target contains `:`
///
/// Special case: bigboss:SUFFIX resolves to bigboss (virtual identity, device-agnostic).
fn match_target(target: &str, instances: &[InstanceInfo]) -> Result<Vec<String>, String> {
    let exact_base: Vec<String> = instances
        .iter()
        .filter(|inst| inst.name.eq_ignore_ascii_case(target))
        .map(|inst| inst.name.clone())
        .collect();
    if !exact_base.is_empty() {
        return Ok(dedup_preserving_order(&exact_base));
    }

    // bigboss is device-agnostic — strip any remote suffix
    if target
        .split_once(':')
        .is_some_and(|(base, _)| base.eq_ignore_ascii_case(SENDER))
    {
        return Ok(vec![SENDER.to_string()]);
    }

    let exact_full: Vec<String> = instances
        .iter()
        .filter(|inst| inst.full_name().eq_ignore_ascii_case(target))
        .map(|inst| inst.name.clone())
        .collect();
    if !exact_full.is_empty() {
        return Ok(dedup_preserving_order(&exact_full));
    }

    if let Some(tag_target) = target.strip_suffix('-') {
        let matches: Vec<String> = instances
            .iter()
            .filter(|inst| !inst.name.contains(':'))
            .filter(|inst| {
                inst.tag
                    .as_deref()
                    .is_some_and(|tag| tag.eq_ignore_ascii_case(tag_target))
            })
            .map(|inst| inst.name.clone())
            .collect();
        return Ok(dedup_preserving_order(&matches));
    }

    if target.contains(':') {
        let target_lower = target.to_ascii_lowercase();
        let mut candidates: Vec<(String, String)> = instances
            .iter()
            .filter_map(|inst| {
                let full = inst.full_name();
                (inst.name.to_ascii_lowercase().starts_with(&target_lower)
                    || full.to_ascii_lowercase().starts_with(&target_lower))
                .then(|| (inst.name.clone(), full))
            })
            .collect();
        candidates.sort();
        candidates.dedup_by(|a, b| a.0 == b.0);

        if candidates.len() == 1 {
            return Ok(vec![candidates[0].0.clone()]);
        }
        if candidates.len() > 1 {
            return Err(format!(
                "Ambiguous remote @mention @{target}; matches: {}",
                candidates
                    .iter()
                    .map(|(_, full)| format!("@{full}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    Ok(Vec::new())
}

fn target_instances_with_sender(enabled_instances: &[InstanceInfo]) -> Vec<InstanceInfo> {
    let mut instances = enabled_instances.to_vec();
    if !instances
        .iter()
        .any(|inst| inst.name.eq_ignore_ascii_case(SENDER))
    {
        instances.push(InstanceInfo {
            name: SENDER.to_string(),
            tag: None,
        });
    }
    instances
}

pub(crate) fn resolve_targets(
    targets: &[String],
    enabled_instances: &[InstanceInfo],
) -> Result<(Vec<String>, Vec<String>), String> {
    let target_instances = target_instances_with_sender(enabled_instances);
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();

    for target in targets {
        let target_matches = match_target(target, &target_instances)?;
        if target_matches.is_empty() {
            unmatched.push(target.clone());
        } else {
            matched.extend(target_matches);
        }
    }

    Ok((dedup_preserving_order(&matched), unmatched))
}

/// Compute message scope and routing data.
///
/// Returns Ok((scope_result, None)) on success, Ok((None, error)) on validation failure.
///
/// Scope types:
/// - Broadcast: No targets → everyone
/// - Mentions: Has targets → explicit targets only
///
/// STRICT FAILURE: Targets that don't match enabled instances return error.
pub fn compute_scope(
    message: &str,
    enabled_instances: &[InstanceInfo],
    explicit_targets: Option<&[String]>,
) -> Result<ScopeResult, String> {
    let target_instances = target_instances_with_sender(enabled_instances);
    let full_names: Vec<String> = target_instances
        .iter()
        .map(InstanceInfo::full_name)
        .collect();

    // If explicit targets specified (via -- separator), use them instead of parsing @mentions
    if let Some(targets) = explicit_targets {
        if !targets.is_empty() {
            let (matched_base_names, unmatched) = resolve_targets(targets, enabled_instances)?;

            if !unmatched.is_empty() {
                return Err(build_unmatched_error(&unmatched, &full_names));
            }

            if !matched_base_names.is_empty() {
                return Ok(ScopeResult {
                    scope: MessageScope::Mentions,
                    mentions: matched_base_names,
                });
            }
        }

        // Empty explicit_targets or no matches = broadcast
        return Ok(ScopeResult {
            scope: MessageScope::Broadcast,
            mentions: vec![],
        });
    }

    // No explicit targets (None) — check for @mentions in message text
    if message.contains('@') {
        // Check for invalid system notification mention attempts like @[hcom-events]
        let system_attempts: Vec<&str> = SYSTEM_BRACKET_RE
            .find_iter(message)
            .map(|m| m.as_str())
            .collect();
        if !system_attempts.is_empty() {
            return Err(format!(
                "System notifications cannot be mentioned: {}\nSystem notifications (names in []) are not agents and cannot receive messages.",
                system_attempts.join(", "),
            ));
        }

        let mentions = extract_mentions(message);
        if !mentions.is_empty() {
            let (matched_base_names, unmatched) = resolve_targets(&mentions, enabled_instances)?;

            // STRICT: fail on unmatched mentions
            if !unmatched.is_empty() {
                // Special cases: literal "@mention", "@name", or "@mentions"
                let special_literals: HashSet<&str> =
                    ["mention", "name", "mentions"].iter().copied().collect();
                let literal_matches: Vec<&String> = unmatched
                    .iter()
                    .filter(|m| special_literals.contains(m.as_str()))
                    .collect();

                if !literal_matches.is_empty() {
                    let literal_text = if literal_matches.len() == 1 {
                        format!("@{}", literal_matches[0])
                    } else {
                        literal_matches
                            .iter()
                            .map(|m| format!("@{}", m))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    return Err(format!(
                        "The literal text {} is not a valid target - use actual instance names",
                        literal_text,
                    ));
                }

                return Err(build_unmatched_error(&unmatched, &full_names));
            }

            return Ok(ScopeResult {
                scope: MessageScope::Mentions,
                mentions: matched_base_names,
            });
        }
    }

    // No @mentions → broadcast to everyone
    Ok(ScopeResult {
        scope: MessageScope::Broadcast,
        mentions: vec![],
    })
}

/// Deduplicate a list preserving insertion order.
fn dedup_preserving_order(items: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            result.push(item.clone());
        }
    }
    result
}

/// Check if message should be delivered based on scope.
///
/// Returns true if receiver should get the message.
pub fn should_deliver_message(
    event_data: &Value,
    receiver_name: &str,
    sender_name: &str,
) -> Result<bool, String> {
    if receiver_name == sender_name {
        return Ok(false);
    }

    let scope = event_data
        .get("scope")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Message missing 'scope' field (old format)".to_string())?;

    validate_scope(scope)?;

    match scope {
        "broadcast" => Ok(true),
        "mentions" => {
            let mentions = event_data
                .get("mentions")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                .unwrap_or_default();

            // Strip device suffix for cross-device matching
            let receiver_base = receiver_name.split(':').next().unwrap_or(receiver_name);
            Ok(mentions
                .iter()
                .any(|m| receiver_base == m.split(':').next().unwrap_or(m)))
        }
        _ => Ok(false),
    }
}

/// Build message prefix from envelope fields.
///
/// Format: [intent:thread #id] or [intent #id] or [thread:name #id] or [new message #id]
/// Remote messages: #id:DEVICE
fn build_message_prefix(msg: &Value) -> String {
    let intent = msg.get("intent").and_then(|v| v.as_str());
    let thread = msg.get("thread").and_then(|v| v.as_str());
    let event_id = msg.get("event_id").and_then(|v| v.as_i64());
    let relay = msg.get("_relay");

    // Build ID reference (local or remote)
    let id_ref = if let Some(relay) = relay {
        let short = relay.get("short").and_then(|v| v.as_str()).unwrap_or("");
        let rid = relay.get("id");
        if !short.is_empty()
            && let Some(rid_val) = rid
        {
            let rid_str = match rid_val {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => String::new(),
            };
            if !rid_str.is_empty() {
                format!("#{}:{}", rid_str, short)
            } else {
                String::new()
            }
        } else {
            event_id.map(|id| format!("#{}", id)).unwrap_or_default()
        }
    } else {
        event_id.map(|id| format!("#{}", id)).unwrap_or_default()
    };

    // Build prefix based on envelope fields
    let prefix = match (intent, thread) {
        (Some(i), Some(t)) => format!("{}:{}", i, t),
        (Some(i), None) => i.to_string(),
        (None, Some(t)) => format!("thread:{}", t),
        (None, None) => "new message".to_string(),
    };

    if !id_ref.is_empty() {
        format!("[{} {}]", prefix, id_ref)
    } else {
        format!("[{}]", prefix)
    }
}

/// Format messages for hook feedback.
///
/// Single message uses verbose format: "sender → recipient + N others"
/// Multiple messages use compact format: "sender → recipient (+N)"
///
/// `instance_name`: base name of the receiving instance.
/// `get_instance_data`: callback to get instance data by name (for tag lookup).
/// `get_config_hints`: callback to get config hints.
/// `tip_checker`: optional callback for tip system (has_seen, mark_seen).
#[allow(clippy::type_complexity)]
pub fn format_hook_messages(
    messages: &[Value],
    instance_name: &str,
    get_instance_data: &dyn Fn(&str) -> Option<Value>,
    get_config_hints: &dyn Fn() -> String,
    tip_checker: Option<&dyn Fn(&str, &str) -> (bool, Box<dyn Fn()>)>,
) -> String {
    let recipient_display = get_display_name_from_data(instance_name, get_instance_data);

    let get_sender_display = |sender_base: &str| -> String {
        if let Some(data) = get_instance_data(sender_base) {
            get_full_name_from_value(&data)
        } else {
            sender_base.to_string()
        }
    };

    let reason = if messages.len() == 1 {
        let msg = &messages[0];
        let others = others_count(msg);
        let recipient = if others > 0 {
            let suffix = if others > 1 { "s" } else { "" };
            format!("{} (+{} other{})", recipient_display, others, suffix)
        } else {
            recipient_display.clone()
        };
        let prefix = build_message_prefix(msg);
        let sender_name = msg
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let sender_display = get_sender_display(sender_name);
        let text = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
        format!("{} {} → {}: {}", prefix, sender_display, recipient, text)
    } else {
        let parts: Vec<String> = messages
            .iter()
            .map(|msg| {
                let others = others_count(msg);
                let recipient = if others > 0 {
                    format!("{} (+{})", recipient_display, others)
                } else {
                    recipient_display.clone()
                };
                let prefix = build_message_prefix(msg);
                let sender_name = msg
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let sender_display = get_sender_display(sender_name);
                let text = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
                format!("{} {} → {}: {}", prefix, sender_display, recipient, text)
            })
            .collect();
        format!("[{} new messages] | {}", messages.len(), parts.join(" | "))
    };

    // Append hints
    let mut result = reason;

    // Per-instance hints from data
    let mut hints = String::new();
    if let Some(data) = get_instance_data(instance_name)
        && let Some(h) = data.get("hints").and_then(|v| v.as_str())
        && !h.is_empty()
    {
        hints = h.to_string();
    }
    if hints.is_empty() {
        hints = get_config_hints();
    }
    if !hints.is_empty() {
        result = format!("{} | [{}]", result, hints);
    }

    // Show recv:thread tip on first receipt in each thread
    if let Some(tip_fn) = tip_checker {
        for msg in messages {
            if let Some(thread) = msg.get("thread").and_then(|v| v.as_str()) {
                let tip_key = format!("recv:thread:{thread}");
                let (seen, mark) = tip_fn(instance_name, &tip_key);
                if !seen {
                    mark();
                    result = format!("{}\n{}", result, get_thread_tip_text(instance_name, thread));
                    return result;
                }
            }
        }

        // Show recv:intent tip on first receipt of each intent type
        for msg in messages {
            if let Some(intent) = msg.get("intent").and_then(|v| v.as_str()) {
                let tip_key = format!("recv:intent:{}", intent);
                let (seen, mark) = tip_fn(instance_name, &tip_key);
                if !seen && let Some(tip_text) = get_tip_text(&tip_key) {
                    mark();
                    result = format!("{}\n{}", result, tip_text);
                    break; // Only show one tip per delivery
                }
            }
        }
    }

    result
}

/// Format messages for model injection — wraps in <hcom> tags.
#[allow(clippy::type_complexity)]
pub fn format_messages_json(
    messages: &[Value],
    instance_name: &str,
    get_instance_data: &dyn Fn(&str) -> Option<Value>,
    get_config_hints: &dyn Fn() -> String,
    tip_checker: Option<&dyn Fn(&str, &str) -> (bool, Box<dyn Fn()>)>,
) -> String {
    let formatted = format_hook_messages(
        messages,
        instance_name,
        get_instance_data,
        get_config_hints,
        tip_checker,
    );
    format!("<hcom>{}</hcom>", formatted)
}

/// Get full name from instance data Value.
fn get_full_name_from_value(data: &Value) -> String {
    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let tag = data.get("tag").and_then(|v| v.as_str()).unwrap_or("");
    if !tag.is_empty() {
        format!("{}-{}", tag, name)
    } else {
        name.to_string()
    }
}

/// Get display name for an instance by looking up its data.
fn get_display_name_from_data(
    base_name: &str,
    get_instance_data: &dyn Fn(&str) -> Option<Value>,
) -> String {
    if let Some(data) = get_instance_data(base_name) {
        let full = get_full_name_from_value(&data);
        if !full.is_empty() {
            return full;
        }
    }
    base_name.to_string()
}

/// Count other recipients (excluding self) from a message.
fn others_count(msg: &Value) -> usize {
    msg.get("delivered_to")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len().saturating_sub(1))
        .unwrap_or(0)
}

/// Tip text for recv:intent tips. Delegates to core::tips for centralized text.
fn get_tip_text(tip_key: &str) -> Option<&'static str> {
    crate::core::tips::get_tip(tip_key)
}

fn get_thread_tip_text(instance_name: &str, thread: &str) -> String {
    let sub_id = crate::db::subscriptions::thread_membership_sub_id(thread, instance_name);
    format!(
        "[tip] You joined thread {thread}. To leave: hcom events unsub {sub_id} (find your sub-id with: hcom events sub list)"
    )
}

/// Remove bash escape sequences from message content.
///
/// Bash escapes special characters when constructing commands. Since hcom
/// receives messages as command arguments, we unescape common sequences
/// that don't affect the actual message intent.
///
/// NOTE: We do NOT unescape '\\\\' to '\\'. If double backslashes survived
/// bash processing, the user intended them (e.g., Windows paths, regex, JSON).
pub fn unescape_bash(text: &str) -> String {
    text.replace("\\!", "!")
        .replace("\\$", "$")
        .replace("\\`", "`")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
}

/// Check if instance data represents an external sender.
///
/// External senders have empty/null session_id, no parent_session_id,
/// and no origin_device_id.
fn is_external_sender_data(data: &Value) -> bool {
    // Remote instances are not external
    if data
        .get("origin_device_id")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    // Subagents have parent_session_id, so are not external
    if data
        .get("parent_session_id")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    // External = no session_id
    let session_id = data
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    session_id.is_empty()
}

/// Compute read receipts from pre-fetched data.
///
/// This is a pure function that takes all needed data as parameters
/// (no DB access). The caller is responsible for querying the DB.
///
/// # Arguments
/// * `sent_messages` - Messages sent by this identity: (id, timestamp, data_json)
/// * `active_instances` - All active instances except sender: {name: {tag, origin_device_id, ...}}
/// * `deliver_events` - Set of instance names that have deliver events after each message
/// * `remote_msg_ts` - For remote instances: {name: latest msg_ts}
/// * `max_text_length` - Max text length before truncation
/// * `format_age_fn` - Function to format seconds as age string
#[allow(clippy::too_many_arguments)]
pub fn compute_read_receipts(
    sent_messages: &[(i64, String, Value)],
    active_instances: &HashMap<String, Value>,
    deliver_events_by_msg: &HashMap<i64, HashSet<String>>,
    remote_msg_ts: &HashMap<String, String>,
    max_text_length: usize,
    format_age_fn: &dyn Fn(f64) -> String,
    now_secs: f64,
    parse_timestamp_fn: &dyn Fn(&str) -> Option<f64>,
) -> Vec<ReadReceipt> {
    let mut receipts = Vec::new();

    for (msg_id, msg_timestamp, msg_data) in sent_messages {
        // Validate scope field present
        if msg_data.get("scope").is_none() {
            continue;
        }

        // Use delivered_to for read receipt denominator
        let delivered_to = match msg_data.get("delivered_to").and_then(|v| v.as_array()) {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>(),
            None => continue,
        };

        let explicit_mentions: HashSet<&str> = msg_data
            .get("mentions")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect();
        let msg_text = msg_data.get("text").and_then(|v| v.as_str()).unwrap_or("");

        let delivered_instances = deliver_events_by_msg
            .get(msg_id)
            .cloned()
            .unwrap_or_default();

        let mut read_by = Vec::new();
        for inst_name in &delivered_to {
            let inst_data = active_instances.get(inst_name);

            // Remote instance: compare msg_ts (timestamp-based)
            if let Some(data) = inst_data
                && data
                    .get("origin_device_id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
            {
                if let Some(ts) = remote_msg_ts.get(inst_name)
                    && ts >= msg_timestamp
                {
                    read_by.push(inst_name.clone());
                }
                continue;
            }

            // Local instance: check for deliver event after message
            if delivered_instances.contains(inst_name) {
                // External senders (no session_id, no parent, not remote) only count
                // as "read" if they were an explicitly resolved recipient.
                // This prevents false-positive read receipts for external watchers.
                if let Some(data) = inst_data
                    && is_external_sender_data(data)
                    && !explicit_mentions.contains(inst_name.as_str())
                {
                    continue;
                }
                read_by.push(inst_name.clone());
            }
        }

        let total_recipients = delivered_to.len();
        if total_recipients > 0 {
            let age_str = parse_timestamp_fn(msg_timestamp)
                .map(|msg_time| format_age_fn(now_secs - msg_time))
                .unwrap_or_else(|| "?".to_string());

            let truncated_text = if msg_text.len() > max_text_length {
                format!(
                    "{}...",
                    crate::delivery::truncate_chars(msg_text, max_text_length.saturating_sub(3))
                )
            } else {
                msg_text.to_string()
            };

            receipts.push(ReadReceipt {
                id: *msg_id,
                age: age_str,
                text: truncated_text,
                read_by,
                total_recipients,
            });
        }
    }

    receipts
}

/// Max length for message preview in PTY trigger.
pub const PREVIEW_MAX_LEN: usize = 60;

/// Build truncated message preview for PTY injection.
///
/// Reuses format_hook_messages but truncates before user message content.
/// User content may contain @ chars that trigger autocomplete in some CLIs.
pub fn build_message_preview(formatted: &str, max_len: usize) -> String {
    let wrapper_open = "<hcom>";
    let wrapper_close = "</hcom>";
    let wrapper_len = wrapper_open.len() + wrapper_close.len();

    if formatted.is_empty() {
        return format!("{}{}", wrapper_open, wrapper_close);
    }

    let content_max = max_len.saturating_sub(wrapper_len);
    if content_max == 0 {
        return format!("{}{}", wrapper_open, wrapper_close);
    }

    // Truncate before user content (after first ": ") to avoid special chars
    if let Some(colon_pos) = formatted.find(": ") {
        let envelope = &formatted[..colon_pos];
        if envelope.len() > content_max {
            if content_max <= 3 {
                return format!("{}{}", wrapper_open, wrapper_close);
            }
            return format!(
                "{}{}...{}",
                wrapper_open,
                crate::delivery::truncate_chars(envelope, content_max - 3),
                wrapper_close
            );
        }
        return format!("{}{}{}", wrapper_open, envelope, wrapper_close);
    }

    // No colon found, just truncate normally
    if formatted.len() > content_max {
        if content_max <= 3 {
            return format!("{}{}", wrapper_open, wrapper_close);
        }
        return format!(
            "{}{}...{}",
            wrapper_open,
            crate::delivery::truncate_chars(formatted, content_max - 3),
            wrapper_close
        );
    }
    format!("{}{}{}", wrapper_open, formatted, wrapper_close)
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;
