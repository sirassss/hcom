# hcom hooks as tool plugins — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship hcom's Claude, Cursor, and Antigravity hooks inside the hcom plugin instead of writing them into shared global config, and stop installing anything as a side effect of launching an agent.

**Architecture:** Three hook manifests live in the repo under `plugin/hcom/hooks/`, one per tool schema, generated from the same event tables that drive today's config writers. A new `src/hooks/plugin.rs` owns paths, file-only verification, and CLI-driven install. `Tool::try_setup_hooks` / `Tool::verify_hooks_installed` route these three tools to it. `ensure_hooks_installed` in the launcher stops repairing and only warns.

**Tech Stack:** Rust, `serde_json`, `std::process::Command` for the tool CLIs, `tempfile` + `serial_test` for env-scoped tests, `cargo test --locked`.

**Spec:** `docs/superpowers/specs/2026-09-03-hcom-hooks-as-plugin-design.md`

---

## Existing code this plan builds on

| Item | Location | Use |
|---|---|---|
| `CLAUDE_HOOK_CONFIGS` — `(event, matcher, suffix, timeout)` | `src/hooks/claude.rs:2759` | Source table for the Claude manifest |
| `CURSOR_HOOK_COMMANDS` — `(event, suffix)` | `src/hooks/cursor.rs:26` | Source table for the Cursor manifest |
| Antigravity hook groups | `src/hooks/antigravity.rs:163-225` | Source table for the AGY manifest |
| `remove_claude_hooks()` / `remove_cursor_hooks()` / `remove_antigravity_hooks()` | `claude.rs:3424`, `cursor.rs:423`, `antigravity.rs:270` | Legacy strip — already written, already preserves foreign entries |
| `Tool::try_setup_hooks` / `Tool::verify_hooks_installed` | `src/tool.rs:114` / `src/tool.rs:82` | Central dispatch to change |
| `ensure_hooks_installed` | `src/launcher.rs:578` | Where auto-install is removed |
| `install_opencode_plugin() -> io::Result<bool>` | `src/hooks/opencode.rs:673` | Pattern to copy for a plugin-shaped installer |
| `build_hcom_command()` | `src/runtime_env.rs:44` | Yields `hcom` or `uvx hcom` |
| `gemini_family_config_dir()` | `src/runtime_env.rs:50` | `~/.gemini` |

**Do not touch:** any `handle_*` function, `src/router.rs`, or the hook payload contracts. This plan changes installation only.

---

## File map

| File | Responsibility |
|---|---|
| `src/hooks/plugin.rs` | Measured paths (Task 1, landed). Gains file-only verify, CLI install, install→verify→strip ordering |
| `plugin/hcom/hooks/hooks.json` | New. Claude hook manifest |
| `plugin/hcom/hooks/hooks-cursor.json` | New. Cursor hook manifest |
| `plugin/hcom/.cursor-plugin/plugin.json` | New. Cursor plugin descriptor |
| `plugin/hcom-agy/.claude-plugin/plugin.json` | New. Descriptor Antigravity reads |
| `plugin/hcom-agy/hooks/hooks.json` | New. Antigravity hook manifest — same conventional path as Claude's, which is why it needs its own directory |
| `src/hooks/mod.rs` | Register the new module |
| `src/tool.rs` | Route Claude/Cursor/Antigravity to the plugin path |
| `src/launcher.rs` | Warn instead of install |
| `src/commands/hooks.rs` | Status output for the two new conditions |
| `skills/hcom-agent-messaging/references/cross-tool.md` | Document the change |
| `README.md` | Install section |

---

### Task 1: Measure where each tool actually reads plugin files

**Status: DONE (commit `ed975a5`).** Results are recorded in the module doc of `src/hooks/plugin.rs`; three of them changed Tasks 4, 5, and 6, and the spec's Measured constraints section. Re-read that doc before starting any later task.

**Files:**
- Create: `src/hooks/plugin.rs`
- Modify: `src/hooks/mod.rs`

This is a spike. The spec deliberately leaves three facts unmeasured because guessing them would bake a wrong constant into every later task. Measure first, then write the constants down.

- [x] **Step 1: Build a throwaway plugin and install it into each tool**

```bash
cd /tmp/claude-1000/*/scratchpad 2>/dev/null || cd /tmp
mkdir -p probe-plugin/.claude-plugin probe-plugin/.cursor-plugin probe-plugin/hooks
cd probe-plugin
cat > .claude-plugin/plugin.json <<'EOF'
{"name":"hcomprobe","version":"0.0.1","description":"placement probe"}
EOF
cat > .cursor-plugin/plugin.json <<'EOF'
{"name":"hcomprobe","version":"0.0.1","description":"placement probe","hooks":"./hooks/hooks-cursor.json"}
EOF
cat > gemini-extension.json <<'EOF'
{"name":"hcomprobe","version":"0.0.1","description":"placement probe"}
EOF
printf '{"hooks":{}}' > hooks/hooks.json
printf '{"version":1,"hooks":{}}' > hooks/hooks-cursor.json
printf '{"hooks":{}}' > hooks.json
```

- [x] **Step 2: Record what each tool does with it**

Run each, and keep the output — it is the input to Step 3:

```bash
agy plugin install "$PWD" ; agy plugin list
find ~/.gemini/config/plugins/hcomprobe -maxdepth 2 -type f

cursor-agent plugin marketplace add "$PWD" 2>&1 | tail -5
find ~/.cursor -maxdepth 4 -name "*hcomprobe*" 2>/dev/null

claude plugin marketplace add "$PWD" 2>&1 | tail -5
find ~/.claude/plugins -maxdepth 4 -name "*hcomprobe*" 2>/dev/null
```

Answer these three questions from the output:

1. **Where does each tool store the enabled/installed marker?** For Claude this is `enabledPlugins` in `~/.claude/settings.json` — confirm the exact key spelling (`hcom@hcom` vs `hcom`). For Cursor and Antigravity, find the equivalent file.
2. **Where does Antigravity expect `hooks.json`** — plugin root, or `hooks/hooks.json`?
3. **Does Cursor find `.cursor-plugin/plugin.json` when the marketplace source points at a repo whose plugin lives in a subdirectory** (`./plugin/hcom`)? If not, the manifest belongs at repo root.

- [x] **Step 3: Write the measurements down as constants**

