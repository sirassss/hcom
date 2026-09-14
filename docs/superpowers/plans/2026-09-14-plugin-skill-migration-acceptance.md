# Plugin skill migration — acceptance notes

Recorded 2026-09-14 on branch `feat/siras/develop`, hcom `0.7.25`.
Plan: [plugin skills and host customization](2026-09-13-plugin-skills-and-host-customization.md).

Commits under acceptance: `9a11e31` (Task 2), `40b61ef` + `e841574` (Task 3),
`3d0d569` (Task 4), agent-skill `ffbdb23` (Task 5), `80a4bc9` (Task 6 Step 4a).

## Step 1 — repository checks

| Check | Result |
|---|---|
| `cargo test` | 2353 passed, 1 failed |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo fmt --check` | clean |
| `bash dotfiles/tests/hcom-install.sh` (agent-skill) | 18 checks, 0 failures |

The single failure is `commands::start::tests::test_vanilla_start_never_installs_a_plugin_tool`.
It is **pre-existing and host-dependent**: it fails identically on the base commit
`1ea5bc6` with none of this work applied (verified by stashing). `start_bare`
returns 0 where the test expects 1, so something on this host satisfies the
vanilla-tool guard that the isolated fixture intends to fail. Not diagnosed here;
it is unrelated to the plugin work and is not counted as a regression.

## Step 2 — per-vendor skill inventory

**Antigravity — measured by a second agent (AGY, read-only, 2026-09-14):**
`agy plugin list` reports hcom with `components: ["hooks"]` and
`"importedAt": "2026-09-08T15:59:22Z"` — the copy installed on this host predates
the skill payload. `~/.gemini/config/plugins/hcom/` holds `.claude-plugin/`,
`hooks/`, `hooks.json` and `plugin.json`, with **no `skills/` directory**, so
`references/patterns.md` cannot be opened through the installed path and zero
`hcom-agent-messaging` skills are visible to the AGY runtime.

This is host state, not a code defect, and hcom reports it correctly. Live output
on this host:

```
Antigravity:  installed    (plugin)
  antigravity: hooks present; messaging skill payload is missing or incomplete
  (missing skill payload .../skills/hcom-agent-messaging/SKILL.md ...).
  Run: hcom hooks add antigravity to reinstall the complete plugin.
