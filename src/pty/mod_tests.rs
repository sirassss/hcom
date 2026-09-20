use super::{
    PtyTarget, initialize_delivery_components, prompt_submit_observed, strip_focus_events,
};
use anyhow::anyhow;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn adhoc_pty_target_stays_adhoc_for_delivery() {
    let target = PtyTarget::AdhocCommand("bash".to_string());
    assert_eq!(target.name(), "bash");
    assert_eq!(target.known_tool(), None);
    assert_eq!(target.delivery_tool(), crate::tool::Tool::Adhoc);
}

fn setup_test_db(with_notify_endpoints: bool) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let temp_dir = std::env::temp_dir();
    let test_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let db_path = temp_dir.join(format!(
        "test_hcom_pty_{}_{}.db",
        std::process::id(),
        test_id
    ));

    if with_notify_endpoints {
        crate::db::HcomDb::open_at(&db_path).unwrap();
    } else {
        let _ = Connection::open(&db_path).unwrap();
    }

    db_path
}

fn cleanup_test_db(path: PathBuf) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn prompt_submit_observed_when_text_clears() {
    assert!(prompt_submit_observed(Some("run tests"), Some("")));
}

#[test]
fn strip_focus_events_removes_focus_in_and_out() {
    assert_eq!(strip_focus_events(b"\x1b[O").unwrap(), b"");
    assert_eq!(strip_focus_events(b"\x1b[I").unwrap(), b"");
    // Embedded between real keystrokes.
    assert_eq!(strip_focus_events(b"ab\x1b[Ocd").unwrap(), b"abcd");
}

#[test]
fn strip_focus_events_passes_through_non_focus_input() {
    // No ESC at all: nothing to strip (fast path returns None).
    assert!(strip_focus_events(b"hello\r").is_none());
    // Other escape sequences (arrow up = CSI A) must be preserved untouched.
    assert!(strip_focus_events(b"\x1b[A").is_none());
    // A trailing partial CSI is left intact (continuation handled by next read).
    assert!(strip_focus_events(b"\x1b[").is_none());
}

#[test]
fn prompt_submit_observed_when_text_temporarily_undetected() {
    assert!(prompt_submit_observed(Some("run tests"), None));
}

#[test]
fn prompt_submit_observed_ignores_startup_empty_edge() {
    assert!(!prompt_submit_observed(None, Some("")));
    assert!(!prompt_submit_observed(Some(""), Some("")));
}

#[test]
fn prompt_submit_observed_ignores_text_edits() {
    assert!(!prompt_submit_observed(Some("run"), Some("run tests")));
}

