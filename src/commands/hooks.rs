//! `hcom hooks` command — add/remove/status for tool hooks.
//!
//!
//! Manages hook installation across every released hook-bearing integration.

use crate::db::HcomDb;
use crate::shared::CommandContext;
use crate::tool::Tool;

/// Parsed arguments for `hcom hooks`.
#[derive(clap::Parser, Debug)]
#[command(name = "hooks", about = "Manage tool hooks")]
pub struct HooksArgs {
    /// Subcommand and arguments (status/add/remove [tool])
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// Released hook-bearing tools, derived from the integration registry.
///
/// Keeping this as a function (rather than a parallel constant) means adding a
/// released integration with hooks automatically exposes it to status/add/remove.
pub(crate) fn hook_tools() -> Vec<Tool> {
    crate::integration_spec::ALL
        .iter()
        .filter(|spec| spec.released && !spec.hooks.names.is_empty())
        .map(|spec| spec.tool)
        .collect()
}

fn hook_tool_names() -> Vec<&'static str> {
    hook_tools().into_iter().map(|tool| tool.as_str()).collect()
}

fn parse_hook_tool(value: &str) -> Option<Tool> {
    value
        .parse::<Tool>()
        .ok()
        .filter(|tool| tool.spec().released && !tool.hooks().is_empty())
}

fn valid_hook_options() -> String {
    let mut names = hook_tool_names();
    names.push("all");
    names.join(", ")
}

/// Refresh permission state for hook integrations that are already installed.
///
/// This is used after `auto_approve` changes. It intentionally skips tools
/// without installed hooks so changing one preference does not install new
/// integrations as a side effect.
///
/// Tools whose hooks ship as a plugin are skipped outright. Their
/// `try_setup_hooks` shells out to the tool's own CLI and clones a marketplace
/// over the network — far too much to happen behind `hcom config auto_approve`
/// — and it would achieve nothing anyway: the plugin installers take no
/// permission argument, because a plugin manifest cannot carry one. Permission
/// allowances for these three are the user's to manage in their tool.
/// Tools whose permissions `hcom config auto_approve` actually manages —
/// every released hook-bearing tool except the three whose hooks ship as a
/// plugin (a plugin manifest cannot carry permission rules). Derived from
/// `hook_tools()` rather than hardcoded, so the printed list cannot drift
/// from what `refresh_installed_hook_permissions` below actually touches.
pub(crate) fn auto_approve_managed_tools() -> Vec<Tool> {
    hook_tools()
        .into_iter()
        .filter(|tool| !tool.hooks_ship_as_plugin())
        .collect()
}

pub(crate) fn refresh_installed_hook_permissions(enabled: bool) -> Vec<(&'static str, String)> {
    let mut failures = Vec::new();
    for tool in hook_tools() {
        if tool.hooks_ship_as_plugin() {
            continue;
        }
        if !tool.verify_hooks_installed(false) {
            continue;
        }
        if let Err(error) = tool.try_setup_hooks(enabled) {
            failures.push((tool.as_str(), error));
        }
    }
    failures
}

/// Get hook installation status for each tool.
///
/// Routes status checks through the typed hook adapter for every registry tool.
fn get_tool_status() -> Vec<(Tool, bool, String)> {
    hook_tools()
        .into_iter()
        .map(|tool| {
            (
                tool,
                tool.verify_hooks_installed(false),
                tool.hooks_settings_path(),
            )
        })
        .collect()
}