Create `src/hooks/plugin.rs`. Replace each `MEASURED:` comment with what Step 2 showed — the values below are the expected shape, not an answer:

```rust
//! Plugin-based hook installation for tools whose config files are shared
//! across harnesses (Claude Code, Cursor, Antigravity).
//!
//! Writing hooks into `~/.claude/settings.json` leaks them: Cursor reads that
//! file too, so one agent ran both hook sets and two sessionEnd handlers raced.
//! Plugin hooks are scoped to the harness that enabled them.

use std::path::PathBuf;

/// Plugin name as the tools address it.
pub(crate) const PLUGIN_NAME: &str = "hcom";

/// Marketplace-qualified id used in Claude's `enabledPlugins`.
/// MEASURED: confirm spelling from `~/.claude/settings.json` after install.
pub(crate) const CLAUDE_PLUGIN_ID: &str = "hcom@hcom";

/// Directory Antigravity copies an installed plugin into.
pub(crate) fn agy_plugin_dir() -> PathBuf {
    crate::runtime_env::gemini_family_config_dir()
        .join("config")
        .join("plugins")
        .join(PLUGIN_NAME)
}

/// File Antigravity reads hooks from, relative to the installed plugin dir.
/// MEASURED: plugin root (`hooks.json`) or `hooks/hooks.json`.
pub(crate) const AGY_HOOKS_RELATIVE: &str = "hooks.json";
```

**Measured answer:** `hooks/hooks.json`. See the committed module for the full set of constants — the skeleton above is what was written before the measurement, kept here so the task reads in order.

Register it in `src/hooks/mod.rs` next to the other tool modules:

```rust
pub mod plugin;
```

- [x] **Step 4: Verify it compiles**

Run: `cargo build --locked`
Expected: builds clean. Dead-code warnings for the unused constants are fine at this stage.

- [x] **Step 5: Clean up the probe**

```bash
agy plugin uninstall hcomprobe 2>/dev/null || true
cursor-agent plugin marketplace remove hcomprobe 2>/dev/null || true
claude plugin marketplace remove hcomprobe 2>/dev/null || true
```

- [x] **Step 6: Commit**

```bash
git add src/hooks/plugin.rs src/hooks/mod.rs
git commit -m "$(cat <<'EOF'
feat(plugin): record measured plugin paths for claude, cursor, agy

Spike output: where each tool stores its enabled marker and hook file.
EOF
)"
```

If a tool's CLI is unavailable on this machine, stop and report BLOCKED with which one. Do not guess the constants.

---

### Task 2: Claude hook manifest, generated from the existing event table

**Files:**
- Create: `plugin/hcom/hooks/hooks.json`
- Test: `src/hooks/plugin.rs` (`mod tests`)

The manifest is a committed file, not generated at install time, so it shows up in diffs. The test's job is to stop it drifting from `CLAUDE_HOOK_CONFIGS`.

- [x] **Step 1: Write the failing test**

Append to `src/hooks/plugin.rs`:

```rust
#[cfg(test)]
mod tests {
    use serde_json::Value;

    const CLAUDE_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks.json");

    #[test]
    fn claude_manifest_covers_every_configured_event() {
        let root: Value = serde_json::from_str(CLAUDE_MANIFEST).unwrap();
        let hooks = root["hooks"].as_object().expect("hooks object");

        for (event, matcher, suffix, _timeout) in super::super::claude::CLAUDE_HOOK_CONFIGS {
            let entries = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event {event}"));
            let group = entries
                .iter()
                .find(|g| {
                    g["hooks"]
                        .as_array()
                        .is_some_and(|inner| {
                            inner.iter().any(|h| {
                                h["command"]
                                    .as_str()
                                    .is_some_and(|c| c.ends_with(&format!("exec $cmd {suffix}")))
                            })
                        })
                })
                .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

            if matcher.is_empty() {
                assert!(group.get("matcher").is_none(), "{event} should have no matcher");
            } else {
                assert_eq!(group["matcher"], *matcher, "{event} matcher");
            }
        }
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
}
```

`CLAUDE_HOOK_CONFIGS` is currently private. Make it `pub(crate)` in `src/hooks/claude.rs:2759`:

```rust
pub(crate) const CLAUDE_HOOK_CONFIGS: &[(&str, &str, &str, Option<u64>)] = &[
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked plugin::tests::claude_manifest -- --test-threads=1`
Expected: FAIL — the file does not exist, so `include_str!` breaks the build.

- [x] **Step 3: Write the manifest**

Create `plugin/hcom/hooks/hooks.json`. One group per event, matcher only where the table has one, `timeout` copied from the table where present. This is the full set from `CLAUDE_HOOK_CONFIGS` — all thirteen events:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd sessionstart || exit 0" } ] }
    ],
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd userpromptsubmit || exit 0" } ] }
    ],
    "PreToolUse": [
      { "matcher": "Bash|PowerShell|Agent|Task|Write|Edit", "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd pre || exit 0" } ] }
    ],
    "PostToolUse": [
      { "hooks": [ { "type": "command", "timeout": 86400, "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd post || exit 0" } ] }
    ],
    "PostToolUseFailure": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd post-failure || exit 0" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "timeout": 86400, "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd poll || exit 0" } ] }
    ],
    "StopFailure": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd stop-failure || exit 0" } ] }
    ],
    "PermissionRequest": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd permission-request || exit 0" } ] }
    ],
    "PermissionDenied": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd permission-denied || exit 0" } ] }
    ],
    "SubagentStart": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd subagent-start || exit 0" } ] }
    ],
    "SubagentStop": [
      { "hooks": [ { "type": "command", "timeout": 86400, "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd subagent-stop || exit 0" } ] }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd notify || exit 0" } ] }
    ],
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd sessionend || exit 0" } ] }
    ]
  }
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked plugin::tests::claude_manifest -- --test-threads=1`
Expected: PASS, both tests.

- [x] **Step 5: Commit**

```bash
git add plugin/hcom/hooks/hooks.json src/hooks/plugin.rs src/hooks/claude.rs
git commit -m "$(cat <<'EOF'
feat(plugin): ship Claude hook manifest inside the hcom plugin

