//! Instance lifecycle state machine and launch failure handling.

use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use crate::db::{HcomDb, InstanceRow};
use crate::shared::time::{now_epoch_f64, now_epoch_i64};
use crate::shared::{ST_ACTIVE, ST_BLOCKED, ST_INACTIVE, ST_LAUNCHING, ST_LISTENING};

/// Parameters for `set_status` beyond the core name/status/context triplet.
#[derive(Debug, Default)]
pub struct StatusUpdate<'a> {
    pub detail: &'a str,
    pub msg_ts: &'a str,
    /// Tool-reported name of the tool call active when this write happened
    /// (e.g. "Bash", "Edit"). Empty when not applicable/available.
    pub tool_name: &'a str,
    /// Tool-reported id of the tool call active when this write happened
    /// (Claude's `tool_use_id`). Empty when not applicable/available.
    pub tool_use_id: &'a str,
}

/// Max time between instance creation and session binding before launch is considered failed.
pub const LAUNCH_PLACEHOLDER_TIMEOUT: i64 = 30;

/// Heartbeat timeout with active TCP listener (PTY, hooks with notify).
/// 35s = 30s hook polling interval + 5s buffer.
pub const HEARTBEAT_THRESHOLD_TCP: i64 = 35;

/// Heartbeat timeout without TCP listener (adhoc instances).
pub const HEARTBEAT_THRESHOLD_NO_TCP: i64 = 10;

/// Heartbeat slack for instances parked in a non-listening status (2 min).
///
/// `status_time` only advances on hook traffic, so an instance left `active`
/// across a long tool call — or a system sleep — can carry an hours-old status
/// while its delivery loop is healthy; there the heartbeat is the only real
/// liveness signal. [`HEARTBEAT_THRESHOLD_TCP`] is sized for the *listening*
/// path and leaves only 5s over the 30s poll, which a scheduler hiccup on wake
/// eats easily. 4x the poll interval instead, so a couple of missed polls can't
/// fake death.
pub const ACTIVE_HEARTBEAT_GRACE: i64 = 120;

/// Heartbeat age when last_stop is missing (marker for unreliable data).
pub const UNKNOWN_HEARTBEAT_AGE: i64 = 999999;

/// Max time without status update before marking inactive (5 min).
pub const STATUS_ACTIVITY_TIMEOUT: i64 = 300;

/// How long placeholder instances can exist before cleanup (2 min).
pub const CLEANUP_PLACEHOLDER_THRESHOLD: i64 = 120;

/// Grace period after sleep/wake before resuming stale cleanup (60s).
pub const WAKE_GRACE_PERIOD: f64 = 60.0;

/// Remote device stale threshold (90s without push).
const REMOTE_DEVICE_STALE_THRESHOLD: f64 = 90.0;

/// Window for showing recently stopped instances (10 minutes).
pub const RECENTLY_STOPPED_WINDOW: f64 = 600.0;

/// Return type for `get_instance_status()` with structured status metadata.
#[derive(Debug, Clone)]
pub struct ComputedStatus {
    pub status: String,
    pub age_string: String,
    pub description: String,
    pub age_seconds: i64,
    /// Simple context key (e.g., "stale", "killed", "timeout").
    pub context: String,
}

pub use crate::shared::time::format_age;

// Tracks wall-clock vs monotonic-clock drift to detect system sleep.
// On macOS, Instant (mach_absolute_time) does not advance during sleep,
// but SystemTime (gettimeofday) does. Large drift means the system just woke.
struct WakeState {
    last_mono: Option<Instant>,
    last_wall: f64,
    grace_until_mono: Option<Instant>,
}

static WAKE_STATE: Mutex<WakeState> = Mutex::new(WakeState {
    last_mono: None,
    last_wall: 0.0,
    grace_until_mono: None,
});

/// Clear the process-local wake state so a test can drive the first-call path.
#[cfg(test)]
fn reset_wake_state_for_test() {
    if let Ok(mut state) = WAKE_STATE.lock() {
        state.last_mono = None;
        state.last_wall = 0.0;
        state.grace_until_mono = None;
    }
}

/// Detect sleep/wake via wall-vs-monotonic drift and report whether grace is active.
///
/// Process-local: the drift comparison needs an earlier sample taken by *this*
/// process, so a one-shot CLI can never detect a wake this way — its first call
/// only seeds the state and reports false. Short-lived callers want
/// [`is_in_wake_grace_shared`] instead.
pub fn is_in_wake_grace() -> bool {
    wake_grace(None, false)
}

