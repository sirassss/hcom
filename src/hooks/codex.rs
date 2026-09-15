//! Codex native hook handlers and settings management.

use std::collections::{HashMap, HashSet};
use std::io::Write;
#[cfg(not(test))]
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
#[cfg(not(test))]
use std::sync::mpsc;
#[cfg(not(test))]
use std::sync::{Arc, Mutex};
#[cfg(not(test))]
use std::time::Duration;
use std::time::UNIX_EPOCH;

use serde_json::Value;
use toml_edit::{DocumentMut, Item, value};

use crate::db::{HcomDb, InstanceRow};
use crate::hooks::{HookPayload, HookResult, common};
use crate::instance_binding;
use crate::instance_lifecycle as lifecycle;
use crate::instances;
use crate::log;
use crate::paths;
use crate::shared::context::HcomContext;
use crate::shared::{ST_ACTIVE, ST_LISTENING};

use super::common::SAFE_HCOM_COMMANDS;

const HCOM_TRIGGER: &str = "<hcom>";
pub(crate) const CODEX_HOOK_COMMANDS: &[(&str, &str, Option<&str>)] = &[
    (
        "SessionStart",
        "codex-sessionstart",
        Some("startup|resume|clear"),
    ),
    ("UserPromptSubmit", "codex-userpromptsubmit", None),
    ("PreToolUse", "codex-pretooluse", Some("Bash")),
    ("PostToolUse", "codex-posttooluse", Some("Bash")),
    ("Stop", "codex-stop", None),
];
const HCOM_TOOL_NAMES: &[&str] = &[
    "claude",
    "gemini",
    "codex",
    "opencode",
    "antigravity",
    "agy",
];
const CODEX_HOOKS_FEATURE_RENAME_VERSION: (u64, u64, u64) = (0, 129, 0);
const CODEX_HOOK_TRUST_MIN_VERSION: (u64, u64, u64) = (0, 131, 0);
/// Wire value of Codex's `HookSource::User` variant.
///
/// `codex_protocol::protocol::HookSource` is `rename_all = "snake_case"`
/// (codex-rs/protocol/src/protocol.rs:1528) and the app-server v2 mirror is
/// `rename_all = "camelCase"` (the `v2_enum_from_core!` macro in
/// codex-rs/app-server-protocol/src/protocol/v2/shared.rs:21-48, applied at
/// v2/hook.rs:42). Both encodings render the single-word `User` variant as
/// "user", so one literal covers the whole protocol surface.
const CODEX_HOOK_SOURCE_USER: &str = "user";
/// Trust statuses Codex already permits without the bypass flag. Everything
/// else (`untrusted`, `modified`, or a status hcom does not recognize) is what
/// `--dangerously-bypass-hook-trust` would newly unlock — see the gate at
/// codex-rs/hooks/src/engine/discovery.rs:565-571.
const CODEX_ALREADY_PERMITTED_TRUST_STATUSES: &[&str] = &["trusted", "managed"];
/// Every Codex hook event that can carry declarations in a hooks.json file or a
/// `[hooks]` TOML table (codex-rs/config/src/hook_config.rs:36-59). Wider than
/// `CODEX_HOOK_COMMANDS`, which lists only the events hcom itself installs.
const CODEX_ALL_HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];
/// Subdirectories of `$CODEX_HOME/plugins` that hold installed plugins
/// (codex-rs/core-plugins/src/store.rs:21-22).
const CODEX_PLUGIN_STORE_DIRS: &[&str] = &["cache", "data"];
/// Codex's default `project_root_markers`
/// (codex-rs/config/src/project_root_markers.rs:5).
const CODEX_DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];
const HCOM_CODEX_CLI_VERSION_KEY: &str = "hcom_codex_cli_version";
const HCOM_HOOK_DEFINITION_HASH_KEY: &str = "hcom_hook_definition_hash";
#[cfg(not(test))]
const CODEX_APP_SERVER_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(not(test))]
const CODEX_APP_SERVER_STDERR_LIMIT: usize = 8192;
type CodexHookHandler = fn(&HcomDb, &HcomContext, &HookPayload) -> HookResult;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CodexHookTrustEntry {
    key: String,
    command: String,
    current_hash: String,
}

/// One hook from a `codex app-server hooks/list` response, reduced to the
/// fields hcom needs to tell its own hooks apart from everyone else's and to
/// predict what `--dangerously-bypass-hook-trust` would unlock.
///
/// Field names on the wire are camelCase (`HookMetadata` in
/// codex-rs/app-server-protocol/src/protocol/v2/plugin.rs:513-542); the
/// snake_case spellings of the core protocol are accepted too.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodexHookListEntry {
    key: Option<String>,
    command: Option<String>,
    /// Codex's own event label for the entry. Without it a handler parked on
    /// the wrong event is invisible to the inventory — the command alone would
    /// still look like ours.
    ///
    /// Measured on codex-cli 0.154.0 (2026-09-14, `hooks/list` against a
    /// scratch `CODEX_HOME`): the field is **lowerCamelCase** — `sessionStart`,
    /// `preToolUse`, `postToolUse`, `userPromptSubmit`, `stop` — while the
    /// event segment of the same entry's `key` is snake_case (`pre_tool_use`).
    /// The two vocabularies are not interchangeable: comparing this field
    /// against a `key` label rejects every healthy handler.
    event_name: Option<String>,
    /// Set when the entry came from an installed plugin, `null` for a config
    /// layer (measured in the same probe). More direct than the `source` label.
    plugin_id: Option<String>,
    source: Option<String>,
    source_path: Option<PathBuf>,
    enabled: bool,
    trust_status: Option<String>,
    current_hash: Option<String>,
}

/// hcom's launch-time verdict on Codex's hook-trust gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CodexHookTrustState {
    /// Nothing to do: Codex predates the trust gate, or hcom's own trust state
    /// in `hooks.state` is exact and its hooks will run on their own.
    Trusted,
    /// Codex's own `hooks/list` inventory says the invocation-wide bypass would
    /// unlock nothing except hcom's hooks.
    BypassSafeFromHooksList,
    /// `hooks/list` was unavailable, but a purely local scan of every hook
    /// definition that could be in scope found only hcom's own.
    BypassSafeFromLocalScan,
    /// The bypass would — or might — unlock a hook hcom does not own.
    BypassUnsafe { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CodexHookLocalEntry {
    key: String,
    command: String,
    definition_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexHooksFeatureKey {
    CodexHooks,
    Hooks,
}

impl CodexHooksFeatureKey {
    fn as_str(self) -> &'static str {
        match self {
            Self::CodexHooks => "codex_hooks",
            Self::Hooks => "hooks",
        }
    }

    fn alternate(self) -> &'static str {
        match self {
            Self::CodexHooks => "hooks",
            Self::Hooks => "codex_hooks",
        }
    }
}

fn hook_noop() -> HookResult {
    HookResult::Allow {
        additional_context: None,
        system_message: None,
        delivery_ack: None,
    }
}

fn codex_event_name(hook_name: &str) -> &'static str {
    CODEX_HOOK_COMMANDS
        .iter()
        .find(|(_, cmd, _)| *cmd == hook_name)
        .map(|(event, _, _)| *event)
        .unwrap_or("Unknown")
}

/// Derive Codex transcript path from session_id.
pub fn derive_codex_transcript_path(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }

    let codex_base = std::env::var("CODEX_HOME").ok().unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".codex").to_string_lossy().to_string())
            .unwrap_or_default()
    });

    let sessions_dir = PathBuf::from(&codex_base).join("sessions");
    let pattern = format!(
        "{}/**/rollout-*-{}.jsonl",
        sessions_dir.display(),
        session_id
    );

    match glob::glob(&pattern) {
        Ok(entries) => {
            let mut matches: Vec<PathBuf> = entries.filter_map(|e| e.ok()).collect();
            if matches.is_empty() {
                return None;
            }
            matches.sort_by(|a, b| {
                let ta = a
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                let tb = b
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                tb.cmp(&ta)
            });
            matches.first().map(|p| p.to_string_lossy().to_string())
        }
        Err(_) => None,
    }
}

/// Normalize Windows verbatim paths before storing them in the instance row.
///
/// Codex can report the same transcript as `C:\...` on the initial hook and
/// `\\?\C:\...` after a resume. Keep the database representation stable so
/// resume and transcript lookup continue to refer to the same file.
fn normalize_codex_transcript_path(path: &str) -> String {
    const VERBATIM_PREFIX: &str = "\\\\?\\";
    const VERBATIM_UNC_PREFIX: &str = "\\\\?\\UNC\\";

    if let Some(unc_path) = path.strip_prefix(VERBATIM_UNC_PREFIX) {
        format!("\\\\{unc_path}")
    } else {
        path.strip_prefix(VERBATIM_PREFIX)
            .unwrap_or(path)
            .to_string()
    }
}

fn resolve_instance_codex(db: &HcomDb, ctx: &HcomContext, session_id: &str) -> Option<InstanceRow> {
    instance_binding::resolve_instance_from_binding(
        db,
        Some(session_id).filter(|s| !s.is_empty()),
        ctx.process_id.as_deref(),
    )
}

fn resolve_codex_instance(
    db: &HcomDb,
    ctx: &HcomContext,
    payload: &HookPayload,
) -> Option<InstanceRow> {
    let session_id = payload.session_id.as_deref().unwrap_or("");
    resolve_instance_codex(db, ctx, session_id)
}

fn update_codex_position(
    db: &HcomDb,
    ctx: &HcomContext,
    payload: &HookPayload,
    instance_name: &str,
) {
    let mut updates = serde_json::Map::new();
    let cwd = payload
        .raw
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| ctx.cwd.to_string_lossy().to_string());
    if !cwd.is_empty() {
        updates.insert("directory".into(), Value::String(cwd));
    }
    if let Some(session_id) = payload.session_id.as_ref().filter(|s| !s.is_empty()) {
        updates.insert("session_id".into(), Value::String(session_id.clone()));
    }
    let transcript_path = payload.transcript_path.clone().or_else(|| {
        payload
            .session_id
            .as_deref()
            .and_then(derive_codex_transcript_path)
    });
    if let Some(tp) = transcript_path {
        updates.insert(
            "transcript_path".into(),
            Value::String(normalize_codex_transcript_path(&tp)),
        );
    }
    if !updates.is_empty() {
        instances::update_instance_position(db, instance_name, &updates);
    }
}

/// Prepare pending messages for a Codex instance.
///
/// Only additionalContext — no systemMessage. Codex TUI renders both
/// as separate visible lines ("warning:" + "hook context:"), causing
/// double output for every delivered message.
fn prepare_codex_delivery(db: &HcomDb, instance_name: &str) -> Option<HookResult> {
    common::prepare_pending_messages(db, instance_name).map(|prepared| HookResult::Allow {
        additional_context: Some(prepared.formatted),
        system_message: None,
        delivery_ack: Some(prepared.ack),
    })
}

fn resolve_and_update_codex_instance(
    db: &HcomDb,
    ctx: &HcomContext,
    payload: &HookPayload,
) -> Option<InstanceRow> {
    let instance = resolve_codex_instance(db, ctx, payload)?;
    update_codex_position(db, ctx, payload, &instance.name);
    Some(instance)
}

fn set_prompt_active(db: &HcomDb, instance_name: &str) {
    lifecycle::set_status(db, instance_name, ST_ACTIVE, "prompt", Default::default());
}

fn handle_sessionstart(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let session_id = match payload.session_id.as_deref() {
        Some(sid) if !sid.is_empty() => sid,
        _ => return hook_noop(),
    };

    let mut instance_name = if let Some(pid) = ctx.process_id.as_deref() {
        instance_binding::bind_session_to_process(db, session_id, Some(pid))
    } else {
        None
    };

    if instance_name.is_none() {
        instance_name = resolve_codex_instance(db, ctx, payload).map(|i| i.name);
    }

    let instance_name = match instance_name {
        Some(name) => name,
        None => return hook_noop(),
    };

    let _ = db.rebind_instance_session(&instance_name, session_id);
    instance_binding::capture_and_store_launch_context(db, &instance_name);
    update_codex_position(db, ctx, payload, &instance_name);
    lifecycle::set_status(
        db,
        &instance_name,
        ST_LISTENING,
        "start",
        Default::default(),
    );
    crate::runtime_env::set_terminal_title(&instance_name);
    crate::relay::worker::ensure_worker(true);
    common::notify_hook_instance_with_db(db, &instance_name);

    // Bootstrap is injected at launch time via developer_instructions flag,
    // not here — Codex TUI renders hook output visibly ("hook context:").
    hook_noop()
}

fn handle_userpromptsubmit(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_and_update_codex_instance(db, ctx, payload) {
        Some(instance) => instance,
        None => return hook_noop(),
    };

    let prompt = payload
        .raw
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if prompt.trim() != HCOM_TRIGGER {
        set_prompt_active(db, &instance.name);
        return hook_noop();
    }

    if let Some(result) = prepare_codex_delivery(db, &instance.name) {
        result
    } else {
        set_prompt_active(db, &instance.name);
        hook_noop()
    }
}

fn handle_pretooluse(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_and_update_codex_instance(db, ctx, payload) {
        Some(instance) => instance,
        None => return hook_noop(),
    };

    common::update_tool_status(
        db,
        &instance.name,
        "codex",
        &payload.tool_name,
        &payload.tool_input,
    );
    hook_noop()
}

fn handle_posttooluse(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_and_update_codex_instance(db, ctx, payload) {
        Some(instance) => instance,
        None => return hook_noop(),
    };

    prepare_codex_delivery(db, &instance.name).unwrap_or_else(hook_noop)
}