Manifest is committed and pinned to CLAUDE_HOOK_CONFIGS by test, so the two
cannot drift.
EOF
)"
```

---

### Task 3: Cursor hook manifest and plugin descriptor

**Files:**
- Create: `plugin/hcom/hooks/hooks-cursor.json`
- Create: `plugin/hcom/.cursor-plugin/plugin.json` (path per Task 1 measurement)
- Test: `src/hooks/plugin.rs` (`mod tests`)

Cursor's schema differs from Claude's in envelope, not just key case: flat array, top-level `"version": 1`, relative command path, explicit `hooks` key in the descriptor.

- [x] **Step 1: Write the failing test**

Append inside the existing `mod tests`:

```rust
    const CURSOR_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks-cursor.json");
    const CURSOR_DESCRIPTOR: &str = include_str!("../../plugin/hcom/.cursor-plugin/plugin.json");

    #[test]
    fn cursor_manifest_covers_every_configured_event() {
        let root: Value = serde_json::from_str(CURSOR_MANIFEST).unwrap();
        assert_eq!(root["version"], 1, "Cursor requires a top-level version");
        let hooks = root["hooks"].as_object().expect("hooks object");

        for (event, suffix) in super::super::cursor::CURSOR_HOOK_COMMANDS {
            let entries = hooks
                .get(*event)
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("missing event {event}"));
            assert!(
                entries.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.ends_with(&format!("exec $cmd {suffix}")))
                }),
                "no hcom command for {event} -> {suffix}"
            );
        }
    }

    #[test]
    fn cursor_descriptor_points_at_its_own_hook_file() {
        let d: Value = serde_json::from_str(CURSOR_DESCRIPTOR).unwrap();
        assert_eq!(d["name"], super::PLUGIN_NAME);
        assert_eq!(d["hooks"], "./hooks/hooks-cursor.json");
        assert_eq!(d["skills"], "./skills/");
    }

    #[test]
    fn cursor_and_claude_manifests_do_not_share_commands() {
        let cursor: Value = serde_json::from_str(CURSOR_MANIFEST).unwrap();
        let text = cursor.to_string();
        for bare in ["exec $cmd sessionend", "exec $cmd poll", "exec $cmd post"] {
            assert!(
                !text.contains(bare),
                "Cursor manifest must not call Claude subcommands: {bare}"
            );
        }
    }
```

`CURSOR_HOOK_COMMANDS` is currently private. Make it `pub(crate)` in `src/hooks/cursor.rs:26`.

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked plugin::tests::cursor -- --test-threads=1`
Expected: FAIL — files missing, `include_str!` breaks the build.

- [x] **Step 3: Write the manifest and descriptor**

`plugin/hcom/hooks/hooks-cursor.json` — six events, `stop` keeps the 30s timeout established by the previous change set:

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [
      { "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd cursor-sessionstart || exit 0", "timeout": 15 }
    ],
    "beforeSubmitPrompt": [
      { "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd cursor-beforesubmitprompt || exit 0", "timeout": 15 }
    ],
    "preToolUse": [
      { "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd cursor-pretooluse || exit 0", "timeout": 15 }
    ],
    "postToolUse": [
      { "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd cursor-posttooluse || exit 0", "timeout": 15 }
    ],
    "stop": [
      { "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd cursor-stop || exit 0", "timeout": 30 }
    ],
    "sessionEnd": [
      { "command": "cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd cursor-sessionend || exit 0", "timeout": 15 }
    ]
  }
}
```

`plugin/hcom/.cursor-plugin/plugin.json`:

```json
{
  "name": "hcom",
  "displayName": "hcom",
  "description": "Let AI agents message, watch, and spawn each other across terminals.",
  "version": "1.0.0",
  "author": { "name": "aannoo" },
  "homepage": "https://github.com/aannoo/hcom",
  "repository": "https://github.com/aannoo/hcom",
  "license": "MIT",
  "skills": "./skills/",
  "hooks": "./hooks/hooks-cursor.json"
}
```

If Task 1 measured that Cursor needs the descriptor at repo root, put it there instead and update both `include_str!` paths in the tests to match.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked plugin::tests::cursor -- --test-threads=1`
Expected: PASS, all three tests.

- [x] **Step 5: Commit**

```bash
git add plugin/hcom/hooks/hooks-cursor.json plugin/hcom/.cursor-plugin/plugin.json src/hooks/plugin.rs src/hooks/cursor.rs
git commit -m "$(cat <<'EOF'
feat(plugin): ship Cursor hook manifest inside the hcom plugin

Cursor schema is flat with version:1; a test asserts it never calls Claude
subcommands, which is the cross-fire this change exists to stop.
EOF
)"
```

---

### Task 4: Antigravity plugin — a separate directory

**Files:**
- Create: `plugin/hcom-agy/.claude-plugin/plugin.json`
- Create: `plugin/hcom-agy/hooks/hooks.json`
- Test: `src/hooks/plugin.rs` (`mod tests`)

**Why a second directory** (measured in Task 1, recorded in `src/hooks/plugin.rs`): Antigravity reads `hooks/hooks.json` — the same conventional path Claude reads — and offers no way to redirect it. A `hooks.json` at the plugin root is skipped, a `"hooks"` key in `gemini-extension.json` is ignored, and deleting `gemini-extension.json` changes nothing because Antigravity reads `.claude-plugin/plugin.json`. Putting both tools' hooks in one directory would make Antigravity run Claude's handlers, which is the defect this whole design exists to remove.

`agy plugin install` takes a directory, so pointing it at `plugin/hcom-agy/` costs nothing but the directory itself. No `gemini-extension.json` is needed.

Antigravity's envelope matches Claude's (`matcher`, nested `hooks[]`, `type: command`) but its event vocabulary is its own, and every command must carry `ANTIGRAVITY_AGENT=1` — that env var is what routes the shared `gemini-*` handler to the Antigravity branch.

- [x] **Step 1: Write the failing test**

Append inside `mod tests`:

