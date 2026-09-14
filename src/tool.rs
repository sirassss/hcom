//! Tool enum for type-safe tool identification across hcom.
//!
//! Per-tool data (hook names, ready pattern, delivery gates, help, status
//! mappings, etc.) lives in [`crate::integration_spec`]. This module just
//! defines the enum and a thin set of forwarders.

use std::str::FromStr;

use crate::integration_spec;

/// Supported AI coding tools
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Claude,
    Gemini,
    Codex,
    OpenCode,
    Kilo,
    Antigravity,
    Cursor,
    Kimi,
    Copilot,
    Pi,
    Omp,
    Adhoc,
}

/// Message for a failed Codex removal. It reports only what was observed: the
/// uninstall failed, and whether the legacy hook entries came out. It must not
/// assert that the plugin is still installed — the CLI may have been missing, so
/// its presence was never established — nor that legacy entries were removed
/// when the remover returned false.
fn codex_remove_failure_message(error: &str, legacy_removed: bool) -> String {
    let legacy = if legacy_removed {
        "the legacy hook entries were removed"
    } else {
        "the legacy hook entries could not be removed either"
    };
    format!("codex plugin uninstall failed: {error}. Separately, {legacy}.")
}

impl Tool {
    /// Ready-pattern bytes for PTY readiness detection.
    pub fn ready_pattern(&self) -> &'static [u8] {
        self.spec().ready_pattern
    }

    /// Lowercase tool name used in DB, CLI output, and external interfaces.
    pub fn as_str(&self) -> &'static str {
        self.spec().name
    }

    /// Hook command names listed for this tool. Some tools borrow another
    /// tool's names; use `owns_hook` for routing ownership.
    pub fn hooks(&self) -> &'static [&'static str] {
        self.spec().hooks.names
    }

    /// True if this tool owns `name` for routing. Borrowed hook names do not
    /// count as ownership.
    pub fn owns_hook(&self, name: &str) -> bool {
        let hooks = &self.spec().hooks;
        hooks.shared_hooks_with.is_none() && hooks.names.contains(&name)
    }

    /// Resolve the tool that owns a hook command name.
    ///
    /// Shared hook specs route to their declared owner. Antigravity, for
    /// example, lists Gemini hook names but routes them to Gemini.
    pub fn from_hook_name(name: &str) -> Option<Self> {
        integration_spec::ALL
            .iter()
            .find(|spec| spec.hooks.names.contains(&name))
            .map(|spec| spec.hooks.shared_hooks_with.unwrap_or(spec.tool))
    }

    /// True if any spec with routing ownership claims this hook name.
    pub fn is_hook_name(name: &str) -> bool {
        Self::from_hook_name(name).is_some()
    }

    // ── Hook-ops adapter ────────────────────────────────────────────────
    //
    // The four helpers below are the single source of truth for routing
    // verify/setup/remove/settings-path to the right per-tool hook module.
    // `commands/hooks.rs` iterates released hook-bearing tools through these
    // helpers so new tools only need a hooks module + a spec + a match arm,
    // not a fresh parallel block per dispatch site.
    //
    // Setup/installation error detail (codex hook-trust fallback, claude
    // diagnostic context, etc.) intentionally stays in `launcher::ensure_hooks_installed`
    // — those error shapes vary per tool and aren't suitable for a uniform trait.

    /// Verify hooks are installed for this tool. `include_permissions` controls
    /// whether the auto-approve permission block is also checked.
    /// True when this tool's hooks ship inside the hcom plugin rather than
    /// being written into a config file the tool shares with other harnesses.
    ///
    /// Installing these means shelling out to that tool's own CLI, which clones
    /// a marketplace over the network — so callers that are not an explicit
    /// `hcom hooks add` must report instead of installing.
    pub fn hooks_ship_as_plugin(&self) -> bool {
        matches!(self, Tool::Claude | Tool::Cursor | Tool::Antigravity)
    }

    pub fn verify_hooks_installed(&self, include_permissions: bool) -> bool {
        match self {
            Tool::Claude => crate::hooks::plugin::verify_claude_plugin_installed(),
            Tool::Gemini => {
                crate::hooks::gemini::verify_gemini_hooks_installed(include_permissions)
            }
            Tool::Codex => {
                crate::hooks::codex::verify_codex_hooks_installed(include_permissions)
                    && crate::hooks::codex::codex_current_feature_enabled()
            }
            Tool::OpenCode => crate::hooks::opencode::verify_opencode_plugin_installed(),
            Tool::Kilo => crate::hooks::opencode::verify_kilo_plugin_installed(),
            Tool::Antigravity => crate::hooks::plugin::verify_agy_plugin_installed(),
            Tool::Cursor => crate::hooks::plugin::verify_cursor_plugin_installed(),
            Tool::Kimi => crate::hooks::kimi::verify_kimi_hooks_installed(include_permissions),
            Tool::Copilot => {
                crate::hooks::copilot::verify_copilot_hooks_installed(include_permissions)
            }
            Tool::Pi => crate::hooks::pi::verify_pi_plugin_installed(),
            Tool::Omp => crate::hooks::omp::verify_omp_plugin_installed(),
            Tool::Adhoc => false,
        }
    }

    /// Try to install hooks for this tool. Returns `Err(message)` on failure.
    /// `Tool::Adhoc` always errors — adhoc has no hook surface.
    pub fn try_setup_hooks(&self, include_permissions: bool) -> Result<(), String> {
        match self {
            Tool::Claude => crate::hooks::plugin::install_claude_plugin(),
            Tool::Gemini => crate::hooks::gemini::try_setup_gemini_hooks(include_permissions)
                .map_err(|e| e.to_string()),
            Tool::Codex => crate::hooks::codex::try_setup_codex_hooks(include_permissions)
                .map_err(|e| e.to_string()),
            Tool::OpenCode => match crate::hooks::opencode::install_opencode_plugin() {
                Ok(true) => Ok(()),
                Ok(false) => Err(String::new()),
                Err(e) => Err(e.to_string()),
            },
            Tool::Kilo => match crate::hooks::opencode::install_kilo_plugin() {
                Ok(true) => Ok(()),
                Ok(false) => Err(String::new()),
                Err(e) => Err(e.to_string()),
            },
            Tool::Antigravity => crate::hooks::plugin::install_agy_plugin(),
            Tool::Cursor => crate::hooks::plugin::install_cursor_plugin(),
            Tool::Kimi => crate::hooks::kimi::try_setup_kimi_hooks(include_permissions)
                .map_err(|e| e.to_string()),
            Tool::Copilot => crate::hooks::copilot::try_setup_copilot_hooks(include_permissions)
                .map_err(|e| e.to_string()),
            Tool::Pi => match crate::hooks::pi::install_pi_plugin() {
                Ok(true) => Ok(()),
                Ok(false) => Err(String::new()),
                Err(e) => Err(e.to_string()),
            },
            Tool::Omp => match crate::hooks::omp::install_omp_plugin() {
                Ok(true) => Ok(()),
                Ok(false) => Err(String::new()),
                Err(e) => Err(e.to_string()),
            },
            Tool::Adhoc => Err("Adhoc has no hooks to install".to_string()),
        }
    }

    /// Remove hooks for this tool. Returns `Ok(true)` on success, `Ok(false)`
    /// if the tool reports a non-error failure, and `Err(message)` on
    /// recoverable errors that callers should display verbatim.
    /// Strip only this tool's legacy config entries, leaving any installed
    /// plugin alone.
    ///
    /// `remove_hooks` takes both down, which is right for "I want hcom out of
    /// this tool" but wrong for finishing a migration: there the plugin is what
    /// the user is keeping. Only meaningful for `hooks_ship_as_plugin` tools.
    pub fn remove_legacy_hooks_only(&self) -> bool {
        match self {
            Tool::Claude => crate::hooks::claude::remove_claude_hooks(),
            Tool::Cursor => crate::hooks::cursor::remove_cursor_hooks(),
            Tool::Antigravity => crate::hooks::antigravity::remove_antigravity_hooks(),
            // Removes only hcom's own entries from Codex's hooks.json, leaving
            // foreign hooks and the installed plugin alone.
            Tool::Codex => crate::hooks::codex::remove_codex_hooks(),
            _ => false,
        }
    }

    pub fn remove_hooks(&self) -> Result<bool, String> {
        match self {
            // These three also carry a plugin (`hooks_ship_as_plugin`). The
            // plugin uninstall is best-effort: a missing CLI or a plugin that
            // was never installed must not stop the legacy config from being
            // stripped, which is the part `hcom hooks remove` must never skip.
            Tool::Claude => {
                if let Err(e) = crate::hooks::plugin::uninstall_claude_plugin() {
                    eprintln!("note: could not remove Claude plugin: {e}");
                }
                Ok(crate::hooks::claude::remove_claude_hooks())
            }
            Tool::Gemini => Ok(crate::hooks::gemini::remove_gemini_hooks()),
            // Both halves, and only these two: the plugin uninstall is
            // best-effort so a missing CLI cannot stop the legacy cleanup, and
            // nothing here touches Claude's plugin or marketplace even though
            // both vendors install the same package.
            Tool::Codex => {
                // The legacy cleanup runs either way — it is the part
                // `hooks remove` must never skip — but a failed plugin
                // uninstall is then reported, not swallowed: exiting 0 with
                // "Removed" while the plugin still fires is the one outcome
                // worse than failing.
                let uninstall = crate::hooks::plugin::uninstall_codex_plugin();
                let removed = crate::hooks::codex::remove_codex_hooks();
                match uninstall {
                    Ok(()) => Ok(removed),
                    Err(e) => Err(codex_remove_failure_message(&e, removed)),
                }
            }
            Tool::OpenCode => crate::hooks::opencode::remove_opencode_plugin()
                .map(|_| true)
                .map_err(|e| e.to_string()),
            Tool::Kilo => crate::hooks::opencode::remove_kilo_plugin()
                .map(|_| true)
                .map_err(|e| e.to_string()),
            Tool::Antigravity => {
                if let Err(e) = crate::hooks::plugin::uninstall_agy_plugin() {
                    eprintln!("note: could not remove Antigravity plugin: {e}");
                }
                Ok(crate::hooks::antigravity::remove_antigravity_hooks())
            }
            Tool::Cursor => {
                if let Err(e) = crate::hooks::plugin::uninstall_cursor_plugin() {
                    eprintln!("note: could not remove Cursor plugin: {e}");
                }
                Ok(crate::hooks::cursor::remove_cursor_hooks())
            }
            Tool::Kimi => Ok(crate::hooks::kimi::remove_kimi_hooks()),
            Tool::Copilot => Ok(crate::hooks::copilot::remove_copilot_hooks()),
            Tool::Pi => crate::hooks::pi::remove_pi_plugin()
                .map(|_| true)
                .map_err(|e| e.to_string()),
            Tool::Omp => crate::hooks::omp::remove_omp_plugin()
                .map(|_| true)
                .map_err(|e| e.to_string()),
            Tool::Adhoc => Ok(false),
        }
    }

    /// Filesystem path the hook integration writes to (settings/config file or
    /// plugin location). Empty for `Tool::Adhoc`.
    pub fn hooks_settings_path(&self) -> String {
        let path_buf = match self {
            Tool::Claude => crate::hooks::claude::get_claude_settings_path(),
            Tool::Gemini => crate::hooks::gemini::get_gemini_settings_path(),
            Tool::Codex => crate::hooks::codex::get_codex_config_path(),
            Tool::OpenCode => crate::hooks::opencode::get_opencode_plugin_path(),
            Tool::Kilo => crate::hooks::opencode::get_kilo_plugin_path(),
            Tool::Antigravity => crate::hooks::antigravity::get_antigravity_hooks_path(),
            Tool::Cursor => crate::hooks::cursor::get_cursor_hooks_path(),
            Tool::Kimi => crate::hooks::kimi::get_kimi_settings_path(),
            Tool::Copilot => crate::hooks::copilot::get_copilot_hooks_path(),
            Tool::Pi => crate::hooks::pi::get_pi_plugin_path(),
            Tool::Omp => crate::hooks::omp::get_omp_plugin_path(),
            Tool::Adhoc => return String::new(),
        };
        path_buf.to_string_lossy().to_string()
    }
}

