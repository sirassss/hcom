# Wake an idle Antigravity agent when a message arrives

**Date:** 2026-09-08
**Prior art:** `docs/issues/2026-08-25-hcom-agy-wake-limitation.md` (the symptom report this supersedes — see *Correcting the issue doc*)
**Status:** Plan written at `docs/superpowers/plans/2026-09-09-agy-wake-and-plugin-source.md`.

**Scope:** Why an idle `agy` agent does not pick up hcom messages, measured on this branch. Covers the PTY delivery gate for Antigravity, the `agy plugin import` status advice, and the no-PTY join path. Handler logic for other tools is untouched.

---

## Correcting the issue doc

`docs/issues/2026-08-25-hcom-agy-wake-limitation.md` describes **hcom python 0.7.25** (`uvx hcom`, hooks written into `~/.gemini/config/hooks.json`). Three of its claims no longer hold on this branch:

| Issue doc says | Measured here |
|---|---|
| "hết lượt → Stop → sessionend → không còn ai chờ tin" | `handle_sessionend` keeps `ST_LISTENING` on turn-end (`gemini.rs:649-656`, `antigravity.rs:854-867`). Turn-end never finalizes. |
| Fix option 2: hand-edit `~/.gemini/config/hooks.json` | That file no longer carries hcom hooks. They ship as a plugin (`plugin/hcom-agy/`), installed to `~/.gemini/config/plugins/hcom/`. |
| Fix option 3: "upstream hcom" — Antigravity stop should poll like Cursor | The wake path already exists here and needs no poll loop. It is gated shut. See D1. |

The issue doc's second half (hcsp vs `spawn-agents.sh` skip-same-vendor) is concluded and stays out of this spec.

---

## Measurement

Two `agy` agents spawned through `hcom agy` on 2026-09-08, differing by one line of code, logged side by side in `~/.hcom/.tmp/logs/hcom.log`:

| | `probe-puma` — `ready_pattern: b"? for shortcuts"` | `probe2-halo` — `ready_pattern: b"Ctx "` |
|---|---|---|
| Launch | `blocked: launch_blocked` ("screen settled before readiness") | `Launch ready (1/1, 1.0s)` |
| `ready=` in every gate evaluation | `false`, permanently | `true` |
| On message arrival | `delivery.wake` → `delivery.gate_blocked: not_idle` repeating past `attempt=55` | `gemini-beforeagent` delivered, then `delivery.no_pending` |
| Reached the model | no | **yes** — the agent began acting on the message |

Hooks fired cleanly on both (`gemini-sessionstart`, `-beforeagent`, `-beforetool`, `-aftertool`, `-afteragent`, `-sessionend`, all `exit_code=0`). The hook layer is not implicated.

`agy` version measured: 1.1.27. Its idle frame carries no `? for shortcuts`:

```
─────────────────────────────────────────────
>
─────────────────────────────────────────────
 Gemini 3.8 Flash (High) |  high |  65ce493d
 Ctx 6% (66k/1048k) |  5h 0% |  ~/workspaces/... | ⎇ feat/siras/develop +5 -0
```

---

## D1 — `ready_pattern` for Antigravity is Claude's, and never matches

`integration_spec.rs:696` sets `ready_pattern: b"? for shortcuts"`. That is Claude's pattern, copied from `integration_spec.rs:419`. `agy` does not print it.

`is_ready()` (`screen.rs:363-375`) requires the pattern to be **currently visible** — it is not sticky. So for Antigravity it returns `false` for the life of the process, and the failure cascades:

1. `launch_requires_ready: true` → `emit_launch_blocked_once` sets status `blocked` / `launch_blocked` (`delivery.rs:1312-1345`).
2. `evaluate_gate` checks `require_idle` **first** (`delivery.rs:1093`). Status `blocked` makes `is_idle()` false → gate returns `not_idle`.
3. The delivery loop never reaches the inject branch. The message stays pending forever.

Note the gate reason is `not_idle`, not `not_ready`: the ready failure is upstream of the reason the log prints. That is why the symptom reads like a status problem.

**This is the whole of the reported bug for PTY-spawned AGY.** No poll loop, no Stop-hook redesign.

### The catch: `is_ready()` doubles as "prompt is empty"

`get_antigravity_input_text` (`screen.rs:940-970`) uses `is_ready()` as its fallback in two places:

