use super::*;

#[test]
fn test_streamline_event_message() {
    let event = json!({
        "id": 1,
        "ts": "2025-02-23T15:30:45.123456",
        "type": "message",
        "instance": "luna",
        "data": {
            "from": "nova",
            "text": "hello",
            "sender_kind": "instance",
            "scope": "mentions",
            "delivered_to": ["luna"],
            "mentions": ["luna"],
            "reply_to": "42",
            "reply_to_local": 42,
        }
    });

    let filters = HashMap::new();
    let result = streamline_event(&event, &filters);

    let data = result.get("data").unwrap();
    assert!(data.get("sender_kind").is_none());
    assert!(data.get("scope").is_none());
    assert!(data.get("delivered_to").is_none());
    assert!(data.get("mentions").is_none());
    assert!(data.get("reply_to").is_none());
    assert!(data.get("reply_to_local").is_some());
    assert_eq!(result.get("ts").unwrap().as_str().unwrap().len(), 19);
}

#[test]
fn test_streamline_event_status() {
    let long_detail = "x".repeat(100);
    let event = json!({
        "id": 2,
        "ts": "2025-02-23T15:30:45",
        "type": "status",
        "instance": "luna",
        "data": {
            "detail": long_detail,
            "position": {"last_event_id": 42},
            "status": "active",
        }
    });

    let filters = HashMap::new();
    let result = streamline_event(&event, &filters);
    let data = result.get("data").unwrap();

    // Detail should be truncated
    let detail = data.get("detail").unwrap().as_str().unwrap();
    assert!(detail.len() <= 64); // 60 + "..."
    assert!(detail.ends_with("..."));

    // Position should be removed
    assert!(data.get("position").is_none());
}

#[test]
fn test_streamline_event_status_with_cmd_filter() {
    let long_detail = "x".repeat(100);
    let event = json!({
        "id": 2,
        "ts": "2025-02-23T15:30:45",
        "type": "status",
        "instance": "luna",
        "data": {
            "detail": long_detail,
        }
    });

    let mut filters = HashMap::new();
    filters.insert("cmd".to_string(), vec!["git".to_string()]);
    let result = streamline_event(&event, &filters);
    let data = result.get("data").unwrap();

    // Detail should NOT be truncated when --cmd filter active
    let detail = data.get("detail").unwrap().as_str().unwrap();
    assert_eq!(detail.len(), 100);
}

#[test]
fn test_streamline_event_life() {
    let event = json!({
        "id": 3,
        "ts": "2025-02-23T15:30:45",
        "type": "life",
        "instance": "luna",
        "data": {
            "action": "stopped",
            "snapshot": {"large": "nested", "object": true},
        }
    });

    let filters = HashMap::new();
    let result = streamline_event(&event, &filters);
    let data = result.get("data").unwrap();

    assert!(data.get("snapshot").is_none());
    assert!(data.get("action").is_some());
}

#[test]
fn test_streamline_preserves_mentions_with_filter() {
    let event = json!({
        "id": 1,
        "ts": "2025-02-23T15:30:45",
        "type": "message",
        "instance": "luna",
        "data": {
            "mentions": ["luna", "nova"],
        }
    });

    let mut filters = HashMap::new();
    filters.insert("mention".to_string(), vec!["luna".to_string()]);
    let result = streamline_event(&event, &filters);
    let data = result.get("data").unwrap();

    assert!(data.get("mentions").is_some());
}

#[test]
fn test_events_args_wait_with_value() {
    use clap::Parser;
    let args = EventsArgs::try_parse_from(["events", "--wait", "30", "--full"]).unwrap();
    assert_eq!(args.wait, Some(30));
    assert!(args.full);
}

#[test]
fn test_events_args_wait_no_value() {
    use clap::Parser;
    let args = EventsArgs::try_parse_from(["events", "--wait", "--full"]).unwrap();
    assert_eq!(args.wait, Some(60)); // default_missing_value
    assert!(args.full);
}

#[test]
fn test_events_args_no_wait() {
    use clap::Parser;
    let args = EventsArgs::try_parse_from(["events", "--full"]).unwrap();
    assert_eq!(args.wait, None);
    assert!(args.full);
}

#[test]
fn test_events_args_last() {
    use clap::Parser;
    let args = EventsArgs::try_parse_from(["events", "--last", "50"]).unwrap();
    assert_eq!(args.last, Some(50));
}