/// Wake-grace check for short-lived processes.
///
/// Reads the window published by the long-lived delivery loops
/// (`_wake_grace_until`), falling back to a gap in their liveness beacon
/// (`_wake_last_wall`) for the sub-second race where a CLI runs after the wake
/// but before any loop has noticed it.
///
/// Never publishes the beacon. A one-shot that wrote `_wake_last_wall` would
/// make every infrequent invocation look like a wake to the next one, and
/// cleanup would grace itself into never running. The one write it does make is
/// `_wake_beacon_armed`, recording that a given beacon value has already been
/// graced so a frozen beacon cannot suppress cleanup indefinitely.
pub fn is_in_wake_grace_shared(db: &crate::db::HcomDb) -> bool {
    wake_grace(Some(db), false)
}

/// Wake-grace check for long-lived loops, which also publishes the shared state
/// that [`is_in_wake_grace_shared`] reads.
///
/// Call it every poll: the beacon write is what tells short-lived processes that
/// a loop is running and up to date, and the drift branch is what arms the grace
/// window for them the instant this process observes a wake.
pub fn is_in_wake_grace_publishing(db: &crate::db::HcomDb) -> bool {
    wake_grace(Some(db), true)
}

fn wake_grace(db: Option<&crate::db::HcomDb>, publish: bool) -> bool {
    let now_mono = Instant::now();
    let now_wall = now_epoch_f64();

    let mut state = match WAKE_STATE.lock() {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Only the read-only callers consult the shared state, and they consult it
    // on every call: they are one-shots that ask once or twice per process, and
    // keying this off "first call in this process" made the answer depend on
    // whoever happened to touch WAKE_STATE first. Publishing callers are
    // long-lived loops that detect drift from their own samples.
    if !publish && let Some(db) = db {
        let mut extend_grace = |deadline: Instant| {
            if state
                .grace_until_mono
                .is_none_or(|existing| deadline > existing)
            {
                state.grace_until_mono = Some(deadline);
            }
        };

        // Beacon gap: the backstop for the race where a one-shot runs after a
        // wake but before any loop has republished. A gap means either the
        // machine was asleep or no loop is running, and one reading cannot tell
        // those apart — so arm at most once per distinct beacon value. A live
        // loop advances the beacon within a poll, closing the gap on its own; a
        // frozen beacon (last loop exited, stale value left behind) therefore
        // grants exactly one grace, then never again.
        //
        // No upper bound on the gap. Being spent-once is what keeps a frozen
        // beacon from suppressing cleanup, so capping the age would only punch
        // a hole in the protection at the sleeps most likely to happen —
        // overnight ones, where the gap is hours and the wake is real.
        if let Ok(Some(persisted_wall)) = db.kv_get("_wake_last_wall")
            && let Ok(last_wall) = persisted_wall.parse::<f64>()
        {
            let wall_elapsed = now_wall - last_wall;
            let already_armed = db
                .kv_get("_wake_beacon_armed")
                .ok()
                .flatten()
                .is_some_and(|armed| armed == persisted_wall);
            if wall_elapsed > 30.0 && !already_armed {
                crate::log::log_info(
                    "cleanup",
                    "sleep_wake_detected",
                    &format!(
                        "drift={:.0}s (cross-process), grace={:.0}s",
                        wall_elapsed, WAKE_GRACE_PERIOD
                    ),
                );
                // Marks this beacon value as spent. Not a beacon write: it
                // never makes a later invocation read a wake that did not
                // happen, which is the reason one-shots must not publish
                // `_wake_last_wall` itself.
                let _ = db.kv_set("_wake_beacon_armed", Some(&persisted_wall));
                extend_grace(now_mono + std::time::Duration::from_secs_f64(WAKE_GRACE_PERIOD));
            }
        }

        // An explicit window from a loop that already saw the wake. Read
        // independently of the beacon: a window that was published must still
        // be honored when the beacon is missing (fresh db, after `hcom reset`),
        // and it must never shorten a grace the beacon already granted.
        if let Ok(Some(grace_until)) = db.kv_get("_wake_grace_until")
            && let Ok(grace_wall) = grace_until.parse::<f64>()
            && now_wall < grace_wall
        {
            let remaining = grace_wall - now_wall;
            extend_grace(now_mono + std::time::Duration::from_secs_f64(remaining));
        }
    }

    if let Some(last_mono) = state.last_mono {
        let mono_elapsed = now_mono.duration_since(last_mono).as_secs_f64();
        let wall_elapsed = now_wall - state.last_wall;
        let drift = wall_elapsed - mono_elapsed;

        if drift > 30.0 {
            crate::log::log_info(
                "cleanup",
                "sleep_wake_detected",
                &format!("drift={:.0}s, grace={:.0}s", drift, WAKE_GRACE_PERIOD),
            );
            let grace_deadline = now_mono + std::time::Duration::from_secs_f64(WAKE_GRACE_PERIOD);
            state.grace_until_mono = Some(grace_deadline);

            if publish && let Some(db) = db {
                let grace_wall = now_wall + WAKE_GRACE_PERIOD;
                let _ = db.kv_set("_wake_grace_until", Some(&grace_wall.to_string()));
            }
        }
    }

    state.last_mono = Some(now_mono);
    state.last_wall = now_wall;

    if publish && let Some(db) = db {
        let _ = db.kv_set("_wake_last_wall", Some(&now_wall.to_string()));
    }

    match state.grace_until_mono {
        Some(deadline) => now_mono < deadline,
        None => false,
    }
}

/// Compute the current status from stored fields and heartbeat.
pub fn get_instance_status(data: &InstanceRow, db: &HcomDb) -> ComputedStatus {
    let status = &data.status;
    let status_time = data.status_time;
    let status_context = &data.status_context;
    let wake_grace = is_in_wake_grace();
    let now = now_epoch_i64();

    if status_context == "new" && (status == ST_INACTIVE || status == "pending") {
        let created_at = data.created_at as i64;
        let age = if created_at > 0 { now - created_at } else { 0 };
        if age < LAUNCH_PLACEHOLDER_TIMEOUT {
            return ComputedStatus {
                status: ST_LAUNCHING.to_string(),
                age_string: if age > 0 {
                    format_age(age)
                } else {
                    String::new()
                },
                description: "launching".to_string(),
                age_seconds: age,
                context: "new".to_string(),
            };
        }

        let detail = get_or_finalize_launch_failure_detail(db, data)
            .or_else(|| extract_launch_failure_detail(data))
            .unwrap_or_else(|| "launch probably failed - check logs or hcom list -v".to_string());
        return ComputedStatus {
            status: ST_INACTIVE.to_string(),
            age_string: format_age(age),
            description: detail,
            age_seconds: age,
            context: "launch_failed".to_string(),
        };
    }

    let mut current_status = status.to_string();
    let mut current_context = status_context.to_string();
    let mut age = if status_time > 0 {
        now - status_time
    } else {
        0
    };
    if status_time == 0 {
        let created_at = data.created_at as i64;
        if created_at > 0 {
            age = now - created_at;
        }
    }

    if current_status == ST_LISTENING {
        let last_stop = data.last_stop;
        let is_remote = data.origin_device_id.is_some();

        if is_remote {
            age = 0;
        } else {
            let heartbeat_age = if last_stop > 0 {
                now - last_stop
            } else if status_time > 0 {
                now - status_time
            } else {
                UNKNOWN_HEARTBEAT_AGE
            };

            let has_tcp = data.tcp_mode != 0 || db.has_notify_endpoint(&data.name);
            let threshold = if has_tcp {
                HEARTBEAT_THRESHOLD_TCP
            } else {
                HEARTBEAT_THRESHOLD_NO_TCP
            };

            if heartbeat_age > threshold {
                if wake_grace {
                    age = 0;
                } else {
                    current_status = ST_INACTIVE.to_string();
                    current_context = "stale:listening".to_string();
                    age = heartbeat_age;
                }
            } else {
                age = 0;
            }
        }
    } else if current_status != ST_INACTIVE {
        let status_age = if status_time > 0 {
            now - status_time
        } else {
            let created_at = data.created_at as i64;
            if created_at > 0 { now - created_at } else { 0 }
        };

        if status_age > STATUS_ACTIVITY_TIMEOUT && data.origin_device_id.is_none() {
            let last_stop = data.last_stop;
            if last_stop > 0 && (now - last_stop) < ACTIVE_HEARTBEAT_GRACE {
                // Fresh heartbeat means the process is alive even if the status is old.
            } else if wake_grace {
                // Grace: heartbeat should refresh after wake.
            } else {
                let prev = current_status.clone();
                current_status = ST_INACTIVE.to_string();
                current_context = format!("stale:{prev}");
                age = status_age;
            }
        }
    }

    let description = get_status_description(&current_status, &current_context);
    let description = if data.tool == "adhoc" && current_status == ST_INACTIVE {
        if let Some(rest) = description.strip_prefix("inactive: ") {
            rest.to_string()
        } else if description == "inactive" {
            String::new()
        } else {
            description
        }
    } else {
        description
    };

    let simple_context = if current_context.contains(':') {
        let (prefix, suffix) = current_context.split_once(':').unwrap();
        if prefix == "exit" {
            suffix.to_string()
        } else {
            prefix.to_string()
        }
    } else {
        current_context.clone()
    };

    ComputedStatus {
        status: current_status,
        age_string: format_age(age),
        description,
        age_seconds: age,
        context: simple_context,
    }
}

pub(crate) fn get_or_finalize_launch_failure_detail(
    db: &HcomDb,
    data: &InstanceRow,
) -> Option<String> {
    finalize_launch_failure_detail(db, data, None)
}

pub(crate) fn get_launch_blocker_detail(data: &InstanceRow) -> Option<String> {
    extract_launch_failure_detail(data)
}

pub(crate) fn finalize_launch_failure_detail(
    db: &HcomDb,
    data: &InstanceRow,
    fallback_detail: Option<&str>,
) -> Option<String> {
    if data.status_context == "launch_failed" && !data.status_detail.is_empty() {
        return Some(data.status_detail.clone());
    }

    if data.status_context != "new" || (data.status != ST_INACTIVE && data.status != "pending") {
        return if data.status_context == "launch_failed" {
            extract_launch_failure_detail(data)
                .or_else(|| fallback_detail.map(ToString::to_string))
                .or_else(|| (!data.status_detail.is_empty()).then(|| data.status_detail.clone()))
        } else {
            None
        };
    }

    if fallback_detail.is_none() {
        let created_at = data.created_at as i64;
        let age = if created_at > 0 {
            now_epoch_i64() - created_at
        } else {
            0
        };
        if age < LAUNCH_PLACEHOLDER_TIMEOUT {
            return None;
        }
    }

    let created_at = data.created_at as i64;
    let age = if created_at > 0 {
        (now_epoch_i64() - created_at).max(0)
    } else {
        0
    };
    // Name what the pid actually is. For a background launch this is the
    // wrapper shell hcom spawned, not the tool: the tool is its grandchild, and
    // a wrapper that is alive says nothing about whether the tool ever started.
    // The old wording ("process alive Ns, never bound") read as "the tool is
    // running but won't bind" and sent a Windows launch-chain stall investigation
    // after the tool instead of the chain.
    let process_state = data.pid.and_then(|pid| {
        let alive = crate::sys::process::is_alive(pid as u32);
        let what = if data.background != 0 {
            "launcher process"
        } else {
            "process"
        };
        alive.then(|| format!("{what} (pid {pid}) alive {age}s, never bound"))
    });
    let mut detail = fallback_detail
        .map(ToString::to_string)
        .or(process_state)
        .unwrap_or_else(|| format!("exited before binding (observed after {age}s)"));
    if !detail.contains("PTY output:")
        && let Some(evidence) = extract_launch_failure_detail(data)
        && !detail.contains(&evidence)
    {
        detail.push('\n');
        detail.push_str(&evidence);
    }

    let mut updates = serde_json::Map::new();
    updates.insert("status".into(), serde_json::json!(ST_INACTIVE));
    updates.insert("status_time".into(), serde_json::json!(now_epoch_i64()));
    updates.insert("status_context".into(), serde_json::json!("launch_failed"));
    updates.insert("status_detail".into(), serde_json::json!(detail.clone()));
    crate::instances::update_instance_position(db, &data.name, &updates);

    let mut event_data = serde_json::json!({
        "status": ST_INACTIVE,
        "context": "launch_failed",
        "position": data.last_event_id,
        "detail": detail.clone(),
    });
    if detail.is_empty() {
        event_data.as_object_mut().map(|obj| obj.remove("detail"));
    }
    let _ = db.log_event("status", &data.name, &event_data);

    Some(detail)
}

fn extract_launch_failure_detail(data: &InstanceRow) -> Option<String> {
    if !data.background_log_file.is_empty()
        && let Some(tail) = read_launch_log_tail(&data.background_log_file)
    {
        return Some(format!("PTY output:\n{tail}"));
    }

    let info = crate::terminal::resolve_terminal_info(
        data.terminal_preset_effective.as_deref(),
        data.launch_context.as_deref(),
    );

    match info.preset_name.as_str() {
        "tmux" | "tmux-split" => capture_tmux_launch_failure(&info.pane_id, &data.tool),
        _ => None,
    }
}

fn read_launch_log_tail(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    if lines.len() > 8 {
        lines = lines.split_off(lines.len() - 8);
    }
    let mut tail = lines.join("\n");
    if tail.chars().count() > 1000 {
        tail = tail.chars().rev().take(1000).collect::<String>();
        tail = tail.chars().rev().collect();
        tail.insert_str(0, "...");
    }
    Some(tail)
}

fn capture_tmux_launch_failure(pane_id: &str, tool: &str) -> Option<String> {
    if pane_id.is_empty() {
        return None;
    }

    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane_id])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_tmux_launch_failure_output(&String::from_utf8_lossy(&output.stdout), tool)
}

