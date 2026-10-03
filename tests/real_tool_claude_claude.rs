//! Cross-agent scenario: one real Claude Code TUI messages another and gets a
//! reply (A -> B -> A) through hcom.
//!
//! Run explicitly with:
//!   cargo test --test real_tool_claude_claude -- --ignored --nocapture --test-threads=1
//!
//! Both Claudes talk to one localhost Anthropic Messages mock that scripts the
//! turns by token: A is prompted with a trigger and runs `hcom send @B`; B is
//! only ever woken by hcom's delivery, so the mock seeing B's token in B's user
//! turn is the real CLI proving it received the message; B answers with
//! `hcom send @A`, and A's mock turn proves the reply came back.

mod support;

use serial_test::serial;
use support::Hcom;
use support::claude_mock::{ClaudeCase, claude_text, claude_tool_use, latest_user_turn};
use support::duo::{
    assert_inbox_drained, delivered_message, launch, name_of, set_name, shared_name,
    wait_pty_proxy_up,
};
use support::mock_http::{MockHttp, Reply};
use support::real_tool::{ToolCase, inject_prompt_until, require_pinned};
use support::unique_suffix;

const SEND_AB: &str = "toolu_a_to_b";
const SEND_BA: &str = "toolu_b_to_a";

#[test]
#[ignore = "requires the pinned real @anthropic-ai/claude-code binary"]
#[serial]
fn real_claude_messages_real_claude_and_gets_a_reply() {
    let h = Hcom::new();
    let claude = ClaudeCase;
    require_pinned(&h, &claude);

    let suffix = unique_suffix();
    let trigger = format!("HCOM_TRIGGER_{suffix}");
    let token_ab = format!("HCOM_A_TO_B_{suffix}");
    let token_ba = format!("HCOM_B_TO_A_{suffix}");

    let (a_name, b_name) = (shared_name(), shared_name());
    let hcom_cmd = h.shell_hcom_command();
    let mock = {
        let (trigger, token_ab, token_ba) = (trigger.clone(), token_ab.clone(), token_ba.clone());
        let (a_name, b_name) = (a_name.clone(), b_name.clone());
        MockHttp::start(move |req| {
            if req.method.eq_ignore_ascii_case("HEAD") {
                return Reply::Empty(200);
            }
            if req.path.contains("count_tokens") {
                return Reply::Json(serde_json::json!({"input_tokens": 1}).to_string());
            }
            if !req.path.contains("/v1/messages") {
                return Reply::Status(404);
            }
            let send = |id: &str, to: String, token: &str| {
                Reply::Sse(claude_tool_use(
                    "msg_send",
                    id,
                    "Bash",
                    &serde_json::json!({
                        "command": format!("{hcom_cmd} send @{to} --intent inform -- {token}"),
                        "description": "message the other claude",
                    }),
                ))
            };
            let (tool_result, text) = latest_user_turn(&req.body).unwrap_or((None, String::new()));
            match tool_result.as_deref() {
                Some(SEND_AB) => Reply::Sse(claude_text("msg_sent_ab", "SENT_A_TO_B")),
                Some(SEND_BA) => Reply::Sse(claude_text("msg_sent_ba", "SENT_B_TO_A")),
                Some(_) => Reply::Status(500),
                None if text.contains(&trigger) => send(SEND_AB, name_of(&b_name), &token_ab),
                None if text.contains(&token_ba) => Reply::Sse(claude_text(
                    "msg_round_trip",
                    &format!("ROUND_TRIP {token_ba}"),
                )),
                None if text.contains(&token_ab) => send(SEND_BA, name_of(&a_name), &token_ba),
                None => Reply::Status(500),
            }
        })
        .expect("start claude mock")
    };
    claude.prepare(&h, &claude.provider_base_url(mock.port()));

    let a = launch(&h, "claude", &claude.launch_args(&h));
    set_name(&a_name, &a);
    wait_pty_proxy_up(&h, &a, "Claude A PTY proxy up");
    claude.drive_startup(&h, &a);
    let b = launch(&h, "claude", &claude.launch_args(&h));
    set_name(&b_name, &b);
    wait_pty_proxy_up(&h, &b, "Claude B PTY proxy up");
    claude.drive_startup(&h, &b);
    assert_ne!(a, b, "two launches must be two instances");

    // The user turns the mock saw whose newest text carries `needle`.
    let user_turns_with = |needle: &str| -> Vec<String> {
        mock.requests()
            .into_iter()
            .filter(|req| req.path.contains("/v1/messages") && !req.path.contains("count_tokens"))
            .filter(
                |req| matches!(latest_user_turn(&req.body), Some((None, t)) if t.contains(needle)),
            )
            .map(|req| req.body)
            .collect()
    };
    let tool_result_seen = |id: &str| {
        mock.requests().iter().any(|req| {
            req.path.contains("/v1/messages")
                && matches!(latest_user_turn(&req.body), Some((Some(t), _)) if t == id)
        })
    };

    inject_prompt_until(
        &h,
        &a,
        &format!("Message the other claude {trigger}"),
        "claude A prompt",
        || !user_turns_with(&trigger).is_empty(),
        || tool_result_seen(SEND_AB),
    );

    // A -> B: hcom recorded it, and B's own model turn carries it in hcom's envelope.
    let sent = delivered_message(&h, &a, &b, &token_ab);
    assert_eq!(sent["data"]["from"].as_str(), Some(a.as_str()));
    h.eventually(
        "B model turn carries A's message",
        std::time::Duration::from_secs(60),
        || Ok((!user_turns_with(&token_ab).is_empty()).then_some(())),
    );
    let b_turns = user_turns_with(&token_ab);
    assert_eq!(
        b_turns.len(),
        1,
        "A's message must produce exactly one model turn in B"
    );
    assert!(
        b_turns[0].contains("<hcom>") && b_turns[0].contains(&a),
        "B's request lacks the hcom delivery envelope from {a}"
    );

    // B -> A: the reply, and A's model turn carries it.
    delivered_message(&h, &b, &a, &token_ba);
    h.eventually(
        "A model turn carries B's reply",
        std::time::Duration::from_secs(60),
        || Ok((!user_turns_with(&token_ba).is_empty()).then_some(())),
    );
    let a_turns = user_turns_with(&token_ba);
    assert_eq!(
        a_turns.len(),
        1,
        "B's reply must produce exactly one model turn in A"
    );
    assert!(
        a_turns[0].contains("<hcom>") && a_turns[0].contains(&b),
        "A's request lacks the hcom delivery envelope from {b}"
    );

    assert_inbox_drained(&h, &a);
    assert_inbox_drained(&h, &b);
    let (code, _, stderr) = h.run(["kill", &a, &b]);
    assert_eq!(code, 0, "cleanup kill failed: {stderr}");
}