```rust
    const AGY_MANIFEST: &str = include_str!("../../plugin/hcom-agy/hooks/hooks.json");
    const AGY_DESCRIPTOR: &str = include_str!("../../plugin/hcom-agy/.claude-plugin/plugin.json");
    const CLAUDE_MANIFEST_FOR_CONTRAST: &str = include_str!("../../plugin/hcom/hooks/hooks.json");

    /// (event, subcommand) as installed today by try_setup_antigravity_hooks.
    const AGY_EXPECTED: &[(&str, &str)] = &[
        ("PreInvocation", "sessionstart"),
        ("PreInvocation", "gemini-beforeagent"),
        ("PostInvocation", "gemini-afteragent"),
        ("Stop", "gemini-sessionend"),
        ("PreToolUse", "gemini-beforetool"),
        ("PostToolUse", "gemini-aftertool"),
    ];

    fn agy_commands(root: &Value, event: &str) -> Vec<String> {
        let mut out = Vec::new();
        for group in root["hooks"][event].as_array().into_iter().flatten() {
            if let Some(inner) = group["hooks"].as_array() {
                out.extend(inner.iter().filter_map(|h| h["command"].as_str().map(String::from)));
            }
            if let Some(cmd) = group["command"].as_str() {
                out.push(cmd.to_string());
            }
        }
        out
    }

    #[test]
    fn agy_manifest_covers_every_configured_event() {
        let root: Value = serde_json::from_str(AGY_MANIFEST).unwrap();
        for (event, suffix) in AGY_EXPECTED {
            let cmds = agy_commands(&root, event);
            assert!(
                cmds.iter().any(|c| c.contains(&format!("exec {} ", "$cmd")) || c.contains(*suffix)),
                "{event} is missing {suffix}; found {cmds:?}"
            );
        }
    }

    #[test]
    fn agy_commands_all_pin_the_antigravity_env_var() {
        let root: Value = serde_json::from_str(AGY_MANIFEST).unwrap();
        for (event, _) in AGY_EXPECTED {
            for cmd in agy_commands(&root, event) {
                assert!(
                    cmd.contains("ANTIGRAVITY_AGENT=1"),
                    "{event} command must pin ANTIGRAVITY_AGENT=1, got: {cmd}"
                );
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
            AGY_MANIFEST, CLAUDE_MANIFEST_FOR_CONTRAST,
            "hooks/hooks.json must differ between plugin/hcom and plugin/hcom-agy"
        );
        let claude: Value = serde_json::from_str(CLAUDE_MANIFEST_FOR_CONTRAST).unwrap();
        let agy: Value = serde_json::from_str(AGY_MANIFEST).unwrap();
        let claude_events: Vec<_> = claude["hooks"].as_object().unwrap().keys().collect();
        let agy_events: Vec<_> = agy["hooks"].as_object().unwrap().keys().collect();
        assert!(
            agy_events.iter().any(|e| e.as_str() == "PreInvocation"),
            "AGY manifest lost its own event vocabulary: {agy_events:?}"
        );
        assert!(
            !claude_events.iter().any(|e| e.as_str() == "PreInvocation"),
            "Claude manifest must not carry AGY events: {claude_events:?}"
        );
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked plugin::tests::agy -- --test-threads=1`
Expected: FAIL — files missing.

- [x] **Step 3: Write the manifest and descriptor**

`plugin/hcom-agy/.claude-plugin/plugin.json` — Antigravity reads this, not `gemini-extension.json`:

```json
{
  "name": "hcom",
  "version": "1.0.0",
  "description": "hcom lifecycle hooks for Antigravity CLI",
  "author": { "name": "aannoo" },
  "homepage": "https://github.com/aannoo/hcom",
  "repository": "https://github.com/aannoo/hcom",
  "license": "MIT"
}
```

`plugin/hcom-agy/hooks/hooks.json`:

```json
{
  "hooks": {
    "PreInvocation": [
      {
        "name": "hcom-sessionstart",
        "type": "command",
        "timeout": 15,
        "description": "Initialize hcom session",
        "command": "sh -c 'command -v hcom >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec hcom sessionstart || exit 0'"
      },
      {
        "name": "hcom-beforeagent",
        "type": "command",
        "timeout": 15,
        "description": "Deliver pending messages",
        "command": "sh -c 'command -v hcom >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec hcom gemini-beforeagent || exit 0'"
      }
    ],
    "PostInvocation": [
      {
        "name": "hcom-afteragent",
        "type": "command",
        "timeout": 15,
        "description": "Signal ready for messages",
        "command": "sh -c 'command -v hcom >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec hcom gemini-afteragent || exit 0'"
      }
    ],
    "Stop": [
      {
        "name": "hcom-sessionend",
        "type": "command",
        "timeout": 15,
        "description": "Disconnect from hcom",
        "command": "sh -c 'command -v hcom >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec hcom gemini-sessionend || exit 0'"
      }
    ],
    "PreToolUse": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "name": "hcom-beforetool",
            "type": "command",
            "timeout": 15,
            "description": "Check for messages before tools",
            "command": "sh -c 'command -v hcom >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec hcom gemini-beforetool || exit 0'"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "name": "hcom-aftertool",
            "type": "command",
            "timeout": 15,
            "description": "Deliver messages after tools",
            "command": "sh -c 'command -v hcom >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec hcom gemini-aftertool || exit 0'"
          }
        ]
      }
    ]
  }
}
```

Compare against the live `hcom-lifecycle` group written by `try_setup_antigravity_hooks` (`src/hooks/antigravity.rs:163-225`) and copy any field this omits — `name`, `description`, and `timeout` all matter to Antigravity's own status output.

Do **not** add a `gemini-extension.json`: Task 1 measured that Antigravity ignores it for hook discovery and installs fine without it. Adding one implies a discovery path that does not exist.

- [x] **Step 3b: Confirm the install actually picks up the hooks**

```bash
agy plugin install "$PWD/plugin/hcom-agy"
agy plugin list | grep -A4 '"name": "hcom"'
agy plugin uninstall hcom
```

Expected: the install output says `hooks : 1 processed` (not `skipped (not found)`), and the list entry shows `"components": ["hooks"]` or `["skills","hooks"]`. If it says skipped, the file is at the wrong path — fix it here rather than discovering it in acceptance.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked plugin::tests::agy -- --test-threads=1`
Expected: PASS, all three tests.

- [x] **Step 5: Commit**

```bash
git add plugin/hcom-agy src/hooks/plugin.rs
git commit -m "$(cat <<'EOF'
feat(plugin): ship Antigravity hooks as a separate plugin directory

