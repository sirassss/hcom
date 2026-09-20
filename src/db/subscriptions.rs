//! Event subscription storage, creation, firing, and delivery.
//!
//! `events_sub:` rows in `kv` are the canonical subscription/event-model
//! contract. The key prefix is `events_sub:` followed by a stable subscription
//! ID. Recognized JSON fields are:
//! - `id`: stable subscription ID, also encoded in the key.
//! - `caller`: agent or external sender that owns the subscription.
//! - `sql`: SQL predicate evaluated against `events_v`.
//! - `params`: optional SQL parameters for parameterized internal subs.
//! - `filters`: original structured filters and internal metadata.
//! - `thread_name`: thread name for delivery-only membership rows.
//! - `on_hit_text`: optional message to send when the subscription fires.
//! - `caller_kind`: frozen sender kind for `on_hit_text` provenance.
//! - `last_id`: cursor for the last event processed by this subscription.
//! - `created`: creation timestamp used for ordering/listing.
//! - `once`: remove the subscription after its first match.
//! - `delivery_only`: internal row used for routing state, not notification.
//! - `auto_thread_member`: delivery-only thread membership marker.
//!
//! Subscription kinds stored under this prefix are filter subs, SQL subs,
//! request watches (`reqwatch-*`), delivery-only thread members, and collision
//! subs. New code should not write `events_sub:` rows outside this module.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params;
use serde_json::json;

use super::HcomDb;
use crate::core::filters::{FILE_WRITE_CONTEXTS, build_sql_from_flags};
use crate::messages::{InstanceInfo, MessageScope, ScopeResult, compute_scope, resolve_targets};
use crate::shared::constants::extract_mentions;

fn subscription_is_delivery_only(sub: &serde_json::Value) -> bool {
    match sub.get("delivery_only") {
        Some(serde_json::Value::Bool(flag)) => *flag,
        Some(serde_json::Value::Number(n)) => n.as_i64() == Some(1),
        Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true") || s == "1",
        _ => false,
    }
}

/// Stable subscription ID for automatic thread membership rows.
pub(crate) fn thread_membership_sub_id(thread: &str, member: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(format!("thread-member:{thread}:{member}").as_bytes());
    let hash = hasher.finalize();
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sub-{}", &hex[..8])
}

/// Outcome of a subscription insert attempt.
pub(crate) enum SubCreateOutcome {
    Created { id: String, final_sql: String },
    AlreadyExists { id: String },
}

/// Build and insert a filter-based subscription row into `kv`.
pub(crate) fn create_filter_subscription(
    db: &HcomDb,
    filters: &HashMap<String, Vec<String>>,
    sql_parts: &[String],
    caller: &str,
    once: bool,
    on_hit: Option<&str>,
) -> Result<SubCreateOutcome, String> {
    let mut sql = match build_sql_from_flags(filters) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return Err("No valid filters provided".to_string()),
        Err(e) => return Err(format!("Filter error: {e}")),
    };

    if !sql_parts.is_empty() {
        let manual_sql = sql_parts.join(" ").replace("\\!", "!");
        if let Err(e) = db.conn().execute(
            &format!("SELECT 1 FROM events_v WHERE ({manual_sql}) LIMIT 0"),
            [],
        ) {
            return Err(format!("Invalid SQL: {e}"));
        }
        sql = format!("({sql}) AND ({manual_sql})");
    }

    if filters.contains_key("collision") {
        let self_relevance = collision_self_relevance_sql(caller);
        sql = format!("({sql}) AND {self_relevance}");
    }

    let id_source = format!(
        "{}:{}:{}:{}:{}",
        caller,
        serde_json::to_string(filters).unwrap_or_default(),
        sql,
        once,
        on_hit.unwrap_or(""),
    );
    let hash = sha256_hash(&id_source);
    let sub_id = format!("sub-{}", &hash[..8]);
    let sub_key = format!("events_sub:{sub_id}");

    if db.kv_get(&sub_key).ok().flatten().is_some() {
        return Ok(SubCreateOutcome::AlreadyExists { id: sub_id });
    }

    let now = crate::shared::time::now_epoch_f64();
    let last_id = db.get_last_event_id();

    let mut sub_data = json!({
        "id": sub_id,
        "caller": caller,
        "filters": filters,
        "sql": sql,
        "created": now,
        "last_id": last_id,
        "once": once,
    });
    if let Some(text) = on_hit {
        sub_data["on_hit_text"] = json!(text);
        sub_data["caller_kind"] = json!(resolve_caller_kind(db, caller));
    }

    let _ = db.kv_set(&sub_key, Some(&sub_data.to_string()));

    Ok(SubCreateOutcome::Created {
        id: sub_id,
        final_sql: sql,
    })
}