- `is_dim_after_prompt` returns `None` (dimness undecidable) → if ready, report the prompt as empty.
- No `>` line found at all → if ready, report the prompt as empty.

With the broken pattern these fallbacks always chose "keep the text". With `"Ctx "` — which renders while agy is **busy** as well as idle (measured: `Ctx 3%` in halo's frame mid-command) — they always choose "empty". Since `require_prompt_empty: true` is what protects a user's half-typed prompt from being overwritten, flipping both fallbacks to "empty" trades a delivery bug for an input-clobbering bug.

### Design

Pick the pattern and the fallback behaviour together, not separately:

- `ready_pattern` answers "is the TUI up and rendering?". `"Ctx "` answers that correctly and is the most stable string in agy's status bar — the model name (`Gemini 3.8 Flash`) changes with the model, the context percentage changes constantly but the label does not.
- Because it is now true while busy, Antigravity's two `is_ready()` fallbacks in `get_antigravity_input_text` must stop inferring emptiness from readiness. When dimness is undecidable, return the observed text (the conservative answer: treat it as the user's); when no `>` line is found, return `None` (prompt not located) rather than `Some("")`.

That keeps each signal answering its own question: readiness for the launch gate, prompt-text extraction for the overwrite guard.

`tool.rs:322` asserts the old value and moves with it.

**Acceptance:** a `hcom agy` spawn reaches `Launch ready`; a message sent to it while idle produces `delivery.gate_pass` → `delivery.injected` → `delivery.send_enter` and the agent answers. A message sent while the user has uncommitted text in the prompt does **not** overwrite it (gate reason `prompt_has_text`).

**Tests:** screen fixtures for agy's real idle frame and busy frame, asserting `is_ready()` on both and asserting `get_antigravity_input_text` returns the typed text (not `""`) when dimness is undecidable.

**Terminal-width risk:** `integration_spec.rs:536` already records that Claude's `? for shortcuts` hides in narrow terminals — the same class of failure this bug is. The status bar is left-anchored so `Ctx ` survives truncation further than a right-anchored string would, but a fixture at a narrow width belongs in the test set.

---

## D2 — the `agy plugin import` warning is a false positive that cannot be cleared

`hcom hooks` prints, for Antigravity:

```
antigravity: hcom hooks came from `agy plugin import` (claude-code), not a local
install — Antigravity is running claude-code's handlers.
Run: hcom hooks remove antigravity && hcom hooks add antigravity
```

Measured: running exactly that command leaves the warning in place. It cannot be cleared by any action the message suggests.

Cause: `agy_imported_hcom_source()` (`plugin.rs:110-120`) reads `~/.gemini/config/import_manifest.json` and treats any `hcom` entry as a foreign import, reporting its `source`. But `hcom hooks add antigravity` installs `plugin/hcom-agy/`, whose manifest directory is named `.claude-plugin/` — the Claude convention — so `agy` records the resulting import as `source: "claude-code"`. The label describes the *manifest format*, not where the plugin came from.

Proof the install is correct: `~/.gemini/config/plugins/hcom/hooks/hooks.json` carries `PreInvocation, PostInvocation, Stop, PreToolUse, PostToolUse` — agy's event names, i.e. `plugin/hcom-agy/hooks/hooks.json`. Claude's manifest would carry `SessionStart`/`PostToolUse`/`Stop` with Claude's shapes.

### Design

The check must compare against what hcom itself installs, not against a source label it does not control. Verify the installed manifest is agy's — the same file `verify_agy_plugin_installed()` already reads — and only warn when the installed hooks are *not* hcom's agy manifest. A warning that survives its own remedy is worse than no warning: it trains the reader to ignore hook status.

**The question is which handlers will run, not which keys exist.** Event names are too coarse to answer it. `PostToolUse` and `Stop` appear in **both** manifests, so neither is evidence of anything; and a key that is present can still be `null`, `[]`, `[null]`, or an entry running `true` — all of which parse and deliver nothing. Even `PreInvocation` carrying an hcom command is not enough: `plugin/hcom-agy/hooks/hooks.json` puts two handlers there, and a manifest keeping only `gemini-sessionstart` has lost the delivery hook while still looking agy-shaped.

