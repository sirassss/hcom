# Ponytail cleanup Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking. This document authorizes no dispatch or publication.

**Goal:** Remove duplicated implementation in the reviewed changes while preserving their observable behavior.

**Architecture:** Reuse the existing lifecycle grouping function; represent Codex install decisions as ordinary values; build AGY event arrays directly in their final JSON map. Keep vendor adapters and all installation/trust boundaries.

**Tech Stack:** Existing Rust, serde_json, cargo tests. No new dependency.

**Spec:** The behavior constraints below supplement [TUI design](../specs/2026-09-11-tui-message-pane-redesign-design.md), [Codex design](../specs/2026-09-13-codex-plugin-skill-and-session-lifecycle-design.md), and [committed skill copies](../specs/2026-09-14-committed-plugin-skill-copies.md).

## Review scope and verdict

Inclusive review: `c12ba4d2d5d31ee68fb29527b343d2567d956090^..05302da8c6c3b54add4ed2335ba1a9b225b5063a`, 11 commits. Findings below refer to file lines at `05302da`, before the upstream merge.

The range adds 20,221 and removes 1,289 lines. Of the additions, 8,488 are design/plan/measurement documents, 2,192 are generated plugin skill copies, and 8,869 are Rust source including unit tests. Raw diff size substantially overstates runtime complexity. No dependency was added.

| Priority | Location at reviewed head | Finding | Minimal replacement |
|---|---|---|---|
| P2 | `src/tui/inline/eject.rs:458`, `src/tui/render/messages.rs:277` | `shrink:` same lifecycle grouping rule implemented twice, with two row representations. A future threshold/boundary change can diverge between viewports. | Group borrowed feed items with existing `group_lifecycle`, then snapshot only the inline rows that survive grouping. Keep owned replay state. |
| P3 | `src/hooks/codex.rs:1590`, `src/hooks/codex.rs:1657` | `yagni:` `Result<CodexAddOutcome, CodexAddPlan>` uses `Err(InstallNatively)` for a successful planning decision; the error enum has one variant. | Return one `CodexAddPlan` enum: `Report(CodexAddOutcome)` or `InstallNatively`. Reserve `Result` for actual installation failure. |
| P3 | `src/hooks/antigravity.rs:241` | `shrink:` event arrays are accumulated in a vector with a linear lookup, then copied into a JSON map. The intermediate event-order policy has no effect: serde_json uses its sorted map with current features. | Append directly through `serde_json::Map::entry`, retaining array insertion order. |
| P3 | `src/tui/filter.rs:8`, `src/tui/filter.rs:225`, `src/tui/state.rs:81` | `delete:` task-in-progress `allow(dead_code)` remains after consumers landed; `describe()` is only called by one test. | Remove obsolete suppression/comments and the unused wrapper; test `describe_with` directly. |

Estimated production reduction: approximately 40–80 lines, not a measured patch result. The main gain is one lifecycle grouping policy and a clearer install decision. A whole subsystem rewrite is not justified.

### Complexity that should stay

- Real skill copies are an explicit owner decision and prevent Codex silently skipping a symlink. Keep the sync script, byte comparison, and release payload gate.
- Keep AGY staging: the current committed spec explicitly retains it. Its canonical-path boundary, cycle rejection, cleanup, and destination checks are not deletion candidates in this behavior-preserving cleanup.
- Keep Codex's activation/trust/origin states. A plugin directory is not proof of active hooks; collapsing these states would weaken diagnosis and migration safety.
- Keep Cursor process-lifetime handling, session aliases, and explicit alias cleanup for older DB layouts. Keep delivery gate ownership checks and the continuous-block timer.
- Keep TUI's shared filter and bounded replay queue. Sharing pure predicates already removed the separate FTS search route; owned replay rows are necessary across render ticks.
- Keep remote resolution and SSH-host normalization: this checkout really uses the `sirassss` SSH alias. Avoid replacing this with a hardcoded upstream URL.
- Historical measurements explain vendor differences. Prefer concise comments linking to them in future work; no mass deletion of history is proposed.

## Global constraints / cleanup specification

- No feature removal, CLI wording/exit-code change, added dependency, schema migration, or installer side effect.
- Lifecycle collapse remains 3+ consecutive activity events, same raw owner and absolute minute; a message/tool/different owner/minute breaks the run. Filter and sort before grouping. Live grouping remains batch-local.
- Preserve inline watermarks for filtered-out items and the owned replay snapshot across data reloads, resize, and chunk boundaries.
- Plugin install still verifies before stripping legacy hooks. Codex still requires user trust; plugin entries must never become eligible for native hook trust bypass.
- Canonical skills remain authored under `skills/hcom-agent-messaging/`; generated adapter trees remain byte-identical real files.
- Cleanup is planned only. The separate upstream merge is authorized for execution in this session.

