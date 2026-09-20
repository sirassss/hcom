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
#[path = "hooks_tests.rs"]
mod tests;
