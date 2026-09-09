# AGY Wake and Plugin Source Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make an idle Antigravity agent wake on an hcom message, and make hook-install status tell the truth for Antigravity and Cursor.

**Architecture:** Five independent defects from `docs/superpowers/specs/2026-09-08-agy-wake-design.md`. D1 unblocks AGY's PTY delivery gate by fixing a ready pattern copied from Claude, and separates "TUI is up" from "prompt is empty" so the fix cannot clobber user input. D2 stops a false-positive warning that cannot be cleared. D5 points both marketplaces at the fork the checkout tracks instead of a local path (Claude) and hardcoded upstream (Cursor). D4 escalates a permanently blocked delivery gate through context and an event, never through `status` — writing `ST_BLOCKED` there would re-create D1's own cascade. D3 is a measurement spike with no production code.

**Tech Stack:** Rust 2024, `cargo test`, vt100 screen parsing (`src/pty/screen.rs`), SQLite via rusqlite.

---

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `src/integration_spec.rs` | Per-tool spec table; holds `ready_pattern` | 1 |
| `src/tool.rs` | Tool enum; test pins `ready_pattern` | 1 |
| `src/pty/screen.rs` | vt100 screen reads: `is_ready`, `get_antigravity_input_text` | 1, 2 |
| `src/hooks/plugin.rs` | Plugin install/verify; marketplace source | 4, 5, 6, 7 |
| `src/commands/hooks.rs` | `hcom hooks` status output | 4, 7 |
| `src/delivery.rs` | PTY delivery loop and gate | 8 |
| `src/db/events.rs` | Life-event emitters | 8 |
| `docs/superpowers/specs/2026-09-08-agy-wake-design.md` | Records D3's measurement | 9 |

Tasks 1–3 are D1 and must land in order. Tasks 4 (D2), 5–7 (D5), 8 (D4) are independent of each other and of D1. Task 9 (D3) is a spike; it can run at any point.

---

### Task 1: AGY's ready pattern matches what agy actually renders

`agy` 1.1.27 never prints `? for shortcuts` — that is Claude's pattern (`integration_spec.rs:419`), copied to Antigravity. Its status bar renders `Ctx 6% (66k/1048k) | …`, and `is_ready()` requires the pattern to be visible *right now*, so Antigravity's readiness has been permanently false.

**Files:**
- Modify: `src/integration_spec.rs:696`
- Modify: `src/tool.rs:322`
- Test: `src/pty/screen.rs` (tests module, after the `// ---- Antigravity input extraction ----` block)

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `src/pty/screen.rs`:

```rust
// ---- Antigravity readiness ----

#[test]
fn antigravity_idle_frame_is_ready() {
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process(
        concat!(
            "> \r\n",
            " Gemini 3.8 Flash (High) |  high |  65ce493d\r\n",
            " Ctx 6% (66k/1048k) |  5h 0% |  ~/workspaces/hcom | branch\r\n",
        )
        .as_bytes(),
    );
    assert!(t.is_ready(), "agy idle frame must satisfy the ready gate");
}

#[test]
fn antigravity_busy_frame_is_also_ready() {
    // The status bar renders while agy is running a command. Readiness answers
    // "is the TUI up", not "is agy idle" — idleness is the gate's own check.
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process(
        concat!(
            "* Running command...\r\n",
            "> \r\n",
            " Ctx 3% (33k/1048k) |  5h 1% |  ~/workspaces/hcom | branch\r\n",
        )
        .as_bytes(),
    );
    assert!(t.is_ready());
}

#[test]
fn antigravity_ready_pattern_survives_a_narrow_terminal() {
    // Claude's pattern hides when the terminal is narrow (integration_spec.rs:536).
    // agy's status bar is left-anchored, so the label survives truncation.
    let mut t = make_tracker(24, 40, "Ctx ");
    t.process(" Ctx 6% (66k/1048k) |  5h 0%\r\n".as_bytes());
    assert!(t.is_ready());
}

#[test]
fn antigravity_frame_without_status_bar_is_not_ready() {
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process("starting agy...\r\n".as_bytes());
    assert!(!t.is_ready());
}
```

- [ ] **Step 2: Run the tests to verify they pass already**

Run: `cargo test --lib antigravity_` (one filter — `cargo test` takes a single positional `TESTNAME`; a second one errors `unexpected argument`)

Expected: PASS. These pin the behaviour of `is_ready()` against a pattern passed directly to `make_tracker`; they do not yet prove the *spec* carries that pattern. The next step is the one that fails.

- [ ] **Step 3: Write the failing spec test**

Change the existing assertion at `src/tool.rs:322` from:

```rust
        assert_eq!(Tool::Antigravity.ready_pattern(), b"? for shortcuts");
```

to:

```rust
        // agy 1.1.27 renders no "? for shortcuts"; its status bar carries
        // "Ctx <pct>% (<used>/<total>)". Measured 2026-09-08: with the old
        // pattern every AGY launch reported blocked and no message was ever
        // injected. See docs/superpowers/specs/2026-09-08-agy-wake-design.md.
        assert_eq!(Tool::Antigravity.ready_pattern(), b"Ctx ");
```

- [ ] **Step 4: Run it to verify it fails**

Run: `cargo test --lib antigravity_ready_pattern`

Expected: FAIL — `assertion \`left == right\` failed`, left `[63, 32, 102, ...]` (`? for shortcuts`), right `[67, 116, 120, 32]` (`Ctx `).

- [ ] **Step 5: Change the spec**

In `src/integration_spec.rs`, in the `ANTIGRAVITY` block, replace line 696:

```rust
    ready_pattern: b"? for shortcuts",
```

with:

```rust
    // Claude's pattern was copied here; agy never prints it. Its status bar
    // renders "Ctx <pct>% (<used>/<total>)" in every frame once the TUI is up.
    ready_pattern: b"Ctx ",
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib antigravity`

Expected: PASS, all antigravity tests.

- [ ] **Step 7: Commit**

```bash
git add src/integration_spec.rs src/tool.rs src/pty/screen.rs
git commit -m "fix(agy): use a ready pattern agy actually renders

agy 1.1.27 prints no \"? for shortcuts\" — that is Claude's pattern,
copied into the Antigravity spec. is_ready() needs the pattern visible
in the current frame, so readiness was permanently false: every launch
reported blocked, is_idle() stayed false, and the delivery gate returned
not_idle forever. Measured side by side against a probe carrying this
pattern, which took delivery end to end."
```

---

### Task 2: readiness stops standing in for "the prompt is empty"

`get_antigravity_input_text` falls back to `is_ready()` in two places to decide the prompt is empty. With the broken pattern that fallback never fired. With Task 1's pattern — which renders while agy is *busy* too — it would always fire, and `require_prompt_empty` is what stops a wake from overwriting a half-typed user prompt. Each signal must answer its own question.

**Files:**
- Modify: `src/pty/screen.rs:940-970` (`get_antigravity_input_text`)
- Test: `src/pty/screen.rs` (tests module)

- [ ] **Step 1: Write the failing test**

Add to the tests module in `src/pty/screen.rs`:

```rust
#[test]
fn antigravity_no_prompt_line_is_unknown_not_empty() {
    // Only the status bar is on screen — the prompt row is not located.
    // "Unknown" must not be reported as "empty", or the delivery gate would
    // inject over whatever the user has typed.
    let mut t = make_tracker(24, 120, "Ctx ");
    t.process(" Ctx 6% (66k/1048k) |  5h 0% |  ~/workspaces/hcom\r\n".as_bytes());
    assert_eq!(t.get_antigravity_input_text(), None);
    assert!(!t.is_prompt_empty("antigravity"));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib antigravity_no_prompt_line_is_unknown_not_empty`

Expected: FAIL — `assertion \`left == right\` failed: left: Some(""), right: None`. The final `if self.is_ready()` fallback reports an empty prompt.

- [ ] **Step 3: Make both fallbacks stop inferring emptiness**

In `src/pty/screen.rs`, in `get_antigravity_input_text`, replace:

```rust
            return match self.is_dim_after_prompt(row_idx as u16, ">") {
                Some(true) => Some(String::new()),
                Some(false) => Some(text.to_string()),
                None => {
                    if self.is_ready() {
                        Some(String::new())
                    } else {
                        Some(text.to_string())
                    }
                }
            };
        }

        if self.is_ready() {
            return Some(String::new());
        }

        None
```

with:

