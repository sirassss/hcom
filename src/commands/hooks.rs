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
///
/// Cursor does not go through here — see [`cursor_status_line`]. Its own
/// verifier can't tell "no install" from "Claude covers it" from "own cache
/// is present but broken", so it needs more than `(plugin, legacy)` to avoid
/// contradicting itself; every other plugin-shipped tool's state is fully
/// described by these two bools.
pub(crate) fn plugin_status_line(tool: &str, plugin: bool, legacy: bool) -> String {
    match (plugin, legacy) {
        // `hooks add` finishes the migration in this state: it finds the plugin
        // already installed and strips the leftover legacy entries. Pointing at
        // `hooks remove` instead would take the plugin down too and leave the
        // tool with nothing.
        (true, true) => format!(
            "{tool}: plugin and legacy hooks both present — both are firing (double-fire risk). \
             Run: hcom hooks add {tool} to drop the legacy entries."
        ),
        (false, _) => format!("{tool}: hooks not installed. Run: hcom hooks add {tool}"),
        (true, false) => String::new(),
    }
}

/// Everything [`cursor_status_line`] needs to describe Cursor's hook state.
///
/// Plain positional bools stopped being legible once Task 1 (Claude
/// coverage) and Task 11 (a cache entry present but its skill payload
/// broken) landed on top of Cursor's own `(installed, legacy)` pair — four
/// independent facts, not two, and two of them can be true at once without
/// contradicting each other (e.g. Claude covers the hook AND Cursor's own
/// cache attempt is broken and worth cleaning up). A struct makes call sites
/// name each fact instead of counting positions.
pub(crate) struct CursorStatus {
    /// `verify_cursor_plugin_installed()` — Cursor has its own verified cache.
    pub(crate) installed: bool,
    /// `legacy_hooks_present(Tool::Cursor)` — pre-plugin hook entries remain.
    pub(crate) legacy: bool,
    /// `cursor_hooks_covered()` — Claude's plugin is confirmed to be running
    /// the hook regardless of what Cursor has of its own.
    pub(crate) claude_covers: bool,
    /// `cursor_cache_entry_missing_skill_payload()` — Some when a Cursor cache
    /// entry has the hook file but a broken messaging-skill payload, which
    /// `installed` alone folds into the same `false` as "no cache at all".
    pub(crate) broken_skill_payload: Option<String>,
}

/// Cursor's status advice. Unlike [`plugin_status_line`], this can now give a
/// definitive answer instead of hedging: `claude_covers` (Task 1) tells hcom
/// for certain whether the hook is running via Claude's plugin cache, so a
/// spawned-agent check is no longer the only way to know.
pub(crate) fn cursor_status_line(status: &CursorStatus) -> String {
    match (status.installed, status.legacy) {
        // Cursor's own verifier proves the plugin materialized into Cursor's
        // plugin cache — stronger than a bare marketplace checkout, but still
        // not that the user enabled the plugin in the TUI (whether the cache
        // entry survives a disable is unmeasured), so hcom must never strip
        // Cursor's legacy hooks on its own — the user says when, once they
        // know the plugin is live.
        (true, true) => "cursor: legacy hooks are firing, and the plugin cache shows an \
             install too — a double-fire risk once /plugins shows hcom enabled, which hcom \
             cannot confirm from disk. Once it is enabled, run: \
             hcom hooks remove cursor --legacy-only \
             (plain `hooks remove` would uninstall the plugin too, leaving no hooks)"
            .to_string(),
        (true, false) => "cursor: plugin cache shows an install, and no legacy hooks are \
             left. hcom cannot confirm from disk whether it is still enabled in /plugins — \
             confirm with a spawned agent showing `bindings: hooks, pty` in hcom list, \
             read after its first turn."
            .to_string(),
        // Cursor has nothing verified of its own, but Claude's plugin
        // definitively covers the hook (measured M1: cursor-agent reads
        // Claude's plugin cache directly). No more hedging needed here.
        (false, legacy) if status.claude_covers => {
            let mut line = cursor_covered_by_claude_message();
            if legacy {
                line.push_str(
                    " Legacy hook entries are also present here and are firing alongside \
                     Claude's plugin — once you've confirmed that's what you want, run: \
                     hcom hooks remove cursor --legacy-only",
                );
            }
            if let Some(reason) = &status.broken_skill_payload {
                line.push_str(&format!(
                    " Separately: a Cursor-owned plugin cache entry exists here but its own \
                     messaging skill payload is broken ({reason}) — that copy is unused while \
                     Claude's plugin covers the hook, but /plugins → install \"hcom\" again \
                     will repair or replace it."
                ));
            }
            line
        }
        // Not covered by Claude, and Cursor's own verifier says `false` — but
        // a cache entry does exist with the two marker files Task 7 checks
        // (`.cache-complete`, `hooks-cursor.json`); only the skill payload it
        // also requires is broken. That hook wiring is independent of the
        // skill files, so the hook can still be firing even though the skill
        // is absent — this is NOT "no cache", and must not share that claim.
        (false, legacy) if status.broken_skill_payload.is_some() => {
            let mut line = cursor_skill_payload_status_line(status.broken_skill_payload.clone());
            if legacy {
                line.push_str(
                    " Legacy hook entries are also present and are what is reliably firing \
                     until the plugin cache is repaired.",
                );
            }
            line
        }
        // Genuinely nothing of Cursor's own, but the legacy hook entries are
        // what is firing. Claude does not cover it here (the guard above
        // already ruled that out), so this is not a hedge.
        (false, true) => "cursor: no Cursor plugin cache; the legacy hook entries are what \
             is firing. Run: hcom hooks add cursor, then /plugins → install \"hcom\", and \
             only then hcom hooks remove cursor --legacy-only"
            .to_string(),
        // Genuinely nothing: no Claude coverage, no Cursor cache of any kind
        // (healthy or broken), no legacy entries. Definitively not running.
        (false, false) => "cursor: hooks not installed — Claude does not cover it and \
             Cursor has no plugin cache of its own. Run: hcom hooks add cursor, then \
             /plugins → install \"hcom\"."
            .to_string(),
    }
}

