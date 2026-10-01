//! Minimal localhost Cursor backend for the real `cursor-agent` TUI.
//!
//! `cursor-agent` honours `CURSOR_API_ENDPOINT`/`CURSOR_API_KEY`, so a real,
//! pinned binary can be started with no Cursor account. Its startup talks to
//! the backend over two wire formats:
//!
//! * `POST /auth/exchange_user_api_key`: plain JSON, answered with fake tokens.
//! * Connect-protocol unary calls (`/aiserver.v1.*`, `application/proto`): the
//!   protobuf bodies are empty or ignorable except the model list, without
//!   which the TUI exits with "No model found".
//!
//! This mock answers exactly that. It does NOT implement the agent run service
//! (cursor-agent opens it over cleartext HTTP/2, which this HTTP/1.1 mock cannot
//! serve),
//! so a prompt submitted to Cursor never gets a model reply: scenarios built on
//! it can prove startup, hook binding and inbound hcom delivery, not a Cursor
//! authored response. Unknown routes are answered with an error and recorded,
//! so a new `cursor-agent` release that starts calling something new shows up
//! in [`unknown_routes`] instead of failing silently.
//!
//! Schema source: the `ModelDetails` message (fields 1 model_id, 3
//! display_model_id, 4 display_name, 5 display_name_short) and the repeated
//! `models` field 1 of `GetUsableModelsResponse`, read from the generated
//! protobuf-es code in the pinned cursor-agent bundle.

use super::Hcom;
use super::mock_http::{RecordedRequest, Reply};

/// Exact `cursor-agent` build the mock was written against. The wire surface is
/// not a public contract, so the test refuses any other build.
pub const PINNED_CURSOR_AGENT: &str = "2026.09.28-64d2043";

pub const MODEL_ID: &str = "auto";
pub const API_KEY: &str = "hcom-real-test-cursor-key";

