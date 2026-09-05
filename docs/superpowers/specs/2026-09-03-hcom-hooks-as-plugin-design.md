# Ship hcom hooks as tool plugins instead of writing global config

**Date:** 2026-09-03
**Prior art:** `docs/superpowers/specs/2026-08-29-cursor-cli-sessionend-and-idle-followup-design.md` (the Cursor fix whose acceptance run exposed this)
**Status:** Design approved; plan at `docs/superpowers/plans/2026-09-03-hcom-hooks-as-plugin.md`; revised 2026-09-03 after the Task 1 spike measured three assumptions to be wrong

**Scope:** How hcom *installs* its hooks for Claude Code, Cursor, and Antigravity (agy). Handler code in `src/hooks/*.rs` is untouched. Codex, Gemini, Kimi, Copilot keep their current install path. Pi, Oh My Pi, OpenCode already ship plugins and are out of scope.

---

## Problem

hcom writes its hooks into each tool's **shared global config**: `~/.claude/settings.json`, `~/.cursor/hooks.json`, `~/.codex/hooks.json`, `~/.gemini/settings.json`. That file is not private to the tool that owns it. Coding agents import each other's profiles, so one config feeds several harnesses.

Measured on 2026-09-03 while accepting the Cursor sessionEnd fix. A single Cursor CLI agent (`sage`, `tool=cursor`, PID 27250) ran **both** hook sets for its entire life:

| Source | Hooks that fired on `sage` |
|---|---|
| `~/.cursor/hooks.json` | `cursor-sessionend`, `cursor-stop`, … |
| `~/.claude/settings.json` | `post` ×40, `pre` ×16, `poll` ×16, `userpromptsubmit` ×15, `sessionstart` ×1, `sessionend` ×1 |

At 14:08:27Z two sessionEnd handlers ran within the same second:

```
cursor.sessionend.ignored  instance=sage reason=completed   ← the new Cursor fix: does not unregister
sessionend                 instance=sage reason=completed   ← finalize_session (common.rs): DELETES the instance
```

The instance still died with `exit:completed by:session`. The fix landed in `src/hooks/cursor.rs` was correct and had no effect on the outcome, because the Claude path is a second, unguarded door into the same lifecycle. The same cross-import also means `handle_stop`'s new `followup_message` path never executed — delivery went through Claude's `hook=poll` instead.

Guarding `finalize_session` would close this one instance of the problem. It would not close the class: every future hcom hook lands in a file that other harnesses read, and every tool-specific guard has to be rediscovered the same way this one was — by reading logs after a wrong outcome.

## Goals

1. A hook hcom installs for tool X runs **only** under tool X, unless a human explicitly imports it elsewhere.
2. hcom stops writing hook entries into config files shared with other tools.
3. Existing installs migrate without a window in which no hooks are active.
4. Hook handler code, event names, and payload contracts are unchanged.
5. hcom never installs anything the user did not ask for. Launching an agent reports what is missing; it does not fix it.

## Non-goals