fn agy_skill_payload_status_line(payload: Result<(), String>) -> String {
    match payload {
        Ok(()) => String::new(),
        Err(reason) => format!(
            "antigravity: hooks present; messaging skill payload is missing or incomplete \
             ({reason}). Run: hcom hooks add antigravity to reinstall the complete plugin."
        ),
    }
}

/// Advice for a Cursor-owned cache entry that has Task 7's two marker files
/// (`.cache-complete`, `hooks/hooks-cursor.json`) but fails the skill-payload
/// check that verifier folds in — a state `verify_cursor_plugin_installed`
/// cannot distinguish from "no cache entry at all" (both report `false`).
/// [`cursor_status_line`] is what decides when this applies instead of the
/// plain "no cache" wording; hcom cannot write into `~/.cursor/` (Task 11
/// decision) — Cursor alone materializes its cache — so the only move is to
/// name the gap and point at the one thing that fixes it: re-running the
/// `/plugins` picker.
fn cursor_skill_payload_status_line(reason: Option<String>) -> String {
    match reason {
        None => String::new(),
        Some(reason) => format!(
            "cursor: a plugin cache entry has the hook file but its messaging skill payload \
             is missing or incomplete ({reason}). The hook may still be running even though \
             the skill is absent. In Cursor: /plugins → install \"hcom\" again to \
             re-materialize it."
        ),
    }
}

/// `cursor_registry_confirms` only gates Cursor, and only when Claude does
/// not already cover it: an on-disk cache surviving `cursor-agent plugin
/// marketplace remove hcom` run outside hcom (M5, `verify_cursor_plugin_installed`
/// left permanently true — that CLI leaves the checkout untouched, plan M3)
/// must not read as "already installed" unless the registry
/// (`cursor_registry_lists_hcom`) still lists hcom. Every other tool, and
/// the Claude-covers-it branch, ignore it — it is folded to `true` (a no-op)
/// by callers for whom it does not apply.
fn plugin_add_can_short_circuit(
    tool: Tool,
    hooks_installed: bool,
    agy_payload_complete: bool,
    cursor_covered_by_claude: bool,
    cursor_registry_confirms: bool,
    force_own: bool,
) -> bool {
    // `--own` means "install a Cursor-owned copy regardless of what already
    // looks installed" — it must bypass every Cursor short-circuit below, not
    // just the Claude-covered one. Without this early return, a stale on-disk
    // cache plus a registry check that fails open (`cursor_registry_lists_hcom`
    // returning `true` on a `cursor-agent` CLI error) still short-circuited
    // `--own` to a no-op, defeating the one flag meant to force past exactly
    // that trap.
    if tool == Tool::Cursor && force_own {
        return false;
    }
    let cursor_cache_confirmed = tool != Tool::Cursor || cursor_registry_confirms;
    (hooks_installed && cursor_cache_confirmed
        || (tool == Tool::Cursor && cursor_covered_by_claude))
        && (tool != Tool::Antigravity || agy_payload_complete)
}

