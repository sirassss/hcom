//! Cross-tool scenario: a real Claude Code TUI messages a real Cursor Agent TUI.
//!
//! Run explicitly with:
//!   cargo test --test real_tool_claude_cursor -- --ignored --nocapture --test-threads=1
//!
//! Claude is routed at a localhost Anthropic Messages mock
//! ([`support::claude_mock`]) that scripts one turn: run `hcom send @<cursor>`
//! through Claude's real Bash tool. Cursor is a real, pinned `cursor-agent`
//! routed at a minimal localhost backend ([`support::cursor_mock`]). No account,
//! subscription or API key is involved on either side.
//!
//! What this proves: hcom launches Cursor under its persistent hooks, binds the
//! session, and delivers a message sent by another tool's agent into Cursor's
//! PTY. What it cannot prove: a Cursor-authored reply, because the mock does
//! not implement Cursor's agent run service.

mod support;

use serde_json::Value;
use serial_test::serial;
use std::time::Duration;
use support::Hcom;
use support::claude_mock::{ClaudeCase, claude_text, claude_tool_use, latest_user_turn};
use support::cursor_mock;
use support::mock_http::{MockHttp, Reply};
use support::real_tool::{ToolCase, require_pinned};
use support::{parse_launch_names, unique_suffix};

const SEND_TOOL: &str = "toolu_claude_to_cursor";

fn launch(h: &Hcom, tool: &str, args: &[String]) -> String {
    let mut argv = vec![
        tool.to_string(),
        "--headless".to_string(),
        "--dir".to_string(),
        h.workspace
            .to_str()
            .expect("UTF-8 workspace path")
            .to_string(),
        "--".to_string(),
    ];
    argv.extend(args.iter().cloned());
    let (code, stdout, stderr) = h.run(argv);
    assert_eq!(
        code,
        0,
        "real {tool} launch failed:\n-- stdout --\n{stdout}\n-- stderr --\n{stderr}\n{}",
        h.diagnostics()
    );
    let names = parse_launch_names(&stdout);
    assert_eq!(
        names.len(),
        1,
        "expected one launched {tool}; stdout={stdout}"
    );
    names[0].clone()
}

fn wait_pty_proxy_up(h: &Hcom, name: &str, what: &str) {
    h.eventually(what, Duration::from_secs(90), || {
        let (code, _stdout, _stderr) = h.run(["term", name]);
        Ok((code == 0).then_some(()))
    });
}