```rust
            return match self.is_dim_after_prompt(row_idx as u16, ">") {
                Some(true) => Some(String::new()),
                Some(false) => Some(text.to_string()),
                // Readiness answers "is the TUI up", not "is the prompt empty":
                // agy's status bar renders while it is busy too. When dimness is
                // undecidable, treat the glyphs as the user's text — reporting
                // "empty" here would let a wake overwrite what they typed.
                None => Some(text.to_string()),
            };
        }

        // Prompt row not located. Unknown is not empty; `is_prompt_empty`
        // treats None as "not safe", which is the answer we want.
        None
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib antigravity`

Expected: PASS. The existing tests `antigravity_prompt_without_trailing_space`, `antigravity_dim_placeholder_with_ready_returns_empty`, `antigravity_empty_prompt_with_ready` and `antigravity_injected_text_with_ready_footer` all reach a `Some(_)`/dim branch, so none of them depend on the removed fallbacks.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`

Expected: PASS, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/pty/screen.rs
git commit -m "fix(agy): stop reading readiness as an empty prompt

The ready pattern now renders while agy is busy, so the two is_ready()
fallbacks in get_antigravity_input_text would have reported every
undecidable frame as an empty prompt — and require_prompt_empty is what
keeps a wake from overwriting a half-typed prompt. Unknown now stays
unknown, which is_prompt_empty already treats as not safe."
```

---

### Task 3: acceptance — an idle AGY takes delivery

Tasks 1 and 2 are unit-tested, but the defect was found in a live probe and must be closed by one. This task runs commands; it writes no code.

**Files:** none.

- [ ] **Step 1: Build and spawn a probe**

```bash
cargo build --release
hcom agy --tag acc --go --hcom-prompt "Say exactly READY and nothing else."
```

Expected: `Launch ready: <name> (1/1 ready, …)`. A `launch blocked: screen settled before readiness` here means Task 1 did not take effect.

- [ ] **Step 2: Send a message while it is idle**

```bash
hcom start --as accprobe
hcom send --name accprobe @acc-<name> --intent request -- reply with the single word WOKE
hcom stop accprobe
```

- [ ] **Step 3: Confirm the full delivery chain in the log**

```bash
grep '"instance":"<name>"' ~/.hcom/.tmp/logs/hcom.log | grep delivery | tail -8
```

Expected, in order: `delivery.wake` → `delivery.gate_pass` → `delivery.injected` → `delivery.text_rendered` → `delivery.send_enter` → `delivery.success`. A repeating `delivery.gate_blocked: not_idle` is the old failure.

- [ ] **Step 4: Confirm the agent answered**

```bash
hcom transcript acc-<name> --last 2 --full
```

Expected: the agent's reply contains `WOKE`.

- [ ] **Step 5: Confirm a typed prompt is not overwritten**

In the agy pane, type `dont clobber me` without pressing Enter, then:

```bash
hcom start --as accprobe2
hcom send --name accprobe2 @acc-<name> --intent inform -- second message
hcom stop accprobe2
```

Expected: the typed text stays in the prompt; the log shows a gate block naming the prompt, not an inject. This is Task 2's guard in the real TUI.

- [ ] **Step 6: Clean up**

```bash
hcom kill tag:acc
```

---

### Task 4: the AGY import warning checks the manifest, not a label

`hcom hooks` warns that Antigravity is running claude-code's handlers, and tells the user to run `hcom hooks remove antigravity && hcom hooks add antigravity`. Measured: running exactly that leaves the warning in place. `agy plugin install <dir>` *does* write an import entry, and it labels our plugin `source: "claude-code"` because its manifest directory is named `.claude-plugin/` — the label describes the manifest format, not the origin.

The check that replaces it reads the installed manifest, and it must answer in three states, not two: ours, foreign, and *unverifiable*. Today every read failure funnels through `ok()?` into `None`, so a corrupt `hooks.json` prints nothing and reports as healthy.

**Files:**
- Modify: `src/hooks/plugin.rs:102-120`
- Modify: `src/commands/hooks.rs:163-171`
- Test: `src/hooks/plugin.rs` (tests module, near `agy_imported_hcom_source_reads_a_foreign_import`)

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `src/hooks/plugin.rs`:

```rust
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

    // The delivery hook swapped for a Claude one. Every event name is still
    // agy's, and every command still says "hcom" — only the handler that
    // actually delivers is gone, which is the whole thing the check exists for.
    let swapped = bundled.replace("gemini-beforeagent", "claude-sessionstart");
    assert_eq!(agy_state_with(&swapped), super::AgyHooks::Malformed);

    // The ready signal removed.
    let no_after = bundled.replace("gemini-afteragent", "gemini-sessionstart");
    assert_eq!(agy_state_with(&no_after), super::AgyHooks::Malformed);

    // Renamed, not removed. A substring test passes every one of these while
    // the subcommand that actually delivers is gone.
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

    // A partial agy install keeping only the events agy and Claude share.
    // `PostToolUse` and `Stop` are in BOTH manifests, so neither may be read
    // as evidence of Claude — this must not come back Foreign.
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib agy_hook_state`

Expected: FAIL to compile — `cannot find type \`AgyHooks\`` and `cannot find function \`agy_hook_state\` in module \`super\``.

- [ ] **Step 3: Add the state and the check**

In `src/hooks/plugin.rs`, immediately after `agy_imported_hcom_source`, add:

```rust
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
        .flat_map(|entry| match entry.get("hooks").and_then(serde_json::Value::as_array) {
            Some(nested) => nested.iter().collect::<Vec<_>>(),
            None => vec![entry],
        })
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
        return AgyHooks::Foreign(agy_imported_hcom_source().unwrap_or_else(|| "unknown".to_string()));
    }
    AgyHooks::Malformed
}
```

Then correct the stale claim in the doc comment on `agy_imported_hcom_source` (line 104): replace

```rust
/// A genuine `agy plugin install` never touches this file — only
/// `agy plugin import <harness>` does. Its entries look like
```

with

```rust
/// Measured 2026-09-08: `agy plugin install <dir>` DOES write here, recording
/// our plugin as `source: "claude-code"` because our manifest directory is
/// named `.claude-plugin/`. So an entry alone proves nothing about origin —
/// callers must check the installed manifest's shape; see [`agy_hook_state`],
/// which is the only caller that should reach for this label. Entries look like
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib agy_hook_state`

Expected: PASS, 5 tests — one per state, plus the bundled-manifest mutations. The healthy one reads the shipped manifest itself.

- [ ] **Step 5: Point status at the new check**

In `src/commands/hooks.rs`, replace lines 163-171:

```rust
            if tool == Tool::Antigravity
                && let Some(source) = crate::hooks::plugin::agy_imported_hcom_source()
            {
                println!(
                    "  antigravity: hcom hooks came from `agy plugin import` ({source}), not a \
                     local install — Antigravity is running {source}'s handlers. \
                     Run: hcom hooks remove antigravity && hcom hooks add antigravity"
                );
            }
```

with:

```rust
            if tool == Tool::Antigravity {
                use crate::hooks::plugin::AgyHooks;
                match crate::hooks::plugin::agy_hook_state() {
                    // Our own manifest. The import entry says `claude-code`
                    // because our manifest dir is `.claude-plugin/`; that is a
                    // format label, and warning on it printed advice that could
                    // never clear itself.
                    AgyHooks::Hcom => {}
                    AgyHooks::Foreign(source) => println!(
                        "  antigravity: the installed manifest carries SessionStart and none \
                         of hcom's handlers, so Antigravity is not running hcom's hooks. \
                         agy records the import as `{source}` — that names the manifest \
                         format, so treat it as a hint, not as proof of what is running. \
                         Run: hcom hooks remove antigravity && hcom hooks add antigravity"
                    ),
                    // Not attributed to anyone: a half-written or hand-edited
                    // manifest is not evidence that another harness did it.
                    AgyHooks::Malformed => println!(
                        "  antigravity: the installed manifest carries none of hcom's working \
                         hooks — hcom's events are missing, empty, or invoke something else, \
                         so no message will be delivered. \
                         Run: hcom hooks remove antigravity && hcom hooks add antigravity"
                    ),
                    AgyHooks::Unverifiable => println!(
                        "  antigravity: {} could not be read or parsed — hook state unverifiable. \
                         Run: hcom hooks add antigravity",
                        crate::hooks::plugin::agy_plugin_dir()
                            .join(crate::hooks::plugin::AGY_HOOKS_RELATIVE)
                            .display()
                    ),
                }
            }
```

- [ ] **Step 6: Verify against the real machine**

Run: `cargo build --release && ./target/release/hcom hooks`

Expected: the `antigravity: hcom hooks came from …` line is gone, and Antigravity still reports `installed (plugin)`.

