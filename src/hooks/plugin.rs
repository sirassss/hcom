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
//! (`marketplace.json` → `"source": "./plugin/hcom"`), so the plugin body can
//! stay where it is.

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

/// Root under which Cursor checks out marketplace repositories:
/// `marketplaces/<host>/<owner>/<repo>/<commit sha>/`.
pub(crate) fn cursor_marketplaces_dir() -> PathBuf {
    crate::hooks::cursor::get_cursor_hooks_path()
        .parent()
        .map(|d| d.join("plugins").join("marketplaces"))
        .unwrap_or_default()
}

/// True when the tool has the plugin on disk *and* records it as enabled.
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
    claude_plugin_dir().is_dir()
}

/// True once Antigravity's copy of the plugin carries the hook file it reads.
/// The directory alone can exist mid-import or from a stale copy; only the
/// hook file proves hooks are live.
pub(crate) fn verify_agy_plugin_installed() -> bool {
    agy_plugin_dir().join(AGY_HOOKS_RELATIVE).is_file()
}

/// Cursor checks a marketplace out under
/// `plugins/marketplaces/<host>/<owner>/<repo>/<commit sha>/`, so the plugin
/// body sits at `<sha>/plugin/hcom/`. Any checkout carrying our Cursor hook
/// file counts.
///
/// This proves the marketplace checkout is present on disk — it does NOT
/// prove the user finished enabling the plugin in Cursor's `/plugins` TUI.
/// Cursor's enabled marker could not be measured (see the module doc): there
/// is no non-interactive install to probe. Callers relying on this as an
/// "installed" signal are relying on the weaker of the two guarantees.
pub(crate) fn verify_cursor_plugin_installed() -> bool {
    let Ok(hosts) = std::fs::read_dir(cursor_marketplaces_dir()) else {
        return false;
    };
    hosts
        .flatten()
        .flat_map(|host| {
            std::fs::read_dir(host.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .flat_map(|owner| {
            std::fs::read_dir(owner.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .flat_map(|repo| {
            std::fs::read_dir(repo.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .any(|sha| {
            sha.path()
                .join("plugin")
                .join(PLUGIN_NAME)
                .join("hooks")
                .join("hooks-cursor.json")
                .is_file()
        })
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
    let output = std::process::Command::new(program)
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

/// A dedicated repository, kept in sync by `scripts/sync-plugin-repo.sh`, so
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
/// a strip on [`verify_cursor_plugin_installed`], but that verifier only proves
/// a marketplace checkout exists on disk — and `marketplace add`, the command
/// immediately above, is what creates that checkout. Gating on it would delete
/// `~/.cursor/hooks.json` (and hcom's Cursor permissions, which
/// `remove_cursor_hooks` also clears) the instant the marketplace was added,
/// while the plugin sits un-enabled in the TUI. Cursor would then be running
/// neither plugin hooks nor legacy hooks, silently.
///
/// Cursor's enabled marker is not readable from disk — Task 1 measured it as
/// unavailable, since there is no non-interactive install to observe — so no
/// honest signal exists to gate on. Leaving the legacy hooks in place is the
/// safe half of the trade: both sets call the same `cursor-*` subcommands, so
/// the worst case is one redundant hook invocation, never a wrong handler.
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
pub(crate) fn uninstall_codex_plugin() -> Result<(), String> {
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

/// Remove the Claude plugin. Gated on [`verify_claude_plugin_installed`] so a
/// user who never installed it (the common case today) does not see a CLI
/// failure on every `hcom hooks remove claude`.
pub(crate) fn uninstall_claude_plugin() -> Result<(), String> {
    if !verify_claude_plugin_installed() {
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

/// Remove Cursor's marketplace registration. Gated on
/// [`verify_cursor_plugin_installed`] for the same reason as Claude above.
pub(crate) fn uninstall_cursor_plugin() -> Result<(), String> {
    if !verify_cursor_plugin_installed() {
        return Ok(());
    }
    let (program, args) = cursor_uninstall_command();
    run_tool_cli(program, &args)
}

/// Remove the Antigravity plugin. Gated on [`verify_agy_plugin_installed`] for
/// the same reason as Claude above.
pub(crate) fn uninstall_agy_plugin() -> Result<(), String> {
    if !verify_agy_plugin_installed() {
        return Ok(());
    }
    let (program, args) = agy_uninstall_command();
    run_tool_cli(program, &args)
}

#[cfg(test)]
mod tests {
    use crate::hooks::test_helpers::EnvGuard;
    use serde_json::Value;
    use serial_test::serial;
    use std::path::PathBuf;

    /// Isolated env for verify tests: a single `home` dir so `HCOM_DIR`'s
    /// parent (what `claude_config_dir`/`tool_config_root` resolve against)
    /// is the same directory tests write fixtures into. Clears
    /// `GEMINI_CLI_HOME` deliberately — `agy_plugin_dir()` reads it, and a
    /// developer's real env must not leak into the AGY test.
    fn plugin_test_env() -> (tempfile::TempDir, PathBuf, EnvGuard) {
        let guard = EnvGuard::new();
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
        // Config's test guard redirects any unregistered HCOM_DIR to a
        // throwaway location (see config.rs from_env) — claude_config_dir
        // resolves through cached Config, so without this the settings file
        // this test writes and the one the verifier reads would disagree.
        crate::paths::test_roots::register(&home);
        crate::config::Config::reset();
        crate::config::Config::init();
        (dir, home, guard)
    }

    fn write_complete_skill_payload(root: &std::path::Path) {
        let skill = root.join("skills").join("hcom-agent-messaging");
        for relative in super::PLUGIN_SKILL_FILES {
            let path = skill.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("fixture for {relative}\n")).unwrap();
        }
    }

    fn write_stage_source(root: &std::path::Path) {
        write_complete_skill_payload(root);
        let adapter = root.join("plugin/hcom-agy");
        std::fs::create_dir_all(adapter.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(adapter.join("hooks")).unwrap();
        std::fs::write(
            adapter.join(".claude-plugin/plugin.json"),
            r#"{"name":"hcom","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(adapter.join("hooks/hooks.json"), r#"{"hooks":{}}"#).unwrap();
    }

    fn relative_files(root: &std::path::Path) -> Vec<String> {
        fn visit(root: &std::path::Path, current: &std::path::Path, files: &mut Vec<String>) {
            for entry in std::fs::read_dir(current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    files.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace(std::path::MAIN_SEPARATOR, "/"),
                    );
                }
            }
        }

        let mut files = Vec::new();
        visit(root, root, &mut files);
        files.sort();
        files
    }

    #[test]
    fn review_regression_production_staging_needs_no_external_shell_tools() {
        let source = tempfile::tempdir().unwrap();
        write_stage_source(source.path());

        let (_staging, artifact) = super::stage_plugin_artifact(source.path(), "hcom-agy")
            .expect("native staging must not require scripts/stage-plugin.sh or sh");

        assert!(artifact.join(".claude-plugin/plugin.json").is_file());
        assert!(artifact.join("hooks/hooks.json").is_file());
        assert!(super::verify_plugin_skill_payload(&artifact).is_ok());
    }

    #[test]
    fn review_regression_agy_install_runner_receives_the_complete_staged_artifact() {
        let source = tempfile::tempdir().unwrap();
        write_stage_source(source.path());
        let inspected = std::cell::Cell::new(false);

        super::install_agy_plugin_from_root(
            source.path(),
            |artifact| {
                assert!(artifact.join(".claude-plugin/plugin.json").is_file());
                assert!(artifact.join("hooks/hooks.json").is_file());
                assert!(super::verify_plugin_skill_payload(artifact).is_ok());
                assert!(
                    !artifact
                        .canonicalize()
                        .unwrap()
                        .starts_with(source.path().canonicalize().unwrap()),
                    "install runner received a path inside the source checkout"
                );
                inspected.set(true);
                Ok(())
            },
            || true,
            || true,
        )
        .unwrap();

        assert!(
            inspected.get(),
            "install runner never inspected staged source"
        );
    }

    #[test]
    fn review_regression_missing_checkout_falls_back_to_the_published_agy_repo() {
        let seen = std::cell::RefCell::new(String::new());

        super::install_agy_plugin_from_url(
            super::HCOM_PLUGIN_REPOSITORY_URL,
            |url| {
                *seen.borrow_mut() = url.to_string();
                Ok(())
            },
            || true,
            || true,
        )
        .unwrap();

        assert_eq!(seen.into_inner(), super::HCOM_PLUGIN_REPOSITORY_URL);
        assert!(
            super::HCOM_PLUGIN_REPOSITORY_URL.starts_with("https://"),
            "agy only recognizes https:// as a remote, not git@host:path: {}",
            super::HCOM_PLUGIN_REPOSITORY_URL
        );
    }

    #[cfg(unix)]
    #[test]
    fn review_regression_staging_rejects_skill_links_outside_the_source_tree() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        write_stage_source(source.path());
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(external.path(), "outside source tree\n").unwrap();
        let linked = source
            .path()
            .join("skills/hcom-agent-messaging/references/cross-tool.md");
        std::fs::remove_file(&linked).unwrap();
        symlink(external.path(), &linked).unwrap();

        let error = super::stage_plugin_artifact(source.path(), "hcom-agy").unwrap_err();

        assert!(
            error.contains("outside"),
            "outside-source link must be rejected explicitly: {error}"
        );
    }

    #[test]
    fn plugin_skill_payload_rejects_hooks_only() {
        let fixture = tempfile::tempdir().unwrap();
        let hooks = fixture.path().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("hooks.json"), "{}").unwrap();

        let error = super::verify_plugin_skill_payload(fixture.path()).unwrap_err();
        assert!(error.contains("SKILL.md"), "unexpected error: {error}");
    }

    #[test]
    fn plugin_skill_payload_rejects_skill_without_references() {
        let fixture = tempfile::tempdir().unwrap();
        let skill = fixture.path().join("skills/hcom-agent-messaging");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "fixture").unwrap();

        let error = super::verify_plugin_skill_payload(fixture.path()).unwrap_err();
        assert!(
            error.contains("references/cross-tool.md"),
            "unexpected error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn plugin_skill_payload_rejects_external_skill_symlink() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        let artifact = fixture.path().join("artifact");
        let external = fixture.path().join("external");
        std::fs::create_dir_all(&artifact).unwrap();
        write_complete_skill_payload(&external);
        symlink(external.join("skills"), artifact.join("skills")).unwrap();

        let error = super::verify_plugin_skill_payload(&artifact).unwrap_err();
        assert!(
            error.contains("outside plugin artifact"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn plugin_skill_payload_accepts_complete_artifact() {
        let fixture = tempfile::tempdir().unwrap();
        write_complete_skill_payload(fixture.path());

        assert!(super::verify_plugin_skill_payload(fixture.path()).is_ok());
    }

    #[test]
    fn plugin_skill_payload_inventory_matches_every_canonical_file() {
        let canonical =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/hcom-agent-messaging");
        let actual = relative_files(&canonical);
        let mut verified = super::PLUGIN_SKILL_FILES
            .iter()
            .map(|relative| (*relative).to_string())
            .collect::<Vec<_>>();
        verified.sort();

        assert_eq!(
            verified, actual,
            "verify_plugin_skill_payload inventory must cover every canonical skill file"
        );
    }

    #[test]
    #[serial]
    fn agy_imported_hcom_source_reads_a_foreign_import() {
        let (_dir, home, _guard) = plugin_test_env();
        let config_dir = home.join(".gemini").join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("import_manifest.json"),
            r#"{"imports":[{"name":"hcom","source":"claude-code","importedAt":"2026-01-01T00:00:00Z","components":["hooks"]}]}"#,
        )
        .unwrap();
        assert_eq!(
            super::agy_imported_hcom_source().as_deref(),
            Some("claude-code")
        );
    }

    #[test]
    #[serial]
    fn agy_imported_hcom_source_is_none_without_a_manifest() {
        let (_dir, _home, _guard) = plugin_test_env();
        assert_eq!(super::agy_imported_hcom_source(), None);
    }

    /// Write an import entry plus an installed manifest, and read the state back.
    fn agy_state_with(manifest: &str) -> super::AgyHooks {
        let config_dir = crate::runtime_env::gemini_family_config_dir().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        // `hcom hooks add antigravity` produces exactly this entry: labelled
        // claude-code, because our manifest dir is `.claude-plugin/`.
        std::fs::write(
            config_dir.join("import_manifest.json"),
            r#"{"imports":[{"name":"hcom","source":"claude-code","importedAt":"2026-01-01T00:00:00Z","components":["hooks"]}]}"#,
        )
        .unwrap();
        let hooks_path = super::agy_plugin_dir().join(super::AGY_HOOKS_RELATIVE);
        std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
        std::fs::write(&hooks_path, manifest).unwrap();
        super::agy_hook_state()
    }

    /// A stand-in for one Claude-shaped entry.
    const CLAUDE_ENTRY: &str =
        r#"{"name":"hcom-sessionstart","type":"command","command":"hcom claude-sessionstart"}"#;

    #[test]
    #[serial]
    fn agy_hook_state_is_hcom_for_the_bundled_manifest() {
        let (_dir, _home, _guard) = plugin_test_env();
        // The real thing, not a hand-written stand-in: if the shipped manifest
        // ever stops satisfying this check, the check is what is wrong.
        let manifest = include_str!("../../plugin/hcom-agy/hooks/hooks.json");
        assert_eq!(agy_state_with(manifest), super::AgyHooks::Hcom);
    }

    #[test]
    #[serial]
    fn agy_hook_state_is_foreign_only_for_a_claude_shaped_manifest() {
        let (_dir, _home, _guard) = plugin_test_env();
        // SessionStart carrying a real command, and not one of our handlers
        // anywhere: that shape is the evidence, so naming the source is warranted.
        let manifest = format!(
            r#"{{"hooks":{{"SessionStart":[{CLAUDE_ENTRY}],"PostToolUse":[{CLAUDE_ENTRY}]}}}}"#
        );
        assert_eq!(
            agy_state_with(&manifest),
            super::AgyHooks::Foreign("claude-code".to_string())
        );
        // A bare key is not a handler.
        assert_eq!(
            agy_state_with(r#"{"hooks":{"SessionStart":[]}}"#),
            super::AgyHooks::Malformed
        );
    }

    #[test]
    #[serial]
    fn agy_hook_state_is_malformed_for_present_but_useless_events() {
        let (_dir, _home, _guard) = plugin_test_env();
        // Each of these parses, carries our event names, and cannot wake an agent.
        // A presence check would call every one of them healthy — and none of them
        // is evidence that another harness installed anything.
        for manifest in [
            r#"{"hooks":{"PreInvocation":[],"PostInvocation":[]}}"#,
            r#"{"hooks":{"PreInvocation":null,"PostInvocation":null}}"#,
            r#"{"hooks":{"PreInvocation":[null],"PostInvocation":[null]}}"#,
            r#"{"hooks":{"PreInvocation":[{"type":"command","command":"true"}],
                         "PostInvocation":[{"type":"command","command":"true"}]}}"#,
            // The word "hcom" without a handler that does anything.
            r#"{"hooks":{"PreInvocation":[{"type":"command","command":"hcom --version"}],
                         "PostInvocation":[{"type":"command","command":"hcom --version"}]}}"#,
            // Not a command entry — it runs nothing.
            r#"{"hooks":{"PreInvocation":[{"type":"http","command":"hcom gemini-beforeagent"}],
                         "PostInvocation":[{"type":"http","command":"hcom gemini-afteragent"}]}}"#,
            // Half an install: PostInvocation missing entirely.
            r#"{"hooks":{"PreInvocation":[{"type":"command","command":"hcom gemini-beforeagent"}]}}"#,
        ] {
            assert_eq!(
                agy_state_with(manifest),
                super::AgyHooks::Malformed,
                "must be reported as broken, not as another harness's: {manifest}"
            );
        }
    }

    #[test]
    #[serial]
    fn agy_hook_state_is_malformed_for_mutations_of_the_bundled_manifest() {
        let (_dir, _home, _guard) = plugin_test_env();
        let bundled = include_str!("../../plugin/hcom-agy/hooks/hooks.json");

        let swapped = bundled.replace("gemini-beforeagent", "claude-sessionstart");
        assert_eq!(agy_state_with(&swapped), super::AgyHooks::Malformed);

        let no_after = bundled.replace("gemini-afteragent", "gemini-sessionstart");
        assert_eq!(agy_state_with(&no_after), super::AgyHooks::Malformed);

        for renamed in [
            bundled.replace("gemini-beforeagent", "gemini-beforeagent-disabled"),
            bundled.replace("gemini-beforeagent", "x-gemini-beforeagent"),
            bundled.replace("gemini-afteragent", "gemini-afteragent2"),
        ] {
            assert_eq!(
                agy_state_with(&renamed),
                super::AgyHooks::Malformed,
                "a handler name that merely contains ours is not ours"
            );
        }

        let shared_only = r#"{"hooks":{"PostToolUse":[{"matcher":".*","hooks":[
            {"type":"command","command":"hcom gemini-aftertool"}]}]}}"#;
        assert_eq!(agy_state_with(shared_only), super::AgyHooks::Malformed);
    }

    #[test]
    #[serial]
    fn agy_hook_state_is_unverifiable_for_broken_json() {
        let (_dir, _home, _guard) = plugin_test_env();
        assert_eq!(agy_state_with("{not json"), super::AgyHooks::Unverifiable);
    }

    #[test]
    fn install_does_not_strip_legacy_when_the_cli_fails() {
        let stripped = std::cell::Cell::new(false);
        let outcome = super::install_then_strip(
            || Err("marketplace add failed: network unreachable".to_string()),
            || false, // verify says not installed
            || {
                stripped.set(true);
                true
            },
        );
        assert!(outcome.is_err(), "failed install must report an error");
        assert!(
            !stripped.get(),
            "legacy hooks must survive a failed install"
        );
    }

    #[test]
    fn install_does_not_strip_legacy_when_verify_fails() {
        let stripped = std::cell::Cell::new(false);
        let outcome = super::install_then_strip(
            || Ok(()), // CLI claims success
            || false,  // but verify disagrees
            || {
                stripped.set(true);
                true
            },
        );
        assert!(outcome.is_err());
        assert!(!stripped.get(), "verify is the gate, not the CLI exit code");
    }

    /// A strip that fails must not read as success: the user would be told the
    /// migration completed while both hook sets stay live and double-fire.
    #[test]
    fn install_reports_a_failed_strip() {
        let outcome = super::install_then_strip(|| Ok(()), || true, || false);
        assert!(outcome.is_err(), "a failed strip must surface");
        assert!(
            outcome.unwrap_err().contains("legacy hook"),
            "the error must name what went wrong"
        );
    }

    /// Regression guard for a defect caught in review: gating Cursor's strip on
    /// `verify_cursor_plugin_installed` deleted the user's hooks the moment the
    /// marketplace was added, because that verifier only proves a checkout
    /// exists and `marketplace add` is what creates it. Cursor's enabled marker
    /// is not readable from disk, so no gate is honest — the source must simply
    /// never strip.
    #[test]
    fn cursor_installer_never_strips_legacy_hooks() {
        let src = include_str!("plugin.rs");
        let body = src
            .split_once("pub(crate) fn install_cursor_plugin")
            .expect("install_cursor_plugin must exist")
            .1
            .split_once("\n}")
            .expect("function must be brace-terminated")
            .0;
        assert!(
            !body.contains("remove_cursor_hooks"),
            "install_cursor_plugin must not strip legacy hooks; found:\n{body}"
        );
    }

    #[test]
    fn install_strips_legacy_only_after_verify_passes() {
        let stripped = std::cell::Cell::new(false);
        let outcome = super::install_then_strip(
            || Ok(()),
            || true,
            || {
                stripped.set(true);
                true
            },
        );
        assert!(outcome.is_ok());
        assert!(stripped.get());
    }

    /// The strip runs on this user's real machine, where settings.json also holds
    /// agentpet, rtk, and herdr entries. Losing those would be a worse bug than
    /// the one we are fixing.
    #[test]
    #[serial]
    fn strip_preserves_hooks_owned_by_other_tools() {
        let (_dir, home, _guard) = plugin_test_env();
        let settings = home.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{
              "hooks": {
                "SessionStart": [
                  {"hooks":[{"type":"command","command":"bash '/home/u/.claude/hooks/herdr-agent-state.sh' session"}]},
                  {"hooks":[{"type":"command","command":"/home/u/.local/bin/agentpet-hook claude"}]},
                  {"hooks":[{"type":"command","command":"cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd sessionstart || exit 0"}]}
                ],
                "PreToolUse": [
                  {"hooks":[{"type":"command","command":"rtk hook claude"}]}
                ]
              }
            }"#,
        )
        .unwrap();

        crate::hooks::claude::remove_claude_hooks();

        let after = std::fs::read_to_string(&settings).unwrap();
        assert!(
            after.contains("herdr-agent-state.sh"),
            "herdr hook lost:\n{after}"
        );
        assert!(
            after.contains("agentpet-hook"),
            "agentpet hook lost:\n{after}"
        );
        assert!(after.contains("rtk hook claude"), "rtk hook lost:\n{after}");
        assert!(
            !after.contains("exec $cmd sessionstart"),
            "hcom hook survived:\n{after}"
        );
    }

    /// A strip that could not parse the file must not report success: hcom's
    /// entries may still be in there, so "migration finished" would be a lie
    /// and both hook sets would keep firing unnoticed.
    #[test]
    #[serial]
    fn strip_reports_failure_on_malformed_json() {
        let (_dir, home, _guard) = plugin_test_env();
        let settings = home.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "{ this is not json").unwrap();

        assert!(
            !crate::hooks::claude::remove_claude_hooks(),
            "an unparseable settings.json must report a failed strip"
        );
    }

    #[test]
    #[serial]
    fn strip_leaves_malformed_json_untouched() {
        let (_dir, home, _guard) = plugin_test_env();
        let settings = home.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let broken = "{ this is not json";
        std::fs::write(&settings, broken).unwrap();

        crate::hooks::claude::remove_claude_hooks();

        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            broken,
            "a file we cannot parse must not be rewritten"
        );
    }

    #[test]
    #[serial]
    fn claude_verify_needs_both_directory_and_enabled_flag() {
        let (_dir, home, _guard) = plugin_test_env();

        let settings = home.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();

        // Neither half present.
        std::fs::write(&settings, r#"{}"#).unwrap();
        assert!(!super::verify_claude_plugin_installed());

        // Enabled flag only, no plugin directory.
        std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":true}}"#).unwrap();
        assert!(!super::verify_claude_plugin_installed());

        // Directory only, no enabled flag.
        std::fs::write(&settings, r#"{}"#).unwrap();
        std::fs::create_dir_all(super::claude_plugin_dir().join("1.0.0")).unwrap();
        assert!(!super::verify_claude_plugin_installed());

        // Both halves present.
        std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":true}}"#).unwrap();
        assert!(super::verify_claude_plugin_installed());

        // Explicitly disabled by the user.
        std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":false}}"#).unwrap();
        assert!(!super::verify_claude_plugin_installed());
    }

    #[test]
    #[serial]
    fn agy_verify_needs_the_hook_file_not_just_the_directory() {
        let (_dir, _home, _guard) = plugin_test_env();
        let plugin_dir = super::agy_plugin_dir();

        std::fs::create_dir_all(&plugin_dir).unwrap();
        assert!(
            !super::verify_agy_plugin_installed(),
            "empty dir is not installed"
        );

        std::fs::create_dir_all(plugin_dir.join(super::AGY_HOOKS_RELATIVE).parent().unwrap())
            .unwrap();
        std::fs::write(
            plugin_dir.join(super::AGY_HOOKS_RELATIVE),
            r#"{"hooks":{}}"#,
        )
        .unwrap();
        assert!(super::verify_agy_plugin_installed());
    }

    #[test]
    #[serial]
    fn cursor_verify_walks_to_the_hook_file() {
        let (_dir, _home, _guard) = plugin_test_env();

        // No marketplaces directory at all.
        assert!(!super::verify_cursor_plugin_installed());

        let marketplaces = super::cursor_marketplaces_dir();
        let checkout = marketplaces
            .join("github.com")
            .join("hcom-owner")
            .join("hcom-repo")
            .join("deadbeef");

        // A nested-but-wrong file: present somewhere under the checkout, but
        // not at the exact path the verifier requires. Proves the walk
        // checks the specific file, not "any file exists under a checkout".
        let wrong = checkout
            .join("plugin")
            .join(super::PLUGIN_NAME)
            .join("hooks");
        std::fs::create_dir_all(&wrong).unwrap();
        std::fs::write(wrong.join("not-the-hook-file.json"), "{}").unwrap();
        assert!(!super::verify_cursor_plugin_installed());

        // The real hook file lands the checkout counts.
        std::fs::write(wrong.join("hooks-cursor.json"), "{}").unwrap();
        assert!(super::verify_cursor_plugin_installed());
    }

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

    /// `plugin/hcom/skills` is a committed directory, so the declared path
    /// resolves. It used to be a symlink to the repo-root `skills/`; Codex's
    /// installer skipped that link and shipped a package with hooks and no
    /// skill, so every adapter now carries real files generated by
    /// `scripts/sync-plugin-skills.sh` (see `tests/plugin_payload.rs`).
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

    const CODEX_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks-codex.json");
    const CODEX_DESCRIPTOR: &str = include_str!("../../plugin/hcom/.codex-plugin/plugin.json");

    /// Drives off `CODEX_HOOK_COMMANDS` — the same table the native installer
    /// builds its hook JSON from — so an event, subcommand or matcher change on
    /// the writer side fails here instead of silently diverging from the
    /// committed overlay. Commands use the self-resolving guard, not
    /// `build_codex_hook_command`: that one embeds `get_hcom_prefix()` resolved
    /// at install time, and a committed manifest is a static file (same reason
    /// spelled out for Cursor above). Native Codex hook JSON carries no
    /// timeouts, so the overlay carries none either.
    #[test]
    fn codex_manifest_covers_every_configured_event() {
        let root: Value = serde_json::from_str(CODEX_MANIFEST).unwrap();
        let hooks = root["hooks"].as_object().expect("hooks object");

        for (event, suffix, matcher) in crate::hooks::codex::CODEX_HOOK_COMMANDS {
            let groups = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event {event}"));
            let expected_command = crate::hooks::claude::build_hook_entry_command(suffix);
            let group = groups
                .iter()
                .find(|g| {
                    g["hooks"]
                        .as_array()
                        .is_some_and(|inner| inner.iter().any(|h| h["command"] == expected_command))
                })
                .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

            assert_eq!(
                group.get("matcher").and_then(Value::as_str),
                *matcher,
                "{event} matcher"
            );
            let hook = group["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["command"] == expected_command)
                .unwrap();
            assert_eq!(hook["type"], "command", "{event} hook type");
            assert!(
                hook.get("timeout").is_none(),
                "{event} must match the native payload, which sets no timeout"
            );
        }

        // Anything beyond the table would ship a handler Codex never fires, or
        // a Claude-only event (SessionEnd, PostToolUseFailure) that the native
        // integration deliberately omits.
        assert_eq!(
            hooks.len(),
            crate::hooks::codex::CODEX_HOOK_COMMANDS.len(),
            "manifest has events the table does not: {:?}",
            hooks.keys().collect::<Vec<_>>()
        );
    }

    /// A command parked on the wrong event must fail the contract above; this
    /// pins that the check is event-scoped, not a whole-file substring scan.
    #[test]
    fn codex_manifest_check_rejects_a_command_on_the_wrong_event() {
        let mut root: Value = serde_json::from_str(CODEX_MANIFEST).unwrap();
        let stop = root["hooks"]["Stop"].take();
        root["hooks"]["UserPromptSubmit"] = stop;

        let expected = crate::hooks::claude::build_hook_entry_command("codex-userpromptsubmit");
        assert!(
            !root["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["hooks"]
                    .as_array()
                    .is_some_and(|inner| inner.iter().any(|h| h["command"] == expected))),
            "swapped event still satisfied its own command"
        );
    }

    /// The selector both Claude and Codex install by must name the marketplace
    /// that `.claude-plugin/marketplace.json` actually declares, and the plugin
    /// inside it. A rename there silently breaks `codex plugin add`.
    #[test]
    fn the_plugin_selector_matches_the_committed_marketplace() {
        let marketplace: Value =
            serde_json::from_str(include_str!("../../.claude-plugin/marketplace.json")).unwrap();
        let (plugin, market) = super::CLAUDE_PLUGIN_ID.split_once('@').unwrap();
        assert_eq!(marketplace["name"], market);
        assert_eq!(super::CLAUDE_MARKETPLACE, market);
        let entries = marketplace["plugins"].as_array().unwrap();
        let entry = entries
            .iter()
            .find(|p| p["name"] == plugin)
            .unwrap_or_else(|| panic!("marketplace declares no plugin named {plugin}"));
        // Codex, Claude and Cursor all install the same package directory.
        assert_eq!(entry["source"], "./plugin/hcom");
    }

    /// Codex, like Cursor, resolves nothing by convention: an undeclared
    /// component is simply absent.
    #[test]
    fn codex_descriptor_points_at_its_own_hook_file() {
        let d: Value = serde_json::from_str(CODEX_DESCRIPTOR).unwrap();
        assert_eq!(d["name"], super::PLUGIN_NAME);
        assert_eq!(d["hooks"], "./hooks/hooks-codex.json");
        assert_eq!(d["skills"], "./skills/");
    }

    /// One package, one version: Claude reads `.claude-plugin/`, Cursor
    /// `.cursor-plugin/` and Codex `.codex-plugin/` out of the same directory,
    /// so a drifting version would report three different releases of one
    /// install.
    #[test]
    fn shared_package_descriptors_agree_on_name_and_version() {
        let claude: Value =
            serde_json::from_str(include_str!("../../plugin/hcom/.claude-plugin/plugin.json"))
                .unwrap();
        for other in [CURSOR_DESCRIPTOR, CODEX_DESCRIPTOR] {
            let d: Value = serde_json::from_str(other).unwrap();
            assert_eq!(d["name"], claude["name"]);
            assert_eq!(d["version"], claude["version"]);
        }
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

    // ── Uninstall command shapes ──────────────────────────────────────
    //
    // Pure argument-list assertions — no subprocess. The end-to-end effect
    // (does `claude plugin marketplace remove hcom` actually clear the
    // registry) is not verifiable without a real Claude/Cursor/Antigravity
    // CLI and account state, so it stays a manual check.

    #[test]
    fn claude_uninstall_runs_uninstall_then_marketplace_remove() {
        let commands = super::claude_uninstall_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0],
            (
                "claude",
                vec!["plugin", "uninstall", super::CLAUDE_PLUGIN_ID]
            )
        );
        assert_eq!(
            commands[1],
            (
                "claude",
                vec!["plugin", "marketplace", "remove", super::CLAUDE_MARKETPLACE]
            )
        );
    }

    #[test]
    fn cursor_uninstall_removes_the_marketplace() {
        assert_eq!(
            super::cursor_uninstall_command(),
            (
                "cursor-agent",
                vec!["plugin", "marketplace", "remove", super::PLUGIN_NAME]
            )
        );
    }

    #[test]
    fn agy_uninstall_uninstalls_the_plugin() {
        assert_eq!(
            super::agy_uninstall_command(),
            ("agy", vec!["plugin", "uninstall", super::PLUGIN_NAME])
        );
    }

    /// Uninstall must not shell out at all when the tool reports no plugin —
    /// otherwise every `hcom hooks remove <tool>` on a machine that never
    /// installed the plugin would spawn a CLI call that fails with a noisy
    /// "not installed" error. Verified by pointing verify at an empty test
    /// env rather than by mocking `run_tool_cli`, matching the pattern the
    /// verify tests above already use.
    #[test]
    #[serial]
    fn uninstall_is_a_noop_when_nothing_is_installed() {
        let (_dir, _home, _guard) = plugin_test_env();
        assert!(!super::verify_claude_plugin_installed());
        assert!(super::uninstall_claude_plugin().is_ok());
        assert!(!super::verify_cursor_plugin_installed());
        assert!(super::uninstall_cursor_plugin().is_ok());
        assert!(!super::verify_agy_plugin_installed());
        assert!(super::uninstall_agy_plugin().is_ok());
    }
}
