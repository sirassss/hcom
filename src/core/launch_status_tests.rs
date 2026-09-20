use super::*;
use tempfile::tempdir;

fn make_test_db() -> (HcomDb, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db = HcomDb::open_raw(&dir.path().join("test.db")).unwrap();
    db.init_db().unwrap();
    (db, dir)
}

#[test]
fn test_launch_status_as_str() {
    assert_eq!(LaunchStatus::Ready.as_str(), "ready");
    assert_eq!(LaunchStatus::Blocked.as_str(), "blocked");
    assert_eq!(LaunchStatus::Timeout.as_str(), "timeout");
    assert_eq!(LaunchStatus::Error.as_str(), "error");
    assert_eq!(LaunchStatus::NoLaunches.as_str(), "no_launches");
}

#[test]
fn test_launch_result_to_json_no_launches() {
    let result = LaunchResult {
        status: LaunchStatus::NoLaunches,
        expected: None,
        ready: None,
        failed: None,
        blocked: None,
        instances: vec![],
        failures: vec![],
        blockers: vec![],
        launcher: None,
        timestamp: None,
        batch_id: None,
        batches: None,
        hint: None,
        message: Some("No launches found".into()),
    };
    let json = result.to_json();
    assert_eq!(json["status"], "no_launches");
    assert_eq!(json["message"], "No launches found");
}

#[test]
fn test_launch_result_to_json_ready() {
    let result = LaunchResult {
        status: LaunchStatus::Ready,
        expected: Some(3),
        ready: Some(3),
        failed: Some(0),
        blocked: Some(0),
        instances: vec!["luna".into(), "nova".into(), "peso".into()],
        failures: vec![],
        blockers: vec![],
        launcher: Some("bigboss".into()),
        timestamp: Some("2024-01-01T00:00:00Z".into()),
        batch_id: Some("batch-123".into()),
        batches: None,
        hint: None,
        message: None,
    };
    let json = result.to_json();
    assert_eq!(json["status"], "ready");
    assert_eq!(json["expected"], 3);
    assert_eq!(json["ready"], 3);
    assert_eq!(json["instances"].as_array().unwrap().len(), 3);
}

#[test]
fn test_launch_result_to_json_timeout() {
    let result = LaunchResult {
        status: LaunchStatus::Timeout,
        expected: Some(3),
        ready: Some(1),
        failed: Some(0),
        blocked: Some(0),
        instances: vec!["luna".into()],
        failures: vec![],
        blockers: vec![],
        launcher: Some("bigboss".into()),
        timestamp: Some("2024-01-01T00:00:00Z".into()),
        batch_id: Some("batch-123".into()),
        batches: None,
        hint: Some("Launch failed: 1/3 ready after 30s (batch: batch-123). Check ~/.hcom/.tmp/logs/background_*.log or hcom list -v".into()),
        message: None,
    };
    let json = result.to_json();
    assert_eq!(json["status"], "timeout");
    assert_eq!(json["timed_out"], true);
    assert!(json["hint"].as_str().unwrap().contains("Launch failed"));
}

#[test]
fn test_wait_for_launch_returns_error_on_launch_failed_event() {
    let (db, _dir) = make_test_db();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-fail",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();
    db.log_event(
        "life",
        "mari",
        &serde_json::json!({
            "action": "launch_failed",
            "batch_id": "batch-fail",
            "reason": "ready_never_observed",
            "detail": "readiness was never observed"
        }),
    )
    .unwrap();

    let result = wait_for_launch(&db, None, Some("batch-fail"), 1);
    assert_eq!(result.status, LaunchStatus::Error);
    assert_eq!(result.ready, Some(0));
    assert_eq!(result.failed, Some(1));
    assert_eq!(
        result.failures,
        vec!["mari: readiness was never observed".to_string()]
    );
    let json = result.to_json();
    assert_eq!(json["status"], "error");
    assert!(json.get("timed_out").is_none());
}

#[test]
fn test_wait_for_launch_counts_status_context_launch_failed() {
    let (db, _dir) = make_test_db();

    let mut data = serde_json::Map::new();
    data.insert("status".into(), serde_json::json!("inactive"));
    data.insert("status_context".into(), serde_json::json!("launch_failed"));
    data.insert(
        "status_detail".into(),
        serde_json::json!("tool startup failed"),
    );
    data.insert(
        "created_at".into(),
        serde_json::json!(crate::shared::time::now_epoch_i64()),
    );
    db.save_instance_named("mari", &data).unwrap();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-row-fail",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();

    let result = wait_for_launch(&db, None, Some("batch-row-fail"), 1);
    assert_eq!(result.status, LaunchStatus::Error);
    assert_eq!(result.ready, Some(0));
    assert_eq!(result.failed, Some(1));
    assert_eq!(
        result.failures,
        vec!["mari: tool startup failed".to_string()]
    );
}

#[test]
fn test_wait_for_launch_returns_blocked_on_launch_blocked_event() {
    let (db, _dir) = make_test_db();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-blocked",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();
    db.log_event(
        "life",
        "mari",
        &serde_json::json!({
            "action": "launch_blocked",
            "batch_id": "batch-blocked",
            "detail": "launch blocked: run hcom term mari"
        }),
    )
    .unwrap();

    let result = wait_for_launch(&db, None, Some("batch-blocked"), 1);
    assert_eq!(result.status, LaunchStatus::Blocked);
    assert_eq!(result.ready, Some(0));
    assert_eq!(result.failed, Some(0));
    assert_eq!(result.blocked, Some(1));
    assert_eq!(
        result.blockers,
        vec!["mari: launch blocked: run hcom term mari".to_string()]
    );
    let json = result.to_json();
    assert_eq!(json["status"], "blocked");
    assert_eq!(json["blocked"], 1);
}