fn handle_stop(db: &HcomDb, ctx: &HcomContext, payload: &HookPayload) -> HookResult {
    let instance = match resolve_and_update_codex_instance(db, ctx, payload) {
        Some(instance) => instance,
        None => return hook_noop(),
    };

    lifecycle::set_status(db, &instance.name, ST_LISTENING, "", Default::default());
    common::notify_hook_instance_with_db(db, &instance.name);
    hook_noop()
}

fn get_codex_handler(hook_name: &str) -> Option<CodexHookHandler> {
    match hook_name {
        "codex-sessionstart" => Some(handle_sessionstart),
        "codex-userpromptsubmit" => Some(handle_userpromptsubmit),
        "codex-pretooluse" => Some(handle_pretooluse),
        "codex-posttooluse" => Some(handle_posttooluse),
        "codex-stop" => Some(handle_stop),
        _ => None,
    }
}

fn dispatch_result_to_stdout(db: &HcomDb, hook_name: &str, result: HookResult) -> i32 {
    match result {
        HookResult::Allow {
            additional_context,
            system_message,
            delivery_ack,
        } => {
            let output = match (hook_name, additional_context, system_message) {
                ("codex-stop", None, None) => Some(serde_json::json!({})),
                (_, Some(ctx), sys) => {
                    let mut obj = serde_json::Map::new();
                    if let Some(msg) = sys {
                        obj.insert("systemMessage".into(), Value::String(msg));
                    }
                    obj.insert(
                        "hookSpecificOutput".into(),
                        serde_json::json!({
                            "hookEventName": codex_event_name(hook_name),
                            "additionalContext": ctx,
                        }),
                    );
                    Some(Value::Object(obj))
                }
                (_, None, Some(msg)) => Some(serde_json::json!({ "systemMessage": msg })),
                _ => None,
            };
            if let Some(json) = output {
                let mut stdout = std::io::stdout().lock();
                if serde_json::to_writer(&mut stdout, &json).is_ok()
                    && stdout.flush().is_ok()
                    && let Some(ack) = delivery_ack.as_ref()
                {
                    common::commit_delivery_ack(db, ack);
                }
            }
            0
        }
        HookResult::Block { reason, .. } => {
            // Codex hooks on exit 2 read the reason from stderr, not stdout.
            let _ = std::io::stderr().lock().write_all(reason.as_bytes());
            2
        }
        HookResult::UpdateInput { updated_input } => {
            let _ = serde_json::to_writer(
                std::io::stdout().lock(),
                &serde_json::json!({ "updatedInput": updated_input }),
            );
            0
        }
    }
}

/// Main entry point for native Codex hooks.
pub fn dispatch_codex_hook_native(hook_name: &str) -> i32 {
    let start = std::time::Instant::now();
    let raw: Value = match serde_json::from_reader(std::io::stdin().lock()) {
        Ok(v) => v,
        Err(e) => {
            log::log_error(
                "hooks",
                "codex.parse_error",
                &format!("hook={hook_name} err={e}"),
            );
            return 0;
        }
    };

    let db = match HcomDb::open() {
        Ok(db) => db,
        Err(e) => {
            log::log_warn(
                "hooks",
                "codex.db_error",
                &format!("hook={hook_name} err={e}"),
            );
            return 0;
        }
    };

    let ctx = HcomContext::from_os();
    if !common::hook_gate_check(&ctx, &db) {
        return 0;
    }

    let payload = HookPayload::from_codex_native(codex_event_name(hook_name), raw);
    let result = common::dispatch_with_panic_guard("codex", hook_name, hook_noop(), || {
        get_codex_handler(hook_name)
            .map(|handler| handler(&db, &ctx, &payload))
            .unwrap_or_else(hook_noop)
    });

    let exit_code = dispatch_result_to_stdout(&db, hook_name, result);
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    log::log_info(
        "hooks",
        "codex.dispatch.timing",
        &format!(
            "hook={} total_ms={:.2} exit_code={}",
            hook_name, total_ms, exit_code
        ),
    );
    exit_code
}

// ---------------------------------------------------------------------------
// Settings management — hooks.json, config.toml, execpolicy
// ---------------------------------------------------------------------------

/// Resolve the Codex config directory.
///
/// Priority: CODEX_HOME env var → tool_config_root()/.codex
fn codex_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEX_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    crate::runtime_env::tool_config_root().join(".codex")
}

/// Get path to Codex config.toml.
pub fn get_codex_config_path() -> PathBuf {
    codex_config_path_at(&codex_config_dir())
}

/// Get path to Codex hooks.json.
pub fn get_codex_hooks_path() -> PathBuf {
    codex_hooks_path_at(&codex_config_dir())
}

/// Get path to Codex execpolicy rules directory.
pub fn get_codex_rules_path() -> PathBuf {
    codex_rules_path_at(&codex_config_dir())
}

fn codex_config_path_at(codex_home: &Path) -> PathBuf {
    codex_home.join("config.toml")
}

fn codex_hooks_path_at(codex_home: &Path) -> PathBuf {
    codex_home.join("hooks.json")
}

fn codex_rules_path_at(codex_home: &Path) -> PathBuf {
    codex_home.join("rules")
}

/// Strip a Windows verbatim prefix and collapse `.`/`..` components.
///
/// Purely lexical, so it works on paths that do not exist.
fn lexically_normalized(path: &Path) -> PathBuf {
    use std::path::Component;

    let text = path.to_string_lossy();
    let plain = text
        .strip_prefix(r"\\?\UNC\")
        .map(|unc| format!(r"\\{unc}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_string));
    let plain = plain.map(PathBuf::from);
    let path = plain.as_deref().unwrap_or(path);

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Whether two paths name the same file, without requiring either to exist.
///
/// Codex passes hook source paths through `AbsolutePathBuf::from_absolute_path`
/// (codex-rs/utils/absolute-path/src/lib.rs:58), which absolutizes lexically but
/// does not resolve symlinks, so a `sourcePath` from Codex can differ from
/// hcom's own `get_codex_hooks_path()` by a `.`/`..` component, a verbatim
/// Windows prefix, or by one side having been canonicalized. Compare lexically
/// first and only then pay for canonicalization.
fn paths_equivalent(a: &Path, b: &Path) -> bool {
    if a == b || lexically_normalized(a) == lexically_normalized(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether a `hooks.state` key names a handler inside hcom's own hooks.json.
///
/// Codex derives these keys as
/// `hook_key(&source.key_source, event_name, group_index, handler_index)` —
/// `"<key_source>:<event_label>:<group>:<handler>"`
/// (codex-rs/hooks/src/lib.rs:105-115) — and for a JSON hook source the
/// key_source is that file's path (codex-rs/hooks/src/engine/discovery.rs:148).
/// Splitting from the right keeps Windows drive colons inside the path part.
fn hook_state_key_belongs_to_hcom_hooks_json(key: &str, hooks_path: &Path) -> bool {
    let mut parts = key.rsplitn(4, ':');
    let (Some(handler_index), Some(group_index), Some(event_label), Some(key_source)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if handler_index.parse::<usize>().is_err() || group_index.parse::<usize>().is_err() {
        return false;
    }
    if !CODEX_HOOK_COMMANDS
        .iter()
        .any(|(event, _, _)| codex_hook_event_state_label(event) == event_label)
    {
        return false;
    }
    paths_equivalent(Path::new(key_source), hooks_path)
}

fn build_codex_hook_command(command: &str) -> String {
    let mut parts = crate::runtime_env::get_hcom_prefix();
    parts.push(command.to_string());
    parts.join(" ")
}

fn build_expected_hook_json() -> Value {
    let mut hooks = serde_json::Map::new();
    for (event, command, matcher) in CODEX_HOOK_COMMANDS {
        let mut group = serde_json::Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".into(), Value::String((*matcher).to_string()));
        }
        group.insert(
            "hooks".into(),
            Value::Array(vec![serde_json::json!({
                "type": "command",
                "command": build_codex_hook_command(command),
            })]),
        );
        hooks.insert(
            (*event).to_string(),
            Value::Array(vec![Value::Object(group)]),
        );
    }
    Value::Object(serde_json::Map::from_iter([(
        "hooks".into(),
        Value::Object(hooks),
    )]))
}

fn is_hcom_codex_command(command: &str) -> bool {
    CODEX_HOOK_COMMANDS.iter().any(|(_, suffix, _)| {
        command == build_codex_hook_command(suffix) || command.ends_with(suffix)
    })
}

fn is_hcom_legacy_notify(item: &Item) -> bool {
    match item {
        Item::Value(v) => {
            if let Some(s) = v.as_str() {
                return s.contains("hcom") && s.contains("codex-notify");
            }
            if let Some(arr) = v.as_array() {
                let values: Vec<&str> = arr.iter().filter_map(|entry| entry.as_str()).collect();
                return values.iter().any(|s| s.contains("hcom"))
                    && values.iter().any(|s| s.contains("codex-notify"));
            }
            false
        }
        _ => false,
    }
}

fn merge_hcom_hooks(existing: &mut Value) {
    if !existing.is_object() {
        *existing = serde_json::json!({ "hooks": {} });
    }

    // Strip existing hcom hooks first so stale matchers don't accumulate.
    remove_hcom_hooks_from_json(existing);

    let hooks_obj = existing
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks_obj.is_object() {
        *hooks_obj = serde_json::json!({});
    }

    let current_hooks = hooks_obj.as_object_mut().unwrap();
    let expected = build_expected_hook_json();
    let expected_hooks = expected["hooks"].as_object().unwrap();

    for (event, expected_groups) in expected_hooks {
        let entry = current_hooks
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !entry.is_array() {
            *entry = Value::Array(Vec::new());
        }
        let groups = entry.as_array_mut().unwrap();

        for expected_group in expected_groups.as_array().unwrap() {
            let expected_matcher = expected_group.get("matcher").and_then(|v| v.as_str());
            let new_hooks = expected_group["hooks"].as_array().unwrap();

            let matched = groups
                .iter_mut()
                .find(|g| g.get("matcher").and_then(|v| v.as_str()) == expected_matcher);

            if let Some(group) = matched {
                if !group.get("hooks").is_some_and(|v| v.is_array()) {
                    group
                        .as_object_mut()
                        .unwrap()
                        .insert("hooks".into(), Value::Array(Vec::new()));
                }
                let hooks_arr = group
                    .get_mut("hooks")
                    .and_then(|v| v.as_array_mut())
                    .unwrap();
                hooks_arr.retain(|h| {
                    !h.get("command")
                        .and_then(|v| v.as_str())
                        .is_some_and(is_hcom_codex_command)
                });
                hooks_arr.extend(new_hooks.iter().cloned());
            } else {
                groups.push(expected_group.clone());
            }
        }
    }
}

fn remove_hcom_hooks_from_json(existing: &mut Value) {
    let Some(hooks_obj) = existing.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return;
    };

    for (_, groups) in hooks_obj.iter_mut() {
        let Some(groups_arr) = groups.as_array_mut() else {
            continue;
        };
        for group in groups_arr.iter_mut() {
            if let Some(hooks_arr) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) {
                hooks_arr.retain(|h| {
                    !h.get("command")
                        .and_then(|v| v.as_str())
                        .is_some_and(is_hcom_codex_command)
                });
            }
        }
        groups_arr.retain(|group| {
            group
                .get("hooks")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| !arr.is_empty())
        });
    }

    hooks_obj.retain(|_, groups| groups.as_array().is_some_and(|arr| !arr.is_empty()));
    if hooks_obj.is_empty() {
        existing.as_object_mut().unwrap().remove("hooks");
    }
}

/// Returns true if `hook` is a legacy hcom Codex entry written in the old
/// `"type":"cmd"` / `"cmd"` format used before Codex 0.129.
fn is_legacy_hcom_codex_cmd_entry(hook: &Value) -> bool {
    hook.get("type").and_then(|v| v.as_str()) == Some("cmd")
        && hook.get("cmd").and_then(|v| v.as_str()).is_some_and(|cmd| {
            CODEX_HOOK_COMMANDS
                .iter()
                .any(|(_, suffix, _)| cmd.ends_with(suffix))
        })
}

/// Remove recognized legacy `"cmd"`-keyed hcom hook entries.
/// Only called when Codex >= CODEX_HOOKS_FEATURE_RENAME_VERSION, which is when
/// the current `"command"`-keyed format is known to be supported.
fn remove_legacy_hcom_cmd_hooks_from_json(existing: &mut Value) {
    let Some(hooks_obj) = existing.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return;
    };
    for (_, groups) in hooks_obj.iter_mut() {
        let Some(groups_arr) = groups.as_array_mut() else {
            continue;
        };
        for group in groups_arr.iter_mut() {
            if let Some(hooks_arr) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) {
                hooks_arr.retain(|h| !is_legacy_hcom_codex_cmd_entry(h));
            }
        }
        groups_arr.retain(|group| {
            group
                .get("hooks")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| !arr.is_empty())
        });
    }
    hooks_obj.retain(|_, groups| groups.as_array().is_some_and(|arr| !arr.is_empty()));
    if hooks_obj.is_empty() {
        existing.as_object_mut().unwrap().remove("hooks");
    }
}

fn codex_hook_event_state_label(event: &str) -> &'static str {
    match event {
        "PreToolUse" => "pre_tool_use",
        "PermissionRequest" => "permission_request",
        "PostToolUse" => "post_tool_use",
        "PreCompact" => "pre_compact",
        "PostCompact" => "post_compact",
        "SessionStart" => "session_start",
        "UserPromptSubmit" => "user_prompt_submit",
        "Stop" => "stop",
        _ => "unknown",
    }
}