/// Build and insert a raw-SQL subscription row into `kv`.
pub(crate) fn build_and_insert_sql_subscription(
    db: &HcomDb,
    sql_parts: &[String],
    caller: &str,
    once: bool,
    on_hit: Option<&str>,
) -> Result<SubCreateOutcome, String> {
    let sql = sql_parts.join(" ").replace("\\!", "!");

    if let Err(e) = db
        .conn()
        .execute(&format!("SELECT 1 FROM events_v WHERE ({sql}) LIMIT 0"), [])
    {
        return Err(format!("Invalid SQL: {e}"));
    }

    let hash = sha256_hash(&format!("{caller}{sql}{once}{}", on_hit.unwrap_or("")));
    let sub_id = format!("sub-{}", &hash[..8]);
    let sub_key = format!("events_sub:{sub_id}");

    if db.kv_get(&sub_key).ok().flatten().is_some() {
        return Ok(SubCreateOutcome::AlreadyExists { id: sub_id });
    }

    let now = crate::shared::time::now_epoch_f64();
    let last_id = db.get_last_event_id();

    let mut sub_data = json!({
        "id": sub_id,
        "sql": sql,
        "caller": caller,
        "once": once,
        "last_id": last_id,
        "created": now,
    });
    if let Some(text) = on_hit {
        sub_data["on_hit_text"] = json!(text);
        sub_data["caller_kind"] = json!(resolve_caller_kind(db, caller));
    }

    let _ = db.kv_set(&sub_key, Some(&sub_data.to_string()));

    Ok(SubCreateOutcome::Created {
        id: sub_id,
        final_sql: sql,
    })
}

pub(crate) use super::reqwatch_policy::AGY_REQWATCH_IDLE_GRACE_SEC;

fn instance_tool(db: &HcomDb, name: &str) -> String {
    db.conn()
        .query_row(
            "SELECT COALESCE(tool, '') FROM instances WHERE name = ?",
            params![name],
            |row| row.get(0),
        )
        .unwrap_or_default()
}

fn reqwatch_reply_exists(db: &HcomDb, request_id: i64, target: &str, sub_caller: &str) -> bool {
    if sub_caller.is_empty() {
        return false;
    }
    db.conn()
        .query_row(
            "SELECT 1 FROM events_v WHERE id > ? AND type = 'message' \
             AND msg_from = ? AND (\
               (msg_scope = 'mentions' AND EXISTS (\
                  SELECT 1 FROM json_each(msg_delivered_to) WHERE value = ?\
                )) \
               OR json_extract(data, '$.reply_to_local') = ? \
             )",
            params![request_id, target, sub_caller, request_id],
            |_| Ok(true),
        )
        .unwrap_or(false)
}

fn kv_store_sub(db: &HcomDb, key: &str, sub: &serde_json::Value) {
    match serde_json::to_string(sub) {
        Ok(json) => {
            if let Err(e) = db.kv_set(key, Some(&json)) {
                crate::log::log_error("db", "reqwatch.kv_set", &format!("{e}"));
            }
        }
        Err(e) => crate::log::log_error("db", "reqwatch.serialize", &format!("{e}")),
    }
}

/// Clear agy grace timers when the target is working again (deliver/tool/active).
fn clear_agy_reqwatch_idle_grace(db: &HcomDb, target: &str) {
    for (key, sub, filters) in load_reqwatch_subs(db) {
        if filters.get("target_tool").and_then(|v| v.as_str()) != Some("antigravity") {
            continue;
        }
        if filters.get("target").and_then(|v| v.as_str()) != Some(target) {
            continue;
        }
        if sub.get("idle_grace_until").is_none() {
            continue;
        }
        let mut sub_mut = sub.clone();
        if let Some(obj) = sub_mut.as_object_mut() {
            obj.remove("idle_grace_until");
            obj.remove("idle_grace_event_id");
            kv_store_sub(db, &key, &sub_mut);
        }
    }
}