/// Advice line for a plugin-based tool. Empty when the state is healthy.
///
/// Nothing self-repairs any more, so status is the only place a user learns
/// they must act: either the plugin was never installed, or a plugin and its
/// legacy hook entries are both present (a machine mid-migration, or a config
/// synced from another host) and will double-fire.
pub(crate) fn plugin_status_line(tool: &str, plugin: bool, legacy: bool) -> String {
    match (plugin, legacy) {
        // `hooks add` finishes the migration in this state: it finds the plugin
        // already installed and strips the leftover legacy entries. Pointing at
        // `hooks remove` instead would take the plugin down too and leave the
        // tool with nothing.
        (true, true) if tool != "cursor" => format!(
            "{tool}: plugin and legacy hooks both present — both are firing (double-fire risk). \
             Run: hcom hooks add {tool} to drop the legacy entries."
        ),
        // Cursor is the exception. Its verifier only proves a marketplace
        // checkout exists, not that the user enabled the plugin in the TUI, so
        // hcom must never strip Cursor's legacy hooks on its own — the user
        // says when, once they know the plugin is live.
        // Cursor's verifier proves a marketplace checkout exists — which
        // `marketplace add` itself creates. Nothing hcom can read says whether
        // the plugin is enabled, so none of Cursor's four states may be stated
        // as an observation of what is firing.
        // Order matters: this bare (true, true) is the cursor branch — the guarded arm above must stay first.
        (true, true) => format!(
            "{tool}: legacy hooks are firing, and the plugin may be too once /plugins shows \
             hcom enabled — a double-fire risk hcom cannot confirm. Once it is enabled, run: \
             hcom hooks remove {tool} --legacy-only \
             (plain `hooks remove` would uninstall the plugin too, leaving no hooks)"
        ),
        (true, false) if tool == "cursor" => format!(
            "{tool}: marketplace indexed; finish in Cursor with /plugins → install \"hcom\". \
             hcom cannot see whether the plugin is enabled — confirm with a spawned agent \
             showing `bindings: hooks, pty` in hcom list, read after its first turn."
        ),
        // Not "no hooks": measured 2026-09-08, a Cursor agent ran hcom's hooks
        // with no marketplace at all, because cursor-agent reads Claude's
        // plugin cache. Saying "not installed" here contradicted a live agent.
        (false, false) if tool == "cursor" => format!(
            "{tool}: no marketplace checkout. Cursor may still be running hcom's hooks out of \
             Claude's plugin cache — check a spawned agent's `bindings` after its first turn. \
             For a Cursor-owned install: hcom hooks add {tool}, then /plugins → install \"hcom\"."
        ),
        (false, true) if tool == "cursor" => format!(
            "{tool}: no marketplace checkout; the legacy hook entries are what is firing. \
             Run: hcom hooks add {tool}, then /plugins → install \"hcom\", and only then \
             hcom hooks remove {tool} --legacy-only"
        ),
        (false, _) => format!("{tool}: hooks not installed. Run: hcom hooks add {tool}"),
        (true, false) => String::new(),
    }
}

/// Whether tool's legacy (pre-plugin) hook entries are still present.
/// Only meaningful for tools that `hooks_ship_as_plugin()`.
pub(crate) fn legacy_hooks_present(tool: Tool) -> bool {
    match tool {
        Tool::Claude => crate::hooks::claude::verify_claude_hooks_installed(None, false),
        Tool::Cursor => crate::hooks::cursor::verify_cursor_hooks_installed(false),
        Tool::Antigravity => crate::hooks::antigravity::verify_antigravity_hooks_installed(false),
        _ => false,
    }
}

