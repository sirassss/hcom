---
name: hcom-agent-messaging
description: >
  Multi-agent communication for AI coding tools. Agents message, watch,
  and spawn each other across terminals. Use when setting up hcom,
  troubleshooting delivery, coordinating agents interactively, or writing
  multi-agent scripts. Triggers on "hcom", "spawn agent", "multi-agent",
  "agent-to-agent", "gửi cho agent", "hỏi agent khác".
---

# hcom — multi-agent communication for AI coding tools

AI agents running in separate terminals are isolated. hcom connects them via hooks and a shared database so they can message, watch, and spawn each other in real-time.

```bash
hcom claude       # or: hcom gemini, hcom codex, hcom opencode, hcom kilo, hcom pi, hcom omp, hcom agy, hcom cursor-agent, hcom kimi, hcom copilot
hcom              # TUI dashboard
```

Not installed yet? See `references/setup-troubleshooting.md`.

## interactive coordination (default mode)

Two modes, two sets of defaults.

**Interactive** — a human watches agents in visible terminal tabs and drives them; agents outlive the turn that spawned them. This is the default; everything in this section applies.

**Script** — `hcom run <script>`, autonomous workflows that spawn and clean up their own agents. Its defaults (`--headless`, `--thread`, `hcom kill` cleanup) deliberately differ and live in *workflow scripting* below plus `references/`.

A host may narrow either mode — which terminal backend is in use, whether a tag
is mandatory, who is allowed to kill an agent. Those conventions are not in this
file: read `~/.hcom/HOST.md` if it exists, and follow it where it is stricter.

### tags and threads are different things

An agent's tag is set at spawn time and is part of its name for life (`review-luna`);
`--thread` is per message and isolates one workflow's stream from another's on a
shared bus. Scripts use both. Interactive coordination uses the tag alone.

Addressing: `@tag-` (trailing hyphen) = whole room · `@tag-name` = one agent ·
`@a-x @b-y` = cross-room.

Never set the tag with `hcom config tag <name>` — it writes `~/.hcom/config.toml`
and tags **every** later spawn on this machine until cleared with `hcom config tag ""`.
Use the `HCOM_TAG=<room>` prefix, which is scoped to the one command.

### join the room before you talk

**Spawning or sending does not put you in the room.** Nobody can reply to you until you register on the bus under the same tag.

1. pick the tag
2. launch yourself in a terminal tab if you aren't in one: `HCOM_TAG=review hcom cursor-agent`
3. join, **inside that agent tab**: `HCOM_TAG=review hcom start --as ops` → you are `review-ops`
4. verify: `hcom list` shows you listening, not `(not participating)`
5. spawn the others with the same tag
6. send: `hcom send @review-claude --intent request -- task...`

`hcom start` must run inside the agent tab. From a throwaway shell that exits, the identity goes `stale_cleanup` and hooks never poll it.

**Claude Code, after `hcom start`: end the turn.** Tools *launched by* hcom bind on their first message or prompt (see *tool support*) — joining an already-running session with `hcom start` is a different path: it prints success immediately, but binding only completes when your Stop hook fires. Work past it and it silently goes `launch_failed` with nothing delivered. Don't paper over that with a `sleep` + `hcom events` poll loop — just end the turn.

`hcom start --as <name>` also reclaims your name after `/clear`, `/compact` or resume.

Reading (`hcom list`, `hcom transcript`) needs no identity — but pulling a transcript is not bus delivery and does not replace joining.

Anti-patterns: `hcom start` from outside the agent tab · sending `@tag-x` before joining that same tag · a different tag on `start` than on `spawn`.

### stop is not kill

| command | agent session | terminal tab | receives hcom |
|---------|---------------|--------------|---------------|
| `hcom stop` | keeps running | stays open | no |
| `hcom kill` | terminated | closed | no |

```bash
hcom stop tag:review        # whole room leaves the bus, agents keep running
hcom stop review-luna       # one agent
hcom kill tag:review        # terminate the room and close its panes
```

Who may use them is a matter of ownership, not of the task being finished. A
script may kill the agents it spawned — that is what its `trap` is for. Whether
an interactive agent may stop or kill anything is a local policy question; on
this host see `~/.hcom/HOST.md`.

---

## sending messages

Keep the body short and plain: a few sentences, no markdown headers/bullets/code
fences — the TUI pane shows raw text, so markdown just adds noise. Point to
files, commits, or issues by path/id instead of pasting content. Use the
language the room uses, with its correct spelling and diacritics.

Always set `--intent`: `request` = reply needed, `inform` = FYI, `ack` = receipt.

Everything else — full command syntax and flags — is in `hcom --help`.

---

## tool support

Delivery is automatic for every tool hcom launches: claude code (incl.
subagents), gemini cli (>= 0.26.0), codex, opencode, kilo code, antigravity,
cursor, kimi, copilot, pi, omp. Connect with `hcom <tool>`. Any other AI tool
works manually: `hcom start` inside the tool, then `hcom listen`. Per-tool quirks
are in `references/cross-tool.md`.

Session binding (`hcom transcript`, `hcom r/f` by session id) happens on first
message or first prompt for all hcom-**launched** tools. Joining an
already-running session yourself with `hcom start` binds on a different path —
see *join the room before you talk*.

---

## setup and troubleshooting

**If the user invokes this skill without arguments, read
`references/setup-troubleshooting.md` and follow its setup steps.** Read it also
when `hcom` is missing or misbehaving, or when a message does not arrive.

First check is always `hcom status`.

---

## workflow scripting (script mode)

these defaults are the opposite of *interactive coordination* above, on purpose: scripts own the agents they spawn.

place scripts in `~/.hcom/scripts/` as `.sh` or `.py`. run with `hcom run <name> "task"`. see `references/script-template.md` for the full annotated template, or run `hcom run docs --scripts` inside an agent.

### key rules

- **never use `sleep`** — use `hcom events --wait` or `hcom listen` (interactive: just end the turn, see above)
- **never hardcode agent names** — parse from `grep '^Names: '` in launch output
- **always use `--thread`** — without it, messages leak across workflows (interactive coordination uses tags, not threads)
- **always use `trap cleanup ERR INT TERM`** — orphan headless agents run indefinitely
- **always use `hcom kill` for cleanup** (not `stop`) — kill also closes the terminal pane. applies only to agents the script itself spawned; never to user-spawned agents
- **always forward `--name`** — hcom injects it, scripts must propagate it
- **always use `--go`** on launch commands — without it, scripts hang on confirmation prompt (`hcom kill` never prompts, so `--go` is optional there)

---

## files

`~/.hcom/` holds `hcom.db`, `config.toml`, `.tmp/logs/` and user `scripts/`.
With `HCOM_DIR` set, that path is used instead.

---

## reference files

| file | when to read |
|------|-------------|
| `references/patterns.md` | writing multi-agent scripts — 6 tested patterns with full code and real event JSON |
| `references/cross-tool.md` | claude + codex + gemini + opencode + kilo + pi + omp + antigravity + cursor + kimi + copilot collaboration details and per-tool quirks |
| `references/gotchas.md` | debugging scripts — timing, message delivery, intent system, cleanup |
| `references/script-template.md` | writing a new script from scratch — full template with commentary |
| `references/setup-troubleshooting.md` | installing hcom, hooks missing, messages not arriving |
| `references/scripts/` | 6 tested, working example scripts |

---

## more info

`hcom --help`, `hcom <command> --help`, https://github.com/aannoo/hcom
