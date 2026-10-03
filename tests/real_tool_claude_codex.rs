//! Cross-agent scenario: a real Claude Code TUI messages a real Codex TUI and
//! Codex replies (Claude -> Codex -> Claude) through hcom.
//!
//! Run explicitly with:
//!   cargo test --test real_tool_claude_codex -- --ignored --nocapture --test-threads=1
//!
//! Claude runs against a localhost Anthropic Messages mock and Codex against a
//! localhost OpenAI Responses mock ([`support::codex_mock`]); neither needs an
//! account. Claude is prompted with a trigger and runs `hcom send @codex`.
//! Codex is only ever woken by hcom's delivery, so its mock seeing the token in
//! Codex's own request is the real CLI proving it received the message; Codex
//! answers by running `hcom send @claude` through its shell tool, and Claude's
//! mock turn proves the reply came back.

mod support;

use serde_json::json;
use serial_test::serial;
use std::time::Duration;
use support::Hcom;
use support::claude_mock::{ClaudeCase, claude_text, claude_tool_use, latest_user_turn};
use support::codex_mock::{CodexCase, MockResponses, completed, created, message, shell_call, sse};
use support::duo::{
    assert_inbox_drained, delivered_message, launch, name_of, set_name, shared_name,
    wait_pty_proxy_up,
};
use support::mock_http::{MockHttp, Reply};
use support::real_tool::{ToolCase, inject_prompt_until, require_pinned, wait_pty_ready};
use support::unique_suffix;

const SEND_TO_CODEX: &str = "toolu_claude_to_codex";
const REPLY_CALL: &str = "CALL_CODEX_REPLY";

