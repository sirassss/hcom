//! Codex launch preprocessing — sandbox flags, DB access, bootstrap injection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, bail};

use crate::paths;

const BYPASS_HOOK_TRUST_FLAG: &str = "--dangerously-bypass-hook-trust";
const BYPASS_HOOK_TRUST_MIN_VERSION: (u64, u64, u64) = (0, 131, 0);

/// Sandbox modes aligned with Codex TUI presets.
///
/// - `workspace`: Default — --sandbox workspace-write (interactive: on-request approvals)
/// - `untrusted`: Workspace writes, approval before untrusted commands
/// - `danger-full-access`: Full Access — --dangerously-bypass-approvals-and-sandbox
/// - `none`: Raw codex, user's own settings (hcom may not work)
///
/// Codex 0.128.0 removed `--full-auto` from the TUI (it was sugar for
/// workspace-write + on-failure approvals). The current shape — --sandbox
/// workspace-write with default on-request approvals — matches the prior
/// behavior closely enough for the TUI flow.
pub fn get_sandbox_flags(mode: &str) -> Vec<String> {
    // Seatbelt blocks Unix sockets by default, breaking tmux/kitty terminal launches.
    // network_access=true adds (allow system-socket) to the seatbelt profile.
    let net = vec![
        "-c".to_string(),
        "sandbox_workspace_write.network_access=true".to_string(),
    ];

    match mode {
        "workspace" => {
            let mut flags = vec!["--sandbox".to_string(), "workspace-write".to_string()];
            flags.extend(net);
            flags
        }
        "untrusted" => {
            // Read-only-equivalent UX for hcom: codex's actual read-only sandbox
            // can't be used (hcom needs DB writes), so we keep workspace-write FS
            // and gate every non-safe command on user approval via -a untrusted.
            let mut flags = vec![
                "--sandbox".to_string(),
                "workspace-write".to_string(),
                "-a".to_string(),
                "untrusted".to_string(),
            ];
            flags.extend(net);
            flags
        }
        "danger-full-access" => {
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
        }
        "none" => vec![],
        // Default to workspace
        _ => {
            let mut flags = vec!["--sandbox".to_string(), "workspace-write".to_string()];
            flags.extend(net);
            flags
        }
    }
}

fn has_explicit_sandbox_or_approval(tokens: &[String]) -> bool {
    const POLICY_FLAGS: &[&str] = &[
        "--sandbox",
        "-s",
        "--ask-for-approval",
        "-a",
        "--dangerously-bypass-approvals-and-sandbox",
        "--full-auto",
        "--yolo",
    ];

    tokens.iter().any(|token| {
        POLICY_FLAGS.iter().any(|flag| {
            token == flag
                || token
                    .strip_prefix(flag)
                    .is_some_and(|suffix| suffix.starts_with('='))
        })
    })
}

/// Ensure ~/.hcom is a writable sandbox root so hcom can write to its DB.
///
/// Injected as `-c sandbox_workspace_write.writable_roots=[...]` rather than
/// `--add-dir`: codex's TUI gates the flag on its effective-permissions
/// preset, and a trusted project (hcom's auto-trust injection) or a missing
/// explicit `-a` resolves to a preset that rejects extra writable roots
/// outright ("Ignoring --add-dir ... Switch to workspace-write"). The config
/// override bypasses that gate; like --add-dir, it is inert outside
/// workspace-write mode.
///
/// If no sandbox flags are present (mode="none"), skip the injection since
/// user is using codex's own folder settings.
pub fn ensure_hcom_writable(tokens: &[String]) -> Vec<String> {
    let has_sandbox = tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "--sandbox"
                | "-s"
                | "--dangerously-bypass-approvals-and-sandbox"
                | "--full-auto"
                | "--yolo"
        ) || token.starts_with("--sandbox=")
            || token.starts_with("-s=")
    });
    if !has_sandbox {
        return tokens.to_vec();
    }

    let hcom_dir = paths::hcom_dir().to_string_lossy().to_string();

    for (i, token) in tokens.iter().enumerate() {
        // A user-supplied roots override owns the whole list — don't clobber.
        if token.contains("sandbox_workspace_write.writable_roots") {
            return tokens.to_vec();
        }
        // Respect an explicit --add-dir for the hcom dir.
        if token == "--add-dir" && i + 1 < tokens.len() && tokens[i + 1] == hcom_dir {
            return tokens.to_vec();
        }
        if token
            .strip_prefix("--add-dir=")
            .is_some_and(|value| value == hcom_dir)
        {
            return tokens.to_vec();
        }
    }

    // TOML basic-string escaping (backslashes first, then quotes) — every
    // Windows path carries backslashes.
    let toml_escaped = crate::runtime_env::toml_escape_path(&hcom_dir);
    let mut result = tokens.to_vec();
    result.extend([
        "-c".to_string(),
        format!("sandbox_workspace_write.writable_roots=[\"{toml_escaped}\"]"),
    ]);
    result
}