#[test]
fn initialize_delivery_components_db_failure_short_circuits_notify() {
    let notify_called = std::cell::Cell::new(false);

    let result = initialize_delivery_components(
        "test",
        || Err(anyhow!("DB connection refused")),
        || {
            notify_called.set(true);
            crate::notify::NotifyServer::new()
        },
    );

    let err = match result {
        Ok(_) => panic!("db failure should propagate"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("Failed to open database"),
        "missing context: {err:#}"
    );
    assert!(
        !notify_called.get(),
        "notify factory should not be called after db failure"
    );
}

#[test]
fn initialize_delivery_components_notify_failure_propagates() {
    let db_path = setup_test_db(true);

    let result = initialize_delivery_components(
        "test",
        || crate::db::HcomDb::open_raw(&db_path),
        || Err(anyhow!("Port already in use")),
    );

    let err = match result {
        Ok(_) => panic!("notify failure should propagate"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("Failed to create notify server"),
        "missing context: {err:#}"
    );

    cleanup_test_db(db_path);
}

#[test]
fn initialize_delivery_components_register_failure_propagates() {
    let db_path = setup_test_db(false);

    let result = initialize_delivery_components(
        "test",
        || crate::db::HcomDb::open_raw(&db_path),
        crate::notify::NotifyServer::new,
    );

    let err = match result {
        Ok(_) => panic!("register notify port failure should propagate"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("Failed to register notify port"),
        "missing context: {err:#}"
    );

    cleanup_test_db(db_path);
}

#[test]
fn initialize_delivery_components_registers_notify_port() {
    let db_path = setup_test_db(true);

    let (db, notify) = initialize_delivery_components(
        "test",
        || crate::db::HcomDb::open_raw(&db_path),
        crate::notify::NotifyServer::new,
    )
    .expect("component init should succeed");
    let notify_port = notify.port();
    drop(db);
    drop(notify);

    let conn = Connection::open(&db_path).unwrap();
    let (kind, port): (String, i64) = conn
        .query_row(
            "SELECT kind, port FROM notify_endpoints WHERE instance = 'test'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "pty");
    assert_eq!(port, notify_port as i64);

    cleanup_test_db(db_path);
}

// ---- pending_utf8_bytes tests ----

use super::pending_utf8_bytes;

#[test]
fn test_pending_utf8_empty() {
    assert_eq!(pending_utf8_bytes(&[]), 0);
}

#[test]
fn test_pending_utf8_ascii_complete() {
    // ASCII text is always complete
    assert_eq!(pending_utf8_bytes(b"Hello world"), 0);
    assert_eq!(pending_utf8_bytes(b"x"), 0);
}

#[test]
fn test_pending_utf8_complete_2byte() {
    // é (U+00E9) = C3 A9 (complete 2-byte)
    assert_eq!(pending_utf8_bytes(&[0xC3, 0xA9]), 0);
}

#[test]
fn test_pending_utf8_incomplete_2byte() {
    // Leading byte of 2-byte sequence without continuation
    assert_eq!(pending_utf8_bytes(&[0xC3]), 1);
}

#[test]
fn test_pending_utf8_complete_3byte() {
    // ─ (U+2500) = E2 94 80 (complete 3-byte)
    assert_eq!(pending_utf8_bytes(&[0xE2, 0x94, 0x80]), 0);
}

#[test]
fn test_pending_utf8_incomplete_3byte_needs_2() {
    // E2 alone needs 2 more bytes
    assert_eq!(pending_utf8_bytes(&[0xE2]), 2);
}

#[test]
fn test_pending_utf8_incomplete_3byte_needs_1() {
    // E2 94 needs 1 more byte
    assert_eq!(pending_utf8_bytes(&[0xE2, 0x94]), 1);
}

#[test]
fn test_pending_utf8_complete_4byte() {
    // 😀 (U+1F600) = F0 9F 98 80 (complete 4-byte)
    assert_eq!(pending_utf8_bytes(&[0xF0, 0x9F, 0x98, 0x80]), 0);
}

#[test]
fn test_pending_utf8_incomplete_4byte_needs_3() {
    // F0 alone needs 3 more bytes
    assert_eq!(pending_utf8_bytes(&[0xF0]), 3);
}

#[test]
fn test_pending_utf8_incomplete_4byte_needs_2() {
    // F0 9F needs 2 more bytes
    assert_eq!(pending_utf8_bytes(&[0xF0, 0x9F]), 2);
}

#[test]
fn test_pending_utf8_incomplete_4byte_needs_1() {
    // F0 9F 98 needs 1 more byte
    assert_eq!(pending_utf8_bytes(&[0xF0, 0x9F, 0x98]), 1);
}

#[test]
fn test_pending_utf8_mixed_content_complete() {
    // "text─more" = complete (box drawing char is complete)
    let data = b"text\xe2\x94\x80more";
    assert_eq!(pending_utf8_bytes(data), 0);
}

#[test]
fn test_pending_utf8_mixed_content_incomplete() {
    // "text" + first 2 bytes of ─
    let data = b"text\xe2\x94";
    assert_eq!(pending_utf8_bytes(data), 1);
}

#[test]
fn test_pending_utf8_line_of_box_drawing_incomplete() {
    // Multiple complete ─ chars followed by incomplete start
    // ─────\xe2 (5 complete + 1 incomplete start)
    let mut data = Vec::new();
    for _ in 0..5 {
        data.extend_from_slice(&[0xE2, 0x94, 0x80]); // ─
    }
    data.push(0xE2); // Start of next ─
    assert_eq!(pending_utf8_bytes(&data), 2);
}

// ---- title_write_safe tests ----

use super::{PendingEscape, has_pending_escape, resolve_pending_escape, title_write_safe};

#[test]
fn test_title_write_allowed_during_clean_output() {
    // A continuously-rendering tool (pi) only ever yields clean-boundary
    // writes; the title must be writable on those, not gated on a quiet
    // iteration. Clean boundary == no pending utf8/escape.
    assert!(title_write_safe(0, PendingEscape::None));
}

#[test]
fn test_title_write_blocked_by_pending_utf8() {
    assert!(!title_write_safe(1, PendingEscape::None));
}

#[test]
fn test_title_write_blocked_by_pending_csi() {
    assert!(!title_write_safe(0, PendingEscape::Csi));
}

#[test]
fn test_title_write_blocked_by_pending_string_seq() {
    assert!(!title_write_safe(0, PendingEscape::StringSeq));
}

#[test]
fn test_title_write_blocked_by_pending_single_shift() {
    assert!(!title_write_safe(0, PendingEscape::SingleShift));
}

#[test]
fn test_title_write_blocked_by_pending_nf_seq() {
    assert!(!title_write_safe(0, PendingEscape::NfSeq));
}

#[test]
fn test_title_write_blocked_by_multiple_conditions() {
    assert!(!title_write_safe(2, PendingEscape::Csi));
}

// ---- has_pending_escape tests ----

#[test]
fn test_pending_escape_empty() {
    assert_eq!(has_pending_escape(&[]), PendingEscape::None);
}

#[test]
fn test_pending_escape_plain_text() {
    assert_eq!(has_pending_escape(b"Hello world"), PendingEscape::None);
}

#[test]
fn test_pending_escape_complete_csi() {
    assert_eq!(has_pending_escape(b"\x1b[38;2;100m"), PendingEscape::None);
}

#[test]
fn test_pending_escape_incomplete_csi() {
    assert_eq!(has_pending_escape(b"\x1b[38;2;"), PendingEscape::Csi);
}

#[test]
fn test_pending_escape_bare_esc() {
    assert_eq!(has_pending_escape(b"text\x1b"), PendingEscape::Csi);
}

#[test]
fn test_pending_escape_complete_osc_bel() {
    assert_eq!(
        has_pending_escape(b"\x1b]8;id=link;https://example.com\x07"),
        PendingEscape::None
    );
}

#[test]
fn test_pending_escape_incomplete_osc() {
    assert_eq!(
        has_pending_escape(b"\x1b]8;id=link;https://example.com"),
        PendingEscape::StringSeq
    );
}

#[test]
fn test_pending_escape_complete_osc_st() {
    assert_eq!(
        has_pending_escape(b"\x1b]8;id=link;https://example.com\x1b\\"),
        PendingEscape::None
    );
}

#[test]
fn test_pending_escape_simple_two_byte() {
    assert_eq!(has_pending_escape(b"\x1bM"), PendingEscape::None);
}

#[test]
fn test_pending_escape_after_complete_sequence() {
    assert_eq!(
        has_pending_escape(b"\x1b[38;2;100mhello"),
        PendingEscape::None
    );
}

#[test]
fn test_pending_escape_incomplete_dcs() {
    assert_eq!(
        has_pending_escape(b"\x1bPsome data"),
        PendingEscape::StringSeq
    );
}

#[test]
fn test_pending_escape_complete_dcs() {
    assert_eq!(
        has_pending_escape(b"\x1bPsome data\x1b\\"),
        PendingEscape::None
    );
}

#[test]
fn test_pending_escape_incomplete_single_shift() {
    // SS2 (ESC N) / SS3 (ESC O) with no following byte yet
    assert_eq!(has_pending_escape(b"text\x1bN"), PendingEscape::SingleShift);
    assert_eq!(has_pending_escape(b"text\x1bO"), PendingEscape::SingleShift);
}

#[test]
fn test_pending_escape_complete_single_shift() {
    // The shifted character completes the sequence
    assert_eq!(has_pending_escape(b"\x1bNx"), PendingEscape::None);
    assert_eq!(has_pending_escape(b"\x1bOx"), PendingEscape::None);
}

#[test]
fn test_pending_escape_incomplete_nf() {
    // nF charset designation mid-sequence (intermediate, no final yet)
    assert_eq!(has_pending_escape(b"text\x1b("), PendingEscape::NfSeq);
    assert_eq!(has_pending_escape(b"text\x1b#"), PendingEscape::NfSeq);
}

#[test]
fn test_pending_escape_complete_nf() {
    // ESC ( B (designate ASCII to G0), ESC # 8 (DEC alignment test)
    assert_eq!(has_pending_escape(b"\x1b(B"), PendingEscape::None);
    assert_eq!(has_pending_escape(b"\x1b#8"), PendingEscape::None);
}

// ---- resolve_pending_escape (cross-chunk) tests ----

#[test]
fn test_resolve_csi_continuation_no_final() {
    // CSI params without final byte — stays pending
    assert_eq!(
        resolve_pending_escape(PendingEscape::Csi, b"100;50;"),
        PendingEscape::Csi
    );
}

#[test]
fn test_resolve_csi_continuation_with_final() {
    // CSI terminated by 'm' (0x6D)
    assert_eq!(
        resolve_pending_escape(PendingEscape::Csi, b"200m"),
        PendingEscape::None
    );
}

#[test]
fn test_resolve_csi_continuation_final_mid_chunk() {
    // Final byte followed by normal text
    assert_eq!(
        resolve_pending_escape(PendingEscape::Csi, b"200mHello world"),
        PendingEscape::None
    );
}

#[test]
fn test_resolve_string_seq_continuation_no_terminator() {
    // OSC URL continuation without BEL — stays pending
    assert_eq!(
        resolve_pending_escape(PendingEscape::StringSeq, b"ample.com/path"),
        PendingEscape::StringSeq
    );
}

#[test]
fn test_resolve_string_seq_continuation_with_bel() {
    // OSC terminated by BEL
    assert_eq!(
        resolve_pending_escape(PendingEscape::StringSeq, b"url\x07rest"),
        PendingEscape::None
    );
}

#[test]
fn test_resolve_none_stays_none() {
    assert_eq!(
        resolve_pending_escape(PendingEscape::None, b"any data"),
        PendingEscape::None
    );
}

#[test]
fn test_resolve_string_seq_letters_dont_clear() {
    // Letters in OSC content (e.g., URL) must NOT clear StringSeq —
    // only BEL or ST terminates. (Letters would falsely clear CSI.)
    assert_eq!(
        resolve_pending_escape(PendingEscape::StringSeq, b"https://example"),
        PendingEscape::StringSeq
    );
}

#[test]
fn test_resolve_single_shift_completes_on_any_byte() {
    // The shifted char arrives in the next chunk (split between ESC N and char)
    assert_eq!(
        resolve_pending_escape(PendingEscape::SingleShift, b"xrest"),
        PendingEscape::None
    );
    // Empty continuation keeps it pending
    assert_eq!(
        resolve_pending_escape(PendingEscape::SingleShift, b""),
        PendingEscape::SingleShift
    );
}

#[test]
fn test_resolve_nf_continuation() {
    // Intermediates only — stays pending
    assert_eq!(
        resolve_pending_escape(PendingEscape::NfSeq, b"  "),
        PendingEscape::NfSeq
    );
    // Final byte (0x30-0x7E) completes it
    assert_eq!(
        resolve_pending_escape(PendingEscape::NfSeq, b"B"),
        PendingEscape::None
    );
}

#[test]
fn test_three_way_csi_split() {
    // Simulate the exact 3-way split bug: ESC[38;2; | 100;50; | 200m
    let chunk1 = b"\x1b[38;2;";
    let chunk2 = b"100;50;";
    let chunk3 = b"200m";

    let state = has_pending_escape(chunk1);
    assert_eq!(state, PendingEscape::Csi);

    // Chunk 2 has no ESC — use resolve
    let state = resolve_pending_escape(state, chunk2);
    assert_eq!(
        state,
        PendingEscape::Csi,
        "must stay pending through middle chunk"
    );

    // Chunk 3 has no ESC — use resolve, 'm' terminates
    let state = resolve_pending_escape(state, chunk3);
    assert_eq!(state, PendingEscape::None);
}

#[test]
fn test_three_way_osc_split() {
    // OSC 8 hyperlink split: ESC]8;id=x; | https://long.url | .com/path BEL
    let chunk1 = b"\x1b]8;id=x;";
    let chunk2 = b"https://long.url";
    let chunk3 = b".com/path\x07";

    let state = has_pending_escape(chunk1);
    assert_eq!(state, PendingEscape::StringSeq);

    let state = resolve_pending_escape(state, chunk2);
    assert_eq!(
        state,
        PendingEscape::StringSeq,
        "URL letters must not terminate OSC"
    );

    let state = resolve_pending_escape(state, chunk3);
    assert_eq!(state, PendingEscape::None);
}