#[test]
#[ignore = "requires the pinned real claude and cursor-agent binaries"]
#[serial]
fn real_claude_message_is_delivered_to_real_cursor() {
    let h = Hcom::new();
    let claude = ClaudeCase;
    require_pinned(&h, &claude);
    cursor_mock::require_pinned(&h);

    let suffix = unique_suffix();
    let token = format!("HCOM_CLAUDE_TO_CURSOR_{suffix}");
    let trigger = format!("HCOM_TRIGGER_{suffix}");

    let cursor_mock_http = MockHttp::start(cursor_mock::respond).expect("start cursor mock");
    cursor_mock::prepare(&h, &format!("http://127.0.0.1:{}", cursor_mock_http.port()));

    // Cursor first: its name is the recipient in Claude's scripted command.
    let cursor_args = cursor_mock::launch_args();
    let cursor = launch(&h, "cursor-agent", &cursor_args);
    wait_pty_proxy_up(
        &h,
        &cursor,
        "Cursor PTY proxy up (inject endpoint registered)",
    );
    // Cursor's placeholder text reads as input, so `prompt_empty` is not a usable
    // readiness signal. A bound session (its `sessionStart` hook reached hcom)
    // is the one that matters here.
    let cursor_row = h.eventually(
        "Cursor session bound by its sessionStart hook",
        Duration::from_secs(90),
        || {
            Ok(h.instance_json(&cursor)?.filter(|row| {
                row.get("hooks_bound")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            }))
        },
    );
    eprintln!("cursor row after bind: {cursor_row}");

    // Claude: one scripted turn that sends the token to Cursor via Bash.
    let send_cmd = format!(
        "{} send @{cursor} --intent inform -- {token}",
        h.shell_hcom_command()
    );
    let (scenario_trigger, scenario_cmd) = (trigger.clone(), send_cmd.clone());
    let scenario_token = token.clone();
    let claude_mock_http = MockHttp::start(move |req| {
        if req.method.eq_ignore_ascii_case("HEAD") {
            return Reply::Empty(200);
        }
        if req.path.contains("count_tokens") {
            return Reply::Json(serde_json::json!({"input_tokens": 1}).to_string());
        }
        if !req.path.contains("/v1/messages") {
            return Reply::Status(404);
        }
        let (tool_result, text) = latest_user_turn(&req.body).unwrap_or((None, String::new()));
        match tool_result {
            Some(id) if id == SEND_TOOL => Reply::Sse(claude_text(
                "msg_sent",
                &format!("SENT_PROOF {scenario_token}"),
            )),
            Some(_) => Reply::Status(500),
            None if text.contains(&scenario_trigger) => Reply::Sse(claude_tool_use(
                "msg_send",
                SEND_TOOL,
                "Bash",
                &serde_json::json!({ "command": scenario_cmd, "description": "message cursor" }),
            )),
            None => Reply::Status(500),
        }
    })
    .expect("start claude mock");
    claude.prepare(&h, &claude.provider_base_url(claude_mock_http.port()));
    let claude_name = launch(&h, "claude", &claude.launch_args(&h));
    wait_pty_proxy_up(
        &h,
        &claude_name,
        "Claude PTY proxy up (inject endpoint registered)",
    );
    claude.drive_startup(&h, &claude_name);

    let saw_turn = || {
        claude_mock_http.requests().iter().any(|req| {
            req.path.contains("/v1/messages")
                && !req.path.contains("count_tokens")
                && matches!(latest_user_turn(&req.body), Some((None, t)) if t.contains(&trigger))
        })
    };
    let sent = || {
        claude_mock_http.requests().iter().any(|req| {
            req.path.contains("/v1/messages")
                && matches!(latest_user_turn(&req.body), Some((Some(id), _)) if id == SEND_TOOL)
        })
    };
    support::real_tool::inject_prompt_until(
        &h,
        &claude_name,
        &format!("Message the cursor agent {trigger}"),
        "claude to cursor prompt",
        saw_turn,
        sent,
    );

    // 1. hcom recorded the message from Claude, addressed to Cursor.
    let delivered_sql = format!(
        "EXISTS (SELECT 1 FROM json_each(json_extract(data,'$.delivered_to')) \
         WHERE json_each.value = '{cursor}')"
    );
    let message = h.eventually(
        "message from Claude delivered to Cursor",
        Duration::from_secs(60),
        || {
            let (code, stdout, stderr) = h.run([
                "events",
                "--type",
                "message",
                "--last",
                "20",
                "--sql",
                &delivered_sql,
            ]);
            if code != 0 {
                return Err(format!("events failed: {stderr}"));
            }
            Ok(stdout
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|v| v["data"]["text"].as_str() == Some(token.as_str())))
        },
    );
    assert_eq!(message["data"]["from"].as_str(), Some(claude_name.as_str()));

    // 2. Cursor's own hooks observed the delivery: its row went active with a
    //    `deliver:<sender>` context. This is the persistent-hook path.
    h.eventually(
        "Cursor hook reports the delivery",
        Duration::from_secs(60),
        || {
            let (_, stdout, _) = h.run([
                "events", "--agent", &cursor, "--type", "status", "--last", "40",
            ]);
            Ok(stdout
                .contains(&format!("deliver:{claude_name}"))
                .then_some(()))
        },
    );

    // Diagnostic only: the screen after delivery. Cursor's model turn cannot
    // complete against this mock, so the redraw after submit is not asserted.
    let (_, screen, _) = h.run(["term", &cursor]);
    eprintln!(
        "cursor screen after delivery (token visible: {}):\n{screen}",
        screen.contains(&token)
    );

    // Diagnostic only: the Cursor status sequence after the delivery, to see
    // whether `stop` returns the row to listening once the model turn fails.
    let (_, status_events, _) = h.run([
        "events", "--agent", &cursor, "--type", "status", "--last", "60",
    ]);
    let contexts: Vec<String> = status_events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|v| {
            format!(
                "{}:{}",
                v["data"]["status"].as_str().unwrap_or("?"),
                v["data"]["context"].as_str().unwrap_or("")
            )
        })
        .collect();
    eprintln!("cursor status sequence: {contexts:?}");

    // Every route the pinned cursor-agent called must be one the mock knows.
    let unknown = cursor_mock::unknown_routes(&cursor_mock_http.requests());
    assert!(
        unknown.is_empty(),
        "cursor-agent called routes the mock does not know: {unknown:?}"
    );
    let (code, _, stderr) = h.run(["kill", &cursor, &claude_name]);
    assert_eq!(code, 0, "cleanup kill failed: {stderr}");
}