/// Fire Antigravity request watches whose idle grace elapsed while no matching
/// event arrived. The conditional delete is the claim: concurrent sweepers can
/// observe the same row, but only one can remove it and emit the one-shot notice.
fn sweep_expired_reqwatch_graces(db: &HcomDb, now: f64) {
    for (key, sub, filters) in load_reqwatch_subs(db) {
        if filters.get("target_tool").and_then(|v| v.as_str()) != Some("antigravity")
            || !super::reqwatch_policy::idle_grace_expired(&sub, now)
        {
            continue;
        }

        let request_id = filters
            .get("request_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let target = filters.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let caller = sub.get("caller").and_then(|v| v.as_str()).unwrap_or("");
        if request_id <= 0 || target.is_empty() || caller.is_empty() {
            continue;
        }

        if reqwatch_reply_exists(db, request_id, target, caller) {
            let _ = db.kv_set(&key, None);
            continue;
        }

        let candidate_event_id = sub
            .get("idle_grace_event_id")
            .and_then(|v| v.as_i64())
            .or_else(|| sub.get("last_id").and_then(|v| v.as_i64()))
            .unwrap_or(0);

        let claimed = db
            .conn()
            .execute(
                "DELETE FROM kv
                 WHERE key = ?1
                   AND CAST(json_extract(value, '$.idle_grace_until') AS REAL) <= ?2
                   AND json_extract(value, '$.filters.target_tool') = 'antigravity'
                   AND EXISTS (
                       SELECT 1 FROM instances
                       WHERE name = ?3 AND status = 'listening' AND last_event_id >= ?4
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM events_v
                       WHERE id > ?4 AND type = 'message' AND msg_from = ?3
                         AND (
                             (msg_scope = 'mentions' AND EXISTS (
                                 SELECT 1 FROM json_each(msg_delivered_to) WHERE value = ?5
                             ))
                             OR json_extract(data, '$.reply_to_local') = ?4
                         )
                   )",
                params![key, now, target, request_id, caller],
            )
            .unwrap_or(0);
        if claimed != 1 {
            continue;
        }

        let sub_id = sub
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(key.as_str());
        let notification = format_sub_notification(
            db,
            sub_id,
            candidate_event_id,
            "status",
            target,
            &serde_json::json!({"status": "listening"}),
            Some(&filters),
        );
        let _ = send_sub_notification(db, caller, &notification);
    }
}

/// Create request-watch subscriptions for each recipient.
pub(crate) fn create_request_watches(
    db: &HcomDb,
    sender: &str,
    request_event_id: i64,
    recipients: &[String],
) {
    let last_id = db.get_last_event_id();
    let now = crate::shared::time::now_epoch_f64();

    for recipient in recipients {
        let sub_id = format!("reqwatch-{request_event_id}-{recipient}");
        let sub_key = format!("events_sub:{sub_id}");
        let target_tool = instance_tool(db, recipient);

        let sql = "(type='status' AND instance=? AND status_val='listening') OR (type='life' AND instance=? AND life_action='stopped')";

        let sub_data = serde_json::json!({
            "id": sub_id,
            "caller": sender,
            "sql": sql,
            "params": [recipient, recipient],
            "filters": {
                "request_watch": true,
                "request_id": request_event_id,
                "target": recipient,
                "target_tool": target_tool,
            },
            "once": true,
            "last_id": last_id,
            "created": now,
        });

        kv_store_sub(db, &sub_key, &sub_data);
    }
}

/// Remove all event subscriptions owned by an instance.
pub(crate) fn cleanup_subscriptions(db: &HcomDb, name: &str) -> Result<u32> {
    let deleted = db.conn.execute(
        "DELETE FROM kv
         WHERE key LIKE 'events_sub:%'
           AND json_extract(value, '$.caller') = ?
           AND COALESCE(json_extract(value, '$.delivery_only'), 0) != 1",
        params![name],
    )?;
    Ok(deleted as u32)
}

/// Remove delivery-only thread memberships for an instance name reuse.
pub(crate) fn cleanup_thread_memberships_for_name_reuse(db: &HcomDb, name: &str) -> Result<u32> {
    let deleted = db.conn.execute(
        "DELETE FROM kv
         WHERE key LIKE 'events_sub:%'
           AND json_extract(value, '$.caller') = ?
           AND json_extract(value, '$.auto_thread_member') = 1
           AND COALESCE(json_extract(value, '$.delivery_only'), 0) = 1",
        params![name],
    )?;
    Ok(deleted as u32)
}

/// Return active members of a thread in join order.
pub(crate) fn get_thread_members(db: &HcomDb, thread: &str) -> Vec<String> {
    let active_instances: HashSet<String> = db
        .conn()
        .prepare("SELECT name FROM instances")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(0))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|r| r.ok())
                .collect()
        })
        .unwrap_or_default();

    let rows: Vec<String> = db
        .conn()
        .prepare(
            "SELECT value FROM kv
             WHERE key LIKE 'events_sub:%'
               AND json_extract(value, '$.auto_thread_member') = 1
               AND json_extract(value, '$.thread_name') = ?
             ORDER BY json_extract(value, '$.created') ASC, key ASC",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![thread], |row| row.get::<_, String>(0))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|r| r.ok())
                .collect()
        })
        .unwrap_or_default();

    let mut members = Vec::new();
    let mut seen = HashSet::new();
    for value in rows {
        let caller = serde_json::from_str::<serde_json::Value>(&value)
            .ok()
            .and_then(|sub| sub.get("caller").and_then(|v| v.as_str()).map(String::from));
        if let Some(caller) = caller
            && active_instances.contains(&caller)
            && seen.insert(caller.clone())
        {
            members.push(caller);
        }
    }
    members
}

