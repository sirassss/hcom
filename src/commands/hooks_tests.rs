use super::*;

/// No state reachable without a usable inventory may read as active or as
/// an observation that both hook sets are firing.
#[test]
fn add_exit_code_puts_failure_above_pending() {
    assert_eq!(super::add_exit_code(0, 0), 0);
    assert_eq!(super::add_exit_code(0, 2), 2);
    assert_eq!(super::add_exit_code(1, 0), 1);
    assert_eq!(
        super::add_exit_code(1, 3),
        1,
        "a failure must not report as pending"
    );
}

/// Codex is routed explicitly in `cmd_hooks_add`, not through the
/// plugin-tool fast path — that path strips the legacy entries, and
/// Codex's are the only hooks firing until the user trusts the plugin.
#[test]
fn codex_never_reaches_the_automatic_legacy_strip() {
    assert!(
        !Tool::Codex.hooks_ship_as_plugin(),
        "Codex in the plugin fast path would strip its legacy hooks on `hooks add`"
    );
}

#[test]
fn codex_states_without_evidence_never_claim_hooks_are_firing() {
    use crate::hooks::codex::{CodexPluginState, CodexPluginStatus};

    for state in [
        CodexPluginState::Unverified,
        CodexPluginState::Discovered,
        CodexPluginState::Missing,
        CodexPluginState::Incomplete,
        CodexPluginState::Incompatible,
    ] {
        let status = CodexPluginStatus {
            state,
            details: vec!["plugin store populated: /codex-home/plugins/cache".to_string()],
        };
        let rendered = super::codex_status_lines(
            &status,
            "/codex-home/hooks.json",
            &crate::hooks::codex::ClaudePresence::Absent,
        )
        .join("\n");
        assert!(
            !rendered.contains("hooks active") && !rendered.contains("both are firing"),
            "{state:?} rendered as an activation claim: {rendered}"
        );
        // The legacy path is evidence of nothing in these states, so it
        // must not appear next to the headline.
        assert!(
            !rendered.lines().next().unwrap().contains("hooks.json"),
            "{state:?} named the legacy file without observing it: {rendered}"
        );
    }
}

#[test]
fn codex_duplicate_names_the_legacy_file_to_remove() {
    use crate::hooks::codex::{CodexPluginState, CodexPluginStatus};

    let rendered = super::codex_status_lines(
        &CodexPluginStatus {
            state: CodexPluginState::Duplicate,
            details: Vec::new(),
        },
        "/codex-home/hooks.json",
        &crate::hooks::codex::ClaudePresence::Absent,
    )
    .join("\n");
    assert!(
        rendered.contains("duplicate hooks; double-fire risk"),
        "{rendered}"
    );
    assert!(rendered.contains("/codex-home/hooks.json"), "{rendered}");
}

#[test]
fn status_flags_plugin_and_legacy_coexisting() {
    let line = super::plugin_status_line("claude", /* plugin */ true, /* legacy */ true);
    assert!(line.contains("double-fire"), "{line}");
    // `hooks add` handles this state: it finds the plugin installed and
    // strips the leftover legacy entries. `hooks remove` would be wrong
    // advice — it takes the plugin down too, leaving no hooks at all.
    assert!(line.contains("hcom hooks add claude"), "{line}");
    assert!(!line.contains("hooks remove"), "{line}");
}

/// When the verifier says no, `hooks add` must reach `try_setup_hooks`
/// instead of printing "already installed" — the originally reported bug
/// this plan's verifier tightening exists to fix.
#[test]
fn add_does_not_short_circuit_when_the_verifier_says_no() {
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Cursor,
        false,
        true,
        false,
        true,
        false
    ));
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Claude,
        false,
        true,
        false,
        true,
        false
    ));
}

/// M1/M2: `cursor-agent` reads Claude's plugin cache directly, so
/// `hooks add cursor` must short-circuit — no CLI call, no second
/// install — the moment Claude's plugin covers it, even though Cursor's
/// own verifier (`hooks_installed` here) still says false.
#[test]
fn add_cursor_short_circuits_when_claude_covers() {
    assert!(super::plugin_add_can_short_circuit(
        Tool::Cursor,
        false, // verify_cursor_plugin_installed() — Cursor has no cache of its own
        true,
        true,  // cursor_hooks_covered() — Claude's plugin covers it
        false, // cursor_registry_lists_hcom() — irrelevant, Claude's coverage takes over
        false,
    ));
}

