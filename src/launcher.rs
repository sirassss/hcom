//! Unified launcher pipeline for released AI-tool integrations.
//!
//!
//! Provides a single entry point for launching all supported AI tools
//! with consistent batch tracking, environment setup, and error handling.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use rand::RngExt;
use serde_json::json;

use crate::config::{self, HcomConfig};
use crate::db::HcomDb;
use crate::instance_binding;
use crate::instance_names;
use crate::instances;
use crate::paths;
use crate::shared::constants::HCOM_IDENTITY_VARS;
use crate::shared::tool_detection::tool_marker_vars;
use crate::terminal;
use crate::tools::launch_arg_validation::{
    ANTIGRAVITY_REJECTED_ARGS, GEMINI_REJECTED_ARGS, KILO_REJECTED_ARGS, KIMI_REJECTED_ARGS,
    OMP_REJECTED_ARGS, OPENCODE_REJECTED_ARGS, PI_REJECTED_ARGS, validate_rejected_args,
};
use crate::tools::{
    codex_preprocessing, copilot_preprocessing, cursor_preprocessing, opencode_preprocessing,
};

/// Canonical tool types for launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchTool {
    Claude,
    ClaudePty,
    Gemini,
    Codex,
    OpenCode,
    Kilo,
    Pi,
    Antigravity,
    Cursor,
    Kimi,
    Copilot,
    Omp,
}

impl LaunchTool {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(LaunchTool::Claude),
            "claude-pty" => Ok(LaunchTool::ClaudePty),
            "gemini" => Ok(LaunchTool::Gemini),
            "codex" => Ok(LaunchTool::Codex),
            "opencode" => Ok(LaunchTool::OpenCode),
            "kilo" | "kilocode" => Ok(LaunchTool::Kilo),
            "pi" | "pi-agent" => Ok(LaunchTool::Pi),
            "omp" | "omp-agent" => Ok(LaunchTool::Omp),
            "antigravity" | "agy" => Ok(LaunchTool::Antigravity),
            "cursor" | "cursor-agent" => Ok(LaunchTool::Cursor),
            "kimi" => Ok(LaunchTool::Kimi),
            "copilot" => Ok(LaunchTool::Copilot),
            _ => bail!("Unknown tool: {}", s),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LaunchTool::Claude => "claude",
            LaunchTool::ClaudePty => "claude-pty",
            LaunchTool::Gemini => "gemini",
            LaunchTool::Codex => "codex",
            LaunchTool::OpenCode => "opencode",
            LaunchTool::Kilo => "kilo",
            LaunchTool::Pi => "pi",
            LaunchTool::Omp => "omp",
            LaunchTool::Antigravity => "antigravity",
            LaunchTool::Cursor => "cursor",
            LaunchTool::Kimi => "kimi",
            LaunchTool::Copilot => "copilot",
        }
    }

    /// Canonical [`Tool`] for this launch surface.
    ///
    /// `ClaudePty` is a launch-surface alias (PTY-wrapped Claude); it resolves
    /// to `Tool::Claude` so all per-tool data flows through one spec.
    pub fn tool(&self) -> crate::tool::Tool {
        match self {
            LaunchTool::Claude | LaunchTool::ClaudePty => crate::tool::Tool::Claude,
            LaunchTool::Gemini => crate::tool::Tool::Gemini,
            LaunchTool::Codex => crate::tool::Tool::Codex,
            LaunchTool::OpenCode => crate::tool::Tool::OpenCode,
            LaunchTool::Kilo => crate::tool::Tool::Kilo,
            LaunchTool::Pi => crate::tool::Tool::Pi,
            LaunchTool::Omp => crate::tool::Tool::Omp,
            LaunchTool::Antigravity => crate::tool::Tool::Antigravity,
            LaunchTool::Cursor => crate::tool::Tool::Cursor,
            LaunchTool::Kimi => crate::tool::Tool::Kimi,
            LaunchTool::Copilot => crate::tool::Tool::Copilot,
        }
    }

    /// Integration spec for this launch surface (shared with the base `Tool`).
    pub fn spec(&self) -> &'static crate::integration_spec::IntegrationSpec {
        self.tool().spec()
    }

    /// Base tool name (without -pty suffix). Equivalent to `self.tool().as_str()`.
    pub fn base_tool(&self) -> &'static str {
        self.tool().as_str()
    }

    /// Whether this tool uses the PTY wrapper.
    pub fn uses_pty(&self) -> bool {
        // ClaudePty is a launch surface (alias), not a Tool variant — it always
        // takes the PTY path. Everything else defers to the canonical spec.
        if matches!(self, LaunchTool::ClaudePty) {
            return true;
        }
        self.spec().launch.uses_pty_default
    }

    /// Executable name on PATH for this tool.
    pub fn cli_binary(&self) -> &'static str {
        self.spec().cli_binary
    }
}

/// How the child process is hosted. Computed from (tool, background, pty) at
/// launch time so dispatch doesn't have to re-derive the combination.
///
/// - `InteractiveVisible`: foreground, user-visible terminal. All tools.
/// - `HeadlessPty`:       background, PTY wrapper in a detached runner. Default
///   for gemini/codex/opencode/kilo/pi/omp/antigravity/cursor/kimi/copilot and for default claude `--headless`.
/// - `NativePrint`:       background, direct claude spawn in print mode
///   (`-p --output-format stream-json --verbose`). Claude only, opt-in via an
///   explicit `-p`/`--print`; kept alive across turns by hcom's stop-hook loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchBackend {
    InteractiveVisible,
    HeadlessPty,
    NativePrint,
}

impl LaunchBackend {
    /// Resolve from the already-prepared (tool, background) pair.
    ///
    /// The PTY-vs-print decision for Claude is encoded in the [`LaunchTool`]
    /// surface chosen by `launch()`: `Claude` is the `-p`/`--print` surface
    /// (→ `NativePrint`), `ClaudePty` is the live PTY surface (→ `HeadlessPty`).
    /// Background Claude defaults to `ClaudePty`; it only becomes
    /// `Claude`/`NativePrint` when the caller passes `-p`/`--print`.
    pub fn resolve(tool: &LaunchTool, background: bool) -> Self {
        if !background {
            return LaunchBackend::InteractiveVisible;
        }
        match tool {
            LaunchTool::Claude => LaunchBackend::NativePrint,
            LaunchTool::ClaudePty => LaunchBackend::HeadlessPty,
            LaunchTool::Gemini
            | LaunchTool::Codex
            | LaunchTool::OpenCode
            | LaunchTool::Kilo
            | LaunchTool::Pi
            | LaunchTool::Omp
            | LaunchTool::Antigravity
            | LaunchTool::Cursor
            | LaunchTool::Kimi
            | LaunchTool::Copilot => LaunchBackend::HeadlessPty,
        }
    }
}

/// Launch parameters.
#[derive(Clone)]
pub struct LaunchParams {
    pub tool: String,
    pub count: usize,
    pub args: Vec<String>,
    /// Raw user/config args to persist for future resume, before hcom injections.
    pub persisted_args: Option<Vec<String>>,
    /// Session id being resumed, inherited by the recreated instance row so a
    /// kill before the tool's first turn (no hook re-bind yet) stays resumable.
    pub prior_session_id: Option<String>,
    pub tag: Option<String>,
    pub system_prompt: Option<String>,
    pub initial_prompt: Option<String>,
    pub background: bool,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub launcher: Option<String>,
    pub run_here: Option<bool>,
    pub batch_id: Option<String>,
    pub name: Option<String>,
    pub skip_validation: bool,
    pub terminal: Option<String>,
    pub append_reply_handoff: bool,
}

impl Default for LaunchParams {
    fn default() -> Self {
        Self {
            tool: "claude".to_string(),
            count: 1,
            args: Vec::new(),
            persisted_args: None,
            prior_session_id: None,
            tag: None,
            system_prompt: None,
            initial_prompt: None,
            background: false,
            cwd: None,
            env: None,
            launcher: None,
            run_here: None,
            batch_id: None,
            name: None,
            skip_validation: false,
            terminal: None,
            append_reply_handoff: true,
        }
    }
}

/// Launch result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchResult {
    pub tool: String,
    pub batch_id: String,
    pub launched: usize,
    pub failed: usize,
    pub background: bool,
    pub log_files: Vec<String>,
    pub handles: Vec<serde_json::Value>,
    pub errors: Vec<serde_json::Value>,
}

/// Predict if launch will block current terminal (run in same window).
/// Find tool executable path with fallbacks.
/// Claude has special fallback locations; other tools just use PATH.
fn find_tool_path(tool: &str) -> Option<String> {
    crate::terminal::which_bin(tool)
}

/// Check if tool CLI is installed (PATH + fallbacks).
fn is_tool_installed(tool: &str) -> bool {
    find_tool_path(tool).is_some()
}