- Changing any handler in `src/hooks/*.rs`.
- Moving Codex, Gemini, Kimi, or Copilot onto plugins.
- Preventing a user who runs `agy plugin import claude` from importing hcom's Claude hooks into Antigravity. That is a deliberate act with a visible command; the goal is that nothing cross-fires *without* one.
- Re-litigating the `finalize_session` guard. Measure double-fire after this ships, then decide.
- A unified cross-tool hook schema. The tools disagree and we do not control them.
- Publishing to third-party marketplaces (Codex's plugin index is a separate repo and a separate job).

---

## Evidence: plugins are isolated, config files are not

Verified on this machine, not assumed:

| Mechanism | Claude Code runs it | Cursor runs it |
|---|---|---|
| `~/.claude/settings.json` hooks | yes | **yes** — the defect |
| Plugin hooks via `enabledPlugins` | yes | **no** |
| Plugin skills / MCP servers | yes | yes (harmless) |

`ponytail`, `remember`, and `episodic-memory` are enabled through `enabledPlugins` and appear in no `settings.json` hook entry. A Claude Code session receives their injected `SessionStart` text; `sage`'s Cursor transcript contains none of it, while still reading skills from the same plugin directory. Plugin hooks are scoped to the harness that enabled them.

Antigravity is the exception that proves the rule. `agy plugin import <gemini|claude>` copies another harness's plugins into `~/.gemini/config/plugins/<name>/`, hooks included — this machine holds `superpowers` imported from `gemini-cli` with `components: ["skills","hooks"]`. Cross-import survives the move to plugins, but it becomes a command a human runs on purpose rather than a file another harness reads behind their back. That is the difference the design buys.

## Measured constraints (2026-09-03 spike, `src/hooks/plugin.rs`)

A probe plugin was installed into all three tools and removed. Three results contradicted the first draft of this design and are load-bearing:

| | Claude Code | Cursor | Antigravity |
|---|---|---|---|
| Install from a local path | yes | **no** — remote git URL only | yes |
| Non-interactive install | yes | **no** — `/plugins` in the TUI | yes |
| Hook file read | `hooks/hooks.json` | the declared `hooks` key | **`hooks/hooks.json`** |
| Descriptor read | `.claude-plugin/plugin.json` | `.cursor-plugin/plugin.json` | **`.claude-plugin/plugin.json`** |
| Enabled marker | `enabledPlugins` in `settings.json` | not measurable without an install | `config/import_manifest.json` |

1. **Antigravity reads the same `hooks/hooks.json` that Claude does.** Three ways to give it a separate file were tried and all failed: a `hooks.json` at the plugin root reports `hooks: skipped (not found)`; a `"hooks"` key in `gemini-extension.json` is ignored; and deleting `gemini-extension.json` entirely still installs, because Antigravity reads `.claude-plugin/plugin.json`. One plugin directory cannot carry different hooks for Claude and Antigravity — installing hcom's Claude plugin into Antigravity would make it run `hcom sessionstart` / `hcom poll` / `hcom sessionend`, which is the exact defect this design exists to remove, relocated rather than fixed.
2. **Cursor cannot install a plugin from the CLI.** `cursor-agent plugin` exposes only `marketplace`; installation happens in the interactive `/plugins` picker.
3. **Cursor marketplaces must be remote git URLs.** A local path is coerced into `https://<first segment>.git` and fails DNS, so `dev_root` cannot drive a Cursor install.

**Unmeasured, and load-bearing for Windows.** `hook_sh_cmd` (`src/hooks/antigravity.rs`) emits two different command syntaxes: a POSIX `sh -c '…'` form, and under `cfg!(windows)` a `cmd.exe` form (`where … && (set "ANTIGRAVITY_AGENT=1" && …) || exit /b 0`). A committed manifest is one static file and cannot carry both, and the spike measured no per-platform discovery mechanism in any of the three tools. Claude documents that it runs hook commands through a POSIX shell on every platform (Git Bash on Windows); Antigravity's behavior is unknown and was not measured.

**The routing has since switched** (`src/tool.rs` now sends Antigravity to the plugin installer), so this is live rather than hypothetical, and it was not measured first as this section originally asked. The failure would be loud rather than fail-open: if Antigravity spawns hook commands through `cmd.exe`, `sh` is not found and the hook errors instead of exiting 0, inverting the contract every manifest here upholds.

Two things narrow the exposure. `install_agy_plugin` requires a local checkout (`agy plugin install` takes a directory, not a URL), so a Windows user with no `dev_root` gets an actionable error and never reaches the manifest. And nothing installs as a side effect any more, so no one arrives here without having typed `hcom hooks add antigravity`. **Still unresolved:** a Windows contributor with `dev_root` set would install a manifest that may not run. Measure on a Windows host before release; if Antigravity needs `cmd.exe`, keep the legacy writer for Antigravity on Windows.

One assumption held: Cursor resolves a plugin declared in a repo subdirectory (`"source": "./plugin/hcom"`), so the plugin body stays where it is.

**Cursor's plugin registry is separate from Claude's, and it is not local.** `cursor-agent plugin marketplace list` describes its own output as marketplaces *"visible to this account"*, and no local file on the test machine contains the entries it lists — the registry lives server-side per Cursor account. The two registries also hold genuinely different sets (Cursor: `thedotmack`, `ecc`, `claude-code-warp`; Claude: `agent-ops-dev`, `superpowers-marketplace`), so the overlap is a human adding the same marketplace twice, not an import.

Two consequences: installing the plugin for Claude does **not** give Cursor anything, so `hcom hooks add cursor` remains the only route for Cursor; and `cursor-agent plugin marketplace add` mutates account state rather than a file, so it cannot be undone by deleting anything under `~/.cursor` — `hcom hooks remove cursor` must call `cursor-agent plugin marketplace remove`.

## Prior art: obra/superpowers

superpowers ships one repo to five harnesses. What it does, and what we take:

- **Separate manifest per tool**, at repo root: `.claude-plugin/plugin.json`, `.cursor-plugin/plugin.json`, `.codex-plugin/plugin.json`, `.opencode/plugins/*.js`, `gemini-extension.json`. One `.claude-plugin/marketplace.json` serves both Claude and Cursor.
- **Separate hook file per tool**, because the schemas differ and cannot be merged:

  | | Claude `hooks/hooks.json` | Cursor `hooks/hooks-cursor.json` |
  |---|---|---|
  | Event case | `SessionStart` | `sessionStart` |
  | Envelope | `matcher` + nested `hooks[]` with `type`/`async` | flat `[{command}]`, top-level `"version": 1` |
  | Command path | `${CLAUDE_PLUGIN_ROOT}/…` | `./hooks/…` (relative) |
  | Declared | by convention | explicitly: `"hooks": "./hooks/hooks-cursor.json"` |

- **Cursor plugin hooks are real** — the explicit `hooks` key above is what settles it.
- **Codex plugin manifests carry `skills` and `interface`, no `hooks`.** Codex cannot receive hooks this way.
- superpowers uses one shell entry point and branches on `CURSOR_PLUGIN_ROOT` / `CLAUDE_PLUGIN_ROOT` / `COPILOT_CLI`, noting in-source that **Cursor may set `CLAUDE_PLUGIN_ROOT` too**, so order matters. We do not copy this: hcom is a binary with per-tool subcommands already, so it can put the tool identity in the manifest instead of sniffing it at runtime.

---

## Decisions

| Topic | Choice |
|---|---|
| Isolation layer | Declaration, not runtime. Each tool's hook file names that tool's subcommand. No env sniffing, no payload heuristics. |
| Entry point | Existing subcommands unchanged: `hcom sessionend` / `hcom poll` for Claude, `hcom cursor-sessionend` / `hcom cursor-stop` for Cursor, `hcom gemini-*` for Antigravity. |
| Antigravity's shared subcommand | Antigravity keeps calling `gemini-*` with `ANTIGRAVITY_AGENT=1` written into the manifest's command string. The env var is part of the declaration, not something hcom infers at runtime, so it stays inside the isolation rule. Adding `agy-*` subcommands would mean touching the router and carrying aliases for old installs; not worth it. |
| Scope | Claude, Cursor, Antigravity. |
| Install trigger | **Only an explicit `hcom hooks add <tool>`.** No command installs anything as a side effect. |
| Launch behavior when not installed | Warn and continue. `hcom claude` / `hcom cursor-agent` / `hcom agy` print what is missing and the exact command to fix it, then launch the agent anyway. Never install, never block. |
| Plugin layout | **Two plugin directories.** `plugin/hcom/` carries Claude's `hooks/hooks.json` plus Cursor's declared `hooks/hooks-cursor.json`; `plugin/hcom-agy/` carries Antigravity's `hooks/hooks.json`. Antigravity reads the same conventional path as Claude, so separate directories are the only way to give them different hooks. |
| Install mechanism | The tool's own CLI where one exists: `claude plugin marketplace add` + `claude plugin install`, `agy plugin install <dir>`. **Cursor has no CLI install**, so `hcom hooks add cursor` adds the marketplace and then prints the one manual step (`/plugins` inside Cursor). hcom does not hand-write plugin registration for any tool. |
| Cursor's manual step | Because the install completes outside hcom, verify cannot pass in the same command — so `hcom hooks add cursor` **does not strip Cursor's legacy hooks**. The strip happens on a later `hcom hooks add cursor` (or `hcom hooks status`) once verification sees the plugin. Until then Cursor keeps working on `~/.cursor/hooks.json`. |
| Verify mechanism | Read files. Verify runs before every spawn; shelling out to a CLI there is too slow. |
| Migration | Plugin wins. Remove hcom's own legacy entries — **only after** the plugin verifies. |
| Ordering | install → verify → remove legacy. Never remove first. |
| Legacy entries by others | Preserved untouched (`agentpet-hook`, `rtk hook`, `herdr-agent-state.sh`, anything else). |
| Marketplace source | Claude and Antigravity: local repo path when `dev_root` is set, otherwise the `repository` URL declared in `plugin/hcom/.claude-plugin/plugin.json` (today `https://github.com/aannoo/hcom`), so a fork that edits that field installs from itself. Cursor: always the remote URL — it rejects local paths. Local Cursor development uses `cursor-agent --plugin-dir <path>` instead. |
| Uninstall | `hcom hooks remove <tool>` removes the plugin and does not restore legacy entries. |

---

## Architecture

```
plugin/hcom/                      installed by Claude (marketplace) and Cursor (marketplace)
  .claude-plugin/plugin.json      exists; unchanged
  .cursor-plugin/plugin.json      new: "hooks": "./hooks/hooks-cursor.json"
  hooks/
    hooks.json                    new: Claude schema  ← Claude reads this by convention
    hooks-cursor.json             new: Cursor schema  ← Cursor reads this by declaration
  skills/                         exists (symlink to ../../skills)

plugin/hcom-agy/                  installed by Antigravity (agy plugin install <dir>)
  .claude-plugin/plugin.json      new: name "hcom" — Antigravity reads this descriptor
  hooks/
    hooks.json                    new: Antigravity schema ← same conventional path, different content
```

Root `.claude-plugin/marketplace.json` already points at `./plugin/hcom` and is reused by Claude and Cursor. Antigravity does not go through a marketplace at all; `agy plugin install` takes a directory, so it is pointed straight at `plugin/hcom-agy/`.

The split exists for exactly one reason: Claude and Antigravity both read `hooks/hooks.json` and neither offers a way to redirect it. Cursor is the tool that *can* declare its own path, which is why it shares a directory with Claude instead of needing a third.

| Tool | Descriptor read | Hook file read | Event names |
|---|---|---|---|
| Claude Code | `.claude-plugin/plugin.json` | `hooks/hooks.json` (convention) | `SessionStart`, `Stop`, `SessionEnd`, … |
| Cursor | `.cursor-plugin/plugin.json` | `hooks/hooks-cursor.json` (declared) | `sessionStart`, `stop`, `sessionEnd`, … |
| Antigravity | `.claude-plugin/plugin.json` | `hooks/hooks.json` (convention) | `PreInvocation`, `PostInvocation`, `PostToolUse`, … |

Each manifest must also carry the per-tool fields hcom's own verifier demands, not just the events. Cursor's `verify_hooks_at` (`src/hooks/cursor.rs`) rejects a `stop` entry that lacks `"loop_limit": null` alongside its 30s timeout, so a manifest missing that field would be judged not-installed by hcom itself. The manifest tests assert these fields rather than assuming the event list is the whole contract.

Antigravity's envelope matches Claude's shape (`matcher`, nested `hooks[]` with `type`/`command`) but its **event names are its own** — the current `hcom-lifecycle` group in `~/.gemini/config/hooks.json` uses `PreInvocation`/`PostInvocation`, which have no Claude equivalent. The manifest reuses the envelope, never the event table.

Both directories declare the plugin name `hcom`, so each tool installs it under that name in its own registry.

Every hook command stays fail-open, and gains a fallback:

```
cmd=${HCOM:-hcom}; command -v "${cmd%% *}" >/dev/null 2>&1 || cmd="uvx hcom"; command -v "${cmd%% *}" >/dev/null 2>&1 && exec $cmd sessionend || exit 0
```

A missing binary exits 0 and never blocks the user's turn. `dev_root` re-exec still applies: it happens inside the router, after the binary starts.

**Why the fallback exists.** `try_setup_claude_hooks` writes `env.HCOM` into `settings.json` — set to `uvx hcom` whenever hcom's binary lives under a `uv` path (`HCOM_PREFIX`, `src/runtime_env.rs`). A plugin manifest cannot set an environment variable, so `${HCOM:-hcom}` degrades to bare `hcom`. Someone who runs hcom only through `uvx`, with no `hcom` on PATH, would hit `command -v hcom` failing and every hook exiting 0 — hcom silently disabled, no error anywhere. Resolving `uvx hcom` inside the command keeps one manifest correct for every install shape and keeps hcom out of the user's config files. The cost is a `uvx` resolution per hook for those users, which is what they already pay today via `env.HCOM`.

The change lands in `build_hook_entry_command` itself, so the legacy `settings.json` path and the plugin manifest emit byte-identical strings; `plugin::tests` compares the manifest against that function, which makes the shared source of truth enforced rather than assumed.

### Manifest placement is a measured question, not an assumption

superpowers' repo root *is* its plugin directory. hcom's plugin lives in `plugin/hcom/`, so where Cursor looks for `.cursor-plugin/plugin.json` under a subdirectory source is unverified. The first implementation task resolves this empirically — `cursor-agent plugin marketplace add <local path>`, then inspect what Cursor resolved — and places the manifest where Cursor actually reads it. If Cursor requires repo root, the manifest goes to root alongside `.claude-plugin/`, and the plugin body stays in `plugin/hcom/`.

---

## Components

### 1. Hook file generation

Three hook files, one per schema, replacing the `settings.json`/`hooks.json` writers for these three tools. Each keeps the event→subcommand mapping it has today; only the envelope changes. All three are committed to the repo and shipped inside the plugin, not generated at install time — which is what makes them reviewable in a diff.

### 2. `hcom hooks add <tool>` — the only thing that installs

```
Claude:  claude plugin marketplace add <source>
         claude plugin install hcom@hcom
         verify → strip legacy

AGY:     agy plugin install <repo>/plugin/hcom-agy
         verify → strip legacy

Cursor:  cursor-agent plugin marketplace add <remote gitUrl>
         print: "finish in Cursor: /plugins → install hcom"
         verify fails here by design → DO NOT strip legacy
```

A failure at any step leaves the machine exactly as it was, legacy hooks included, and reports what to run by hand. There is no state in which the agent has neither plugin nor legacy hooks.

Cursor is the asymmetric case: its install finishes in the TUI, outside hcom's control, so the first `hcom hooks add cursor` cannot complete the migration. Running it again later — or `hcom hooks status` — sees the plugin and strips the legacy entries then. Leaving `~/.cursor/hooks.json` in place in the meantime is the safe half of the trade: Cursor double-fires nothing, because its plugin hooks and its legacy hooks call the same `cursor-*` subcommands, and the worst case is one wasted hook invocation rather than a wrong handler.

### 3. Verify

Replaces `verify_claude_hooks_installed` and friends for the plugin path. File reads only, no subprocess: the plugin directory exists, and the tool records it as enabled (`enabledPlugins["hcom@hcom"]` for Claude; the equivalent per-tool marker for Cursor and Antigravity, confirmed by the same placement task).

Verify still runs before every spawn, but its false branch now **reports instead of repairing**.

### 3a. Launcher behavior — warn, never install

`hcom claude`, `hcom cursor-agent`, and `hcom agy` call verify. On failure they print one block and continue launching:

```
hcom hooks are not installed for claude.
Messages will not be delivered automatically this session.
  Install:  hcom hooks add claude
```

No install, no config write, no prompt, no blocked launch. The agent starts; it simply runs without hook-based delivery, which is the documented ad-hoc mode that already exists. The same warning surfaces in `hcom status` and `hcom hooks status`.

This is a deliberate break from the current behavior, where the launcher silently rewrote a stale `~/.cursor/hooks.json` on the next spawn (the migration trigger used for the 15s→30s timeout change). That convenience is what let a config change land without anyone deciding to make it. Upgrades are now visible: an existing machine keeps its legacy hooks and keeps working until someone runs `hcom hooks add`.

### 3b. Every install trigger, not just the launcher

Goal 5 says hcom installs nothing the user did not ask for. Honouring that meant finding every place that installs as a side effect, and the first pass missed one: `start_bare` in `src/commands/start.rs` auto-installs hooks when it detects an unmanaged ("vanilla") tool. Once the three tools route to the plugin path, that call shells out to their CLI and clones a marketplace over the network — from a command whose only job is to join the bus. Four existing tests caught it; one hung two minutes on a real clone.

`Tool::hooks_ship_as_plugin()` is the single source of truth for which tools this applies to, so a fourth call site cannot be fixed by remembering a list. Sites that install because the user explicitly asked — `hcom hooks add` — are unaffected.

### 4. Legacy entry removal

Matches only entries hcom wrote, by their `${HCOM:-hcom}; … exec $cmd <sub>` shape, and deletes those. Everything else in the file survives byte-for-byte. The existing `try_setup_*` writers are already idempotent and preserve foreign entries; this is the same discipline in reverse.

### 5. Status reporting

`hcom hooks status` and `hcom status` report plugin presence for Claude, Cursor, and Antigravity. Two conditions get named explicitly rather than shown as a bare `~`:

- **Not installed** — with the exact `hcom hooks add <tool>` to run. This is now the only way a user learns they need to act, since nothing self-repairs.
- **Plugin and legacy entries both present** — a machine mid-migration, or a config synced from another host. Flagged as a double-fire risk, pointing at `hcom hooks add <tool>` to clean up.

---

## Data flow

**Fresh install.** `hcom hooks add claude` → register → enable → verify passes → nothing legacy to strip → next Claude session loads hooks from the plugin.

**Upgrade of an existing machine.** User runs `hcom claude`. Verify fails, the launcher prints the warning above, and the agent launches on its existing legacy hooks — delivery keeps working exactly as before. Nothing changes until the user runs `hcom hooks add claude`, which installs the plugin and only then strips the legacy entries. Cursor sessions started after that point no longer inherit `hcom sessionend` or `hcom poll`.

**Install fails.** Register or enable errors. Legacy entries remain. hcom reports the failing command. `hcom status` shows the tool as not fully installed.

**User never migrates.** Legacy hooks stay. The cross-import defect stays with them. This is a supported state, not a broken one: hcom warns on every launch and never forces the change.

**Cursor session, post-migration.** Cursor reads `~/.claude/settings.json` as it always did and finds no hcom hooks there. It runs only `cursor-*` hooks, from `~/.cursor/hooks.json` or its own plugin. One sessionEnd, one handler.

---

## Error handling

| Situation | Behavior |
|---|---|
| Tool CLI absent from PATH | Do not touch legacy entries. Print the manual commands. |
| `marketplace add` / `install` fails | Same: leave the machine as found, report. |
| Plugin installed but disabled by the user | Verify fails; launch warns; status shows `~`. hcom does **not** re-enable it — the user disabled it on purpose. |
| Plugin missing at launch | Warn, launch anyway, no install. |
| Plugin and legacy entries both present | Status warns of double-fire and names the fix. |
| Legacy file is malformed JSON | Do not rewrite it. Report the path and leave it alone. |
| `hcom hooks remove` | Remove the plugin — for Cursor this means `cursor-agent plugin marketplace remove hcom`, since the registry is account state and deleting local files leaves it listed. Do not resurrect legacy entries. |
| hcom binary missing when a hook fires | `command -v` guard exits 0; the turn is unaffected. |
| `agy plugin import claude` pulled hcom's Claude hooks into Antigravity | Not preventable, and now materially worse than before: Claude's `hooks/hooks.json` sits at the exact path Antigravity reads, so the import lands Claude handlers on an Antigravity agent. `hcom status` reads `config/import_manifest.json` and warns when an `hcom` entry there has `source` other than the local install. |
| Cursor plugin added but never installed in the TUI | Verify fails; status shows the pending manual step; legacy hooks stay and keep working. |

---

## Testing

**Unit**

1. Claude hook file matches Claude's schema: PascalCase events, `matcher`, `${CLAUDE_PLUGIN_ROOT}`, nested `hooks[]`.
2. Cursor hook file matches Cursor's schema: camelCase events, `"version": 1`, relative command path, flat array.
3. Antigravity hook file keeps its own event names (`PreInvocation`, `PostInvocation`, `PostToolUse`, …) and carries `ANTIGRAVITY_AGENT=1` in every command string.
4. Every event maps to the same subcommand it maps to today — no event silently dropped in the move. Table-driven across all three tools.
5. Verify returns true only when both the plugin directory and the tool's enabled-marker are present; false for each missing half independently.
6. Legacy stripping deletes hcom entries and preserves foreign ones. Claude fixture must contain `agentpet-hook`, `rtk hook`, and a plain `bash …/herdr-agent-state.sh` entry; the Antigravity fixture must contain the sibling `agentpet` group key next to `hcom-lifecycle`. All survive.
7. Ordering: with the install step stubbed to fail, legacy entries are **not** removed.
8. Malformed legacy JSON: no write, no panic.
9. **Launcher never installs:** with verify stubbed false, launching an agent produces the warning, leaves every config file byte-identical, and still starts the agent. This is the regression guard for the behavior change and must fail if anyone reintroduces auto-install.

**Acceptance (manual, not merge-blocking)**

Launch a Claude agent, a Cursor agent, and an agy agent; exchange a message with each; end all sessions; then grep `~/.hcom/.tmp/logs/hcom.log`: each instance shows exactly one sessionEnd handler. The `cursor.sessionend.ignored` + `sessionend` pair from the `sage` run must not reappear, and no Cursor instance may show `hook=poll`.

---

## Alternatives considered

1. **Guard `finalize_session` on `tool == "cursor"`.** Smallest possible diff, fixes the observed instance. Rejected as the primary fix: it treats one symptom of shared config, leaves every other hook cross-firing, and has to be re-derived per tool pair. Still available as defense-in-depth once double-fire is measured post-migration.
2. **One plugin, one entry point, detect the tool from env vars** (superpowers' approach). Rejected: `CURSOR_PLUGIN_ROOT` and `CLAUDE_PLUGIN_ROOT` can both be set, so correctness depends on branch order in a contract nobody publishes. hcom already has per-tool subcommands and can encode the answer in the manifest.
3. **One plugin, one hook file listing every tool's event names.** Rejected on evidence: the schemas differ in envelope shape, not just key case, and Antigravity's event vocabulary (`PreInvocation`/`PostInvocation`) has no counterpart in the others. No single document is valid for all three.
4. **Separate plugins per tool (`hcom-claude`, `hcom-cursor`).** Rejected: N manifests to keep in sync and a user-visible chance of installing the wrong one, buying nothing over per-tool hook files inside one plugin.
5. **hcom writes `enabledPlugins` itself instead of calling the CLI.** Rejected: same failure mode we are leaving — hand-editing a config file whose format we do not own.
6. **Move all eleven tools to plugins now.** Rejected as scope: Codex plugins carry no hooks, Copilot already uses a private file (`~/.copilot/hooks/hcom.json`), and Pi/OMP/OpenCode are already plugins. Claude, Cursor, and Antigravity are where the cross-import lives.
7. **Keep the launcher's auto-install (today's behavior).** Rejected on the user's call, and the record supports it: that path silently rewrote `~/.cursor/hooks.json` during the timeout change, so a config edit shipped without anyone choosing it. Convenience here means hcom editing files on someone's machine as a side effect of starting an agent. The cost of dropping it is that migration needs one deliberate command, and machines that never run it keep the old defect — visibly, with a warning on every launch.
8. **Give Antigravity its own `agy-*` subcommands.** Rejected: it widens the change into `src/router.rs` and forces alias upkeep for installs that still call `gemini-*`, buying isolation that the manifest already provides by pinning `ANTIGRAVITY_AGENT=1` in the command string.

---

## File touch list (for the plan)

| File | Change |
|---|---|
| `plugin/hcom/hooks/hooks.json` | New: Claude hook manifest |
| `plugin/hcom/hooks/hooks-cursor.json` | New: Cursor hook manifest |
| `plugin/hcom/.cursor-plugin/plugin.json` | New: Cursor plugin descriptor |
| `plugin/hcom/.claude-plugin/plugin.json` | Unchanged — Claude finds `hooks/hooks.json` by convention |
| `plugin/hcom-agy/.claude-plugin/plugin.json` | New: descriptor Antigravity reads |
| `plugin/hcom-agy/hooks/hooks.json` | New: Antigravity hook manifest |
| `src/hooks/plugin.rs` | Measured paths and constants (landed in task 1) |
| `src/hooks/claude.rs` | Install/verify path only — no handler change |
| `src/hooks/cursor.rs` | Install/verify path only — no handler change |
| `src/hooks/antigravity.rs` | Install/verify path only — no handler change |
| `src/launcher.rs` | Verify branch warns instead of installing; auto-install removed for all three tools |
| `src/hooks/mod.rs` or a new `src/hooks/plugin.rs` | Shared install/verify/strip helpers |
| `skills/hcom-agent-messaging/references/cross-tool.md` | Document plugin-based install, and that hooks are never installed automatically |
| `README.md` | Install section: hooks now require an explicit `hcom hooks add <tool>` |
