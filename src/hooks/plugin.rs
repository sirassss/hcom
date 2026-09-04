//! Plugin-based hook installation for tools whose config files are shared
//! across harnesses (Claude Code, Cursor, Antigravity).
//!
//! Writing hooks into `~/.claude/settings.json` leaks them: Cursor reads that
//! file too, so one Cursor agent ran both hook sets and two sessionEnd handlers
//! raced — the Cursor one logged `cursor.sessionend.ignored` while the Claude
//! one called `finalize_session` and deleted the live instance. Plugin hooks are
//! scoped to the harness that enabled them.
//!
//! # Measured behavior (2026-09-03, probe plugin installed and removed)
//!
//! | | Claude Code | Cursor | Antigravity |
//! |---|---|---|---|
//! | Install from local path | yes | **no** — git URL only | yes |
//! | Non-interactive install | yes | **no** — `/plugins` in the TUI | yes |
//! | Hook file read | `hooks/hooks.json` | declared `hooks` key | **`hooks/hooks.json`** |
//! | Descriptor read | `.claude-plugin/plugin.json` | `.cursor-plugin/plugin.json` | `.claude-plugin/plugin.json` |
//! | Enabled marker | `enabledPlugins` in settings.json | not measured | `import_manifest.json` |
//!
//! Three consequences the design has to absorb:
//!
//! 1. **Antigravity reads the same `hooks/hooks.json` Claude does**, ignores a
//!    `hooks` key in `gemini-extension.json`, and does not even require that
//!    file — it reads `.claude-plugin/plugin.json`. One plugin directory
//!    therefore cannot carry different hooks for Claude and Antigravity; they
//!    need separate directories.
//! 2. **Cursor cannot install a plugin from the CLI.** `cursor-agent plugin`
//!    exposes only `marketplace`; installing is `/plugins` inside the TUI.
//! 3. **Cursor marketplaces must be remote git URLs.** A local path is coerced
//!    into `https://<first path segment>.git` and fails DNS, so `dev_root`
//!    cannot drive a Cursor install; `cursor-agent --plugin-dir <path>` is the
//!    local-development route instead.
//!
//! Cursor does resolve a plugin declared in a repo subdirectory
//! (`marketplace.json` → `"source": "./plugin/hcom"`), so the plugin body can
//! stay where it is.

use std::path::PathBuf;

/// Plugin name as every tool addresses it.
pub(crate) const PLUGIN_NAME: &str = "hcom";

/// Marketplace-qualified id in Claude's `enabledPlugins`: `<plugin>@<marketplace>`.
/// Both halves are `hcom` because `.claude-plugin/marketplace.json` names the
/// marketplace `hcom` and the plugin inside it `hcom`.
pub(crate) const CLAUDE_PLUGIN_ID: &str = "hcom@hcom";

/// Marketplace name alone, as it appears in `extraKnownMarketplaces`.
pub(crate) const CLAUDE_MARKETPLACE: &str = "hcom";

/// Where Claude caches an installed plugin: `<marketplace>/<plugin>/<version>/`.
/// The version segment varies, so callers check the parent for any child.
pub(crate) fn claude_plugin_dir() -> PathBuf {
    crate::hooks::claude::get_claude_settings_path()
        .parent()
        .map(|d| {
            d.join("plugins")
                .join("cache")
                .join(CLAUDE_MARKETPLACE)
                .join(PLUGIN_NAME)
        })
        .unwrap_or_default()
}

/// Directory Antigravity copies an installed plugin into.
pub(crate) fn agy_plugin_dir() -> PathBuf {
    crate::runtime_env::gemini_family_config_dir()
        .join("config")
        .join("plugins")
        .join(PLUGIN_NAME)
}

/// File Antigravity reads hooks from, relative to the installed plugin dir.
///
/// Measured: only `hooks/hooks.json` is picked up. A `hooks.json` at the plugin
/// root reports `hooks: skipped (not found)`, and a `hooks` key in
/// `gemini-extension.json` is ignored.
pub(crate) const AGY_HOOKS_RELATIVE: &str = "hooks/hooks.json";

/// Antigravity's record of imported plugins.
pub(crate) fn agy_import_manifest() -> PathBuf {
    crate::runtime_env::gemini_family_config_dir()
        .join("config")
        .join("import_manifest.json")
}