So the check is for the handlers wake depends on, matched by their subcommand token: `gemini-sessionstart` and `gemini-beforeagent` under `PreInvocation`, `gemini-afteragent` under `PostInvocation`, each in an entry whose `type` is `command`. The token is matched rather than the whole command because the commands are long `sh -c` one-liners whose text varies with `$HCOM` resolution — but the subcommand is what decides which handler runs, and it is stable.

**Four states, not two.** The check answers with more than yes/no, and only one state names another harness:

| Installed manifest | Status says |
|---|---|
| every hcom handler present in a command entry | nothing (healthy) |
| `SessionStart` carrying a real command, and none of hcom's handlers anywhere | Antigravity is not running hcom's hooks; the import label is quoted as a format hint |
| parses, but neither of the above — partial, empty, or hand-edited | none of hcom's working hooks are installed; blamed on nobody |
| unreadable — file missing, or JSON that will not parse | hook state unverifiable; name the file |

The last two rows are what the current code loses. Every read failure funnels through `ok()?` into `None`, which prints nothing, so a corrupt `hooks.json` reports as healthy; and a half-written manifest is not evidence that another harness installed anything, so it must be reported as broken rather than attributed. `SessionStart` is the only discriminating event, and even it counts only when it carries a command entry.

**Acceptance:** after `hcom hooks add antigravity`, `hcom hooks` prints no advice line for Antigravity, checked against the shipped manifest itself. A manifest with hcom's delivery handler removed or swapped is reported as broken even though every event name is still agy's. A Claude-shaped manifest is the only one that names a source, and the wording says the label describes the manifest format. A manifest that cannot be read or parsed says so instead of staying silent.

---

## D3 — a vanilla `agy` that joins with `hcom start` has no wake path at all

Measured 2026-09-20 on agy 1.2.7, interactive TUI, probe hooks per Task 9. All four questions hold.

Wake for Antigravity lives entirely in the PTY delivery loop (`delivery.rs:1746+`), which only runs for an agent hcom launched. An agent started by hand and joined with `hcom start` has `bindings: hooks` and no delivery loop, so nothing injects into its TUI. Claude covers this case with a blocking `hcom poll` in its Stop hook; Antigravity has no equivalent.

This is a separate defect from D1 and must not be bundled with it: D1 is a broken gate on a working path, D3 is a missing path.

### Design

Measure first, then choose. The probe has to be an **interactive** agy session: `agy -p` is print mode — it runs one turn and exits, so it cannot show whether a `Stop` decision starts a *second* turn, which is the whole question. Hooks load from `~/.gemini/config/hooks.json`.

**Correction to the isolation plan:** the probe was designed around a scratch `GEMINI_CLI_HOME`, on the assumption that agy resolves its config directory the same way hcom's installer does (`antigravity.rs:68-75`, `runtime_env.rs:50-56`). That assumption is wrong for the binary itself — `strings $(which agy) | grep GEMINI_CLI_HOME` matches nothing; the string is not in the binary. A scratch `GEMINI_CLI_HOME` was confirmed inert: `agy` ran a full turn against it and wrote nothing under it, while reading plugins and skills from the real `~/.gemini`. agy resolves its config home from the process `HOME` (Go `os.UserHomeDir()`); overriding `HOME` instead produced the unauthenticated first-run screen, confirming `HOME` is the actual variable. Isolation therefore requires an `HOME` override, with the real `~/.gemini` (auth included) copied into the scratch home and only `config/hooks.json` replaced — not a `GEMINI_CLI_HOME` override. `runtime_env.rs:50-56`'s `GEMINI_CLI_HOME` handling is real and used by hcom's own installer/uninstaller; it just isn't consulted by agy's own runtime, so it does not help isolate a hand-run probe session.

A second confound surfaced on the first interactive run and is worth recording for whoever re-runs this: with the real `~/.gemini/config/plugins/superpowers` and `~/.gemini/GEMINI.md` present in the scratch home (copied along with auth), a single first turn ("say hi") drove 15 `PreInvocation` cycles before any `Stop` fired — the model used tool calls to inspect the probe's own files unprompted. That demonstrates `PreInvocation` fires per tool-invocation step *within* one user turn, not once per turn, which the probe's turn-counting `n >= 2` marker gate does not distinguish from a woken turn on its own — only the `after_stops` field (which counts `Stop` *entries*, tied to `decision=continue` lines) disambiguates them. The clean runs below removed `plugins/` and `GEMINI.md` from the scratch home first.

