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
//! Codex is not in that table. Measured read-only on codex-cli 0.154.0
//! (2026-09-14): the binary carries `.codex-plugin/plugin.json` alongside
//! `.claude-plugin/plugin.json` and `.cursor-plugin/plugin.json`, so the
//! overlay descriptor directory is the right one, and `skills/` is its default
//! skill root. It also carries a `hooks/hooks.json` literal, so whether the
//! manifest's `hooks` key actually overrides that convention is **unverified**
//! — no install/import probe was run, and `codex plugin` exposes no `validate`.
//! If the convention wins, Codex would read Claude's `hooks/hooks.json` and get
//! Claude handlers; that is the `Incompatible` state Task 3 classifies, and
//! Task 6 is where import is exercised. The committed overlay payload is pinned
//! against `CODEX_HOOK_COMMANDS` by unit test only.
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
//! Cursor runs hook commands through a POSIX shell (measured 2026-09-03: a
//! `sessionStart` hook using `${HOME:-nohome}`, `||` and a redirect executed
//! correctly). That is what lets `hooks-cursor.json` carry the same
//! self-resolving `cmd=${HCOM:-hcom}; …` guard as Claude's manifest instead of
//! needing Antigravity's explicit `sh -c '…'` wrapper. Cursor's legacy commands
//! were bare `hcom cursor-stop` with no metacharacters, so nothing before this
//! measurement established it.
//!
//! Claude's install commands are idempotent: re-running `plugin marketplace add`
//! on a known marketplace prints "already on disk" and `plugin install` on an
//! installed plugin prints "is already installed", both exiting 0 (measured
//! 2026-09-03). That matters because re-running `hcom hooks add claude` is the
//! documented recovery after a failed install — a nonzero exit there would
//! abort at the `?` before verification ever ran.
//!
//! Cursor does resolve a plugin declared in a repo subdirectory
//! (`marketplace.json` → `"source": "./hcom"`), so the plugin body can stay
//! where it is.

use std::path::{Path, PathBuf};

/// Plugin name as every tool addresses it.
pub(crate) const PLUGIN_NAME: &str = "hcom";

/// Marketplace-qualified id, `<plugin>@<marketplace>`: what Claude records in
/// `enabledPlugins` and what both `claude plugin install` and `codex plugin add`
/// take as their selector.
///
/// Both halves are `hcom` because `.claude-plugin/marketplace.json` names the
/// marketplace `hcom` and the plugin inside it `hcom` — and Codex resolves the
/// same file: its binary carries `.claude-plugin/marketplace.json` and
/// `.cursor-plugin/marketplace.json` literals and **no** `.codex-plugin`
/// variant (measured, codex-cli 0.154.0). So there is nothing Codex-specific to
/// add for the marketplace descriptor, and the name is shared rather than
/// Claude's alone despite what this constant is called.
pub(crate) const CLAUDE_PLUGIN_ID: &str = "hcom@hcom";

/// Marketplace name alone, as it appears in `extraKnownMarketplaces`.
pub(crate) const CLAUDE_MARKETPLACE: &str = "hcom";

/// Directory Claude keeps its plugin registry under.
pub(crate) fn claude_plugins_root() -> PathBuf {
    crate::hooks::claude::get_claude_settings_path()
        .parent()
        .map(|d| d.join("plugins"))
        .unwrap_or_default()
}

/// Read a JSON file, `None` if missing or malformed.
fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
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

/// Canonical messaging-skill payload every staged plugin must carry.
///
/// Keep this explicit: checking only `SKILL.md` let an apparently installed
/// plugin fail as soon as the skill followed one of its bundled references.
const PLUGIN_SKILL_FILES: &[&str] = &[
    "SKILL.md",
    "references/cross-tool.md",
    "references/gotchas.md",
    "references/patterns.md",
    "references/script-template.md",
    "references/scripts/basic-messaging.sh",
    "references/scripts/cascade-pipeline.sh",
    "references/scripts/codex-worker.sh",
    "references/scripts/cross-tool-duo.sh",
    "references/scripts/ensemble-consensus.sh",
    "references/scripts/review-loop.sh",
    "references/setup-troubleshooting.md",
];

/// Verify a staged or installed plugin has the complete canonical skill and
/// that none of its required files resolves outside the artifact root.
///
/// This is deliberately separate from [`verify_agy_plugin_installed`], whose
/// compatibility contract is only "the AGY hook file is present".
pub(crate) fn verify_plugin_skill_payload(root: &Path) -> Result<(), String> {
    let artifact_root = root
        .canonicalize()
        .map_err(|e| format!("plugin artifact {} is not readable: {e}", root.display()))?;
    let skill_root = root.join("skills").join("hcom-agent-messaging");

    for relative in PLUGIN_SKILL_FILES {
        let path = skill_root.join(relative);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("missing skill payload {}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "skill payload {} must be a regular file inside the plugin artifact",
                path.display()
            ));
        }
        let resolved = path
            .canonicalize()
            .map_err(|e| format!("skill payload {} is not readable: {e}", path.display()))?;
        if !resolved.starts_with(&artifact_root) {
            return Err(format!(
                "skill payload {} resolves outside plugin artifact {}",
                path.display(),
                root.display()
            ));
        }
    }
    Ok(())
}