#[test]
fn test_events_args_with_filters() {
    use clap::Parser;
    let args =
        EventsArgs::try_parse_from(["events", "--agent", "peso", "--type", "message"]).unwrap();
    assert_eq!(args.filters.agent, vec!["peso"]);
    assert_eq!(args.filters.event_type, vec!["message"]);
    assert!(args.subcmd.is_none());
}

#[test]
fn test_events_sub_args() {
    use clap::Parser;
    let args = EventsArgs::try_parse_from(["events", "sub", "--agent", "peso", "--once"]).unwrap();
    match args.subcmd {
        Some(EventsSubcmd::Sub(ref sub)) => {
            assert!(sub.once);
            assert_eq!(sub.filters.agent, vec!["peso"]);
        }
        _ => panic!("Expected Sub subcommand"),
    }
}

#[test]
fn test_events_unsub_args() {
    use clap::Parser;
    let args = EventsArgs::try_parse_from(["events", "unsub", "sub-abc123"]).unwrap();
    match args.subcmd {
        Some(EventsSubcmd::Unsub(ref unsub)) => {
            assert_eq!(unsub.id, "sub-abc123");
        }
        _ => panic!("Expected Unsub subcommand"),
    }
}

#[test]
fn test_events_launch_args() {
    use clap::Parser;
    let args =
        EventsArgs::try_parse_from(["events", "launch", "batch1", "--timeout", "60"]).unwrap();
    match args.subcmd {
        Some(EventsSubcmd::Launch(ref launch)) => {
            assert_eq!(launch.batch_id, Some("batch1".to_string()));
            assert_eq!(launch.timeout, 60);
        }
        _ => panic!("Expected Launch subcommand"),
    }
}

const UNRELATED_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

struct WaiterFixture {
    _temp: tempfile::TempDir,
    db_path: std::path::PathBuf,
    writer: HcomDb,
}

impl WaiterFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("events_wait_test.db");
        let writer = HcomDb::open_raw(&db_path).unwrap();
        writer.init_db().unwrap();
        Self {
            _temp: temp,
            db_path,
            writer,
        }
    }

    fn open_reader(&self) -> HcomDb {
        HcomDb::open_raw(&self.db_path).unwrap()
    }

    fn register_instance(&self, name: &str) {
        self.writer
            .conn()
            .execute(
                "INSERT INTO instances (name, tool, status, created_at, last_event_id) VALUES (?1, 'test', 'active', 1000.0, 0)",
                rusqlite::params![name],
            )
            .unwrap();
    }

    fn wait_for_notify_endpoint(&self, name: &str) -> bool {
        for _ in 0..100 {
            if self.writer.has_notify_endpoint_kind(name, "events_wait") {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn send_message(&self, from: &str, text: &str) {
        let msg_data = json!({
            "from": from,
            "text": text,
            "scope": "broadcast",
        });
        self.writer
            .log_event_with_ts("message", from, &msg_data, None)
            .unwrap();
    }

    fn send_status(&self, instance: &str, status: &str, detail: &str) {
        let status_data = json!({
            "status": status,
            "detail": detail,
        });
        self.writer
            .log_event_with_ts("status", instance, &status_data, None)
            .unwrap();
    }

    fn wake(&self, instance: &str) {
        crate::notify::wake(
            &self.writer,
            instance,
            &[crate::notify::WakeKind::EventsWait],
        );
    }
}

struct WaiterWorker {
    handle: std::thread::JoinHandle<i32>,
    rx: std::sync::mpsc::Receiver<i32>,
}

impl WaiterWorker {
    fn spawn(
        reader: HcomDb,
        filter_query: &'static str,
        timeout_secs: u64,
        instance: &'static str,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let code = events_wait(
                &reader,
                filter_query,
                timeout_secs,
                false,
                &HashMap::new(),
                Some(instance),
            );
            let _ = tx.send(code);
            code
        });
        Self { handle, rx }
    }

    fn probe(&self, timeout: Duration) -> Result<i32, std::sync::mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    fn join(self) -> i32 {
        self.handle.join().unwrap_or(-1)
    }
}

