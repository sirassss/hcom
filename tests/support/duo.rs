//! Helpers shared by the two-agent real-tool scenarios (one real CLI messages
//! another through hcom). Each scenario scripts both models with localhost
//! mocks, so no account or API key is involved on either side.

use super::Hcom;
use super::parse_launch_names;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// An instance name that exists only after launch, readable from a mock
/// responder that must start (and fix its port) before the launch.
pub type SharedName = Arc<Mutex<String>>;

pub fn shared_name() -> SharedName {
    Arc::new(Mutex::new(String::new()))
}

pub fn name_of(name: &SharedName) -> String {
    name.lock().expect("name lock").clone()
}

pub fn set_name(name: &SharedName, value: &str) {
    *name.lock().expect("name lock") = value.to_string();
}

/// `hcom <tool> --headless --dir <workspace> -- <args>`; returns the one name it launched.
pub fn launch(h: &Hcom, tool: &str, args: &[String]) -> String {
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

/// Wait for the launched tool's inject endpoint; `hcom term` exits nonzero until then.
pub fn wait_pty_proxy_up(h: &Hcom, name: &str, what: &str) {
    h.eventually(what, Duration::from_secs(90), || {
        let (code, _stdout, _stderr) = h.run(["term", name]);
        Ok((code == 0).then_some(()))
    });
}

/// Wait until hcom recorded `token` sent by `from` and delivered to `to`.
pub fn delivered_message(h: &Hcom, from: &str, to: &str, token: &str) -> Value {
    let delivered_sql = format!(
        "EXISTS (SELECT 1 FROM json_each(json_extract(data,'$.delivered_to')) \
         WHERE json_each.value = '{to}')"
    );
    h.eventually(
        &format!("message {from} -> {to} delivered"),
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
                .find(|v| {
                    v["data"]["text"].as_str() == Some(token)
                        && v["data"]["from"].as_str() == Some(from)
                }))
        },
    )
}

/// The agent's delivered inbox is drained: the cursor advanced past the message.
pub fn assert_inbox_drained(h: &Hcom, name: &str) {
    h.eventually(
        &format!("{name} inbox drained"),
        Duration::from_secs(30),
        || {
            Ok(h.instance_json(name)?
                .filter(|row| row["unread_count"].as_u64() == Some(0)))
        },
    );
}
