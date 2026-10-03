//! Cross-agent scenario: a real Claude Code TUI messages a real Antigravity
//! (`agy`) TUI through hcom, over hcom's persistent Antigravity hooks.
//!
//! OPT-IN: `agy` has no mock backend (see [`support::agy_real`]), so this spends
//! a little real model quota and needs a Google login. Run it with:
//!   HCOM_RT_AGY_AUTH_DIR=<dir with antigravity-oauth-token + installation_id> \
//!     cargo test --test real_tool_claude_agy -- --ignored --nocapture --test-threads=1
//! or in a clean container: `scripts/tests/docker-real-tool.sh real_tool_claude_agy`.
//!
//! Claude runs a scripted turn that executes `hcom send @<agy>`. What is proved
//! is delivery, not the model's answer (a real model's reply is not scripted):
//! hcom records the message for agy, agy's own hooks acknowledge it
//! (`deliver:<sender>`), and agy's inbox drains.

mod support;

use serial_test::serial;
use std::time::Duration;
use support::Hcom;
use support::agy_real;
use support::claude_mock::{ClaudeCase, claude_text, claude_tool_use, latest_user_turn};
use support::duo::{assert_inbox_drained, delivered_message, launch, wait_pty_proxy_up};
use support::mock_http::{MockHttp, Reply};
use support::real_tool::{ToolCase, inject_prompt_until, require_pinned, wait_pty_ready};
use support::unique_suffix;

const SEND_TOOL: &str = "toolu_claude_to_agy";

#[test]
#[ignore = "opt-in: real agy, a Google login (HCOM_RT_AGY_AUTH_DIR) and real model quota"]
#[serial]
fn real_claude_message_is_delivered_to_real_agy() {
    let h = Hcom::new();
    let claude = ClaudeCase;
    require_pinned(&h, &claude);
    agy_real::require_pinned(&h);

    let suffix = unique_suffix();
    let token = format!("HCOM_CLAUDE_TO_AGY_{suffix}");
    let trigger = format!("HCOM_TRIGGER_{suffix}");

    agy_real::prepare(&h);
    // agy first: its name is the recipient in Claude's scripted command.
    let agy = launch(&h, "agy", &[]);
    wait_pty_proxy_up(&h, &agy, "agy PTY proxy up");
    wait_pty_ready(&h, &agy, "agy idle at its prompt");

    let send_cmd = format!(
        "{} send @{agy} --intent inform -- {token}",
        h.shell_hcom_command()
    );
    let claude_mock = {
        let trigger = trigger.clone();
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
            match latest_user_turn(&req.body).unwrap_or((None, String::new())) {
                (Some(id), _) if id == SEND_TOOL => {
                    Reply::Sse(claude_text("msg_sent", "SENT_TO_AGY"))
                }
                (Some(_), _) => Reply::Status(500),
                (None, text) if text.contains(&trigger) => Reply::Sse(claude_tool_use(
                    "msg_send",
                    SEND_TOOL,
                    "Bash",
                    &serde_json::json!({ "command": send_cmd, "description": "message agy" }),
                )),
                (None, _) => Reply::Status(500),
            }
        })
        .expect("start claude mock")
    };
    claude.prepare(&h, &claude.provider_base_url(claude_mock.port()));
    let claude_name = launch(&h, "claude", &claude.launch_args(&h));
    wait_pty_proxy_up(&h, &claude_name, "Claude PTY proxy up");
    claude.drive_startup(&h, &claude_name);

    inject_prompt_until(
        &h,
        &claude_name,
        &format!("Message the agy agent {trigger}"),
        "claude to agy prompt",
        || {
            claude_mock.requests().iter().any(|req| {
                matches!(latest_user_turn(&req.body), Some((None, t)) if t.contains(&trigger))
            })
        },
        || {
            claude_mock.requests().iter().any(
                |req| matches!(latest_user_turn(&req.body), Some((Some(id), _)) if id == SEND_TOOL),
            )
        },
    );

    // 1. hcom recorded the message from Claude, addressed to agy.
    delivered_message(&h, &claude_name, &agy, &token);

    // 2. agy's own hooks observed the delivery: its row went active with a
    //    `deliver:<sender>` context. This is the persistent-hook path.
    h.eventually(
        "agy hook reports the delivery",
        Duration::from_secs(120),
        || {
            let (_, stdout, _) = h.run([
                "events", "--agent", &agy, "--type", "status", "--last", "60",
            ]);
            Ok(stdout
                .contains(&format!("deliver:{claude_name}"))
                .then_some(()))
        },
    );
    assert_inbox_drained(&h, &agy);

    // Diagnostic only, and no screen dump: agy's header shows the account's email.
    let (_, screen, _) = h.run(["term", &agy]);
    eprintln!(
        "agy screen shows the token after delivery: {}",
        screen.contains(&token)
    );

    let (code, _, stderr) = h.run(["kill", &agy, &claude_name]);
    assert_eq!(code, 0, "cleanup kill failed: {stderr}");
}