/// Without Claude's coverage, Cursor with no cache of its own must still
/// go through the normal install path.
#[test]
fn add_cursor_still_installs_without_claude() {
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Cursor,
        false,
        true,
        false,
        true,
        false,
    ));
}

/// M5: `cursor-agent plugin marketplace remove hcom` run outside hcom
/// leaves the on-disk cache (`.cache-complete`, `hooks-cursor.json`, skill
/// payload) completely untouched (plan M3), so
/// `verify_cursor_plugin_installed()` — `hooks_installed` here — still
/// reads true. Without a registry check, `hooks add cursor` would read
/// that stale cache as "already installed" and permanently block
/// reinstall. Claude does not cover it here either, so the only signal
/// left is the registry: `cursor_registry_lists_hcom()` (mocked via
/// `HCOM_TEST_CURSOR_MARKETPLACE_LIST` in `hooks/plugin.rs` tests) says
/// hcom is not registered, so the stale cache must not short-circuit.
#[test]
fn add_cursor_reinstalls_when_only_a_stale_cache_remains() {
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Cursor,
        true, // verify_cursor_plugin_installed() — stale, orphaned cache
        true,
        false, // cursor_hooks_covered() — Claude does not cover it
        false, // cursor_registry_lists_hcom() — registry does not confirm
        false,
    ));
    // Contrast: the same stale-looking cache short-circuits once the
    // registry actually confirms it — the ordinary healthy-install path
    // this must not break.
    assert!(super::plugin_add_can_short_circuit(
        Tool::Cursor,
        true,
        true,
        false,
        true, // registry confirms
        false,
    ));
}

/// Task 6 (already merged) rewrote `verify_agy_plugin_installed()` to
/// require BOTH an `import_manifest.json` entry naming hcom's hooks
/// component AND the hook file on disk — an orphan hook file left behind
/// after `agy plugin uninstall` (or extracted by hand) with no manifest
/// entry now makes it return false, so `hooks_installed` here is already
/// false in that state. This locks that in at the `cmd_hooks_add`
/// short-circuit level: an orphan dir must never read as "already
/// installed" and block reinstall.
#[test]
fn add_antigravity_reinstalls_over_orphan_dir() {
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Antigravity,
        false, // verify_agy_plugin_installed() — orphan dir, no manifest entry
        true,  // skill payload files may still be present on disk
        false,
        true,
        false,
    ));
}

/// Task 3: `hcom hooks add cursor --own` must force a Cursor-owned
/// install even when Claude's plugin already covers Cursor — the escape
/// hatch `cursor_covered_by_claude_message` promises. Before this flag
/// was wired up, `--own` was silently ignored by `cmd_hooks_add` (only
/// `argv[0]` is inspected for the tool name), so this state still
/// short-circuited and never reached `try_setup_hooks`.
#[test]
fn add_cursor_force_installs_despite_claude() {
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Cursor,
        false, // Cursor has no cache of its own
        true,
        true, // Claude's plugin covers it
        true, // cursor_registry_lists_hcom() — irrelevant, --own forces install anyway
        true, // --own forces the real install anyway
    ));
}

/// M5's trap can also be reached with Claude NOT covering it: a stale
/// on-disk cache reads `hooks_installed == true`, and `cursor-agent`
/// erroring out makes `cursor_registry_lists_hcom()` fail open to `true`
/// (correct for its original uninstall caller, wrong here) — so
/// `cursor_registry_confirms` is `true` too. Before this fix, `--own`
/// only suppressed the Claude-covered disjunct and did nothing here, so
/// this exact state still short-circuited to "already installed" even
/// with `--own` passed — the trap Task 5 was built to close, just
/// relocated behind a CLI failure instead of a registry removal.
#[test]
fn add_cursor_force_installs_despite_stale_cache_and_broken_registry_check() {
    assert!(!super::plugin_add_can_short_circuit(
        Tool::Cursor,
        true, // verify_cursor_plugin_installed() — stale on-disk cache
        true,
        false, // cursor_hooks_covered() — Claude does NOT cover it
        true,  // cursor_registry_lists_hcom() fails open on a CLI error
        true,  // --own must still force the real install
    ));
}

#[test]
fn cursor_covered_by_claude_message_explains_and_names_the_force_flag() {
    let msg = super::cursor_covered_by_claude_message();
    assert!(msg.contains("Claude"), "{msg}");
    assert!(
        msg.contains("Removing hcom from Claude") && msg.contains("remove Cursor's hook"),
        "must warn that removing hcom from Claude takes Cursor's hook with it: {msg}"
    );
    assert!(
        msg.contains("hooks add cursor --own"),
        "must name the forced-install path: {msg}"
    );
}