fn add_tmux_server_remediation(detail: &str) -> String {
    if !detail.contains("Operation not permitted") {
        return detail.to_string();
    }
    format!(
        "{detail} Fully reset tmux first (`tmux kill-server`), then start a fresh tmux server with approval/escalation (for example: `tmux new-session -d -s hcom-external`), then retry."
    )
}

fn parse_tmux_launch_failure_output(captured: &str, _tool: &str) -> Option<String> {
    let mut warning: Option<String> = None;

    for line in captured.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("Error:") {
            return Some(add_tmux_server_remediation(trimmed));
        }
        if warning.is_none() && trimmed.starts_with("WARNING:") {
            warning = Some(add_tmux_server_remediation(trimmed));
        }
    }

    warning
}

/// Build a human-readable status description from status and context tokens.
pub fn get_status_description(status: &str, context: &str) -> String {
    match status {
        ST_ACTIVE => {
            if let Some(sender) = context.strip_prefix("deliver:") {
                format!("active: msg from {sender}")
            } else if let Some(tool) = context.strip_prefix("tool:") {
                format!("active: {tool}")
            } else if let Some(tool) = context.strip_prefix("approved:") {
                format!("active: approved {tool}")
            } else if let Some(tool) = context.strip_prefix("denied:") {
                format!("active: denied {tool}")
            } else if context == "resuming" {
                "resuming...".to_string()
            } else if context.is_empty() {
                "active".to_string()
            } else {
                format!("active: {context}")
            }
        }
        ST_LISTENING => {
            // A delivery gate blocked past the escalation threshold carries a
            // `:stalled` suffix on its `tui:<reason>` context (see delivery.rs
            // `gate_block_context`). Strip it before matching so the reason
            // still renders friendly, then mark it stalled.
            let (context, stalled) = match context.strip_suffix(":stalled") {
                Some(base) => (base, " (stalled)"),
                None => (context, ""),
            };
            let desc = if context == "tui:not-ready" {
                "listening: blocked".to_string()
            } else if context == "tui:not-idle" {
                "listening: waiting for idle".to_string()
            } else if context == "tui:user-active" {
                "listening: user typing".to_string()
            } else if context == "tui:output-unstable" {
                "listening: output streaming".to_string()
            } else if context == "tui:prompt-has-text" {
                "listening: uncommitted text".to_string()
            } else if let Some(reason) = context.strip_prefix("tui:") {
                format!("listening: {}", reason.replace('-', " "))
            } else if context == "suspended" {
                "listening: suspended".to_string()
            } else {
                "listening".to_string()
            };
            format!("{desc}{stalled}")
        }
        ST_BLOCKED => {
            if context == "pty:approval" || context == "approval" {
                "blocked: approval pending".to_string()
            } else if context.is_empty() {
                "blocked: permission needed".to_string()
            } else {
                format!("blocked: {context}")
            }
        }
        ST_INACTIVE => {
            if context.starts_with("stale:") {
                "inactive: stale".to_string()
            } else if let Some(reason) = context.strip_prefix("exit:") {
                format!("inactive: {reason}")
            } else if context == "subagent:dormant" {
                "inactive: dormant subagent".to_string()
            } else if context == "unknown" {
                "inactive: unknown".to_string()
            } else if context.is_empty() {
                "inactive".to_string()
            } else {
                format!("inactive: {context}")
            }
        }
        _ => "unknown".to_string(),
    }
}

