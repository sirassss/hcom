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

Already installed as a plugin with `hcom` on `PATH`? Skip the installer — it is
for fresh hosts only.

```bash
curl -fsSL https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.sh | sh
hcom claude       # or: hcom gemini, hcom codex, hcom opencode, hcom kilo, hcom pi, hcom omp, hcom agy, hcom cursor-agent, hcom kimi, hcom copilot
hcom              # TUI dashboard
```

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

## what humans can do

tell any agent:

> send a message to claude

> when codex goes idle send it the next task

> watch gemini's file edits, review each and send feedback if any bugs

> fork yourself to investigate the bug and report back

> find which agent worked on terminal_id code, resume them and ask why it sucks

---

## what agents can do

**Message** each other in real-time, bundle context for handoffs.

**Observe** each other: transcripts, file edits, terminal screens, command history.

**Subscribe** to each other: notify on status changes, file edits, specific events. React automatically.

**Spawn**, **fork**, **resume**, **kill** each other, in any terminal emulator.

run `hcom --help` for full command syntax and flags.

---

## tool support

| tool | delivery | connect |
|------|----------|---------|
| claude code (incl. subagents) | automatic | `hcom claude` |
| gemini cli (>= 0.26.0) | automatic | `hcom gemini` |
| codex | automatic | `hcom codex` |
| opencode | automatic | `hcom opencode` |
| kilo code | automatic | `hcom kilo` |
| antigravity | automatic | `hcom agy` |
| cursor | automatic | `hcom cursor-agent` |
| any other ai tool | manual via `hcom listen` | `hcom start` (run inside tool) |

session binding (hcom transcript, hcom r/f by session id) happens on first message or first prompt for all hcom-**launched** tools. joining an already-running session yourself with `hcom start` binds on a different path — see *join the room before you talk*.

---

## setup

if the user invokes this skill without arguments:

1. run `hcom status` — if "command not found", install first:
   ```bash
   curl -fsSL https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.sh | sh
   ```
2. run `hcom hooks add` to install hooks for all detected tools
3. restart the AI tool for hooks to activate

| status output | meaning | action |
|---------------|---------|--------|
| command not found | not installed | install via `brew install aannoo/hcom/hcom`, the curl installer above, or `pip install hcom` |
| `[~] claude` | tool exists, hooks not installed | `hcom hooks add` then restart |
| `[✓] claude` | hooks installed | ready |
| `[✗] claude` | tool not found | install the AI tool first |

---

## troubleshooting

### "hcom not working"

```bash
hcom status          # check installation
hcom hooks status    # check hooks specifically
hcom relay status    # check cross-device relay
```

hooks missing? `hcom hooks add` then restart tool.

still broken?
```bash
hcom reset all && hcom hooks add
# close all ai tool windows
hcom claude          # fresh start
```

### "messages not arriving"

| symptom | diagnosis | fix |
|---------|-----------|-----|
| agent not in `hcom list` | agent stopped or never bound | relaunch or wait for binding |
| message sent but not delivered | check `hcom events --last 5` | verify @mention matches agent name/tag |
| message reaches more than one agent | duplicate base name across tags | target the full `@tag-name` to hit exactly one |
| messages leaking between workflows | no thread isolation | script mode: always use `--thread`. interactive: use a tag instead |

### intent system

agents follow these rules from their bootstrap:
- `--intent request` -> agent always responds
- `--intent inform` -> agent responds only if useful
- `--intent ack` -> agent does not respond

### sandbox / permission issues

```bash
export HCOM_DIR="$PWD/.hcom"     # project-local mode
hcom hooks add                   # installs to project dir
```

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

### agent topologies

| topology | agents | pattern |
|----------|--------|---------|
| worker-reviewer | 2 | worker sends result, reviewer reads transcript, sends APPROVED/FIX |
| pipeline | N sequential | each stage reads previous via `hcom transcript`, signals via thread |
| ensemble | N+1 (judge) | N agents answer independently, judge reads all via `hcom events --sql` |
| hub-spoke | 1+N | coordinator broadcasts to `@tag-`, workers report back |
| reactive | N | `hcom events sub` triggers agent actions on file edits/status changes |

---

## files

| what | location |
|------|----------|
| database | `~/.hcom/hcom.db` |
| config | `~/.hcom/config.toml` |
| logs | `~/.hcom/.tmp/logs/` |
| user scripts | `~/.hcom/scripts/` |

with `HCOM_DIR` set, uses that path instead of `~/.hcom`.

---

## reference files

| file | when to read |
|------|-------------|
| `references/patterns.md` | writing multi-agent scripts — 6 tested patterns with full code and real event JSON |
| `references/cross-tool.md` | claude + codex + gemini + opencode + kilo + pi + omp + antigravity + cursor + kimi + copilot collaboration details and per-tool quirks |
| `references/gotchas.md` | debugging scripts — timing, message delivery, intent system, cleanup |
| `references/script-template.md` | writing a new script from scratch — full template with commentary |
| `references/scripts/` | 6 tested, working example scripts |

---

## more info

```bash
hcom --help              # all commands
hcom <command> --help    # command details
```

github: https://github.com/aannoo/hcom