/// Antigravity's record of imported plugins.
pub(crate) fn agy_import_manifest() -> PathBuf {
    crate::runtime_env::gemini_family_config_dir()
        .join("config")
        .join("import_manifest.json")
}

/// Source harness hcom's Antigravity plugin was imported from, if any.
///
/// Measured 2026-09-08: `agy plugin install <dir>` DOES write here, recording
/// our plugin as `source: "claude-code"` because our manifest directory is
/// named `.claude-plugin/`. So an entry alone proves nothing about origin —
/// callers must check the installed manifest's shape; see [`agy_hook_state`],
/// which is the only caller that should reach for this label. Entries look like
/// `{"name": "hcom", "source": "claude-code", ...}`. Since Claude's
/// `hooks/hooks.json` sits at the exact path Antigravity reads (module doc),
/// an import lands Claude's handlers on Antigravity agents. Returns the
/// `source` string when such an entry exists, so status can name it.
pub(crate) fn agy_imported_hcom_source() -> Option<String> {
    let contents = std::fs::read_to_string(agy_import_manifest()).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&contents).ok()?;
    manifest
        .get("imports")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("name").and_then(|n| n.as_str()) == Some(PLUGIN_NAME))
        .and_then(|entry| entry.get("source").and_then(|s| s.as_str()))
        .map(str::to_string)
}

/// Which harness's hooks Antigravity is actually running.
///
/// Only meaningful once `verify_agy_plugin_installed()` is true — it is the
/// verifier that proves the file exists at all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AgyHooks {
    /// hcom's agy manifest: every event we install is present and carries entries.
    Hcom,
    /// Another harness's manifest: it carries that harness's events and none
    /// of ours. Only this state names a source, and only because the shape is
    /// the evidence — the import label alone is a manifest-format name.
    Foreign(String),
    /// Parsed, but neither ours nor recognisably another harness's: a partial
    /// or hand-edited install. Reported as broken, not blamed on anybody.
    Malformed,
    /// The manifest is on disk but cannot be read or parsed, so nothing can be
    /// claimed either way. Silence here would report a corrupt file as healthy.
    Unverifiable,
}

/// The handlers hcom's agy manifest installs, per event, that wake actually
/// depends on. `gemini-beforeagent` is the delivery hook and
/// `gemini-afteragent` the ready signal; `gemini-sessionstart` binds the
/// session they run in. A manifest missing any of them cannot wake an agent
/// however agy-shaped the rest of it looks — which is why the check is for
/// these subcommands and not for the events that contain them.
const AGY_REQUIRED_HANDLERS: [(&str, &[&str]); 2] = [
    (
        "PreInvocation",
        &["gemini-sessionstart", "gemini-beforeagent"],
    ),
    ("PostInvocation", &["gemini-afteragent"]),
];

/// The one event Claude declares that agy's manifest does not.
///
/// `PostToolUse` and `Stop` are in **both** manifests, so neither is evidence
/// of anything: a partial agy install keeping only `PostToolUse` would be
/// blamed on Claude. `SessionStart` is the discriminator, and it counts only
/// when it carries a real command entry — a bare key proves nothing either.
const CLAUDE_ONLY_EVENT: &str = "SessionStart";