Then prove the other three states are reachable, since a healthy machine only exercises one. Use a scratch `GEMINI_CLI_HOME` — the real installed plugin is never touched, so a crash or a forgotten restore cannot leave the machine without hooks:

```bash
probe=$(mktemp -d)
mkdir -p "$probe/.gemini/config/plugins/hcom/hooks"
hooks="$probe/.gemini/config/plugins/hcom/hooks/hooks.json"

printf '{not json' > "$hooks"
GEMINI_CLI_HOME=$probe ./target/release/hcom hooks | grep -i antigravity

printf '{"hooks":{"SessionStart":[{"type":"command","command":"hcom claude-sessionstart"}],"PostToolUse":[]}}' > "$hooks"
GEMINI_CLI_HOME=$probe ./target/release/hcom hooks | grep -i antigravity

printf '{"hooks":{"PreInvocation":[],"PostInvocation":[]}}' > "$hooks"
GEMINI_CLI_HOME=$probe ./target/release/hcom hooks | grep -i antigravity

rm -rf "$probe"
```

Expected, in order: `hook state unverifiable`; then the Foreign line — `carries SessionStart and none of hcom's handlers`, quoting the import label as a format hint (`unknown` when the scratch home has no import entry); then `none of hcom's working hooks`, with no harness named at all.

- [ ] **Step 7: Commit**

```bash
git add src/hooks/plugin.rs src/commands/hooks.rs
git commit -m "fix(agy): warn on foreign hooks, not on a manifest-format label

agy plugin install writes an import entry labelled claude-code for our
own plugin, because our manifest dir follows Claude's convention. Status
took that label as proof agy was running Claude's handlers, so the
warning fired forever and the remedy it printed could never clear it.
Compare the installed manifest instead — requiring hcom's events to be
present AND non-empty, and reporting a manifest that cannot be parsed as
unverifiable rather than silently healthy."
```

---

### Task 5: the marketplace source is the fork the checkout tracks

`marketplace_source()` hands Claude the `dev_root` **path**, so `hcom hooks add claude` re-points the marketplace at a local directory. The decided arrangement is a fork as the single source: Claude and Cursor index the fork's URL, AGY installs from a local checkout of that same fork (agy takes no URL — `agy plugin install --help` answers `install target must be a directory`).

**Files:**
- Modify: `src/hooks/plugin.rs:252-266`
- Test: `src/hooks/plugin.rs` (tests module)

- [ ] **Step 1: Write the failing test**

Add to the tests module in `src/hooks/plugin.rs`:

```rust
// One `cargo test` filter takes one positional TESTNAME, so these three share
// a prefix rather than being named for what each asserts alone.
#[test]
fn normalize_git_url_rewrites_an_ssh_remote() {
    assert_eq!(
        super::normalize_git_url("git@github.com:sirassss/hcom.git"),
        "https://github.com/sirassss/hcom"
    );
}

#[test]
fn normalize_git_url_strips_only_the_git_suffix() {
    assert_eq!(
        super::normalize_git_url("https://github.com/sirassss/hcom.git"),
        "https://github.com/sirassss/hcom"
    );
    assert_eq!(
        super::normalize_git_url("https://github.com/sirassss/hcom"),
        "https://github.com/sirassss/hcom"
    );
}

#[test]
fn normalize_git_url_trims_git_output_whitespace() {
    assert_eq!(
        super::normalize_git_url("git@github.com:sirassss/hcom.git\n"),
        "https://github.com/sirassss/hcom"
    );
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib normalize_git_url`

Expected: FAIL to compile — `cannot find function \`normalize_git_url\``.

- [ ] **Step 3: Implement the resolution**

First widen the import at `src/hooks/plugin.rs:53` — the new helpers take `&Path`:

```rust
use std::path::{Path, PathBuf};
```

Then replace `marketplace_source` (lines 252-266) with:

```rust
/// Marketplace source: the git remote the checkout's current branch tracks.
///
/// A developer's work lives on a fork, and that fork is what Claude and Cursor
/// must index — Claude has no branch flag, so the fork's default branch has to
/// carry the work. Handing Claude the `dev_root` *path* instead (what this used
/// to do) re-points the marketplace at a local directory and undoes that.
/// Antigravity does not call this: `agy plugin install` takes a directory only.
fn marketplace_source() -> String {
    // `paths::db_path()` is a free function, so nothing has to be threaded
    // through to reach dev_root here.
    let db_path = crate::paths::db_path();
    let Some((root, _source)) = crate::router::resolve_effective_dev_root(&db_path) else {
        return HCOM_REPOSITORY_URL.to_string();
    };
    checkout_remote_url(&root).unwrap_or_else(|| HCOM_REPOSITORY_URL.to_string())
}

/// URL of the remote the checkout's branch tracks, falling back to `origin`.
fn checkout_remote_url(root: &Path) -> Option<String> {
    let branch = git_output(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let remote = git_output(root, &["config", "--get", &format!("branch.{branch}.remote")])
        .unwrap_or_else(|| "origin".to_string());
    let url = git_output(root, &["remote", "get-url", &remote])?;
    Some(normalize_git_url(&url))
}

/// Run git in `root`, returning trimmed stdout when it exits 0.
fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// `git@host:owner/repo.git` → `https://host/owner/repo`. Neither Claude nor
/// Cursor accepts an SSH remote as a marketplace source.
fn normalize_git_url(url: &str) -> String {
    let url = url.trim().trim_end_matches(".git");
    if let Some(rest) = url.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        return format!("https://{host}/{path}");
    }
    url.to_string()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib normalize_git_url`

Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "fix(plugin): index the fork the checkout tracks, not a local path

marketplace_source handed Claude the dev_root path, so hooks add claude
re-pointed the marketplace at a directory and undid the fork
arrangement. Resolve the tracking branch's remote instead, normalising
an SSH remote to https since neither Claude nor Cursor accepts one."
```

---

### Task 6: Cursor indexes the same fork

`install_cursor_plugin` hardcodes `HCOM_REPOSITORY_URL` (`aannoo/hcom`), so a developer's Cursor indexes upstream — which does not carry `plugin/hcom/hooks/hooks-cursor.json` (added on this branch), leaving the verifier permanently false and the printed `/plugins` step impossible to complete.

**Files:**
- Modify: `src/hooks/plugin.rs:301-311`

- [ ] **Step 1: Use the resolved source**

In `src/hooks/plugin.rs`, in `install_cursor_plugin`, replace:

```rust
    run_tool_cli(
        "cursor-agent",
        &["plugin", "marketplace", "add", HCOM_REPOSITORY_URL],
    )?;
```

with:

```rust
    // Same source as Claude: the fork this checkout tracks, so a developer's
    // Cursor indexes the branch that actually carries hooks-cursor.json.
    // Cursor takes a git URL only — a path or file:// URL is mangled into an
    // unresolvable https host (measured 2026-09-09).
    let source = marketplace_source();
    run_tool_cli(
        "cursor-agent",
        &["plugin", "marketplace", "add", source.as_str()],
    )?;
```

- [ ] **Step 2: Update the module doc that states the opposite**

In `src/hooks/plugin.rs`, in the doc comment above `install_cursor_plugin`, replace:

```rust
/// `cursor-agent plugin` exposes only `marketplace` — installation happens in
/// the interactive `/plugins` picker (measured, module doc). Cursor also
/// rejects local paths for a marketplace, so `dev_root` cannot drive this and
/// the remote URL is always used.
```

with:

```rust
/// `cursor-agent plugin` exposes only `marketplace` — installation happens in
/// the interactive `/plugins` picker (measured, module doc). Cursor rejects
/// local paths, so `dev_root` cannot be passed directly; what it can index is
/// the *remote* that checkout tracks, which is what `marketplace_source`
/// resolves.
```

- [ ] **Step 3: Verify the build and the real command**

```bash
cargo build --release
./target/release/hcom hooks add cursor
cursor-agent plugin marketplace list | grep hcom
```

Expected: the listed hcom marketplace URL is the fork this checkout tracks, not `aannoo/hcom`.

- [ ] **Step 4: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "fix(cursor): index the checkout's own remote, not upstream