fn parse_codex_cli_version(output: &str) -> Option<(u64, u64, u64)> {
    output
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(|token| {
            let mut parts = token.split('.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch = parts.next()?.parse().ok()?;
            Some((major, minor, patch))
        })
        .next_back()
}

fn codex_supports_bypass_hook_trust() -> bool {
    if let Ok(version) = std::env::var("HCOM_TEST_CODEX_CLI_VERSION") {
        return parse_codex_cli_version(&version)
            .is_some_and(|version| version >= BYPASS_HOOK_TRUST_MIN_VERSION);
    }

    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let output = match crate::terminal::executable_command("codex")
            .arg("--version")
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                crate::log::log_warn(
                    "codex",
                    "codex.version_failed",
                    &format!(
                        "could not run codex --version; skipping {BYPASS_HOOK_TRUST_FLAG}: {e}"
                    ),
                );
                return false;
            }
        };
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        parse_codex_cli_version(&text)
            .is_some_and(|version| version >= BYPASS_HOOK_TRUST_MIN_VERSION)
    })
}

/// Resolve `CODEX_HOME` the same way Codex itself does: env var if set and
/// non-empty, otherwise `~/.codex`.
fn resolve_codex_home() -> Option<(PathBuf, bool)> {
    if let Ok(val) = std::env::var("CODEX_HOME")
        && !val.is_empty()
    {
        return Some((PathBuf::from(val), true));
    }
    dirs::home_dir().map(|h| (h.join(".codex"), false))
}

/// Resolve the Codex state directory from the effective child launch
/// environment, including values supplied through `~/.hcom/env` or `--env`.
pub(crate) fn resolve_codex_home_from_env(
    env: &HashMap<String, String>,
    launch_dir: &Path,
) -> Option<(PathBuf, bool)> {
    // `dirs::home_dir()` reads HOME on Unix but uses the platform profile API
    // on Windows. Reproduce that distinction from the child's effective env.
    #[cfg(windows)]
    let default_home = dirs::home_dir();
    #[cfg(not(windows))]
    let default_home = env
        .get("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir);
    resolve_codex_home_from_env_with(
        env,
        launch_dir,
        default_home.or_else(|| Some(crate::runtime_env::tool_config_root())),
        cfg!(windows),
    )
}

fn resolve_codex_home_from_env_with(
    env: &HashMap<String, String>,
    launch_dir: &Path,
    default_home: Option<PathBuf>,
    case_insensitive: bool,
) -> Option<(PathBuf, bool)> {
    let configured = if case_insensitive {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("CODEX_HOME"))
            .map(|(_, value)| value.as_str())
    } else {
        env.get("CODEX_HOME").map(String::as_str)
    };
    if let Some(value) = configured.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        return Some((
            if path.is_absolute() {
                path
            } else {
                launch_dir.join(path)
            },
            true,
        ));
    }
    default_home.map(|home| (home.join(".codex"), false))
}