/// Set instance status with timestamp and log the status-change event.
#[track_caller]
pub fn set_status(
    db: &HcomDb,
    instance_name: &str,
    status: &str,
    context: &str,
    upd: StatusUpdate<'_>,
) {
    if let Err(error) = try_set_status(db, instance_name, status, context, upd) {
        eprintln!("[hcom] warn: set_status failed for {instance_name}: {error}");
    }
}

/// Fallible status transition for operations that cannot report success until
/// registration is persisted, such as orphan recovery.
#[track_caller]
pub fn try_set_status(
    db: &HcomDb,
    instance_name: &str,
    status: &str,
    context: &str,
    upd: StatusUpdate<'_>,
) -> anyhow::Result<()> {
    let StatusUpdate {
        detail,
        msg_ts,
        tool_name,
        tool_use_id,
    } = upd;
    let writer = std::panic::Location::caller();

    let current_data = db.get_instance_full(instance_name)?;
    let now = now_epoch_i64();
    let mut updates = serde_json::Map::new();
    updates.insert("status".into(), serde_json::json!(status));
    updates.insert("status_time".into(), serde_json::json!(now));
    updates.insert("status_context".into(), serde_json::json!(context));
    updates.insert("status_detail".into(), serde_json::json!(detail));

    if status == ST_LISTENING {
        updates.insert("last_stop".into(), serde_json::json!(now));
    }

    let old_status = current_data.as_ref().map(|d| d.status.as_str());
    let status_changed = old_status != Some(status);
    let status_event_changed = current_data.as_ref().is_none_or(|d| {
        d.status != status || d.status_context != context || d.status_detail != detail
    });

    db.update_instance_fields(instance_name, &updates)?;

    if status_changed {
        crate::notify::wake(db, instance_name, crate::notify::WakeKind::DELIVERY_LOOPS);
    }

    // The pi-family plugins (pi, and its fork omp) structurally double-write tool
    // status: the extension's tool_call handler calls reportStatus (omp/pi-status)
    // AND the Rust beforetool hook calls update_tool_status, both with the same
    // tool:<name>+detail. Suppress the redundant unchanged event for this family so
    // it doesn't emit duplicate status events (~30% of events for omp otherwise).
    let is_pi_family = matches!(
        current_data.as_ref().map(|d| d.tool.as_str()),
        Some("pi") | Some("omp")
    );
    if is_pi_family && !status_event_changed && msg_ts.is_empty() {
        return Ok(());
    }

    let position = current_data.as_ref().map(|d| d.last_event_id).unwrap_or(0);
    let mut data = serde_json::json!({
        "status": status,
        "context": context,
        "position": position,
    });
    if !detail.is_empty() {
        data["detail"] = serde_json::json!(detail);
    }
    if !msg_ts.is_empty() {
        data["msg_ts"] = serde_json::json!(msg_ts);
    }
    // old_* differs from the prior status event when set_gate_status() touched
    // the row without logging (tui:* gate context churns silently).
    data["old_status"] = serde_json::json!(old_status);
    data["old_context"] =
        serde_json::json!(current_data.as_ref().map(|d| d.status_context.as_str()));
    data["old_detail"] = serde_json::json!(current_data.as_ref().map(|d| d.status_detail.as_str()));
    data["new_status"] = serde_json::json!(status);
    data["new_context"] = serde_json::json!(context);
    data["new_detail"] = serde_json::json!(detail);
    data["writer"] = serde_json::json!(format!("{}:{}", writer.file(), writer.line()));
    if let Some(session_id) = current_data.as_ref().and_then(|d| d.session_id.as_deref()) {
        data["session"] = serde_json::json!(session_id);
    }
    if let Some(agent_id) = current_data.as_ref().and_then(|d| d.agent_id.as_deref()) {
        data["agent_id"] = serde_json::json!(agent_id);
    }
    if !tool_name.is_empty() {
        data["tool_name"] = serde_json::json!(tool_name);
    }
    if !tool_use_id.is_empty() {
        data["tool_use_id"] = serde_json::json!(tool_use_id);
    }
    let _ = db.log_event("status", instance_name, &data);
    Ok(())
}