/// Codex's wire spelling of an event in `hooks/list` (`HookEventName`):
/// lowerCamelCase, unlike the snake_case label the hook `key` carries.
fn codex_hook_event_wire_name(event: &str) -> String {
    let mut chars = event.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn hcom_hook_definition_hash(event: &str, group: &Value, hook: &Value) -> String {
    use sha2::{Digest, Sha256};

    let definition = serde_json::json!({
        "event": event,
        "matcher": group.get("matcher").cloned().unwrap_or(Value::Null),
        "hook": hook,
    });
    let encoded = serde_json::to_vec(&definition).unwrap_or_default();
    let digest = Sha256::digest(&encoded);
    let hex = digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(&mut acc, "{b:02x}");
        acc
    });
    format!("sha256:{hex}")
}

fn hcom_hook_local_entries_from_hooks_json(
    json: &Value,
    hooks_path: &Path,
) -> Vec<CodexHookLocalEntry> {
    let source = hooks_path.to_path_buf();
    let Some(hooks_obj) = json.get("hooks").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for (event, _, _) in CODEX_HOOK_COMMANDS {
        let Some(groups) = hooks_obj.get(*event).and_then(|v| v.as_array()) else {
            continue;
        };
        for (group_index, group) in groups.iter().enumerate() {
            let Some(hooks) = group.get("hooks").and_then(|v| v.as_array()) else {
                continue;
            };
            for (handler_index, hook) in hooks.iter().enumerate() {
                let Some(command) = hook.get("command").and_then(|v| v.as_str()) else {
                    continue;
                };
                if is_hcom_codex_command(command) {
                    entries.push(CodexHookLocalEntry {
                        key: format!(
                            "{}:{}:{}:{}",
                            source.display(),
                            codex_hook_event_state_label(event),
                            group_index,
                            handler_index
                        ),
                        command: command.to_string(),
                        definition_hash: hcom_hook_definition_hash(event, group, hook),
                    });
                }
            }
        }
    }
    entries
}

fn hcom_hook_state_keys_from_hooks_json(json: &Value, hooks_path: &Path) -> HashSet<String> {
    hcom_hook_local_entries_from_hooks_json(json, hooks_path)
        .into_iter()
        .map(|entry| entry.key)
        .collect()
}

fn hcom_hook_definition_hashes_from_hooks_json(
    json: &Value,
    hooks_path: &Path,
) -> HashMap<String, String> {
    hcom_hook_local_entries_from_hooks_json(json, hooks_path)
        .into_iter()
        .map(|entry| (entry.key, entry.definition_hash))
        .collect()
}

fn hcom_hook_definition_hashes_from_hooks_path(
    hooks_path: &Path,
) -> Result<HashMap<String, String>, VerifyFailReason> {
    let hooks_content = std::fs::read_to_string(hooks_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            VerifyFailReason::HooksPathMissing(hooks_path.to_path_buf())
        }
        _ => VerifyFailReason::HooksUnreadable(hooks_path.to_path_buf()),
    })?;
    let hooks_json: Value = serde_json::from_str(&hooks_content)
        .map_err(|_| VerifyFailReason::HooksUnreadable(hooks_path.to_path_buf()))?;
    Ok(hcom_hook_definition_hashes_from_hooks_json(
        &hooks_json,
        hooks_path,
    ))
}

fn hcom_hook_local_entries_from_hooks_path(
    hooks_path: &Path,
) -> Result<Vec<CodexHookLocalEntry>, VerifyFailReason> {
    let hooks_content = std::fs::read_to_string(hooks_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            VerifyFailReason::HooksPathMissing(hooks_path.to_path_buf())
        }
        _ => VerifyFailReason::HooksUnreadable(hooks_path.to_path_buf()),
    })?;
    let hooks_json: Value = serde_json::from_str(&hooks_content)
        .map_err(|_| VerifyFailReason::HooksUnreadable(hooks_path.to_path_buf()))?;
    Ok(hcom_hook_local_entries_from_hooks_json(
        &hooks_json,
        hooks_path,
    ))
}

fn expected_hcom_hook_commands() -> HashSet<String> {
    CODEX_HOOK_COMMANDS
        .iter()
        .map(|(_, command, _)| build_codex_hook_command(command))
        .collect()
}

/// `(hooks.state event label, command)` for each hook hcom installs. Lets tests
/// in other modules build realistic hooks/list responses.
#[cfg(test)]
pub(crate) fn test_expected_hook_specs() -> Vec<(&'static str, String)> {
    CODEX_HOOK_COMMANDS
        .iter()
        .map(|(event, command, _)| {
            (
                codex_hook_event_state_label(event),
                build_codex_hook_command(command),
            )
        })
        .collect()
}

/// Read one string field, accepting the camelCase wire spelling and the
/// snake_case spelling of the core protocol.
fn hook_list_str_field<'a>(hook: &'a Value, camel: &str, snake: &str) -> Option<&'a str> {
    hook.get(camel)
        .or_else(|| hook.get(snake))
        .and_then(|v| v.as_str())
}

fn parse_codex_hook_list_entries(value: &Value) -> Result<Vec<CodexHookListEntry>, String> {
    // Every group, not just `data[0]`: Codex returns one group per hook layer,
    // so reading the first alone would drop a plugin's handlers whenever the
    // user layer is listed first — and report "no plugin hooks" while the
    // plugin is firing.
    let groups = value
        .pointer("/result/data")
        .or_else(|| value.pointer("/data"))
        .and_then(|v| v.as_array());
    let hooks: Vec<&Value> = match groups {
        Some(groups) => {
            let mut collected = Vec::new();
            for group in groups {
                // A group Codex could not evaluate is not an empty group.
                // Swallowing its `errors` would turn a failed inventory into
                // "no hooks installed", and that answer is what decides
                // whether hcom installs anything.
                if let Some(errors) = group.get("errors").and_then(|v| v.as_array())
                    && !errors.is_empty()
                {
                    return Err(format!(
                        "codex hooks/list reported errors: {}",
                        Value::Array(errors.clone())
                    ));
                }
                let Some(hooks) = group.get("hooks").and_then(|v| v.as_array()) else {
                    return Err(
                        "codex hooks/list returned a group without a hooks array".to_string()
                    );
                };
                collected.extend(hooks.iter());
            }
            collected
        }
        None => value
            .get("hooks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "codex hooks/list response did not contain hooks".to_string())?
            .iter()
            .collect(),
    };

    Ok(hooks
        .iter()
        .map(|hook| CodexHookListEntry {
            key: hook_list_str_field(hook, "key", "key").map(str::to_string),
            command: hook_list_str_field(hook, "command", "command").map(str::to_string),
            event_name: hook_list_str_field(hook, "eventName", "event_name").map(str::to_string),
            plugin_id: hook_list_str_field(hook, "pluginId", "plugin_id").map(str::to_string),
            source: hook_list_str_field(hook, "source", "source").map(str::to_string),
            source_path: hook_list_str_field(hook, "sourcePath", "source_path").map(PathBuf::from),
            // Codex only treats an explicit `false` as disabled; absent means
            // enabled (default_enabled in the v2 protocol shared module).
            enabled: hook
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            trust_status: hook_list_str_field(hook, "trustStatus", "trust_status")
                .map(str::to_string),
            current_hash: hook_list_str_field(hook, "currentHash", "current_hash")
                .map(str::to_string),
        })
        .collect())
}

/// Whether a `hooks/list` entry is one of hcom's own hook handlers.
///
/// Command equality alone is not identity: any repository can ship a
/// `.codex/hooks.json` containing `{"command": "hcom codex-pretooluse"}`, and a
/// command-only test would let that entry collect hcom's trust state or pass as
/// "already ours" when deciding on the bypass. The entry must also come from the
/// user layer and from hcom's own hooks.json, the single file hcom writes.
fn hook_list_entry_is_hcom_owned(
    entry: &CodexHookListEntry,
    expected_commands: &HashSet<String>,
    hooks_path: &Path,
) -> bool {
    entry
        .command
        .as_deref()
        .is_some_and(|command| expected_commands.contains(command))
        && entry.source.as_deref() == Some(CODEX_HOOK_SOURCE_USER)
        && entry
            .source_path
            .as_deref()
            .is_some_and(|path| paths_equivalent(path, hooks_path))
}

fn describe_hook_list_entry(entry: &CodexHookListEntry) -> String {
    let what = entry
        .command
        .as_deref()
        .or(entry.key.as_deref())
        .unwrap_or("<unnamed hook>");
    match entry.source_path.as_deref() {
        Some(path) => format!("{what} in {}", path.display()),
        None => what.to_string(),
    }
}

/// Hooks that `--dangerously-bypass-hook-trust` would unlock and hcom does not
/// own.
///
/// The flag is invocation-wide for every non-managed hook source — user layer,
/// project layer, and plugins alike (codex-rs/hooks/src/engine/discovery.rs:150,
/// :245, :565-571) — and it also suppresses Codex's own "Hooks need review"
/// prompt (codex-rs/tui/src/startup_hooks_review.rs:245-247). Codex's own help
/// text spells out the contract: "Intended only for automation that already vets
/// hook sources." This is that vetting step, so the flag is only safe when every
/// hook it would newly permit belongs to hcom.
///
/// Note that foreign hooks routinely live in hcom's own hooks.json — hcom merges
/// its entries into whatever file is already there — so the source path alone is
/// never identity.
fn foreign_hooks_unlocked_by_bypass(
    entries: &[CodexHookListEntry],
    hooks_path: &Path,
) -> Vec<String> {
    let expected = expected_hcom_hook_commands();
    entries
        .iter()
        .filter(|entry| {
            // A status hcom does not recognize counts as unlockable: hcom must
            // not reimplement Codex's currentHash algorithm, so it cannot prove
            // such a hook is already trusted.
            let unlockable = entry
                .trust_status
                .as_deref()
                .is_none_or(|status| !CODEX_ALREADY_PERMITTED_TRUST_STATUSES.contains(&status));
            entry.enabled
                && unlockable
                && !hook_list_entry_is_hcom_owned(entry, &expected, hooks_path)
        })
        .map(describe_hook_list_entry)
        .collect()
}

/// `HookSource::Plugin` — an entry contributed by an installed Codex plugin
/// rather than by a config layer.
const CODEX_HOOK_SOURCE_PLUGIN: &str = "plugin";

/// What Codex's own hook inventory says about hcom's Codex handlers.
///
/// Deliberately separate from [`hook_list_entry_is_hcom_owned`]: that predicate
/// answers "may hcom write trust state for this entry", and widening it to
/// accept plugin entries would hand plugin hooks the invocation-wide trust
/// bypass. This enum only describes what is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexPluginState {
    Active,
    ReviewRequired,
    Disabled,
    Duplicate,
    LegacyOnly,
    Discovered,
    Incompatible,
    Incomplete,
    Missing,
    Unverified,
}