/// Every command string an event's entries carry.
///
/// agy takes flat entries; a matcher-style entry nests them under `hooks`.
/// Entries without `type: "command"` are skipped — a malformed or non-command
/// entry runs nothing, so counting it would be the same false pass as
/// counting `[]`.
fn event_commands(events: &serde_json::Value, name: &str) -> Vec<String> {
    let Some(entries) = events.get(name).and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .flat_map(
            |entry| match entry.get("hooks").and_then(serde_json::Value::as_array) {
                Some(nested) => nested.iter().collect::<Vec<_>>(),
                None => vec![entry],
            },
        )
        .filter(|e| e.get("type").and_then(serde_json::Value::as_str) == Some("command"))
        .filter_map(|e| {
            e.get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// True when `haystack` carries `token` as a whole word.
///
/// A substring test is not enough: `gemini-beforeagent-disabled` contains
/// `gemini-beforeagent` and invokes a different subcommand entirely. This is
/// not a shell parser — it only checks that neither neighbouring character
/// could be part of the same token, which is what separates a subcommand from
/// a longer name built out of it. `-` and `_` count as token characters
/// because the subcommands themselves contain them.
fn contains_token(haystack: &str, token: &str) -> bool {
    let is_token_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    haystack.match_indices(token).any(|(idx, _)| {
        let before = haystack[..idx].chars().next_back();
        let after = haystack[idx + token.len()..].chars().next();
        before.is_none_or(|c| !is_token_char(c)) && after.is_none_or(|c| !is_token_char(c))
    })
}

/// True when any of `handlers` appears as a whole token in `event`'s commands.
fn event_runs_any(events: &serde_json::Value, event: &str, handlers: &[&str]) -> bool {
    let commands = event_commands(events, event);
    handlers
        .iter()
        .any(|handler| commands.iter().any(|cmd| contains_token(cmd, handler)))
}

/// True when every handler hcom installs for `event` is present in it.
///
/// Matched by subcommand token, not by full string: the commands are long
/// `sh -c` one-liners whose text varies with `$HCOM` resolution, but the
/// subcommand is the part that decides which handler runs, and it is stable.
fn event_has_handlers(events: &serde_json::Value, event: &str, handlers: &[&str]) -> bool {
    let commands = event_commands(events, event);
    handlers
        .iter()
        .all(|handler| commands.iter().any(|cmd| contains_token(cmd, handler)))
}

/// Read the installed manifest and say whose hooks agy will run.
///
/// `agy_imported_hcom_source` alone is not the answer: `agy plugin install`
/// records our own plugin as `source: "claude-code"`, because our manifest
/// directory follows Claude's `.claude-plugin/` convention. The label names
/// the manifest *format*, not the origin — so the hooks on disk are what gets
/// compared, and the label is quoted only once the shape already proves the
/// manifest is another harness's.
pub(crate) fn agy_hook_state() -> AgyHooks {
    let path = agy_plugin_dir().join(AGY_HOOKS_RELATIVE);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return AgyHooks::Unverifiable;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return AgyHooks::Unverifiable;
    };
    let events = manifest.get("hooks").unwrap_or(&manifest);
    if AGY_REQUIRED_HANDLERS
        .iter()
        .all(|(event, handlers)| event_has_handlers(events, event, handlers))
    {
        return AgyHooks::Hcom;
    }
    // Claude's own event, carrying a real command, and not one of our handlers
    // anywhere: the shape is the evidence, and only now is the import label
    // worth quoting — and even then only as the format hint it is.
    let claude_shaped = !event_commands(events, CLAUDE_ONLY_EVENT).is_empty();
    let any_of_ours = AGY_REQUIRED_HANDLERS
        .iter()
        .any(|(event, handlers)| event_runs_any(events, event, handlers));
    if claude_shaped && !any_of_ours {
        return AgyHooks::Foreign(
            agy_imported_hcom_source().unwrap_or_else(|| "unknown".to_string()),
        );
    }
    AgyHooks::Malformed
}

/// Cursor's config root, the parent of `hooks.json`. Base for everything
/// Cursor's plugin system writes under `plugins/` (cache, marketplaces).
fn cursor_plugins_root() -> PathBuf {
    crate::hooks::cursor::get_cursor_hooks_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Where Cursor materializes an installed plugin:
/// `plugins/cache/<marketplace>/<plugin>/<id>/`. Same shape as Claude's cache.
/// The final segment is a changing hash, so callers walk the subdirectories.
pub(crate) fn cursor_plugin_cache_dir() -> PathBuf {
    cursor_plugins_root()
        .join("plugins")
        .join("cache")
        .join(CLAUDE_MARKETPLACE)
        .join(PLUGIN_NAME)
}

/// True when Claude has the plugin enabled, still has its marketplace
/// registered, and has a real install on disk.
///
/// Measured 2026-09-16: Claude MARKS an orphaned cache
/// (`cache/hcom/hcom/<ver>/.orphaned_at`) instead of deleting it, so the old
/// question — "is `cache/hcom/hcom/` a directory" — stayed true long after a
/// marketplace was removed by hand. `installed_plugins.json` points at the
/// version actually in use, and `known_marketplaces.json` is the only thing
/// that witnesses a marketplace removal.
///
/// Reads files only — this runs before every agent spawn, so a subprocess here
/// would cost a process launch per agent.
pub(crate) fn verify_claude_plugin_installed() -> bool {
    let settings_path = crate::hooks::claude::get_claude_settings_path();
    let Some(settings) = crate::hooks::claude::load_claude_settings(&settings_path) else {
        return false;
    };
    let enabled = settings
        .get("enabledPlugins")
        .and_then(|p| p.get(CLAUDE_PLUGIN_ID))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return false;
    }

    let root = claude_plugins_root();

    let marketplace_known = read_json(&root.join("known_marketplaces.json"))
        .map(|v| v.get(CLAUDE_MARKETPLACE).is_some())
        .unwrap_or(false);
    if !marketplace_known {
        return false;
    }

    read_json(&root.join("installed_plugins.json"))
        .and_then(|v| {
            Some(
                v.get("plugins")?
                    .get(CLAUDE_PLUGIN_ID)?
                    .as_array()?
                    .iter()
                    .any(|entry| {
                        entry
                            .get("installPath")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|p| Path::new(p).is_dir())
                    }),
            )
        })
        .unwrap_or(false)
}

/// True once `import_manifest.json` records hcom's hooks component **and**
/// the hook file Antigravity actually reads is still on disk.
///
/// Measured 2026-09-17 (issue doc item 3): the premise this check used to
/// rest on — "AGY has no registry to check against" — was wrong. `agy
/// plugin list` reads exactly `import_manifest.json`, so the registry is
/// real, and it is a plain JSON file on disk: no subprocess is needed to
/// consult it, which matters because this runs before every agent spawn.
///
/// Neither half is sufficient alone. The hook file alone is what the old
/// check trusted, and it is exactly what an orphan copy of the plugin
/// directory carries: extracted by hand, left behind after `agy plugin
/// uninstall` cleared the manifest without deleting the files, or mid-import
/// before the manifest entry is written. The manifest entry alone would
/// accept an import that has not finished copying `hooks/hooks.json` yet.
/// Both together is what an install that is actually live looks like.
pub(crate) fn verify_agy_plugin_installed() -> bool {
    if !agy_plugin_dir().join(AGY_HOOKS_RELATIVE).is_file() {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(agy_import_manifest()) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };
    manifest
        .get("imports")
        .and_then(|v| v.as_array())
        .is_some_and(|imports| {
            imports.iter().any(|entry| {
                entry.get("name").and_then(|n| n.as_str()) == Some(PLUGIN_NAME)
                    && entry
                        .get("components")
                        .and_then(|c| c.as_array())
                        .is_some_and(|components| {
                            components.iter().any(|c| c.as_str() == Some("hooks"))
                        })
            })
        })
}

/// True when Cursor has materialized the hcom plugin into its cache.
///
/// Measured 2026-09-16: an installed plugin lands at
/// `~/.cursor/plugins/cache/hcom/hcom/<id>/`, carrying `.cache-complete` and
/// `hooks/hooks-cursor.json` — the same shape as Claude's cache.
///
/// Also requires [`verify_plugin_skill_payload`] on that cache entry (M6,
/// 2026-09-19): a real dev-host cache carried both marker files while
/// `skills` was a dangling symlink, so the two-file check alone reported an
/// install whose messaging skill could never actually load. Same three-part
/// AND shape AGY's install path already uses.
///
/// Deliberately NOT `plugins/marketplaces/<host>/<owner>/<repo>/<sha>/`: that
/// directory only proves `marketplace add` cloned some repo. The old verifier
/// scanned every repo there, so a leftover checkout of the old
/// `sirassss/hcom` repo made it return true even after the current
/// marketplace was removed (issue 2026-09-16, D1), and its hardcoded
/// `<sha>/plugin/hcom/...` path was already stale once the plugin moved to
/// its own repo with layout `<sha>/hcom/...` (D2).
///
/// Still does NOT prove the user finished enabling the plugin in
/// `/plugins`: whether this cache entry survives a disable is unmeasured.
/// Don't use it to unlock stripping legacy hooks (see the doc on
/// `install_cursor_plugin`).
///
/// Reads files only — this runs before every agent spawn.
pub(crate) fn verify_cursor_plugin_installed() -> bool {
    let Ok(entries) = std::fs::read_dir(cursor_plugin_cache_dir()) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let root = entry.path();
        root.join(".cache-complete").is_file()
            && root.join("hooks").join("hooks-cursor.json").is_file()
            && verify_plugin_skill_payload(&root).is_ok()
    })
}