Upstream's default branch does not carry hooks-cursor.json, so a
developer following the printed /plugins step installed a plugin with no
Cursor hooks and the verifier stayed false forever."
```

---

### Task 7: Cursor status says what it actually knows

`verify_cursor_plugin_installed` tests for a file in a marketplace checkout, and `marketplace add` is what creates that checkout. So it answers "was a marketplace added?" while status prints it as "installed". Measured wrong in both directions on 2026-09-08/09: `not installed` while Cursor was running hcom's hooks out of Claude's plugin cache, then `installed` for a plugin nobody had installed in `/plugins`.

The verifier itself stays as it is — `cursor-agent plugin` exposes no enabled marker, so there is nothing stronger for it to test. What changes is status, and **all four states change, not just the one that reads `installed`**: a checkout's presence and hcom's hooks firing are independent facts, so every combination currently claims more than it knows.

| checkout | legacy | today | must say |
|---|---|---|---|
| no | no | `not installed` | no checkout — but hooks may still be firing from Claude's plugin cache |
| no | yes | `not installed` + `hooks add` | no checkout; the legacy entries are what is firing |
| yes | no | `installed (plugin)` | marketplace indexed; `/plugins` install still to do |
| yes | yes | "both are firing" | legacy is firing, the plugin *may* be — a risk, not an observation |

**Files:**
- Modify: `src/commands/hooks.rs:110-132` (`plugin_status_line`)
- Modify: `src/commands/hooks.rs:150-158` (the `installed (plugin)` line)
- Test: `src/commands/hooks.rs` (tests module)

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `src/commands/hooks.rs`. One filter reaches all of them:

```rust
#[test]
fn plugin_status_line_cursor_marketplace_only_is_not_an_install() {
    let line = super::plugin_status_line("cursor", true, false);
    assert!(
        line.contains("/plugins"),
        "a marketplace checkout is not an install; status must name the \
         remaining step. got: {line:?}"
    );
}

#[test]
fn plugin_status_line_cursor_no_checkout_admits_hooks_may_still_fire() {
    // Measured 2026-09-08: probe3-dune bound `hooks, pty` and took delivery
    // end to end with no Cursor marketplace at all — Cursor was reading
    // Claude's plugin cache. Flat "not installed" contradicted that.
    let line = super::plugin_status_line("cursor", false, false);
    assert!(
        line.contains("Claude"),
        "status must not claim Cursor has no hcom hooks. got: {line:?}"
    );
}

#[test]
fn plugin_status_line_cursor_double_fire_is_a_risk_not_an_observation() {
    let line = super::plugin_status_line("cursor", true, true);
    assert!(
        !line.contains("both are firing"),
        "hcom cannot see whether the plugin is enabled, so it cannot say both \
         are firing. got: {line:?}"
    );
    assert!(line.contains("--legacy-only"), "got: {line:?}");
}

#[test]
fn plugin_status_line_cursor_no_checkout_with_legacy_names_what_fires() {
    let line = super::plugin_status_line("cursor", false, true);
    assert!(
        line.contains("legacy"),
        "with legacy entries present, they are what is firing — status must \
         say so instead of repeating the generic install advice. got: {line:?}"
    );
    assert!(line.contains("/plugins"), "got: {line:?}");
}