impl CodexPluginState {
    /// The spec's headlines, verbatim. None of the states reachable without a
    /// usable inventory may render as active or as a double-fire observation.
    pub(crate) fn headline(self) -> &'static str {
        match self {
            Self::Active => "installed (plugin hooks active)",
            Self::ReviewRequired => "installed; hook review required",
            Self::Disabled => "installed; hooks disabled",
            Self::Duplicate => "duplicate hooks; double-fire risk",
            Self::LegacyOnly => "installed (legacy native hooks)",
            Self::Discovered => "plugin discovered; activation unverified",
            Self::Incompatible => "incompatible Claude handlers",
            Self::Incomplete => "incomplete hook set",
            // Task 4 splits this on ClaudePresence into
            // "not active; import from Claude required" when Claude is present.
            // The probe belongs there, so the pure classifier stays inventory-only.
            Self::Missing => "not installed",
            Self::Unverified => "state unverified",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexPluginStatus {
    pub state: CodexPluginState,
    pub details: Vec<String>,
}

/// `(command, event label)` for every handler hcom's Codex integration installs.
/// Both halves must match: a correct command parked on the wrong event is not a
/// working handler, and Codex reports the event it actually bound.
fn expected_codex_handlers() -> Vec<(Vec<String>, String)> {
    CODEX_HOOK_COMMANDS
        .iter()
        .map(|(event, command, _)| {
            (
                handler_command_forms(command),
                codex_hook_event_wire_name(event),
            )
        })
        .collect()
}

/// Both command spellings hcom ships for one handler.
///
/// The native installer writes the resolved `hcom codex-stop`, while the
/// committed plugin manifest ships the self-resolving
/// `cmd=${HCOM:-hcom}; … exec $cmd codex-stop || exit 0` guard — a static file
/// cannot embed an install-time path. Matching only the first form makes the
/// shipped plugin's own handlers invisible to this classifier, which would
/// report a live plugin as missing. Exact equality against both forms, never a
/// substring scan for `hcom`.
fn handler_command_forms(suffix: &str) -> Vec<String> {
    vec![
        build_codex_hook_command(suffix),
        crate::hooks::claude::build_hook_entry_command(suffix),
    ]
}

/// Claude's handlers, which Codex must never be running. Built from Claude's own
/// registry so a handler added there cannot silently become unrecognized here.
fn claude_handler_commands() -> HashSet<String> {
    crate::hooks::claude::CLAUDE_HOOK_COMMANDS
        .iter()
        .flat_map(|suffix| handler_command_forms(suffix))
        .collect()
}

/// Which origin an inventory entry came from, as far as the classifier cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HandlerOrigin {
    Plugin,
    Legacy,
    Foreign,
}

fn handler_origin(
    entry: &CodexHookListEntry,
    hooks_path: &Path,
    plugin_roots: &[PathBuf],
) -> HandlerOrigin {
    if entry.plugin_id.is_some() {
        return HandlerOrigin::Plugin;
    }
    let from_plugin_path = entry.source_path.as_deref().is_some_and(|path| {
        plugin_roots
            .iter()
            .any(|root| path_is_within(path, root.as_path()))
    });
    if entry.source.as_deref() == Some(CODEX_HOOK_SOURCE_PLUGIN) || from_plugin_path {
        return HandlerOrigin::Plugin;
    }
    if entry.source.as_deref() == Some(CODEX_HOOK_SOURCE_USER)
        && entry
            .source_path
            .as_deref()
            .is_some_and(|path| paths_equivalent(path, hooks_path))
    {
        return HandlerOrigin::Legacy;
    }
    HandlerOrigin::Foreign
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    path.ancestors().any(|ancestor| ancestor == root)
}

/// One origin's view of the handler set.
#[derive(Default)]
struct OriginHandlers {
    present: HashSet<String>,
    /// Events whose handler this origin has *enabled*. Double-fire is about
    /// what runs, so the overlap check uses this, not `present`.
    enabled_events: HashSet<String>,
    disabled: Vec<String>,
    unreviewed: Vec<String>,
}

impl OriginHandlers {
    fn is_complete(&self, expected: usize) -> bool {
        self.present.len() == expected
    }
    fn has_any(&self) -> bool {
        !self.present.is_empty()
    }
}

/// Classify hcom's Codex hook activation from an inventory Codex itself
/// returned. Pure: every input is passed in, so every spec state is reachable
/// from a fixture.
///
/// `plugin_roots` are directories whose contents Codex loads as plugins; an
/// entry rooted there is plugin-sourced even when its `source` label is absent.
/// Their mere existence never upgrades a state beyond `Discovered` — a
/// discovery hint cannot stand in for a missing runtime handler.
pub(crate) fn classify_codex_plugin_hooks(
    entries: &[CodexHookListEntry],
    hooks_path: &Path,
    plugin_roots: &[PathBuf],
) -> CodexPluginStatus {
    let expected = expected_codex_handlers();
    let claude_commands = claude_handler_commands();
    let mut details = Vec::new();
    let mut incompatible = Vec::new();
    let mut plugin = OriginHandlers::default();
    let mut legacy = OriginHandlers::default();

    for entry in entries {
        let Some(command) = entry.command.as_deref() else {
            continue;
        };
        if claude_commands.contains(command) {
            incompatible.push(describe_hook_list_entry(entry));
            continue;
        }
        let Some((forms, expected_event)) = expected
            .iter()
            .find(|(forms, _)| forms.iter().any(|form| form == command))
        else {
            continue;
        };
        // Identity is the canonical form, so the same handler counts once
        // whichever spelling Codex reports it under.
        let command = forms[0].as_str();
        // Affirmative event identity only: an entry that does not say which
        // event it is bound to has not shown it is bound to the right one.
        // Codex sends `eventName` on every entry (measured); the hook key
        // carries the same event in its own snake_case spelling, so either
        // proves it.
        let key_event = entry.key.as_deref().map(hcom_wire_event_for_hook_state_key);
        let event = entry.event_name.clone().or(key_event);
        if event.as_deref() != Some(expected_event.as_str()) {
            details.push(format!(
                "{command} bound to {} (expected {expected_event}) in {}",
                event.as_deref().unwrap_or("<no event identity>"),
                entry
                    .source_path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown source>".to_string()),
            ));
            continue;
        }

        let bucket = match handler_origin(entry, hooks_path, plugin_roots) {
            HandlerOrigin::Plugin => &mut plugin,
            HandlerOrigin::Legacy => &mut legacy,
            // A foreign hooks file can carry hcom's exact command; it is not
            // hcom's installation and must not complete anyone's handler set.
            HandlerOrigin::Foreign => {
                details.push(format!("foreign {}", describe_hook_list_entry(entry)));
                continue;
            }
        };
        bucket.present.insert(command.to_string());
        if entry.enabled {
            bucket.enabled_events.insert(expected_event.clone());
        } else {
            bucket.disabled.push(describe_hook_list_entry(entry));
        }
        let reviewed = entry
            .trust_status
            .as_deref()
            .is_some_and(|status| CODEX_ALREADY_PERMITTED_TRUST_STATUSES.contains(&status));
        if !reviewed {
            bucket.unreviewed.push(describe_hook_list_entry(entry));
        }
    }

    if !incompatible.is_empty() {
        details.extend(incompatible);
        return CodexPluginStatus {
            state: CodexPluginState::Incompatible,
            details,
        };
    }

    let total = expected.len();
    let plugin_complete = plugin.is_complete(total);
    let legacy_complete = legacy.is_complete(total);

    // Double-fire is per event and about what is *enabled*: one enabled legacy
    // handler beside a complete enabled plugin set fires twice on that event,
    // and two complete sets whose legacy half is disabled fire once. Requiring
    // two complete sets would miss the first and misreport the second.
    let mut double_fired: Vec<&String> = plugin
        .enabled_events
        .intersection(&legacy.enabled_events)
        .collect();
    double_fired.sort();

    let state = if !double_fired.is_empty() {
        details.push(format!(
            "both a plugin and a legacy handler are enabled for {}; legacy lives in {}",
            double_fired
                .iter()
                .map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            hooks_path.display()
        ));
        CodexPluginState::Duplicate
    } else if plugin_complete {
        if legacy.has_any() {
            details.push(format!(
                "{} of hcom's legacy handlers are also installed in {} (none enabled alongside the plugin)",
                legacy.present.len(),
                hooks_path.display()
            ));
        }
        if !plugin.disabled.is_empty() {
            CodexPluginState::Disabled
        } else if !plugin.unreviewed.is_empty() {
            CodexPluginState::ReviewRequired
        } else {
            CodexPluginState::Active
        }
    } else if legacy_complete && !plugin.has_any() {
        CodexPluginState::LegacyOnly
    } else if plugin.has_any() || legacy.has_any() {
        let found: HashSet<&String> = plugin.present.union(&legacy.present).collect();
        let mut missing: Vec<&str> = expected
            .iter()
            .map(|(forms, _)| forms[0].as_str())
            .filter(|command| !found.contains(&command.to_string()))
            .collect();
        missing.sort_unstable();
        details.push(format!("missing handlers: {}", missing.join(", ")));
        CodexPluginState::Incomplete
    } else if !plugin_roots.is_empty() {
        details.push("plugin store is populated but no handler reached the inventory".to_string());
        CodexPluginState::Discovered
    } else {
        CodexPluginState::Missing
    };

    for (label, bucket) in [("plugin", &plugin), ("legacy", &legacy)] {
        for disabled in &bucket.disabled {
            details.push(format!("{label} handler disabled: {disabled}"));
        }
        for unreviewed in &bucket.unreviewed {
            details.push(format!("{label} handler not trusted: {unreviewed}"));
        }
    }

    CodexPluginStatus { state, details }
}

/// Directories whose contents Codex loads as plugins, restricted to those that
/// actually exist — an empty store is not a discovery hint.
fn codex_plugin_roots(codex_home: &Path) -> Vec<PathBuf> {
    let plugins_root = codex_home.join("plugins");
    CODEX_PLUGIN_STORE_DIRS
        .iter()
        .map(|sub| plugins_root.join(sub))
        .filter(|dir| std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()))
        .collect()
}

/// Whether the Claude CLI is on this machine, which decides how Codex gets the
/// hcom plugin: Codex can import an installed Claude plugin, and that flow is
/// interactive, so hcom must not install anything itself when Claude is there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaudePresence {
    Present,
    Absent,
    /// The probe neither confirmed nor ruled Claude out (permission denied, a
    /// non-zero exit, a hang). Nothing may be written in this state: installing
    /// on a machine that turns out to have Claude creates the duplicate the
    /// import route exists to avoid.
    Indeterminate(String),
}

/// How long the Claude probe may take before it counts as indeterminate.
const CLAUDE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) fn claude_presence() -> ClaudePresence {
    let Some(binary) = crate::terminal::which_bin("claude") else {
        return ClaudePresence::Absent;
    };
    let mut child = match std::process::Command::new(&binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        // Resolution succeeded but the file will not run — not evidence that
        // Claude is absent.
        Err(error) => return ClaudePresence::Indeterminate(format!("{binary}: {error}")),
    };

    let deadline = std::time::Instant::now() + CLAUDE_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return ClaudePresence::Present,
            Ok(Some(status)) => {
                return ClaudePresence::Indeterminate(format!("{binary} --version: {status}"));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return ClaudePresence::Indeterminate(format!(
                        "{binary} --version did not answer within {}s",
                        CLAUDE_PROBE_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => return ClaudePresence::Indeterminate(format!("{binary}: {error}")),
        }
    }
}

/// What `hcom hooks add codex` accomplished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodexAddOutcome {
    /// Codex's own inventory already shows the complete handler set running.
    AlreadyActive,
    /// Nothing was installed and the user must do something. Exit 2.
    ActionRequired(String),
    /// A CLI install reported success but the inventory has not confirmed the
    /// handlers yet. Exit 2 — install success is not activation.
    InstalledUnverified(String),
}

/// Decide what `hooks add codex` should do, given what Codex reports and
/// whether Claude is around. Pure, so every branch is reachable from a fixture.
pub(crate) fn plan_codex_add(
    status: &CodexPluginStatus,
    claude: &ClaudePresence,
    claude_plugin_installed: bool,
) -> CodexAddPlan {
    use CodexAddPlan::{InstallNatively, Report};

    let detail = |extra: &str| {
        let mut text = extra.to_string();
        for line in &status.details {
            text.push_str("\n  ");
            text.push_str(line);
        }
        text
    };

    match status.state {
        CodexPluginState::Active => Report(CodexAddOutcome::AlreadyActive),
        // Never strip Codex's legacy entries as a side effect of `add`: unlike
        // Claude's, they are the only thing firing until the plugin is trusted.
        CodexPluginState::Duplicate => Report(CodexAddOutcome::ActionRequired(detail(
            "plugin and legacy hooks are both active. Once the plugin is trusted, run: \
             hcom hooks remove codex --legacy-only",
        ))),
        CodexPluginState::ReviewRequired => Report(CodexAddOutcome::ActionRequired(detail(
            "open Codex and review/trust hcom's hooks, then: hcom hooks status",
        ))),
        CodexPluginState::Disabled => Report(CodexAddOutcome::ActionRequired(detail(
            "enable hcom's hooks in Codex, then: hcom hooks status",
        ))),
        CodexPluginState::Incompatible => Report(CodexAddOutcome::ActionRequired(detail(
            "Codex is running Claude's handlers, which cannot serve Codex sessions. \
             Reinstall the plugin so the Codex overlay is selected.",
        ))),
        CodexPluginState::Unverified => Report(CodexAddOutcome::ActionRequired(detail(
            "Codex's hook inventory is unavailable, so nothing was installed.",
        ))),
        CodexPluginState::LegacyOnly => Report(CodexAddOutcome::ActionRequired(detail(
            "hcom's native Codex hooks are in place and working. To migrate to the plugin, \
             install it first and remove the legacy entries only once it is trusted: \
             hcom hooks remove codex --legacy-only",
        ))),
        CodexPluginState::Missing | CodexPluginState::Incomplete | CodexPluginState::Discovered => {
            match claude {
                ClaudePresence::Present => {
                    let mut text = String::new();
                    if !claude_plugin_installed {
                        text.push_str("first: hcom hooks add claude\nthen: ");
                    }
                    text.push_str(
                        "in Codex run /import, pick the hcom plugin and its skill (not the \
                         standalone hcom skill), restart Codex, review the hooks, then: \
                         hcom hooks status",
                    );
                    Report(CodexAddOutcome::ActionRequired(detail(&text)))
                }
                ClaudePresence::Indeterminate(why) => {
                    Report(CodexAddOutcome::ActionRequired(detail(&format!(
                        "could not determine whether Claude is installed ({why}), so nothing was installed. Re-run once `claude --version` answers."
                    ))))
                }
                ClaudePresence::Absent => InstallNatively,
            }
        }
    }
}

/// Either report the current state or install through the tool CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodexAddPlan {
    Report(CodexAddOutcome),
    InstallNatively,
}

/// `hcom hooks add codex`: consume Codex's own inventory, then either report
/// what the user must do or run the plugin install.
pub(crate) fn add_codex_plugin() -> Result<CodexAddOutcome, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let status = codex_plugin_status(&cwd);
    let plan = plan_codex_add(
        &status,
        &claude_presence(),
        crate::hooks::plugin::verify_claude_plugin_installed(),
    );
    match plan {
        CodexAddPlan::Report(outcome) => Ok(outcome),
        CodexAddPlan::InstallNatively => {
            crate::hooks::plugin::install_codex_plugin()?;
            // Install success is not activation: ask Codex again.
            match codex_plugin_status(&cwd).state {
                CodexPluginState::Active => Ok(CodexAddOutcome::AlreadyActive),
                state => Ok(CodexAddOutcome::InstalledUnverified(format!(
                    "plugin installed; Codex reports: {}. Restart Codex, review the hooks, \
                     then: hcom hooks status",
                    state.headline()
                ))),
            }
        }
    }
}