#[test]
fn agy_hooks_without_skill_payload_require_reinstall() {
    assert!(
        !super::plugin_add_can_short_circuit(Tool::Antigravity, true, false, false, true, false),
        "hook presence alone must not bypass AGY repair"
    );
    let line = super::agy_skill_payload_status_line(Err(
        "missing skill payload skills/hcom-agent-messaging/SKILL.md".to_string(),
    ));
    assert!(line.contains("hooks present"), "{line}");
    assert!(line.contains("skill payload"), "{line}");
    assert!(line.contains("hcom hooks add antigravity"), "{line}");
}

#[test]
fn complete_agy_payload_can_use_the_installed_shortcut() {
    assert!(super::plugin_add_can_short_circuit(
        Tool::Antigravity,
        true,
        true,
        false,
        true,
        false,
    ));
    assert!(super::agy_skill_payload_status_line(Ok(())).is_empty());
}

/// Terse constructor for `cursor_status_line` tests — every field named
/// at the call site instead of counting positions.
fn cursor_status(
    installed: bool,
    legacy: bool,
    claude_covers: bool,
    broken_skill_payload: Option<&str>,
) -> super::CursorStatus {
    super::CursorStatus {
        installed,
        legacy,
        claude_covers,
        broken_skill_payload: broken_skill_payload.map(str::to_string),
    }
}

/// Cursor cannot take the same advice: its verifier only proves the
/// plugin materialized into Cursor's plugin cache, so `hooks add` must
/// not strip on its behalf. The user removes the legacy entries once
/// they have enabled the plugin in the TUI.
/// Cross-vendor review (verify-taro, a Cursor agent) caught this: the
/// advice told the user to run `hooks remove cursor`, which uninstalls the
/// plugin *and* strips the legacy entries — following it left Cursor with
/// no hooks at all. The Claude branch's own test says "hooks remove would
/// be wrong — it takes the plugin down too"; the Cursor branch contradicted
/// it.
#[test]
fn cursor_advice_never_sends_the_user_to_a_full_removal() {
    let line = super::cursor_status_line(&cursor_status(true, true, false, None));
    let cmd = line
        .split("run: ")
        .nth(1)
        .and_then(|rest| rest.split(" (").next())
        .expect("advice must contain a `run: <command>`");
    assert_eq!(
        cmd, "hcom hooks remove cursor --legacy-only",
        "the command handed to the user must spare the plugin"
    );
    assert!(
        line.contains("leaving no hooks"),
        "must say why the plain command is wrong: {line}"
    );
}

#[test]
fn status_tells_cursor_to_remove_only_after_enabling() {
    let line = super::cursor_status_line(&cursor_status(true, true, false, None));
    assert!(line.contains("double-fire"), "{line}");
    assert!(line.contains("hcom hooks remove cursor"), "{line}");
    assert!(
        line.contains("/plugins"),
        "must say when it is safe: {line}"
    );
    assert!(!line.contains("hooks add"), "{line}");
}

#[test]
fn status_flags_missing_install() {
    let line = super::cursor_status_line(&cursor_status(false, false, false, None));
    assert!(line.contains("hcom hooks add cursor"), "{line}");
}

#[test]
fn status_is_quiet_when_only_the_plugin_is_present() {
    let line = super::plugin_status_line("agy", true, false);
    assert!(
        line.is_empty(),
        "healthy state needs no advice, got: {line}"
    );
}

#[test]
fn plugin_status_line_cursor_cache_present_does_not_claim_enabled() {
    // A materialized plugin cache proves an install happened, but Task 6
    // re-measured that it still doesn't prove the plugin is *enabled* in
    // the TUI right now (unmeasured whether the cache survives a
    // disable) — so status must not go silent the way it does for the
    // other plugin tools' healthy (true, false) state.
    let line = super::cursor_status_line(&cursor_status(true, false, false, None));
    assert!(
        line.contains("/plugins"),
        "must say where to confirm the enabled state. got: {line:?}"
    );
    assert!(
        line.contains("cannot confirm"),
        "a plugin cache must not be reported as proof the plugin is enabled. \
         got: {line:?}"
    );
}