#[test]
fn test_events_wait_unrelated_unread_arriving_after_readiness() {
    let f = WaiterFixture::new();
    f.register_instance("waiter_arriving");
    let initial_cursor = f.writer.get_cursor("waiter_arriving");

    let worker = WaiterWorker::spawn(
        f.open_reader(),
        " AND (type = 'status')",
        5,
        "waiter_arriving",
    );
    let ready = f.wait_for_notify_endpoint("waiter_arriving");

    f.send_message("sender", "unrelated arriving message");
    f.wake("waiter_arriving");

    let probe = worker.probe(UNRELATED_PROBE_TIMEOUT);
    if probe.is_err() {
        f.send_status("waiter_arriving", "active", "matched");
        f.wake("waiter_arriving");
    }
    let code = worker.join();
    let endpoint_cleaned = !f
        .writer
        .has_notify_endpoint_kind("waiter_arriving", "events_wait");
    let cursor_after = f.writer.get_cursor("waiter_arriving");

    assert_eq!(cursor_after, initial_cursor);
    assert!(
        endpoint_cleaned,
        "notify endpoint must be cleaned up on exit"
    );
    assert!(
        ready && probe.is_err() && code == 0,
        "waiter must remain pending on unrelated unread: ready={ready}, probe={probe:?}, code={code}"
    );
}

#[test]
fn test_events_wait_preexisting_unrelated_unread_does_not_satisfy_filter() {
    let f = WaiterFixture::new();
    f.register_instance("waiter_pre");
    f.send_message("sender", "preexisting unread message");
    let initial_cursor = f.writer.get_cursor("waiter_pre");

    let worker = WaiterWorker::spawn(f.open_reader(), " AND (type = 'status')", 1, "waiter_pre");

    let probe = worker.probe(UNRELATED_PROBE_TIMEOUT);
    let code = worker.join();
    let endpoint_cleaned = !f
        .writer
        .has_notify_endpoint_kind("waiter_pre", "events_wait");
    let cursor_after = f.writer.get_cursor("waiter_pre");

    assert_eq!(cursor_after, initial_cursor);
    assert!(
        endpoint_cleaned,
        "notify endpoint must be cleaned up on exit"
    );
    assert!(
        probe.is_err() && code == 1,
        "events_wait must not break with 0 on preexisting unread message: probe={probe:?}, code={code}"
    );
}

#[test]
fn test_events_wait_matching_status_after_readiness_exits_zero() {
    let f = WaiterFixture::new();
    f.register_instance("waiter_matching");
    let initial_cursor = f.writer.get_cursor("waiter_matching");

    let worker = WaiterWorker::spawn(
        f.open_reader(),
        " AND (type = 'status')",
        5,
        "waiter_matching",
    );
    let ready = f.wait_for_notify_endpoint("waiter_matching");

    f.send_status("waiter_matching", "active", "ready");
    f.wake("waiter_matching");

    let code = worker.join();
    let endpoint_cleaned = !f
        .writer
        .has_notify_endpoint_kind("waiter_matching", "events_wait");
    let cursor_after = f.writer.get_cursor("waiter_matching");

    assert!(ready, "endpoint must be registered");
    assert!(
        endpoint_cleaned,
        "notify endpoint must be cleaned up on exit"
    );
    assert_eq!(cursor_after, initial_cursor);
    assert_eq!(code, 0, "matching status event must wake and exit 0");
}

#[test]
fn test_events_wait_timeout_returns_one() {
    let f = WaiterFixture::new();
    f.register_instance("timeout_agent");
    let filters = HashMap::new();
    let reader = f.open_reader();
    let code = events_wait(
        &reader,
        " AND (type = 'nonexistent')",
        1,
        false,
        &filters,
        Some("timeout_agent"),
    );
    assert_eq!(code, 1);
    assert!(
        !f.writer
            .has_notify_endpoint_kind("timeout_agent", "events_wait")
    );
}

#[test]
fn test_events_wait_invalid_sql_returns_two() {
    let f = WaiterFixture::new();
    f.register_instance("sql_agent");
    let filters = HashMap::new();
    let reader = f.open_reader();
    let code = events_wait(
        &reader,
        " AND (invalid sql %%%)",
        1,
        false,
        &filters,
        Some("sql_agent"),
    );
    assert_eq!(code, 2);
    assert!(
        !f.writer
            .has_notify_endpoint_kind("sql_agent", "events_wait")
    );
}