The measurement, on a hand-started `agy`:

1. Does agy honour a `timeout` larger than its 30s default on a `Stop` hook? (`HOOK_TIMEOUT_SEC = 15` today, `antigravity.rs:127`.)

   **Yes, to at least 48s.** `stop#1 entered=1789913657` paired with `stop#1-exit at=1789913705 decision=continue`, a run with `PROBE_SLEEP=45`. The 120s configured timeout was not established — only the 48s actually slept was proven. Pinning the true ceiling needs a higher `PROBE_SLEEP` paired with an independent timeout observation (agy's own diagnostic, or a process watch), which this run did not attempt.

2. Does returning `{"decision":"continue"}` from `Stop` start a new turn? (`antigravity.rs:119-120` claims any value other than `continue` allows the stop — this is a code comment, not a measurement.)

   **Yes.** In both the `PROBE_SLEEP=1` and `PROBE_SLEEP=45` runs, `pre#N` (`after_stops` incremented) is logged in the same second as the preceding `stop#(N-1)-exit ... decision=continue`, with no user submission between them — e.g. `stop#1-exit at=1789913705 decision=continue` immediately followed by `pre#2 injected marker at 1789913705 after_stops=1`.

3. Is there a loop guard limiting consecutive continues?

   **Not established above 3.** agy honoured all three `decision=continue` responses the probe emitted (`stop#1`, `stop#2`, `stop#3`), and the probe's own cap fired first (`stop#4-exit ... decision=allow (probe cap)`). Whether agy has an internal guard, and where, is unmeasured past that bound — this run cannot distinguish "no guard" from "guard above 3".

4. Does the new turn run `PreInvocation`, so `gemini-beforeagent` can deliver via `injectSteps[*].ephemeralMessage` (`gemini.rs:702-730`)?

   **Yes, confirmed transport, not just correlation.** In the `PROBE_SLEEP=1` run the TUI showed three separate `PROBE_DELIVERED` replies verbatim, each one the sole content of a turn whose `pre#N` (`after_stops=1,2,3`) was immediately preceded by a `stop#(N-1)-exit ... decision=continue` line.

Q4 needs a marker only a *woken* turn can produce. `PreInvocation` fires on the first turn too, before any `Stop` has run, so seeing the marker proves nothing by itself. The probe counts its own invocations in a file and injects the marker from turn 2 onward; a reply mentioning it then means the turn that saw it was started by the `Stop` decision, which is the claim under test.

The probe must also be able to stop. A hook that always returns `continue` loops for as long as agy allows — that is Q3's answer, and also a way to hang the measurement. Cap the continues the probe emits, and log every invocation with its own entry and exit timestamps, so Q1 is read off the log instead of inferred.

**All four hold, so `handle_sessionend` blocks for pending messages up to the measured timeout and returns `continue` when one arrives** — the Claude Stop-hook shape, expressed in agy's protocol. The implementation should:

- Block only up to a timeout at or below the ~48s floor actually measured, not the 120s configured-but-unproven figure, until a follow-up run pins the real ceiling (open item, not blocking — Q1 above).
- Not assume a specific continue-count ceiling from agy itself (Q3 unmeasured past 3); if hcom's own loop needs a cap for its blocking `hcom poll`, that cap must come from hcom's side, not from an assumed agy guard.
- Use the `HOME`-override isolation approach above (not `GEMINI_CLI_HOME`) for any future probe or test harness targeting a hand-started agy.

**Acceptance:** a hand-started `agy` joined via `hcom start`, running `handle_sessionend` as a blocking `Stop` hook, receives a queued message via `PreInvocation`'s `injectSteps[*].ephemeralMessage` on the turn immediately following a `continue` decision, with no user action in between — mirroring the measurement above. Acceptance does not require a specific timeout or continue-count value; both are read from configuration, not hardcoded to the figures measured here.

---

## D4 — a permanently blocked gate never escalates

Secondary, and this spec's own logs are the argument for keeping it small: `probe-puma` sat at `gate_blocked` past `attempt=55` while `hcom list` showed `blocked: launch_blocked`. The status was already visible; what was missing was any signal that *delivery* — as opposed to launch — had stalled.

`set_gate_status` already writes `tui:<reason>` context after 2s of blocking (`delivery.rs:2023-2075`), and `hcom list` renders `status (context)` (`list.rs:601-604`). So the gap is narrow: a long block is indistinguishable from a short one, and nothing is emitted for a coordinator to observe.

### Design

**The escalation must not touch `status`.** The obvious move — set `ST_BLOCKED`, mirroring `emit_launch_blocked_once` (`delivery.rs:1312-1345`) — is the one design this defect cannot take, and D1 is the proof. `is_idle()` (`instances.rs:891`) accepts only `ST_LISTENING`, and `evaluate_gate` checks idleness first, so writing `ST_BLOCKED` makes the gate's own precondition false: the escalation becomes the reason delivery never resumes. That is D1's cascade, re-created deliberately. Recovery cannot rescue it either — the stability-based path (`delivery.rs:1975+`) only rewrites `ST_ACTIVE`.

`set_gate_status` (`instances.rs:199`) is the primitive that already fits: it writes `status_context`/`status_detail` and leaves `status` alone, so the instance keeps reading `listening`, `is_idle()` keeps returning true, and the gate reopens the moment its real cause clears. Escalation is therefore **context plus event, never status** — one part per gap:

- **Context** closes the first gap. Past the threshold, write a context that differs from the 2s one — `tui:<reason>` becomes `tui:<reason>:stalled` — so `hcom list` tells a two-second block from a two-minute one at a glance. Under the same guard the 2s updater already uses: only an instance reading `listening` gets its context rewritten. When status is `ST_ACTIVE` the hooks own the context (`tool:Bash`) and a coordinator already sees `working`; the event still fires, and the event is the part that was missing. The same is true of `probe-puma`, whose status read `blocked: launch_blocked` — an instance already carrying a visible signal gets the event only. The context exists for the case that has none: an instance reading plain `listening` while nothing moves.

  The event's own `status` field carries whichever of those was observed, never a constant — an event claiming `listening` for an `ST_ACTIVE` instance would be the overclaiming D2 and D5 exist to end.
- **Event** closes the second. Emit a life event under its own action, `delivery_blocked`, carrying `gate.reason` and the elapsed seconds. It must not reuse `emit_launch_blocked_event` (`events.rs:285`), which hardcodes the action `launch_blocked`; a `context` argument does not change the action, and a coordinator would read a launch failure for an agent that launched fine. `emit_launch_lifecycle_event` (`events.rs:214`) already takes the action as a parameter, so the delivery emitter is a wrapper over it.

**The clock is per-block, not per-process.** `block_since` is what measures "continuous", so it must clear everywhere a block ends — including the `!db.has_pending()` path (`delivery.rs:1868-1877`), which today returns to `State::Idle` without clearing it. Left set, a hook that drains the queue before the gate opens donates its elapsed time to the next message's clock and an unrelated block escalates instantly. The once-only latch that keeps the event from repeating every poll clears at exactly the same sites, or the first block permanently disarms escalation for every block after it.

**`not_idle` counts.** A turn running longer than the threshold is not a bug, but from the coordinator's side it is indistinguishable from one, and in both cases the message is not being delivered — which is all the event claims. The reason field is what separates them, so it is carried, not filtered on.

Threshold is a judgement call, not a measurement — 60s is short enough to matter to a coordinator and long enough that an ordinary turn does not trip it in passing. A genuinely long turn *will* trip it, and that is the intent: the event says delivery has stalled, and the reason field says whether the cause is a busy agent or a stuck one.

**Acceptance:** an agent whose gate blocks continuously past the threshold shows `listening (tui:<reason>:stalled)` in `hcom list` — still `listening`, because that is what idleness is read from — and a `delivery_blocked` life event carries the reason and the duration. Delivery resumes on its own once the cause clears, with no recovery step and no second escalation for the same block. A block that ends because the queue drained does not carry its clock into the next one.

---

## D5 — the Cursor plugin install path cannot complete, and its verifier reads the wrong reality

Found while verifying the dev-env switch on 2026-09-08. Not an AGY defect, but the same class as D2 — status that cannot be made true by following its own advice.

`install_cursor_plugin` (`plugin.rs:301-305`) runs `cursor-agent plugin marketplace add HCOM_REPOSITORY_URL`. Cursor rejects local paths for a marketplace (`plugin.rs:284-285`), so unlike Claude and Antigravity — which both install from `dev_root` when it is set — Cursor always clones the **published** repository.

Measured: that clone lands at `~/.cursor/plugins/marketplaces/github.com/aannoo/hcom/<sha>/` and contains `plugin/hcom/skills` and `plugin/hcom/.claude-plugin/plugin.json` — **no `plugin/hcom/hooks/hooks-cursor.json`**. That manifest was added on this branch (`29ae26d`) and is not on upstream `main`.

`verify_cursor_plugin_installed` (`plugin.rs:192-199`) tests for exactly that file. So:

- The advice `hcom hooks add cursor` prints ("run `/plugins` and install hcom") cannot produce a working install — the plugin it would install carries no Cursor hooks.
- `hcom hooks` reports `Cursor: not installed` permanently, and will keep doing so until this branch reaches upstream `main`.

Meanwhile Cursor **is** running hcom's hooks, by a path neither the installer nor the verifier models: `cursor-agent` reads Claude's plugin cache, and `~/.claude/plugins/cache/hcom/hcom/1.0.0/hooks/hooks-cursor.json` is present because `hcom hooks add claude` installed the plugin from `dev_root`. Measured on `probe3-dune`: `~/.cursor/hooks.json` holds no hcom entry, `cursor-agent plugin marketplace list` shows no installed hcom plugin, yet `cursor.rs:541 handle_sessionstart` ran and the agent received message #3704 through the full inject path (`gate_pass → injected → send_enter → delivery.success`). This is the cross-tool hook contamination the plugin design exists to end — here it happens to load the right code.

### The verifier is wrong in both directions (measured 2026-09-09)

Two measurements, same day, opposite errors:

| State | `hcom hooks` says | Reality |
|---|---|---|
| Claude's plugin enabled, no Cursor marketplace | `Cursor: not installed` | Cursor **was** running hcom hooks (via Claude's plugin cache): `probe3-dune` bound `hooks, pty` and took delivery end-to-end |
| Cursor marketplace indexed at this branch, plugin never installed in `/plugins` | `Cursor: installed (plugin)` | Cursor runs **no** hcom hooks: `probe6-nami` bound `pty` only |