/// Fetch Codex's hook inventory and classify it. A failed or timed-out fetch is
/// `Unverified`, never "not installed": the discovery hints below it are not
/// evidence of what is running.
pub(crate) fn codex_plugin_status(cwd: &Path) -> CodexPluginStatus {
    codex_plugin_status_at(cwd, &codex_config_dir())
}

/// Inspect the same config home the child will use, including launch overrides.
pub(crate) fn codex_plugin_status_at(cwd: &Path, codex_home: &Path) -> CodexPluginStatus {
    // No Codex on this machine is "not installed", not "unverified": the
    // unverified state means hcom could not read an inventory Codex would
    // otherwise have, and its remediation tells the user to run a binary that
    // is not there.
    if crate::terminal::which_bin("codex").is_none() {
        return CodexPluginStatus {
            state: CodexPluginState::Missing,
            details: vec!["no codex executable on PATH".to_string()],
        };
    }
    match fetch_codex_hook_list(cwd, codex_home) {
        Ok(entries) => classify_codex_plugin_hooks(
            &entries,
            &codex_hooks_path_at(codex_home),
            &codex_plugin_roots(codex_home),
        ),
        Err(error) => {
            let mut details = vec![format!("codex hooks/list unavailable: {error}")];
            let roots = codex_plugin_roots(codex_home);
            for root in roots {
                details.push(format!("plugin store populated: {}", root.display()));
            }
            details.push("run `codex app-server` once, then `hcom hooks status`".to_string());
            CodexPluginStatus {
                state: CodexPluginState::Unverified,
                details,
            }
        }
    }
}

fn hcom_trust_entries_from_hook_list(
    entries: &[CodexHookListEntry],
    hooks_path: &Path,
) -> Result<Vec<CodexHookTrustEntry>, String> {
    let expected = expected_hcom_hook_commands();
    let mut trust_entries = Vec::new();
    for entry in entries {
        if !hook_list_entry_is_hcom_owned(entry, &expected, hooks_path) {
            continue;
        }
        // Ownership implies a command was present.
        let command = entry.command.clone().unwrap_or_default();
        let key = entry
            .key
            .clone()
            .ok_or_else(|| format!("hcom hook {command} missing key"))?;
        let current_hash = entry
            .current_hash
            .clone()
            .ok_or_else(|| format!("hcom hook {command} missing currentHash"))?;
        trust_entries.push(CodexHookTrustEntry {
            key,
            command,
            current_hash,
        });
    }

    let found: HashSet<&str> = trust_entries
        .iter()
        .map(|entry| entry.command.as_str())
        .collect();
    let missing: Vec<String> = expected
        .iter()
        .filter(|command| !found.contains(command.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "codex hooks/list missing hcom hooks: {}",
            missing.join(", ")
        ));
    }

    Ok(trust_entries)
}

#[cfg(test)]
fn parse_hcom_hook_entries_from_hooks_list(
    value: &Value,
) -> Result<Vec<CodexHookTrustEntry>, String> {
    let entries = parse_codex_hook_list_entries(value)?;
    hcom_trust_entries_from_hook_list(&entries, &get_codex_hooks_path())
}

/// Synthesize the hooks/list response Codex would return for hcom's own
/// hooks.json, so unit tests exercise the real identity checks without an RPC.
#[cfg(test)]
fn test_hook_list_from_hooks_json(hooks_path: &Path) -> Result<Vec<CodexHookListEntry>, String> {
    let content = std::fs::read_to_string(hooks_path).map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let keys = hcom_hook_state_keys_from_hooks_json(&json, hooks_path);
    let commands = expected_hcom_hook_commands();
    if keys.len() != commands.len() {
        return Err(format!(
            "test hooks.json contained {} hcom hook keys, expected {}",
            keys.len(),
            commands.len()
        ));
    }
    let mut keys: Vec<String> = keys.into_iter().collect();
    keys.sort();
    Ok(keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| CodexHookListEntry {
            command: Some(hcom_command_for_hook_state_key(&key)),
            event_name: Some(hcom_wire_event_for_hook_state_key(&key)),
            plugin_id: None,
            key: Some(key),
            source: Some(CODEX_HOOK_SOURCE_USER.to_string()),
            source_path: Some(hooks_path.to_path_buf()),
            enabled: true,
            trust_status: Some("untrusted".to_string()),
            current_hash: Some(format!("sha256:test-{index}")),
        })
        .collect())
}

fn fetch_codex_hook_list(cwd: &Path, codex_home: &Path) -> Result<Vec<CodexHookListEntry>, String> {
    #[cfg(test)]
    {
        let _ = cwd;
        if let Ok(value) = std::env::var("HCOM_TEST_CODEX_HOOKS_LIST_JSON") {
            if value == "__fail__" {
                return Err("test hook list failure".to_string());
            }
            let json: Value = serde_json::from_str(&value).map_err(|e| e.to_string())?;
            return parse_codex_hook_list_entries(&json);
        }
        test_hook_list_from_hooks_json(&codex_hooks_path_at(codex_home))
    }

    #[cfg(not(test))]
    {
        let mut child = crate::terminal::executable_command("codex")
            .args(["app-server", "--listen", "stdio://"])
            .env("CODEX_HOME", codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to start codex app-server: {e}"))?;

        let stderr_buf = child
            .stderr
            .take()
            .map(spawn_bounded_stderr_reader)
            .unwrap_or_else(|| Arc::new(Mutex::new(String::new())));

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to capture codex app-server stdout".to_string())?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to capture codex app-server stdin".to_string())?;
        let initialize = serde_json::json!({
            "method": "initialize",
            "id": 1,
            "params": {
                "clientInfo": {
                    "name": "hcom",
                    "title": "hcom",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": true }
            }
        });
        writeln!(stdin, "{initialize}").map_err(|e| e.to_string())?;
        read_jsonrpc_response(&rx, 1).map_err(|e| with_app_server_stderr(e, &stderr_buf))?;

        writeln!(
            stdin,
            "{}",
            serde_json::json!({"method":"initialized","params":{}})
        )
        .map_err(|e| e.to_string())?;
        let request = serde_json::json!({
            "method": "hooks/list",
            "id": 2,
            "params": { "cwds": [cwd] }
        });
        writeln!(stdin, "{request}").map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;

        let response =
            read_jsonrpc_response(&rx, 2).map_err(|e| with_app_server_stderr(e, &stderr_buf));
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        parse_codex_hook_list_entries(&response?)
    }
}

fn fetch_codex_hcom_hook_entries(
    cwd: &Path,
    codex_home: &Path,
) -> Result<Vec<CodexHookTrustEntry>, String> {
    let entries = fetch_codex_hook_list(cwd, codex_home)?;
    hcom_trust_entries_from_hook_list(&entries, &codex_hooks_path_at(codex_home))
}

#[cfg(not(test))]
fn spawn_bounded_stderr_reader<R>(mut stderr: R) -> Arc<Mutex<String>>
where
    R: Read + Send + 'static,
{
    let buf = Arc::new(Mutex::new(String::new()));
    let thread_buf = Arc::clone(&buf);
    std::thread::spawn(move || {
        let mut chunk = [0_u8; 1024];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&chunk[..n]);
                    let Ok(mut current) = thread_buf.lock() else {
                        break;
                    };
                    let remaining = CODEX_APP_SERVER_STDERR_LIMIT.saturating_sub(current.len());
                    if remaining == 0 {
                        continue;
                    }
                    for ch in text.chars() {
                        if current.len() + ch.len_utf8() > CODEX_APP_SERVER_STDERR_LIMIT {
                            break;
                        }
                        current.push(ch);
                    }
                }
                Err(_) => break,
            }
        }
    });
    buf
}

#[cfg(not(test))]
fn with_app_server_stderr(mut error: String, stderr_buf: &Arc<Mutex<String>>) -> String {
    let stderr = stderr_buf
        .lock()
        .ok()
        .map(|buf| buf.trim().to_string())
        .unwrap_or_default();
    if !stderr.is_empty() {
        error.push_str("; stderr: ");
        error.push_str(&stderr);
    }
    error
}

#[cfg(not(test))]
fn read_jsonrpc_response(rx: &mpsc::Receiver<String>, id: i64) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + CODEX_APP_SERVER_TIMEOUT;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(format!(
                "timed out waiting for codex app-server response id {id}"
            ));
        }
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(now))
            .map_err(|e| format!("codex app-server closed before response id {id}: {e}"))?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
            if let Some(error) = value.get("error") {
                return Err(format!(
                    "codex app-server returned error for id {id}: {error}"
                ));
            }
            return Ok(value);
        }
    }
}

fn parse_codex_cli_version(output: &str) -> Option<(u64, u64, u64)> {
    output
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|token| {
            let mut parts = token.split('.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch = parts.next()?.parse().ok()?;
            Some((major, minor, patch))
        })
}

fn codex_cli_version_output_for_hook_trust() -> Result<String, String> {
    #[cfg(test)]
    if let Ok(version) = std::env::var("HCOM_TEST_CODEX_CLI_VERSION") {
        return Ok(version);
    }

    #[cfg(not(test))]
    {
        static CACHE: OnceLock<Result<String, String>> = OnceLock::new();
        CACHE
            .get_or_init(|| {
                let output = crate::terminal::executable_command("codex")
                    .arg("--version")
                    .output()
                    .map_err(|e| {
                        format!("could not run codex --version for hook trust check: {e}")
                    })?;
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                Ok(text.trim().to_string())
            })
            .clone()
    }

    #[cfg(test)]
    {
        Err("HCOM_TEST_CODEX_CLI_VERSION not set".to_string())
    }
}

fn codex_hook_trust_version() -> Result<Option<String>, String> {
    let output = codex_cli_version_output_for_hook_trust()?;
    let version = parse_codex_cli_version(&output).ok_or_else(|| {
        format!("could not parse version from codex --version output: {output:?}")
    })?;
    if version >= CODEX_HOOK_TRUST_MIN_VERSION {
        Ok(Some(format!("{}.{}.{}", version.0, version.1, version.2)))
    } else {
        Ok(None)
    }
}

fn codex_hooks_feature_key_for_version(version: (u64, u64, u64)) -> CodexHooksFeatureKey {
    if version >= CODEX_HOOKS_FEATURE_RENAME_VERSION {
        CodexHooksFeatureKey::Hooks
    } else {
        CodexHooksFeatureKey::CodexHooks
    }
}

/// Cached result of `detect_codex_hooks_feature_key`.  Tests bypass the
/// cache when `HCOM_TEST_CODEX_CLI_VERSION` is set so that changing the
/// env var mid-process produces the expected value.
static CODEX_HOOKS_FEATURE_KEY_CACHE: OnceLock<CodexHooksFeatureKey> = OnceLock::new();

fn detect_codex_hooks_feature_key() -> CodexHooksFeatureKey {
    #[cfg(test)]
    if let Ok(version) = std::env::var("HCOM_TEST_CODEX_CLI_VERSION") {
        return parse_codex_cli_version(&version)
            .map(codex_hooks_feature_key_for_version)
            .unwrap_or(CodexHooksFeatureKey::Hooks);
    }

    *CODEX_HOOKS_FEATURE_KEY_CACHE.get_or_init(|| {
        let output = match crate::terminal::executable_command("codex")
            .arg("--version")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                crate::log::log_warn(
                    "hooks",
                    "codex.version_failed",
                    &format!("could not run codex --version: {e}"),
                );
                return CodexHooksFeatureKey::Hooks;
            }
        };
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        match parse_codex_cli_version(&text) {
            Some(version) => codex_hooks_feature_key_for_version(version),
            None => {
                crate::log::log_warn(
                    "hooks",
                    "codex.version_unparseable",
                    "could not parse version from codex --version output",
                );
                CodexHooksFeatureKey::Hooks
            }
        }
    })
}