/// Distinguishes "a Cursor cache entry exists but its skill payload is
/// broken" from "no cache entry at all" (Task 11) — a split
/// `verify_cursor_plugin_installed` cannot make itself, because Task 7 folded
/// the skill-payload check into that single boolean, so both states report
/// `false`. Status needs the split: a cache entry with the two marker files
/// present is evidence the hook may still be firing even though the skill
/// isn't, which is a materially different message from "nothing was ever
/// installed".
///
/// Reads files only, and only from status; not on the hot pre-spawn path.
pub(crate) fn cursor_cache_entry_missing_skill_payload() -> Option<String> {
    let entries = std::fs::read_dir(cursor_plugin_cache_dir()).ok()?;
    entries.flatten().find_map(|entry| {
        let root = entry.path();
        let has_markers = root.join(".cache-complete").is_file()
            && root.join("hooks").join("hooks-cursor.json").is_file();
        has_markers
            .then(|| verify_plugin_skill_payload(&root).err())
            .flatten()
    })
}

/// True when Cursor is already running the hcom hook, whether or not Cursor
/// itself has anything installed.
///
/// Measured 2026-09-19 (M1): a Cursor agent spawned with **zero** Cursor-side
/// artifacts (registry, cache, and all stale marketplace checkouts removed)
/// still reported `bindings: hooks, pty` and loaded the messaging skill out
/// of Claude's plugin cache — `cursor-agent` reads Claude's installed plugin
/// directly. So Cursor's hooks are covered whenever *either* vendor has a
/// verified install; see `cursor_and_claude_verifier_truth_table` for the
/// full row-by-row measurement this rests on.
///
/// This does **not** replace [`verify_cursor_plugin_installed`]. Uninstall
/// still needs that narrower meaning — "does Cursor have its own cache
/// entry" — to decide whether there is anything of Cursor's own left to
/// strip; borrowing Claude's install here would make `hooks remove cursor`
/// report a Cursor-owned install that was never there.
///
/// Reads files only — this runs before every agent spawn.
pub(crate) fn cursor_hooks_covered() -> bool {
    verify_claude_plugin_installed() || verify_cursor_plugin_installed()
}