`verify_cursor_plugin_installed` tests for a file in a marketplace checkout. `marketplace add` creates that checkout. So the verifier answers "was a marketplace added?", never "is the plugin loaded?" — and reports installed for a plugin nobody installed.

Four load routes were measured, all with hcom's plugin **disabled in Claude** so the contamination path could not mask the result. Read `bindings` only after the agent has taken a turn — `hooks` appears when `cursor-sessionstart` first fires, not at spawn, and an early read reports `pty` for an agent whose hooks are fine:

| Route | Probe | `bindings` | `session_id` |
|---|---|---|---|
| Symlink `plugin/hcom` → `~/.cursor/plugins/local/hcom` | `probe4-kula` | `pty` | none |
| `cursor-agent --plugin-dir <path>` | `probe5-gino` | **`hooks, pty`** | bound |
| `marketplace add`, plugin never installed in `/plugins` | `probe6-nami` | `pty` | none |
| `marketplace add` + `/plugins` install | `probe7-memo` | **`hooks, pty`** | bound |

So there **is** a local route that works, and it needs no push and no interactive step: `--plugin-dir`, pointed at the checkout's own `plugin/hcom`. Dropping the directory into `~/.cursor/plugins/local/` is not equivalent — Cursor does not load it without an enable step.

### What Cursor actually supports (measured 2026-09-09)

