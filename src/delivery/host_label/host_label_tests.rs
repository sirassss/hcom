use super::*;
use crate::shared::{ST_ACTIVE, ST_BLOCKED, ST_INACTIVE, ST_LAUNCHING, ST_LISTENING};
use serial_test::serial;

#[test]
fn map_report_state_covers_hcom_statuses() {
    assert_eq!(map_report_state(ST_LISTENING), "idle");
    assert_eq!(map_report_state(ST_ACTIVE), "working");
    assert_eq!(map_report_state(ST_BLOCKED), "blocked");
    assert_eq!(map_report_state(ST_INACTIVE), "unknown");
    assert_eq!(map_report_state(ST_LAUNCHING), "unknown");
}

#[test]
#[serial]
fn pane_title_label_skips_when_tool_empty() {
    let (_dir, _hcom_dir, _home, _guard) = crate::hooks::test_helpers::isolated_test_env();
    let db = crate::db::HcomDb::open().unwrap();

    assert_eq!(pane_title_label(&db, "luna", ST_LISTENING, ""), "");
}

#[test]
#[serial]
fn resolve_does_not_seed_last_pushed_from_pane_title_env() {
    // The built-in herdr preset opens the pane via `tab create --label
    // {instance_name}`, so herdr's initial tab label is the bare name
    // (e.g. `luna`). Seeding `last_pushed` from HCOM_PANE_TITLE would
    // silently swallow the first push and leave the pane stuck on
    // `luna` until the next status transition.
    // SAFETY: test is #[serial].
    unsafe {
        std::env::set_var("HCOM_PANE_TITLE", "\u{25c9} luna [claude]");
    }
    let label = HostLabel::resolve();
    // SAFETY: clear before assert so a panic doesn't leak env.
    unsafe {
        std::env::remove_var("HCOM_PANE_TITLE");
    }
    assert!(
        label.last_pushed.is_none(),
        "last_pushed must start unset so the first delivery-loop \
         iteration always pushes a styled label"
    );
}

#[cfg(unix)]
#[test]
fn classify_response_distinguishes_error_success_and_closed() {
    // A JSON `error` envelope is a semantic rejection (herdr alive but
    // said no) — keep the backend, retry the op (issue #102, F1/F2).
    let err = r#"{"id":"hcom:agent:rename","error":{"code":"not_agent","message":"pane w1:p1 is not an agent"}}"#;
    match classify_response(err) {
        Err(SocketError::Rejected(msg)) => assert!(msg.contains("not an agent")),
        _ => panic!("expected Rejected for an error envelope"),
    }

    // A `result` envelope is success — the op applied.
    let ok = r#"{"id":"hcom:agent:rename","result":{"type":"agent_renamed"}}"#;
    assert!(classify_response(ok).is_ok());

    // An ignored `report_agent` still comes back as a success envelope,
    // so it must classify as Ok (we can't detect the shadowing — F2).
    let ignored = r#"{"id":"hcom:pane:report_agent","result":{"type":"agent_reported"}}"#;
    assert!(classify_response(ignored).is_ok());

    // A non-empty but unparseable line: herdr answered, so treat as Ok
    // rather than wedging retries on a body we don't read.
    assert!(classify_response("not json at all\n").is_ok());

    // An empty line means herdr closed the connection without a reply.
    assert!(matches!(
        classify_response("   \n"),
        Err(SocketError::Unreachable(_))
    ));
}