Antigravity reads hooks/hooks.json, the same conventional path Claude reads, and
offers no way to redirect it — so a shared directory would hand Claude handlers
to an Antigravity agent. Keeps the gemini-* subcommands with ANTIGRAVITY_AGENT=1
pinned in each command string.
EOF
)"
```

---

### Task 5: File-only verification

**Files:**
- Modify: `src/hooks/plugin.rs`
- Test: `src/hooks/plugin.rs` (`mod tests`)

Verify runs before every spawn, so it must not shell out. Two reads: the plugin exists on disk, and the tool records it as enabled.

- [x] **Step 1: Write the failing test**

```rust
    use crate::hooks::test_helpers::EnvGuard;
    use serial_test::serial;

    #[test]
    #[serial]
    fn claude_verify_needs_both_directory_and_enabled_flag() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let _guard = EnvGuard::set_home(home);

        let settings = home.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();

        // Neither half present.
        std::fs::write(&settings, r#"{}"#).unwrap();
        assert!(!super::verify_claude_plugin_installed());

        // Enabled flag only, no plugin directory.
        std::fs::write(
            &settings,
            r#"{"enabledPlugins":{"hcom@hcom":true}}"#,
        )
        .unwrap();
        assert!(!super::verify_claude_plugin_installed());

        // Both halves present.
        std::fs::create_dir_all(home.join(".claude/plugins/cache/hcom/hcom")).unwrap();
        assert!(super::verify_claude_plugin_installed());

        // Explicitly disabled by the user.
        std::fs::write(
            &settings,
            r#"{"enabledPlugins":{"hcom@hcom":false}}"#,
        )
        .unwrap();
        assert!(!super::verify_claude_plugin_installed());
    }

    #[test]
    #[serial]
    fn agy_verify_needs_the_hook_file_not_just_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set_home(dir.path());
        let plugin_dir = super::agy_plugin_dir();

        std::fs::create_dir_all(&plugin_dir).unwrap();
        assert!(!super::verify_agy_plugin_installed(), "empty dir is not installed");

        std::fs::write(plugin_dir.join(super::AGY_HOOKS_RELATIVE), r#"{"hooks":{}}"#).unwrap();
        assert!(super::verify_agy_plugin_installed());
    }
```

`EnvGuard` lives in `src/hooks/mod.rs:40` and restores a fixed field list including `HOME`. Check its constructor name and adapt this call; if it has no `set_home`, use the pattern the Cursor tests already use (`cursor_test_env` in `src/hooks/cursor.rs`).

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked plugin::tests::claude_verify plugin::tests::agy_verify -- --test-threads=1`
Expected: FAIL — `verify_claude_plugin_installed` is not defined.

- [x] **Step 3: Write the implementation**

Add to `src/hooks/plugin.rs`:

```rust
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

/// Where Claude caches an installed marketplace plugin.
fn claude_plugin_dir() -> PathBuf {
    crate::hooks::claude::get_claude_settings_path()
        .parent()
        .map(|d| d.join("plugins").join("cache").join(PLUGIN_NAME).join(PLUGIN_NAME))
        .unwrap_or_default()
}

pub(crate) fn verify_agy_plugin_installed() -> bool {
    agy_plugin_dir().join(AGY_HOOKS_RELATIVE).is_file()
}

/// Cursor checks a marketplace out under
/// `plugins/marketplaces/<host>/<owner>/<repo>/<sha>/`, so the plugin body sits
/// at `<sha>/plugin/hcom/`. Any checkout carrying our Cursor hook file counts.
pub(crate) fn verify_cursor_plugin_installed() -> bool {
    let Ok(hosts) = std::fs::read_dir(cursor_marketplaces_dir()) else {
        return false;
    };
    hosts
        .flatten()
        .flat_map(|host| std::fs::read_dir(host.path()).into_iter().flatten().flatten())
        .flat_map(|owner| std::fs::read_dir(owner.path()).into_iter().flatten().flatten())
        .flat_map(|repo| std::fs::read_dir(repo.path()).into_iter().flatten().flatten())
        .any(|sha| {
            sha.path()
                .join("plugin")
                .join(PLUGIN_NAME)
                .join("hooks")
                .join("hooks-cursor.json")
                .is_file()
        })
}
```

`cursor_marketplaces_dir()` and `claude_plugin_dir()` already exist from Task 1 with the measured layouts. Two caveats recorded there:

- Claude's cache path ends in a **version** segment (`cache/hcom/hcom/1.0.0/`), so `claude_plugin_dir()` returns the parent and `verify_claude_plugin_installed` checks it is a directory rather than looking for a fixed version.
- Cursor's *enabled* marker could not be measured, because installing requires the interactive picker. This verifier therefore proves the marketplace checkout is present, not that the user finished the install. That is the weaker guarantee the spec accepts for Cursor, and it is why `install_cursor_plugin` does not strip legacy hooks.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked plugin::tests -- --test-threads=1`
Expected: PASS, all manifest and verify tests.

- [x] **Step 5: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "$(cat <<'EOF'
feat(plugin): verify plugin installs by reading files only

Verify runs before every spawn; shelling out to a tool CLI there would cost a
process launch per agent.
EOF
)"
```

---

### Task 6: Install through the tool's own CLI, strip legacy only after verify

**Files:**
- Modify: `src/hooks/plugin.rs`
- Test: `src/hooks/plugin.rs` (`mod tests`)

The ordering is the whole safety property: install → verify → strip. A failure anywhere before the strip leaves the machine exactly as it was, still working on its legacy hooks.

- [x] **Step 1: Write the failing test**

```rust
    #[test]
    fn install_does_not_strip_legacy_when_the_cli_fails() {
        let mut stripped = false;
        let outcome = super::install_then_strip(
            || Err("marketplace add failed: network unreachable".to_string()),
            || false, // verify says not installed
            || stripped = true,
        );
        assert!(outcome.is_err(), "failed install must report an error");
        assert!(!stripped, "legacy hooks must survive a failed install");
    }

    #[test]
    fn install_does_not_strip_legacy_when_verify_fails() {
        let mut stripped = false;
        let outcome = super::install_then_strip(
            || Ok(()),   // CLI claims success
            || false,    // but verify disagrees
            || stripped = true,
        );
        assert!(outcome.is_err());
        assert!(!stripped, "verify is the gate, not the CLI exit code");
    }

    #[test]
    fn install_strips_legacy_only_after_verify_passes() {
        let mut stripped = false;
        let outcome = super::install_then_strip(|| Ok(()), || true, || stripped = true);
        assert!(outcome.is_ok());
        assert!(stripped);
    }

    /// The strip runs on this user's real machine, where settings.json also holds
    /// agentpet, rtk, and herdr entries. Losing those would be a worse bug than
    /// the one we are fixing.
    #[test]
    #[serial]
    fn strip_preserves_hooks_owned_by_other_tools() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set_home(dir.path());
        let settings = dir.path().join(".claude/settings.json");
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
        assert!(after.contains("herdr-agent-state.sh"), "herdr hook lost:\n{after}");
        assert!(after.contains("agentpet-hook"), "agentpet hook lost:\n{after}");
        assert!(after.contains("rtk hook claude"), "rtk hook lost:\n{after}");
        assert!(!after.contains("exec $cmd sessionstart"), "hcom hook survived:\n{after}");
    }

    #[test]
    #[serial]
    fn strip_leaves_malformed_json_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set_home(dir.path());
        let settings = dir.path().join(".claude/settings.json");
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
```

The last two exercise `remove_claude_hooks`, which already exists. If it already has equivalent coverage, keep these anyway — the fixture here is this user's actual machine layout, and it is the case the migration must not break.

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked plugin::tests::install_ -- --test-threads=1`
Expected: FAIL — `install_then_strip` is not defined.