/// Show hook installation status for all tools.
fn cmd_hooks_status() -> i32 {
    let status = get_tool_status();
    for (tool, installed, path) in &status {
        let tool = *tool;
        if tool.hooks_ship_as_plugin() {
            // `hooks_settings_path()` returns the legacy file for these three
            // tools, which holds nothing once the plugin is in use — printing
            // it next to "installed" would point at an empty file.
            // Cursor's signal is weaker than the others' in both directions: a
            // marketplace checkout is not a live plugin, and no checkout is not
            // "no hooks". Neither headline may be stated flatly for it.
            let state = match (tool == Tool::Cursor, *installed) {
                (true, true) => "marketplace ready",
                (true, false) => "no marketplace",
                (false, true) => "installed   ",
                (false, false) => "not installed",
            };
            if *installed || tool == Tool::Cursor {
                println!("{}:  {state} (plugin)", tool.spec().label);
            } else {
                println!("{}:  {state}", tool.spec().label);
            }
            let advice = plugin_status_line(tool.as_str(), *installed, legacy_hooks_present(tool));
            if !advice.is_empty() {
                println!("  {advice}");
            }
            if tool == Tool::Antigravity && *installed {
                use crate::hooks::plugin::AgyHooks;
                match crate::hooks::plugin::agy_hook_state() {
                    // Our own manifest. The import entry says `claude-code`
                    // because our manifest dir is `.claude-plugin/`; that is a
                    // format label, and warning on it printed advice that could
                    // never clear itself.
                    AgyHooks::Hcom => {}
                    AgyHooks::Foreign(source) => println!(
                        "  antigravity: the installed manifest carries SessionStart and none \
                         of hcom's handlers, so Antigravity is not running hcom's hooks. \
                         agy records the import as `{source}` — that names the manifest \
                         format, so treat it as a hint, not as proof of what is running. \
                         Run: hcom hooks remove antigravity && hcom hooks add antigravity"
                    ),
                    // Not attributed to anyone: a half-written or hand-edited
                    // manifest is not evidence that another harness did it.
                    AgyHooks::Malformed => println!(
                        "  antigravity: the installed manifest carries none of hcom's working \
                         hooks — hcom's events are missing, empty, or invoke something else, \
                         so no message will be delivered. \
                         Run: hcom hooks remove antigravity && hcom hooks add antigravity"
                    ),
                    AgyHooks::Unverifiable => println!(
                        "  antigravity: {} could not be read or parsed — hook state unverifiable. \
                         Run: hcom hooks remove antigravity && hcom hooks add antigravity",
                        crate::hooks::plugin::agy_plugin_dir()
                            .join(crate::hooks::plugin::AGY_HOOKS_RELATIVE)
                            .display()
                    ),
                }
            }
        } else if *installed {
            println!("{}:  installed    ({path})", tool.spec().label);
        } else {
            println!("{}:  not installed", tool.spec().label);
        }
    }
    0
}

/// Add hooks for specified tool(s).
fn cmd_hooks_add(argv: &[String]) -> i32 {
    // Get auto_approve from config
    let include_permissions = crate::config::load_config_snapshot().core.auto_approve;

    // Determine which tools to install.
    let tools: Vec<Tool> = if argv.is_empty() {
        // Auto-detect current tool; outside a supported tool, operate on all.
        parse_hook_tool(detect_current_tool())
            .map(|tool| vec![tool])
            .unwrap_or_else(hook_tools)
    } else if argv[0] == "all" {
        hook_tools()
    } else if let Some(tool) = parse_hook_tool(&argv[0]) {
        vec![tool]
    } else {
        eprintln!("Error: Unknown tool: {}", argv[0]);
        eprintln!("Valid options: {}", valid_hook_options());
        return 1;
    };

    // Install hooks — propagate error detail where available
    // Outcome: "already" = was already installed, "added" = newly added, "failed" = error
    enum AddResult {
        Already,
        Added,
        LegacyStripped,
        Failed(Option<String>),
    }
    let mut results: Vec<(Tool, AddResult)> = Vec::new();
    for tool in &tools {
        if tool.verify_hooks_installed(include_permissions) {
            // Plugin present but the legacy entries survived — a machine that
            // installed the plugin by hand, or a config synced from another
            // host. Both sets fire. Finishing the migration is the whole job of
            // this command, so do it here: the installer is never reached in
            // this state, and `hooks remove` would take the plugin down too.
            //
            // Cursor is excluded deliberately: its verifier only proves a
            // marketplace checkout exists, so "plugin present" here does not
            // mean the user ever enabled it — stripping would leave Cursor with
            // nothing. Cursor's legacy entries only ever go on an explicit
            // `hcom hooks remove cursor`.
            if tool.hooks_ship_as_plugin()
                && *tool != Tool::Cursor
                && legacy_hooks_present(*tool)
                && tool.remove_legacy_hooks_only()
            {
                results.push((*tool, AddResult::LegacyStripped));
                continue;
            }
            results.push((*tool, AddResult::Already));
            continue;
        }
        let outcome = match tool.try_setup_hooks(include_permissions) {
            Ok(()) => AddResult::Added,
            Err(msg) if msg.is_empty() => AddResult::Failed(None),
            Err(msg) => AddResult::Failed(Some(msg)),
        };
        results.push((*tool, outcome));
    }

    // Report results
    let post_status = get_tool_status();
    let mut added_count = 0;
    let mut fail_count = 0;
    for (tool, outcome) in &results {
        let path = post_status
            .iter()
            .find(|(t, _, _)| t == tool)
            .map(|(_, _, p)| p.as_str())
            .unwrap_or("");
        let name = tool.spec().label;
        // `hooks_settings_path()` returns the legacy config file for a plugin
        // tool, which the install path just stripped — printing it here would
        // report the file that was just emptied out as the install location.
        let location = if tool.hooks_ship_as_plugin() {
            "(plugin)".to_string()
        } else {
            format!("({path})")
        };
        match outcome {
            AddResult::Already => println!("{name} hooks already installed  {location}"),
            AddResult::LegacyStripped => {
                println!("{name} hooks already installed  {location}");
                println!("  removed the leftover legacy entries; only the plugin fires now");
            }
            AddResult::Added => {
                println!("Added {name} hooks  {location}");
                added_count += 1;
            }
            AddResult::Failed(Some(e)) => {
                eprintln!("Failed to add {name} hooks: {e}");
                fail_count += 1;
            }
            AddResult::Failed(None) => {
                eprintln!("Failed to add {name} hooks");
                fail_count += 1;
            }
        }
    }

    if added_count > 0 {
        println!();
        if tools.len() == 1 {
            println!("Restart {} to activate hooks.", tools[0].spec().label);
        } else {
            println!("Restart the tool(s) to activate hooks.");
        }
    }

    if fail_count > 0 { 1 } else { 0 }
}