/// Upsert memberships for recipients of a thread message.
pub(crate) fn add_thread_memberships(
    db: &HcomDb,
    thread: &str,
    sender: Option<&str>,
    recipients: &[String],
) {
    let mut members = recipients.to_vec();
    if let Some(sender) = sender {
        members.push(sender.to_string());
    }

    let now = crate::shared::time::now_epoch_f64();
    let last_id = db.get_last_event_id();
    let mut seen = HashSet::new();
    for (idx, member) in members.into_iter().enumerate() {
        if !seen.insert(member.clone()) {
            continue;
        }
        let sub_id = thread_membership_sub_id(thread, &member);
        let key = format!("events_sub:{sub_id}");
        let data = serde_json::json!({
            "id": sub_id,
            "caller": member,
            "thread_name": thread,
            "auto_thread_member": true,
            "delivery_only": true,
            "sql": "0",
            "created": now + (idx as f64 * 0.000001),
            "last_id": last_id,
            "once": false,
        });
        let _ = db.kv_set(&key, Some(&data.to_string()));
    }
}

/// Check subscriptions and send matching notifications.
/// Called inline from log_event(). Errors logged, never propagated.
pub(crate) fn process_logged_event(
    db: &HcomDb,
    event_id: i64,
    event_type: &str,
    instance: &str,
    data: &serde_json::Value,
) {
    // Recursion guard: skip events that could cause notification loops.
    if instance.starts_with("sys_") {
        return;
    }
    if event_type == "message" {
        let sender = data.get("from").and_then(|v| v.as_str()).unwrap_or("");
        let sender_kind = data
            .get("sender_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if sender == "[hcom-events]" || sender_kind == "system" {
            return;
        }
    }

    if event_type == "message" {
        let msg_sender = data.get("from").and_then(|v| v.as_str()).unwrap_or("");
        let reply_to_id = data.get("reply_to_local").and_then(|v| v.as_i64());

        if let Some("mentions") = data.get("scope").and_then(|v| v.as_str()) {
            let msg_delivered_to: Vec<String> = data
                .get("delivered_to")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !msg_sender.is_empty() && !msg_delivered_to.is_empty() {
                cancel_request_watches_by_flow(db, msg_sender, &msg_delivered_to, reply_to_id);
            }
        }

        if let Some(rid) = reply_to_id
            && !msg_sender.is_empty()
        {
            cancel_request_watches_by_reply_id(db, msg_sender, rid);
        }
    }

    // agy: turn-end `listening` is normal; reset reqwatch grace when target is active again.
    if event_type == "status" {
        let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "active" || status == "blocked" {
            clear_agy_reqwatch_idle_grace(db, instance);
        }
    }

    let rows: Vec<(String, String)> = match db.conn.prepare_cached(
        "SELECT key, value FROM kv
         WHERE key LIKE 'events_sub:%'
           AND COALESCE(json_extract(value, '$.delivery_only'), 0) != 1
           AND COALESCE(json_extract(value, '$.delivery_only'), 'false') != 'true'",
    ) {
        Ok(mut stmt) => stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(_) => return,
    };

    if rows.is_empty() {
        return;
    }

    for (key, value) in &rows {
        let sub: serde_json::Value = match serde_json::from_str(value) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if subscription_is_delivery_only(&sub) {
            continue;
        }
        let sub_id = sub
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(key.as_str());

        let last_id = sub.get("last_id").and_then(|v| v.as_i64()).unwrap_or(0);
        if event_id <= last_id {
            continue;
        }

        let sql = sub.get("sql").and_then(|v| v.as_str()).unwrap_or("");
        if !sql.is_empty() {
            let filter_query = format!("SELECT 1 FROM events_v WHERE id = ? AND ({})", sql);
            let stored_params: Vec<String> = sub
                .get("params")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let matched = if stored_params.is_empty() {
                db.conn
                    .query_row(&filter_query, params![event_id], |_| Ok(()))
                    .is_ok()
            } else {
                let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(event_id)];
                for p in &stored_params {
                    all_params.push(Box::new(p.clone()));
                }
                let refs: Vec<&dyn rusqlite::types::ToSql> =
                    all_params.iter().map(|p| p.as_ref()).collect();
                db.conn
                    .query_row(&filter_query, refs.as_slice(), |_| Ok(()))
                    .is_ok()
            };

            if !matched {
                continue;
            }
        }

        let sub_filters = sub
            .get("filters")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if sub_filters.get("request_watch").is_some() {
            let request_id = sub_filters
                .get("request_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let target = sub_filters
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let sub_caller = sub.get("caller").and_then(|v| v.as_str()).unwrap_or("");
            if request_id > 0 && !target.is_empty() {
                let waterline: i64 = db
                    .conn
                    .query_row(
                        "SELECT last_event_id FROM instances WHERE name = ?",
                        params![target],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if waterline < request_id {
                    let mut sub_mut = sub.clone();
                    sub_mut["last_id"] = serde_json::json!(event_id);
                    kv_store_sub(db, key, &sub_mut);
                    continue;
                }

                if reqwatch_reply_exists(db, request_id, target, sub_caller) {
                    if let Err(e) = db.kv_set(key, None) {
                        crate::log::log_error(
                            "db",
                            "check_event_subscriptions.kv_set_cleanup",
                            &format!("{e}"),
                        );
                    }
                    continue;
                }

                let target_tool = sub_filters
                    .get("target_tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let now = crate::shared::time::now_epoch_f64();
                match super::reqwatch_policy::reqwatch_notify_decision(
                    target_tool,
                    event_type,
                    data,
                    &sub,
                    now,
                ) {
                    super::reqwatch_policy::ReqwatchNotifyDecision::Skip => continue,
                    super::reqwatch_policy::ReqwatchNotifyDecision::Defer {
                        set_grace_if_absent,
                    } => {
                        let mut sub_mut = sub.clone();
                        sub_mut["last_id"] = serde_json::json!(event_id);
                        if set_grace_if_absent {
                            sub_mut["idle_grace_until"] =
                                serde_json::json!(now + AGY_REQWATCH_IDLE_GRACE_SEC);
                            sub_mut["idle_grace_event_id"] = serde_json::json!(event_id);
                        }
                        kv_store_sub(db, key, &sub_mut);
                        continue;
                    }
                    super::reqwatch_policy::ReqwatchNotifyDecision::Proceed => {}
                }
            }
        }

        let still_exists: bool = db
            .conn
            .query_row("SELECT 1 FROM kv WHERE key = ?", params![key], |_| Ok(true))
            .unwrap_or(false);
        if !still_exists {
            continue;
        }

        let caller = sub.get("caller").and_then(|v| v.as_str()).unwrap_or("");
        if caller.is_empty() {
            continue;
        }

        let filters_opt = sub.get("filters");
        let notification = format_sub_notification(
            db,
            sub_id,
            event_id,
            event_type,
            instance,
            data,
            filters_opt,
        );
        let _ = send_sub_notification(db, caller, &notification);

        if let Some(on_hit_text) = sub.get("on_hit_text").and_then(|v| v.as_str()) {
            let caller_kind = sub
                .get("caller_kind")
                .and_then(|v| v.as_str())
                .unwrap_or("external");
            if let Err(e) = send_message_as(db, caller, caller_kind, on_hit_text) {
                crate::log::log_error("db", "check_event_subscriptions.on_hit", &format!("{e}"));
            }
        }

        if sub.get("once").and_then(|v| v.as_bool()).unwrap_or(false) {
            if let Err(e) = db.kv_set(key, None) {
                crate::log::log_error(
                    "db",
                    "check_event_subscriptions.kv_set_once",
                    &format!("{e}"),
                );
            }
        } else {
            let mut sub_mut = sub.clone();
            sub_mut["last_id"] = serde_json::json!(event_id);
            match serde_json::to_string(&sub_mut) {
                Ok(json) => {
                    if let Err(e) = db.kv_set(key, Some(&json)) {
                        crate::log::log_error(
                            "db",
                            "check_event_subscriptions.kv_set_cursor",
                            &format!("{e}"),
                        );
                    }
                }
                Err(e) => {
                    crate::log::log_error(
                        "db",
                        "check_event_subscriptions.serialize_cursor",
                        &format!("{e}"),
                    );
                }
            }
        }
    }

    // A grace expiry is not itself an event. Sweep after handling the current
    // event so replies, active/blocked transitions, and stop events win first.
    sweep_expired_reqwatch_graces(db, crate::shared::time::now_epoch_f64());
}

/// Load all reqwatch subscriptions as (key, parsed_sub, filters) tuples.
pub(crate) fn load_reqwatch_subs(
    db: &HcomDb,
) -> Vec<(String, serde_json::Value, serde_json::Value)> {
    let rows: Vec<(String, String)> = match db
        .conn
        .prepare_cached("SELECT key, value FROM kv WHERE key LIKE 'events_sub:reqwatch-%'")
    {
        Ok(mut stmt) => stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(_) => return vec![],
    };

    rows.into_iter()
        .filter_map(|(key, value)| {
            let sub: serde_json::Value = serde_json::from_str(&value).ok()?;
            let filters = sub.get("filters")?.clone();
            Some((key, sub, filters))
        })
        .collect()
}

/// Cancel request-watch subs when watched target messages the requester.
pub(crate) fn cancel_request_watches_by_flow(
    db: &HcomDb,
    sender: &str,
    delivered_to: &[String],
    reply_to_id: Option<i64>,
) {
    for (key, sub, filters) in &load_reqwatch_subs(db) {
        let target = filters.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let sub_caller = sub.get("caller").and_then(|v| v.as_str()).unwrap_or("");

        if target == sender && delivered_to.iter().any(|d| d == sub_caller) {
            if let Some(rid) = reply_to_id {
                let req_id = filters
                    .get("request_id")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if req_id != rid {
                    continue;
                }
            }
            if let Err(e) = db.kv_set(key, None) {
                crate::log::log_error(
                    "db",
                    "cancel_request_watches_by_flow.kv_set",
                    &format!("{e}"),
                );
            }
        }
    }
}

/// Cancel request-watch subs by explicit reply_to match.
pub(crate) fn cancel_request_watches_by_reply_id(db: &HcomDb, sender: &str, reply_to_id: i64) {
    for (key, _sub, filters) in &load_reqwatch_subs(db) {
        let target = filters.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let req_id = filters
            .get("request_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if target == sender
            && req_id == reply_to_id
            && let Err(e) = db.kv_set(key, None)
        {
            crate::log::log_error(
                "db",
                "cancel_request_watches_by_reply.kv_set",
                &format!("{e}"),
            );
        }
    }
}

/// Send a system notification message.
pub(crate) fn send_system_message(
    db: &HcomDb,
    sender_name: &str,
    message: &str,
) -> Result<Vec<String>> {
    send_message_as(db, sender_name, "system", message)
}

/// Send a message from a specific sender kind.
pub(crate) fn send_message_as(
    db: &HcomDb,
    sender_name: &str,
    sender_kind: &str,
    message: &str,
) -> Result<Vec<String>> {
    let mut stmt = db.conn.prepare_cached("SELECT name, tag FROM instances")?;
    let instances: Vec<InstanceInfo> = stmt
        .query_map([], |row| {
            Ok(InstanceInfo {
                name: row.get::<_, String>(0)?,
                tag: row.get::<_, Option<String>>(1)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let parsed_mentions = extract_mentions(message);
    let scope_result = if parsed_mentions.is_empty() {
        compute_scope(message, &instances, None).map_err(anyhow::Error::msg)?
    } else {
        let (mentions, _) =
            resolve_targets(&parsed_mentions, &instances).map_err(anyhow::Error::msg)?;
        ScopeResult {
            scope: MessageScope::Mentions,
            mentions,
        }
    };
    let (scope, mention_list, delivered_to) = if scope_result.scope == MessageScope::Broadcast {
        let delivered: Vec<String> = instances
            .iter()
            .filter(|inst| inst.name != sender_name)
            .map(|inst| inst.name.clone())
            .collect();
        ("broadcast".to_string(), vec![], delivered)
    } else {
        let delivered: Vec<String> = scope_result
            .mentions
            .iter()
            .filter(|n| n.as_str() != sender_name)
            .cloned()
            .collect();
        ("mentions".to_string(), scope_result.mentions, delivered)
    };

    let mut event_data = serde_json::json!({
        "from": sender_name,
        "sender_kind": sender_kind,
        "scope": scope,
        "text": message,
        "delivered_to": delivered_to,
    });
    if !mention_list.is_empty() {
        event_data["mentions"] = serde_json::json!(mention_list);
    }

    let routing_instance = match sender_kind {
        "instance" => sender_name.to_string(),
        "external" => format!("ext_{}", sender_name),
        _ => format!("sys_{}", sender_name),
    };
    db.log_event("message", &routing_instance, &event_data)?;

    Ok(delivered_to)
}

fn resolve_caller_kind(db: &HcomDb, caller: &str) -> &'static str {
    let exists: bool = db
        .conn()
        .query_row(
            "SELECT 1 FROM instances WHERE name = ?",
            rusqlite::params![caller],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if exists { "instance" } else { "external" }
}

fn collision_self_relevance_sql(caller: &str) -> String {
    let caller_escaped = caller.replace('\'', "''");
    format!(
        "(events_v.instance = '{caller_escaped}' OR EXISTS (SELECT 1 FROM events_v e2 WHERE e2.type = 'status' AND e2.status_context IN {ctx} AND e2.status_detail = events_v.status_detail AND e2.instance = '{caller_escaped}' AND ABS(strftime('%s', events_v.timestamp) - strftime('%s', e2.timestamp)) < 30))",
        ctx = FILE_WRITE_CONTEXTS
    )
}

fn format_sub_notification(
    db: &HcomDb,
    sub_id: &str,
    event_id: i64,
    event_type: &str,
    instance: &str,
    data: &serde_json::Value,
    filters: Option<&serde_json::Value>,
) -> String {
    if let Some(f) = filters {
        if f.get("request_watch").is_some() {
            let request_id = f
                .get("request_id")
                .and_then(|v| v.as_i64())
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            let target = f.get("target").and_then(|v| v.as_str()).unwrap_or(instance);
            let action = if event_type == "status" {
                "went idle"
            } else {
                "stopped"
            };
            return format!(
                "[sub:{}] #{} {} {} without responding to your request #{}",
                sub_id, event_id, target, action, request_id
            );
        }

        if f.get("collision").is_some() && event_type == "status" {
            let file_path = data.get("detail").and_then(|v| v.as_str()).unwrap_or("?");
            if let Some(partner) = find_collision_partner(db, event_id, instance, file_path) {
                return format!(
                    "\u{26a0}\u{fe0f} COLLISION [sub:{}] #{}: {} and {} both edited {}",
                    sub_id, event_id, instance, partner, file_path
                );
            }
            return format!(
                "\u{26a0}\u{fe0f} COLLISION [sub:{}] #{}: {} edited {} (conflict with another agent)",
                sub_id, event_id, instance, file_path
            );
        }
    }

    let mut parts = vec![
        format!("[sub:{}]", sub_id),
        format!("#{}", event_id),
        event_type.to_string(),
        instance.to_string(),
    ];

    match event_type {
        "message" => {
            let mut text = data
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if text.len() > 60 {
                let mut end = 57;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text = format!("{}...", &text[..end]);
            }
            text = text.replace('@', "(at)");
            let from = data.get("from").and_then(|v| v.as_str()).unwrap_or("?");
            parts.push(format!("from:{}", from));
            parts.push(format!("\"{}\"", text));
        }
        "status" => {
            parts.push(
                data.get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
            );
            if let Some(ctx) = data.get("context").and_then(|v| v.as_str())
                && !ctx.is_empty()
            {
                parts.push(ctx.to_string());
                if let Some(detail) = data.get("detail").and_then(|v| v.as_str())
                    && !detail.is_empty()
                {
                    let truncated = if detail.len() > 40 {
                        if ctx.contains("Bash") {
                            let end = (0..=37)
                                .rev()
                                .find(|&i| detail.is_char_boundary(i))
                                .unwrap_or(0);
                            format!("{}...", &detail[..end])
                        } else {
                            let start = (detail.len().saturating_sub(37)..=detail.len())
                                .find(|&i| detail.is_char_boundary(i))
                                .unwrap_or(detail.len());
                            format!("...{}", &detail[start..])
                        }
                    } else {
                        detail.to_string()
                    };
                    parts.push(truncated);
                }
            }
        }
        "life" => {
            parts.push(
                data.get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
            );
            if let Some(by) = data.get("by").and_then(|v| v.as_str())
                && !by.is_empty()
            {
                parts.push(format!("by:{}", by));
            }
        }
        _ => {}
    }

    parts.join(" | ")
}

fn find_collision_partner(
    db: &HcomDb,
    event_id: i64,
    instance: &str,
    file_path: &str,
) -> Option<String> {
    db.conn
        .query_row(
            &format!(
                "SELECT e.instance FROM events_v e
                 WHERE e.type = 'status' AND e.status_context IN {}
                 AND e.status_detail = ?
                 AND e.instance != ?
                 AND EXISTS (
                     SELECT 1 FROM events_v ev WHERE ev.id = ?
                     AND ABS(strftime('%s', ev.timestamp) - strftime('%s', e.timestamp)) < 30
                 )
                 ORDER BY e.id DESC LIMIT 1",
                FILE_WRITE_CONTEXTS
            ),
            params![file_path, instance, event_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

fn send_sub_notification(db: &HcomDb, caller: &str, message: &str) -> bool {
    let row: Option<(String, Option<String>)> = db
        .conn
        .query_row(
            "SELECT name, tag FROM instances WHERE name = ?",
            params![caller],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .ok();

    let Some((name, tag)) = row else {
        return false;
    };

    let full_name = match tag.filter(|t| !t.is_empty()) {
        Some(t) => format!("{}-{}", t, name),
        None => name,
    };

    let text = format!("@{} {}", full_name, message);
    let Ok(delivered_to) = send_system_message(db, "[hcom-events]", &text) else {
        return false;
    };
    // send_system_message only logs the [hcom-events] row; unlike `hcom send`
    // it does not ping notify endpoints, so the notification can sit unread
    // until an unrelated wake. Wake only the matching caller here: this path
    // runs inline from log_event, so avoid a broader wake_all fan-out.
    if delivered_to.iter().any(|recipient| recipient == caller) {
        crate::notify::wake(db, caller, &[]);
    }
    true
}

/// SHA-256 hex hash.
fn sha256_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod tests;
