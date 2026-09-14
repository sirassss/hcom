# Standalone `hcom` skill vs plugin `hcom-agent-messaging` — keep / drop review

**Status:** consensus reached (ligo · bono · hana · sumo, unanimous, 2026-09-10) — awaiting siras approval to execute
**Date:** 2026-09-10
**Decision owner:** siras

---

## Decision

**One skill source = plugin `hcom-agent-messaging` (this repo).** Retire the standalone `~/.claude/skills/hcom` skill and delete the `agent-skill/hcom` skill dir. **No** thin leftover policy skill — a second skill with overlapping triggers recreates the contradiction bug.

Machine checks backing this:
- `hcom` is on `PATH` (`~/.local/bin/hcom`) → drop the `uvx` prefix everywhere.
- `hcom hooks add` covers **Codex delivery hooks** 100% (`~/.codex/hooks.json` has SessionStart / UserPromptSubmit / Pre+PostToolUse / Stop); `hcom codex` injects the bootstrap via `developer_instructions` (`src/bootstrap.rs`). It does **not** teach *vanilla* Codex the skill → `~/.codex/AGENTS.md` must stay as a pointer.
- All four bootstraps currently point at the standalone dir: `~/.claude/HCOM.md`, `~/.codex/AGENTS.md`, `~/.cursor/rules/hcom.mdc`, AGY `GEMINI.md`.

**All rule Verdicts = the "Suggested" column below, with these deltas:**

| Rule | Delta |
|---|---|
| A1 | drop `uvx`, standardise on `hcom` |
| B6 | keep **reframed**: `--thread` = script mode only; interactive = tag only |
| E2–E5 | keep, but write conditionally: *"if backend is herdr (default on this host)…"* — don't assume every backend is herdr, don't bury it |
| F2 | keep **scoped**: never stop/kill *user-spawned* agents without explicit go-ahead; script-owned ephemeral agents may `hcom kill` themselves (trap); interactive agents never kill peers |
| D1 | fold into section C, don't keep standalone |
| H4 | **keep** `~/.cursor/rules/hcom.mdc` as a Cursor rule (`alwaysApply`) — it covers PTY dual-UUID / sessionEnd / 15s stop-timeout that plugin `hooks.json` does not. Repoint the "read SKILL.md" path only. Not a second skill. |
| H5 | `CHEATSHEET.md` is human/zsh (`hcsp`) — keep in `agent-skill/dotfiles` or a human README, **do not** dump into the agent `SKILL.md` |

**Do not merge** the plugin's curl-installer / "run `hcom hooks add` if missing" block as the default path — this host is already installed via plugin + `PATH`.