pub fn will_run_in_current_terminal(
    count: usize,
    background: bool,
    run_here: Option<bool>,
    terminal: Option<&str>,
    inside_ai_tool: bool,
) -> bool {
    if let Some(rh) = run_here {
        return rh;
    }
    // terminal=here forces current terminal
    if terminal == Some("here") {
        return true;
    }
    if inside_ai_tool {
        return false;
    }
    if background {
        return false;
    }
    count == 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchEnvRegime {
    HumanShell,
    ContaminatedParent,
    RunHere,
}

pub fn launch_env_regime(run_here: bool, inside_ai_tool: bool) -> LaunchEnvRegime {
    if run_here {
        LaunchEnvRegime::RunHere
    } else if contaminated_parent_with_inside_ai_tool(inside_ai_tool) {
        LaunchEnvRegime::ContaminatedParent
    } else {
        LaunchEnvRegime::HumanShell
    }
}

pub fn contaminated_parent() -> bool {
    contaminated_parent_with_inside_ai_tool(crate::shared::platform::is_inside_ai_tool())
}

fn contaminated_parent_with_inside_ai_tool(inside_ai_tool: bool) -> bool {
    inside_ai_tool
        || std::env::var_os("CI").is_some()
        || std::env::var_os("GITHUB_ACTIONS").is_some()
        || std::env::vars_os().any(|(key, _)| key.to_string_lossy().starts_with("CARGO_"))
}

/// Build base environment, then overlay config.toml + ~/.hcom/env (these win).
///
/// This makes the new-window/runner-script path behave like the PTY path
/// (`Command::new` inherits parent env by default). The runner script
/// already `unset`s TOOL_MARKER_VARS + HCOM_IDENTITY_VARS before exec,
/// so those categories are safe to include here (they're cleared in-script).
pub fn build_launch_env(
    hcom_config: &HcomConfig,
    regime: LaunchEnvRegime,
) -> HashMap<String, String> {
    build_launch_env_with_resolver(hcom_config, regime, crate::shell_env::resolved_shell_env)
}

/// Apply the launcher's inherited `HCOM_NOTES` to a child instance env as a
/// fallback only.
///
/// `instance_env` (via `base_env`) already carries any notes resolved from the
/// explicit sources — config.toml, `~/.hcom/env`, and `LaunchParams.env` — and
/// those must win. The inherited parent value only fills the gap for a nested
/// launch where no explicit notes were configured, so it must not clobber a
/// value already present (including an intentional empty string used to clear
/// notes).
fn apply_inherited_notes(instance_env: &mut HashMap<String, String>, inherited: Option<String>) {
    if let Some(val) = inherited {
        instance_env.entry("HCOM_NOTES".to_string()).or_insert(val);
    }
}

fn build_codex_bootstrap(
    db: &HcomDb,
    hcom_dir: &Path,
    instance_name: &str,
    background: bool,
    instance_env: &HashMap<String, String>,
    tag: &str,
    relay_enabled: bool,
) -> String {
    let notes = instance_env
        .get("HCOM_NOTES")
        .map(String::as_str)
        .unwrap_or("");
    crate::bootstrap::get_bootstrap(
        db,
        hcom_dir,
        instance_name,
        "codex",
        background,
        true,
        notes,
        tag,
        relay_enabled,
        None,
    )
}

fn build_launch_env_with_resolver<F>(
    hcom_config: &HcomConfig,
    regime: LaunchEnvRegime,
    resolved_shell_env: F,
) -> HashMap<String, String>
where
    F: Fn() -> Option<HashMap<String, String>>,
{
    let base: HashMap<String, String> = match regime {
        LaunchEnvRegime::HumanShell | LaunchEnvRegime::RunHere => std::env::vars().collect(),
        LaunchEnvRegime::ContaminatedParent => {
            resolved_shell_env().unwrap_or_else(|| std::env::vars().collect())
        }
    };
    let strip = match regime {
        LaunchEnvRegime::RunHere => run_here_env_strip_set(),
        LaunchEnvRegime::HumanShell | LaunchEnvRegime::ContaminatedParent => env_strip_set(),
    };
    let mut env: HashMap<String, String> = base
        .into_iter()
        .filter(|(k, _)| !strip.contains(k.as_str()))
        .collect();

    // HCOM_* settings from config.toml
    for (key, value) in hcom_config.to_env_dict() {
        if !value.is_empty() {
            insert_effective_env(&mut env, key, value, cfg!(windows));
        }
    }

    // Passthrough vars from env file (these win over everything)
    let env_path = paths::hcom_path(&["env"]);
    for (key, value) in config::load_env_extras(&env_path) {
        if !value.is_empty() {
            insert_effective_env(&mut env, key, value, cfg!(windows));
        }
    }

    env
}

/// Build the set of env var names to strip from inherited env.
///
/// Three closed categories (owned by hcom):
/// 1. HCOM_IDENTITY_VARS
/// 2. TOOL_MARKER_VARS
/// 3. TERMINAL_CONTEXT_VARS
///
/// Per-tool instance-state vars are NOT in the initial strip — they are
/// stripped per-instance later via `strip_instance_state_vars` so that
/// cross-tool nesting doesn't strip vars the child tool doesn't own.
fn env_strip_set() -> std::collections::HashSet<String> {
    let mut strip: std::collections::HashSet<String> = std::collections::HashSet::new();

    strip.extend(run_here_env_strip_set());
    for v in crate::terminal::TERMINAL_CONTEXT_VARS {
        strip.insert((*v).to_string());
    }

    strip
}

fn run_here_env_strip_set() -> std::collections::HashSet<String> {
    let mut strip: std::collections::HashSet<String> = std::collections::HashSet::new();

    for v in crate::shared::constants::HCOM_IDENTITY_VARS {
        strip.insert((*v).to_string());
    }
    for v in tool_marker_vars() {
        strip.insert((*v).to_string());
    }
    strip.insert("HCOM_LAUNCHED_PRESET".to_string());

    strip
}

fn isolated_tool_config_dir(tool: &LaunchTool) -> Option<std::path::PathBuf> {
    let root = crate::runtime_env::tool_config_root();
    if dirs::home_dir().as_deref() == Some(root.as_path()) {
        return None;
    }
    let dirname = match tool.tool() {
        crate::tool::Tool::Claude => ".claude",
        crate::tool::Tool::Gemini | crate::tool::Tool::Antigravity => ".gemini",
        crate::tool::Tool::Codex => ".codex",
        crate::tool::Tool::Kilo => ".kilo",
        crate::tool::Tool::Pi => ".pi",
        crate::tool::Tool::Omp => ".omp",
        crate::tool::Tool::Cursor => ".cursor",
        crate::tool::Tool::Kimi => ".kimi",
        crate::tool::Tool::Copilot => ".copilot",
        crate::tool::Tool::OpenCode | crate::tool::Tool::Adhoc => return None,
    };
    Some(root.join(dirname))
}

/// Insert an environment override using the target platform's key semantics.
/// Windows environment names are case-insensitive, while `HashMap` keys are
/// not; remove an earlier spelling so the child receives one authoritative
/// value instead of an order-dependent pair.
fn insert_effective_env(
    env: &mut HashMap<String, String>,
    key: String,
    value: String,
    case_insensitive: bool,
) {
    if case_insensitive {
        env.retain(|existing, _| !existing.eq_ignore_ascii_case(&key));
    }
    env.insert(key, value);
}

fn effective_env_value<'a>(
    env: &'a HashMap<String, String>,
    key: &str,
    case_insensitive: bool,
) -> Option<&'a str> {
    if case_insensitive {
        env.iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    } else {
        env.get(key).map(String::as_str)
    }
}

/// Make the tool config directory explicit in the child environment.
///
/// Some launch backends clear the inherited environment while others do not.
/// Copying an ambient override into the effective launch map keeps preflight,
/// hook setup, and the child process on the same directory in both cases.
fn ensure_tool_config_env(tool: &LaunchTool, env: &mut HashMap<String, String>) {
    let Some(env_var) = tool.spec().launch.config_dir_env else {
        return;
    };
    let case_insensitive = cfg!(windows);
    if let Some(value) = effective_env_value(env, env_var, case_insensitive).map(str::to_owned) {
        insert_effective_env(env, env_var.to_string(), value, case_insensitive);
        return;
    }
    if let Some(value) = std::env::var(env_var)
        .ok()
        .filter(|value| !value.is_empty())
    {
        insert_effective_env(env, env_var.to_string(), value, case_insensitive);
    } else if let Some(config_dir) = isolated_tool_config_dir(tool) {
        insert_effective_env(
            env,
            env_var.to_string(),
            config_dir.to_string_lossy().to_string(),
            case_insensitive,
        );
    }
}

/// Get system prompt file path for Gemini/Codex.
fn get_system_prompt_path(tool: &str) -> std::path::PathBuf {
    let prompts_dir = paths::hcom_path(&["system-prompts"]);
    fs::create_dir_all(&prompts_dir).ok();
    prompts_dir.join(format!("{}.md", tool))
}

/// Write system prompt to file (only if content differs).
fn write_system_prompt_file(system_prompt: &str, tool: &str) -> String {
    let filepath = get_system_prompt_path(tool);

    // Only write if content differs
    if let Ok(existing) = fs::read_to_string(&filepath)
        && existing == system_prompt
    {
        return filepath.to_string_lossy().to_string();
    }

    if let Err(e) = fs::write(&filepath, system_prompt) {
        eprintln!(
            "[hcom] warn: failed to write system prompt to {}: {e}",
            filepath.display()
        );
    }
    filepath.to_string_lossy().to_string()
}

/// Generate a UUID v4-like process ID string.
fn generate_process_id() -> String {
    let mut rng = rand::rng();
    let a: u32 = rng.random();
    let b: u16 = rng.random();
    let c: u16 = (rng.random::<u16>() & 0x0FFF) | 0x4000; // version 4
    let d: u16 = (rng.random::<u16>() & 0x3FFF) | 0x8000; // variant 1
    let e: u64 = rng.random::<u64>() & 0xFFFFFFFFFFFF; // 48 bits
    format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}", a, b, c, d, e)
}

/// Message shown when a tool's hooks are not installed.
///
/// Launching must never fix this: installing hooks edits files on the user's
/// machine, and that should happen because they asked, not as a side effect of
/// starting an agent.
fn hooks_missing_warning(tool: &LaunchTool) -> String {
    let name = match tool {
        LaunchTool::ClaudePty => "claude",
        other => other.as_str(),
    };
    format!(
        "hcom hooks are not installed for {name}.\n\
         Messages will not be delivered automatically this session.\n  \
         Install:  hcom hooks add {name}"
    )
}

fn install_diag_context(tool: &LaunchTool, paths: &[(&str, std::path::PathBuf)]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Diagnostic context:");
    for (label, p) in paths {
        let _ = writeln!(out, "  resolved {label}={}", p.display());
    }
    let _ = writeln!(
        out,
        "  HCOM_DIR={}",
        std::env::var("HCOM_DIR").unwrap_or_else(|_| "<unset>".into())
    );
    let tool_env_var = tool.spec().launch.config_dir_env;
    if let Some(env_var) = tool_env_var {
        let _ = writeln!(
            out,
            "  {env_var}={}",
            std::env::var(env_var).unwrap_or_else(|_| "<unset>".into())
        );
    }
    let _ = writeln!(
        out,
        "  cwd={}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".into())
    );
    out
}

fn format_plugin_install_error(
    label: &str,
    tool: &str,
    target: &std::path::Path,
    error: &std::io::Error,
    diag: &str,
) -> String {
    use std::io::ErrorKind;

    let mut message = format!(
        "Failed to install {label} plugin at {}: {error}",
        target.display()
    );
    if matches!(
        error.kind(),
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
    ) {
        message.push_str(
            "\nThe current process cannot write to the tool's config directory. If an AI agent ran this command inside a sandbox, retry the original hcom launch with approval or elevated permission to write this path.",
        );
    }
    message.push_str(&format!("\nManual retry: hcom hooks add {tool}\n{diag}"));
    message
}