/// Delete placeholder instances that have been launching too long.
pub fn cleanup_stale_placeholders(db: &HcomDb) -> i32 {
    let mut deleted = 0;
    let now = now_epoch_f64();

    if let Ok(instances) = db.iter_instances_full() {
        for data in &instances {
            if !crate::instances::is_launching_placeholder(data) {
                continue;
            }
            let created_at = data.created_at;
            if created_at > 0.0 && (now - created_at) > CLEANUP_PLACEHOLDER_THRESHOLD as f64 {
                crate::hooks::common::stop_placeholder_instance(
                    db,
                    &data.name,
                    "system",
                    "stale_cleanup",
                );
                deleted += 1;
            }
        }
    }
    deleted
}

/// Delete instances that have been inactive too long.
/// Three tiers: exit contexts (1 min), stale (1 hr), other inactive (12 hr).
pub fn cleanup_stale_instances(
    db: &HcomDb,
    max_stale_seconds: i64,
    max_inactive_seconds: i64,
) -> i32 {
    // Short-lived callers dominate this path (it runs from `hcom list`), and
    // they cannot detect a wake on their own — see is_in_wake_grace_shared.
    if is_in_wake_grace_shared(db) {
        return 0;
    }

    cleanup_stale_remote_instances(db);

    let mut deleted = 0;

    if let Ok(instances) = db.iter_instances_full() {
        for data in &instances {
            let computed = get_instance_status(data, db);

            if computed.status != ST_INACTIVE {
                continue;
            }

            let context = &computed.context;
            let age = computed.age_seconds;

            let reason = if matches!(
                context.as_str(),
                "killed" | "closed" | "timeout" | "interrupted" | "session_switch"
            ) && age > 60
            {
                "exit_cleanup"
            } else if context == "stale" && max_stale_seconds > 0 && age > max_stale_seconds {
                "stale_cleanup"
            } else if max_inactive_seconds > 0 && age > max_inactive_seconds {
                "inactive_cleanup"
            } else {
                continue;
            };

            // Staleness is a clock inference, not an observed death: a wedged
            // heartbeat (system sleep, a starved delivery loop) is
            // indistinguishable from an exited tool by timestamps alone. Losing
            // that bet is unrecoverable for the session — the row and both
            // bindings are deleted, and every later hook resolves to
            // no_instance with no path back — so let the clock lose to a live
            // PID. Exit contexts are exempt: those record an end that was
            // observed, not inferred.
            //
            // Tradeoff: a recycled PID can keep a dead row listed. That costs a
            // stale line in `hcom list`; the opposite mistake costs a running
            // agent.
            if reason != "exit_cleanup"
                && let Some(pid) = data.pid
                && tracked_pid_liveness(data) != Some(false)
            {
                crate::log::log_info(
                    "cleanup",
                    "skip_unconfirmed_dead_pid",
                    &format!(
                        "instance={} reason={} context={} age={}s pid={}",
                        data.name, reason, context, age, pid
                    ),
                );
                continue;
            }

            if crate::hooks::common::stop_instance(db, &data.name, "system", reason)
                == crate::hooks::common::StopOutcome::Stopped
            {
                deleted += 1;
            }
        }
    }

    deleted
}