#[test]
fn plugin_status_line_other_tools_keep_their_wording() {
    assert_eq!(super::plugin_status_line("claude", true, false), "");
    assert_eq!(super::plugin_status_line("antigravity", true, false), "");
    assert!(
        super::plugin_status_line("claude", true, true).contains("both are firing"),
        "only Cursor's plugin state is unobservable; Claude's is not"
    );
    assert!(super::plugin_status_line("antigravity", false, false).contains("hooks add"));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib plugin_status_line_`

Expected: the four `cursor` tests FAIL (`(true, false)` returns `""`; both `(false, _)` states return the same flat `hooks not installed` line; `(true, true)` still says "both are firing"), `plugin_status_line_other_tools_keep_their_wording` PASSES.

- [ ] **Step 3: Rewrite the Cursor arms**

In `src/commands/hooks.rs`, in `plugin_status_line`, replace the Cursor `(true, true)` arm and the two catch-alls:

```rust
        (true, true) => format!(
            "{tool}: plugin and legacy hooks both present — both are firing (double-fire risk). \
             Once /plugins shows hcom enabled, run: hcom hooks remove {tool} --legacy-only \
             (plain `hooks remove` would uninstall the plugin too, leaving no hooks)"
        ),
        (false, _) => format!("{tool}: hooks not installed. Run: hcom hooks add {tool}"),
        (true, false) => String::new(),
```

with:

```rust
        // Cursor's verifier proves a marketplace checkout exists — which
        // `marketplace add` itself creates. Nothing hcom can read says whether
        // the plugin is enabled, so none of Cursor's four states may be stated
        // as an observation of what is firing.
        (true, true) => format!(
            "{tool}: legacy hooks are firing, and the plugin may be too once /plugins shows \
             hcom enabled — a double-fire risk hcom cannot confirm. Once it is enabled, run: \
             hcom hooks remove {tool} --legacy-only \
             (plain `hooks remove` would uninstall the plugin too, leaving no hooks)"
        ),
        (true, false) if tool == "cursor" => format!(
            "{tool}: marketplace indexed; finish in Cursor with /plugins → install \"hcom\". \
             hcom cannot see whether the plugin is enabled — confirm with a spawned agent \
             showing `bindings: hooks, pty` in hcom list, read after its first turn."
        ),
        // Not "no hooks": measured 2026-09-08, a Cursor agent ran hcom's hooks
        // with no marketplace at all, because cursor-agent reads Claude's
        // plugin cache. Saying "not installed" here contradicted a live agent.
        (false, false) if tool == "cursor" => format!(
            "{tool}: no marketplace checkout. Cursor may still be running hcom's hooks out of \
             Claude's plugin cache — check a spawned agent's `bindings` after its first turn. \
             For a Cursor-owned install: hcom hooks add {tool}, then /plugins → install \"hcom\"."
        ),
        (false, true) if tool == "cursor" => format!(
            "{tool}: no marketplace checkout; the legacy hook entries are what is firing. \
             Run: hcom hooks add {tool}, then /plugins → install \"hcom\", and only then \
             hcom hooks remove {tool} --legacy-only"
        ),
        (false, _) => format!("{tool}: hooks not installed. Run: hcom hooks add {tool}"),
        (true, false) => String::new(),
```

Note the `(true, true)` arm above it — the one guarded `if tool != "cursor"` — is untouched: Claude's and Antigravity's plugin state *is* observable, so "both are firing" is accurate there.

- [ ] **Step 4: Soften the headline for Cursor**

In `src/commands/hooks.rs`, replace lines 153-158:

```rust
            if *installed {
                println!("{}:  installed    (plugin)", tool.spec().label);
            } else {
                println!("{}:  not installed", tool.spec().label);
            }
```

with:

```rust
            // Cursor's signal is weaker than the others' in both directions: a
            // marketplace checkout is not a live plugin, and no checkout is not
            // "no hooks". Neither headline may be stated flatly for it.
            let state = match (tool == Tool::Cursor, *installed) {
                (true, true) => "marketplace ready",
                (true, false) => "no marketplace",
                (false, true) => "installed   ",
                (false, false) => "not installed",
            };
            if *installed || tool == Tool::Cursor {
                println!("{}:  {state} (plugin)", tool.spec().label);
            } else {
                println!("{}:  {state}", tool.spec().label);
            }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib plugin_status_line_`

Expected: PASS, 5 tests — one per Cursor state, plus the guard that the other tools' wording is unchanged.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test`

Expected: PASS, 0 failed. Any test asserting the literal `Cursor:  installed` or `Cursor:  not installed` string must be updated to the new wording, not the other way round.

- [ ] **Step 7: Commit**

```bash
git add src/commands/hooks.rs
git commit -m "fix(cursor): report a marketplace checkout as what it is

The verifier tests a checkout that marketplace add itself creates, so it
answers \"was a marketplace added?\" — printed as \"installed\" it was
measured wrong in both directions. All four states overclaimed: no
checkout is not \"no hooks\" (Cursor reads Claude's plugin cache), and a
checkout plus legacy entries is a double-fire risk, not an observed one.
Name the remaining TUI step and the observation that settles it."
```

---

### Task 8: a permanently blocked delivery gate escalates

`probe-puma` sat at `delivery.gate_blocked` past `attempt=55` with nothing distinguishing it from a momentary block. Two gaps: `hcom list` looked the same at 2s and at 2min, and no event was emitted for a coordinator to observe.

**The escalation must not write `status`.** Mirroring `emit_launch_blocked_once` — set `ST_BLOCKED` — is the one shape this cannot take. `is_idle()` (`instances.rs:891`) accepts only `ST_LISTENING` and `evaluate_gate` checks idleness first, so `ST_BLOCKED` makes the gate's own precondition false and the escalation becomes the reason delivery never resumes: D1's cascade, re-created on purpose. The stability recovery at `delivery.rs:1975+` only rewrites `ST_ACTIVE`, so it would not rescue it either. `set_gate_status` (`instances.rs:199`) writes `status_context`/`status_detail` and leaves `status` alone — that is the primitive, and it is already in this branch.

**Files:**
- Modify: `src/db/events.rs` (a delivery emitter next to `emit_launch_blocked_event`)
- Modify: `src/db/instances.rs` (a compare-and-clear next to `set_gate_status`)
- Modify: `src/delivery.rs` (block clock, the gate-blocked branch, every gate-clear site)
- Test: `src/db/events.rs`, `src/db/instances.rs` and `src/delivery.rs` (tests modules)

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `src/delivery.rs`. These are behavioural — the threshold predicate alone would not have caught the two ways this feature breaks (a stalled context overwritten on the next poll, and a latch that never rearms):

```rust
#[test]
fn delivery_block_escalates_only_past_the_threshold() {
    use std::time::Duration;
    let threshold = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS);
    assert!(!should_escalate_block(Duration::from_secs(0), threshold));
    assert!(!should_escalate_block(
        threshold - Duration::from_millis(1),
        threshold
    ));
    assert!(should_escalate_block(threshold, threshold));
}

#[test]
fn delivery_block_context_is_stable_across_polls() {
    use std::time::Duration;
    let below = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS - 1);
    let at = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS);
    let later = Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS + 30);

    assert_eq!(gate_block_context("prompt_has_text", below), "tui:prompt-has-text");
    assert_eq!(
        gate_block_context("prompt_has_text", at),
        "tui:prompt-has-text:stalled"
    );
    // The bug this test exists for: the 2s updater recomputes the context every
    // poll and writes whenever it differs from the last one written. If it
    // rebuilt the unsuffixed string after the escalation, the stalled marker
    // would vanish on the very next poll and never come back — `arm()` fires
    // once. Same input, same string, at any elapsed time past the threshold.
    assert_eq!(
        gate_block_context("prompt_has_text", later),
        gate_block_context("prompt_has_text", at)
    );
    // A gate whose reason changes gets a new context and is written again.
    assert_ne!(
        gate_block_context("not_idle", at),
        gate_block_context("prompt_has_text", at)
    );
}

#[test]
fn delivery_block_clock_arms_once_per_block() {
    let mut clock = BlockClock::start();
    assert!(!clock.escalated);
    assert!(clock.arm(), "first arm past the threshold must fire");
    assert!(!clock.arm(), "one block escalates once, not every poll");
    // A new block is a new clock — this is why the latch lives inside it and
    // not beside it: every `block_since = None` site rearms for free.
    assert!(BlockClock::start().arm());
}
```

And to the tests module in `src/db/events.rs` — the test that would have caught the hardcoded action. It uses the helpers already in that module (`setup_full_test_db` / `cleanup_test_db` from `src/db/mod.rs`, `get_events_since` at `events.rs:514`, which returns `serde_json::Value` rows whose `data` is already parsed):

```rust
#[test]
fn delivery_blocked_event_carries_its_own_action() {
    use crate::shared::ST_ACTIVE;
    let (db, db_path) = crate::db::tests::setup_full_test_db();

    db.emit_delivery_blocked_event("nova", ST_ACTIVE, "not_idle", 63)
        .unwrap();

    let events = db.get_events_since(0, Some("life"), Some("nova")).unwrap();
    let blocked: Vec<_> = events
        .iter()
        .filter(|e| e["data"]["action"] == "delivery_blocked")
        .collect();
    assert_eq!(blocked.len(), 1, "one block, one event: {events:?}");
    let data = &blocked[0]["data"];
    // `emit_launch_blocked_event` hardcodes "launch_blocked" and its `context`
    // argument does not change that — a coordinator would read a launch
    // failure for an agent that launched fine.
    assert_eq!(data["reason"], "not_idle");
    // The observed status, not a constant: a not_idle block is an ACTIVE
    // instance, and the event must not claim otherwise.
    assert_eq!(data["status"], ST_ACTIVE);
    assert!(
        data["detail"].as_str().unwrap().contains("63"),
        "the duration is the whole point of the event: {data}"
    );

    crate::db::tests::cleanup_test_db(db_path);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run each on its own — `cargo test` takes a single positional filter:

```bash
cargo test --lib delivery_block_
cargo test --lib delivery_blocked_event_carries_its_own_action
```

Expected: FAIL to compile — `cannot find value \`DELIVERY_BLOCKED_ESCALATE_SECS\``, `cannot find function \`should_escalate_block\``, `cannot find function \`gate_block_context\``, `cannot find type \`BlockClock\``, and no method `emit_delivery_blocked_event`.

- [ ] **Step 3: Add the delivery event emitter**

In `src/db/events.rs`, immediately after `emit_launch_blocked_event`, add:

```rust
    /// A delivery gate that has stayed blocked long enough for a coordinator
    /// to care. Deliberately not `emit_launch_blocked_event`: that one
    /// hardcodes the action `launch_blocked`, and its `context` parameter does
    /// not change it, so reusing it would report a launch failure for an agent
    /// that launched fine. Status stays untouched — see the delivery loop.
    pub(crate) fn emit_delivery_blocked_event(
        &self,
        name: &str,
        status: &str,
        reason: &str,
        blocked_secs: u64,
    ) -> Result<()> {
        self.emit_launch_lifecycle_event(
            name,
            "delivery_blocked",
            status,
            "delivery_blocked",
            Some(reason),
            Some(&format!("gate blocked {blocked_secs}s continuously")),
        )?;
        Ok(())
    }
```

`status` is the caller's *observed* status, not a constant: a `not_idle` block sits on an `ST_ACTIVE` instance, and an event that hardcoded `listening` would claim a state the instance is not in — the same overclaiming D2 and Task 7 exist to stop. When the caller cannot read a status at all it passes `"unknown"`, which is also not an observation but at least does not name a state.

- [ ] **Step 4: Add the threshold, the context builder and the clock**

In `src/delivery.rs`, next to the other module constants near the top, add:

```rust
/// How long a delivery gate may block continuously before it is escalated. A
/// judgement call, not a measurement: long enough that an ordinary turn does
/// not trip it in passing, short enough that a coordinator notices a stuck
/// agent within one working minute. A genuinely long turn will trip it, and
/// that is intended — see `gate_block_context`'s callers.
const DELIVERY_BLOCKED_ESCALATE_SECS: u64 = 60;

/// True once a continuous block has lasted at least `threshold`.
fn should_escalate_block(blocked_for: Duration, threshold: Duration) -> bool {
    blocked_for >= threshold
}

/// The gate-block context `hcom list` renders: `tui:<reason>`, and
/// `tui:<reason>:stalled` once the block has run past the threshold.
///
/// One function because two call sites write this string, and the 2s updater
/// writes whenever its computed context differs from the last one written. If
/// the escalation wrote a suffixed string the updater did not know about, the
/// updater would overwrite it on the very next poll — and the escalation
/// latch fires once, so the stalled marker would never come back.
fn gate_block_context(reason: &str, blocked_for: Duration) -> String {
    let stalled = should_escalate_block(
        blocked_for,
        Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS),
    );
    format!(
        "tui:{}{}",
        reason.replace('_', "-"),
        if stalled { ":stalled" } else { "" }
    )
}

/// How long the current block has run, and whether it has already escalated.
///
/// The two live in one value on purpose: the loop clears the clock at several
/// sites, and a separate `bool` would have to be cleared at all of them too.
/// Miss one and the first block disarms escalation for every block after it.
struct BlockClock {
    since: Instant,
    escalated: bool,
}

impl BlockClock {
    fn start() -> Self {
        Self { since: Instant::now(), escalated: false }
    }

    /// Elapsed time of this block.
    fn elapsed(&self) -> Duration {
        self.since.elapsed()
    }

    /// Take the one escalation this block is allowed. False on every later call.
    fn arm(&mut self) -> bool {
        if self.escalated {
            return false;
        }
        self.escalated = true;
        true
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test --lib delivery_block_
cargo test --lib delivery_blocked_event_carries_its_own_action
```

Expected: PASS.

- [ ] **Step 6: Put the clock in the loop**

`Instant` is `Copy`; `BlockClock` is not, so the two existing `if let Some(since) = block_since` reads would *move* it out of the option. Both must borrow.

In `src/delivery.rs`, change the declaration at line 1754:

```rust
        let mut block_since: Option<Instant> = None;
```

to:

```rust
        let mut block_since: Option<BlockClock> = None;
```

Then:

- lines 1967-1968 and 2409: `block_since = Some(Instant::now())` → `block_since = Some(BlockClock::start())`
- line 2023: `if let Some(since) = block_since` → `if let Some(clock) = block_since.as_ref()`, and `since.elapsed().as_secs_f64()` → `clock.elapsed().as_secs_f64()`
- line 2056: `} else if let Some(since) = block_since {` → `} else if let Some(clock) = block_since.as_ref() {`, same `elapsed()` rewrite inside

The `block_since = None` sites need no change — clearing the clock clears the latch with it, which is why the two were merged.

- [ ] **Step 7: Route both context writers through the builder**

Both blocked-branch updaters build their own context string today, and neither knows about the threshold. Replace the hardcoded strings so the suffix appears and stays.

At line 2027 (the `not_idle` path), replace:

```rust
                                        let context = "tui:not-idle".to_string();
```

with:

```rust
                                        let context =
                                            gate_block_context(gate.reason, clock.elapsed());
```

At lines 2063-2065 (the general path), replace:

```rust
                                        let reason_formatted = gate.reason.replace("_", "-");
                                        let context = format!("tui:{}", reason_formatted);
```

with:

```rust
                                        let context =
                                            gate_block_context(gate.reason, clock.elapsed());
```

Both keep their existing `if context != last_block_context` guard and their existing `status == ST_LISTENING` guard — the suffix rides along on machinery that already works, and an instance whose status is `active` or `blocked` still keeps its hook-owned context.

- [ ] **Step 8: Emit the event once per block**

In the `} else {` branch of `if gate.safe`, immediately after the `if block_since.is_none() { block_since = Some(BlockClock::start()); }` statement, add:

```rust
                        // Past the threshold, a block stops being a transient:
                        // nothing is being delivered, and until now nothing was
                        // emitted for a coordinator to see. Event only — the
                        // context is written by the updaters below, and writing
                        // ST_BLOCKED here would make is_idle() false so the gate
                        // could never reopen (see D4).
                        if let Some(clock) = block_since.as_mut()
                            && should_escalate_block(
                                clock.elapsed(),
                                Duration::from_secs(DELIVERY_BLOCKED_ESCALATE_SECS),
                            )
                            && clock.arm()
                        {
                            let blocked_secs = clock.elapsed().as_secs();
                            // The status the event reports is the one observed.
                            // A failed lookup is not an observation: say so
                            // rather than naming a state we did not read.
                            let observed = match db.get_status(&current_name) {
                                Ok(Some((status, _))) => status,
                                _ => "unknown".to_string(),
                            };
                            // `not_idle` is carried, not filtered: a turn longer
                            // than the threshold is not a bug, but the message
                            // is still not being delivered, and the reason field
                            // is what tells the two apart.
                            if let Err(e) = db.emit_delivery_blocked_event(
                                &current_name,
                                &observed,
                                gate.reason,
                                blocked_secs,
                            ) {
                                log_warn("native", "delivery.escalate_event_fail", &format!("{e}"));
                            }
                            log_warn(
                                "native",
                                "delivery.blocked_escalated",
                                &format!(
                                    "{current_name}: gate blocked {blocked_secs}s ({}, status {observed})",
                                    gate.reason
                                ),
                            );
                        }
```

- [ ] **Step 9: Make clearing the gate context compare-and-clear**

A stalled context lives in the database, so clearing the local clock is not enough — the row keeps saying `stalled` after the block is over. But the existing clear is `set_gate_status(name, "", "")`, which writes unconditionally, and `last_block_context` being non-empty does **not** mean the row still holds what we wrote. The interleaving that breaks it: the loop writes `tui:prompt-has-text:stalled`, a hook drains the queue and writes its own `active` / `tool:Bash` / `running tests`, then the loop reaches its cleanup with a non-empty marker and erases the hook's context. Reading first and clearing after is still racy — the hook can land in between. The comparison has to be in the `WHERE` clause.

This is not new to the escalation: `delivery.rs:2306-2312`, `2363-2373` and `2469-2472` all clear unconditionally today. One helper fixes all of them.

In `src/db/instances.rs`, immediately after `set_gate_status`, add:

```rust
    /// Clear a gate-block context, but only if it is still the one we wrote.
    ///
    /// The delivery loop writes `tui:<reason>` contexts and later clears them,
    /// while hooks write their own (`tool:Bash`) to the same column. An
    /// unconditional clear erases whatever the hook put there; a read-then-
    /// clear still loses the race. Comparing in the WHERE clause is what makes
    /// this safe. `cmd:listen` details are preserved as in `set_gate_status`.
    ///
    /// Returns whether a row matched, so a caller can tell "cleared" from
    /// "someone else owns it now" — both mean the caller may drop its marker,
    /// but only a `Err` means it should keep it and retry.
    pub fn clear_gate_status_if(&self, name: &str, expected_context: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE instances SET status_context = '',
                status_detail = CASE WHEN status_detail = 'cmd:listen' THEN status_detail ELSE '' END
             WHERE name = ? AND status_context = ?",
            params![name, expected_context],
        )?;
        Ok(rows > 0)
    }
```

Add to that module's tests:

```rust
    #[test]
    fn clear_gate_status_only_clears_our_own_context() {
        use crate::shared::ST_ACTIVE;
        let (db, db_path) = setup_full_test_db();
        db.conn
            .execute(
                "INSERT INTO instances (name, tool, created_at, status, status_context) VALUES (?1, ?2, ?3, ?4, ?5)",
                params!["nova", "antigravity", 1.0f64, "listening", "start"],
            )
            .unwrap();

        // Our own row: both columns cleared.
        db.set_gate_status("nova", "tui:prompt-has-text:stalled", "gate blocked 60s")
            .unwrap();
        assert!(
            db.clear_gate_status_if("nova", "tui:prompt-has-text:stalled")
                .unwrap()
        );
        let (_, context) = db.get_status("nova").unwrap().unwrap();
        assert_eq!(context, "");
        assert_eq!(db.get_instance_status("nova").unwrap().unwrap().detail, "");

        // A hook wrote its own context AND detail after ours. Neither may move.
        db.set_gate_status("nova", "tui:prompt-has-text:stalled", "gate blocked 60s")
            .unwrap();
        db.set_status("nova", ST_ACTIVE, "tool:Bash").unwrap();
        db.conn
            .execute(
                "UPDATE instances SET status_detail = 'running tests' WHERE name = ?1",
                params!["nova"],
            )
            .unwrap();
        assert!(
            !db.clear_gate_status_if("nova", "tui:prompt-has-text:stalled")
                .unwrap()
        );
        let (status, context) = db.get_status("nova").unwrap().unwrap();
        assert_eq!(status, ST_ACTIVE);
        assert_eq!(context, "tool:Bash");
        assert_eq!(
            db.get_instance_status("nova").unwrap().unwrap().detail,
            "running tests"
        );

        // A hand-joined instance keeps its cmd:listen detail, as set_gate_status does.
        db.set_gate_status("nova", "tui:not-idle:stalled", "gate blocked 60s")
            .unwrap();
        db.conn
            .execute(
                "UPDATE instances SET status_detail = 'cmd:listen' WHERE name = ?1",
                params!["nova"],
            )
            .unwrap();
        assert!(db.clear_gate_status_if("nova", "tui:not-idle:stalled").unwrap());
        assert_eq!(
            db.get_instance_status("nova").unwrap().unwrap().detail,
            "cmd:listen"
        );

        cleanup_test_db(db_path);
    }
```

`get_instance_status` is the existing accessor at `src/db/instances.rs:147`; its `detail` field is `status_detail`. The INSERT is the fixture idiom at `src/db/instances.rs:1029-1034`.

Then in `src/delivery.rs`, next to `should_escalate_block`, add the one operation every site shares — six call sites is where a copied four-line `match` starts drifting, and the `Idle` retry in the next step needs something it can be tested against:

```rust
/// Release the gate context this loop owns, if it still owns it.
///
/// `marker` is the last context this loop wrote; empty means it owns nothing.
/// It is cleared once the database has been asked — a row that no longer
/// matches belongs to a hook now, which is equally "not ours". It is kept only
/// when the database errored, so `State::Idle` can retry.
fn release_gate_context(db: &HcomDb, name: &str, marker: &mut String) {
    if marker.is_empty() {
        return;
    }
    match db.clear_gate_status_if(name, marker) {
        Ok(_) => marker.clear(),
        Err(e) => log_warn("native", "delivery.gate_clear_fail", &format!("{e}")),
    }
}
```

Then replace the clear at **all five sites** — the three existing ones above and the two this task adds — with:

```rust
                        release_gate_context(db, &current_name, &mut last_block_context);
                        block_since = None;
```

`db` inside the loop is already `&mut HcomDb` (`run_delivery_loop`, `delivery.rs:1559`), which reborrows to `&HcomDb` on its own — `&db` would build a `&&mut HcomDb` and trip the plan's `clippy -D warnings` gate. The test passes `&db` because its `db` is owned.

The two new sites are:

- **The `no_pending` branch** (lines 1868-1877), which returns to `State::Idle` without touching either. This is the leak: a hook that drains the queue before the gate opens leaves a stalled context on the row *and* donates its elapsed seconds to the next message's clock.
- **The `gate.safe` branch**, immediately after the `log_info("native", "delivery.gate_pass", …)` call. The clock measures continuous *blocking*, so it ends when the gate opens — not when delivery is later confirmed at `VerifyCursor`.

- [ ] **Step 10: Retry a clear that failed**

Keeping the marker on `Err` only helps if something comes back to it, and nothing does: every clear site sits on a path out of `State::Pending`, and `State::Idle` (`delivery.rs:1782-1864`) only leaves for `Pending` when new messages arrive. One failed clear on an agent that then goes quiet leaves a `stalled` context on the row for the life of the process.

`Idle` is where the loop already spends its waiting time, and a non-empty marker there means exactly one thing — a clear that did not complete, since blocks live in `Pending`. Add at the top of the `State::Idle` arm, before the wall-clock capture:

```rust
                    // A failed clear has no other way back: this arm only
                    // leaves for Pending when a message arrives, and no path
                    // out of Pending clears a context it did not write. A
                    // marker surviving into Idle is that failure, so retry it
                    // here. Compare-and-clear makes the retry safe on its own:
                    // if a hook has taken the row since, no rows match.
                    release_gate_context(db, &current_name, &mut last_block_context);
```

No separate copy of the expected context is needed. A new block writes its own context to both the row and the marker, so the marker always names what is on the row; the only state this retry can see is the one it is for.

The retry is exactly `release_gate_context` run again, so the test drives that rather than the loop. The clear is made to fail by taking the table out from under it — deterministic, in-process, and it exercises the real `Err` arm rather than a mock:

```rust
#[test]
fn a_failed_gate_clear_is_retried_and_still_respects_ownership() {
    // The db::tests helpers are `pub(super)` — visible inside `db`, not here.
    // This is the fixture `delivery.rs` already uses (see
    // `status_refresh_repairs_codex_approval_cache_divergence`, line 2665).
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = HcomDb::open_raw(&db_path).unwrap();
    db.init_db().unwrap();
    db.conn()
        .execute(
            "INSERT INTO instances (name, tool, created_at, status, status_context) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["nova", "antigravity", 1.0f64, ST_LISTENING, "start"],
        )
        .unwrap();
    db.set_gate_status("nova", "tui:not-idle:stalled", "gate blocked 60s")
        .unwrap();
    let mut marker = "tui:not-idle:stalled".to_string();

    // The clear fails. The marker must survive, or nothing knows the row is
    // still dirty — this is the leak the Idle retry exists for.
    db.conn()
        .execute("ALTER TABLE instances RENAME TO instances_hidden", [])
        .unwrap();
    release_gate_context(&db, "nova", &mut marker);
    assert_eq!(marker, "tui:not-idle:stalled", "a failed clear keeps the marker");

    // Next idle iteration: same call, and now it lands.
    db.conn()
        .execute("ALTER TABLE instances_hidden RENAME TO instances", [])
        .unwrap();
    release_gate_context(&db, "nova", &mut marker);
    assert!(marker.is_empty(), "the retry drops the marker once cleared");
    let (_, context) = db.get_status("nova").unwrap().unwrap();
    assert_eq!(context, "");

    // Ownership still holds on the retry path: a hook took the row while the
    // marker was being carried, so the retry must leave it alone.
    db.set_gate_status("nova", "tui:not-idle:stalled", "gate blocked 60s")
        .unwrap();
    let mut stale = "tui:not-idle:stalled".to_string();
    db.set_status("nova", ST_ACTIVE, "tool:Bash").unwrap();
    release_gate_context(&db, "nova", &mut stale);
    assert!(stale.is_empty(), "the row is a hook's now; we own nothing");
    let (status, context) = db.get_status("nova").unwrap().unwrap();
    assert_eq!(status, ST_ACTIVE);
    assert_eq!(context, "tool:Bash");

    drop(db); // tempdir cleans up behind it
}
```

`ST_ACTIVE` and `ST_LISTENING` are already imported by `delivery.rs`'s tests module (`use super::*` reaches the module's `crate::shared` import at line 16). `rusqlite::params!` is spelled out because that module imports only `super::*`.