#[test]
fn test_wait_for_launch_ignores_stopped_after_ready() {
    let (db, _dir) = make_test_db();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-stopped",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();
    db.log_event(
        "life",
        "mari",
        &serde_json::json!({
            "action": "ready",
            "batch_id": "batch-stopped",
            "status": "listening",
            "context": "ready_observed"
        }),
    )
    .unwrap();
    db.log_event(
        "life",
        "mari",
        &serde_json::json!({
            "action": "stopped",
            "by": "pty",
            "reason": "closed"
        }),
    )
    .unwrap();

    let result = wait_for_launch(&db, None, Some("batch-stopped"), 1);
    assert_eq!(result.status, LaunchStatus::Ready);
    assert_eq!(result.ready, Some(1));
    assert_eq!(result.failed, Some(0));
    assert!(result.failures.is_empty());
}

#[test]
fn test_wait_for_launch_counts_stopped_before_ready_as_failed() {
    let (db, _dir) = make_test_db();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-stopped-before-ready",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();
    db.log_event(
        "life",
        "mari",
        &serde_json::json!({
            "action": "stopped",
            "by": "pty",
            "reason": "closed"
        }),
    )
    .unwrap();

    let result = wait_for_launch(&db, None, Some("batch-stopped-before-ready"), 1);
    assert_eq!(result.status, LaunchStatus::Error);
    assert_eq!(result.ready, Some(0));
    assert_eq!(result.failed, Some(1));
    assert_eq!(
        result.failures,
        vec!["mari: launch stopped before it remained ready: closed by pty".to_string()]
    );
}

#[test]
fn test_wait_for_launch_does_not_finalize_fresh_placeholder() {
    let (db, _dir) = make_test_db();

    let mut data = serde_json::Map::new();
    data.insert("status".into(), serde_json::json!("inactive"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert(
        "created_at".into(),
        serde_json::json!(crate::shared::time::now_epoch_i64()),
    );
    data.insert("status_time".into(), serde_json::json!(0));
    data.insert("tool".into(), serde_json::json!("claude"));
    db.save_instance_named("mari", &data).unwrap();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-fresh-placeholder",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();

    let result = wait_for_launch(&db, None, Some("batch-fresh-placeholder"), 1);
    assert_eq!(result.status, LaunchStatus::Timeout);
    assert_eq!(result.ready, Some(0));
    assert_eq!(result.failed, Some(0));
    assert!(result.failures.is_empty());

    let stored = db.get_instance_full("mari").unwrap().unwrap();
    assert_eq!(stored.status_context, "new");
}

#[test]
fn test_get_batch_failure_details_uses_batch_instances_and_status_detail() {
    let (db, _dir) = make_test_db();

    let mut data = serde_json::Map::new();
    data.insert("status".into(), serde_json::json!("inactive"));
    data.insert("status_context".into(), serde_json::json!("launch_failed"));
    data.insert(
        "created_at".into(),
        serde_json::json!(crate::shared::time::now_epoch_i64()),
    );
    data.insert(
        "status_detail".into(),
        serde_json::json!("Error: Operation not permitted (os error 1) Fully reset tmux first (`tmux kill-server`), then start a fresh tmux server with approval/escalation (for example: `tmux new-session -d -s hcom-external`), then retry."),
    );
    db.save_instance_named("mari", &data).unwrap();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-123",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();

    let details = get_batch_failure_details_for_ids(&db, &["batch-123".to_string()]);
    assert_eq!(
        details,
        vec!["mari: Error: Operation not permitted (os error 1) Fully reset tmux first (`tmux kill-server`), then start a fresh tmux server with approval/escalation (for example: `tmux new-session -d -s hcom-external`), then retry.".to_string()]
    );
}

#[test]
fn test_get_batch_failure_details_finalizes_new_instance() {
    let (db, _dir) = make_test_db();

    let mut data = serde_json::Map::new();
    data.insert("status".into(), serde_json::json!("inactive"));
    data.insert("status_context".into(), serde_json::json!("new"));
    data.insert(
        "created_at".into(),
        serde_json::json!(
            crate::shared::time::now_epoch_i64()
                - crate::instance_lifecycle::LAUNCH_PLACEHOLDER_TIMEOUT
                - 1
        ),
    );
    data.insert("tool".into(), serde_json::json!("codex"));
    data.insert(
        "launch_context".into(),
        serde_json::json!(r#"{"terminal_preset":"tmux","pane_id":""}"#),
    );
    db.save_instance_named("mari", &data).unwrap();

    db.log_event(
        "life",
        "leku",
        &serde_json::json!({
            "action": "batch_launched",
            "batch_id": "batch-456",
            "launched": 1,
            "instances": ["mari"]
        }),
    )
    .unwrap();

    let details = get_batch_failure_details_for_ids(&db, &["batch-456".to_string()]);
    assert_eq!(details.len(), 1);
    let detail = details[0]
        .strip_prefix("mari: exited before binding (observed after ")
        .and_then(|value| value.strip_suffix("s)"))
        .and_then(|value| value.parse::<i64>().ok())
        .expect("failure detail should include the observed age in seconds");
    assert!(detail > crate::instance_lifecycle::LAUNCH_PLACEHOLDER_TIMEOUT);

    let stored = db.get_instance_full("mari").unwrap().unwrap();
    assert_eq!(stored.status_context, "launch_failed");
    assert_eq!(
        stored.status_detail,
        details[0].strip_prefix("mari: ").unwrap()
    );
}