/// Whether a Codex launch should install the native hooks.
///
/// Only when the plugin is not the thing delivering messages. An installed
/// plugin whose handlers Codex reports — untrusted or already duplicated
/// included — must not be shadowed by a fresh native install. `Unverified`
/// still installs: an unreadable inventory is not evidence that something else
/// is delivering messages, and a session with no hooks is silent.
fn codex_launch_needs_native_hooks(state: crate::hooks::codex::CodexPluginState) -> bool {
    use crate::hooks::codex::CodexPluginState as S;
    !matches!(
        state,
        S::Active | S::ReviewRequired | S::Duplicate | S::Disabled
    )
}

/// Verify hooks are installed for the target tool, auto-install if needed.
///
/// Uses verify-first pattern: read-only check first, only write if needed.
/// Strict gate: refuses to launch if hooks can't be installed.
fn ensure_hooks_installed(
    tool: &LaunchTool,
    include_permissions: bool,
    codex_home: Option<&std::path::Path>,
    launch_dir: &std::path::Path,
) -> Result<()> {
    match tool {
        LaunchTool::Claude | LaunchTool::ClaudePty => {
            if !crate::hooks::plugin::verify_claude_plugin_installed() {
                eprintln!("{}", hooks_missing_warning(tool));
            }
            Ok(())
        }
        LaunchTool::Gemini => {
            if !crate::hooks::gemini::is_gemini_version_supported() {
                if let Some(ver) = crate::hooks::gemini::get_gemini_version() {
                    bail!(
                        "Gemini CLI version {}.{}.{} is too old. Update: npm i -g @google/gemini-cli@latest",
                        ver.0,
                        ver.1,
                        ver.2
                    );
                } else {
                    eprintln!("Warning: Could not detect Gemini CLI version");
                }
            }
            if crate::hooks::gemini::verify_gemini_hooks_installed(include_permissions) {
                return Ok(());
            }
            if let Err(e) = crate::hooks::gemini::try_setup_gemini_hooks(include_permissions) {
                let diag = install_diag_context(
                    tool,
                    &[(
                        "settings_path",
                        crate::hooks::gemini::get_gemini_settings_path(),
                    )],
                );
                bail!(
                    "Failed to setup Gemini hooks: {e}\n\
                     Run: hcom hooks add gemini\n\
                     {diag}"
                );
            }
            Ok(())
        }
        LaunchTool::Codex => {
            let codex_home = codex_home.expect("Codex launch must resolve CODEX_HOME");
            // Codex's hooks can now come from the plugin. Installing the native
            // set behind a live plugin is what turned `hooks remove codex
            // --legacy-only` into a no-op undone by the next agent launch,
            // straight back into a double-fire.
            if !codex_launch_needs_native_hooks(
                crate::hooks::codex::codex_plugin_status_at(launch_dir, codex_home).state,
            ) {
                return Ok(());
            }
            if crate::hooks::codex::verify_codex_hooks_installed_at(include_permissions, codex_home)
                && crate::hooks::codex::codex_current_feature_enabled_at(codex_home)
            {
                return Ok(());
            }
            if let Err(e) =
                crate::hooks::codex::try_setup_codex_hooks_at(include_permissions, codex_home)
            {
                if matches!(e, crate::hooks::codex::SetupError::HookTrustFailed { .. }) {
                    crate::log::log_warn(
                        "codex",
                        "codex.hook_trust_setup_warn",
                        &format!(
                            "Codex hook setup could not write trust state; launch preprocessing may fall back to hook-trust bypass: {e}"
                        ),
                    );
                } else {
                    let diag = install_diag_context(
                        tool,
                        &[
                            ("config_path", codex_home.join("config.toml")),
                            ("hooks_path", codex_home.join("hooks.json")),
                        ],
                    );
                    bail!(
                        "Failed to setup Codex hooks: {e}\n\
                         Run: hcom hooks add codex\n\
                         {diag}"
                    );
                }
            }
            Ok(())
        }
        LaunchTool::OpenCode => {
            match crate::hooks::opencode::ensure_plugin_installed("opencode") {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    let target = crate::hooks::opencode::get_opencode_plugin_path();
                    let diag = install_diag_context(tool, &[("plugin_path", target.clone())]);
                    bail!(
                        "{}",
                        format_plugin_install_error("OpenCode", "opencode", &target, &error, &diag,)
                    );
                }
            }
            let diag = install_diag_context(
                tool,
                &[(
                    "plugin_path",
                    crate::hooks::opencode::get_opencode_plugin_path(),
                )],
            );
            bail!("Failed to setup OpenCode plugin. Run: hcom hooks add opencode\n{diag}");
        }
        LaunchTool::Kilo => {
            match crate::hooks::opencode::ensure_plugin_installed("kilo") {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    let target = crate::hooks::opencode::get_kilo_plugin_path();
                    let diag = install_diag_context(tool, &[("plugin_path", target.clone())]);
                    bail!(
                        "{}",
                        format_plugin_install_error("Kilo Code", "kilo", &target, &error, &diag,)
                    );
                }
            }
            let diag = install_diag_context(
                tool,
                &[(
                    "plugin_path",
                    crate::hooks::opencode::get_kilo_plugin_path(),
                )],
            );
            bail!("Failed to setup Kilo Code plugin. Run: hcom hooks add kilo\n{diag}");
        }
        LaunchTool::Pi => {
            if crate::hooks::pi::ensure_pi_plugin_installed() {
                return Ok(());
            }
            let diag = install_diag_context(tool, &[]);
            bail!("Failed to setup Pi plugin. Run: hcom hooks add pi\n{diag}");
        }
        LaunchTool::Omp => {
            if crate::hooks::omp::ensure_omp_plugin_installed() {
                return Ok(());
            }
            let diag = install_diag_context(tool, &[]);
            bail!("Failed to setup Oh My Pi plugin. Run: hcom hooks add omp\n{diag}");
        }
        LaunchTool::Antigravity => {
            if !crate::hooks::plugin::verify_agy_plugin_installed() {
                eprintln!("{}", hooks_missing_warning(tool));
            }
            Ok(())
        }
        LaunchTool::Cursor => {
            if !crate::hooks::plugin::verify_cursor_plugin_installed() {
                eprintln!("{}", hooks_missing_warning(tool));
            }
            Ok(())
        }
        LaunchTool::Kimi => {
            if crate::hooks::kimi::verify_kimi_hooks_installed(include_permissions) {
                return Ok(());
            }
            if let Err(e) = crate::hooks::kimi::try_setup_kimi_hooks(include_permissions) {
                let diag = install_diag_context(
                    tool,
                    &[("hooks_path", crate::hooks::kimi::get_kimi_settings_path())],
                );
                bail!(
                    "Failed to setup Kimi hooks: {e}\n\
                     Run: hcom hooks add kimi\n\
                     {diag}"
                );
            }
            Ok(())
        }
        LaunchTool::Copilot => {
            if crate::hooks::copilot::verify_copilot_hooks_installed(include_permissions) {
                return Ok(());
            }
            if let Err(e) = crate::hooks::copilot::try_setup_copilot_hooks(include_permissions) {
                let diag = install_diag_context(
                    tool,
                    &[(
                        "hooks_path",
                        crate::hooks::copilot::get_copilot_hooks_path(),
                    )],
                );
                bail!(
                    "Failed to setup Copilot hooks: {e}\n\
                     Run: hcom hooks add copilot\n\
                     {diag}"
                );
            }
            Ok(())
        }
    }
}

/// Build a command string for Claude (non-PTY mode). Quoting matches the
/// shell that will run the resulting script: PowerShell on Windows, POSIX
/// shell elsewhere (see `launch_terminal`'s `create_powershell_script` /
/// `create_bash_script` split).
fn build_claude_command(args: &[String]) -> String {
    let mut parts = vec!["claude".to_string()];
    for arg in args {
        if cfg!(windows) {
            parts.push(terminal::ps_quote(arg));
        } else {
            parts.push(crate::tools::args_common::shell_quote(arg));
        }
    }
    parts.join(" ")
}

/// Tool-specific extra environment variables for PTY mode.
fn tool_extra_env(tool: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    // Claude is driven by the PTY wrapper (ConPTY on Windows, openpty on Unix),
    // which handles injection; HCOM_PTY_MODE tells the Stop hook to defer to it.
    if tool == "claude" {
        m.insert("HCOM_PTY_MODE".to_string(), "1".to_string());
    }
    if tool == "antigravity" {
        m.insert("ANTIGRAVITY_AGENT".to_string(), "1".to_string());
    }
    // herdr classifies an agent pane from its *foreground* process, which under
    // PTY mode is `hcom pty`, not the tool — so the pane never enters `herdr
    // agent list` and `agent.rename` keeps failing with "agent target not
    // found". HERDR_AGENT is herdr's documented hint for exactly this wrapper
    // case ("set HERDR_AGENT=<agent> on the wrapper command"); it names the
    // screen manifest to evaluate. Routed through the sidecar (non-HCOM_ key),
    // so it reaches `hcom pty` and dies with it rather than leaking into the
    // login shell the launch script drops to afterwards. Set unconditionally:
    // outside herdr nothing reads it, and on a nested spawn it must *overwrite*
    // the parent pane's inherited value so claude → codex names codex.
    m.insert("HERDR_AGENT".to_string(), tool.to_string());
    m
}

fn background_runner_env(
    tool: &str,
    env: &HashMap<String, String>,
    instance_name: &str,
) -> HashMap<String, String> {
    let mut runner_env = env.clone();
    runner_env.insert("HCOM_INSTANCE_NAME".to_string(), instance_name.to_string());
    // Default HCOM_TOOL when the caller didn't already set it (most callers
    // come from `launch()` which inserts it; this keeps the standalone PTY
    // and headless paths consistent so `{tool}` template substitution and
    // delivery-loop label formatting see the right value).
    runner_env
        .entry("HCOM_TOOL".to_string())
        .or_insert_with(|| tool.to_string());
    runner_env.extend(tool_extra_env(tool));
    runner_env
}