const PROTO: &str = "application/proto";

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// A length-delimited field: tag, length, payload.
fn field(number: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = varint(number << 3 | 2);
    out.extend(varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn model_details() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(field(1, MODEL_ID.as_bytes()));
    out.extend(field(3, MODEL_ID.as_bytes()));
    out.extend(field(4, b"Auto"));
    out.extend(field(5, b"Auto"));
    out
}

/// Answer one request. `None` means the route is not one the pinned build is
/// known to call; the caller records it.
fn known_reply(req: &RecordedRequest) -> Option<Reply> {
    let path = req.path.split('?').next().unwrap_or_default();
    if req.method.eq_ignore_ascii_case("HEAD") {
        return Some(Reply::Empty(200));
    }
    // `PRI * HTTP/2.0` is the cleartext HTTP/2 connection preface cursor-agent
    // sends when it opens the agent run service. This mock speaks HTTP/1.1 only,
    // so the turn fails; that is the documented limit, not a new route.
    if req.method == "PRI" {
        return Some(Reply::Empty(404));
    }
    match path {
        "/auth/exchange_user_api_key" => Some(Reply::Json(
            serde_json::json!({
                "accessToken": "hcom.mock.access.token",
                "refreshToken": "hcom.mock.refresh.token",
            })
            .to_string(),
        )),
        "/aiserver.v1.AiService/GetUsableModels" => Some(Reply::Raw {
            content_type: PROTO,
            body: field(1, &model_details()),
        }),
        "/aiserver.v1.AiService/GetDefaultModelForCli" => Some(Reply::Raw {
            content_type: PROTO,
            body: field(1, &model_details()),
        }),
        // Telemetry sinks.
        "/v1/traces" | "/v1/logs" | "/v1/metrics" => Some(Reply::Empty(200)),
        p if p.starts_with("/aiserver.v1.AnalyticsService/") => Some(Reply::Empty(200)),
        // Every other startup unary call is satisfied by an empty message.
        p if p.starts_with("/aiserver.v1.") => Some(Reply::Raw {
            content_type: PROTO,
            body: Vec::new(),
        }),
        _ => None,
    }
}

/// Responder for [`super::mock_http::MockHttp::start`]. The agent run service
/// (`/agent.v1.`) is deliberately unimplemented: it is answered with a benign
/// 404 so a delivered prompt fails its model turn instead of hanging.
pub fn respond(req: &RecordedRequest) -> Reply {
    if let Some(reply) = known_reply(req) {
        return reply;
    }
    if req.path.starts_with("/agent.v1.") {
        return Reply::Empty(404);
    }
    Reply::Status(404)
}

/// Request paths the mock does not know, for a failure message.
pub fn unknown_routes(requests: &[RecordedRequest]) -> Vec<String> {
    let mut paths: Vec<String> = requests
        .iter()
        .filter(|req| known_reply(req).is_none() && !req.path.starts_with("/agent.v1."))
        .map(|req| format!("{} {}", req.method, req.path))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Route the launched `cursor-agent` at `endpoint`. Goes through the
/// `$HCOM_DIR/env` passthrough, which survives hcom's `CI=1` clean-shell rebuild.
pub fn prepare(h: &Hcom, endpoint: &str) {
    // hcom writes Cursor's hooks.json under its tool-config root (the parent of
    // HCOM_DIR), while the harness gives the tool a separate HOME and
    // cursor-agent reads `$HOME/.cursor/hooks.json`. Without this link the tool
    // never sees the hooks and nothing ever binds. Unix only, like these tests.
    let hcom_side = h.root_path().join(".cursor");
    std::fs::create_dir_all(&hcom_side).expect("create hcom-side Cursor config dir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&hcom_side, h.home.join(".cursor"))
        .expect("link HOME/.cursor to hcom's Cursor config dir");
    let shim_dir = install_ci_shim(h);
    // Keep the exact hcom binary reachable (the hooks call plain `hcom`).
    let hcom_bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_hcom"))
        .parent()
        .expect("hcom binary has a parent dir")
        .to_path_buf();
    let inherited = std::env::var("PATH").unwrap_or_default();
    let shim_path = format!(
        "{}:{}:{inherited}",
        shim_dir.display(),
        hcom_bin_dir.display()
    );
    h.set_launch_envs(&[
        ("PATH", shim_path.as_str()),
        ("CURSOR_API_ENDPOINT", endpoint),
        ("CURSOR_API_KEY", API_KEY),
    ]);
}

/// Panic unless the resolved `cursor-agent` is exactly the pinned build.
pub fn require_pinned(h: &Hcom) {
    let resolved = h
        .resolve_external("cursor-agent")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<cursor-agent not found on PATH>".to_string());
    match h.external_version("cursor-agent") {
        Ok(version) if version.trim() == PINNED_CURSOR_AGENT => {}
        Ok(version) => panic!(
            "real cursor test requires cursor-agent {PINNED_CURSOR_AGENT}, found `{version}` \
             at {resolved}"
        ),
        Err(reason) => panic!(
            "real cursor test requires cursor-agent {PINNED_CURSOR_AGENT}: {reason}. \
             Resolved to: {resolved}"
        ),
    }
}

/// Arguments appended after `hcom cursor-agent --headless --dir <ws> --`.
pub fn launch_args() -> Vec<String> {
    vec![
        "--trust".to_string(),
        "--model".to_string(),
        MODEL_ID.to_string(),
    ]
}

/// A `cursor-agent` that execs the real binary with `CI` removed.
///
/// cursor-agent treats the mere presence of `CI` as a non-interactive run
/// (measured on 2026.09.28-64d2043 with a raw PTY capture): `CI=1` or an empty
/// value never draws the prompt box, and even `CI=0` draws it as plain text.
/// hcom tells "empty prompt" from "typed text" by the placeholder's dim
/// attribute, so plain text reads as a draft and the delivery gate never opens.
///
/// The harness runs hcom with `CI=1` and `NO_COLOR=1`, and the tool receives
/// both (checked by dumping the env the shim sees). hcom normally launches from
/// a clean shell env when its own env looks contaminated (`launcher::
/// ContaminatedParent`) and falls back to the parent env only when that
/// resolution fails, which an `env_clear`ed harness does not exercise the usual
/// way; an interactive run with a `CI`-bearing parent did NOT leak `CI`. Only an
/// unset `CI` yields the styled placeholder, so the shim removes it at exec.
#[cfg(unix)]
fn install_ci_shim(h: &Hcom) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let real = h
        .resolve_external("cursor-agent")
        .expect("cursor-agent on PATH for the colour shim");
    let dir = h.root_path().join("cursor-shim");
    std::fs::create_dir_all(&dir).expect("create cursor shim dir");
    let shim = dir.join("cursor-agent");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nexec env -u CI '{}' \"$@\"\n",
            real.display().to_string().replace('\'', "'\\''")
        ),
    )
    .expect("write cursor shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
        .expect("chmod cursor shim");
    dir
}