fn cleanup_stale_remote_instances(db: &HcomDb) {
    let now = now_epoch_f64();
    let sync_map: std::collections::HashMap<String, String> = db
        .kv_prefix("relay_sync_time_")
        .unwrap_or_default()
        .into_iter()
        .collect();

    if let Ok(instances) = db.iter_instances_full() {
        let device_ids: std::collections::HashSet<String> = instances
            .iter()
            .filter_map(|d| d.origin_device_id.clone())
            .collect();

        for device_id in device_ids {
            let sync_val = sync_map.get(&format!("relay_sync_time_{device_id}"));
            let sync_time: f64 = sync_val.and_then(|s| s.parse().ok()).unwrap_or(0.0);
            if sync_time > 0.0 && (now - sync_time) <= REMOTE_DEVICE_STALE_THRESHOLD {
                continue;
            }
            if let Err(e) = db.conn().execute(
                "DELETE FROM instances WHERE origin_device_id = ?",
                rusqlite::params![device_id],
            ) {
                crate::log::log_warn("cleanup", "remote_stale_cleanup_fail", &e.to_string());
            } else {
                crate::log::log_info(
                    "cleanup",
                    "remote_device_stale",
                    crate::relay::device_id_prefix(&device_id),
                );
            }
        }
    }
}