/// Branch 1 (Task 8): Claude's plugin definitively covers the hook and
/// Cursor has no cache of its own — the whole point of Task 1's
/// `cursor_hooks_covered()`. Must be definitive (no "may still be"
/// hedging), name the no-separate-install-needed fact, and name what
/// removing hcom from Claude does to it.
#[test]
fn cursor_status_line_claude_covers_with_no_own_cache() {
    let line = super::cursor_status_line(&cursor_status(false, false, true, None));
    assert!(
        line.contains("Claude") && line.contains("no separate"),
        "must state plainly that no separate Cursor install is needed: {line:?}"
    );
    assert!(
        line.contains("Removing hcom from Claude") && line.contains("remove Cursor's hook"),
        "must name the removal consequence: {line:?}"
    );
    assert!(
        line.contains("hooks add cursor --own"),
        "must still name the force-install escape hatch: {line:?}"
    );
    assert!(
        !line.contains("may still be"),
        "Task 1 makes this definitive; must not hedge: {line:?}"
    );
}

/// Branch 2 (Task 8): neither Claude nor Cursor's own cache covers it —
/// genuinely not running. No more "Cursor may still be running out of
/// Claude's cache" speculation once `cursor_hooks_covered()` says no.
#[test]
fn cursor_status_line_neither_covers_it() {
    let line = super::cursor_status_line(&cursor_status(false, false, false, None));
    assert!(line.contains("hcom hooks add cursor"), "{line:?}");
    assert!(
        !line.contains("may still be running"),
        "with cursor_hooks_covered() false, there is nothing left to hedge about: {line:?}"
    );
}

/// Companion headline fix for the same broken-payload state as
/// `cursor_status_line_claude_covers_and_own_cache_broken_states_both_facts`
/// / `cursor_status_line_broken_payload_without_claude_names_the_entry`:
/// the headline `hooks status` prints one line above that advice must not
/// say "no plugin cache" when a cache entry does exist, broken payload or
/// not — it gets its own headline instead of collapsing into the
/// genuinely-empty case.
#[test]
fn status_headline_distinguishes_broken_cache_from_no_cache() {
    assert_eq!(
        super::plugin_status_headline(true, false, false),
        "no plugin cache"
    );
    assert_eq!(
        super::plugin_status_headline(true, false, true),
        "plugin cache incomplete"
    );
    assert_eq!(
        super::plugin_status_headline(true, true, false),
        "plugin cache ready"
    );
    // A non-Cursor tool ignores the broken-payload bit entirely — only
    // Cursor's verifier folds two distinct states into one `false`.
    assert_eq!(
        super::plugin_status_headline(false, false, true),
        "not installed"
    );
}

/// Branch 4 (Task 8, the deferred Minor from Task 11's review): a Cursor
/// cache entry exists with a broken skill payload, but Claude's plugin
/// separately covers the hook. Both true facts must survive in the same
/// message without contradicting each other — the old code printed "no
/// Cursor plugin cache" right next to "a cache entry exists".
#[test]
fn cursor_status_line_claude_covers_and_own_cache_broken_states_both_facts() {
    let line = super::cursor_status_line(&cursor_status(
        false,
        false,
        true,
        Some("missing skill payload skills/hcom-agent-messaging/SKILL.md"),
    ));
    assert!(
        line.contains("Claude") && line.contains("no separate"),
        "must still state Claude's coverage plainly: {line:?}"
    );
    assert!(
        line.contains("skill payload") && line.contains("broken"),
        "must also name the broken own-cache attempt: {line:?}"
    );
    assert!(
        !line.contains("no Cursor plugin cache") && !line.contains("no plugin cache"),
        "must not claim no cache exists when one does, broken or not: {line:?}"
    );
}

/// A cache entry with a broken skill payload but no Claude coverage: the
/// hook wiring (`hooks-cursor.json`) is independent of the skill files,
/// so it can still be firing even though the skill is absent — this must
/// not collapse into the generic "no cache" wording.
#[test]
fn cursor_status_line_broken_payload_without_claude_names_the_entry() {
    let line = super::cursor_status_line(&cursor_status(
        false,
        false,
        false,
        Some("missing skill payload skills/hcom-agent-messaging/SKILL.md"),
    ));
    assert!(line.contains("/plugins"), "{line:?}");
    assert!(
        line.contains("hook") && line.contains("may still be running"),
        "must say the hook can still be firing even though the skill is absent: {line:?}"
    );
    assert!(
        !line.contains("no Cursor plugin cache") && !line.contains("no plugin cache"),
        "a cache entry does exist, broken payload or not: {line:?}"
    );
}