- [ ] **Step 11: Run the whole suite**

Run: `cargo test`

Expected: PASS, 0 failed.

- [ ] **Step 12: Confirm the gate still reopens**

The failure this task must not introduce is the one D1 was: an escalation that becomes its own cause. Prove it on a live agent — hold the prompt occupied past the threshold, then clear it:

```bash
hcom agy --tag esc --go --hcom-prompt "Say exactly READY and nothing else."
# in the agy pane: type "hold" without pressing Enter, and leave it
hcom start --as escprobe
hcom send --name escprobe @esc-<name> --intent request -- reply with the single word LATE
# wait past 60s, then check the escalation landed
hcom list esc-<name>
hcom events --agent esc-<name> --last 5
# now clear the typed text in the pane
```

Expected: at ~60s `hcom list` shows **`listening`** with a `tui:prompt-has-text:stalled` context — not `blocked` — and one `delivery_blocked` event carrying the reason and duration. After the prompt is cleared, delivery completes with no further intervention and the context returns to normal. A status of `blocked` here, or a delivery that never resumes, is this task re-creating D1.

Then the two paths that end a block without the gate ever passing, which is where the persisted context leaks:

```bash
# queue drains before the gate opens: send, then let the agy hook take the
# message itself while the prompt is still occupied
hcom send --name escprobe @esc-<name> --intent inform -- drain me
hcom list esc-<name>
```

