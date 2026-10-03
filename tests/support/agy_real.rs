//! Real Antigravity (`agy`) in a hermetic harness.
//!
//! Unlike Claude, Codex and Cursor there is no localhost mock for `agy`: it
//! authenticates with Google OAuth and talks to Google's backend, and nothing
//! measured lets an endpoint be redirected (2026-10-03, agy 1.2.12: a clean HOME
//! asks for a browser login, and `GEMINI_API_KEY` / `GOOGLE_GEMINI_BASE_URL`
//! change nothing). So scenarios using this module spend real model quota and
//! need an existing login: they are opt-in and never part of `just ci`.
//!
//! The login is supplied as a directory holding `antigravity-oauth-token` and
//! `installation_id` (`HCOM_RT_AGY_AUTH_DIR`). Both are copied into the
//! harness's own config tree, so the originals are never written to.

use super::Hcom;

/// Exact `agy` build the scenario was written against.
pub const PINNED_AGY: &str = "1.2.16";
pub const AUTH_DIR_ENV: &str = "HCOM_RT_AGY_AUTH_DIR";
const AUTH_FILES: [&str; 2] = ["antigravity-oauth-token", "installation_id"];

/// Panic unless the resolved `agy` is exactly the pinned build.
pub fn require_pinned(h: &Hcom) {
    let resolved = h
        .resolve_external("agy")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<agy not found on PATH>".to_string());
    match h.external_version("agy") {
        Ok(version) if version.trim() == PINNED_AGY => {}
        Ok(version) => panic!(
            "real agy test requires agy {PINNED_AGY}, found `{}` at {resolved}",
            version.trim()
        ),
        Err(reason) => {
            panic!("real agy test requires agy {PINNED_AGY}: {reason}. Resolved to: {resolved}")
        }
    }
}

/// Copy the login into the harness and make the workspace trusted, so `agy`
/// reaches its prompt without any dialog.
pub fn prepare(h: &Hcom) {
    let auth_dir = std::env::var(AUTH_DIR_ENV).unwrap_or_else(|_| {
        panic!(
            "real agy test needs a Google login: set {AUTH_DIR_ENV} to a directory holding \
             {AUTH_FILES:?} (copy them from ~/.gemini/antigravity-cli/)"
        )
    });
    // hcom writes the Antigravity hooks under its tool-config root (the parent of
    // HCOM_DIR), while the harness gives the tool a separate HOME and `agy` reads
    // `$HOME/.gemini`. Link them, or the hooks never reach the tool.
    let hcom_side = h.root_path().join(".gemini");
    let state = hcom_side.join("antigravity-cli");
    std::fs::create_dir_all(&state).expect("create agy state dir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&hcom_side, h.home.join(".gemini"))
        .expect("link HOME/.gemini to hcom's Gemini config dir");
    for file in AUTH_FILES {
        std::fs::copy(std::path::Path::new(&auth_dir).join(file), state.join(file))
            .unwrap_or_else(|e| panic!("copy {file} from {auth_dir}: {e}"));
    }
    // Without this flag the first screen is the colour-scheme picker, which
    // hcom reports as `launch_blocked` and nothing ever reaches the prompt.
    std::fs::create_dir_all(state.join("cache")).expect("create agy cache dir");
    std::fs::write(
        state.join("cache").join("onboarding.json"),
        serde_json::json!({
            "consumerOnboardingComplete": true,
            "enterpriseOnboardingComplete": false,
            "onboardingComplete": true,
        })
        .to_string(),
    )
    .expect("write agy onboarding state");
    let workspace = h.workspace.to_str().expect("UTF-8 workspace path");
    std::fs::write(
        state.join("settings.json"),
        serde_json::json!({
            "toolPermission": "always-proceed",
            "showFeedbackSurvey": false,
            "trustedWorkspaces": [workspace],
        })
        .to_string(),
    )
    .expect("write agy settings");

    // Keep the exact hcom binary reachable: the hooks call plain `hcom`.
    let hcom_bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_hcom"))
        .parent()
        .expect("hcom binary has a parent dir")
        .to_path_buf();
    let inherited = std::env::var("PATH").unwrap_or_default();
    h.set_launch_envs(&[(
        "PATH",
        format!("{}:{inherited}", hcom_bin_dir.display()).as_str(),
    )]);
}