## Task 1: Reuse lifecycle grouping and remove stale TUI scaffolding

**Files:** `src/tui/inline/eject.rs`, `src/tui/filter.rs`, `src/tui/state.rs`. Reuse `src/tui/render/messages.rs::group_lifecycle` and its `Row` type without adding a new abstraction.

**Interfaces:** `collect_new_items<'a>(..., data: &'a DataState, ...) -> Vec<FeedItem<'a>>`; `group_eject_rows(items: Vec<FeedItem<'_>>) -> VecDeque<ReplayRow>`. The latter produces the same owned queue consumed by `eject_replay_chunk`.

- [ ] Keep existing grouping and replay tests; extend their fixture cases with a tool/message boundary, a different owner, and equal clock time on different dates. Assert concrete row counts/IDs, including runs of lengths 2 and 3.
- [ ] Run `cargo test --bin hcom tui:: -- --test-threads=1` and record the baseline.
- [ ] Let `begin_replay` pass `filter::collect_items` directly to `group_eject_rows`. Return borrowed `FeedItem` from `collect_new_items`, keeping both watermark updates exactly where they are. Sort with `order_cmp` directly.
- [ ] Replace the duplicate grouping loop with this adapter (import existing `Row` and `group_lifecycle`):

```rust
fn group_eject_rows(items: Vec<FeedItem<'_>>) -> VecDeque<ReplayRow> {
    group_lifecycle(&items)
        .into_iter()
        .map(|row| match row {
            Row::Item(FeedItem::Msg(m)) => ReplayRow::Item(EjectItem::Msg((*m).clone())),
            Row::Item(FeedItem::Ev(e)) => ReplayRow::Item(EjectItem::Ev((*e).clone())),
            Row::LifecycleRun { agent, time, count } =>
                ReplayRow::LifecycleRun { agent, time, count },
        })
        .collect()
}
```

- [ ] Update existing tests that construct `EjectItem` inputs to pass borrowed feeds. Remove `as_feed` only if its remaining callers disappear; keep `row_id` if replay still consumes it. No additional cloning before grouping.
- [ ] Delete module-wide stale `allow(dead_code)` in `filter.rs`, `tag_of`'s task-only suppression, and `MsgFilter::describe`. Change its sole test to `MsgFilter::default().describe_with(&str::to_string)`.
- [ ] Run the TUI test command above and `cargo check`. Verify replay cancellation, limits, ordering, and watermarks remain covered. Commit only these TUI changes.

## Task 2: Make Codex install planning an ordinary enum

**File:** `src/hooks/codex.rs`, including its existing `add_routes_every_state_without_installing_on_its_own` test.

**Interfaces:** `plan_codex_add(...) -> CodexAddPlan`; `add_codex_plugin() -> Result<CodexAddOutcome, String>` remains unchanged.

- [ ] Run `cargo test --bin hcom add_routes_every_state -- --test-threads=1` as baseline; retain every state/Claude-presence combination and message assertion.
- [ ] Replace the single-variant enum with:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodexAddPlan {
    Report(CodexAddOutcome),
    InstallNatively,
}
```

- [ ] In `plan_codex_add` change report returns from `Ok(outcome)` to `CodexAddPlan::Report(outcome)`, and installation from `Err(CodexAddPlan::InstallNatively)` to `CodexAddPlan::InstallNatively`. In its executor match `Report(outcome) => Ok(outcome)` and the install variant directly. Do not alter decision conditions or diagnostics.
- [ ] Update the existing assertions to the new variants. Run `cargo test --bin hcom hooks::codex:: -- --test-threads=1` and `cargo test --bin hcom commands::hooks:: -- --test-threads=1`. Commit this independently.

## Task 3: Construct AGY event arrays directly

**File:** `src/hooks/antigravity.rs::try_setup_antigravity_hooks`. Existing payload-equivalence coverage lives in `src/hooks/plugin.rs`.

**Interfaces:** No signature change. Produce the same `hcom-lifecycle` JSON object.

- [ ] Run `cargo test --bin hcom hooks::antigravity:: -- --test-threads=1` and `cargo test --bin hcom hooks::plugin:: -- --test-threads=1` as baseline.
- [ ] Initialize `let mut lifecycle_map = serde_json::Map::new();` before iterating `AGY_HOOK_CONFIGS`. Keep construction of each `hook` and `entry`. Replace the vector lookup and final conversion loop with:

```rust
lifecycle_map
    .entry(event.to_string())
    .or_insert_with(|| Value::Array(Vec::new()))
    .as_array_mut()
    .expect("lifecycle entries are arrays")
    .push(entry);