/// Probe whether `CODEX_HOME` is writable before launching codex.
///
/// When hcom is invoked from inside a sandboxed parent codex (e.g.
/// `--sandbox workspace-write`), seatbelt/landlock is inherited by the entire
/// process chain. The child codex then fails to init its state DB
/// (SQLITE_READONLY) and hangs on an interactive "Repair Codex local data
/// now? [y/N]:" prompt with no human to answer.
///
/// Catching this synchronously and exiting non-zero with a permission-denied
/// message lets the parent codex's existing sandbox-escalation flow ("approve
/// to run unsandboxed?") trigger naturally on the failed shell command,
/// instead of leaving a brick agent behind.
pub fn ensure_codex_home_writable() -> Result<()> {
    let Some((codex_home, explicit_env)) = resolve_codex_home() else {
        return Ok(());
    };
    ensure_codex_home_writable_at(&codex_home, explicit_env)
}

pub(crate) fn ensure_codex_home_writable_at(codex_home: &Path, explicit_env: bool) -> Result<()> {
    let probe_dir = if codex_home.exists() {
        codex_home
    } else if explicit_env {
        return Ok(());
    } else {
        let Some(parent) = codex_home.ancestors().find(|p| p.exists()) else {
            return Ok(());
        };
        parent
    };
    let probe = probe_dir.join(".hcom_writable_probe");
    match std::fs::write(&probe, b"") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => {
            use std::io::ErrorKind;
            let denied = matches!(
                e.kind(),
                ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
            );
            if !denied {
                return Ok(());
            }
            bail!(
                "Operation not permitted: cannot write to CODEX_HOME ({}): {}\n\
                 The current process is running inside a sandbox that denies writes \
                 to the codex state directory. If this hcom command was invoked by \
                 a sandboxed agent (e.g. codex --sandbox workspace-write), approve \
                 it to run unsandboxed and retry.",
                codex_home.display(),
                e
            );
        }
    }
}

/// What hcom decided to do about Codex's hook-trust gate for one launch.
///
/// Codex 0.131.0+ refuses to run unmanaged hooks until they are trusted. hcom
/// normally writes exact trust state for its own hooks; when that fails, the only
/// remaining lever is `--dangerously-bypass-hook-trust`, which is
/// invocation-wide for *every* non-managed hook source and also suppresses
/// Codex's own "Hooks need review" prompt. So the flag is added only when hcom
/// can show that nothing but its own hooks would be unlocked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexHookTrustOutcome {
    /// Nothing to do: Codex predates the trust gate, the user passed the bypass
    /// flag themselves, or hcom's own trust state is exact.
    NoActionNeeded,
    /// Bypass granted after Codex's own hooks/list confirmed that every enabled,
    /// untrusted hook is one of hcom's.
    BypassVerifiedByCodex,
    /// Bypass granted from hcom's local scan alone, because hooks/list was
    /// unavailable.
    BypassFromLocalScan,
    /// Bypass withheld: a hook hcom does not own would have been unlocked, or
    /// hcom could not prove otherwise.
    BypassWithheld,
}

impl CodexHookTrustOutcome {
    fn adds_bypass_flag(self) -> bool {
        matches!(
            self,
            Self::BypassVerifiedByCodex | Self::BypassFromLocalScan
        )
    }

    /// Whether hcom must skip its own workspace-trust injection for this launch.
    ///
    /// Only for a bypass granted from the local scan. That scan reads hook
    /// declarations off disk, and a project layer only contributes hooks when its
    /// `.codex` folder is trusted (codex-rs/config/src/loader/mod.rs:907-923).
    /// Injecting `-c projects={…trust_level="trusted"}` would hand that trust out
    /// while hcom is already admitting it cannot see the full picture, so the two
    /// must never be combined. A user-supplied trust override is untouched — this
    /// only suppresses hcom's own injection.
    pub fn suppresses_workspace_trust(self) -> bool {
        matches!(self, Self::BypassFromLocalScan)
    }
}

/// Decide once, before anything else is injected, what to do about Codex's
/// hook-trust gate for a codex launched in `launch_dir`.
///
/// Split from `preprocess_codex_args` because the outcome also governs
/// workspace-trust injection, which happens earlier in the launch sequence.
pub fn resolve_codex_hook_trust(codex_args: &[String], launch_dir: &Path) -> CodexHookTrustOutcome {
    let Some((codex_home, _)) = resolve_codex_home() else {
        return CodexHookTrustOutcome::NoActionNeeded;
    };
    resolve_codex_hook_trust_at(codex_args, launch_dir, &codex_home)
}