impl FromStr for Tool {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        // Primary name match.
        if let Some(spec) = integration_spec::ALL.iter().find(|s| s.name == lower) {
            return Ok(spec.tool);
        }
        // Alias match.
        if let Some(spec) = integration_spec::ALL
            .iter()
            .find(|s| s.aliases.iter().any(|a| *a == lower))
        {
            return Ok(spec.tool);
        }
        Err(format!("Unknown tool: {}", s))
    }
}

impl std::fmt::Display for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The failure message may only say what was observed. A missing `codex`
    /// binary means the plugin's presence was never established, so the message
    /// must not claim it is still installed — and it must not claim the legacy
    /// entries came out when they did not.
    #[test]
    fn codex_remove_failure_message_asserts_only_what_was_observed() {
        let missing_cli = super::codex_remove_failure_message("codex not runnable", false);
        assert!(!missing_cli.contains("still installed"), "{missing_cli}");
        assert!(
            missing_cli.contains("could not be removed either"),
            "{missing_cli}"
        );

        let legacy_gone = super::codex_remove_failure_message("exit 1", true);
        assert!(legacy_gone.contains("were removed"), "{legacy_gone}");
        assert!(!legacy_gone.contains("still installed"), "{legacy_gone}");
    }

    #[test]
    fn adhoc_has_no_hooks() {
        assert!(Tool::Adhoc.hooks().is_empty());
        assert_ne!(Tool::from_hook_name("poll"), Some(Tool::Adhoc));
    }

    #[test]
    fn hook_names_are_disjoint() {
        // Shared hooks are owned by their spec's `shared_hooks_with` declaration.
        let mut owners = HashMap::new();
        for spec in crate::integration_spec::ALL {
            if spec.hooks.shared_hooks_with.is_some() {
                continue;
            }
            let tool = spec.tool;
            for hook in tool.hooks() {
                assert_eq!(
                    owners.insert(*hook, tool),
                    None,
                    "{hook} has multiple owners"
                );
                assert_eq!(Tool::from_hook_name(hook), Some(tool));
            }
        }
    }

    #[test]
    fn antigravity_borrows_gemini_hooks_without_owning_them() {
        assert!(Tool::Gemini.owns_hook("gemini-beforeagent"));
        assert!(!Tool::Antigravity.owns_hook("gemini-beforeagent"));
        assert_eq!(
            Tool::from_hook_name("gemini-beforeagent"),
            Some(Tool::Gemini)
        );
    }

    #[test]
    fn antigravity_as_str() {
        assert_eq!(Tool::Antigravity.as_str(), "antigravity");
    }

    #[test]
    fn antigravity_from_str() {
        assert_eq!("antigravity".parse::<Tool>(), Ok(Tool::Antigravity));
    }

    #[test]
    fn antigravity_agy_alias() {
        assert_eq!("agy".parse::<Tool>(), Ok(Tool::Antigravity));
    }

    #[test]
    fn antigravity_ready_pattern() {
        // agy 1.1.27 renders no "? for shortcuts"; its status bar carries
        // "Ctx <pct>% (<used>/<total>)". Measured 2026-09-08: with the old
        // pattern every AGY launch reported blocked and no message was ever
        // injected. See docs/superpowers/specs/2026-09-08-agy-wake-design.md.
        assert_eq!(Tool::Antigravity.ready_pattern(), b"Ctx ");
    }

    #[test]
    fn copilot_from_alias() {
        assert_eq!("copilot".parse::<Tool>(), Ok(Tool::Copilot));
    }

    #[test]
    fn pi_from_str() {
        assert_eq!("pi".parse::<Tool>(), Ok(Tool::Pi));
        assert_eq!("pi-agent".parse::<Tool>(), Ok(Tool::Pi));
    }

    #[test]
    fn omp_from_str() {
        assert_eq!("omp".parse::<Tool>(), Ok(Tool::Omp));
        assert_eq!("omp-agent".parse::<Tool>(), Ok(Tool::Omp));
    }

    #[test]
    fn antigravity_shares_gemini_hooks() {
        assert_eq!(Tool::Antigravity.hooks(), Tool::Gemini.hooks());
    }

    #[test]
    fn kilo_shares_opencode_hooks() {
        assert_eq!(Tool::Kilo.hooks(), Tool::OpenCode.hooks());
        assert!(!Tool::Kilo.owns_hook("opencode-start"));
        assert_eq!(Tool::from_hook_name("opencode-start"), Some(Tool::OpenCode));
    }

    /// Same isolation `plugin::tests::plugin_test_env` uses: a fresh HOME so
    /// `Config` (cached, and what the plugin verifiers resolve paths through)
    /// agrees with the fixtures this test writes.
    fn plugin_tool_test_env() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        crate::hooks::test_helpers::EnvGuard,
    ) {
        let guard = crate::hooks::test_helpers::EnvGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("HCOM_DIR", home.join(".hcom"));
            std::env::remove_var("CURSOR_CONFIG_DIR");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            std::env::remove_var("GEMINI_CLI_HOME");
        }
        crate::paths::test_roots::register(&home);
        crate::config::Config::reset();
        crate::config::Config::init();
        (dir, home, guard)
    }

    #[test]
    #[serial_test::serial]
    fn plugin_tools_verify_through_the_plugin_path() {
        let (_dir, home, _guard) = plugin_tool_test_env();

        // Nothing installed anywhere.
        assert!(!Tool::Claude.verify_hooks_installed(false));
        assert!(!Tool::Cursor.verify_hooks_installed(false));
        assert!(!Tool::Antigravity.verify_hooks_installed(false));

        // A legacy settings.json full of hcom hooks must NOT count as installed
        // any more — that file is exactly what we are migrating away from.
        let settings = home.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        crate::hooks::claude::try_setup_claude_hooks(false).unwrap();
        assert!(
            !Tool::Claude.verify_hooks_installed(false),
            "legacy hooks must not satisfy the plugin verifier"
        );
    }
}