Expected: once nothing is pending, the context is empty — no `:stalled` left on the row — and the next block starts its own clock rather than escalating on its first poll.

```bash
hcom kill tag:esc
```

- [ ] **Step 13: Commit**

```bash
git add src/delivery.rs src/db/events.rs src/db/instances.rs
git commit -m "feat(delivery): escalate a gate that stays blocked

A blocked gate looked identical at two seconds and at two minutes: the
instance kept reading listening while nothing was delivered, and nothing
was emitted for a coordinator to observe. Past 60s of continuous
blocking, write a :stalled gate context and emit delivery_blocked with
the reason and duration.

Status is deliberately untouched. Setting ST_BLOCKED would make
is_idle() false, and the gate checks idleness first — the escalation
would become the reason delivery never resumed.

Both blocked-branch updaters now build their context through one
function, so the stalled suffix survives: the 2s updater writes whenever
its computed context differs from the last one written, and a suffix it
did not know about would be overwritten on the next poll while the
escalation latch fires only once. The block clock carries that latch
inside it so every site that clears the clock rearms for free, and the
no_pending and gate_pass paths now clear the persisted context too
instead of leaving a stalled row behind and donating elapsed time to the
next message.

Clearing that context is now compare-and-clear. A non-empty local marker
was never proof the row still held what we wrote: a hook can write
tool:Bash in between, and the unconditional set_gate_status(name, \"\",
\"\") erased it. All five clear sites route through one helper that puts
the comparison in the WHERE clause, and State::Idle retries a clear that
failed — without it, keeping the marker was a promise nothing kept,
since Idle only leaves for Pending when a message arrives."
```

---

### Task 9: measure whether a hand-started AGY can be woken (spike)

D3 in the spec is unmeasured. A vanilla `agy` joined with `hcom start` has `bindings: hooks` and no PTY delivery loop, so nothing injects into its TUI. Claude covers this with a blocking `hcom poll` in its Stop hook; whether agy can do the same is a claim in a code comment (`antigravity.rs:119-120`), not a measurement. **This task writes no production code** — it answers four questions and records them.

Two constraints the probe has to respect:

- **It must be an interactive session.** `agy -p` is print mode: one turn, then exit. It cannot show whether a `Stop` decision starts a *second* turn, which is the entire question. Run agy as a TUI in its own terminal and drive it by hand.
- **A marker alone proves nothing.** `PreInvocation` fires on the first turn too, before any `Stop` has run. The probe counts its invocations and injects the marker only from turn 2 onward, so a reply mentioning it can only have come from a turn the `Stop` decision started.