- [x] **Step 3: Write the implementation**

**Do not gate a strip on `verify_cursor_plugin_installed` here.** An earlier draft did, and review caught it: that verifier only proves a marketplace checkout exists, and `marketplace add` — the line immediately above it — is what creates the checkout. Once the repo ships `plugin/hcom/hooks/hooks-cursor.json`, the gate passes the instant that command succeeds, so the strip would delete `~/.cursor/hooks.json` (and hcom's Cursor permissions, which `remove_cursor_hooks` also clears) while the plugin sits un-enabled in the TUI — leaving Cursor with no hooks at all, silently. Cursor's enabled marker is not readable from disk, so there is no honest signal to gate on. The Cursor path never strips.

```rust
/// Install → verify → strip, in that order.
///
/// `strip` runs only when `verify` returns true. A CLI that exits 0 without
/// actually installing must not cost the user their working legacy hooks, so
/// verification — not the exit code — is the gate.
pub(crate) fn install_then_strip<I, V, S>(install: I, verify: V, strip: S) -> Result<(), String>
where
    I: FnOnce() -> Result<(), String>,
    V: FnOnce() -> bool,
    S: FnOnce(),
{
    install()?;
    if !verify() {
        return Err("plugin install reported success but verification failed".to_string());
    }
    strip();
    Ok(())
}

/// Run a tool CLI, returning its stderr on failure.
fn run_tool_cli(program: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program} not runnable: {e}. Install it or run the command by hand."))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Published repository, used verbatim by Cursor (which rejects local paths).
pub(crate) const HCOM_REPOSITORY_URL: &str = "https://github.com/aannoo/hcom";

/// Marketplace source: the local checkout when dev_root is set, else the
/// repository URL declared in the plugin manifest.
///
/// dev_root support is what lets a contributor install their working tree
/// without pushing — both `claude plugin marketplace add` and
/// `agy plugin install` accept a local path. Cursor does not; see
/// `install_cursor_plugin`.
fn marketplace_source() -> String {
    // `paths::db_path()` is a free function, so nothing has to be threaded
    // through `try_setup_hooks` to reach dev_root.
    let db_path = crate::paths::db_path();
    if let Some((root, _source)) = crate::router::resolve_effective_dev_root(&db_path) {
        return root.to_string_lossy().to_string();
    }
    HCOM_REPOSITORY_URL.to_string()
}

pub(crate) fn install_claude_plugin() -> Result<(), String> {
    let source = marketplace_source();
    install_then_strip(
        || {
            run_tool_cli("claude", &["plugin", "marketplace", "add", &source])?;
            run_tool_cli("claude", &["plugin", "install", CLAUDE_PLUGIN_ID])
        },
        verify_claude_plugin_installed,
        || {
            crate::hooks::claude::remove_claude_hooks();
        },
    )
}

/// Cursor: add the marketplace, then hand the user the one step hcom cannot do.
///
/// `cursor-agent plugin` exposes only `marketplace` — installation happens in
/// the interactive `/plugins` picker (measured, Task 1). Verification therefore
/// cannot pass inside this call, so the legacy strip is deliberately skipped:
/// a later `hcom hooks add cursor`, once the plugin is really installed, does
/// it. Cursor also rejects local paths for a marketplace, so `dev_root` cannot
/// drive this and the remote URL is always used.
pub(crate) fn install_cursor_plugin() -> Result<(), String> {
    run_tool_cli(
        "cursor-agent",
        &["plugin", "marketplace", "add", HCOM_REPOSITORY_URL],
    )?;

    Err(format!(
        "marketplace added. Finish inside Cursor: run /plugins and install \"{PLUGIN_NAME}\".\n\
         Your existing hooks in ~/.cursor/hooks.json are left in place and keep working;\n\
         remove them with `hcom hooks remove cursor` once the plugin is enabled."
    ))
}

pub(crate) fn install_agy_plugin() -> Result<(), String> {
    let source = format!("{}/plugin/hcom-agy", marketplace_source());
    install_then_strip(
        || run_tool_cli("agy", &["plugin", "install", &source]),
        verify_agy_plugin_installed,
        || {
            crate::hooks::antigravity::remove_antigravity_hooks();
        },
    )
}
```

`agy plugin install` takes a **directory**, not a git URL. With `dev_root` set it points at the working tree's `plugin/hcom-agy`. With `dev_root` unset there is no local checkout to point at, so `install_agy_plugin` must detect that case and return an actionable error — `clone the repo and run: agy plugin install <repo>/plugin/hcom-agy` — rather than passing a URL that `agy` will reject.

All three installers take no arguments.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked plugin::tests -- --test-threads=1`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "$(cat <<'EOF'
feat(plugin): install via tool CLI, strip legacy hooks only after verify

Verification gates the strip, not the CLI exit code: a CLI that exits 0 without
installing must not cost the user their working hooks.
EOF
)"
```

---

### Task 7: Route the three tools through the plugin path

**Files:**
- Modify: `src/tool.rs:82` (`verify_hooks_installed`), `src/tool.rs:114` (`try_setup_hooks`)
- Test: `src/tool.rs` (`mod tests`)

- [x] **Step 1: Write the failing test**

Add to `src/tool.rs` tests:

```rust
    #[test]
    #[serial]
    fn plugin_tools_verify_through_the_plugin_path() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::hooks::test_helpers::EnvGuard::set_home(dir.path());

        // Nothing installed anywhere.
        assert!(!Tool::Claude.verify_hooks_installed(false));
        assert!(!Tool::Cursor.verify_hooks_installed(false));
        assert!(!Tool::Antigravity.verify_hooks_installed(false));

        // A legacy settings.json full of hcom hooks must NOT count as installed
        // any more — that file is exactly what we are migrating away from.
        let settings = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        crate::hooks::claude::try_setup_claude_hooks(false).unwrap();
        assert!(
            !Tool::Claude.verify_hooks_installed(false),
            "legacy hooks must not satisfy the plugin verifier"
        );
    }
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test --locked plugin_tools_verify_through_the_plugin_path -- --test-threads=1`
Expected: FAIL — the last assertion, because `verify_hooks_installed` still reads `settings.json`.

- [x] **Step 3: Write the implementation**

In `src/tool.rs`, replace the three arms in `verify_hooks_installed`:

```rust
            Tool::Claude => crate::hooks::plugin::verify_claude_plugin_installed(),
            Tool::Cursor => crate::hooks::plugin::verify_cursor_plugin_installed(),
            Tool::Antigravity => crate::hooks::plugin::verify_agy_plugin_installed(),
```

and the three arms in `try_setup_hooks`:

```rust
            Tool::Claude => crate::hooks::plugin::install_claude_plugin(),
            Tool::Cursor => crate::hooks::plugin::install_cursor_plugin(),
            Tool::Antigravity => crate::hooks::plugin::install_agy_plugin(),
```

`try_setup_hooks` already returns `Result<(), String>`, and no signature change is needed: `crate::paths::db_path()` is a free function, so `marketplace_source` resolves dev_root on its own. (An earlier draft of this plan proposed threading a `db_path` parameter through `try_setup_hooks` — unnecessary, and it would have rippled to every caller.)

- [x] **Step 4: Run tests to verify they pass**

Run:
```
cargo test --locked tool:: -- --test-threads=1
cargo test --locked plugin::tests -- --test-threads=1
```
Expected: PASS. Other tools' arms are untouched.

- [x] **Step 5: Commit**

```bash
git add src/tool.rs
git commit -m "$(cat <<'EOF'
feat(plugin): route claude, cursor, agy through the plugin installer

Legacy settings.json entries no longer satisfy verification for these three.
EOF
)"
```

---

### Task 8: The launcher warns and never installs

**Files:**
- Modify: `src/launcher.rs:578` (`ensure_hooks_installed`)
- Test: `src/launcher.rs` (`mod tests`)

This is the behavior change the user asked for, and the regression guard is the point of the task: nothing may install as a side effect of launching an agent.

- [x] **Step 1: Write the failing test**

```rust
    #[test]
    #[serial]
    fn launching_never_installs_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::hooks::test_helpers::EnvGuard::set_home(dir.path());
        let settings = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "{}\n").unwrap();
        let before = std::fs::read_to_string(&settings).unwrap();

        // Hooks are not installed; launching must still succeed.
        let result = super::ensure_hooks_installed(&LaunchTool::Claude, false, None);
        assert!(result.is_ok(), "a missing plugin must not block a launch");

        let after = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(before, after, "launching must not write config files");
        assert!(
            !dir.path().join(".claude/plugins").exists(),
            "launching must not install a plugin"
        );
    }

    #[test]
    #[serial]
    fn launching_reports_the_install_command_when_hooks_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::hooks::test_helpers::EnvGuard::set_home(dir.path());
        let warning = super::hooks_missing_warning(&LaunchTool::Cursor);
        assert!(warning.contains("hcom hooks add cursor"), "{warning}");
        assert!(warning.contains("not installed"), "{warning}");
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked launching_never_installs_hooks launching_reports_the_install_command -- --test-threads=1`
Expected: FAIL — `hooks_missing_warning` is undefined, and `ensure_hooks_installed` still calls `try_setup_*`.

- [x] **Step 3: Write the implementation**

Add to `src/launcher.rs`:

```rust
/// Message shown when a tool's hooks are not installed.
///
/// Launching must never fix this: installing hooks edits files on the user's
/// machine, and that should happen because they asked, not as a side effect of
/// starting an agent.
fn hooks_missing_warning(tool: &LaunchTool) -> String {
    let name = match tool {
        LaunchTool::Claude | LaunchTool::ClaudePty => "claude",
        LaunchTool::Cursor => "cursor",
        LaunchTool::Antigravity => "antigravity",
        other => other.as_str(),
    };
    format!(
        "hcom hooks are not installed for {name}.\n\
         Messages will not be delivered automatically this session.\n  \
         Install:  hcom hooks add {name}"
    )
}
```

`LaunchTool::as_str()` exists at `src/launcher.rs:81` and already returns `"cursor"` for `LaunchTool::Cursor`; the explicit arms above exist only to collapse `ClaudePty` onto `claude` and to match the names `hcom hooks add` accepts. If `as_str` already returns exactly those three strings, drop the match and call it directly.

Replace the three arms in `ensure_hooks_installed`:

```rust
        LaunchTool::Claude | LaunchTool::ClaudePty => {
            if !crate::hooks::plugin::verify_claude_plugin_installed() {
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
        LaunchTool::Antigravity => {
            if !crate::hooks::plugin::verify_agy_plugin_installed() {
                eprintln!("{}", hooks_missing_warning(tool));
            }
            Ok(())
        }
```

Every other arm stays exactly as it is. Delete the now-unused `install_diag_context` calls for these three only if the compiler flags them as dead; leave the helper itself alone since other tools use it.

- [x] **Step 4: Run tests to verify they pass**

Run:
```
cargo test --locked launching_never_installs_hooks launching_reports_the_install_command -- --test-threads=1
cargo test --locked launcher:: -- --test-threads=1
```
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/launcher.rs
git commit -m "$(cat <<'EOF'
feat(launcher): warn instead of installing hooks on spawn

Starting an agent no longer edits config files. A missing plugin prints the
install command and launches anyway, falling back to ad-hoc mode.
EOF
)"
```

---

### Task 9: Status reports the two conditions that now need a human

**Facts confirmed against the code before writing this task:**

- `cmd_hooks_status` (`src/commands/hooks.rs:83`) prints from `get_tool_status()` (`:69`), which returns `(tool, tool.verify_hooks_installed(false), tool.hooks_settings_path())`. After Task 7 the boolean is the *plugin* verifier's answer for Claude/Cursor/Antigravity — so the "installed" half needs no new plumbing.
- The legacy verifiers are all still public and are how to detect the second half: `crate::hooks::claude::verify_claude_hooks_installed(None, false)`, `crate::hooks::cursor::verify_cursor_hooks_installed(false)`, `crate::hooks::antigravity::verify_antigravity_hooks_installed(false)`.
- **`hooks_settings_path()` becomes misleading for these three** (`src/tool.rs:182-188`): it returns `~/.claude/settings.json`, `~/.cursor/hooks.json`, `~/.gemini/config/hooks.json`, none of which hold hcom's hooks once the plugin is in use. Status must not print that path as the location of an installed plugin. Print the plugin directory instead, or omit the path for these tools.


**Files:**
- Modify: `src/commands/hooks.rs`
- Test: `src/commands/hooks.rs` (`mod tests`)

With nothing self-repairing, status is the only place a user learns they must act.

- [x] **Step 1: Write the failing test**

```rust
    #[test]
    fn status_flags_plugin_and_legacy_coexisting() {
        let line = super::plugin_status_line("claude", /* plugin */ true, /* legacy */ true);
        assert!(line.contains("double-fire"), "{line}");
        assert!(line.contains("hcom hooks add claude"), "{line}");
    }

    #[test]
    fn status_flags_missing_install() {
        let line = super::plugin_status_line("cursor", false, false);
        assert!(line.contains("hcom hooks add cursor"), "{line}");
    }

    #[test]
    fn status_is_quiet_when_only_the_plugin_is_present() {
        let line = super::plugin_status_line("agy", true, false);
        assert!(line.is_empty(), "healthy state needs no advice, got: {line}");
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked status_flags status_is_quiet -- --test-threads=1`
Expected: FAIL — `plugin_status_line` undefined.

- [x] **Step 3: Write the implementation**

```rust
/// Advice line for a plugin-based tool. Empty when the state is healthy.
pub(crate) fn plugin_status_line(tool: &str, plugin: bool, legacy: bool) -> String {
    match (plugin, legacy) {
        (true, true) => format!(
            "{tool}: plugin and legacy hooks both present — double-fire risk. \
             Run: hcom hooks add {tool}"
        ),
        (false, _) => format!("{tool}: hooks not installed. Run: hcom hooks add {tool}"),
        (true, false) => String::new(),
    }
}
```

Wire it into the existing status output next to the current per-tool reporting, passing the plugin verifier's result and a legacy check (`get_claude_settings_path()` containing an hcom command, via the existing `is_hcom_hook_command`).

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked commands::hooks -- --test-threads=1`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add src/commands/hooks.rs
git commit -m "$(cat <<'EOF'
feat(hooks): status names missing installs and double-fire risk

Nothing self-repairs any more, so status is where a user learns to act.
EOF
)"
```

---

### Task 10: Documentation

**Files:**
- Modify: `skills/hcom-agent-messaging/references/cross-tool.md`
- Modify: `README.md`
- Modify: spec status line in `docs/superpowers/specs/2026-09-03-hcom-hooks-as-plugin-design.md`

- [x] **Step 1: Update the cross-tool reference**

In the Claude, Cursor, and Antigravity sections, replace the hook-install sentence with:

```markdown
- **Hook install**: Ships as a plugin (`hcom hooks add <tool>`), not as entries in the tool's shared config. Config files like `~/.claude/settings.json` are read by other harnesses — Cursor reads Claude's — so hooks placed there fire under agents they were never meant for. hcom never installs hooks automatically; launching an agent without them warns and falls back to ad-hoc mode.
```

- [x] **Step 2: Update README**

In the Install section, after the install commands, add:

```markdown
Hooks are not installed automatically. Enable them per tool:

```bash
hcom hooks add claude    # or: cursor, antigravity
hcom hooks status
```
```

- [x] **Step 3: Flip the spec status**

Change the spec header **Status** to: `Plan written at docs/superpowers/plans/2026-09-03-hcom-hooks-as-plugin.md`

- [x] **Step 4: Commit**

```bash
git add skills/hcom-agent-messaging/references/cross-tool.md README.md docs/superpowers/specs/2026-09-03-hcom-hooks-as-plugin-design.md
git commit -m "$(cat <<'EOF'
docs: plugin-based hook install, no automatic installation
EOF
)"
```

No failing test for markdown.

---

### Task 11: Regression sweep and manual acceptance

**Files:** none new.

- [x] **Step 1: Full suite**

```
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

Expected: all pass. `shell_env::tests::resolver_discards_stderr_without_breaking_env_resolution` is a known pre-existing environment flake and does not block.

- [x] **Step 2: Confirm the handler code never moved**

```bash
git diff --stat main..HEAD -- src/hooks/claude.rs src/hooks/cursor.rs src/hooks/antigravity.rs src/router.rs
```

Expected: `src/router.rs` untouched; the three hook files changed only in their install/verify surface and constant visibility. If a `handle_*` function appears in the diff, that is a scope violation — revert it.