pub(crate) fn resolve_codex_hook_trust_at(
    codex_args: &[String],
    launch_dir: &Path,
    codex_home: &Path,
) -> CodexHookTrustOutcome {
    if !codex_supports_bypass_hook_trust() {
        return CodexHookTrustOutcome::NoActionNeeded;
    }
    // The user's own escape hatch: passing the flag (directly or via
    // `[launch.codex] args`) opts back into the old unconditional behavior.
    if codex_args.iter().any(|arg| arg == BYPASS_HOOK_TRUST_FLAG) {
        return CodexHookTrustOutcome::NoActionNeeded;
    }

    match crate::hooks::codex::resolve_codex_hook_trust_state_at(launch_dir, codex_home) {
        crate::hooks::codex::CodexHookTrustState::Trusted => CodexHookTrustOutcome::NoActionNeeded,
        crate::hooks::codex::CodexHookTrustState::BypassSafeFromHooksList => {
            warn_bypass_granted("Codex's own hook list");
            CodexHookTrustOutcome::BypassVerifiedByCodex
        }
        crate::hooks::codex::CodexHookTrustState::BypassSafeFromLocalScan => {
            warn_bypass_granted("a local scan of your Codex hook files");
            CodexHookTrustOutcome::BypassFromLocalScan
        }
        crate::hooks::codex::CodexHookTrustState::BypassUnsafe { reason } => {
            warn_bypass_withheld(&reason);
            CodexHookTrustOutcome::BypassWithheld
        }
    }
}

fn warn_bypass_granted(evidence: &str) {
    crate::log::log_warn(
        "codex",
        "codex.hook_trust_bypass_granted",
        &format!(
            "hcom hook trust state is incomplete; adding {BYPASS_HOOK_TRUST_FLAG} after verifying via {evidence} that no non-hcom hooks are in scope"
        ),
    );
    eprintln!(
        "[hcom] Warning: hcom could not record exact Codex hook trust, so this codex \
         runs with {BYPASS_HOOK_TRUST_FLAG}."
    );
    eprintln!(
        "[hcom] Enabled hooks run without review for this invocation. hcom verified via \
         {evidence} that no non-hcom hooks are in scope."
    );
}

fn warn_bypass_withheld(reason: &str) {
    crate::log::log_warn(
        "codex",
        "codex.hook_trust_bypass_withheld",
        &format!("withholding {BYPASS_HOOK_TRUST_FLAG}: {reason}"),
    );
    eprintln!(
        "[hcom] Warning: Codex hook trust is incomplete and hcom is not bypassing it: {reason}"
    );
    eprintln!("[hcom] hcom's hooks may not run, so messaging and status may be silent.");
    eprintln!(
        "[hcom] Fix it with: hcom hooks add codex — or in interactive codex run /hooks and \
         choose \"Trust all\"."
    );
    eprintln!(
        "[hcom] To restore the old unconditional behavior, add \
         \"{BYPASS_HOOK_TRUST_FLAG}\" to [launch.codex] args yourself."
    );
}

fn apply_hook_trust_outcome(
    codex_args: &[String],
    hook_trust: CodexHookTrustOutcome,
) -> Vec<String> {
    let mut result = codex_args.to_vec();
    if hook_trust.adds_bypass_flag() && !result.iter().any(|arg| arg == BYPASS_HOOK_TRUST_FLAG) {
        result.push(BYPASS_HOOK_TRUST_FLAG.to_string());
    }
    result
}