```

The reinstall would change the host, so it was not run. Single-skill runtime
discovery therefore remains **unverified** until the owner authorises it.

**Loader contract (closes Task 1 Step 1):** AGY loads `skills/` by convention and
needs no `"skills"` key — `~/.gemini/antigravity-cli/builtin/skills/agy-customizations/docs/plugins.md`:
"All skills, rules, hooks, and MCP servers defined within the plugin's directory
structure are automatically loaded." `plugin/hcom-agy/.claude-plugin/plugin.json`
is correct as committed; nothing to add.

**Legacy aliases:** `~/.hcom/SKILL.md`, `~/.hcom/skill` and `~/.claude/skills/hcom`
do not exist on this host.

**Name collision (AGY, read-only):** nothing else on this host claims
`hcom-agent-messaging` — no workspace `.agents`, no other installed plugin
(`superpowers`, `ponytail`, `agent-ops`), nothing under
`~/.gemini/config/skills/` or the AGY builtin skills. So once the payload is
installed, AGY would load exactly one instance. The verifier also confirmed that
`plugin/hcom-agy/skills/` is deliberately absent from the checkout: staging
materializes it at install time, and generated skill copies are not committed.

**Measured live on this host** with `hcom hooks status` (read-only), which is the
first non-fixture exercise of the Task 3 path end to end — it really did spawn a
Codex app-server, fetch `hooks/list` and classify the result:

```
Claude:       installed    (plugin)
codex:        installed (legacy native hooks) (/home/alam/.codex/hooks.json)
Antigravity:  installed    (plugin)   [payload incomplete, see above]
Cursor:       marketplace ready (plugin)
```

So on this host: Claude's plugin is installed, Codex is on the legacy native
layer with no plugin handlers, and Cursor has a marketplace checkout whose
enabled state hcom cannot read by design. Skill *inventory* per vendor (as
opposed to hook state) is still measured only for Antigravity.

**Marketplace descriptor:** Codex resolves `.claude-plugin/marketplace.json` —
its binary carries that literal and `.cursor-plugin/marketplace.json`, with no
`.codex-plugin` variant (0.154.0). So `hcom@hcom` is the right selector for
`codex plugin add` and no Codex-specific marketplace file is needed. A test pins
the selector against the committed marketplace.

## Step 3 — Codex import and native route

`codex plugin` (0.154.0) exposes `add`, `list`, `marketplace`, `remove`; there is
no `validate`. `codex plugin marketplace add` accepts "a local path,
owner/repo[@ref], HTTPS Git URL, or SSH Git URL", which is why the native route
reuses the same `marketplace_source()` policy Claude uses.

**Overlay selection — schema and docs support it, runtime still unmeasured.**
A Codex-side verifier found that the official packaging documentation
(<https://developers.openai.com/plugins/build/plugins>, "Bundled hooks") states an
explicit `hooks` value replaces the default `hooks/hooks.json` discovery. So
Claude's `hooks/hooks.json` should not win by convention, and the earlier worry
that Codex would be fed Claude's handlers is answered at the documentation level.
It is **not** a runtime measurement: no install or `/import` was performed, so
which file an installed package actually selects remains unverified. If the
convention did win, the result is the `incompatible Claude handlers` state the
classifier reports.

## Cross-vendor review

A Codex agent reviewed Tasks 2–4 read-only at `6daae9c` and returned FAIL with six
findings, all real and all fixed in `55e06af`; an AGY agent verified the skill
inventory (above). Highlights, because two of them would have shipped broken:

- The committed overlay ships the self-resolving `cmd=${HCOM:-hcom}; … exec $cmd
  codex-stop` guard while the classifier matched only the resolved `hcom
  codex-stop`, so every handler of a live plugin was invisible and the plugin read
  as missing. The regression now builds its fixture from `hooks-codex.json` itself.
- `Duplicate` required two complete sets and ignored `enabled`, so one enabled
  legacy handler beside a complete plugin set reported as active despite
  double-firing. Overlap is now per event, over enabled handlers only.
- A group Codex could not evaluate parsed as an empty group, so a failed inventory
  could become "nothing installed" — the verdict that decides whether hcom
  installs. Group errors now fail the parse into `Unverified`.
- A failed Codex plugin uninstall was printed as a note and reported as success.
- `hooks status` never rendered the spec's "not active; import from Claude
  required" row.

A second review round confirmed all six closed and found one more defect, in the
fix itself: the failed-removal message asserted that the legacy entries were
removed and the plugin was still installed, neither of which is known — the
remover can return false, and a missing `codex` binary means the plugin's
presence was never established. Fixed in `9f815cc`, with a unit test pinning
that the message asserts only what was observed.

Final repository state after the review rounds, run with a scratch `HCOM_DIR`:
`cargo test` 2359 passed / 1 failed (the pre-existing failure above), clippy
`-D warnings` clean, `cargo fmt --check` clean.

## Known hazard: `cargo test` drops live agents from the bus

`mark_dead_instances()` (`src/instance_lifecycle.rs:774`) runs on every hcom
invocation and deletes any instance whose recorded pid is not alive. Running the
test suite against the real `~/.hcom` removed three live agents mid-session
(`hcom list --stopped` shows `exit:reboot by:system` for all three at once).
Re-running the same suite with `HCOM_DIR` pointed at a scratch directory leaves
the roster intact. Use `HCOM_DIR=<scratch> cargo test` in this repo. This is a
test-isolation defect of the same family as the pre-existing failure above; it is
outside this plan's scope and is recorded, not fixed.

## Step 4 — artifact independence

`tests/plugin_payload.rs::agy_package_carries_the_canonical_skill` stages the
artifact, renames the source tree away, and then compares every file under the
canonical skill — including `references/scripts/` — byte for byte, asserting each
resolves inside the artifact root. This is not inferred from a repo symlink.

## Step 4a — skill mode scope

Audited `skills/hcom-agent-messaging/SKILL.md` and its four references. The merge
at `1ea5bc6` is preserved. One remaining contradiction found and fixed in
`80a4bc9`: `references/gotchas.md` stated the `hcom kill` cleanup rule without
scope, where it read as host policy inside a reusable skill; it now says the rule
covers only agents the script itself spawned and defers to `~/.hcom/HOST.md`.
No other contradiction found.

## Measured externally (read-only, nothing installed)

codex-cli 0.154.0, `hooks/list` against a scratch `CODEX_HOME` carrying hcom's
five hooks, 2026-09-14:

- Response shape is `/result/data[]`, one group per layer, each with `cwd`,
  `hooks`, `warnings`, `errors`. hcom previously read `data[0]` only.
- Entry fields: `key`, `eventName`, `handlerType`, `command`, `async`, `matcher`,
  `timeoutSec`, `statusMessage`, `additionalContextLimit`, `sourcePath`, `source`,
  `pluginId`, `displayOrder`, `enabled`, `isManaged`, `currentHash`, `trustStatus`.
- `eventName` is lowerCamelCase (`sessionStart`, `preToolUse`, `postToolUse`,
  `userPromptSubmit`, `stop`) while the same entry's `key` segment is snake_case.
  A classifier comparing the two calls a healthy install incomplete.
- `pluginId` is null for config layers, so it is the plugin-origin signal.

The codex binary also carries `.codex-plugin/plugin.json` alongside
`.claude-plugin/plugin.json` and `.cursor-plugin/plugin.json`, which is what the
Task 2 overlay directory name rests on.

## Owner-run measurements, 2026-09-14 (after the review rounds)

The owner reinstalled the AGY plugin and ran Codex `/import`. Both measured with
`hcom hooks status`.

**Antigravity — now clean.** The payload warning is gone; the installed copy
carries the canonical skill. This closes the AGY half of Step 2.

**Codex — `incompatible Claude handlers`, and the classifier is right.** The
imported package at `~/.codex/plugins/cache/hcom/hcom/1.0.0/` contains
`.claude-plugin/`, `.cursor-plugin/` and `hooks/` — **no `.codex-plugin/` and no
`skills/`**. Codex therefore had no overlay to select and fell back to
`hooks/hooks.json`, which is Claude's, so it is running `hcom pre`, `post`,
`sessionstart`, `poll`, `subagent-start` and friends. hcom reported exactly that,
naming every offending handler and its source path.

This is **not** evidence that the overlay mechanism fails. The marketplace source
is the git remote this checkout tracks, and none of this work has been pushed:
`origin/feat/siras/develop` is 24 commits behind local and predates the Task 2
overlay entirely. Codex imported a revision that never had a `.codex-plugin/`
directory.

So overlay selection is still unmeasured, and now has a concrete way to measure
it without pushing: `codex plugin marketplace add <local path>` accepts a local
path (0.154.0 `--help`), so the checkout itself can be the marketplace. That
changes the host, so it awaits the owner.

Incidentally this is the first live confirmation that the Task 3 classifier
catches the incompatible state on a real machine, with real inventory data, and
refuses to call it working.

## Codex overlay and skill payload — measured end to end, 2026-09-14

With the owner's go-ahead the Codex plugin was reinstalled from the local
checkout as a marketplace (`codex plugin marketplace add <path>`, supported since
0.154.0), which removes the "unpushed remote" confound above.

**First install, straight from the checkout:** the plugin cache carried
`.claude-plugin/`, `.codex-plugin/`, `.cursor-plugin/` and `hooks/` — and **no
`skills/`**. `plugin/hcom/skills` is a symlink to the canonical skill and Codex's
copier does not follow it. Claude's installer materializes the same link (its
cache does carry `skills/`), and every other plugin in Codex's own cache has a
real `skills/` directory, so this is specific to shipping that path as a link —
the same hazard Task 1 solved for Antigravity, which the shared-package route had
not applied. Fixed in `00e3301`: with a local checkout, Codex is pointed at a
staged marketplace whose `skills/` is materialized.

**Overlay selection — confirmed at runtime.** Codex read
`hooks/hooks-codex.json`, not `hooks/hooks.json`: `hooks status` listed all five
`codex-*` handlers sourced from the overlay file. The `hooks` key beats the
convention in practice, not only in the documentation. This closes Step 3's
central question.

**After the staged reinstall:** the cache carries
`skills/hcom-agent-messaging/` with `references/` and `references/scripts/`, and
`hooks status` reports `installed; hook review required` — the correct spec row
for a complete, enabled, untrusted handler set. The trust step is the user's.

Along the way the classifier was exercised live on three different real states and
was right each time: `incompatible Claude handlers` (pre-overlay import),
`duplicate hooks; double-fire risk` (plugin plus still-enabled legacy entries),
and `installed; hook review required`.

**Still open:** the remote route ships the symlink, so a released Codex install
would carry hooks and no skill. Materializing the directory for a release is a
packaging decision, not made here.

## Released route — measured 2026-09-14 (owner-authorised)

Tasks 1-3 and 5 of the [committed skill copies plan](2026-09-14-committed-plugin-skill-copies.md)
were implemented, then the released route was exercised from a clone.

**Clone.** `git clone --depth 1 "file://$(pwd)"` at revision
`046324eea36d81efc5814ca54a26d9b877df3f97`. `find <clone>/plugin -type l` returned
**0** symlinks. `diff -r` against `skills/hcom-agent-messaging` printed nothing for
both `plugin/hcom/skills/hcom-agent-messaging` and
`plugin/hcom-agy/skills/hcom-agent-messaging` — same file set, same bytes.

**Codex**, installed from that clone as a local-path marketplace: the cache at
`~/.codex/plugins/cache/hcom/hcom/1.0.0/` now contains `.claude-plugin`,
`.codex-plugin`, `.cursor-plugin`, `hooks` **and `skills`**, and `diff -r`
against the canonical tree printed nothing. Before this plan the same install
produced no `skills/` at all.

**Antigravity**, installed directly from `<clone>/plugin/hcom-agy` — deliberately
not through hcom, whose staging substitutes the canonical tree and would pass with
the committed copy absent. `agy plugin install` reported `skills: 1 processed`, and
`diff -r` against the canonical tree printed nothing. hcom's own route was then
re-run as a separate regression (`HCOM_DEV_ROOT=<clone> hcom hooks add antigravity`
→ "already installed", status clean); that is evidence about hcom's staging path, not
about the committed copy.

**Codex hook status** reads `duplicate hooks; double-fire risk`, naming all five
events and `~/.codex/hooks.json`. That is correct and expected: hcom-launched Codex
agents install the native hooks at launch, so the legacy set returned after the
earlier `--legacy-only` removal. Resolving it is a user action
(`hcom hooks remove codex --legacy-only` once the plugin's hooks are trusted).

**Still unmeasured:** a real git-URL marketplace. `marketplace_source()` carries no
ref, so vendors resolve the remote's default branch, and nothing here is pushed. A
local-path marketplace proves tree contents only, never the vendor's git acquisition
or ref selection. That verification is Task 4 Step 6 of the plan and is blocked on
publication.

**Release gate:** `custom-payload` now appears in the generated `release.yml` inside
`build-global-artifacts`' needs, `host`'s needs, and `host`'s condition, which permits
each custom job only as `skipped` or `success`. A failing payload job is neither, so
`host` does not run and nothing is published. Reasoned from the generated edges; no
failing CI run has been observed.

## Plugin source: the owner's GitHub fork — 2026-09-14

**Correction.** An earlier pass read "point the plugin at the fork" as the local
checkout and made the source prefer it (`4a52083`); the owner meant the fork on
GitHub. Reverted in `2a43d27`. The marketplace source is the git remote the
checkout's branch tracks — `origin`, the fork — never upstream and never a local
path, which Cursor cannot use and other machines cannot reach.

Resolved on this host: `origin` is `git@sirassss:sirassss/hcom.git`, `sirassss` is
a `~/.ssh/config` alias whose real host is `github.com`, so the marketplace URL is
`https://github.com/sirassss/hcom`. Without the alias resolution added in
`a3ba019` this came out as `https://sirassss/sirassss/hcom`, which resolves
nowhere.