/// Remove hooks for specified tool(s). Called from both `hcom hooks remove` and `hcom reset hooks`.
pub fn cmd_hooks_remove(argv: &[String]) -> i32 {
    // `--legacy-only` strips the old config entries and leaves an installed
    // plugin alone. It exists for the one state hcom cannot resolve itself:
    // Cursor's plugin is enabled in a TUI hcom cannot drive, so hcom never
    // strips Cursor's legacy hooks on its own. Without this flag the only
    // advice for that state would be a plain `hooks remove`, which also
    // uninstalls the plugin and leaves the tool with no hooks at all.
    let legacy_only = argv.iter().any(|a| a == "--legacy-only");
    let filtered: Vec<String> = argv
        .iter()
        .filter(|a| *a != "--legacy-only")
        .cloned()
        .collect();
    let argv = filtered.as_slice();

    // Determine which tools to remove.
    let tools: Vec<Tool> = if argv.is_empty() || (argv.len() == 1 && argv[0] == "all") {
        hook_tools()
    } else if let Some(tool) = parse_hook_tool(&argv[0]) {
        vec![tool]
    } else {
        eprintln!("Error: Unknown tool: {}", argv[0]);
        eprintln!("Valid options: {}", valid_hook_options());
        return 1;
    };

    // Check status for messaging, but always attempt removal for all paths
    // to clean up stale hooks at old paths (e.g. before env var override was set).
    let pre_status = get_tool_status();
    let mut fail_count = 0;
    for tool in &tools {
        let was_installed = pre_status
            .iter()
            .find(|(t, _, _)| t == tool)
            .map(|(_, installed, _)| *installed)
            .unwrap_or(false);
        let name = tool.spec().label;

        let ok = match if legacy_only {
            Ok(tool.remove_legacy_hooks_only())
        } else {
            tool.remove_hooks()
        } {
            Ok(ok) => ok,
            Err(e) => {
                eprintln!("Failed to remove {name} hooks: {e}");
                fail_count += 1;
                continue;
            }
        };
        if ok {
            if was_installed {
                println!("Removed {name} hooks");
            } else {
                println!("{name} hooks already removed");
            }
        } else {
            eprintln!("Failed to remove {name} hooks");
            fail_count += 1;
        }
    }

    if fail_count > 0 { 1 } else { 0 }
}

/// Detect current AI tool from environment.
fn detect_current_tool() -> &'static str {
    crate::shared::detect_current_tool_from_env()
}