**Files:**
- Modify: `docs/superpowers/specs/2026-09-08-agy-wake-design.md` (the D3 section)

- [ ] **Step 1: Build the probe hooks**

Both hooks log every invocation with entry and exit timestamps — Q1 is read off that log, not inferred. The `Stop` hook caps its own continues so the measurement cannot hang.

```bash
mkdir -p /tmp/agy-stop-probe
cat > /tmp/agy-stop-probe/stop.sh <<'SH'
#!/bin/sh
dir=/tmp/agy-stop-probe
n=$(cat "$dir/stop.count" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$dir/stop.count"
start=$(date +%s)
# Entry is logged BEFORE the sleep on purpose. If agy kills the hook at its
# own 30s default, a line written after the sleep would never exist — and an
# absent line is exactly the Q1 failure case we are trying to observe. A
# stop#N with no matching stop#N-exit line means "terminated", which is an
# answer; inferring a duration from a missing record is not.
echo "stop#$n entered=$start sleep=${PROBE_SLEEP:-45}" >> "$dir/log"
# Q3: never emit more than 3 continues, so a missing loop guard in agy
# cannot turn this probe into an endless session.
if [ "$n" -gt 3 ]; then
  printf '{"decision":"allow"}'
  echo "stop#$n-exit at=$(date +%s) decision=allow (probe cap)" >> "$dir/log"
  exit 0
fi
sleep "${PROBE_SLEEP:-45}"
printf '{"decision":"continue","reason":"probe"}'
echo "stop#$n-exit at=$(date +%s) decision=continue" >> "$dir/log"
SH
cat > /tmp/agy-stop-probe/pre.sh <<'SH'
#!/bin/sh
dir=/tmp/agy-stop-probe
n=$(cat "$dir/pre.count" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$dir/pre.count"
# Q4: the marker must be unreachable on turn 1. PreInvocation fires there too,
# before any Stop has run, so a marker seen then would prove nothing.
stops=$(cat "$dir/stop.count" 2>/dev/null || echo 0)
if [ "$n" -ge 2 ]; then
  # The marker asks for an exact echo, so a reply that contains it cannot be
  # the model paraphrasing the conversation — and `after_stops` records which
  # Stop this turn followed, so a woken turn is distinguishable from a turn
  # the user started.
  printf '{"injectSteps":[{"ephemeralMessage":"Reply with exactly PROBE_DELIVERED and nothing else."}]}'
  echo "pre#$n injected marker at $(date +%s) after_stops=$stops" >> "$dir/log"
else
  printf '{}'
  echo "pre#$n first turn, no marker, at $(date +%s) after_stops=$stops" >> "$dir/log"
fi
SH
chmod +x /tmp/agy-stop-probe/stop.sh /tmp/agy-stop-probe/pre.sh
```

- [ ] **Step 2: Point a scratch agy config at them**

`GEMINI_CLI_HOME` is a raw prefix; hcom and agy both append `.gemini` (`runtime_env.rs:50-56`). This is the same `hooks.json` hcom's own installer writes and removes (`antigravity.rs:68-75`), so the scratch config exercises a path agy really reads.

```bash
mkdir -p /tmp/agy-stop-probe/home/.gemini/config
cat > /tmp/agy-stop-probe/home/.gemini/config/hooks.json <<'JSON'
{
  "probe": {
    "Stop": [
      { "name": "probe-stop", "type": "command",
        "command": "/tmp/agy-stop-probe/stop.sh", "timeout": 120 }
    ],
    "PreInvocation": [
      { "name": "probe-pre", "type": "command",
        "command": "/tmp/agy-stop-probe/pre.sh", "timeout": 15 }
    ]
  }
}
JSON
```

- [ ] **Step 3: Confirm the scratch config is actually loaded**

Before measuring anything, prove the probe is wired — otherwise every later "no" is indistinguishable from "the hooks never ran".

```bash
GEMINI_CLI_HOME=/tmp/agy-stop-probe/home PROBE_SLEEP=1 agy
```

In the TUI, send one message (`say hi`), let it finish, then quit and:

```bash
cat /tmp/agy-stop-probe/log
```

Expected: at least a `pre#1` line. No lines at all means **the probe is not wired** — nothing more. It does not show that a vanilla agy has no hook surface: the plugin route (`~/.gemini/config/plugins/hcom/hooks/hooks.json`) demonstrably works, so a scratch `hooks.json` that agy ignores is a fact about this scaffold. Record it as *inconclusive*, then retry through the plugin directory of the scratch home before drawing any D3 conclusion. Every later "no" in this task is only readable once this step says yes.

- [ ] **Step 4: Answer Q1 and Q2**

```bash
rm -f /tmp/agy-stop-probe/log /tmp/agy-stop-probe/*.count
GEMINI_CLI_HOME=/tmp/agy-stop-probe/home agy
```

Send one message, let the turn end, and watch the session. Then read the log.

Record: **Q1** — pair the `stop#1` line with its `stop#1-exit` line. Both present, ~45s apart: agy waited past its 30s default, so a blocking Stop is supported *to at least 45s* — the configured 120s is **not** established by this run, only the 45 that was slept. `stop#1` with no `stop#1-exit`: the hook did not run to completion — record it as **terminated or incomplete**, and nothing more. The gap to the next log line is not the cap: the next line can arrive arbitrarily later, and a crashed probe looks the same as a killed one. To claim a timeout *duration*, pair it with an independent observation — agy's own diagnostic for a timed-out hook, or a watch on the process — and if neither is available, report the ceiling as unmeasured. To pin the ceiling, re-run with `PROBE_SLEEP` above the value you want to prove and read the same pair.

**Q2** — did the TUI start another turn after the hook returned `continue`, or did the session end? A `pre#2` line whose `after_stops=1` is preceded by a `stop#1-exit decision=continue` corroborates it. `after_stops` counts hook *entries*, not successful returns, so it is read together with that exit line, never on its own.

- [ ] **Step 5: Answer Q3 — is there a loop guard?**

```bash
rm -f /tmp/agy-stop-probe/log /tmp/agy-stop-probe/*.count
GEMINI_CLI_HOME=/tmp/agy-stop-probe/home PROBE_SLEEP=1 agy
```

Send one message and let it run. Record how many `decision=continue` lines agy honoured before stopping anyway. A `decision=allow (probe cap)` line means agy honoured all three and the probe stopped first — agy's own guard, if any, is above 3.

- [ ] **Step 6: Answer Q4 — does the woken turn deliver?**

Same run as Step 5. Three records, and they answer different things:

- No `pre#2` line: no second turn ran `PreInvocation`. Q4 is unreachable; Q2 is the failure.
- `pre#2` present, with a `stop#N-exit decision=continue` before it and no user submission in between: a turn ran and followed a `Stop` that actually returned `continue`. That correlation is corroboration for a woken turn, not proof — `after_stops` counts entries, so the exit line is the half that matters.
- The reply: the marker asks for an exact echo, so `PROBE_DELIVERED` in the reply is delivery. Its **absence is inconclusive, not proof of failure** — a model can be handed an ephemeral message and answer something else. If the echo is missing while `pre#2` is present, re-run before concluding, and record it as unconfirmed transport rather than absent transport.

- [ ] **Step 7: Record the answers in the spec**

Replace the four numbered questions in D3 with the measured answers and their date, then write the acceptance criteria for whichever branch the measurement supports — a blocking `handle_sessionend` if all four hold, or a plain statement at `hcom start` that a vanilla agy will not wake if any fails. Note explicitly if Q1 came back at agy's default rather than the configured timeout: that caps how long a blocking Stop can wait and belongs in the design, not just the log.

- [ ] **Step 8: Clean up and commit**

```bash
rm -rf /tmp/agy-stop-probe
git add docs/superpowers/specs/2026-09-08-agy-wake-design.md
git commit -m "docs: measure agy's Stop hook against the D3 questions"
```

---

## Verification

After every task:

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

Expected: tests pass with 0 failed, clippy clean, formatting unchanged.

After Tasks 1–3, the reported bug is closed. After Tasks 5–7, `hcom hooks add claude` and `hcom hooks add cursor` from this checkout both point at the fork it tracks. Manifest changes under `plugin/` need `git push <fork> <branch>:main` plus `claude plugin marketplace update` / `cursor-agent plugin marketplace update` before either tool sees them; Rust changes do not, because the hooks only invoke `hcom` from `PATH`.