**Blocking for this route:** the URL carries no ref, so vendors resolve the fork's
**default branch**, which is `main` at `60dc686`. Local `HEAD` is **53 commits
ahead** of it, and none of the plugin work — the Codex overlay, the committed
adapter skill copies — exists on `main`. Installing from the fork URL today would
fetch a package with no `.codex-plugin/` and no `skills/`. The route is correct in
code and unusable until the work lands on the branch the fork serves.

## Local-checkout measurement (superseded, kept as evidence) — 2026-09-14

Implemented per the [local fork plugin source plan](2026-09-14-local-fork-plugin-source.md),
for an owner who builds from their own fork and never publishes a GitHub release.

**Marketplace source.** `codex plugin marketplace list` now shows
`hcom  /home/alam/workspaces/research/agent-connect/hcom` — the checkout itself,
no URL, no host resolution. The installed payload at
`~/.codex/plugins/cache/hcom/hcom/1.0.0/skills/hcom-agent-messaging` is byte-identical
to the working tree (`diff -r`, no output).

**Edit-to-install loop proved.** A probe line appended to
`skills/hcom-agent-messaging/SKILL.md`, then `scripts/sync-plugin-skills.sh` and a
reinstall, reached the installed copy (`grep -c` = 1). The edit was reverted, resynced
and reinstalled; `diff -r` is clean again and the worktree carries no leftover.