/// Which cadence detected the dead process, recorded on the life event so
/// the startup pass and the (later) TUI cadence are distinguishable in
/// logs/snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadProcessDetector {
    /// The once-per-process reaper run from `main.rs` at startup.
    Startup,
    /// The in-TUI cadence.
    Tui,
}

impl DeadProcessDetector {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Tui => "tui",
        }
    }
}

/// `None` means this caller cannot establish death: a negative kill(pid, 0)
/// from a foreign PID namespace says nothing about the host process.
///
/// The namespace is stamped beside the PID on every write (see
/// `HcomDb::with_pid_namespace`), so a row that has one was written by a
/// process that could see that PID. Rows predating the column have none and
/// stay `None`: never trade a stale row for a live agent silently losing its
/// identity. The next PID write stamps them.
fn tracked_pid_liveness(inst: &crate::db::InstanceRow) -> Option<bool> {
    let pid = u32::try_from(inst.pid?).ok().filter(|pid| *pid > 0)?;
    // A row with no marker passes `""`, which matches no real namespace — so
    // it reads as unknown rather than as "namespace check not applicable".
    crate::sys::process::is_alive_in(pid, Some(inst.pid_namespace.as_deref().unwrap_or("")))
}

/// Outcome of probing whether a PID still names a live process.
///
/// `is_alive` treats only `EPERM` as alive; every other error — `ESRCH`
/// included — reads as dead, which is the tracked cross-namespace bug (see
/// `docs/issues/2026-09-17-cross-namespace-liveness-reaps-live-agents.md`).
/// `Unknown` covers PID namespaces this caller cannot inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessProbe {
    Alive,
    Dead,
    Unknown,
}

/// Detect and clean up instances whose processes died (crash, `kill`,
/// reboot, closed terminal/process group).
///
/// Finds local instances with a tracked PID that no longer names a live
/// process, saves a stopped snapshot for resume, and removes the dead row.
/// Hook-based agents without tracked PIDs are handled by the existing
/// heartbeat staleness detection the next time `hcom list` runs.
///
/// The underlying finalize is identity- *and* liveness-guarded
/// (`HcomDb::finalize_instance_stop_guarded`): a row that has since been
/// rebound or resumed under the same name is left alone rather than deleted
/// out from under it, and a concurrent finalizer (e.g. a PTY exit) racing in
/// first is a normal no-op here, not an error.
///
/// Returns the number of instances actually reconciled this pass. Errors
/// (DB unreadable, a single instance's finalize failing) are not fatal: the
/// caller logs and retries on the next pass, since a skipped row is always
/// retryable — nothing here is destructive without winning its guard.
pub fn reconcile_dead_instances(
    db: &HcomDb,
    detector: DeadProcessDetector,
) -> anyhow::Result<usize> {
    reconcile_dead_instances_with_probe(db, detector, |inst, _pid| {
        match tracked_pid_liveness(inst) {
            Some(true) => ProcessProbe::Alive,
            Some(false) => ProcessProbe::Dead,
            None => ProcessProbe::Unknown,
        }
    })
}