/// Add hcom bootstrap to codex developer_instructions.
///
/// Builds full bootstrap and adds via `-c developer_instructions=...` flag.
/// If user also provided developer_instructions, bootstrap comes first,
/// then separator, then user content.
///
pub fn add_codex_developer_instructions(
    codex_args: &[String],
    bootstrap_text: &str,
) -> Vec<String> {
    let mut existing_dev_instructions: Option<String> = None;
    let mut remaining = Vec::with_capacity(codex_args.len() + 2);
    let mut i = 0;
    while i < codex_args.len() {
        let token = &codex_args[i];
        if let Some(value) = token
            .strip_prefix("-c=developer_instructions=")
            .or_else(|| token.strip_prefix("--config=developer_instructions="))
        {
            existing_dev_instructions = Some(value.to_string());
            i += 1;
            continue;
        }
        if (token == "-c" || token == "--config")
            && i + 1 < codex_args.len()
            && let Some(value) = codex_args[i + 1].strip_prefix("developer_instructions=")
        {
            existing_dev_instructions = Some(value.to_string());
            i += 2;
            continue;
        }
        remaining.push(token.clone());
        i += 1;
    }

    let combined = if let Some(existing) = existing_dev_instructions {
        format!("{}\n---\n{}", bootstrap_text, existing)
    } else {
        bootstrap_text.to_string()
    };

    // `-c` values are TOML expressions. A raw multiline string happened to be
    // accepted by older Codex builds but is ignored by current builds,
    // silently dropping the hcom identity bootstrap. Serialize a real TOML
    // string so quotes, backslashes, and newlines survive on every platform.
    let encoded = toml::Value::String(combined).to_string();
    remaining.extend([
        "-c".to_string(),
        format!("developer_instructions={encoded}"),
    ]);
    remaining
}

/// Remove any Codex `developer_instructions=...` config entries.
///
/// Resume/fork should not carry the previous instance's embedded hcom session
/// block because it hard-codes the original instance name. A fresh bootstrap is
/// injected later for the new instance.
pub fn strip_codex_developer_instructions(codex_args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < codex_args.len() {
        let token = &codex_args[i];

        if token.starts_with("-c=developer_instructions=")
            || token.starts_with("--config=developer_instructions=")
        {
            i += 1;
            continue;
        }

        if (token == "-c" || token == "--config") && i + 1 < codex_args.len() {
            let next = &codex_args[i + 1];
            if next.starts_with("developer_instructions=") {
                i += 2;
                continue;
            }
        }

        result.push(token.clone());
        i += 1;
    }

    result
}

/// Preprocess Codex CLI arguments for hcom integration.
///
/// Applies:
/// 1. Strip stale developer_instructions (resume/fork only — they carry old identity)
/// 2. Sandbox flags based on mode
/// 3. Runtime hook-trust bypass, per the already-resolved `hook_trust` decision
/// 4. writable_roots config override for ~/.hcom DB writes
/// 5. Bootstrap injection via developer_instructions
pub fn preprocess_codex_args(
    codex_args: &[String],
    bootstrap_text: &str,
    sandbox_mode: &str,
    hook_trust: CodexHookTrustOutcome,
) -> Vec<String> {
    // 1. Strip stale developer_instructions for resume/fork only.
    //    Fresh launches may have user system_prompt in developer_instructions
    //    that add_codex_developer_instructions will merge with bootstrap.
    let codex_args = if codex_args
        .iter()
        .any(|arg| matches!(arg.as_str(), "resume" | "fork"))
    {
        strip_codex_developer_instructions(codex_args)
    } else {
        codex_args.to_vec()
    };

    let mut args = codex_args;

    // 2. Inject the configured policy only as a default. An explicit user
    // sandbox, approval, or bypass selector owns the complete Codex policy;
    // appending hcom's profile would make clap's last-value-wins behavior
    // silently override it.
    if !has_explicit_sandbox_or_approval(&args) {
        args.extend(get_sandbox_flags(sandbox_mode));
    }

    // 3. Codex 0.131.0+ requires unmanaged hooks to be trusted. The decision was
    // made by `resolve_codex_hook_trust` before workspace trust was injected,
    // because the two interact; here it is only applied.
    args = apply_hook_trust_outcome(&args, hook_trust);

    // Warn if mode is "none"
    if sandbox_mode == "none" {
        eprintln!(
            "[hcom] Warning: Sandbox mode is 'none' - ~/.hcom writable-root injection disabled."
        );
        eprintln!("[hcom] hcom commands may fail unless HCOM_DIR is within workspace.");
    }

    // 4. Ensure ~/.hcom is a writable sandbox root (skips if mode="none")
    args = ensure_hcom_writable(&args);

    // 5. Add bootstrap to developer_instructions
    args = add_codex_developer_instructions(&args, bootstrap_text);

    args
}

#[cfg(test)]
#[path = "codex_preprocessing_tests.rs"]
mod tests;