/// Non-HCOM ambient env to forward through the sidecar, with marker/identity/
/// instance-state/terminal-color vars stripped. On Windows env names are
/// case-insensitive (and the paired PowerShell `Remove-Item Env:` folds case),
/// so `case_insensitive` folds case for the match; Unix keeps exact-case.
fn sidecar_ambient_env<'a>(
    env: &HashMap<String, String>,
    strip_vars: impl Iterator<Item = &'a str>,
    case_insensitive: bool,
) -> HashMap<String, String> {
    let norm = |k: &str| {
        if case_insensitive {
            k.to_ascii_lowercase()
        } else {
            k.to_string()
        }
    };
    let strip: std::collections::HashSet<String> = strip_vars.map(&norm).collect();
    env.iter()
        .filter(|(k, _)| !k.starts_with("HCOM_") && !strip.contains(&norm(k)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

struct RunnerBinaries {
    path_dirs: Vec<String>,
    tool_path: Option<String>,
}

/// Resolve the executables needed by generated runners and order their directories.
///
/// The selected Node runtime must precede every other prepended directory: tool,
/// hcom, and Python directories can all contain an older `node`/`npx`. The tool is
/// resolved before PATH changes and passed explicitly to `hcom pty`, so moving its
/// directory after Node does not change which tool executable is launched.
fn resolve_runner_binaries(
    initial_dirs: Vec<String>,
    tool_bin: &str,
    mut which_bin: impl FnMut(&str) -> Option<String>,
) -> RunnerBinaries {
    let tool_path = which_bin(tool_bin);
    let hcom_path = which_bin("hcom");
    let node_path = which_bin("node");
    let python_path = which_bin("python3");
    let mut path_dirs = Vec::new();

    fn append_binary_dir(path_dirs: &mut Vec<String>, path: &str) {
        if let Some(dir) = Path::new(path).parent() {
            let dir = dir.to_string_lossy().into_owned();
            if !path_dirs.contains(&dir) {
                path_dirs.push(dir);
            }
        }
    }

    for path in node_path.iter() {
        append_binary_dir(&mut path_dirs, path);
    }
    for dir in initial_dirs {
        if !path_dirs.contains(&dir) {
            path_dirs.push(dir);
        }
    }
    for path in tool_path
        .iter()
        .chain(hcom_path.iter())
        .chain(python_path.iter())
    {
        append_binary_dir(&mut path_dirs, path);
    }

    RunnerBinaries {
        path_dirs,
        tool_path,
    }
}

/// Windows runner: a PowerShell script that launches the tool through the hcom
/// ConPTY wrapper (`hcom pty <tool>`), mirroring the Unix bash runner. The
/// wrapper runs the delivery loop so idle agents can be woken. Mirrors the bash
/// runner's env scrubbing, HCOM env, secret sidecar, and PATH setup.
fn create_runner_script_windows(
    tool: &str,
    cwd: &str,
    instance_name: &str,
    env: &HashMap<String, String>,
    tool_args: &[String],
    run_here: bool,
) -> Result<String> {
    let tool_spec = tool.parse::<crate::tool::Tool>().map(|t| t.spec()).ok();
    let instance_state_env: &[&str] = tool_spec.map(|s| s.instance_state_env).unwrap_or(&[]);

    let launch_dir = paths::hcom_path(&[paths::LAUNCH_DIR]);
    fs::create_dir_all(&launch_dir).ok();
    let script_file = launch_dir.join(format!(
        "{}_{}_{}_{}.ps1",
        tool,
        instance_name,
        std::process::id(),
        rand::random::<u16>() % 9000 + 1000
    ));

    // Visible HCOM_* env, plus the managed-launch marker so hooks engage.
    let mut hcom_env: HashMap<String, String> = env
        .iter()
        .filter(|(k, _)| k.starts_with("HCOM_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    hcom_env.insert("HCOM_LAUNCHED".to_string(), "1".to_string());
    let env_block = terminal::build_env_string(&hcom_env, "powershell");

    // Non-HCOM ambient env (may carry secrets) goes through a private sidecar
    // that is dot-sourced then deleted, matching the bash runner.
    let pane_identity_vars = if run_here {
        std::collections::HashSet::new()
    } else {
        crate::config::pane_identity_env_vars()
    };
    let ambient_env = sidecar_ambient_env(
        env,
        tool_marker_vars()
            .iter()
            .chain(HCOM_IDENTITY_VARS.iter())
            .chain(instance_state_env.iter())
            .chain(crate::terminal::TERMINAL_COLOR_VARS.iter())
            .copied()
            .chain(pane_identity_vars.iter().map(String::as_str)),
        true,
    );
    let sidecar_source = if ambient_env.is_empty() {
        String::new()
    } else {
        let env_file = launch_dir.join(format!(
            "{}_{}_{}_{}.ps1",
            tool,
            instance_name,
            std::process::id(),
            rand::random::<u16>() % 9000 + 1000
        ));
        let mut file = crate::sys::fs::create_private_new(&env_file)?;
        // Windows PowerShell 5.1 reads BOM-less files using the legacy ANSI
        // code page, corrupting non-ASCII env values; a UTF-8 BOM forces it
        // to read as UTF-8.
        file.write_all(b"\xEF\xBB\xBF")?;
        writeln!(
            file,
            "{}",
            terminal::build_env_string(&ambient_env, "powershell")
        )?;
        let q = terminal::ps_quote(&env_file.to_string_lossy());
        format!("if (Test-Path {q}) {{ . {q}; Remove-Item -Force {q} }}")
    };

    // Scrub inherited tool markers / identity / instance-state vars.
    let unset_names: Vec<String> = tool_marker_vars()
        .iter()
        .chain(HCOM_IDENTITY_VARS.iter())
        .chain(instance_state_env.iter())
        .map(|v| format!("Env:{v}"))
        .collect();
    let unset_line = if unset_names.is_empty() {
        String::new()
    } else {
        format!(
            "Remove-Item {} -ErrorAction SilentlyContinue",
            unset_names.join(",")
        )
    };

    // Resolve binary directories for minimal PATH environments.
    let mut path_dirs: Vec<String> = Vec::new();
    if let Ok(dev_root) = std::env::var("HCOM_DEV_ROOT")
        && let Some(bin) = crate::shared::dev_root_binary(Path::new(&dev_root))
        && let Some(dir) = bin.parent()
    {
        path_dirs.push(dir.to_string_lossy().into_owned());
    }
    // Ensure the launched tool (and its hooks) can call back to *this* hcom by
    // name — a dev binary may not be on the global PATH.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let d = dir.to_string_lossy().to_string();
        if !path_dirs.contains(&d) {
            path_dirs.push(d);
        }
    }
    let tool_bin = tool
        .parse::<crate::tool::Tool>()
        .map(|t| t.spec().cli_binary)
        .unwrap_or(tool);
    let binaries = resolve_runner_binaries(path_dirs, tool_bin, terminal::which_bin);
    let path_dirs = binaries.path_dirs;
    let path_line = if path_dirs.is_empty() {
        String::new()
    } else {
        format!(
            "$env:PATH = {} + $env:PATH",
            terminal::ps_quote(&format!("{};", path_dirs.join(";")))
        )
    };

    // Run through the hcom PTY wrapper (ConPTY) so the tool is driven by the
    // delivery loop — this is what wakes an idle agent on Windows. Mirrors the
    // Unix runner's `hcom pty <tool>` call.
    //
    // Tool args travel via a JSON sidecar file, not the command line: the
    // PowerShell → native-exe argv boundary mangles embedded double quotes
    // (powershell.exe passes them unescaped, so the child's command-line
    // parser re-splits at quote/space boundaries). Codex args always contain
    // quotes (`-c projects={ "path" = ... }`, developer_instructions), which
    // made `hcom codex` fail with "unexpected argument" (#66). Only the
    // sidecar path — generated by hcom, never quote-bearing — goes on the
    // command line. `hcom pty` reads and deletes the file.
    let hcom_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "hcom".to_string());
    let tool_path_arg = binaries
        .tool_path
        .as_deref()
        .map(|path| format!(" --hcom-tool-path {}", terminal::ps_quote(path)))
        .unwrap_or_default();
    let run_line = if tool_args.is_empty() {
        format!(
            "& {} pty {}{}",
            terminal::ps_quote(&hcom_bin),
            tool,
            tool_path_arg
        )
    } else {
        let args_file = launch_dir.join(format!(
            "{}_{}_{}_{}.args.json",
            tool,
            instance_name,
            std::process::id(),
            rand::random::<u16>() % 9000 + 1000
        ));
        let mut file = crate::sys::fs::create_private_new(&args_file)?;
        file.write_all(serde_json::to_string(tool_args)?.as_bytes())?;
        format!(
            "& {} pty {}{} --hcom-args-file {}",
            terminal::ps_quote(&hcom_bin),
            tool,
            tool_path_arg,
            terminal::ps_quote(&args_file.to_string_lossy())
        )
    };
    // `powershell -File` returns 0 unless the script exits with an explicit
    // code, so surface the wrapper's real exit status (agent failures, PTY
    // crashes, kill signals) instead of always reporting success.
    let run_line = format!("{run_line}\nexit $LASTEXITCODE");

    let display = tool
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .collect::<String>()
        + &tool[1..];
    let content = format!(
        "# {display} hcom native runner ({instance_name})\n\
         Set-Location {cwd}\n\
         {unset_line}\n\
         {env_block}\n\
         if ($env:HCOM_BACKGROUND) {{ Write-Host '[hcom runner] environment ready' }}\n\
         {sidecar_source}\n\
         {path_line}\n\
         if ($env:HCOM_BACKGROUND) {{ Write-Host '[hcom runner] starting PTY wrapper' }}\n\
         \n\
         {run_line}\n",
        cwd = terminal::ps_quote(cwd),
    );

    // Windows PowerShell 5.1 reads BOM-less files using the legacy ANSI code
    // page, corrupting non-ASCII usernames/paths/prompts/env values; a UTF-8
    // BOM forces it to read as UTF-8.
    let mut bytes = Vec::with_capacity(content.len() + 3);
    bytes.extend_from_slice(b"\xEF\xBB\xBF");
    bytes.extend_from_slice(content.as_bytes());
    fs::write(&script_file, &bytes)?;

    crate::log::log_info(
        "pty",
        "native.script",
        &format!(
            "script={} tool={} instance={} (windows ConPTY launch)",
            script_file.display(),
            tool,
            instance_name
        ),
    );

    Ok(script_file.to_string_lossy().to_string())
}

/// Create a bash script that runs a tool via the hcom native PTY wrapper.
///
/// The script sets up the environment and calls `hcom pty <tool> [args...]`.
pub fn create_runner_script(
    tool: &str,
    cwd: &str,
    instance_name: &str,
    env: &HashMap<String, String>,
    tool_args: &[String],
    run_here: bool,
) -> Result<String> {
    if cfg!(windows) {
        return create_runner_script_windows(tool, cwd, instance_name, env, tool_args, run_here);
    }
    // Resolve the tool's IntegrationSpec for instance-state env stripping
    let tool_spec = tool.parse::<crate::tool::Tool>().map(|t| t.spec()).ok();
    let instance_state_env: &[&str] = tool_spec.map(|s| s.instance_state_env).unwrap_or(&[]);
    let native_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("hcom"));
    let native_bin_str = native_bin.to_string_lossy();

    let launch_dir = paths::hcom_path(&[paths::LAUNCH_DIR]);
    fs::create_dir_all(&launch_dir).ok();

    let script_file = launch_dir.join(format!(
        "{}_{}_{}_{}.sh",
        tool,
        instance_name,
        std::process::id(),
        rand::random::<u16>() % 9000 + 1000
    ));

    // Route ALL forwarded non-HCOM env vars through the 0600 sidecar.
    // The visible .sh only exports HCOM_* vars + PATH + cwd (minimal).
    // This avoids the sensitivity-classification heuristic entirely — no
    // secret ever lands in the 0755 world-readable script.
    let hcom_env: HashMap<String, String> = env
        .iter()
        .filter(|(k, _)| k.starts_with("HCOM_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let pane_identity_vars = if run_here {
        std::collections::HashSet::new()
    } else {
        crate::config::pane_identity_env_vars()
    };
    let ambient_env = sidecar_ambient_env(
        env,
        tool_marker_vars()
            .iter()
            .chain(HCOM_IDENTITY_VARS.iter())
            .chain(instance_state_env.iter())
            .chain(crate::terminal::TERMINAL_COLOR_VARS.iter())
            .copied()
            .chain(pane_identity_vars.iter().map(String::as_str)),
        false,
    );
    let env_block = terminal::build_env_string(&hcom_env, "bash_export");
    let sensitive_env_source = if ambient_env.is_empty() {
        String::new()
    } else {
        let env_file = launch_dir.join(format!(
            "{}_{}_{}_{}.env",
            tool,
            instance_name,
            std::process::id(),
            rand::random::<u16>() % 9000 + 1000
        ));
        let mut file = crate::sys::fs::create_private_new(&env_file)?;
        writeln!(
            file,
            "{}",
            terminal::build_env_string(&ambient_env, "bash_export")
        )?;
        let quoted = crate::tools::args_common::shell_quote(&env_file.to_string_lossy());
        format!("if [ -f {quoted} ]; then\n  . {quoted}\n  rm -f {quoted}\nfi")
    };
    let tool_args_str: String = tool_args
        .iter()
        .map(|a| crate::tools::args_common::shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    // Resolve binary paths for minimal PATH environments
    let mut path_dirs: Vec<String> = Vec::new();

    // Dev mode: prepend the worktree's Cargo output dir
    if let Ok(dev_root) = std::env::var("HCOM_DEV_ROOT")
        && let Some(bin) = crate::shared::dev_root_binary(Path::new(&dev_root))
        && let Some(dir) = bin.parent()
    {
        path_dirs.push(dir.to_string_lossy().into_owned());
    }

    let tool_bin = tool
        .parse::<crate::tool::Tool>()
        .map(|t| t.spec().cli_binary)
        .unwrap_or(tool);
    let binaries = resolve_runner_binaries(path_dirs, tool_bin, terminal::which_bin);
    let path_dirs = binaries.path_dirs;

    let path_export = if !path_dirs.is_empty() {
        format!("export PATH=\"{}:$PATH\"", path_dirs.join(":"))
    } else {
        String::new()
    };

    let use_exec = if run_here { "" } else { "exec " };
    let tool_path_arg = binaries
        .tool_path
        .as_deref()
        .map(|path| {
            format!(
                " --hcom-tool-path {}",
                crate::tools::args_common::shell_quote(path)
            )
        })
        .unwrap_or_default();

    let content = format!(
        "#!/bin/bash\n\
         # {} hcom native PTY runner ({})\n\
         # Using: {}\n\
         cd {}\n\
         \n\
         unset {}\n\
         unset {}\n\
         unset {}\n\
         {}\n\
         {}\n\
         {}\n\
         \n\
         {}{} pty {}{} {}\n",
        tool.chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .collect::<String>()
            + &tool[1..],
        instance_name,
        native_bin_str,
        crate::tools::args_common::shell_quote(cwd),
        tool_marker_vars().join(" "),
        HCOM_IDENTITY_VARS.join(" "),
        instance_state_env.join(" "),
        env_block,
        sensitive_env_source,
        path_export,
        use_exec,
        crate::tools::args_common::shell_quote(&native_bin_str),
        tool,
        tool_path_arg,
        tool_args_str,
    );

    fs::write(&script_file, &content)?;
    crate::sys::fs::set_executable(&script_file)?;

    crate::log::log_info(
        "pty",
        "native.script",
        &format!(
            "script={} tool={} instance={} forwarded_keys=[{}]",
            script_file.display(),
            tool,
            instance_name,
            ambient_env.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    );

    Ok(script_file.to_string_lossy().to_string())
}

/// Build the command that runs a generated runner script in the launched
/// terminal. On Windows the outer launcher is already PowerShell, so invoke
/// the runner in that process instead of starting a second PowerShell host.
/// Besides avoiding needless startup cost, this removes a launch stage that
/// can intermittently stall before `hcom pty` is reached.
fn runner_invocation_command_for_platform(script_file: &str, windows: bool) -> String {
    if windows {
        format!("& {}", crate::terminal::ps_quote(script_file))
    } else {
        format!(
            "bash {}",
            crate::tools::args_common::shell_quote(script_file)
        )
    }
}

fn runner_invocation_command(script_file: &str) -> String {
    runner_invocation_command_for_platform(script_file, cfg!(windows))
}

/// Launch a tool via PTY wrapper in a terminal.
#[allow(clippy::too_many_arguments)]
pub fn launch_pty(
    tool: &str,
    cwd: &str,
    env: &HashMap<String, String>,
    instance_name: &str,
    tool_args: &[String],
    run_here: bool,
    terminal: Option<&str>,
    inside_ai_tool: bool,
) -> Result<bool> {
    if env.get("HCOM_PROCESS_ID").is_none_or(|v| v.is_empty()) {
        crate::log::log_error(
            "pty",
            "pty.exit",
            &format!("HCOM_PROCESS_ID not set in env for {}", instance_name),
        );
        return Ok(false);
    }

    let mut runner_env = env.clone();
    runner_env.insert("HCOM_INSTANCE_NAME".to_string(), instance_name.to_string());
    runner_env
        .entry("HCOM_TOOL".to_string())
        .or_insert_with(|| tool.to_string());
    runner_env.extend(tool_extra_env(tool));

    let script_file =
        create_runner_script(tool, cwd, instance_name, &runner_env, tool_args, run_here)?;

    let command = runner_invocation_command(&script_file);
    let terminal_env: HashMap<String, String> = runner_env
        .iter()
        .filter(|(k, _)| k.starts_with("HCOM_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let (launch_result, effective_preset) = terminal::launch_terminal(
        &command,
        &terminal_env,
        Some(cwd),
        false, // not background
        run_here,
        terminal,
        inside_ai_tool,
        Some(tool),
    )?;

    instance_binding::persist_terminal_launch_context(
        &crate::db::HcomDb::open()?,
        instance_name,
        terminal,
        &effective_preset,
        env.get("HCOM_PROCESS_ID").map(|s| s.as_str()),
    );

    match launch_result {
        terminal::LaunchResult::Success => Ok(true),
        terminal::LaunchResult::Background(_, _) => Ok(true),
        terminal::LaunchResult::Failed(_) => Ok(false),
    }
}

/// Identity and tracking context for a background launch, shared across tool types.
struct BackgroundLaunchCtx<'a> {
    db: &'a HcomDb,
    tool: &'a str,
    instance_name: &'a str,
    process_id: &'a str,
    terminal_mode: Option<&'a str>,
    tag: &'a str,
    working_dir: &'a str,
    log_files: &'a mut Vec<String>,
    handles: &'a mut Vec<serde_json::Value>,
}

/// Shared bookkeeping after a successful background launch for gemini/codex/opencode.
/// Persists the launch context, updates position, records the PID, and appends
/// log_file / handle entries. Per-tool differences (args, prompt) stay in the caller.
fn finalize_background_launch(
    ctx: &mut BackgroundLaunchCtx<'_>,
    log_file: String,
    pid: u32,
    effective_preset: String,
) {
    instance_binding::persist_terminal_launch_context(
        ctx.db,
        ctx.instance_name,
        ctx.terminal_mode,
        &effective_preset,
        Some(ctx.process_id),
    );
    instances::update_instance_position(
        ctx.db,
        ctx.instance_name,
        &serde_json::Map::from_iter([
            ("pid".to_string(), json!(pid)),
            ("background_log_file".to_string(), json!(&log_file)),
        ]),
    );
    crate::pidtrack::record_pid(&crate::pidtrack::PidRecord {
        process_id: ctx.process_id,
        terminal_preset: &effective_preset,
        tag: ctx.tag,
        ..crate::pidtrack::PidRecord::new(
            &crate::paths::hcom_dir(),
            pid,
            ctx.tool,
            ctx.instance_name,
            ctx.working_dir,
        )
    });
    ctx.log_files.push(log_file.clone());
    ctx.handles.push(json!({
        "tool": ctx.tool,
        "instance_name": ctx.instance_name,
        "log_file": log_file,
        "pid": pid,
    }));
}

fn launch_background_runner(
    tool: &str,
    cwd: &str,
    instance_name: &str,
    instance_env: &mut HashMap<String, String>,
    tool_args: &[String],
    terminal_mode: Option<&str>,
    inside_ai_tool: bool,
) -> Result<(String, u32, String)> {
    let log_filename = format!(
        "background_{}_{}.log",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        rand::random::<u16>() % 9000 + 1000
    );
    let mut runner_env = background_runner_env(tool, instance_env, instance_name);
    runner_env.insert("HCOM_BACKGROUND".to_string(), log_filename);
    let script_file =
        create_runner_script(tool, cwd, instance_name, &runner_env, tool_args, false)?;
    let command = runner_invocation_command(&script_file);
    let terminal_env: HashMap<String, String> = runner_env
        .iter()
        .filter(|(k, _)| k.starts_with("HCOM_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (launch_result, effective_preset) = terminal::launch_terminal(
        &command,
        &terminal_env,
        Some(cwd),
        true,
        false,
        terminal_mode,
        inside_ai_tool,
        Some(tool),
    )?;
    match launch_result {
        terminal::LaunchResult::Background(log_file, pid) => Ok((log_file, pid, effective_preset)),
        _ => bail!("background launch failed"),
    }
}

/// Common launch path for gemini/codex/opencode: background or PTY foreground.
///
/// Handles `launch_background_runner` + `finalize_background_launch` for background,
/// and `will_run_in_current_terminal` + `launch_pty` for foreground.
fn launch_pty_or_background(
    ctx: &mut BackgroundLaunchCtx<'_>,
    instance_env: &mut HashMap<String, String>,
    tool_args: &[String],
    params: &LaunchParams,
    inside_ai_tool: bool,
) -> Result<bool> {
    if params.background {
        let (log_file, pid, effective_preset) = launch_background_runner(
            ctx.tool,
            ctx.working_dir,
            ctx.instance_name,
            instance_env,
            tool_args,
            ctx.terminal_mode,
            inside_ai_tool,
        )?;
        finalize_background_launch(ctx, log_file, pid, effective_preset);
        Ok(true)
    } else {
        let effective_run_here = will_run_in_current_terminal(
            params.count,
            false,
            params.run_here,
            ctx.terminal_mode,
            inside_ai_tool,
        );
        let ok = launch_pty(
            ctx.tool,
            ctx.working_dir,
            instance_env,
            ctx.instance_name,
            tool_args,
            effective_run_here,
            ctx.terminal_mode,
            inside_ai_tool,
        )?;
        if ok {
            ctx.handles
                .push(json!({"tool": ctx.tool, "instance_name": ctx.instance_name}));
        }
        Ok(ok)
    }
}

/// Resolve a naming conflict for an explicit instance name.
///
/// - Name is free → Ok(()).
/// - Name held by an inactive row → consume the row (delete) and return Ok(()).
///   An inactive row is a resume handle from agy soft-finalize; launch will
///   re-create a fresh row with the same name.
/// - Name held by a `pending` placeholder reservation → Ok(()) without
///   deleting. This is *our own* reservation: the fork/resume path calls
///   `reserve_generated_name` (under flock, against an unused name) before the
///   launch, then passes that name as `params.name`. The pre-register step
///   (`initialize_instance_in_position_file`) promotes the placeholder in
///   place, so it must survive — bailing here broke every tracked `hcom f`.
/// - Name held by anything else (listening/active/blocked) → Err.
fn resolve_explicit_name_conflict(db: &HcomDb, name: &str) -> Result<()> {
    let Some(row) = db.get_instance(name).ok().flatten() else {
        return Ok(());
    };
    let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "inactive" {
        db.delete_instance(name).map_err(|e| {
            anyhow::anyhow!("Failed to clear inactive resume row '{}': {}", name, e)
        })?;
        return Ok(());
    }
    // A pending placeholder with no session yet is a reservation, not a live
    // agent — leave it for the launcher's pre-register promotion.
    let status_context = row
        .get("status_context")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_empty = row
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true);
    if status == instance_names::PLACEHOLDER_STATUS
        && status_context == instance_names::PLACEHOLDER_CONTEXT
        && session_empty
    {
        return Ok(());
    }
    bail!(
        "Instance '{}' already exists (stop it first or use a different name)",
        name
    );
}

/// Inject ephemeral workspace trust flags into args for gemini and codex.
///
/// - gemini: adds `--skip-trust` (session-scoped, no persisted state)
/// - codex: adds `-c` with a TOML inline-table value that sets trust for the
///   canonical CWD. Uses key `projects` (no dots in the key path) so codex's
///   naive `.`-splitting never touches the path itself, making it robust to
///   dotted directory components. The path is TOML-escaped (`\` and `"`) —
///   every Windows path carries backslashes, and unescaped they made codex
///   reject the value as "invalid type: string, expected a map".
///
/// When `auto_trust` is false, returns immediately without modifying args.
/// Idempotent: no-op if the relevant flag is already present.
/// Cursor is handled separately via `ensure_cursor_workspace_trusted`.
pub(crate) fn inject_workspace_trust_args(
    tool: &LaunchTool,
    canonical_dir: &std::path::Path,
    args: &mut Vec<String>,
    auto_trust: bool,
) {
    if !auto_trust {
        return;
    }
    match tool {
        LaunchTool::Gemini if !args.iter().any(|a| a == "--skip-trust") => {
            args.push("--skip-trust".to_string());
        }
        LaunchTool::Gemini => {}
        LaunchTool::Codex => {
            let already_set = args
                .windows(2)
                .any(|w| w[0] == "-c" && w[1].contains("trust_level"));
            if !already_set {
                let canonical_str = canonical_dir.to_string_lossy();
                // std::fs::canonicalize returns \\?\-prefixed verbatim paths on
                // Windows; codex tracks projects by their ordinary absolute
                // form, so strip the prefix or the trust key never matches.
                let simplified = if let Some(unc) = canonical_str.strip_prefix(r"\\?\UNC\") {
                    format!(r"\\{unc}")
                } else {
                    canonical_str
                        .strip_prefix(r"\\?\")
                        .unwrap_or(&canonical_str)
                        .to_string()
                };
                // codex -c: key="projects" (no dots → no split issue), value is a TOML
                // inline table with the quoted path as key. apply_single_override replaces
                // the projects table for this session only (file stays untouched).
                // Escape backslashes before quotes: the quote escape introduces
                // a backslash that must not itself be doubled.
                let toml_escaped = crate::runtime_env::toml_escape_path(&simplified);
                let trust_override = format!(
                    "projects={{ \"{}\" = {{ trust_level = \"trusted\" }} }}",
                    toml_escaped
                );
                args.push("-c".to_string());
                args.push(trust_override);
            }
        }
        _ => {}
    }
}

fn validate_launch_count(tool: &LaunchTool, count: usize) -> Result<()> {
    if count == 0 {
        bail!("Count must be positive");
    }
    let max = tool.spec().launch.max_launch_count;
    if count > max {
        bail!(
            "Too many {} instances requested (max {})",
            tool.as_str(),
            max
        );
    }
    Ok(())
}

fn inject_omp_extension_args(tool: &LaunchTool, args: &mut Vec<String>) {
    if !matches!(tool, LaunchTool::Omp) {
        return;
    }
    let extension_args = crate::hooks::omp::extension_inject_args();
    let plugin_path = extension_args
        .get(1)
        .map(String::as_str)
        .unwrap_or_default();
    let already_present = args
        .windows(2)
        .any(|window| matches!(window, [flag, path] if flag == "-e" && path == plugin_path));
    if !already_present {
        args.extend(extension_args);
    }
}

fn append_initial_prompt_args(
    tool: &LaunchTool,
    args: &mut Vec<String>,
    prompt: String,
) -> Result<()> {
    match tool.spec().launch.initial_prompt {
        crate::integration_spec::InitialPromptShape::Unsupported { reason } => bail!("{reason}"),
        crate::integration_spec::InitialPromptShape::DashDashPositional => {
            args.push("--".to_string());
            args.push(prompt);
        }
        crate::integration_spec::InitialPromptShape::Positional => args.push(prompt),
        crate::integration_spec::InitialPromptShape::Flag(flag) => {
            args.push(flag.to_string());
            args.push(prompt);
        }
    }
    Ok(())
}

/// Launch one or more AI tool instances with consistent tracking.
///
/// This is the unified entry point for launching Claude, Gemini, Codex,
/// and OpenCode instances with batch tracking, environment setup, and
/// error handling.
pub fn launch(db: &HcomDb, mut params: LaunchParams) -> Result<LaunchResult> {
    // Claude background defaults to the live PTY surface (`ClaudePty`); it only
    // drops to the `-p`/`--print` surface (`Claude`/`NativePrint`) when the
    // caller explicitly passes `-p`/`--print` in the args. Both stay alive —
    // PTY hosts the live TUI, print mode loops via the stop hook. `-p` is gated
    // behind explicit opt-in because, from 2026-06-15, `claude -p` draws from a
    // separate Agent SDK credit pool on subscription plans.
    // Exact-token match is intentional: `--print` is a boolean flag, so an
    // equals form (`--print=x`) is malformed for claude and surfaces as a
    // launch failure rather than being silently mis-routed to the PTY surface.
    let claude_print = (params.tool == "claude" || params.tool == "claude-pty")
        && params
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-p" | "--print"));

    // `claude-pty` is the PTY surface; `-p`/`--print` selects the NativePrint
    // surface. The two are mutually exclusive: passing both would route print
    // mode through the HeadlessPty runner, which is broken. Fail fast.
    if params.tool == "claude-pty" && claude_print {
        bail!(
            "The PTY surface does not support `-p`/`--print`.\n\
             Use one of:\n\
             • `hcom claude --headless`  — live PTY session\n\
             • `hcom claude -p ...`      — print/pipe mode"
        );
    }

    let normalized = if params.tool == "claude" && !claude_print {
        LaunchTool::ClaudePty
    } else {
        LaunchTool::from_str(&params.tool)?
    };
    let base_tool = normalized.base_tool();
    let backend = LaunchBackend::resolve(&normalized, params.background);

    // Validation
    validate_launch_count(&normalized, params.count)?;

    // HCOM_DIR placement: refuse if it sits under a tool-protected metadata
    // directory. codex hard-denies apply_patch into these via
    // FileSystemSandboxPolicy with no approval path; claude/gemini gate them
    // behind permission prompts on every hcom write. Either way the user gets
    // a broken session — fail fast at launch with a clear message instead.
    let hcom_dir_path = paths::hcom_dir();
    if let Some(protected) = paths::protected_hcom_dir_component(&hcom_dir_path) {
        bail!(
            "HCOM_DIR ({}) sits under a protected directory component '{}'.\n\
             AI tools (codex/claude/gemini) deny writes under .git/.codex/.claude/.agents,\n\
             which would block hcom DB writes from the launched agent.\n\
             Set HCOM_DIR to a path outside these directories.",
            hcom_dir_path.display(),
            protected
        );
    }

    let tool_binary = normalized.cli_binary();
    if !is_tool_installed(tool_binary) {
        bail!("'{}' is not installed or not in PATH", tool_binary);
    }

    if !params.skip_validation {
        let validation_errors = validate_tool_args(&normalized, &params.args);
        if !validation_errors.is_empty() {
            bail!("{}", validation_errors.join("\n"));
        }
    }

    // Load config before hook setup so auto_approve is authoritative for
    // wrapped launches as well as manual `hcom hooks add`.
    let hcom_config = HcomConfig::load(None).unwrap_or_else(|e| {
        eprintln!("[hcom] warn: config load failed, using defaults: {e}");
        let mut c = HcomConfig::default();
        c.normalize();
        c
    });

    let inside_ai_tool = crate::shared::context::HcomContext::from_os().is_inside_ai_tool();
    let terminal_mode = params
        .terminal
        .as_deref()
        .or(Some(hcom_config.terminal.as_str()).filter(|t| !t.is_empty()));
    let base_env_run_here = will_run_in_current_terminal(
        params.count,
        params.background,
        params.run_here,
        terminal_mode,
        inside_ai_tool,
    );

    // Build base environment for the current launch regime, then overlay
    // config.toml + ~/.hcom/env which win.
    let mut base_env = build_launch_env(
        &hcom_config,
        launch_env_regime(base_env_run_here, inside_ai_tool),
    );
    if let Some(ref caller_env) = params.env {
        for (key, value) in caller_env {
            insert_effective_env(&mut base_env, key.clone(), value.clone(), cfg!(windows));
        }
    }
    base_env.remove("HCOM_TERMINAL");
    ensure_tool_config_env(&normalized, &mut base_env);

    let working_dir = params.cwd.as_deref().unwrap_or(".");
    let canonical_dir = std::fs::canonicalize(working_dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(working_dir));

    // Codex preflight and hook setup must use the same effective CODEX_HOME as
    // the child, including overrides from ~/.hcom/env and caller-provided env.
    let codex_home = if matches!(normalized, LaunchTool::Codex) {
        crate::tools::codex_preprocessing::resolve_codex_home_from_env(&base_env, &canonical_dir)
    } else {
        None
    };
    if let Some((ref path, explicit_env)) = codex_home {
        insert_effective_env(
            &mut base_env,
            "CODEX_HOME".to_string(),
            path.to_string_lossy().into_owned(),
            cfg!(windows),
        );
        crate::tools::codex_preprocessing::ensure_codex_home_writable_at(path, explicit_env)?;
    }

    // Ensure hooks are installed (strict: refuse to launch without hooks)
    ensure_hooks_installed(
        &normalized,
        hcom_config.auto_approve,
        codex_home.as_ref().map(|(path, _)| path.as_path()),
        &canonical_dir,
    )?;

    // Tag resolution
    let effective_tag = if let Some(ref tag) = params.tag {
        base_env.insert("HCOM_TAG".to_string(), tag.clone());
        tag.clone()
    } else if let Some(tag) = base_env.get("HCOM_TAG").cloned() {
        tag
    } else {
        let default = hcom_config.tag.clone();
        if !default.is_empty() {
            base_env.insert("HCOM_TAG".to_string(), default.clone());
        }
        default
    };

    // Explicit name validation
    if let Some(ref name) = params.name {
        if params.count > 1 {
            bail!(
                "Cannot use explicit name with count > 1 (count={})",
                params.count
            );
        }
        resolve_explicit_name_conflict(db, name)?;
    }

    // System prompt file for Gemini/Codex
    if let Some(ref sp) = params.system_prompt
        && normalized == LaunchTool::Gemini
    {
        let path = write_system_prompt_file(sp, "gemini");
        base_env.insert("GEMINI_SYSTEM_MD".to_string(), path);
    }

    // Folder trust: on first run each tool shows a "do you trust this folder?"
    // prompt — the user accepts to continue or declines and it exits. When an
    // agent launches another agent via hcom, auto-approve the prompt for the
    // launch dir to smooth the process. Cursor's lever is a marker file (its
    // `--trust` flag is print-only, inert in our PTY), so it's seeded here;
    // gemini/codex use arg injection below.
    if hcom_config.auto_trust_workspace && matches!(normalized, LaunchTool::Cursor) {
        cursor_preprocessing::ensure_cursor_workspace_trusted(&canonical_dir)?;
    }
    if hcom_config.auto_trust_workspace && matches!(normalized, LaunchTool::Copilot) {
        copilot_preprocessing::ensure_copilot_workspace_trusted(&canonical_dir)?;
    }

    // Scrub any hcom-managed OMP extension arg a previous hcom version may have
    // baked into the stored args, from BOTH the live and the persisted vectors
    // (the snapshot below prefers persisted_args, and resume supplies that
    // historical vector separately). Without this, replaying an old session can
    // pass a stale `-e <old hcom.ts>` alongside the freshly injected current
    // path — failing startup or loading hcom twice. User extensions are kept.
    if matches!(normalized, LaunchTool::Omp) {
        crate::hooks::omp::strip_managed_extension_args(&mut params.args);
        if let Some(persisted) = params.persisted_args.as_mut() {
            crate::hooks::omp::strip_managed_extension_args(persisted);
        }
    }

    // Capture the persistable args BEFORE any hcom launch injection below.
    // Resume replays only user/config args; workspace-trust injection
    // (gemini `--skip-trust`, codex `-c projects=…trust_level`), the OMP
    // delivery-extension path (`-e <abs hcom.ts>`), and the `--hcom-prompt`
    // translation are session/path-specific and must not be baked into
    // launch_args, or they would replay stale state on resume/fork (e.g. a
    // stale plugin path if PI_CODING_AGENT_DIR or the install location moves).
    let stored_launch_args = params
        .persisted_args
        .clone()
        .unwrap_or_else(|| params.args.clone());

    // Injected after the snapshot so the internal plugin path is never persisted;
    // resume re-injects the current path via the same call.
    inject_omp_extension_args(&normalized, &mut params.args);

    // Resolved here, before any trust injection, and threaded to
    // preprocess_codex_args below. Codex's hook-trust bypass is invocation-wide,
    // so a bypass hcom grants on the strength of a local hook scan must not be
    // paired with a project layer that hcom itself just marked trusted — that
    // layer could contribute a hook source the scan never saw.
    let codex_hook_trust = if matches!(normalized, LaunchTool::Codex) {
        codex_preprocessing::resolve_codex_hook_trust_at(
            &params.args,
            &canonical_dir,
            codex_home
                .as_ref()
                .map(|(path, _)| path.as_path())
                .expect("Codex launch must resolve CODEX_HOME"),
        )
    } else {
        codex_preprocessing::CodexHookTrustOutcome::NoActionNeeded
    };

    inject_workspace_trust_args(
        &normalized,
        &canonical_dir,
        &mut params.args,
        hcom_config.auto_trust_workspace && !codex_hook_trust.suppresses_workspace_trust(),
    );
    let launcher_name: String = params.launcher.take().unwrap_or_else(|| {
        // Try to resolve caller identity from the live process binding.
        let process_id = std::env::var("HCOM_PROCESS_ID").ok();
        match crate::identity::resolve_identity(db, None, None, None, process_id.as_deref(), None) {
            Ok(id) => id.name,
            Err(_) => "api".to_string(),
        }
    });

    // Inject --hcom-prompt into tool args (translated per-tool).
    // When a real hcom participant launched us, append a reply instruction so
    // the spawned agent knows to send its result back.
    if let Some(ref prompt) = params.initial_prompt {
        let reply_suffix =
            if params.append_reply_handoff && launcher_name != "api" && launcher_name != "user" {
                format!("\n\nWhen done, send your result back to @{launcher_name} via hcom.")
            } else {
                String::new()
            };
        let full_prompt = format!("{prompt}{reply_suffix}");
        append_initial_prompt_args(&normalized, &mut params.args, full_prompt)?;
    }
    let batch_id = params
        .batch_id
        .take()
        .unwrap_or_else(|| format!("{:08x}", rand::rng().random::<u32>()));

    let mut launched = 0usize;
    let mut log_files: Vec<String> = Vec::new();
    let mut handles: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();

    for _ in 0..params.count {
        let mut instance_env = base_env.clone();
        instance_env.insert("HCOM_LAUNCHED".to_string(), "1".to_string());
        instance_env.insert(
            "HCOM_LAUNCH_EVENT_ID".to_string(),
            db.get_last_event_id().to_string(),
        );
        instance_env.insert("HCOM_LAUNCHED_BY".to_string(), launcher_name.to_string());
        instance_env.insert("HCOM_LAUNCH_BATCH_ID".to_string(), batch_id.clone());
        instance_env.insert(
            "HCOM_DIR".to_string(),
            paths::hcom_dir().to_string_lossy().to_string(),
        );

        // Propagate dev root
        if let Ok(val) = std::env::var("HCOM_DEV_ROOT") {
            instance_env.insert("HCOM_DEV_ROOT".to_string(), val);
        }
        // Propagate HCOM_NOTES from the launcher's own environment so a nested
        // launch (an agent that inherited notes spawning a child) doesn't lose
        // them — but only as a fallback (see `apply_inherited_notes`).
        apply_inherited_notes(&mut instance_env, std::env::var("HCOM_NOTES").ok());

        let process_id = generate_process_id();
        instance_env.insert("HCOM_PROCESS_ID".to_string(), process_id.clone());

        // Fork mode detection
        if matches!(normalized, LaunchTool::Claude | LaunchTool::ClaudePty)
            && params.args.iter().any(|a| a == "--fork-session")
        {
            instance_env.insert("HCOM_IS_FORK".to_string(), "1".to_string());
        }

        let instance_name = if let Some(ref name) = params.name {
            name.clone()
        } else {
            instance_names::generate_unique_name(db)?
        };
        instance_env.insert("HCOM_INSTANCE_NAME".to_string(), instance_name.clone());

        // Process ID export: allow custom env var name
        if let Ok(export_var) = std::env::var("HCOM_PROCESS_ID_EXPORT")
            && !export_var.is_empty()
        {
            instance_env.insert(export_var, process_id.clone());
        }

        // Name/process export vars
        if let Ok(export_var) = std::env::var("HCOM_NAME_EXPORT") {
            if !export_var.is_empty() {
                instance_env.insert(export_var, instance_name.clone());
            }
        } else if !hcom_config.name_export.is_empty() {
            instance_env.insert(hcom_config.name_export.clone(), instance_name.clone());
        }

        let tool_type = base_tool;
        instance_env.insert("HCOM_TOOL".to_string(), tool_type.to_string());

        // Pre-format the pane title for templates that substitute
        // `{pane_title}` (custom user templates only — the built-in herdr
        // preset passes `{instance_name}` and the delivery loop pushes the
        // styled label via `pane.rename`). `display_for_title` mirrors
        // `identity::get_display_name` for the about-to-be-created instance
        // row.
        let display_for_title = if effective_tag.is_empty() {
            instance_name.clone()
        } else {
            format!("{}-{}", effective_tag, instance_name)
        };
        instance_env.insert(
            "HCOM_PANE_TITLE".to_string(),
            crate::shared::format_pane_title(
                crate::shared::ST_LISTENING,
                &display_for_title,
                tool_type,
            ),
        );

        // Pre-register instance
        if let Err(e) = (|| -> Result<()> {
            instance_binding::initialize_instance_in_position_file(
                db,
                &instance_name,
                params.prior_session_id.as_deref(),
                None,            // parent_session_id
                None,            // parent_name
                None,            // agent_id
                None,            // transcript_path
                Some(tool_type), // tool
                params.background,
                if effective_tag.is_empty() {
                    None
                } else {
                    Some(effective_tag.as_str())
                },
                None,              // wait_timeout
                None,              // subagent_timeout
                None,              // hints
                Some(working_dir), // cwd_override: use launch params cwd, not current_dir()
            );
            db.set_process_binding(&process_id, "", &instance_name)?;
            Ok(())
        })() {
            errors.push(json!({"tool": base_tool, "error": e.to_string()}));
            continue;
        }

        // Dispatch to tool-specific launcher
        let launch_result = (|| -> Result<bool> {
            match normalized {
                LaunchTool::Claude => {
                    let claude_cmd = build_claude_command(&params.args);

                    // Store launch_args
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );

                    // LaunchTool::Claude only resolves to NativePrint (background,
                    // direct spawn in print mode) or InteractiveVisible — the
                    // PTY-backed variants live in LaunchTool::ClaudePty below.
                    if matches!(backend, LaunchBackend::NativePrint) {
                        let log_filename = format!(
                            "background_{}_{}.log",
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                            rand::random::<u16>() % 9000 + 1000
                        );
                        instance_env.insert("HCOM_BACKGROUND".to_string(), log_filename.clone());

                        let (launch_result, effective_preset) = terminal::launch_terminal(
                            &claude_cmd,
                            &instance_env,
                            Some(working_dir),
                            true, // background
                            false,
                            terminal_mode,
                            inside_ai_tool,
                            Some("claude"),
                        )?;
                        match launch_result {
                            terminal::LaunchResult::Background(log_file, pid) => {
                                finalize_background_launch(
                                    &mut BackgroundLaunchCtx {
                                        db,
                                        tool: "claude",
                                        instance_name: &instance_name,
                                        process_id: &process_id,
                                        terminal_mode,
                                        tag: params.tag.as_deref().unwrap_or(""),
                                        working_dir,
                                        log_files: &mut log_files,
                                        handles: &mut handles,
                                    },
                                    log_file,
                                    pid,
                                    effective_preset,
                                );
                                Ok(true)
                            }
                            _ => Ok(false),
                        }
                    } else {
                        let effective_run_here = will_run_in_current_terminal(
                            params.count,
                            false,
                            params.run_here,
                            terminal_mode,
                            inside_ai_tool,
                        );
                        let (launch_result, effective_preset) = terminal::launch_terminal(
                            &claude_cmd,
                            &instance_env,
                            Some(working_dir),
                            false,
                            effective_run_here,
                            terminal_mode,
                            inside_ai_tool,
                            Some("claude"),
                        )?;
                        instance_binding::persist_terminal_launch_context(
                            db,
                            &instance_name,
                            terminal_mode,
                            &effective_preset,
                            Some(&process_id),
                        );

                        match launch_result {
                            terminal::LaunchResult::Success => {
                                handles.push(
                                    json!({"tool": "claude", "instance_name": instance_name}),
                                );
                                Ok(true)
                            }
                            _ => Ok(false),
                        }
                    }
                }

                LaunchTool::ClaudePty => {
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );
                    // Same background/foreground split as gemini/codex/opencode:
                    // foreground → visible PTY in a terminal; background → PTY
                    // wrapper in a detached runner. The wrapper handles the TUI
                    // the same way either way, which is what lets PTY-headless
                    // claude keep a live session that accepts hcom inject.
                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "claude",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }

                LaunchTool::Gemini => {
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );
                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "gemini",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }

                LaunchTool::Codex => {
                    // Bootstrap delivered via developer_instructions at launch
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([("name_announced".to_string(), json!(true))]),
                    );

                    // Build effective args: system_prompt + preprocessing
                    let mut effective_args = params.args.clone();
                    if let Some(ref sp) = params.system_prompt {
                        let mut pre =
                            vec!["-c".to_string(), format!("developer_instructions={}", sp)];
                        pre.extend(effective_args);
                        effective_args = pre;
                    }

                    // Generate bootstrap text for preprocessing
                    let bootstrap = build_codex_bootstrap(
                        db,
                        &paths::hcom_dir(),
                        &instance_name,
                        params.background,
                        &instance_env,
                        &effective_tag,
                        hcom_config.relay_enabled,
                    );

                    let sandbox_mode = instance_env
                        .get("HCOM_CODEX_SANDBOX_MODE")
                        .cloned()
                        .unwrap_or_else(|| "workspace".to_string());

                    effective_args = codex_preprocessing::preprocess_codex_args(
                        &effective_args,
                        &bootstrap,
                        &sandbox_mode,
                        codex_hook_trust,
                    );

                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );

                    instance_env.insert("HCOM_CODEX_SANDBOX_MODE".to_string(), sandbox_mode);

                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "codex",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &effective_args,
                        &params,
                        inside_ai_tool,
                    )
                }

                LaunchTool::OpenCode | LaunchTool::Kilo | LaunchTool::Pi | LaunchTool::Omp => {
                    opencode_preprocessing::preprocess_opencode_env(
                        &mut instance_env,
                        base_tool,
                        &instance_name,
                        hcom_config.auto_approve,
                    );

                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );

                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: base_tool,
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }
                LaunchTool::Antigravity => {
                    // Antigravity ignores GEMINI_SYSTEM_MD; bootstrap is delivered via the
                    // SessionStart hook's inject_bootstrap_once (name_announced stays 0 here).
                    instance_env.insert("ANTIGRAVITY_AGENT".to_string(), "1".to_string());

                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );
                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "antigravity",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }
                LaunchTool::Cursor => {
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );
                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "cursor",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }

                LaunchTool::Kimi => {
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );
                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "kimi",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }
                LaunchTool::Copilot => {
                    instances::update_instance_position(
                        db,
                        &instance_name,
                        &serde_json::Map::from_iter([(
                            "launch_args".to_string(),
                            json!(&stored_launch_args),
                        )]),
                    );
                    launch_pty_or_background(
                        &mut BackgroundLaunchCtx {
                            db,
                            tool: "copilot",
                            instance_name: &instance_name,
                            process_id: &process_id,
                            terminal_mode,
                            tag: params.tag.as_deref().unwrap_or(""),
                            working_dir,
                            log_files: &mut log_files,
                            handles: &mut handles,
                        },
                        &mut instance_env,
                        &params.args,
                        &params,
                        inside_ai_tool,
                    )
                }
            }
        })();

        match launch_result {
            Ok(true) => launched += 1,
            Ok(false) => {
                cleanup_instance(db, &instance_name, &process_id);
            }
            Err(e) => {
                cleanup_instance(db, &instance_name, &process_id);
                errors.push(json!({"tool": base_tool, "error": e.to_string()}));
            }
        }
    }

    let failed = params.count - launched;
    if launched == 0 {
        if !errors.is_empty() {
            let details: Vec<String> = errors
                .iter()
                .filter_map(|e| {
                    e.get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            bail!(
                "No instances launched (0/{}): {}",
                params.count,
                details.join("; ")
            );
        }
        bail!("No instances launched (0/{})", params.count);
    }

    // Log batch launch event
    db.log_event(
        "life",
        &launcher_name,
        &json!({
            "action": "batch_launched",
            "by": &launcher_name,
            "batch_id": batch_id,
            // User-facing tool identity (`claude`, not the `claude-pty` launch
            // surface) — consistent with per-instance events and LaunchResult.
            "tool": base_tool,
            "count_requested": params.count,
            "launched": launched,
            "failed": failed,
            "background": params.background,
            "tag": effective_tag,
            "instances": handles
                .iter()
                .filter_map(|h| h.get("instance_name").and_then(|v| v.as_str()))
                .collect::<Vec<_>>(),
        }),
    )
    .ok();

    // Push launch event to relay (best-effort)
    crate::relay::spawn_background_push();

    Ok(LaunchResult {
        // User-facing identity. The PTY-vs-print backend distinction lives in
        // `LaunchBackend`, not the tool string, so consumers see `claude`.
        tool: base_tool.to_string(),
        batch_id,
        launched,
        failed,
        background: params.background,
        log_files,
        handles,
        errors,
    })
}

/// Validate tool args (pure parsing, no mutation).
pub(crate) fn validate_tool_args(tool: &LaunchTool, args: &[String]) -> Vec<String> {
    match tool {
        LaunchTool::Claude | LaunchTool::ClaudePty | LaunchTool::Codex => Vec::new(),
        LaunchTool::Gemini => {
            validate_rejected_args("Gemini", "hcom gemini", args, GEMINI_REJECTED_ARGS)
        }
        LaunchTool::Cursor => crate::tools::cursor_preprocessing::validate_cursor_args(args),
        LaunchTool::Kimi => validate_rejected_args("Kimi", "hcom kimi", args, KIMI_REJECTED_ARGS),
        LaunchTool::OpenCode => {
            validate_rejected_args("OpenCode", "hcom opencode", args, OPENCODE_REJECTED_ARGS)
        }
        LaunchTool::Kilo => validate_rejected_args("Kilo", "hcom kilo", args, KILO_REJECTED_ARGS),
        LaunchTool::Pi => validate_rejected_args("Pi", "hcom pi", args, PI_REJECTED_ARGS),
        LaunchTool::Omp => validate_rejected_args("Oh My Pi", "hcom omp", args, OMP_REJECTED_ARGS),
        LaunchTool::Antigravity => validate_rejected_args(
            "Antigravity",
            "hcom antigravity",
            args,
            ANTIGRAVITY_REJECTED_ARGS,
        ),
        LaunchTool::Copilot => crate::tools::copilot_preprocessing::validate_copilot_args(args),
    }
}

/// Clean up instance and process binding on failure.
fn cleanup_instance(db: &HcomDb, name: &str, process_id: &str) {
    db.delete_instance(name).ok();
    db.delete_process_binding(process_id).ok();
}

#[cfg(test)]
#[path = "launcher_tests.rs"]
mod tests;