/// Install → verify → strip, in that order.
///
/// `strip` runs only when `verify` returns true. A CLI that exits 0 without
/// actually installing must not cost the user their working legacy hooks, so
/// verification — not the exit code — is the gate.
pub(crate) fn install_then_strip<I, V, S>(install: I, verify: V, strip: S) -> Result<(), String>
where
    I: FnOnce() -> Result<(), String>,
    V: FnOnce() -> bool,
    S: FnOnce() -> bool,
{
    install()?;
    if !verify() {
        return Err("plugin install reported success but verification failed".to_string());
    }
    // A strip that fails leaves the legacy hooks next to the freshly installed
    // plugin — the double-fire state the design exists to avoid. It is not
    // dangerous (nothing is left without hooks), but reporting success here
    // would hide it, so surface it and let status tell the user what to do.
    if !strip() {
        return Err(
            "plugin installed, but the legacy hook entries could not be removed. \
             Both will fire until they are; see `hcom hooks status`."
                .to_string(),
        );
    }
    Ok(())
}

/// Run a tool CLI, returning its stderr on failure.
fn run_tool_cli(program: &str, args: &[&str]) -> Result<(), String> {
    let output = crate::terminal::executable_command(program)
        .args(args)
        .output()
        .map_err(|e| {
            format!("{program} not runnable: {e}. Install it or run the command by hand.")
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Build a self-contained plugin artifact without relying on the target CLI
/// to dereference the repository's canonical-skill link.
fn stage_plugin_artifact(
    source_root: &Path,
    adapter: &str,
) -> Result<(tempfile::TempDir, PathBuf), String> {
    let staging = tempfile::tempdir()
        .map_err(|e| format!("could not create plugin staging directory: {e}"))?;
    let destination = staging.path().join(adapter);
    super::plugin_stage::materialize_plugin_artifact(source_root, adapter, &destination)?;
    verify_plugin_skill_payload(&destination)
        .map_err(|e| format!("staged plugin payload is incomplete: {e}"))?;
    Ok((staging, destination))
}

fn install_agy_plugin_from_root<I, V, S>(
    root: &Path,
    install: I,
    verify: V,
    strip: S,
) -> Result<(), String>
where
    I: FnOnce(&Path) -> Result<(), String>,
    V: FnOnce() -> bool,
    S: FnOnce() -> bool,
{
    let (_staging, source) = stage_plugin_artifact(root, "hcom-agy")?;
    install_then_strip(|| install(&source), verify, strip)
}

/// A dedicated repository, kept in sync by `scripts/sync-plugin-skills.sh --publish`, so
/// no vendor's marketplace/install has to clone the whole monorepo (`src/`,
/// `tests/`, `docs/`...) just to reach `plugin/`. Its root is
/// `plugin/hcom-agy`'s content; a `hcom` subdirectory holds `plugin/hcom`'s
/// content, alongside its own `.claude-plugin/marketplace.json` declaring
/// `"source": "./hcom"`.
///
/// AGY specifically requires its package to sit at this URL's root: `agy
/// plugin install <target>` clones a URL's default branch and reads
/// skills/hooks there directly, with no ref or subdirectory selector
/// (measured 2026-09-15: `owner/repo@ref` is parsed as `@marketplace`, and a
/// URL `#ref` fragment is silently ignored; only `https://` is recognized as
/// a remote — an SSH `git@host:path` string is treated as an unknown
/// marketplace name). Claude/Codex/Cursor instead register this same URL as
/// a marketplace and read the `marketplace.json` indirection above — ordinary
/// marketplace behavior, no root placement needed.
pub(crate) const HCOM_PLUGIN_REPOSITORY_URL: &str = "https://github.com/sirassss/hcom-plugin";

pub(crate) fn install_claude_plugin() -> Result<(), String> {
    install_then_strip(
        || {
            run_tool_cli(
                "claude",
                &["plugin", "marketplace", "add", HCOM_PLUGIN_REPOSITORY_URL],
            )?;
            run_tool_cli("claude", &["plugin", "install", CLAUDE_PLUGIN_ID])
        },
        verify_claude_plugin_installed,
        crate::hooks::claude::remove_claude_hooks,
    )
}

/// Cursor: add the marketplace, then hand the user the one step hcom cannot do.
///
/// `cursor-agent plugin` exposes only `marketplace` — installation happens in
/// the interactive `/plugins` picker (measured, module doc). Cursor rejects
/// local paths, so `dev_root` cannot be passed directly; what it can index is
/// the published [`HCOM_PLUGIN_REPOSITORY_URL`].
///
/// **This never strips the legacy hooks, on any pass.** It is tempting to gate
/// a strip on [`verify_cursor_plugin_installed`], but that verifier only
/// proves the plugin materialized into Cursor's plugin cache
/// (`~/.cursor/plugins/cache/hcom/hcom/<id>/`, complete with
/// `.cache-complete` and `hooks/hooks-cursor.json`) — not that the user
/// finished enabling it in the `/plugins` TUI, and whether that cache entry
/// survives a disable is unmeasured. Gating on it risks deleting
/// `~/.cursor/hooks.json` (and hcom's Cursor permissions, which
/// `remove_cursor_hooks` also clears) while the plugin is disabled or the
/// cache entry is stale, leaving Cursor running neither plugin hooks nor
/// legacy hooks, silently.
///
/// No honest signal exists to gate on: Cursor's enabled marker is not
/// readable from disk (there is no non-interactive install to observe), and
/// a completed cache entry falls short of proving "enabled" for the reason
/// above. Leaving the legacy hooks in place is the safe half of the trade:
/// both sets call the same `cursor-*` subcommands, so the worst case is one
/// redundant hook invocation, never a wrong handler.
pub(crate) fn install_cursor_plugin() -> Result<(), String> {
    // Cursor takes a git URL only — a path or file:// URL is mangled into an
    // unresolvable https host (measured 2026-09-09).
    run_tool_cli(
        "cursor-agent",
        &["plugin", "marketplace", "add", HCOM_PLUGIN_REPOSITORY_URL],
    )?;

    Err(format!(
        "marketplace added. Finish inside Cursor: run /plugins and install \"{PLUGIN_NAME}\".\n\
         Your existing hooks in ~/.cursor/hooks.json are left in place and keep working;\n\
         remove them with `hcom hooks remove cursor` once the plugin is enabled."
    ))
}

/// Prefer the local checkout when one is configured, so editing
/// `plugin/hcom-agy` and reinstalling never waits on a publish. A host with no
/// checkout falls back to the published repository above instead of erroring.
pub(crate) fn install_agy_plugin() -> Result<(), String> {
    let db_path = crate::paths::db_path();
    match crate::router::resolve_effective_dev_root(&db_path) {
        Some((root, _source)) => install_agy_plugin_from_root(
            &root,
            |source| {
                let source = source.to_string_lossy();
                run_tool_cli("agy", &["plugin", "install", &source])
            },
            || {
                verify_agy_plugin_installed()
                    && verify_plugin_skill_payload(&agy_plugin_dir()).is_ok()
            },
            crate::hooks::antigravity::remove_antigravity_hooks,
        ),
        None => install_agy_plugin_from_url(
            HCOM_PLUGIN_REPOSITORY_URL,
            |url| run_tool_cli("agy", &["plugin", "install", url]),
            || {
                verify_agy_plugin_installed()
                    && verify_plugin_skill_payload(&agy_plugin_dir()).is_ok()
            },
            crate::hooks::antigravity::remove_antigravity_hooks,
        ),
    }
}

/// Same install → verify → strip shape as [`install_agy_plugin_from_root`],
/// for the no-local-checkout path: `install` gets the published URL directly
/// instead of a staged directory, so a test can assert which URL it saw
/// without `agy` (or the network) ever running.
fn install_agy_plugin_from_url<I, V, S>(
    url: &str,
    install: I,
    verify: V,
    strip: S,
) -> Result<(), String>
where
    I: FnOnce(&str) -> Result<(), String>,
    V: FnOnce() -> bool,
    S: FnOnce() -> bool,
{
    install_then_strip(|| install(url), verify, strip)
}

/// Commands hcom runs to remove the Claude plugin: uninstall it, then drop the
/// marketplace registration too — otherwise `claude plugin marketplace list`
/// keeps showing it after `hcom hooks remove claude`.
/// Codex: add the marketplace, then install the plugin from it.
///
/// `codex plugin marketplace add` takes "a local path, owner/repo[@ref], HTTPS
/// Git URL, or SSH Git URL" (0.154.0 `--help`), so it uses the same
/// [`HCOM_PLUGIN_REPOSITORY_URL`] Claude does.
///
/// Unlike Claude's install, **this never strips the legacy hook entries.**
/// Codex's hooks require an explicit trust step hcom cannot perform, so the
/// native entries are the only thing firing until the user reviews the plugin.
/// They go on an explicit `hcom hooks remove codex --legacy-only`.
pub(crate) fn install_codex_plugin() -> Result<(), String> {
    run_tool_cli(
        "codex",
        &["plugin", "marketplace", "add", HCOM_PLUGIN_REPOSITORY_URL],
    )?;
    run_tool_cli("codex", &["plugin", "add", CLAUDE_PLUGIN_ID])
}

/// Uninstall the Codex plugin. Deliberately does **not** touch Claude's plugin
/// or Claude's marketplace registration: one shared package, but each vendor
/// installs and removes its own copy.
///
/// No `codex` binary on PATH means no Codex plugin could exist to remove —
/// same "not installed, not unverified" read `codex_plugin_status_at` uses —
/// so this returns `Ok(())` instead of running the CLI into a "not runnable"
/// error that would otherwise fail `hooks remove` on any machine without Codex.
pub(crate) fn uninstall_codex_plugin() -> Result<(), String> {
    if crate::terminal::which_bin("codex").is_none() {
        return Ok(());
    }
    run_tool_cli("codex", &["plugin", "remove", CLAUDE_PLUGIN_ID])
}

fn claude_uninstall_commands() -> [(&'static str, Vec<&'static str>); 2] {
    [
        ("claude", vec!["plugin", "uninstall", CLAUDE_PLUGIN_ID]),
        (
            "claude",
            vec!["plugin", "marketplace", "remove", CLAUDE_MARKETPLACE],
        ),
    ]
}

/// Cursor's plugin registry is account state, not a local file — deleting
/// anything under `~/.cursor` cannot clear it, so removal must go through the
/// CLI (design doc, "Decisions": Uninstall).
fn cursor_uninstall_command() -> (&'static str, Vec<&'static str>) {
    (
        "cursor-agent",
        vec!["plugin", "marketplace", "remove", PLUGIN_NAME],
    )
}

fn agy_uninstall_command() -> (&'static str, Vec<&'static str>) {
    ("agy", vec!["plugin", "uninstall", PLUGIN_NAME])
}

/// True when *any* of the three vertices [`verify_claude_plugin_installed`]
/// ANDs together is present, instead of requiring all three.
///
/// Deliberately wider than the verifier: `hooks remove` must clean up
/// whatever is left, not just a fully-healthy install. A user who removed the
/// marketplace by hand (`known_marketplaces.json` loses `hcom`) while
/// `enabledPlugins` and the install-path cache survive would otherwise make
/// `uninstall_claude_plugin` a silent no-op — the exact bug this task exists
/// to close for Cursor, and Claude has the same three-vertex AND shape Task 5
/// just built, so it inherits the same gap. `installPath` is checked for
/// presence only, not directory existence: a dangling path is still a trace
/// worth telling `claude plugin uninstall` about.
fn claude_plugin_has_any_trace() -> bool {
    let settings_path = crate::hooks::claude::get_claude_settings_path();
    let enabled_entry_present = crate::hooks::claude::load_claude_settings(&settings_path)
        .and_then(|s| s.get("enabledPlugins")?.get(CLAUDE_PLUGIN_ID).cloned())
        .is_some();
    if enabled_entry_present {
        return true;
    }

    let root = claude_plugins_root();
    let marketplace_known = read_json(&root.join("known_marketplaces.json"))
        .map(|v| v.get(CLAUDE_MARKETPLACE).is_some())
        .unwrap_or(false);
    if marketplace_known {
        return true;
    }

    read_json(&root.join("installed_plugins.json"))
        .and_then(|v| {
            Some(
                !v.get("plugins")?
                    .get(CLAUDE_PLUGIN_ID)?
                    .as_array()?
                    .is_empty(),
            )
        })
        .unwrap_or(false)
}

/// Remove the Claude plugin. Gated on [`claude_plugin_has_any_trace`] — wider
/// than [`verify_claude_plugin_installed`] on purpose (see that function's
/// doc) — so a user who never touched Claude at all (the common case today)
/// does not see a CLI failure on every `hcom hooks remove claude`, while a
/// half-installed or half-removed state still gets cleaned up.
pub(crate) fn uninstall_claude_plugin() -> Result<(), String> {
    if !claude_plugin_has_any_trace() {
        return Ok(());
    }
    let mut errors = Vec::new();
    for (program, args) in claude_uninstall_commands() {
        if let Err(e) = run_tool_cli(program, &args) {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// `cursor-agent plugin marketplace list` stdout, or the reason it could not
/// be produced.
///
/// In tests, `HCOM_TEST_CURSOR_MARKETPLACE_LIST` substitutes for the real
/// CLI — same pattern as `HCOM_TEST_CODEX_HOOKS_LIST_JSON` in `codex.rs` —
/// so a test never depends on (or mutates) this host's actual Cursor account
/// state. Absent that override, test builds report an empty listing rather
/// than spawning the real CLI. Set the override to `"__fail__"` to simulate
/// a CLI failure.
fn cursor_marketplace_list_output() -> Result<String, String> {
    #[cfg(test)]
    {
        if let Ok(value) = std::env::var("HCOM_TEST_CURSOR_MARKETPLACE_LIST") {
            if value == "__fail__" {
                return Err("test marketplace list failure".to_string());
            }
            return Ok(value);
        }
        Ok(String::new())
    }

    #[cfg(not(test))]
    {
        let output = crate::terminal::executable_command("cursor-agent")
            .args(["plugin", "marketplace", "list"])
            .output()
            .map_err(|e| {
                format!("cursor-agent not runnable: {e}. Install it or run the command by hand.")
            })?;
        if !output.status.success() {
            return Err(format!(
                "cursor-agent plugin marketplace list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// True when Cursor's marketplace *registry* — not the local disk — actually
/// lists hcom.
///
/// Measured 2026-09-19 against a real `cursor-agent` 2026.09.18-9a7762b with
/// hcom actually registered. `cursor-agent plugin marketplace list` prints a
/// whitespace-column table, one marketplace per line — name, scope
/// (`global`/`user`), and a URL column that is only present for marketplaces
/// added by URL (a built-in `global` entry like `cursor-public` prints with
/// no URL column at all):
/// ```text
/// cursor-public     global
/// hcom              user    https://github.com/sirassss/hcom-plugin
/// i-have-adhd       user    https://github.com/ayghri/i-have-adhd
/// ```
/// So a URL-only match (the original, unmeasured version of this function)
/// works for hcom today, but is one Cursor formatting change away from going
/// permanently blind if the URL column is ever dropped — matches on EITHER
/// signal: [`HCOM_PLUGIN_REPOSITORY_URL`] with its scheme stripped
/// (`github.com/sirassss/hcom-plugin`) as a substring, robust to `http` vs
/// `https`, a trailing `.git`, or trailing whitespace; OR the literal
/// marketplace name ([`PLUGIN_NAME`], `"hcom"`) as an exact first column
/// token on some line — a whole-word match on the name column, not a
/// substring of the whole table, so it can't false-positive on some other
/// marketplace whose name or URL merely contains "hcom".
///
/// Fails open — returns `true` — when the CLI errors or `cursor-agent` is
/// not installed: that is missing evidence, not evidence of absence, so
/// [`uninstall_cursor_plugin`] should still try. This keeps the "still gỡ
/// nấy" (still attempt to clean up whatever's there) spirit of
/// `cursor_marketplace_checkout_exists`, the on-disk check this replaces,
/// without inheriting its actual bug: `cursor-agent plugin marketplace
/// remove` leaves that on-disk checkout completely untouched even after it
/// succeeds (measured, plan M3), so the old check stayed stuck at `true`
/// forever and made `uninstall_cursor_plugin` fail with "No marketplace
/// matches" in an infinite loop (measured, plan M4). The registry list
/// reflects the CLI's own removal, so it does not share that failure mode.
pub(crate) fn cursor_registry_lists_hcom() -> bool {
    let marker = HCOM_PLUGIN_REPOSITORY_URL
        .split("://")
        .nth(1)
        .unwrap_or(HCOM_PLUGIN_REPOSITORY_URL);
    match cursor_marketplace_list_output() {
        Ok(stdout) => {
            stdout.contains(marker)
                || stdout
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(PLUGIN_NAME))
        }
        Err(_) => true,
    }
}

/// Whether [`uninstall_cursor_plugin`] should attempt the removal CLI at all.
///
/// Reads the registry alone, not `verify_cursor_plugin_installed() || ...`
/// — the command this gates (`cursor-agent plugin marketplace remove`) only
/// ever succeeds or fails based on the marketplace *registry*, so gating it
/// on cache completeness too reintroduces the exact bug this function was
/// built to close, just through a different on-disk artifact. Measured
/// 2026-09-20: `cursor-agent plugin marketplace remove hcom` (real host)
/// leaves the materialized plugin cache (`.cache-complete`, `hooks/`,
/// `skills/`) completely untouched, exactly like the marketplace checkout
/// directory M3 measured. An OR with `verify_cursor_plugin_installed()`
/// made that surviving cache re-trigger the removal CLI on every single
/// `hooks remove cursor` forever, each attempt printing the same "No
/// marketplace matches" note — the M4 infinite loop, unblocked by cache
/// instead of by checkout. `cursor_registry_lists_hcom` already fails open
/// (`true`) when the CLI errors or is missing, so "still gỡ nấy" survives
/// without the cache OR.
fn cursor_uninstall_should_attempt() -> bool {
    cursor_registry_lists_hcom()
}

/// Remove Cursor's marketplace registration. Gated on
/// [`cursor_uninstall_should_attempt`], not the strict verifier — see its doc.
pub(crate) fn uninstall_cursor_plugin() -> Result<(), String> {
    if !cursor_uninstall_should_attempt() {
        return Ok(());
    }
    let (program, args) = cursor_uninstall_command();
    run_tool_cli(program, &args)
}

/// Remove the Antigravity plugin. Gated on [`verify_agy_plugin_installed`]
/// directly, unlike Claude and Cursor above — deliberately not widened.
///
/// Task 6 made [`verify_agy_plugin_installed`] itself a multi-condition AND
/// (the hook file on disk AND an `import_manifest.json` entry naming hcom's
/// `hooks` component), the same shape that made Claude's and Cursor's strict
/// verifiers too narrow for removal — so widening this gate the same way
/// Cursor's was widened (`cursor_uninstall_should_attempt`) would make sense
/// in principle. It stays narrow anyway: an orphan hook file with no manifest
/// entry (e.g. left behind by hand-editing, or a half-finished install) now
/// makes the verifier — and this gate — read `false`, so `hcom hooks remove
/// antigravity` no longer cleans it up. That is an accepted, deliberate
/// narrowing, not a gap to fix: Task 5's `add` escape hatch already handles
/// reinstalling over such an orphan (`add_antigravity_reinstalls_over_orphan_dir`),
/// and a real `agy plugin uninstall` would itself fail against a manifest
/// with no matching entry, so there is nothing this gate could remove that
/// the underlying CLI would accept anyway.
pub(crate) fn uninstall_agy_plugin() -> Result<(), String> {
    if !verify_agy_plugin_installed() {
        return Ok(());
    }
    let (program, args) = agy_uninstall_command();
    run_tool_cli(program, &args)
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