fn write_hcom_hook_trust_state(
    config_path: &Path,
    hooks_path: &Path,
    entries: &[CodexHookTrustEntry],
    stale_keys: &HashSet<String>,
    codex_cli_version: &str,
    definition_hashes: &HashMap<String, String>,
) -> Result<(), String> {
    // Defense in depth. `hooks.state` lives in the user's global config.toml and
    // each entry written here both trusts a hook and force-enables it, so a key
    // belonging to any other source — a repo's `.codex/hooks.json`, a plugin —
    // must never reach this table, no matter how the entry was identified
    // upstream. Fail loudly instead of silently skipping: a caller that hands
    // over a foreign key has a bug worth surfacing.
    if let Some(foreign) = entries
        .iter()
        .find(|entry| !hook_state_key_belongs_to_hcom_hooks_json(&entry.key, hooks_path))
    {
        return Err(format!(
            "refusing to write Codex hook trust state for '{}' ({}): key does not belong to hcom's own hooks file {}",
            foreign.key,
            foreign.command,
            hooks_path.display()
        ));
    }

    let mut doc: DocumentMut = if config_path.exists() {
        std::fs::read_to_string(config_path)
            .map_err(|e| e.to_string())?
            .parse::<DocumentMut>()
            .unwrap_or_default()
    } else {
        DocumentMut::new()
    };

    if !doc.contains_table("hooks") {
        doc["hooks"] = Item::Table(toml_edit::Table::new());
    }
    if doc["hooks"]
        .get("state")
        .is_none_or(|item| !item.is_table_like())
    {
        doc["hooks"]["state"] = Item::Table(toml_edit::Table::new());
    }
    let state = doc["hooks"]["state"]
        .as_table_like_mut()
        .ok_or_else(|| "hooks.state config section is not a table".to_string())?;

    for key in stale_keys {
        state.remove(key);
    }

    for entry in entries {
        if state
            .get(&entry.key)
            .is_none_or(|item| !item.is_table_like())
        {
            state.insert(&entry.key, Item::Table(toml_edit::Table::new()));
        }
        let Some(item) = state.get_mut(&entry.key) else {
            continue;
        };
        item["trusted_hash"] = value(entry.current_hash.clone());
        item["enabled"] = value(true);
        item[HCOM_CODEX_CLI_VERSION_KEY] = value(codex_cli_version.to_string());
        if let Some(definition_hash) = definition_hashes.get(&entry.key) {
            item[HCOM_HOOK_DEFINITION_HASH_KEY] = value(definition_hash.clone());
        }
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    paths::atomic_write_io(config_path, &doc.to_string()).map_err(|e| e.to_string())
}

/// Rewrite hcom's own `hooks.state` entries from an authoritative hooks/list
/// inventory, so Codex's `currentHash` values land in the trusted hashes.
fn write_hcom_trust_state_from_hook_list(
    hook_list: &[CodexHookListEntry],
    codex_cli_version: &str,
    codex_home: &Path,
) -> Result<(), String> {
    let hooks_path = codex_hooks_path_at(codex_home);
    let entries = hcom_trust_entries_from_hook_list(hook_list, &hooks_path)?;
    let definition_hashes =
        hcom_hook_definition_hashes_from_hooks_path(&hooks_path).map_err(|e| e.to_string())?;
    write_hcom_hook_trust_state(
        &codex_config_path_at(codex_home),
        &hooks_path,
        &entries,
        &HashSet::new(),
        codex_cli_version,
        &definition_hashes,
    )
}

/// Decide, once per launch, what hcom may do about Codex's hook-trust gate for a
/// codex started in `launch_dir`.
///
/// Exact trust state is always preferred; the invocation-wide bypass flag is a
/// last resort and is only permitted when hcom can show that nothing but its own
/// hooks would be unlocked by it.
pub(crate) fn resolve_codex_hook_trust_state_at(
    launch_dir: &Path,
    codex_home: &Path,
) -> CodexHookTrustState {
    let codex_cli_version = match codex_hook_trust_version() {
        // Codex predates the trust gate — nothing is holding hcom's hooks back.
        Ok(None) => return CodexHookTrustState::Trusted,
        Ok(Some(version)) => Some(version),
        // Without a version hcom cannot write valid trust state at all, so treat
        // this exactly like an unavailable hooks/list and decide locally.
        Err(e) => {
            log::log_warn(
                "codex",
                "codex.hook_trust_version_unknown",
                &format!("could not determine Codex version for hook trust: {e}"),
            );
            None
        }
    };

    if let Some(codex_cli_version) = codex_cli_version {
        // This is the launch-time guardrail. Cheap status/verify paths only
        // inspect local metadata, but before opening Codex we ask Codex for
        // authoritative currentHash values and rewrite hcom's trust entries.
        match fetch_codex_hook_list(launch_dir, codex_home) {
            Ok(hook_list) => {
                match write_hcom_trust_state_from_hook_list(
                    &hook_list,
                    &codex_cli_version,
                    codex_home,
                ) {
                    Ok(())
                        if codex_hcom_hooks_trusted_locally_for_version(
                            &codex_cli_version,
                            codex_home,
                        ) =>
                    {
                        return CodexHookTrustState::Trusted;
                    }
                    Ok(()) => log::log_warn(
                        "codex",
                        "codex.hook_trust_self_heal_incomplete",
                        "Codex hook trust self-heal completed but trusted state still looks incomplete",
                    ),
                    Err(e) => log::log_warn(
                        "codex",
                        "codex.hook_trust_self_heal_failed",
                        &format!("Codex hook trust self-heal failed: {e}"),
                    ),
                }

                // Self-heal did not land, but Codex just reported every hook it
                // can see along with its trust status, so the bypass can be
                // judged precisely instead of guessed at.
                let foreign =
                    foreign_hooks_unlocked_by_bypass(&hook_list, &codex_hooks_path_at(codex_home));
                return if foreign.is_empty() {
                    CodexHookTrustState::BypassSafeFromHooksList
                } else {
                    CodexHookTrustState::BypassUnsafe {
                        reason: format!(
                            "Codex reports enabled but untrusted hooks that are not hcom's: {}",
                            foreign.join(", ")
                        ),
                    }
                };
            }
            Err(e) => {
                // hooks/list is how hcom *refreshes* trust state, not how it
                // checks it. When the RPC is unavailable but the state already
                // on disk is exact for this Codex version, hcom's hooks run on
                // their own and nothing is degraded — going blind here would
                // warn the user and weigh up a bypass that is not needed at all.
                // Worth its own step because a flaky or slow app-server is the
                // ordinary failure here, and it must not turn every launch into
                // a false alarm.
                if codex_hcom_hooks_trusted_locally_for_version(&codex_cli_version, codex_home) {
                    log::log_warn(
                        "codex",
                        "codex.hook_list_unavailable_state_exact",
                        &format!(
                            "codex hooks/list unavailable, but hcom's persisted hook trust is already exact; launching unchanged: {e}"
                        ),
                    );
                    return CodexHookTrustState::Trusted;
                }
                log::log_warn(
                    "codex",
                    "codex.hook_list_unavailable",
                    &format!(
                        "codex hooks/list unavailable; falling back to a local hook scan: {e}"
                    ),
                );
            }
        }
    }

    // Blind mode: no authoritative inventory. Only bypass when a purely local
    // scan proves that nothing but hcom's own hooks could be in scope.
    match scan_local_codex_hook_definitions(launch_dir, codex_home) {
        Ok(foreign) if foreign.is_empty() => CodexHookTrustState::BypassSafeFromLocalScan,
        Ok(foreign) => CodexHookTrustState::BypassUnsafe {
            reason: format!(
                "local scan found hook definitions that are not hcom's: {}",
                foreign.join(", ")
            ),
        },
        Err(e) => CodexHookTrustState::BypassUnsafe {
            reason: format!("local hook scan was inconclusive: {e}"),
        },
    }
}

/// Enumerate every Codex hook definition that could be in scope for a launch in
/// `launch_dir` without talking to Codex, and describe each one hcom does not
/// own.
///
/// Covers the three source kinds `--dangerously-bypass-hook-trust` unlocks:
/// - the user layer — `$CODEX_HOME/hooks.json` and a `[hooks]` table in
///   `$CODEX_HOME/config.toml`
/// - project layers — `.codex/hooks.json` and `[hooks]` in `.codex/config.toml`
///   (codex-rs/config/src/loader/mod.rs:1214 `load_project_layers`)
/// - plugins, which hcom cannot resolve into declarations — see
///   `note_possible_plugin_hooks`
///
/// hcom writes exactly one hooks file, so only handlers in that file with a
/// command hcom installs are hcom's; everything found anywhere else is foreign.
/// `Err` means the scan could not be completed and the caller must fail closed.
fn scan_local_codex_hook_definitions(
    launch_dir: &Path,
    codex_home: &Path,
) -> Result<Vec<String>, String> {
    let hcom_hooks_path = codex_hooks_path_at(codex_home);
    let expected = expected_hcom_hook_commands();
    let mut foreign = Vec::new();

    collect_foreign_hooks_from_hooks_json(&hcom_hooks_path, &expected, true, &mut foreign)?;
    let user_config_path = codex_home.join("config.toml");
    let user_config = read_toml_value_if_present(&user_config_path)?;
    if let Some(config) = user_config.as_ref() {
        collect_foreign_hooks_from_config_toml(&user_config_path, config, &expected, &mut foreign)?;
        note_declared_plugins(&user_config_path, config, &mut foreign);
    }
    note_possible_plugin_hooks(codex_home, &mut foreign);

    let markers = codex_project_root_markers(user_config.as_ref())?;
    for dir in codex_project_layer_dirs(launch_dir, &markers)? {
        let dot_codex = dir.join(".codex");
        // Codex skips a project `.codex` that resolves to CODEX_HOME itself
        // (codex-rs/config/src/loader/mod.rs:1256-1259).
        if paths_equivalent(&dot_codex, codex_home) || !dot_codex.is_dir() {
            continue;
        }
        collect_foreign_hooks_from_hooks_json(
            &dot_codex.join("hooks.json"),
            &expected,
            false,
            &mut foreign,
        )?;
        let config_path = dot_codex.join("config.toml");
        if let Some(config) = read_toml_value_if_present(&config_path)? {
            collect_foreign_hooks_from_config_toml(&config_path, &config, &expected, &mut foreign)?;
            note_declared_plugins(&config_path, &config, &mut foreign);
        }
    }

    Ok(foreign)
}

/// Codex's project-root markers for this machine.
///
/// Codex reads `project_root_markers` from the *merged* config
/// (codex-rs/config/src/loader/mod.rs:305-307); hcom can only see the user layer,
/// so a managed layer overriding the key is out of reach. An explicitly empty
/// array disables root detection, which Codex honors.
fn codex_project_root_markers(user_config: Option<&toml::Value>) -> Result<Vec<String>, String> {
    let Some(markers) = user_config.and_then(|config| config.get("project_root_markers")) else {
        return Ok(CODEX_DEFAULT_PROJECT_ROOT_MARKERS
            .iter()
            .map(|marker| (*marker).to_string())
            .collect());
    };
    markers
        .as_array()
        .ok_or_else(|| "project_root_markers in Codex config.toml is not an array".to_string())?
        .iter()
        .map(|marker| {
            marker.as_str().map(str::to_string).ok_or_else(|| {
                "project_root_markers in Codex config.toml is not an array of strings".to_string()
            })
        })
        .collect()
}

/// Directories whose `.codex` folder Codex would load as a project layer: every
/// directory from the project root down to `launch_dir`
/// (codex-rs/config/src/loader/mod.rs:1214-1235). The project root is the nearest
/// ancestor of `launch_dir` holding one of `markers`, or `launch_dir` itself when
/// no marker is found or the marker list is empty
/// (codex-rs/config/src/loader/mod.rs:1154 `find_project_root`).
fn codex_project_layer_dirs(launch_dir: &Path, markers: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::new();
    for ancestor in launch_dir.ancestors() {
        // A `.git` file rather than directory means a linked worktree or a
        // submodule. For linked worktrees Codex reads hook declarations from the
        // *root* checkout's `.codex`, somewhere else on disk entirely
        // (`root_checkout_hooks_folder_for_dir`,
        // codex-rs/config/src/loader/mod.rs:925-935). hcom does not follow that
        // indirection, so the scan cannot claim to be complete.
        if ancestor.join(".git").is_file() {
            return Err(format!(
                "{} is a linked worktree or submodule, so its project hook layer may live in another checkout",
                ancestor.display()
            ));
        }
        dirs.push(ancestor.to_path_buf());
        if markers.is_empty() || markers.iter().any(|marker| ancestor.join(marker).exists()) {
            return Ok(dirs);
        }
    }
    // No marker anywhere up the tree: Codex treats the launch dir as the root.
    Ok(vec![launch_dir.to_path_buf()])
}

fn read_toml_value_if_present(path: &Path) -> Result<Option<toml::Value>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    toml::from_str(&content)
        .map(Some)
        .map_err(|e| format!("could not parse {}: {e}", path.display()))
}

fn collect_foreign_hooks_from_hooks_json(
    path: &Path,
    expected_commands: &HashSet<String>,
    hcom_owns_file: bool,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let json: Value = serde_json::from_str(&content)
        .map_err(|e| format!("could not parse {}: {e}", path.display()))?;
    let Some(events) = json.get("hooks") else {
        return Ok(());
    };
    collect_foreign_hook_events(path, events, expected_commands, hcom_owns_file, out)
}

fn collect_foreign_hooks_from_config_toml(
    path: &Path,
    config: &toml::Value,
    expected_commands: &HashSet<String>,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let Some(events) = config.get("hooks") else {
        return Ok(());
    };
    // A `[hooks]` TOML table has the same shape as the `hooks` object of a
    // hooks.json (both deserialize into `HookEventsToml`,
    // codex-rs/config/src/hook_config.rs:36), so re-encode it and reuse one
    // walker. hcom never writes hook declarations into config.toml, so nothing
    // found here is hcom's.
    let events = serde_json::to_value(events)
        .map_err(|e| format!("could not read [hooks] from {}: {e}", path.display()))?;
    collect_foreign_hook_events(path, &events, expected_commands, false, out)
}

fn collect_foreign_hook_events(
    source: &Path,
    events: &Value,
    expected_commands: &HashSet<String>,
    hcom_owns_file: bool,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let Some(events) = events.as_object() else {
        return Err(format!("hooks in {} is not a table", source.display()));
    };
    for (event, groups) in events {
        // `hooks.state` is trust bookkeeping; only PascalCase event names carry
        // declarations.
        if !CODEX_ALL_HOOK_EVENTS.contains(&event.as_str()) {
            continue;
        }
        let Some(groups) = groups.as_array() else {
            return Err(format!(
                "event '{event}' in {} is not an array of matcher groups",
                source.display()
            ));
        };
        for group in groups {
            let Some(handlers) = group.get("hooks").and_then(|v| v.as_array()) else {
                continue;
            };
            for handler in handlers {
                let command = handler.get("command").and_then(|v| v.as_str());
                // Handlers with no command are Codex's prompt/agent kinds, which
                // hcom cannot inspect — count them as foreign, the fail-closed
                // direction.
                if hcom_owns_file
                    && command.is_some_and(|command| expected_commands.contains(command))
                {
                    continue;
                }
                out.push(format!(
                    "{} in {}",
                    command.unwrap_or("<hook with no command>"),
                    source.display()
                ));
            }
        }
    }
    Ok(())
}