`cursor-agent plugin` exposes only `marketplace` — no `install`, no `enable`. Installation is the interactive `/plugins` picker, as the module doc says.

`marketplace add` takes `<gitUrl>` and rejects anything that is not one: a bare path becomes `https://home/alam/workspaces.git`, and `file:///…` becomes `https:///home/alam.git`. Both fail to resolve. There is no local-directory install route; `~/.cursor/plugins/local/` exists but nothing hcom runs populates it.

But `marketplace add` accepts **`--git-ref <ref>`** — "Branch, tag, or commit to index (defaults to the default branch)". That is the missing piece: the source must be a git *remote*, but it need not be `main`, and it need not be upstream. Adding `https://github.com/sirassss/hcom --git-ref feat/siras/develop` succeeded and checked out that branch's tree.

So the constraint is narrower than "Cursor cannot install from a branch". It is: **whatever Cursor installs must be reachable as a pushed git ref.** A local-only commit cannot be installed, by any route.

### Design — decided: the fork is the single source

Measured constraint that settles it: **AGY cannot install from a URL.** `agy plugin install --help` answers `install target must be a directory`. Claude takes a URL, path, or GitHub repo but has **no branch flag** — it reads the default branch. Cursor takes a git URL only, with an optional `--git-ref`.

So the arrangement is: one fork carries the truth, and each tool reaches it by the only route it supports.