#[test]
#[ignore = "requires the pinned real claude and codex binaries"]
#[serial]
fn real_claude_messages_real_codex_and_gets_a_reply() {
    let h = Hcom::new();
    let claude = ClaudeCase;
    let codex = CodexCase;
    require_pinned(&h, &claude);
    require_pinned(&h, &codex);

    let suffix = unique_suffix();
    let trigger = format!("HCOM_TRIGGER_{suffix}");
    let token_out = format!("HCOM_CLAUDE_TO_CODEX_{suffix}");
    let token_back = format!("HCOM_CODEX_TO_CLAUDE_{suffix}");

    let (claude_name, codex_name) = (shared_name(), shared_name());
    let hcom_cmd = h.shell_hcom_command();

    let codex_mock = {
        let (token_out, token_back) = (token_out.clone(), token_back.clone());
        let (claude_name, hcom_cmd) = (claude_name.clone(), hcom_cmd.clone());
        MockResponses::start(move |body: &str| {
            if body.contains("function_call_output") && body.contains(REPLY_CALL) {
                Reply::Sse(sse(&[
                    created("RESP_DONE"),
                    message("ITEM_DONE", "SENT_CODEX_REPLY"),
                    completed("RESP_DONE"),
                ]))
            } else if body.contains(&token_out) {
                let cmd = format!(
                    "{hcom_cmd} send @{} --intent inform -- {token_back}",
                    name_of(&claude_name)
                );
                Reply::Sse(sse(&[
                    created("RESP_REPLY"),
                    shell_call(REPLY_CALL, &cmd),
                    completed("RESP_REPLY"),
                ]))
            } else {
                Reply::Status(500)
            }
        })
        .expect("start codex mock")
    };

    let claude_mock = {
        let (trigger, token_out, token_back) =
            (trigger.clone(), token_out.clone(), token_back.clone());
        let codex_name = codex_name.clone();
        MockHttp::start(move |req| {
            if req.method.eq_ignore_ascii_case("HEAD") {
                return Reply::Empty(200);
            }
            if req.path.contains("count_tokens") {
                return Reply::Json(json!({"input_tokens": 1}).to_string());
            }
            if !req.path.contains("/v1/messages") {
                return Reply::Status(404);
            }
            let (tool_result, text) = latest_user_turn(&req.body).unwrap_or((None, String::new()));
            match tool_result.as_deref() {
                Some(SEND_TO_CODEX) => Reply::Sse(claude_text("msg_sent", "SENT_TO_CODEX")),
                Some(_) => Reply::Status(500),
                None if text.contains(&trigger) => Reply::Sse(claude_tool_use(
                    "msg_send",
                    SEND_TO_CODEX,
                    "Bash",
                    &json!({
                        "command": format!(
                            "{hcom_cmd} send @{} --intent inform -- {token_out}",
                            name_of(&codex_name)
                        ),
                        "description": "message codex",
                    }),
                )),
                None if text.contains(&token_back) => Reply::Sse(claude_text(
                    "msg_round_trip",
                    &format!("ROUND_TRIP {token_back}"),
                )),
                None => Reply::Status(500),
            }
        })
        .expect("start claude mock")
    };

    claude.prepare(&h, &claude.provider_base_url(claude_mock.port()));
    codex.prepare(&h, &codex_mock.base_url());

    // Codex first: it must be idle and bound before the message reaches it.
    let codex_id = launch(&h, "codex", &codex.launch_args(&h));
    set_name(&codex_name, &codex_id);
    wait_pty_proxy_up(&h, &codex_id, "Codex PTY proxy up");
    wait_pty_ready(&h, &codex_id, "Codex idle at its prompt");
    let claude_id = launch(&h, "claude", &claude.launch_args(&h));
    set_name(&claude_name, &claude_id);
    wait_pty_proxy_up(&h, &claude_id, "Claude PTY proxy up");
    claude.drive_startup(&h, &claude_id);

    let claude_turns_with = |needle: &str| {
        claude_mock
            .requests()
            .into_iter()
            .filter(|req| req.path.contains("/v1/messages") && !req.path.contains("count_tokens"))
            .filter(
                |req| matches!(latest_user_turn(&req.body), Some((None, t)) if t.contains(needle)),
            )
            .count()
    };
    let claude_tool_result_seen = |id: &str| {
        claude_mock.requests().iter().any(|req| {
            req.path.contains("/v1/messages")
                && matches!(latest_user_turn(&req.body), Some((Some(t), _)) if t == id)
        })
    };

    inject_prompt_until(
        &h,
        &claude_id,
        &format!("Message the codex agent {trigger}"),
        "claude prompt",
        || claude_turns_with(&trigger) > 0,
        || claude_tool_result_seen(SEND_TO_CODEX),
    );

    // Claude -> Codex: recorded by hcom, and Codex's own first request carries it
    // inside hcom's delivery envelope (the following request is the tool follow-up).
    delivered_message(&h, &claude_id, &codex_id, &token_out);
    h.eventually(
        "Codex model turn carries Claude's message",
        Duration::from_secs(60),
        || {
            Ok(codex_mock
                .requests()
                .iter()
                .any(|body| body.contains(&token_out))
                .then_some(()))
        },
    );
    let codex_first_turns: Vec<String> = codex_mock
        .requests()
        .into_iter()
        .filter(|body| body.contains(&token_out) && !body.contains("function_call_output"))
        .collect();
    assert_eq!(
        codex_first_turns.len(),
        1,
        "Claude's message must produce exactly one fresh model turn in Codex"
    );
    assert!(
        codex_first_turns[0].contains("<hcom>") && codex_first_turns[0].contains(&claude_id),
        "Codex's request lacks the hcom delivery envelope from {claude_id}"
    );

    // Codex -> Claude: the reply, and Claude's model turn carries it.
    delivered_message(&h, &codex_id, &claude_id, &token_back);
    h.eventually(
        "Claude model turn carries Codex's reply",
        Duration::from_secs(60),
        || Ok((claude_turns_with(&token_back) > 0).then_some(())),
    );
    assert_eq!(
        claude_turns_with(&token_back),
        1,
        "Codex's reply must produce exactly one model turn in Claude"
    );

    assert_inbox_drained(&h, &codex_id);
    assert_inbox_drained(&h, &claude_id);
    assert!(
        codex_mock.unexpected().is_empty(),
        "Codex sent requests the mock rejected: {:?}",
        codex_mock.unexpected()
    );
    let (code, _, stderr) = h.run(["kill", &codex_id, &claude_id]);
    assert_eq!(code, 0, "cleanup kill failed: {stderr}");
}