/// A `[plugins]` table in any in-scope layer can activate plugin hook sources.
/// Resolving those into declarations needs each plugin's manifest plus
/// marketplace state (codex-rs/core-plugins/src/loader.rs:199-229), so hcom
/// treats the declaration itself as disqualifying.
fn note_declared_plugins(source: &Path, config: &toml::Value, out: &mut Vec<String>) {
    let declared = config
        .get("plugins")
        .and_then(|plugins| plugins.as_table())
        .is_some_and(|plugins| !plugins.is_empty());
    if declared {
        out.push(format!("[plugins] declared in {}", source.display()));
    }
}

/// Installed plugins can contribute hook sources without appearing in any config
/// file hcom reads, so a non-empty plugin store is disqualifying on its own.
fn note_possible_plugin_hooks(codex_home: &Path, out: &mut Vec<String>) {
    let plugins_root = codex_home.join("plugins");
    let populated = CODEX_PLUGIN_STORE_DIRS.iter().any(|sub| {
        std::fs::read_dir(plugins_root.join(sub)).is_ok_and(|mut entries| entries.next().is_some())
    });
    if populated {
        out.push(format!(
            "installed plugins under {}",
            plugins_root.display()
        ));
    }
}

fn codex_hcom_hooks_trusted_locally_for_version(
    codex_cli_version: &str,
    codex_home: &Path,
) -> bool {
    let hooks_path = codex_hooks_path_at(codex_home);
    let hooks_content = match std::fs::read_to_string(&hooks_path) {
        Ok(content) => content,
        Err(_) => return false,
    };
    let hooks_json: Value = match serde_json::from_str(&hooks_content) {
        Ok(json) => json,
        Err(_) => return false,
    };
    if verify_hooks_json_value(&hooks_json).is_err() {
        return false;
    }
    let entries = hcom_hook_local_entries_from_hooks_json(&hooks_json, &hooks_path);
    if entries.len() != CODEX_HOOK_COMMANDS.len() {
        return false;
    }
    let definition_hashes: HashMap<String, String> = entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.definition_hash.clone()))
        .collect();
    let keys: HashSet<String> = entries.into_iter().map(|entry| entry.key).collect();

    codex_hcom_hook_keys_trusted_for_version(
        &codex_config_path_at(codex_home),
        &keys,
        codex_cli_version,
        &definition_hashes,
    )
}

fn codex_hcom_hook_keys_trusted_for_version(
    config_path: &Path,
    keys: &HashSet<String>,
    codex_cli_version: &str,
    definition_hashes: &HashMap<String, String>,
) -> bool {
    let config_content = match std::fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(_) => return false,
    };
    let doc = match config_content.parse::<DocumentMut>() {
        Ok(doc) => doc,
        Err(_) => return false,
    };
    let Some(state) = doc
        .get("hooks")
        .and_then(|hooks| hooks.get("state"))
        .and_then(|state| state.as_table_like())
    else {
        return false;
    };

    keys.iter().all(|key| {
        let Some(entry) = state.get(key) else {
            return false;
        };
        let Some(trusted_hash) = entry.get("trusted_hash").and_then(|v| v.as_str()) else {
            return false;
        };
        !trusted_hash.is_empty()
            && entry.get("enabled").and_then(|v| v.as_bool()) != Some(false)
            && entry
                .get(HCOM_CODEX_CLI_VERSION_KEY)
                .and_then(|v| v.as_str())
                == Some(codex_cli_version)
            && entry
                .get(HCOM_HOOK_DEFINITION_HASH_KEY)
                .and_then(|v| v.as_str())
                == definition_hashes.get(key).map(String::as_str)
    })
}

#[cfg(test)]
fn hcom_command_for_hook_state_key(key: &str) -> String {
    let mut parts = key.rsplitn(4, ':');
    let _handler_index = parts.next();
    let _group_index = parts.next();
    let event_label = parts.next();
    if let Some(event_label) = event_label {
        for (event, command, _) in CODEX_HOOK_COMMANDS {
            if codex_hook_event_state_label(event) == event_label {
                return build_codex_hook_command(command);
            }
        }
    }
    key.to_string()
}

/// Codex's wire event name for the hook a key names. The key's event segment is
/// snake_case while the `eventName` field is lowerCamelCase, so this translates
/// through the registry instead of reusing the key text.
fn hcom_wire_event_for_hook_state_key(key: &str) -> String {
    let mut parts = key.rsplitn(4, ':');
    let _handler_index = parts.next();
    let _group_index = parts.next();
    let label = parts.next().unwrap_or("unknown");
    CODEX_HOOK_COMMANDS
        .iter()
        .find(|(event, _, _)| codex_hook_event_state_label(event) == label)
        .map(|(event, _, _)| codex_hook_event_wire_name(event))
        .unwrap_or_else(|| label.to_string())
}

fn verify_hcom_hook_keys_trusted_for_version(
    config_path: &Path,
    entries: &[CodexHookLocalEntry],
    codex_cli_version: &str,
) -> Result<(), VerifyFailReason> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| VerifyFailReason::HookTrustUnavailable(e.to_string()))?;
    let doc = content
        .parse::<DocumentMut>()
        .map_err(|e| VerifyFailReason::HookTrustUnavailable(e.to_string()))?;
    let state = doc
        .get("hooks")
        .and_then(|hooks| hooks.get("state"))
        .and_then(|state| state.as_table_like())
        .ok_or_else(|| VerifyFailReason::HookTrustUnavailable("hooks.state missing".to_string()))?;

    for entry in entries {
        let command = entry.command.clone();
        let Some(state_entry) = state.get(&entry.key) else {
            return Err(VerifyFailReason::HookTrustMissing { command });
        };
        if state_entry.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            return Err(VerifyFailReason::HookDisabled { command });
        }
        let trusted_hash = state_entry
            .get("trusted_hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| VerifyFailReason::HookTrustMissing {
                command: command.clone(),
            })?;
        if trusted_hash.is_empty() {
            return Err(VerifyFailReason::HookTrustMissing { command });
        }
        if state_entry
            .get(HCOM_CODEX_CLI_VERSION_KEY)
            .and_then(|v| v.as_str())
            != Some(codex_cli_version)
        {
            return Err(VerifyFailReason::HookTrustStale { command });
        }
        if state_entry
            .get(HCOM_HOOK_DEFINITION_HASH_KEY)
            .and_then(|v| v.as_str())
            != Some(entry.definition_hash.as_str())
        {
            return Err(VerifyFailReason::HookTrustStale { command });
        }
    }

    Ok(())
}

fn verify_hcom_hook_trust_state(
    config_path: &Path,
    hooks_path: &Path,
) -> Result<(), VerifyFailReason> {
    let Some(codex_cli_version) =
        codex_hook_trust_version().map_err(VerifyFailReason::CodexUnavailable)?
    else {
        return Ok(());
    };
    let entries = hcom_hook_local_entries_from_hooks_path(hooks_path)?;
    if entries.len() != CODEX_HOOK_COMMANDS.len() {
        return Err(VerifyFailReason::HookTrustUnavailable(format!(
            "could not derive all hcom hook trust keys from {}",
            hooks_path.display()
        )));
    }

    verify_hcom_hook_keys_trusted_for_version(config_path, &entries, &codex_cli_version)
}

fn ensure_codex_feature_enabled(
    config_path: &Path,
    feature_key: CodexHooksFeatureKey,
) -> Result<(), String> {
    let mut doc: DocumentMut = if config_path.exists() {
        std::fs::read_to_string(config_path)
            .map_err(|e| e.to_string())?
            .parse::<DocumentMut>()
            .unwrap_or_default()
    } else {
        DocumentMut::new()
    };

    if !doc.contains_table("features") {
        doc["features"] = Item::Table(toml_edit::Table::new());
    }
    // Codex renamed the feature flag from codex_hooks to hooks in 0.129.0.
    // Always clean the deprecated codex_hooks key if present; never remove
    // hooks — it's the shared flag for all Codex hooks, not just hcom's.
    remove_codex_hooks_aliases(&mut doc, feature_key);
    doc["features"][feature_key.as_str()] = value(true);
    // Remove the old hcom-owned codex-notify form only; leave unrelated notify untouched.
    let is_hcom_notify = doc.get("notify").is_some_and(is_hcom_legacy_notify);
    if is_hcom_notify {
        doc.remove("notify");
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if paths::atomic_write(config_path, &doc.to_string()) {
        Ok(())
    } else {
        Err("atomic_write failed".to_string())
    }
}

fn remove_codex_hooks_aliases(doc: &mut DocumentMut, feature_key: CodexHooksFeatureKey) {
    if let Some(features) = doc.get_mut("features")
        && let Some(table) = features.as_table_like_mut()
    {
        table.remove("codex_hooks");
    }

    if feature_key != CodexHooksFeatureKey::Hooks {
        return;
    }

    let Some(profiles) = doc
        .get_mut("profiles")
        .and_then(|item| item.as_table_like_mut())
    else {
        return;
    };
    for (_, profile) in profiles.iter_mut() {
        let Some(features) = profile
            .as_table_like_mut()
            .and_then(|profile| profile.get_mut("features"))
        else {
            continue;
        };
        if let Some(table) = features.as_table_like_mut() {
            table.remove("codex_hooks");
        }
    }
}

fn codex_selected_feature_enabled(config_path: &Path, feature_key: CodexHooksFeatureKey) -> bool {
    let Ok(content) = std::fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return false;
    };
    doc.get("features")
        .and_then(|item| item.get(feature_key.as_str()))
        .and_then(|item| item.as_bool())
        .unwrap_or(false)
}

fn codex_deprecated_feature_present(config_path: &Path, feature_key: CodexHooksFeatureKey) -> bool {
    if feature_key != CodexHooksFeatureKey::Hooks {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return false;
    };
    if doc
        .get("features")
        .and_then(|item| item.get("codex_hooks"))
        .is_some()
    {
        return true;
    }

    let Some(active_profile) = doc.get("profile").and_then(|item| item.as_str()) else {
        return false;
    };
    doc.get("profiles")
        .and_then(|item| item.as_table_like())
        .and_then(|profiles| profiles.get(active_profile))
        .and_then(|profile| profile.get("features"))
        .and_then(|features| features.get("codex_hooks"))
        .is_some()
}

fn codex_feature_enabled(config_path: &Path, feature_key: CodexHooksFeatureKey) -> bool {
    if codex_selected_feature_enabled(config_path, feature_key) {
        return true;
    }

    let Ok(content) = std::fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return false;
    };
    // Check the version-selected key first, fall back to the alternate
    // so that a config written by an older (or newer) hcom still passes
    // verification until the next setup call canonicalizes it.
    doc.get("features")
        .and_then(|item| item.get(feature_key.alternate()))
        .and_then(|item| item.as_bool())
        .unwrap_or(false)
}

/// Whether Codex config already uses the feature flag key expected by the
/// installed Codex CLI. Verification accepts either key for compatibility, but
/// launch setup uses this to self-heal stale `codex_hooks` configs. Modern
/// Codex warns if the deprecated key is present at all, even when `hooks` is
/// also enabled, so treat that mixed state as not current.
pub(crate) fn codex_current_feature_enabled() -> bool {
    codex_current_feature_enabled_at(&codex_config_dir())
}

pub(crate) fn codex_current_feature_enabled_at(codex_home: &Path) -> bool {
    let config_path = codex_config_path_at(codex_home);
    let feature_key = detect_codex_hooks_feature_key();
    codex_selected_feature_enabled(&config_path, feature_key)
        && !codex_deprecated_feature_present(&config_path, feature_key)
}

fn verify_hooks_json_at(hooks_path: &Path) -> Result<(), VerifyFailReason> {
    let content = std::fs::read_to_string(hooks_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            VerifyFailReason::HooksPathMissing(hooks_path.to_path_buf())
        }
        _ => VerifyFailReason::HooksUnreadable(hooks_path.to_path_buf()),
    })?;
    let json: Value = serde_json::from_str(&content)
        .map_err(|_| VerifyFailReason::HooksUnreadable(hooks_path.to_path_buf()))?;
    verify_hooks_json_value(&json)
}