```

- [ ] Keep `let hcom_lifecycle = Value::Object(lifecycle_map);`. Update the event-order comment to promise only order within each event array.
- [ ] Rerun both commands and `cargo test --test plugin_payload`. The existing tests must still cover both PreInvocation handlers, matcher nesting, fallbacks, and unchanged committed payloads. Commit independently.

## Upstream assessment and integration record

Fetched `upstream/main` at `fabb309`. It contains 16 commits absent by ancestry from `05302da`:

| Commits | Value / merge decision |
|---|---|
| `92546ff` | Keep live processes registered; worthwhile reliability fix. |
| `d66d995`, `caa1b53` | Remove passive identity stealing and align native/adhoc reclaim tests. Worthwhile; retain fork's plugin preflight and explicit native-session fixtures. |
| `11fe1ba` | Filtered event wait survives unrelated unread messages. Worthwhile. |
| `24d05fd` | Remote reply origin uses relay metadata. Worthwhile. |
| `fc2276d`, `8719445` | Honest model-history output, literal/unambiguous transcript prefixes. Worthwhile. |
| `dc633b5`, `fabb309` | Effective child CODEX_HOME and selected Node runtime precedence. Worthwhile; overlap with fork fixes, upstream handles more edge cases. |
| `4a0c408` | Kimi lifecycle/delivery acknowledgements and tool-family ownership checks. Worthwhile correctness changes accompany the module split. |
| `9a0f3be`, `d542a9f` | Windows reset archive/connection lifetime and PowerShell runner fixes. Keep for cross-platform support; Linux tests do not prove Windows runtime behavior. |
| `db9ab43` | Additive JSON send output for programmatic callers. Useful, no dependency. |
| `b40de98` | Optional Ptyxis terminal support. Limited additive scope. |
| `1de31a1`, `7a08f90` | Preview reminder and documentation. Low integration cost. |

Merge conflicts: `src/commands/start.rs`, `src/delivery.rs`, `src/launcher.rs`, `src/tools/codex_preprocessing.rs`.

Resolution policy: keep upstream native identity/environment/Node rules; retain fork plugin launch guard and delivery stall escalation alongside upstream liveness refresh. Pass effective child home and working directory into the Codex plugin inventory too, so its decision agrees with upstream native hook setup. Update the missing-plugin start fixture to carry a native Claude session ID.

Baseline parallel suite: 2,364 unit tests passed, one shell-env test failed, one ignored; integration targets passed. First merged parallel run showed an outdated native-start fixture plus two environment-race failures in launcher tests. Final verification is recorded after the completed merge, with tests serialized because environment variables are process-global. No live vendor install, publication, or push is part of this integration.

### Verification evidence

- Regression `plugin_status_uses_effective_child_home`: first failed with `Unverified` instead of `LegacyOnly` when the wrapper ignored its child-home argument; passed after inventory, source-path classification, and plugin-store discovery all used that argument.
- Full serialized run: 2,411 unit tests passed, one ignored, one pre-existing failure in `shell_env::tests::resolver_discards_stderr_without_breaking_env_resolution`; every integration target passed.
- The shell failure also reproduces alone. This host has an executable `~/.local/bin/env` that only adjusts PATH; login-shell resolution selects it instead of the system `env`, so `env -0` emits no environment entries. No shell configuration was changed by this review.
- Final scoped verification command: `cargo test --bins --tests --no-fail-fast -- --test-threads=1 --skip shell_env::tests::resolver_discards_stderr_without_breaking_env_resolution`. The explicit skip is a known baseline environment failure, not a clean full-suite claim.
- `cargo fmt --check` and `git diff --check` passed. Live vendor and Windows-only runtime tests were not run.

### Final local outcome

Merge commit: `0a03355` (`05302da` + upstream `fabb309`). The final scoped test run exited 0: **2,458 passed, 16 ignored, one explicitly filtered baseline failure**. Upstream ancestry is integrated; no push was performed. Cleanup Tasks 1–3 remain a proposal, not an applied refactor.

net: approximately -40 to -80 production lines possible.