**Canonical path for the bootstraps (H1–H4) to reference:** the live repo file `skills/hcom-agent-messaging/SKILL.md`. Do **not** point them at `~/.claude/plugins/cache/…` or the marketplace clone (`~/.claude/plugins/marketplaces/hcom/…` — a `sirassss/hcom` git checkout that lags until a plugin update). Do **not** put the pointer under `~/.claude/skills/` (that dual-loads it as a skill again — the exact bug we're removing). Recommended: a symlink we own, `~/.hcom/SKILL.md` → the live repo skill; H1–H4 all read that.

---

## Why this doc

Two hcom skills are active on this machine at once:

| | Standalone `hcom` | Plugin `hcom-agent-messaging` |
|---|---|---|
| Path | `~/.claude/skills/hcom` → symlink to `/home/alam/workspaces/agent-skill/hcom` (repo `sirassss/agent-skill`) | `skills/hcom-agent-messaging/` in this repo (`sirassss/hcom`, fork of `aannoo/hcom`), installed as plugin `hcom@hcom` |
| Content | 1× `SKILL.md` (172 lines) + `CHEATSHEET.md` + per-vendor bootstrap stubs (`claude/HCOM.md`, `codex/AGENTS.md`, `antigravity/GEMINI.md`, `cursor/hcom.mdc`) | `SKILL.md` + `references/` (patterns, cross-tool, gotchas, script-template) + `references/scripts/` (6 working `.sh`) |
| Also ships | nothing else | plugin hooks for claude / cursor / antigravity (`hooks.json`, `hooks-cursor.json`, `hcom-agy/hooks/hooks.json`) — **no Codex hook** |

Both have near-identical trigger descriptions (`hcom`, `spawn agent`, `multi-agent`, `send message`), so the model may load either. Where they overlap they sometimes **contradict**.

**Goal of this review:** decide, rule by rule, what from the standalone `SKILL.md` should be merged into `skills/hcom-agent-messaging/` in this repo, what should be dropped as already-covered, and what should stay behind as separate wiring. After the decision, the standalone skill gets retired or reduced to bootstrap stubs only.

---

## The lens to evaluate through

Almost every conflict below is the **same axis**:

- **Plugin skill + `references/`** are written for **headless script orchestration** — `hcom run <script>`, `--headless`, `--go`, agents spawned and killed inside one autonomous workflow, multiple workflows possibly sharing the bus.
- **Standalone `SKILL.md`** is written for **interactive human-in-the-loop coordination** — the user watches agents as visible herdr tabs and tells an agent what to do; one workflow at a time; agents outlive the turn that spawned them.

When judging each rule, ask: *is this a genuine contradiction, or two correct answers for two different modes?* Most are the latter — which argues for a merged skill with an explicit "interactive mode / script mode" split rather than picking a winner.

### `--thread` vs `tag` (the most cited "conflict")

Not alternatives — orthogonal:

| | `tag` | `--thread` |
|---|---|---|
| Set at | spawn time; property of the agent for its whole life | per `send` / `events` call; property of the message |
| Purpose | identity + address a whole group (`@tag-`) | isolate one workflow's message stream from another's on a shared bus |
| Miss it and… | agent has no group | receiver sees nothing (both sides must pass the exact same value) |

Headless scripts use **both**. Interactive coordination needs only `tag` — `--thread` is pure overhead when there's one human driving one workflow. Standalone's "don't use `--thread` for rooms" is correct *for its mode*; the references' heavy `--thread` use is correct *for theirs*.

---

## Rule-by-rule

Legend for **Plugin?**: ✅ covered · ⚠️ partial / different framing · ❌ absent
Legend for **Type**: `NEW` (plugin lacks) · `CONFLICT` (plugin says otherwise) · `DUP` (plugin covers)

Reviewers: fill **Verdict** (`keep` / `drop` / `keep-as-wiring`) and **Notes**.

### A. Invocation

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| A1 | Command is `uvx hcom` (not bare `hcom`) | ⚠️ uses `hcom` | CONFLICT (cosmetic) | drop — align on `hcom`; keep `uvx` only if there's a real reason | | |

### B. Tags / rooms

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| B1 | Never spawn without a tag | ❌ (tags optional) | NEW | keep | | |
| B2 | Use `HCOM_TAG=<room>` per-command prefix | ⚠️ README shows both env + `hcom config tag` | NEW (stricter) | keep | | |
| B3 | Never use `hcom config tag <name>` for coordination (writes global `config.toml`, tags every later spawn until cleared) | ❌ README presents it as fine | CONFLICT | keep — real footgun | | |
| B4 | Addressing: `@tag-` = room, `@tag-name` = one agent, `@a-x @b-y` = cross-room | ✅ | DUP | drop | | |
| B5 | No tag / user names a room → pick a slug and tell the user which; reuse existing tag when adding to a room | ❌ | NEW (minor) | keep — cheap, helpful | | |
| B6 | Don't use `--thread` for rooms | ⚠️ references use `--thread` everywhere | CONFLICT (mode split) | keep, reframed: `--thread` = script mode only; interactive = tag only | | |

### C. Coordinator flow — join room before talking

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| C1 | Spawn or send alone does **not** join you to the room; others can't reply to you until you `start` with the same tag | ❌ | NEW | keep — high value, common failure | | |
| C2 | 6-step order: pick tag → open your agent tab → `hcom start --as <name>` inside it → verify `hcom list` shows you `listening` → spawn others same tag → send | ⚠️ "run `hcom start` inside the tool" only | NEW | keep | | |
| C3 | `hcom start` must run inside the agent tab; run from a throwaway shell that exits → identity goes `stale_cleanup`, hooks never poll it | ⚠️ implied, not stated | NEW | keep | | |
| C4 | **Claude Code:** after `hcom start`, end the turn — binding completes only when the Stop hook fires; working past it → silent `launch_failed`, nothing delivered | ❌ | NEW | keep — critical | | |
| C5 | Don't paper over C4 with a `sleep` + `hcom events` poll loop (busy-poll anti-pattern) | ⚠️ "never use sleep" (script context) | CONFLICT (framing) | keep — merge with the references' no-sleep rule | | |
| C6 | Reading (`hcom list`, `transcript`) needs no identity; pulling a transcript is not bus delivery and doesn't replace joining | ⚠️ | NEW (minor) | keep | | |
| C7 | Anti-patterns list: `start` outside the tab; `@tag-x` before joining same tag; different tag on `start` vs `spawn` | ❌ | NEW | keep | | |

### D. Join & talk

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| D1 | `hcom start` makes the Stop hook block on `hcom poll` each turn (up to `HCOM_TIMEOUT`, default 300s) | ⚠️ | DUP-ish | drop or fold into C | | |
| D2 | `hcom start --as <name>` to reclaim a name after `/clear`, `/compact`, resume | ⚠️ "binding happens on first message/prompt" | NEW (minor) | keep | | |
| D3 | `send ... -- <msg>` — everything after `--` is the message, no quotes | ✅ (send --help) | DUP | drop | | |
| D4 | `--reply-to <id> --intent ack|request|inform` | ✅ | DUP | drop | | |

### E. Spawn

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| E1 | `HCOM_TAG=<room> uvx hcom [N] <tool>` — full tool list; `agy --dir <path> --hcom-prompt` | ✅ | DUP | drop | | |
| E2 | Default = **no `--headless`**: backend is herdr, opens each agent as a visible tab in the currently-focused workspace | ❌ (references default to `--headless`) | CONFLICT (mode split) | keep — interactive default; note scripts differ | | |
| E3 | Same project → spawn as-is. Different project → `herdr workspace create --cwd <path> --label <name> --focus` first | ❌ | NEW | keep | | |
| E4 | Only `--headless` when the user explicitly asks | ❌ | CONFLICT (mode split) | keep, scoped to interactive mode | | |
| E5 | Agent `■ blocked` (needs approval) → surface its pane: `hcom list --json` → `directory` → match `cwd` in `herdr agent list` → `pane_id` → `herdr agent focus` | ❌ | NEW | keep — herdr-specific, no equivalent | | |

### F. Stop / kill

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| F1 | **Never stop/kill an agent without the user's explicit go-ahead** | ❌ | CONFLICT | keep | | |
| F2 | Do **not** follow the upstream "stop agents once the task is done" advice on its own; a `listening` agent is idle and costs nothing until a message reaches it. Report "done and idle", wait for the user | ❌ references say "always use `hcom kill` for cleanup", prompts end with "Then: hcom stop" | CONFLICT | keep — deliberate override; scope it to user-spawned agents, leave scripts free to clean up their own ephemeral agents | | |
| F3 | `stop` vs `kill` semantics (stop = leave bus, agent lives; kill = terminate + close pane); `stop tag:<room>` / `stop <tag>-<name>` / `kill tag:<room>` | ✅ | DUP | drop | | |

### G. Housekeeping

| # | Rule | Plugin? | Type | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| G1 | `hcom status` / `hcom hooks` / `hcom transcript <tag>-<name>` | ✅ | DUP | drop | | |
| G2 | Inside a sandbox, prefix everything with `HCOM_DIR=$PWD/.hcom` | ✅ | DUP | drop | | |

### H. Per-vendor bootstrap files (not `SKILL.md` rules — wiring)

| # | Artifact | What it does | Plugin equivalent? | Suggested | Verdict | Notes |
|---|---|---|---|---|---|---|
| H1 | `claude/HCOM.md` (`~/.claude/HCOM.md` symlink; loaded by global `CLAUDE.md` via `@HCOM.md`) | points Claude at the skill | plugin skill auto-loads for Claude | keep-as-wiring or drop — repoint `@HCOM.md` at the plugin skill, or delete | | |
| H2 | `codex/AGENTS.md` | tells Codex to read the skill guide | **none** — plugin ships no Codex hook or bootstrap | keep-as-wiring — this is the one load-bearing reason the standalone dir can't fully disappear | | |
| H3 | `antigravity/GEMINI.md` | AGY bootstrap stub | `hcom-agy` plugin ships AGY hooks | keep-as-wiring or drop — confirm `hcom-agy` covers the prompt side | | |
| H4 | `cursor/hcom.mdc` (1.4K Cursor rule) | Cursor-side guidance | plugin ships `hooks-cursor.json` (hooks, not prompt rules) | review — hooks ≠ prompt guidance; may still be needed | | |
| H5 | `CHEATSHEET.md` (96 lines) | command quick-ref | partly overlaps `references/` | review — fold useful bits into a `references/cheatsheet.md`, drop the rest | | |

---

## Open questions for reviewers

1. **One skill or two?** Merge everything into `hcom-agent-messaging` with an explicit *interactive mode* vs *script mode* split — or keep a thin standalone policy skill that `references:` the plugin for the script/knowledge layer?
2. **Mode default.** If merged: does the skill default to interactive (visible herdr tabs, don't-kill) and treat headless/scripts as the opt-in exception, or the reverse?
3. **F1/F2 scope.** "Never kill without go-ahead" clearly applies to user-spawned agents. Should it also constrain script-spawned ephemeral agents, or are those explicitly exempt (the script owns their lifecycle)?
4. **Codex (H2).** Is `codex/AGENTS.md` still the mechanism, or has Codex bootstrap moved into `hcom hooks add`? If the latter, the standalone dir can be deleted outright.
5. **`uvx hcom` (A1).** Any reason to keep the `uvx` prefix, or standardise on `hcom`?
6. **herdr coupling (E2–E5).** These assume herdr is the terminal backend. Should they be conditional ("if backend is herdr…") so the skill still works with other backends?

### Agreed answers (2026-09-10, unanimous)

1. **One skill** — the plugin. Merge the interactive policy into `skills/hcom-agent-messaging/SKILL.md` with an interactive-vs-script note. No second policy skill (overlapping triggers = the bug).
2. **Default interactive** at the top of `SKILL.md`. Script/headless mode is opt-in (`hcom run`, `--headless`) with its defaults kept in `references/`, not the top of the file.
3. **Scripts exempt.** User-spawned agents: never stop/kill without explicit go-ahead. Script-owned ephemeral agents: the script may `hcom kill` them itself (trap). Interactive agents never kill peers.
4. **Moved into `hcom hooks add`** for delivery hooks + `hcom codex` bootstrap injection. Vanilla/bare Codex still can't learn the skill on its own, so `~/.codex/AGENTS.md` stays as a real-file pointer at the canonical `SKILL.md`. With that repointed, the standalone dir is deleted.
5. **Standardise on `hcom`.** Drop `uvx`.
6. **Make E2–E5 conditional:** *"if backend is herdr (default on this host)…"*.

---

## Approved merge plan (pending siras execute-approval)

Order matters — do **not** delete anything until the wiring is repointed and verified.

1. Add an **Interactive coordination** section to `skills/hcom-agent-messaging/SKILL.md` carrying: B1–B3, B5, B6 (reframed), C1–C7, D2, E2–E5 (herdr-conditional), F1–F2 (scoped). ~40–50 lines, at the top; script defaults stay in `references/`.
2. Fold C5 into the references' existing "never use `sleep`" rule as one statement covering both modes.
3. Mark the `references/` + scripts as **script mode**; note where its defaults (`--headless`, `--thread`, `hcom kill` cleanup) deliberately differ from interactive mode.
4. Drop all `DUP` rows (A1, B4, D1, D3, D4, E1, F3, G1, G2). Do not import the curl-installer / "`hcom hooks add` if missing" block as the default path.
5. Create the canonical pointer: `~/.hcom/SKILL.md` → live repo `skills/hcom-agent-messaging/SKILL.md` (not under `~/.claude/skills/`).
6. Repoint the four bootstraps at `~/.hcom/SKILL.md`, replacing symlinks-into-`agent-skill` with real content where needed:
   - H1 `~/.claude/HCOM.md`
   - H2 `~/.codex/AGENTS.md` — real file, pointer for vanilla Codex only
   - H3 AGY `GEMINI.md` — confirm `hcom-agy` covers the prompt side
   - H4 `~/.cursor/rules/hcom.mdc` — keep the Cursor-specific quirks, change only the `SKILL.md` path
7. Move `CHEATSHEET.md` (H5) to `agent-skill/dotfiles` or a human README.
8. Verify: `hcom list` / delivery still work for each vendor; no dangling symlinks into `agent-skill/hcom`.
9. Only then: delete the `agent-skill/hcom` skill dir (SKILL.md, CHEATSHEET, per-vendor stubs) and remove the `~/.claude/skills/hcom` symlink.

Execution touches cross-vendor wiring on the host → run it as an agent-ops issue (task → cross-vendor verify → settle), not an inline edit.