/// Root under which Cursor checks out marketplace repositories:
/// `marketplaces/<host>/<owner>/<repo>/<commit sha>/`.
pub(crate) fn cursor_marketplaces_dir() -> PathBuf {
    crate::hooks::cursor::get_cursor_hooks_path()
        .parent()
        .map(|d| d.join("plugins").join("marketplaces"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    const CLAUDE_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks.json");

    #[test]
    fn claude_manifest_covers_every_configured_event() {
        let root: Value = serde_json::from_str(CLAUDE_MANIFEST).unwrap();
        let hooks = root["hooks"].as_object().expect("hooks object");

        for (event, matcher, suffix, timeout) in crate::hooks::claude::CLAUDE_HOOK_CONFIGS {
            let entries = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event {event}"));

            // Compare against what production writes into settings.json rather
            // than a substring: byte equality also catches a mangled
            // `command -v` guard, and needs no reasoning about suffixes that
            // prefix each other (`post` vs `post-failure`).
            let expected_command = crate::hooks::claude::build_hook_entry_command(suffix);
            let group = entries
                .iter()
                .find(|g| {
                    g["hooks"].as_array().is_some_and(|inner| {
                        inner
                            .iter()
                            .any(|h| h["command"].as_str() == Some(expected_command.as_str()))
                    })
                })
                .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

            if matcher.is_empty() {
                assert!(
                    group.get("matcher").is_none(),
                    "{event} should have no matcher"
                );
            } else {
                assert_eq!(group["matcher"], *matcher, "{event} matcher");
            }

            // A dropped timeout is silent breakage: the legacy verifier treats
            // it as fatal (VerifyFailReason::HookTimeoutMissing), and Stop /
            // PostToolUse / SubagentStop rely on the long value to poll.
            assert_eq!(
                group["hooks"][0].get("timeout").and_then(Value::as_u64),
                *timeout,
                "{event} timeout"
            );
        }

        // Table -> JSON above; JSON -> table here, so a stray event cannot ride
        // along unnoticed.
        assert_eq!(
            hooks.len(),
            crate::hooks::claude::CLAUDE_HOOK_CONFIGS.len(),
            "manifest has events the table does not: {:?}",
            hooks.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn claude_manifest_commands_fail_open() {
        let root: Value = serde_json::from_str(CLAUDE_MANIFEST).unwrap();
        for (_event, entries) in root["hooks"].as_object().unwrap() {
            for group in entries.as_array().unwrap() {
                for hook in group["hooks"].as_array().unwrap() {
                    let cmd = hook["command"].as_str().unwrap();
                    assert!(
                        cmd.contains("command -v") && cmd.contains("|| exit 0"),
                        "command must exit 0 when hcom is absent: {cmd}"
                    );
                    assert_eq!(hook["type"], "command");
                }
            }
        }
    }

    const CURSOR_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks-cursor.json");
    const CURSOR_DESCRIPTOR: &str = include_str!("../../plugin/hcom/.cursor-plugin/plugin.json");

    #[test]
    fn cursor_manifest_covers_every_configured_event() {
        let root: Value = serde_json::from_str(CURSOR_MANIFEST).unwrap();
        assert_eq!(root["version"], 1, "Cursor requires a top-level version");
        let hooks = root["hooks"].as_object().expect("hooks object");

        for (event, suffix) in crate::hooks::cursor::CURSOR_HOOK_COMMANDS {
            let entries = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event {event}"));

            // Cursor's manifest must not use build_cursor_hook_command: that
            // function embeds get_hcom_prefix(), resolved at install time, and
            // a committed manifest is a static file. Task 2 solved the same
            // problem for Claude with a self-resolving builder; reuse it here
            // rather than duplicating it under a Cursor-specific name.
            // Find the hcom entry first, then assert every field on that one
            // entry. Scanning the array separately per field would let a
            // foreign entry supply a correct timeout while ours carries a
            // wrong one.
            let expected_command = crate::hooks::claude::build_hook_entry_command(suffix);
            let entry = entries
                .iter()
                .find(|h| h["command"].as_str() == Some(expected_command.as_str()))
                .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

            let expected_timeout = if *event == "stop" {
                crate::hooks::cursor::STOP_HOOK_TIMEOUT_SECS
            } else {
                crate::hooks::cursor::HOOK_TIMEOUT_SECS
            };
            assert_eq!(
                entry["timeout"].as_u64(),
                Some(expected_timeout),
                "{event} timeout"
            );

            // Cursor caps a stop hook's follow-up loop unless the entry opts
            // out with an explicit null, so the field must be present, not
            // merely absent-and-defaulted. `entry["loop_limit"].is_null()`
            // would not say that: serde_json's Index yields Null for a missing
            // key, so it passes either way. `get` distinguishes them — the
            // same idiom `verify_hooks_at` uses in src/hooks/cursor.rs.
            //
            // Note this manifest is not verifiable by `verify_hooks_at`: that
            // function compares commands against `build_cursor_hook_command`
            // ("hcom cursor-stop"), while these carry the self-resolving guard.
            // The plugin path verifies by file presence instead.
            if *event == "stop" {
                assert!(
                    entry.get("loop_limit").is_some_and(Value::is_null),
                    "stop must carry an explicit loop_limit: null"
                );
            }
        }

        assert_eq!(
            hooks.len(),
            crate::hooks::cursor::CURSOR_HOOK_COMMANDS.len(),
            "manifest has events the table does not: {:?}",
            hooks.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn cursor_descriptor_points_at_its_own_hook_file() {
        let d: Value = serde_json::from_str(CURSOR_DESCRIPTOR).unwrap();
        assert_eq!(d["name"], super::PLUGIN_NAME);
        assert_eq!(d["hooks"], "./hooks/hooks-cursor.json");
        // Cursor, unlike Claude, finds nothing by convention — an undeclared
        // component is simply absent. Dropping this key would install hcom into
        // Cursor without the skill that teaches an agent to use it.
        assert_eq!(d["skills"], "./skills/");
    }

    /// `plugin/hcom/skills` is a symlink to the repo-root `skills/`, so the
    /// declared path resolves. Checked here because a broken symlink turns the
    /// descriptor above into a promise the package cannot keep, and git records
    /// symlinks as ordinary blobs that are easy to clobber.
    #[test]
    fn cursor_declared_skills_path_resolves() {
        let skills = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugin")
            .join("hcom")
            .join("skills");
        assert!(skills.is_dir(), "{} is not a directory", skills.display());
        assert!(
            skills.join("hcom-agent-messaging").is_dir(),
            "hcom-agent-messaging missing under {}",
            skills.display()
        );
    }

    const AGY_MANIFEST: &str = include_str!("../../plugin/hcom-agy/hooks/hooks.json");
    const AGY_DESCRIPTOR: &str = include_str!("../../plugin/hcom-agy/.claude-plugin/plugin.json");

    /// Find the entry named `name` inside an event's array, regardless of
    /// whether the event nests under `hooks: [...]` (PreToolUse/PostToolUse,
    /// which also carry a matcher on the outer group) or carries `name`/
    /// `command` directly on the array element (the three lifecycle events,
    /// where PreInvocation holds two such elements side by side). Returns the
    /// outer group (for matcher) alongside the resolved hook object (for
    /// everything else) — for a flat entry these are the same value.
    fn agy_group_and_hook<'a>(root: &'a Value, event: &str, name: &str) -> (&'a Value, &'a Value) {
        let entries = root["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("missing event {event}"));
        for group in entries {
            if let Some(inner) = group["hooks"].as_array() {
                if let Some(hook) = inner.iter().find(|h| h["name"] == name) {
                    return (group, hook);
                }
            } else if group["name"] == name {
                return (group, group);
            }
        }
        panic!("no entry named {name} under event {event}");
    }

    /// Drives off `AGY_HOOK_CONFIGS` — the same table `try_setup_antigravity_hooks`
    /// builds its `json!` from — instead of a hand-copied local table, so a
    /// writer-side change to a timeout, subcommand, matcher, description or
    /// fallback fails this test instead of silently diverging from the
    /// committed manifest.
    #[test]
    fn agy_manifest_matches_live_installer_exactly() {
        let root: Value = serde_json::from_str(AGY_MANIFEST).unwrap();

        for &(event, name, suffix, matcher, fallback, description) in
            crate::hooks::antigravity::AGY_HOOK_CONFIGS
        {
            let on_missing = if fallback.is_empty() {
                "exit 0".to_string()
            } else {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(fallback.as_bytes());
                format!("{{ printf %s {b64} | base64 -d; exit 0; }}")
            };
            let guard = crate::hooks::claude::build_hook_entry_command_with(
                suffix,
                "ANTIGRAVITY_AGENT=1 ",
                &on_missing,
            );
            let expected_command = format!("sh -c '{guard}'");

            let (group, hook) = agy_group_and_hook(&root, event, name);

            // Built from the shared builder, not reconstructed independently:
            // a changed subcommand or fallback in AGY_HOOK_CONFIGS changes
            // expected_command too, so it can't drift from the manifest
            // without this assertion catching it.
            assert_eq!(
                hook["command"].as_str(),
                Some(expected_command.as_str()),
                "{event}/{name} command"
            );
            assert_eq!(hook["name"].as_str(), Some(name), "{event}/{name} name");
            assert_eq!(
                hook["type"].as_str(),
                Some("command"),
                "{event}/{name} type"
            );
            assert_eq!(
                hook.get("timeout").and_then(Value::as_u64),
                Some(crate::hooks::antigravity::HOOK_TIMEOUT_SEC),
                "{event}/{name} timeout"
            );
            assert_eq!(
                hook["description"].as_str(),
                Some(description),
                "{event}/{name} description"
            );

            if matcher.is_empty() {
                assert!(
                    group.get("matcher").is_none(),
                    "{event}/{name} should have no matcher"
                );
            } else {
                assert_eq!(
                    group.get("matcher").and_then(Value::as_str),
                    Some(matcher),
                    "{event}/{name} matcher"
                );
            }
        }

        // Manifest's event set must equal the table's — a stray top-level
        // event (e.g. an extra "SessionStart": []) contributes zero entries
        // to the total-count check below, so it needs its own assertion.
        let hooks = root["hooks"].as_object().unwrap();
        let manifest_events: std::collections::HashSet<&str> =
            hooks.keys().map(String::as_str).collect();
        let table_events: std::collections::HashSet<&str> =
            crate::hooks::antigravity::AGY_HOOK_CONFIGS
                .iter()
                .map(|row| row.0)
                .collect();
        assert_eq!(manifest_events, table_events, "manifest events vs table");

        // Every entry the table declares — and no more. Counts both the outer
        // arrays (lifecycle events) and the inner `hooks` arrays nested under
        // a matcher (PreToolUse/PostToolUse), so an entry smuggled into either
        // shape is caught, not just a stray top-level event.
        let total_entries: usize = hooks
            .values()
            .map(|entries| {
                entries
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|group| match group.get("hooks").and_then(Value::as_array) {
                        Some(inner) => inner.len(),
                        None => 1,
                    })
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(
            total_entries,
            crate::hooks::antigravity::AGY_HOOK_CONFIGS.len(),
            "manifest has hook entries the table does not account for"
        );
    }

    /// Whole-file sweep, mirroring `claude_manifest_commands_fail_open`: every
    /// command in the AGY manifest — not just the six the table goes looking
    /// for — must carry the ANTIGRAVITY_AGENT=1 marker that routes it away
    /// from Claude's handler.
    #[test]
    fn agy_manifest_commands_carry_antigravity_marker() {
        let root: Value = serde_json::from_str(AGY_MANIFEST).unwrap();
        for (event, entries) in root["hooks"].as_object().unwrap() {
            for group in entries.as_array().unwrap() {
                let hooks: Vec<&Value> = match group.get("hooks").and_then(Value::as_array) {
                    Some(inner) => inner.iter().collect(),
                    None => vec![group],
                };
                for hook in hooks {
                    let cmd = hook["command"].as_str().unwrap();
                    assert!(
                        cmd.contains("ANTIGRAVITY_AGENT=1"),
                        "{event}: command missing ANTIGRAVITY_AGENT=1 marker: {cmd}"
                    );
                    assert_eq!(hook["type"], "command");
                }
            }
        }
    }

    #[test]
    fn agy_descriptor_declares_the_plugin_name() {
        let d: Value = serde_json::from_str(AGY_DESCRIPTOR).unwrap();
        assert_eq!(d["name"], super::PLUGIN_NAME);
        assert!(d["version"].is_string());
    }

    /// The whole reason for a second plugin directory: Antigravity and Claude
    /// read the same conventional path, so the two files at that path must not
    /// be the same file's content.
    #[test]
    fn agy_and_claude_conventional_manifests_are_disjoint() {
        assert_ne!(
            AGY_MANIFEST, CLAUDE_MANIFEST,
            "hooks/hooks.json must differ between plugin/hcom and plugin/hcom-agy"
        );
        let claude_events: std::collections::HashSet<String> =
            serde_json::from_str::<Value>(CLAUDE_MANIFEST).unwrap()["hooks"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
        let agy_events: std::collections::HashSet<String> =
            serde_json::from_str::<Value>(AGY_MANIFEST).unwrap()["hooks"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
        assert!(
            agy_events.contains("PreInvocation"),
            "AGY manifest lost its own event vocabulary: {agy_events:?}"
        );
        assert!(
            !claude_events.contains("PreInvocation"),
            "Claude manifest must not carry AGY events: {claude_events:?}"
        );

        // The name-check above only ever probed one event in each direction.
        // PreToolUse/PostToolUse/Stop are genuinely shared vocabulary — AGY
        // borrows Claude's event names by design — so a literal empty-
        // intersection assertion is not achievable here (verified: the real
        // intersection is {PostToolUse, PreToolUse, Stop}). What must not
        // happen is a Claude-only event leaking into AGY's file (or vice
        // versa): compare the file-level overlap against the overlap the two
        // production tables themselves declare, so any *extra* shared name
        // — one the tables don't already share — fails.
        let agy_table_events: std::collections::HashSet<&str> =
            crate::hooks::antigravity::AGY_HOOK_CONFIGS
                .iter()
                .map(|row| row.0)
                .collect();
        let claude_table_events: std::collections::HashSet<&str> =
            crate::hooks::claude::CLAUDE_HOOK_CONFIGS
                .iter()
                .map(|row| row.0)
                .collect();
        let shared_in_files: std::collections::HashSet<&str> = agy_events
            .intersection(&claude_events)
            .map(String::as_str)
            .collect();
        let shared_in_tables: std::collections::HashSet<&str> = agy_table_events
            .intersection(&claude_table_events)
            .copied()
            .collect();
        assert_eq!(
            shared_in_files, shared_in_tables,
            "manifests share event names neither production table shares"
        );
    }

    /// Neither manifest may invoke the other tool's subcommands. Driven off the
    /// real tables rather than a hand-picked few: the earlier three-name list
    /// let `notify`, `pre`, `subagent-stop` and seven others through, and
    /// cross-fire between these two tools is the entire reason this plugin
    /// exists. The `exec $cmd ` prefix anchors each needle, so `pre` does not
    /// match `cursor-pretooluse`.
    #[test]
    fn manifests_never_call_the_other_tools_subcommands() {
        let cursor_text = CURSOR_MANIFEST.to_string();
        for (_event, _matcher, suffix, _timeout) in crate::hooks::claude::CLAUDE_HOOK_CONFIGS {
            assert!(
                !cursor_text.contains(&format!("exec $cmd {suffix} ")),
                "Cursor manifest calls Claude subcommand {suffix}"
            );
        }

        let claude_text = CLAUDE_MANIFEST.to_string();
        for (_event, suffix) in crate::hooks::cursor::CURSOR_HOOK_COMMANDS {
            assert!(
                !claude_text.contains(&format!("exec $cmd {suffix} ")),
                "Claude manifest calls Cursor subcommand {suffix}"
            );
        }

        // AGY is the sharpest case: it reads the same conventional
        // `hooks/hooks.json` path Claude does, so this is the exact
        // cross-fire the split directory exists to prevent.
        let agy_text = AGY_MANIFEST.to_string();
        for (_event, _matcher, suffix, _timeout) in crate::hooks::claude::CLAUDE_HOOK_CONFIGS {
            assert!(
                !agy_text.contains(&format!("exec $cmd {suffix} ")),
                "AGY manifest calls Claude subcommand {suffix}"
            );
        }
        for (_event, suffix) in crate::hooks::cursor::CURSOR_HOOK_COMMANDS {
            assert!(
                !agy_text.contains(&format!("exec $cmd {suffix} ")),
                "AGY manifest calls Cursor subcommand {suffix}"
            );
        }
    }
}