fn verify_hooks_json_value(json: &Value) -> Result<(), VerifyFailReason> {
    let hooks_obj = json
        .get("hooks")
        .and_then(|v| v.as_object())
        .ok_or(VerifyFailReason::HooksKeyMissing)?;

    // Check all expected hooks are present with correct matchers.
    for (event, command, matcher) in CODEX_HOOK_COMMANDS {
        let groups = match hooks_obj.get(*event).and_then(|v| v.as_array()) {
            Some(arr) if !arr.is_empty() => arr,
            _ => {
                return Err(VerifyFailReason::HookEventMissing {
                    event: (*event).to_string(),
                });
            }
        };
        let expected_command = build_codex_hook_command(command);
        let expected_hook = serde_json::json!({
            "type": "command",
            "command": expected_command,
        });
        // Mirror merge_hcom_hooks: for None-matcher events only match groups
        // with no "matcher" key, not groups with "matcher":"" (which may belong
        // to other tools such as context-mode).
        let matching_group = groups.iter().find(|group| match matcher {
            Some(expected) => group.get("matcher").and_then(|v| v.as_str()) == Some(*expected),
            None => group.get("matcher").and_then(|v| v.as_str()).is_none(),
        });
        let Some(group) = matching_group else {
            return Err(VerifyFailReason::HookCommandMissing {
                event: (*event).to_string(),
                expected_command,
            });
        };
        let hooks = group
            .get("hooks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| VerifyFailReason::HookCommandMissing {
                event: (*event).to_string(),
                expected_command: expected_command.clone(),
            })?;
        let hcom_hooks: Vec<&Value> = hooks
            .iter()
            .filter(|hook| {
                hook.get("command")
                    .and_then(|v| v.as_str())
                    .is_some_and(is_hcom_codex_command)
            })
            .collect();
        if !hcom_hooks.iter().any(|hook| **hook == expected_hook) {
            return Err(VerifyFailReason::HookCommandMissing {
                event: (*event).to_string(),
                expected_command,
            });
        }
        if hcom_hooks.iter().any(|hook| **hook != expected_hook) {
            return Err(VerifyFailReason::HookDefinitionChanged {
                event: (*event).to_string(),
                expected_command,
            });
        }
    }

    // Check no stale hcom hooks exist in groups with non-matching matchers.
    for (event, groups) in hooks_obj {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let has_hcom_command =
                group
                    .get("hooks")
                    .and_then(|v| v.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|v| v.as_str())
                                .is_some_and(is_hcom_codex_command)
                        })
                    });
            if !has_hcom_command {
                continue;
            }
            // This group has an hcom command — it must match an expected entry.
            let group_matcher = group.get("matcher").and_then(|v| v.as_str());
            let is_expected = CODEX_HOOK_COMMANDS
                .iter()
                .any(|(exp_event, _, exp_matcher)| {
                    *exp_event == event.as_str()
                        && match exp_matcher {
                            Some(m) => group_matcher == Some(*m),
                            None => group_matcher.is_none(),
                        }
                });
            if !is_expected {
                return Err(VerifyFailReason::StaleHcomHookEntry {
                    event: event.clone(),
                    matcher: group_matcher.map(|s| s.to_string()),
                });
            }
        }
    }

    Ok(())
}

fn build_codex_rules() -> String {
    let prefix = crate::runtime_env::get_hcom_prefix();
    let prefix_parts: String = prefix
        .iter()
        .map(|p| format!("\"{}\"", p))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rules = vec!["# hcom integration - auto-approve safe commands".to_string()];
    for cmd in SAFE_HCOM_COMMANDS {
        rules.push(format!(
            "prefix_rule(pattern=[{}, \"{}\"], decision=\"allow\")",
            prefix_parts, cmd
        ));
    }
    for tool in HCOM_TOOL_NAMES {
        rules.push(format!(
            "prefix_rule(pattern=[{}, \"{}\", \"--help\"], decision=\"allow\")",
            prefix_parts, tool
        ));
        rules.push(format!(
            "prefix_rule(pattern=[{}, \"{}\", \"-h\"], decision=\"allow\")",
            prefix_parts, tool
        ));
    }
    rules.join("\n") + "\n"
}

/// Set up Codex execpolicy rules for auto-approval.
pub fn setup_codex_execpolicy() -> bool {
    setup_codex_execpolicy_at(&codex_config_dir())
}

fn setup_codex_execpolicy_at(codex_home: &Path) -> bool {
    let rules_dir = codex_rules_path_at(codex_home);
    let rules_file = rules_dir.join("hcom.rules");
    let rule_content = build_codex_rules();

    if rules_file.exists()
        && std::fs::read_to_string(&rules_file).ok().as_deref() == Some(rule_content.as_str())
    {
        return true;
    }

    let _ = std::fs::create_dir_all(&rules_dir);
    paths::atomic_write(&rules_file, &rule_content)
}

/// Remove hcom execpolicy rule.
pub fn remove_codex_execpolicy() -> bool {
    remove_codex_execpolicy_at(&codex_config_dir())
}

fn remove_codex_execpolicy_at(codex_home: &Path) -> bool {
    let rules_file = codex_rules_path_at(codex_home).join("hcom.rules");
    if rules_file.exists() {
        std::fs::remove_file(&rules_file).is_ok()
    } else {
        true
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum VerifyFailReason {
    #[error("Codex config.toml missing: {}", .0.display())]
    ConfigPathMissing(PathBuf),
    #[error("Codex hooks.json missing: {}", .0.display())]
    HooksPathMissing(PathBuf),
    #[error("Codex experimental hooks feature not enabled in {}", .0.display())]
    CodexFeatureDisabled(PathBuf),
    #[error("Codex hooks.json missing or not parseable as JSON: {}", .0.display())]
    HooksUnreadable(PathBuf),
    #[error("'hooks' key missing or not an object")]
    HooksKeyMissing,
    #[error("hook event '{event}' missing or empty")]
    HookEventMissing { event: String },
    #[error("hcom hook command not found under event '{event}' (expected: {expected_command})")]
    HookCommandMissing {
        event: String,
        expected_command: String,
    },
    #[error("hcom hook definition changed under event '{event}' (expected: {expected_command})")]
    HookDefinitionChanged {
        event: String,
        expected_command: String,
    },
    #[error("stale hcom hook entry in event '{event}' under unexpected matcher: {matcher:?}")]
    StaleHcomHookEntry {
        event: String,
        matcher: Option<String>,
    },
    #[error("Codex CLI unavailable for hook trust check: {0}")]
    CodexUnavailable(String),
    #[error("hcom Codex hook trust state unavailable: {0}")]
    HookTrustUnavailable(String),
    #[error("hcom Codex hook '{command}' has no trusted_hash in hooks.state")]
    HookTrustMissing { command: String },
    #[error("hcom Codex hook '{command}' trusted_hash is stale")]
    HookTrustStale { command: String },
    #[error("hcom Codex hook '{command}' is disabled in hooks.state")]
    HookDisabled { command: String },
    #[error("hcom.rules file missing: {}", .0.display())]
    PermissionsRulesMissing(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("failed to enable Codex experimental hooks feature in {}: {reason}", path.display())]
    EnsureFeatureFailed { path: PathBuf, reason: String },
    #[error("failed to read existing {}: {source}", path.display())]
    HooksReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON serialization failed: {0}")]
    SerializationFailed(#[from] serde_json::Error),
    #[error("failed to create parent dir {}: {source}", path.display())]
    DirCreateFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
    #[error(
        "failed to trust Codex hooks: {reason}. hcom-wrapped Codex launches may fall back to --dangerously-bypass-hook-trust, but vanilla Codex will not run hcom hooks until trust succeeds"
    )]
    HookTrustFailed { reason: String },
}

pub fn try_setup_codex_hooks(include_permissions: bool) -> Result<(), SetupError> {
    try_setup_codex_hooks_at(include_permissions, &codex_config_dir())
}

pub(crate) fn try_setup_codex_hooks_at(
    include_permissions: bool,
    codex_home: &Path,
) -> Result<(), SetupError> {
    let config_path = codex_config_path_at(codex_home);
    let hooks_path = codex_hooks_path_at(codex_home);
    let feature_key = detect_codex_hooks_feature_key();

    ensure_codex_feature_enabled(&config_path, feature_key).map_err(|e| {
        SetupError::EnsureFeatureFailed {
            path: config_path.clone(),
            reason: e,
        }
    })?;

    let mut hooks_json = if hooks_path.exists() {
        let content =
            std::fs::read_to_string(&hooks_path).map_err(|source| SetupError::HooksReadFailed {
                path: hooks_path.clone(),
                source,
            })?;
        serde_json::from_str::<Value>(&content)
            .unwrap_or_else(|_| serde_json::json!({ "hooks": {} }))
    } else {
        serde_json::json!({ "hooks": {} })
    };
    // Strip legacy "cmd"-keyed hcom entries written by pre-0.129 installs.
    // Only safe once Codex supports the current "command"-keyed format.
    if feature_key == CodexHooksFeatureKey::Hooks {
        remove_legacy_hcom_cmd_hooks_from_json(&mut hooks_json);
    }
    let old_hcom_hook_keys = hcom_hook_state_keys_from_hooks_json(&hooks_json, &hooks_path);
    merge_hcom_hooks(&mut hooks_json);

    if let Some(parent) = hooks_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SetupError::DirCreateFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let content =
        serde_json::to_string_pretty(&hooks_json).map_err(SetupError::SerializationFailed)?;
    paths::atomic_write_io(&hooks_path, &content).map_err(|source| {
        SetupError::AtomicWriteFailed {
            path: hooks_path.clone(),
            source,
        }
    })?;

    verify_hooks_json_at(&hooks_path).map_err(|reason| SetupError::PostWriteVerifyFailed {
        path: hooks_path.clone(),
        reason,
    })?;

    match codex_hook_trust_version() {
        Ok(Some(codex_cli_version)) => {
            let definition_hashes =
                hcom_hook_definition_hashes_from_hooks_json(&hooks_json, &hooks_path);
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match fetch_codex_hcom_hook_entries(&cwd, codex_home).and_then(|entries| {
                let current_keys: HashSet<String> =
                    entries.iter().map(|entry| entry.key.clone()).collect();
                let stale_keys: HashSet<String> = old_hcom_hook_keys
                    .difference(&current_keys)
                    .cloned()
                    .collect();
                write_hcom_hook_trust_state(
                    &config_path,
                    &hooks_path,
                    &entries,
                    &stale_keys,
                    &codex_cli_version,
                    &definition_hashes,
                )
            }) {
                Ok(()) => {}
                Err(e) => return Err(SetupError::HookTrustFailed { reason: e }),
            }
        }
        Ok(None) => {}
        Err(e) => log::log_warn(
            "hooks",
            "codex.hook_trust_version_warn",
            &format!(
                "hooks installed but Codex version check failed; launch may fall back to Codex hook-trust bypass: {e}"
            ),
        ),
    }

    let ep_ok = if include_permissions {
        setup_codex_execpolicy_at(codex_home)
    } else {
        remove_codex_execpolicy_at(codex_home)
    };
    if !ep_ok {
        log::log_warn(
            "hooks",
            "codex.execpolicy_warn",
            "hooks installed but execpolicy write failed; auto-approval will not work",
        );
    }
    Ok(())
}

pub fn setup_codex_hooks(include_permissions: bool) -> bool {
    try_setup_codex_hooks(include_permissions).is_ok()
}

pub fn verify_codex_hooks_installed(check_permissions: bool) -> bool {
    verify_codex_hooks_installed_at(check_permissions, &codex_config_dir())
}

pub(crate) fn verify_codex_hooks_installed_at(check_permissions: bool, codex_home: &Path) -> bool {
    verify_codex_hooks_inner_at(check_permissions, codex_home).is_ok()
}

fn verify_codex_hooks_inner_at(
    check_permissions: bool,
    codex_home: &Path,
) -> Result<(), VerifyFailReason> {
    let config_path = codex_config_path_at(codex_home);
    let hooks_path = codex_hooks_path_at(codex_home);

    if !config_path.exists() {
        return Err(VerifyFailReason::ConfigPathMissing(config_path));
    }
    let feature_key = detect_codex_hooks_feature_key();
    if !codex_feature_enabled(&config_path, feature_key) {
        return Err(VerifyFailReason::CodexFeatureDisabled(config_path));
    }
    // No exists() pre-check: verify_hooks_json_at converts NotFound to
    // HooksPathMissing, avoiding a stat-then-open race.
    verify_hooks_json_at(&hooks_path)?;
    verify_hcom_hook_trust_state(&config_path, &hooks_path)?;
    if check_permissions {
        let rules_file = codex_rules_path_at(codex_home).join("hcom.rules");
        if !rules_file.exists() {
            return Err(VerifyFailReason::PermissionsRulesMissing(rules_file));
        }
    }
    Ok(())
}

/// Remove hcom hooks from a single Codex hooks.json + execpolicy at the given base dir.
fn remove_codex_hooks_from_dir(base: &std::path::Path) -> bool {
    let hooks_path = base.join("hooks.json");
    let rules_file = base.join("rules").join("hcom.rules");
    let mut ok = true;

    if hooks_path.exists() {
        match std::fs::read_to_string(&hooks_path) {
            Ok(content) => {
                let mut json = serde_json::from_str::<Value>(&content)
                    .unwrap_or_else(|_| serde_json::json!({ "hooks": {} }));
                remove_hcom_hooks_from_json(&mut json);
                if json.get("hooks").is_none() && json.as_object().is_some_and(|o| o.is_empty()) {
                    ok &= std::fs::remove_file(&hooks_path).is_ok();
                } else {
                    let content =
                        serde_json::to_string_pretty(&json).unwrap_or_else(|_| "{}".into());
                    ok &= paths::atomic_write(&hooks_path, &content);
                }
            }
            Err(_) => ok = false,
        }
    }

    if rules_file.exists() {
        ok &= std::fs::remove_file(&rules_file).is_ok();
    }

    ok
}

/// Remove hcom hooks from Codex config.
///
/// Cleans the default (~/.codex), env-var (CODEX_HOME), and active HCOM_DIR-local paths.
pub fn remove_codex_hooks() -> bool {
    let default_dir = dirs::home_dir()
        .map(|h| h.join(".codex"))
        .unwrap_or_default();
    let env_dir = std::env::var("CODEX_HOME")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    let local_dir = codex_config_dir();

    let default_ok = remove_codex_hooks_from_dir(&default_dir);
    let env_ok = match env_dir {
        Some(ref d) if *d != default_dir => remove_codex_hooks_from_dir(d),
        _ => true,
    };
    let local_ok = if local_dir != default_dir && Some(&local_dir) != env_dir.as_ref() {
        remove_codex_hooks_from_dir(&local_dir)
    } else {
        true
    };

    default_ok && env_ok && local_ok
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