/// Message for `hooks add cursor` when Cursor has no install of its own but
/// `cursor_hooks_covered()` is true because Claude's plugin is installed —
/// `cursor-agent` reads Claude's plugin cache directly (see
/// `cursor_hooks_covered` in `hooks/plugin.rs`), so a second install would
/// only add artifacts, not a second working hook (measured M2).
///
/// `--own` forces a Cursor-owned install anyway (see the `force_own` param on
/// `plugin_add_can_short_circuit` and its parsing in `cmd_hooks_add`).
fn cursor_covered_by_claude_message() -> String {
    "Cursor: currently running Claude's installed hcom plugin — no separate \
     Cursor install needed. Removing hcom from Claude will also remove \
     Cursor's hook. To install a separate copy for Cursor anyway, run: \
     hcom hooks add cursor --own"
        .to_string()
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
/// Render a Codex hook status as the lines `hooks status` prints. Separated
/// from the printing so the states that must never read as "active" can be
/// asserted in a unit test.
fn codex_status_lines(
    status: &crate::hooks::codex::CodexPluginStatus,
    hooks_path: &str,
    claude: &crate::hooks::codex::ClaudePresence,
) -> Vec<String> {
    use crate::hooks::codex::CodexPluginState;

    let mut lines = vec![match status.state {
        // Only the states that actually observed hcom's legacy file name it.
        CodexPluginState::LegacyOnly | CodexPluginState::Duplicate => {
            format!("codex:  {} ({hooks_path})", status.state.headline())
        }
        // The spec splits "no hooks" in two, on whether Codex can import an
        // installed Claude plugin. Only a confirmed presence changes the
        // headline: a probe that could not answer leaves "not installed"
        // standing and says why.
        CodexPluginState::Missing => match claude {
            crate::hooks::codex::ClaudePresence::Present => {
                "codex:  not active; import from Claude required".to_string()
            }
            _ => format!("codex:  {}", status.state.headline()),
        },
        _ => format!("codex:  {}", status.state.headline()),
    }];
    if let crate::hooks::codex::ClaudePresence::Indeterminate(why) = claude
        && status.state == CodexPluginState::Missing
    {
        lines.push(format!("  could not check for Claude: {why}"));
    }
    lines.extend(status.details.iter().map(|detail| format!("  {detail}")));
    lines
}

/// The one-word(ish) headline `hooks status` prints for a plugin-shipped
/// tool. Split out from `cmd_hooks_status` so it is unit-testable without a
/// full filesystem/env sandbox.
///
/// `installed == false` alone can't distinguish "no Cursor cache at all"
/// from "a cache entry exists but its skill payload is broken" (Task 7's
/// `verify_cursor_plugin_installed` folds both to `false`; Task 11's
/// `cursor_cache_entry_missing_skill_payload` is what tells them apart) — the
/// old two-column match collapsed the broken-payload case into "no plugin
/// cache", directly contradicting the advice line printed right below it,
/// which names the cache entry that does exist.
fn plugin_status_headline(
    is_cursor: bool,
    installed: bool,
    broken_skill_payload: bool,
) -> &'static str {
    match (is_cursor, installed, broken_skill_payload) {
        (true, true, _) => "plugin cache ready",
        (true, false, true) => "plugin cache incomplete",
        (true, false, false) => "no plugin cache",
        (false, true, _) => "installed   ",
        (false, false, _) => "not installed",
    }
}