**Codex hook state:** `installed (plugin hooks active)` — the first time this host has
reached the fully-active row. `~/.codex/hooks.json` holds no hcom entries, so nothing
double-fires.

**Not exercised here:** hcom's own native install branch. Claude is present on this
host, so `hooks add codex` correctly stops at the `/import` guidance and never reaches
the installer; the two CLI commands that branch would run were issued directly instead.
The branch's source choice is covered by unit test
(`a_local_checkout_is_preferred_by_every_consumer_that_accepts_one`).

**Not live-verified:** that a Codex agent launch no longer reinstalls the native hooks.
The guard (`codex_launch_needs_native_hooks`, `src/launcher.rs`) is unit-tested across
all ten inventory states, but a live spawn was not run because a Codex agent was already
on the roster and the host policy forbids a duplicate vendor.

## Verdict

Tasks 2–6 are complete. The Codex-side verifier returned **PASS** on Tasks 2–4
after two fix rounds (seven defects found and closed), and the owner then ran the
two host-changing measurements, which closed Step 2 and Step 3 and surfaced an
eighth defect — the missing skill payload on Codex installs — fixed in `00e3301`.

One decision remains, and it is a packaging one rather than a task: the remote
marketplace route still ships `plugin/hcom/skills` as a symlink, which Codex
drops. A release has to materialize that directory.

## Not done

- No plugin was installed or removed on the live host.
- No agent was stopped or killed.
- Nothing was pushed.