/// Claude covers the hook AND legacy entries are also present: both are
/// genuinely firing now (Task 1 makes this an observation, not a guess),
/// so the double-fire risk and its `--legacy-only` remedy must both
/// survive alongside the Claude-coverage statement.
#[test]
fn cursor_status_line_claude_covers_with_legacy_also_present() {
    let line = super::cursor_status_line(&cursor_status(false, true, true, None));
    assert!(line.contains("Claude"), "{line:?}");
    assert!(
        line.contains("Legacy") && line.contains("hcom hooks remove cursor --legacy-only"),
        "must still name the legacy remedy: {line:?}"
    );
}

#[test]
fn cursor_skill_payload_status_line_names_reinstall_and_live_hook_risk() {
    // Task 7 folded the skill-payload check into
    // `verify_cursor_plugin_installed`, so a cache entry with the two
    // marker files but a broken skill payload collapses into the same
    // `installed == false` bucket as "no cache at all" — the generic
    // `plugin_status_line` message ("no Cursor plugin cache") is then
    // inaccurate: a cache entry *does* exist, it is just missing its
    // skill. This line names that specific case instead.
    let line = super::cursor_skill_payload_status_line(Some(
        "missing skill payload skills/hcom-agent-messaging/SKILL.md".to_string(),
    ));
    assert!(
        line.contains("/plugins"),
        "must point at the TUI picker: {line:?}"
    );
    assert!(
        line.to_lowercase().contains("hcom"),
        "must name the plugin to install: {line:?}"
    );
    assert!(
        line.contains("hook") && line.contains("may still be running"),
        "must say the hook can still be firing even though the skill is absent: {line:?}"
    );
}

#[test]
fn cursor_skill_payload_status_line_is_quiet_when_healthy() {
    assert!(super::cursor_skill_payload_status_line(None).is_empty());
}

#[test]
fn plugin_status_line_cursor_double_fire_is_a_risk_not_an_observation() {
    let line = super::cursor_status_line(&cursor_status(true, true, false, None));
    assert!(
        !line.contains("both are firing"),
        "hcom cannot see whether the plugin is enabled, so it cannot say both \
         are firing. got: {line:?}"
    );
    assert!(line.contains("--legacy-only"), "got: {line:?}");
}

/// Branch 5 (Task 8): no own cache, no Claude coverage, no broken
/// payload — just legacy entries. This was already definitive before
/// Task 1 and should not change.
#[test]
fn plugin_status_line_cursor_no_cache_with_legacy_names_what_fires() {
    let line = super::cursor_status_line(&cursor_status(false, true, false, None));
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

#[test]
fn test_detect_current_tool_default() {
    // In test env, none of the AI tool vars should be set
    // (unless running inside one, which is fine — it'll detect it)
    let tool = detect_current_tool();
    let parsed = tool
        .parse::<Tool>()
        .expect("detected tool must be canonical");
    assert!(
        parsed == Tool::Adhoc || hook_tools().contains(&parsed),
        "unexpected tool: {tool}"
    );
}

/// The printed tool list for `hcom config auto_approve` must name exactly
/// what `refresh_installed_hook_permissions` can touch: every plugin tool
/// excluded, since a plugin manifest carries no permission argument, and
/// nothing hardcoded to drift out of sync with `hook_tools()`.
#[test]
fn auto_approve_managed_tools_excludes_plugin_tools() {
    let managed = super::auto_approve_managed_tools();
    assert!(!managed.is_empty());
    for tool in &managed {
        assert!(
            !tool.hooks_ship_as_plugin(),
            "{} ships as a plugin and must not be listed",
            tool.as_str()
        );
    }
    for tool in super::hook_tools() {
        assert_eq!(
            managed.contains(&tool),
            !tool.hooks_ship_as_plugin(),
            "{} membership disagrees with hooks_ship_as_plugin",
            tool.as_str()
        );
    }
}

/// `hcom config auto_approve <v>` must not reach a plugin tool's installer.
/// That installer shells out to the tool's CLI and clones a marketplace
/// over the network, and it cannot apply a permission setting anyway — a
/// plugin manifest carries no permissions. Regression guard: this is the
/// fourth side-effect install path found in this migration, after the
/// launcher, bare start, and hooks add.
#[test]
fn permission_refresh_skips_plugin_tools() {
    let plugin_tools: Vec<_> = super::hook_tools()
        .into_iter()
        .filter(|t| t.hooks_ship_as_plugin())
        .collect();
    assert!(
        !plugin_tools.is_empty(),
        "expected at least one plugin-backed tool"
    );
    for tool in plugin_tools {
        assert!(
            tool.hooks_ship_as_plugin(),
            "{} must be skipped by refresh_installed_hook_permissions",
            tool.as_str()
        );
    }
}