fn cmd_hooks_status() -> i32 {
    let status = get_tool_status();
    for (tool, installed, path) in &status {
        let tool = *tool;
        if tool.hooks_ship_as_plugin() {
            // `hooks_settings_path()` returns the legacy file for these three
            // tools, which holds nothing once the plugin is in use — printing
            // it next to "installed" would point at an empty file.
            // Cursor's signal is weaker than the others' in both directions: a
            // materialized plugin cache does not prove the plugin is enabled
            // in the TUI, and no cache does not mean "no hooks" — Claude may
            // definitively cover it instead (see `cursor_status_line` below,
            // which knows for certain via `cursor_hooks_covered()`). Neither
            // headline may be stated flatly for it.
            // Computed once and reused for both the headline and the advice
            // line below — `installed == false` alone can't tell "no cache at
            // all" apart from "a cache entry exists but its skill payload is
            // broken" (Task 7/11), and printing "no plugin cache" for the
            // latter directly contradicts the advice line underneath it,
            // which says a cache entry DOES exist.
            let broken_skill_payload = if tool == Tool::Cursor {
                crate::hooks::plugin::cursor_cache_entry_missing_skill_payload()
            } else {
                None
            };
            let state = plugin_status_headline(
                tool == Tool::Cursor,
                *installed,
                broken_skill_payload.is_some(),
            );
            if *installed || tool == Tool::Cursor {
                println!("{}:  {state} (plugin)", tool.spec().label);
            } else {
                println!("{}:  {state}", tool.spec().label);
            }
            let advice = if tool == Tool::Cursor {
                cursor_status_line(&CursorStatus {
                    installed: *installed,
                    legacy: legacy_hooks_present(tool),
                    claude_covers: crate::hooks::plugin::cursor_hooks_covered(),
                    broken_skill_payload: broken_skill_payload.clone(),
                })
            } else {
                plugin_status_line(tool.as_str(), *installed, legacy_hooks_present(tool))
            };
            if !advice.is_empty() {
                println!("  {advice}");
            }
            if tool == Tool::Antigravity && *installed {
                let payload = agy_skill_payload_status_line(
                    crate::hooks::plugin::verify_plugin_skill_payload(
                        &crate::hooks::plugin::agy_plugin_dir(),
                    ),
                );
                if !payload.is_empty() {
                    println!("  {payload}");
                }
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
        } else if tool == Tool::Codex {
            // Codex's own inventory is the only authority on what is firing:
            // a legacy hooks.json on disk says nothing about a plugin's
            // handlers, and vice versa. `installed` is deliberately unused here.
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // `path` here is Codex's config.toml; hcom's legacy hook entries
            // live in hooks.json beside it, and that is the file the legacy
            // states are about.
            let hooks_file = crate::hooks::codex::get_codex_hooks_path();
            let codex_status = crate::hooks::codex::codex_plugin_status(&cwd);
            // Probe Claude only when the answer can change what is printed.
            let claude = if codex_status.state == crate::hooks::codex::CodexPluginState::Missing {
                crate::hooks::codex::claude_presence()
            } else {
                crate::hooks::codex::ClaudePresence::Absent
            };
            for line in codex_status_lines(&codex_status, &hooks_file.to_string_lossy(), &claude) {
                println!("{line}");
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
    // `--own` forces a Cursor-owned install even when Claude's plugin already
    // covers Cursor (see `cursor_covered_by_claude_message`) — the escape
    // hatch for a user who wants Cursor's hook independent of Claude's. It
    // only means anything for Cursor; every other tool ignores it. Parsed the
    // same way `cmd_hooks_remove` parses `--legacy-only`: stripped out of
    // argv before tool-name resolution, checked as a bool.
    let force_own = argv.iter().any(|a| a == "--own");
    let filtered: Vec<String> = argv.iter().filter(|a| *a != "--own").cloned().collect();
    let argv = filtered.as_slice();

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
        /// Cursor has no install of its own, but Claude's covers it — a
        /// distinct message from `Already`, since there is nothing installed
        /// to call "already installed" on Cursor's side.
        CursorCoveredByClaude,
        /// Nothing was installed, or an install is not yet confirmed running.
        /// Exit 2, so a dotfiles installer can tell "you must act" apart from
        /// both success and failure.
        Pending(String),
        Failed(Option<String>),
    }
    let mut results: Vec<(Tool, AddResult)> = Vec::new();
    for tool in &tools {
        // Codex routes on its own hook inventory, not on a file check: the
        // legacy file says nothing about a plugin's handlers, and nothing here
        // may strip the legacy entries, which are the only thing firing until
        // the user trusts the plugin.
        if *tool == Tool::Codex {
            let outcome = match crate::hooks::codex::add_codex_plugin() {
                Ok(crate::hooks::codex::CodexAddOutcome::AlreadyActive) => AddResult::Already,
                Ok(crate::hooks::codex::CodexAddOutcome::ActionRequired(text))
                | Ok(crate::hooks::codex::CodexAddOutcome::InstalledUnverified(text)) => {
                    AddResult::Pending(text)
                }
                Err(error) => AddResult::Failed(Some(error)),
            };
            results.push((*tool, outcome));
            continue;
        }
        let hooks_installed = tool.verify_hooks_installed(include_permissions);
        let agy_payload_complete = *tool != Tool::Antigravity
            || crate::hooks::plugin::verify_plugin_skill_payload(
                &crate::hooks::plugin::agy_plugin_dir(),
            )
            .is_ok();
        // Only relevant, and only computed, for Cursor without its own
        // install — cursor_hooks_covered() re-checks Claude's plugin state,
        // no reason to pay for that read on any other tool.
        let cursor_covered_by_claude = *tool == Tool::Cursor
            && !hooks_installed
            && crate::hooks::plugin::cursor_hooks_covered();
        // Only worth a subprocess call when it can actually change the
        // outcome: Cursor, not already covered by Claude, and the on-disk
        // cache claims installed (M5 — that cache can be a stale orphan left
        // behind by `cursor-agent plugin marketplace remove hcom` run
        // outside hcom). Short-circuits to `true` (a no-op for the check
        // below) in every other case.
        let cursor_registry_confirms = *tool != Tool::Cursor
            || cursor_covered_by_claude
            || !hooks_installed
            || crate::hooks::plugin::cursor_registry_lists_hcom();
        if plugin_add_can_short_circuit(
            *tool,
            hooks_installed,
            agy_payload_complete,
            cursor_covered_by_claude,
            cursor_registry_confirms,
            force_own,
        ) {
            if cursor_covered_by_claude {
                results.push((*tool, AddResult::CursorCoveredByClaude));
                continue;
            }
            // Plugin present but the legacy entries survived — a machine that
            // installed the plugin by hand, or a config synced from another
            // host. Both sets fire. Finishing the migration is the whole job of
            // this command, so do it here: the installer is never reached in
            // this state, and `hooks remove` would take the plugin down too.
            //
            // Cursor is excluded deliberately: its verifier only proves the
            // plugin materialized into Cursor's plugin cache, so "plugin
            // present" here does not mean the user ever enabled it in the
            // TUI — stripping would leave Cursor with nothing. Cursor's
            // legacy entries only ever go on an explicit `hcom hooks remove
            // cursor`.
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
    let mut fail_count: usize = 0;
    let mut pending_count: usize = 0;
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
            AddResult::CursorCoveredByClaude => println!("{}", cursor_covered_by_claude_message()),
            AddResult::Already => println!("{name} hooks already installed  {location}"),
            AddResult::LegacyStripped => {
                println!("{name} hooks already installed  {location}");
                println!("  removed the leftover legacy entries; only the plugin fires now");
            }
            AddResult::Added => {
                println!("Added {name} hooks  {location}");
                added_count += 1;
            }
            AddResult::Pending(text) => {
                println!("{name}: action required");
                for line in text.lines() {
                    println!("  {line}");
                }
                pending_count += 1;
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

    add_exit_code(fail_count, pending_count)
}

/// `hooks add` exit contract, frozen here because the dotfiles installer reads
/// it: 1 = something failed, 2 = nothing failed but the user must act, 0 =
/// done. An error outranks a pending outcome, so `add all` that failed
/// somewhere never reports as merely "needs your attention".
fn add_exit_code(fail_count: usize, pending_count: usize) -> i32 {
    if fail_count > 0 {
        1
    } else if pending_count > 0 {
        2
    } else {
        0
    }
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
             hcom hooks add cursor --own Force a Cursor-owned install even if Claude's plugin covers it\n  \
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

    /// No state reachable without a usable inventory may read as active or as
    /// an observation that both hook sets are firing.
    #[test]
    fn add_exit_code_puts_failure_above_pending() {
        assert_eq!(super::add_exit_code(0, 0), 0);
        assert_eq!(super::add_exit_code(0, 2), 2);
        assert_eq!(super::add_exit_code(1, 0), 1);
        assert_eq!(
            super::add_exit_code(1, 3),
            1,
            "a failure must not report as pending"
        );
    }

    /// Codex is routed explicitly in `cmd_hooks_add`, not through the
    /// plugin-tool fast path — that path strips the legacy entries, and
    /// Codex's are the only hooks firing until the user trusts the plugin.
    #[test]
    fn codex_never_reaches_the_automatic_legacy_strip() {
        assert!(
            !Tool::Codex.hooks_ship_as_plugin(),
            "Codex in the plugin fast path would strip its legacy hooks on `hooks add`"
        );
    }

    #[test]
    fn codex_states_without_evidence_never_claim_hooks_are_firing() {
        use crate::hooks::codex::{CodexPluginState, CodexPluginStatus};

        for state in [
            CodexPluginState::Unverified,
            CodexPluginState::Discovered,
            CodexPluginState::Missing,
            CodexPluginState::Incomplete,
            CodexPluginState::Incompatible,
        ] {
            let status = CodexPluginStatus {
                state,
                details: vec!["plugin store populated: /codex-home/plugins/cache".to_string()],
            };
            let rendered = super::codex_status_lines(
                &status,
                "/codex-home/hooks.json",
                &crate::hooks::codex::ClaudePresence::Absent,
            )
            .join("\n");
            assert!(
                !rendered.contains("hooks active") && !rendered.contains("both are firing"),
                "{state:?} rendered as an activation claim: {rendered}"
            );
            // The legacy path is evidence of nothing in these states, so it
            // must not appear next to the headline.
            assert!(
                !rendered.lines().next().unwrap().contains("hooks.json"),
                "{state:?} named the legacy file without observing it: {rendered}"
            );
        }
    }

    #[test]
    fn codex_duplicate_names_the_legacy_file_to_remove() {
        use crate::hooks::codex::{CodexPluginState, CodexPluginStatus};

        let rendered = super::codex_status_lines(
            &CodexPluginStatus {
                state: CodexPluginState::Duplicate,
                details: Vec::new(),
            },
            "/codex-home/hooks.json",
            &crate::hooks::codex::ClaudePresence::Absent,
        )
        .join("\n");
        assert!(
            rendered.contains("duplicate hooks; double-fire risk"),
            "{rendered}"
        );
        assert!(rendered.contains("/codex-home/hooks.json"), "{rendered}");
    }

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

    /// When the verifier says no, `hooks add` must reach `try_setup_hooks`
    /// instead of printing "already installed" — the originally reported bug
    /// this plan's verifier tightening exists to fix.
    #[test]
    fn add_does_not_short_circuit_when_the_verifier_says_no() {
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Cursor,
            false,
            true,
            false,
            true,
            false
        ));
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Claude,
            false,
            true,
            false,
            true,
            false
        ));
    }

    /// M1/M2: `cursor-agent` reads Claude's plugin cache directly, so
    /// `hooks add cursor` must short-circuit — no CLI call, no second
    /// install — the moment Claude's plugin covers it, even though Cursor's
    /// own verifier (`hooks_installed` here) still says false.
    #[test]
    fn add_cursor_short_circuits_when_claude_covers() {
        assert!(super::plugin_add_can_short_circuit(
            Tool::Cursor,
            false, // verify_cursor_plugin_installed() — Cursor has no cache of its own
            true,
            true,  // cursor_hooks_covered() — Claude's plugin covers it
            false, // cursor_registry_lists_hcom() — irrelevant, Claude's coverage takes over
            false,
        ));
    }

    /// Without Claude's coverage, Cursor with no cache of its own must still
    /// go through the normal install path.
    #[test]
    fn add_cursor_still_installs_without_claude() {
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Cursor,
            false,
            true,
            false,
            true,
            false,
        ));
    }

    /// M5: `cursor-agent plugin marketplace remove hcom` run outside hcom
    /// leaves the on-disk cache (`.cache-complete`, `hooks-cursor.json`, skill
    /// payload) completely untouched (plan M3), so
    /// `verify_cursor_plugin_installed()` — `hooks_installed` here — still
    /// reads true. Without a registry check, `hooks add cursor` would read
    /// that stale cache as "already installed" and permanently block
    /// reinstall. Claude does not cover it here either, so the only signal
    /// left is the registry: `cursor_registry_lists_hcom()` (mocked via
    /// `HCOM_TEST_CURSOR_MARKETPLACE_LIST` in `hooks/plugin.rs` tests) says
    /// hcom is not registered, so the stale cache must not short-circuit.
    #[test]
    fn add_cursor_reinstalls_when_only_a_stale_cache_remains() {
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Cursor,
            true, // verify_cursor_plugin_installed() — stale, orphaned cache
            true,
            false, // cursor_hooks_covered() — Claude does not cover it
            false, // cursor_registry_lists_hcom() — registry does not confirm
            false,
        ));
        // Contrast: the same stale-looking cache short-circuits once the
        // registry actually confirms it — the ordinary healthy-install path
        // this must not break.
        assert!(super::plugin_add_can_short_circuit(
            Tool::Cursor,
            true,
            true,
            false,
            true, // registry confirms
            false,
        ));
    }

    /// Task 6 (already merged) rewrote `verify_agy_plugin_installed()` to
    /// require BOTH an `import_manifest.json` entry naming hcom's hooks
    /// component AND the hook file on disk — an orphan hook file left behind
    /// after `agy plugin uninstall` (or extracted by hand) with no manifest
    /// entry now makes it return false, so `hooks_installed` here is already
    /// false in that state. This locks that in at the `cmd_hooks_add`
    /// short-circuit level: an orphan dir must never read as "already
    /// installed" and block reinstall.
    #[test]
    fn add_antigravity_reinstalls_over_orphan_dir() {
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Antigravity,
            false, // verify_agy_plugin_installed() — orphan dir, no manifest entry
            true,  // skill payload files may still be present on disk
            false,
            true,
            false,
        ));
    }

    /// Task 3: `hcom hooks add cursor --own` must force a Cursor-owned
    /// install even when Claude's plugin already covers Cursor — the escape
    /// hatch `cursor_covered_by_claude_message` promises. Before this flag
    /// was wired up, `--own` was silently ignored by `cmd_hooks_add` (only
    /// `argv[0]` is inspected for the tool name), so this state still
    /// short-circuited and never reached `try_setup_hooks`.
    #[test]
    fn add_cursor_force_installs_despite_claude() {
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Cursor,
            false, // Cursor has no cache of its own
            true,
            true, // Claude's plugin covers it
            true, // cursor_registry_lists_hcom() — irrelevant, --own forces install anyway
            true, // --own forces the real install anyway
        ));
    }

    /// M5's trap can also be reached with Claude NOT covering it: a stale
    /// on-disk cache reads `hooks_installed == true`, and `cursor-agent`
    /// erroring out makes `cursor_registry_lists_hcom()` fail open to `true`
    /// (correct for its original uninstall caller, wrong here) — so
    /// `cursor_registry_confirms` is `true` too. Before this fix, `--own`
    /// only suppressed the Claude-covered disjunct and did nothing here, so
    /// this exact state still short-circuited to "already installed" even
    /// with `--own` passed — the trap Task 5 was built to close, just
    /// relocated behind a CLI failure instead of a registry removal.
    #[test]
    fn add_cursor_force_installs_despite_stale_cache_and_broken_registry_check() {
        assert!(!super::plugin_add_can_short_circuit(
            Tool::Cursor,
            true, // verify_cursor_plugin_installed() — stale on-disk cache
            true,
            false, // cursor_hooks_covered() — Claude does NOT cover it
            true,  // cursor_registry_lists_hcom() fails open on a CLI error
            true,  // --own must still force the real install
        ));
    }

    #[test]
    fn cursor_covered_by_claude_message_explains_and_names_the_force_flag() {
        let msg = super::cursor_covered_by_claude_message();
        assert!(msg.contains("Claude"), "{msg}");
        assert!(
            msg.contains("Removing hcom from Claude") && msg.contains("remove Cursor's hook"),
            "must warn that removing hcom from Claude takes Cursor's hook with it: {msg}"
        );
        assert!(
            msg.contains("hooks add cursor --own"),
            "must name the forced-install path: {msg}"
        );
    }

    #[test]
    fn agy_hooks_without_skill_payload_require_reinstall() {
        assert!(
            !super::plugin_add_can_short_circuit(
                Tool::Antigravity,
                true,
                false,
                false,
                true,
                false
            ),
            "hook presence alone must not bypass AGY repair"
        );
        let line = super::agy_skill_payload_status_line(Err(
            "missing skill payload skills/hcom-agent-messaging/SKILL.md".to_string(),
        ));
        assert!(line.contains("hooks present"), "{line}");
        assert!(line.contains("skill payload"), "{line}");
        assert!(line.contains("hcom hooks add antigravity"), "{line}");
    }

    #[test]
    fn complete_agy_payload_can_use_the_installed_shortcut() {
        assert!(super::plugin_add_can_short_circuit(
            Tool::Antigravity,
            true,
            true,
            false,
            true,
            false,
        ));
        assert!(super::agy_skill_payload_status_line(Ok(())).is_empty());
    }

    /// Terse constructor for `cursor_status_line` tests — every field named
    /// at the call site instead of counting positions.
    fn cursor_status(
        installed: bool,
        legacy: bool,
        claude_covers: bool,
        broken_skill_payload: Option<&str>,
    ) -> super::CursorStatus {
        super::CursorStatus {
            installed,
            legacy,
            claude_covers,
            broken_skill_payload: broken_skill_payload.map(str::to_string),
        }
    }

    /// Cursor cannot take the same advice: its verifier only proves the
    /// plugin materialized into Cursor's plugin cache, so `hooks add` must
    /// not strip on its behalf. The user removes the legacy entries once
    /// they have enabled the plugin in the TUI.
    /// Cross-vendor review (verify-taro, a Cursor agent) caught this: the
    /// advice told the user to run `hooks remove cursor`, which uninstalls the
    /// plugin *and* strips the legacy entries — following it left Cursor with
    /// no hooks at all. The Claude branch's own test says "hooks remove would
    /// be wrong — it takes the plugin down too"; the Cursor branch contradicted
    /// it.
    #[test]
    fn cursor_advice_never_sends_the_user_to_a_full_removal() {
        let line = super::cursor_status_line(&cursor_status(true, true, false, None));
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
        let line = super::cursor_status_line(&cursor_status(true, true, false, None));
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
        let line = super::cursor_status_line(&cursor_status(false, false, false, None));
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
    fn plugin_status_line_cursor_cache_present_does_not_claim_enabled() {
        // A materialized plugin cache proves an install happened, but Task 6
        // re-measured that it still doesn't prove the plugin is *enabled* in
        // the TUI right now (unmeasured whether the cache survives a
        // disable) — so status must not go silent the way it does for the
        // other plugin tools' healthy (true, false) state.
        let line = super::cursor_status_line(&cursor_status(true, false, false, None));
        assert!(
            line.contains("/plugins"),
            "must say where to confirm the enabled state. got: {line:?}"
        );
        assert!(
            line.contains("cannot confirm"),
            "a plugin cache must not be reported as proof the plugin is enabled. \
             got: {line:?}"
        );
    }

    /// Branch 1 (Task 8): Claude's plugin definitively covers the hook and
    /// Cursor has no cache of its own — the whole point of Task 1's
    /// `cursor_hooks_covered()`. Must be definitive (no "may still be"
    /// hedging), name the no-separate-install-needed fact, and name what
    /// removing hcom from Claude does to it.
    #[test]
    fn cursor_status_line_claude_covers_with_no_own_cache() {
        let line = super::cursor_status_line(&cursor_status(false, false, true, None));
        assert!(
            line.contains("Claude") && line.contains("no separate"),
            "must state plainly that no separate Cursor install is needed: {line:?}"
        );
        assert!(
            line.contains("Removing hcom from Claude") && line.contains("remove Cursor's hook"),
            "must name the removal consequence: {line:?}"
        );
        assert!(
            line.contains("hooks add cursor --own"),
            "must still name the force-install escape hatch: {line:?}"
        );
        assert!(
            !line.contains("may still be"),
            "Task 1 makes this definitive; must not hedge: {line:?}"
        );
    }

    /// Branch 2 (Task 8): neither Claude nor Cursor's own cache covers it —
    /// genuinely not running. No more "Cursor may still be running out of
    /// Claude's cache" speculation once `cursor_hooks_covered()` says no.
    #[test]
    fn cursor_status_line_neither_covers_it() {
        let line = super::cursor_status_line(&cursor_status(false, false, false, None));
        assert!(line.contains("hcom hooks add cursor"), "{line:?}");
        assert!(
            !line.contains("may still be running"),
            "with cursor_hooks_covered() false, there is nothing left to hedge about: {line:?}"
        );
    }

    /// Companion headline fix for the same broken-payload state as
    /// `cursor_status_line_claude_covers_and_own_cache_broken_states_both_facts`
    /// / `cursor_status_line_broken_payload_without_claude_names_the_entry`:
    /// the headline `hooks status` prints one line above that advice must not
    /// say "no plugin cache" when a cache entry does exist, broken payload or
    /// not — it gets its own headline instead of collapsing into the
    /// genuinely-empty case.
    #[test]
    fn status_headline_distinguishes_broken_cache_from_no_cache() {
        assert_eq!(
            super::plugin_status_headline(true, false, false),
            "no plugin cache"
        );
        assert_eq!(
            super::plugin_status_headline(true, false, true),
            "plugin cache incomplete"
        );
        assert_eq!(
            super::plugin_status_headline(true, true, false),
            "plugin cache ready"
        );
        // A non-Cursor tool ignores the broken-payload bit entirely — only
        // Cursor's verifier folds two distinct states into one `false`.
        assert_eq!(
            super::plugin_status_headline(false, false, true),
            "not installed"
        );
    }

    /// Branch 4 (Task 8, the deferred Minor from Task 11's review): a Cursor
    /// cache entry exists with a broken skill payload, but Claude's plugin
    /// separately covers the hook. Both true facts must survive in the same
    /// message without contradicting each other — the old code printed "no
    /// Cursor plugin cache" right next to "a cache entry exists".
    #[test]
    fn cursor_status_line_claude_covers_and_own_cache_broken_states_both_facts() {
        let line = super::cursor_status_line(&cursor_status(
            false,
            false,
            true,
            Some("missing skill payload skills/hcom-agent-messaging/SKILL.md"),
        ));
        assert!(
            line.contains("Claude") && line.contains("no separate"),
            "must still state Claude's coverage plainly: {line:?}"
        );
        assert!(
            line.contains("skill payload") && line.contains("broken"),
            "must also name the broken own-cache attempt: {line:?}"
        );
        assert!(
            !line.contains("no Cursor plugin cache") && !line.contains("no plugin cache"),
            "must not claim no cache exists when one does, broken or not: {line:?}"
        );
    }

    /// A cache entry with a broken skill payload but no Claude coverage: the
    /// hook wiring (`hooks-cursor.json`) is independent of the skill files,
    /// so it can still be firing even though the skill is absent — this must
    /// not collapse into the generic "no cache" wording.
    #[test]
    fn cursor_status_line_broken_payload_without_claude_names_the_entry() {
        let line = super::cursor_status_line(&cursor_status(
            false,
            false,
            false,
            Some("missing skill payload skills/hcom-agent-messaging/SKILL.md"),
        ));
        assert!(line.contains("/plugins"), "{line:?}");
        assert!(
            line.contains("hook") && line.contains("may still be running"),
            "must say the hook can still be firing even though the skill is absent: {line:?}"
        );
        assert!(
            !line.contains("no Cursor plugin cache") && !line.contains("no plugin cache"),
            "a cache entry does exist, broken payload or not: {line:?}"
        );
    }

    /// Claude covers the hook AND legacy entries are also present: both are
    /// genuinely firing now (Task 1 makes this an observation, not a guess),
    /// so the double-fire risk and its `--legacy-only` remedy must both
    /// survive alongside the Claude-coverage statement.
    #[test]
    fn cursor_status_line_claude_covers_with_legacy_also_present() {
        let line = super::cursor_status_line(&cursor_status(false, true, true, None));
        assert!(line.contains("Claude"), "{line:?}");
        assert!(
            line.contains("Legacy") && line.contains("hcom hooks remove cursor --legacy-only"),
            "must still name the legacy remedy: {line:?}"
        );
    }

    #[test]
    fn cursor_skill_payload_status_line_names_reinstall_and_live_hook_risk() {
        // Task 7 folded the skill-payload check into
        // `verify_cursor_plugin_installed`, so a cache entry with the two
        // marker files but a broken skill payload collapses into the same
        // `installed == false` bucket as "no cache at all" — the generic
        // `plugin_status_line` message ("no Cursor plugin cache") is then
        // inaccurate: a cache entry *does* exist, it is just missing its
        // skill. This line names that specific case instead.
        let line = super::cursor_skill_payload_status_line(Some(
            "missing skill payload skills/hcom-agent-messaging/SKILL.md".to_string(),
        ));
        assert!(
            line.contains("/plugins"),
            "must point at the TUI picker: {line:?}"
        );
        assert!(
            line.to_lowercase().contains("hcom"),
            "must name the plugin to install: {line:?}"
        );
        assert!(
            line.contains("hook") && line.contains("may still be running"),
            "must say the hook can still be firing even though the skill is absent: {line:?}"
        );
    }

    #[test]
    fn cursor_skill_payload_status_line_is_quiet_when_healthy() {
        assert!(super::cursor_skill_payload_status_line(None).is_empty());
    }

    #[test]
    fn plugin_status_line_cursor_double_fire_is_a_risk_not_an_observation() {
        let line = super::cursor_status_line(&cursor_status(true, true, false, None));
        assert!(
            !line.contains("both are firing"),
            "hcom cannot see whether the plugin is enabled, so it cannot say both \
             are firing. got: {line:?}"
        );
        assert!(line.contains("--legacy-only"), "got: {line:?}");
    }

    /// Branch 5 (Task 8): no own cache, no Claude coverage, no broken
    /// payload — just legacy entries. This was already definitive before
    /// Task 1 and should not change.
    #[test]
    fn plugin_status_line_cursor_no_cache_with_legacy_names_what_fires() {
        let line = super::cursor_status_line(&cursor_status(false, true, false, None));
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