| Tool | Source | Persists across hand-opened sessions |
|---|---|---|
| Claude | fork URL, default branch | yes |
| Cursor | same fork URL + `/plugins` install | yes |
| AGY | a local checkout of that same fork | yes |

Verified 2026-09-09 after pushing the branch to the fork's `main`: `claude plugin install hcom@hcom` from the fork URL, `cursor-agent plugin marketplace add <fork>` with no ref, and a Cursor agent spawned afterwards bound `hooks, pty`. Because the fork's default branch carries the work, no `--git-ref` is needed anywhere.

**What hcom must change:**

1. **`marketplace_source()` returns the wrong thing.** It hands Claude the `dev_root` *path*, so `hcom hooks add claude` re-points the marketplace at a local directory and undoes the fork arrangement. It should resolve the checkout's **remote URL** instead — the fork the developer actually pushes to — and fall back to the published URL only when there is no remote.
2. **`install_cursor_plugin` hardcodes upstream.** `HCOM_REPOSITORY_URL` points at `aannoo/hcom`, not at the fork the checkout tracks. Same resolution as above: use the checkout's remote.
3. **Status must stop overclaiming; the verifier stays as it is.** It tests a marketplace checkout, so it answers "was a marketplace added?" — measured wrong in both directions against "are hcom's hooks running?". There is nothing better for it to test: `cursor-agent plugin` exposes no enabled marker (measured above), so the fix is not a stronger verifier but status that reports the weaker fact as the weaker fact. All four states must be reworded, not just the one that reads `installed`, because a checkout's presence and hcom's hooks firing are independent facts:

   | marketplace checkout | legacy entries | Status must say |
   |---|---|---|
   | no | no | no checkout; hooks may still be firing out of Claude's plugin cache — confirm with a spawned agent's `bindings` |
   | no | yes | no checkout; the legacy entries are what is firing |
   | yes | no | marketplace indexed, `/plugins` install still to do; hcom cannot see whether it is enabled |
   | yes | yes | legacy entries are firing and the plugin *may* be too once enabled — a double-fire risk, not an observed double-fire |

   `bindings: hooks, pty` on a spawned agent, read after its first turn, is the only observation that settles it; status names that check rather than guessing.
4. **AGY keeps its local install** and needs no change beyond D2. It is not a gap in the arrangement; it is the only route agy offers.

**Withdrawn, both proposed on smaller evidence:** passing `--git-ref` (unnecessary once the fork's default branch carries the work) and spawning Cursor with `--plugin-dir` (it covers only hcom-spawned agents, while a `/plugins` install covers every session — including the ones the user opens by hand, which was the actual requirement).

**Acceptance:** `hcom hooks add claude` and `hcom hooks add cursor` from a checkout with a fork remote point both marketplaces at that remote, not at a local path and not at upstream. A Cursor agent the user opens by hand carries hcom's hooks. `hcom hooks` never reports a state contradicted by hooks that are observably firing.

**Runbook:**

```bash
git push <fork> <branch>:main                       # the fork's default branch is the source
claude plugin marketplace add <fork-url> && claude plugin install hcom@hcom
cursor-agent plugin marketplace add <fork-url>      # then /plugins → install "hcom"
hcom hooks add antigravity                          # local checkout; agy takes no URL
```

---

## Order

D1 first — it is the reported bug, it is measured, and D4's threshold logic is untestable in practice while every AGY gate is blocked from launch. D2 is independent and small. D3's measurement can run in parallel with D1 but its implementation depends on the result.

---

## Out of scope

- hcsp vs `spawn-agents.sh` skip-same-vendor (concluded in the issue doc).
- Folding the hcom skill into the plugin.
- Blocking same-vendor spawns.
- Hook install paths for tools other than Antigravity.