fn reconcile_dead_instances_with_probe(
    db: &HcomDb,
    detector: DeadProcessDetector,
    probe: impl Fn(&crate::db::InstanceRow, u32) -> ProcessProbe,
) -> anyhow::Result<usize> {
    let instances = db.iter_instances_full()?;
    let mut reconciled = 0;

    for inst in &instances {
        if inst.status == ST_INACTIVE || inst.status == ST_LAUNCHING {
            continue;
        }

        let is_remote = inst
            .origin_device_id
            .as_deref()
            .is_some_and(|v| !v.is_empty());
        if is_remote {
            continue;
        }

        let pid = match inst.pid {
            Some(p) if p > 0 => p as u32,
            _ => continue,
        };

        // Recheck liveness immediately before finalizing. Anything but a
        // confirmed-dead probe result fails safe and keeps the row.
        match probe(inst, pid) {
            ProcessProbe::Alive | ProcessProbe::Unknown => continue,
            ProcessProbe::Dead => {}
        }

        let snapshot = serde_json::json!({
            "name": inst.name,
            "transcript_path": inst.transcript_path,
            "session_id": inst.session_id,
            "tool": inst.tool,
            "directory": inst.directory,
            "parent_name": inst.parent_name,
            "tag": inst.tag,
            "wait_timeout": inst.wait_timeout,
            "subagent_timeout": inst.subagent_timeout,
            "hints": inst.hints,
            "pid": inst.pid,
            "created_at": inst.created_at,
            "background": inst.background,
            "agent_id": inst.agent_id,
            "launch_args": inst.launch_args,
            "origin_device_id": inst.origin_device_id,
            "background_log_file": inst.background_log_file,
            "last_event_id": inst.last_event_id,
        });

        // Một hcom khác có thể đang dừng instance này có chủ đích; lý do của nó
        // đúng hơn phỏng đoán "dead_process" của ta.
        let (reason, initiated_by) =
            crate::hooks::common::read_stop_reason(db, &inst.name, inst.created_at)
                .unwrap_or_else(|| ("exit:dead_process".to_string(), "system".to_string()));

        let event_data = serde_json::json!({
            "action": "stopped",
            "by": initiated_by,
            "reason": reason,
            "detector": detector.as_str(),
            "snapshot": snapshot,
        });
        // Guarded on identity AND the observed pid/status: a concurrent
        // finalizer (PTY exit, another reconciler pass) or a fresh
        // rebind/resume under the same name must not be double-published or
        // deleted out from under it.
        match db.finalize_instance_stop_guarded(
            &inst.name,
            inst.created_at,
            inst.session_id.as_deref(),
            inst.agent_id.as_deref(),
            inst.pid,
            inst.pid_namespace.as_deref().unwrap_or(""),
            &inst.status,
            &event_data,
        ) {
            Ok(true) => {
                reconciled += 1;
                crate::log::log_info(
                    "lifecycle",
                    "mark_dead",
                    &format!(
                        "instance={} pid={} tool={} detector={}",
                        inst.name,
                        pid,
                        inst.tool,
                        detector.as_str(),
                    ),
                );
            }
            Ok(false) => {
                // Normal race: another finalizer already won, or the row
                // changed underneath us. Nothing to retry for this pass.
            }
            Err(e) => {
                crate::log::log_warn(
                    "lifecycle",
                    "reconcile_dead_instance_failed",
                    &format!("instance={} err={e}", inst.name),
                );
            }
        }
    }

    Ok(reconciled)
}

/// Startup-compatible wrapper around [`reconcile_dead_instances`]. Runs once
/// per `hcom` process (`main.rs`, before dispatch). Best-effort: a failure to
/// even read the instance table is logged and treated as zero reconciled,
/// never aborts startup — the next `hcom` invocation retries.
///
/// Returns the number of instances marked dead.
pub fn mark_dead_instances(db: &HcomDb) -> i32 {
    match reconcile_dead_instances(db, DeadProcessDetector::Startup) {
        Ok(n) => n as i32,
        Err(e) => {
            crate::log::log_warn("lifecycle", "mark_dead_instances_failed", &e.to_string());
            0
        }
    }
}

#[cfg(test)]
#[path = "instance_lifecycle_tests.rs"]
mod tests;