pub fn cmd_hooks(_db: &HcomDb, args: &HooksArgs, _ctx: Option<&CommandContext>) -> i32 {
    let argv = &args.args;
    if argv.is_empty() {
        // No args = show status
        return cmd_hooks_status();
    }

    let first = argv[0].as_str();

    if first == "--help" || first == "-h" {
        let options = valid_hook_options();
        println!(
            "hcom hooks - Manage tool hooks for hcom integration\n\n\
             Hooks enable automatic message delivery and status tracking. Without hooks,\n\
             you can still use hcom in ad-hoc mode (run hcom start in any ai tool).\n\n\
             Usage:\n  \
             hcom hooks                  Show hook status for all tools\n  \
             hcom hooks status           Same as above\n  \
             hcom hooks add [tool]       Add hooks ({options})\n  \
             hcom hooks remove [tool]    Remove hooks ({options})\n\
             hcom hooks remove [tool] --legacy-only   Strip old config, keep the plugin\n\n\
             Examples:\n  \
             hcom hooks add claude       Add Claude Code hooks only\n  \
             hcom hooks add              Auto-detect tool or add all\n  \
             hcom hooks remove all       Remove all hooks\n\n\
             After adding, restart the tool to activate hooks."
        );
        return 0;
    }

    let sub_argv = argv[1..].to_vec();

    match first {
        "status" => cmd_hooks_status(),
        "add" | "install" => cmd_hooks_add(&sub_argv),
        "remove" | "uninstall" => cmd_hooks_remove(&sub_argv),
        _ => {
            eprintln!("Error: Unknown hooks subcommand: {first}");
            eprintln!("Usage: hcom hooks [status|add|remove] [tool]");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_flags_plugin_and_legacy_coexisting() {
        let line =
            super::plugin_status_line("claude", /* plugin */ true, /* legacy */ true);
        assert!(line.contains("double-fire"), "{line}");
        // `hooks add` handles this state: it finds the plugin installed and
        // strips the leftover legacy entries. `hooks remove` would be wrong
        // advice — it takes the plugin down too, leaving no hooks at all.
        assert!(line.contains("hcom hooks add claude"), "{line}");
        assert!(!line.contains("hooks remove"), "{line}");
    }

    /// Cursor cannot take the same advice: its verifier only proves a
    /// marketplace checkout exists, so `hooks add` must not strip on its
    /// behalf. The user removes the legacy entries once they have enabled the
    /// plugin in the TUI.
    /// Cross-vendor review (verify-taro, a Cursor agent) caught this: the
    /// advice told the user to run `hooks remove cursor`, which uninstalls the
    /// plugin *and* strips the legacy entries — following it left Cursor with
    /// no hooks at all. The Claude branch's own test says "hooks remove would
    /// be wrong — it takes the plugin down too"; the Cursor branch contradicted
    /// it.
    #[test]
    fn cursor_advice_never_sends_the_user_to_a_full_removal() {
        let line = super::plugin_status_line("cursor", true, true);
        let cmd = line
            .split("run: ")
            .nth(1)
            .and_then(|rest| rest.split(" (").next())
            .expect("advice must contain a `run: <command>`");
        assert_eq!(
            cmd, "hcom hooks remove cursor --legacy-only",
            "the command handed to the user must spare the plugin"
        );
        assert!(
            line.contains("leaving no hooks"),
            "must say why the plain command is wrong: {line}"
        );
    }

    #[test]
    fn status_tells_cursor_to_remove_only_after_enabling() {
        let line = super::plugin_status_line("cursor", true, true);
        assert!(line.contains("double-fire"), "{line}");
        assert!(line.contains("hcom hooks remove cursor"), "{line}");
        assert!(
            line.contains("/plugins"),
            "must say when it is safe: {line}"
        );
        assert!(!line.contains("hooks add"), "{line}");
    }

    #[test]
    fn status_flags_missing_install() {
        let line = super::plugin_status_line("cursor", false, false);
        assert!(line.contains("hcom hooks add cursor"), "{line}");
    }

    #[test]
    fn status_is_quiet_when_only_the_plugin_is_present() {
        let line = super::plugin_status_line("agy", true, false);
        assert!(
            line.is_empty(),
            "healthy state needs no advice, got: {line}"
        );
    }

    #[test]
    fn plugin_status_line_cursor_marketplace_only_is_not_an_install() {
        let line = super::plugin_status_line("cursor", true, false);
        assert!(
            line.contains("/plugins"),
            "a marketplace checkout is not an install; status must name the \
             remaining step. got: {line:?}"
        );
        assert!(
            !line.contains("installed"),
            "a marketplace checkout must not be reported as an install. got: {line:?}"
        );
    }

    #[test]
    fn plugin_status_line_cursor_no_checkout_admits_hooks_may_still_fire() {
        // Measured 2026-09-08: probe3-dune bound `hooks, pty` and took delivery
        // end to end with no Cursor marketplace at all — Cursor was reading
        // Claude's plugin cache. Flat "not installed" contradicted that.
        let line = super::plugin_status_line("cursor", false, false);
        assert!(
            line.contains("Claude"),
            "status must not claim Cursor has no hcom hooks. got: {line:?}"
        );
    }

    #[test]
    fn plugin_status_line_cursor_double_fire_is_a_risk_not_an_observation() {
        let line = super::plugin_status_line("cursor", true, true);
        assert!(
            !line.contains("both are firing"),
            "hcom cannot see whether the plugin is enabled, so it cannot say both \
             are firing. got: {line:?}"
        );
        assert!(line.contains("--legacy-only"), "got: {line:?}");
    }

    #[test]
    fn plugin_status_line_cursor_no_checkout_with_legacy_names_what_fires() {
        let line = super::plugin_status_line("cursor", false, true);
        assert!(
            line.contains("legacy"),
            "with legacy entries present, they are what is firing — status must \
             say so instead of repeating the generic install advice. got: {line:?}"
        );
        assert!(line.contains("/plugins"), "got: {line:?}");
    }

    #[test]
    fn plugin_status_line_other_tools_keep_their_wording() {
        assert_eq!(super::plugin_status_line("claude", true, false), "");
        assert_eq!(super::plugin_status_line("antigravity", true, false), "");
        assert!(
            super::plugin_status_line("claude", true, true).contains("both are firing"),
            "only Cursor's plugin state is unobservable; Claude's is not"
        );
        assert!(super::plugin_status_line("antigravity", false, false).contains("hooks add"));
    }

    #[test]
    fn test_detect_current_tool_default() {
        // In test env, none of the AI tool vars should be set
        // (unless running inside one, which is fine — it'll detect it)
        let tool = detect_current_tool();
        let parsed = tool
            .parse::<Tool>()
            .expect("detected tool must be canonical");
        assert!(
            parsed == Tool::Adhoc || hook_tools().contains(&parsed),
            "unexpected tool: {tool}"
        );
    }

    /// The printed tool list for `hcom config auto_approve` must name exactly
    /// what `refresh_installed_hook_permissions` can touch: every plugin tool
    /// excluded, since a plugin manifest carries no permission argument, and
    /// nothing hardcoded to drift out of sync with `hook_tools()`.
    #[test]
    fn auto_approve_managed_tools_excludes_plugin_tools() {
        let managed = super::auto_approve_managed_tools();
        assert!(!managed.is_empty());
        for tool in &managed {
            assert!(
                !tool.hooks_ship_as_plugin(),
                "{} ships as a plugin and must not be listed",
                tool.as_str()
            );
        }
        for tool in super::hook_tools() {
            assert_eq!(
                managed.contains(&tool),
                !tool.hooks_ship_as_plugin(),
                "{} membership disagrees with hooks_ship_as_plugin",
                tool.as_str()
            );
        }
    }

    /// `hcom config auto_approve <v>` must not reach a plugin tool's installer.
    /// That installer shells out to the tool's CLI and clones a marketplace
    /// over the network, and it cannot apply a permission setting anyway — a
    /// plugin manifest carries no permissions. Regression guard: this is the
    /// fourth side-effect install path found in this migration, after the
    /// launcher, bare start, and hooks add.
    #[test]
    fn permission_refresh_skips_plugin_tools() {
        let plugin_tools: Vec<_> = super::hook_tools()
            .into_iter()
            .filter(|t| t.hooks_ship_as_plugin())
            .collect();
        assert!(
            !plugin_tools.is_empty(),
            "expected at least one plugin-backed tool"
        );
        for tool in plugin_tools {
            assert!(
                tool.hooks_ship_as_plugin(),
                "{} must be skipped by refresh_installed_hook_permissions",
                tool.as_str()
            );
        }
    }
}