- [ ] **Step 3: Manual acceptance (not merge-blocking)**

On a machine with all three CLIs:

1. `hcom hooks add claude && hcom hooks add cursor && hcom hooks add antigravity`
2. `hcom hooks status` — all three report installed, no double-fire warning.
3. Confirm `~/.claude/settings.json` has no hcom hook entries left, and still has any `agentpet-hook` / `rtk hook` / `herdr-agent-state.sh` entries it had before.
4. Launch one agent of each kind, send each a message, end each session.
5. `grep -c '"event":"sessionend"' ~/.hcom/.tmp/logs/hcom.log` — one sessionEnd per instance. No Cursor instance shows `hook=poll`. The `cursor.sessionend.ignored` + `sessionend` pair from the `sage` run must not reappear.
6. `hcom hooks remove claude` then launch Claude — expect the warning, and expect the agent to start anyway.

No commit unless a step produced a fix.

---

## Out of scope (do not implement)

- Any change to a `handle_*` function or to `src/router.rs`
- `agy-*` subcommands (Antigravity keeps `gemini-*` + `ANTIGRAVITY_AGENT=1`)
- Moving Codex, Gemini, Kimi, or Copilot onto plugins
- The `finalize_session` guard from the previous change set — decide after measuring double-fire in production
- Preventing `agy plugin import claude` from importing Claude hooks into Antigravity
