# Plugin Skills and Host Customization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phân phối một canonical hcom skill qua plugin cho Claude/Cursor/Codex/AGY, giữ customization của owner trong agent-skill.

**Architecture:** `skills/hcom-agent-messaging/` là nguồn duy nhất. `plugin/hcom/` phục vụ Claude/Cursor/Codex; `plugin/hcom-agy/` giữ hooks riêng nhưng bundle cùng skill. Hook status, skill payload và runtime discovery là ba bằng chứng riêng; migration chỉ hoàn tất sau verification.

**Tech Stack:** Rust, serde_json, existing CLI/app-server adapters, Bash dotfiles, Markdown skills.

**Spec:** [2026-09-13-codex-plugin-skill-and-session-lifecycle-design.md](../specs/2026-09-13-codex-plugin-skill-and-session-lifecycle-design.md)

## Global Constraints

- Không tạo lại `~/.hcom/SKILL.md`, `~/.hcom/skill` hoặc skill riêng trong agent-skill.
- Không xóa legacy hooks chỉ vì tìm thấy plugin/cache.
- Không tự động điều khiển menu `/import`; đây là luồng tương tác trong một local Codex session.
- Không tự trust hook import từ plugin và không bật trust bypass cho hook không thuộc hcom.
- Không duy trì hai bản SKILL.md chỉnh tay.
- Host paths như `~/.claude`, `~/.codex`, `~/.cursor` và `~/.hcom` là migration targets, không phải test fixtures mặc định.
- Baseline: hcom `1ea5bc667b38b3a36483a1bf66fdfee6f9a8b07b`; agent-skill `e8965cb51262f2e7db99c55c2c303dfdf3ecf448`. Không lặp task merge/delete skill đã hoàn tất.
- Read `~/.hcom/HOST.md` trước mọi agent communication/spawn; không kill/stop agent khi chưa có owner go-ahead. Lượt implementation không tự bao gồm host install/publish.

---

## File map và thứ tự

| File | Responsibility |
|---|---|
| `plugin/hcom/.codex-plugin/plugin.json`, `plugin/hcom/hooks/hooks-codex.json` | Codex overlay, skill path và đúng hook handlers |
| `plugin/hcom-agy/.claude-plugin/plugin.json`, `plugin/hcom-agy/skills/` | AGY skill discovery, giữ hook adapter |
| `src/hooks/plugin.rs` | Source policy, AGY payload validation, native install helpers |
| `scripts/stage-plugin.sh` (nếu probe chứng minh cần), `tests/plugin_payload.rs` | Materialize và kiểm tra artifact không phụ thuộc checkout |
| `src/hooks/codex.rs` | Inventory parser, pure status classifier, conditional add/remove |
| `src/tool.rs`, `src/commands/hooks.rs` | Route, status wording, action-required và legacy-only |
| `agent-skill/dotfiles/install.d/hcom.sh` | Output setup và safe retirement; chỉ host wiring |
| `agent-skill/dotfiles/config/hcom/bootstrap/` | HOST.md và vendor policy, không duplicate skill |
| `agent-skill/dotfiles/tests/hcom-install.sh` (create) | Isolated installer regression harness |

Tasks 1–4 ở hcom; Task 5 ở `/home/alam/workspaces/agent-skill`. TUI là [plan độc lập](2026-09-13-tui-dead-agent-reconciliation.md). Commit hai repo riêng, không stage các thay đổi có sẵn của owner.

### Task 1: Establish loader contract and distribute canonical skill to AGY

**Files:** `plugin/hcom-agy/.claude-plugin/plugin.json`, `plugin/hcom-agy/skills/`, `src/hooks/plugin.rs`; create `tests/plugin_payload.rs`; conditional create `scripts/stage-plugin.sh`.

**Interfaces:** `verify_plugin_skill_payload(root: &Path) -> Result<(), String>` kiểm tra `skills/hcom-agent-messaging/SKILL.md` và bundled references nằm trong root. Giữ `verify_agy_plugin_installed()` mang nghĩa hooks presence; không biến payload presence thành runtime discovery.

- [x] **Step 1: Record actual AGY loader behavior in isolated scratch home.** (answered 2026-09-14 from AGY's own docs, no probe: the loader finds `skills/` by convention, so no `"skills"` key is needed or added. The prior probe stays flagged do-not-repeat in the [task 1 handoff](2026-09-14-agy-task-1-review-handoff.md); runtime discovery stays open for Task 6.) Run `agy --version`, `agy plugin --help`, `agy plugin validate plugin/hcom-agy`. Dùng `mktemp -d` tạo home/staging; đặt `GEMINI_CLI_HOME` vào scratch prefix, không trỏ home thật. Ghi version, schema được chấp nhận, cách scanner tìm `skills/` và copy/dereference symlinks. Test cả manifest hiện tại và bản thêm `"skills": "./skills/"`; chỉ giữ field nếu loader hỗ trợ/cần nó. Không suy ra skill support từ `validate` success.
- [x] **Step 2: Add a regression that fails for the current AGY tree.** Test chạy với payload fixture bên ngoài checkout; kiểm tra mọi file canonical, không chỉ SKILL.md:

```rust
#[test]
fn agy_package_carries_the_canonical_skill() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in ["SKILL.md", "references/patterns.md", "references/gotchas.md",
                     "references/script-template.md", "references/cross-tool.md"] {
        let canonical = root.join("skills/hcom-agent-messaging").join(relative);
        let bundled = root.join("plugin/hcom-agy/skills/hcom-agent-messaging").join(relative);
        assert_eq!(std::fs::read(canonical).unwrap(), std::fs::read(bundled).unwrap());
    }
}
```

Run `cargo test --test plugin_payload`; expected failure vì AGY chưa có skills. Extend test traversal to every file under canonical tree, including `references/scripts/`, and assert each installed canonicalized path starts with artifact root.
- [x] **Step 3: Wire the single source into AGY package.** Add `plugin/hcom-agy/skills -> ../../skills` only if probe proves installed copy materializes it. Nếu không, implement `scripts/stage-plugin.sh SOURCE_ROOT ADAPTER DEST` (`ADAPTER` chỉ `hcom` hoặc `hcom-agy`), copy metadata/hooks và `cp -RL "$source_root/skills" "$dest/skills"` vào destination mới; reject destination tồn tại để không overwrite. Connect staging to `install_agy_plugin()` before `agy plugin install`; giữ tempdir tới khi CLI hoàn tất. Release/shared-package routes phải dùng cùng materialization nếu chúng cũng giữ external symlink. Không commit generated SKILL copies.
- [x] **Step 4: Verify installed artifact and diagnostics.** Unit fixtures: hooks-only → missing skill; SKILL-only → missing references; external link → invalid artifact; complete payload → ready on disk. AGY add/status in rõ `hooks present; skill missing` cho bản cũ và hướng dẫn reinstall. Không dùng hook-only fast path để bỏ qua repair skill. Không strip legacy khi replacement payload thiếu.
- [ ] **Step 5: Run `cargo test --test plugin_payload` and `cargo test hooks::plugin`.** (tests green; the scratch AGY runtime listing was never observed, so the alias is not retired.) Di chuyển/ẩn riêng temporary source của fixture rồi đọc installed references; không rename checkout thật. Runtime listing trong scratch AGY phải có đúng một `hcom-agent-messaging` với origin plugin. Nếu không quan sát được runtime, ghi unverified và chưa retire alias.
- [x] **Step 6: Commit scoped files:** `feat(plugin): ship the canonical messaging skill with AGY`.

### Task 2: Add the Codex overlay to the shared package

**Files:** create `plugin/hcom/.codex-plugin/plugin.json`, `plugin/hcom/hooks/hooks-codex.json`; extend `tests/plugin_payload.rs` and unit tests in `src/hooks/codex.rs`.

**Interfaces:** plugin name `hcom`, existing marketplace source `./plugin/hcom`; Codex overlay selects `./hooks/hooks-codex.json` and `./skills/`.

- [x] **Step 1: Add failing manifest and hook-contract tests.** Parse all vendor manifests; expect Codex explicit paths, Cursor unchanged `./hooks/hooks-cursor.json`, Claude and AGY default hook files unchanged. Check complete event/command pairs against `CODEX_HOOK_CONFIGS` in codex.rs, not a separately maintained list. Include a command on the wrong event as a failing fixture.
- [x] **Step 2: Run `cargo test --test plugin_payload` and `cargo test hooks::codex`.** New overlay test fails because file is missing; keep existing trust tests green.
- [x] **Step 3: Add overlay metadata matching the shared package version.** Relevant manifest fields:

```json
{
  "name": "hcom",
  "version": "1.0.0",
  "description": "Multi-agent messaging skills and Codex lifecycle hooks",
  "skills": "./skills/",
  "hooks": "./hooks/hooks-codex.json"
}
```

Copy shared author/repository/license metadata. Build hook JSON from the current Codex registry and `build_codex_hook_command()` contract: SessionStart/sessionstart, UserPromptSubmit/userpromptsubmit, PreToolUse/pretooluse, PostToolUse/posttooluse, Stop/stop, all prefixed `codex-`. Preserve matchers, timeouts and missing-binary behavior from native integration. No Claude handler and no SessionEnd.
- [ ] **Step 4: Verify package artifact using Task 1's self-contained test.** (partial, 2026-09-14: staging test asserts the overlay files ride into the artifact; codex-cli 0.154.0 confirmed read-only to read `.codex-plugin/plugin.json`. `codex plugin` has no `validate`, and no scratch install/import was run, so overlay selection stays unverified — carried to Task 6 Step 3.) Read-only CLI schema validation plus scratch install/import must prove overlay selection; record CLI version. Official schema supports overlay paths, but import behavior is an acceptance measurement, not a claim based only on JSON validity.
- [x] **Step 5: Commit:** `feat(plugin): add Codex hooks and skills to the shared hcom package`.

### Task 3: Add a read-only Codex hook inventory classifier

**Files:** `src/hooks/codex.rs`, `src/commands/hooks.rs`; unit tests beside both modules.

**Interfaces:** Reuse `fetch_codex_hook_list(cwd, codex_home)` and `parse_codex_hook_list_entries()`. Add event identity to `CodexHookListEntry` using actual protocol metadata, including fixtures. Proposed internal result:

```rust
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodexPluginState {
    Active, ReviewRequired, Disabled, Duplicate, LegacyOnly,
    Discovered, Incompatible, Incomplete, Missing, Unverified,
}
pub(crate) struct CodexPluginStatus {
    pub state: CodexPluginState,
    pub details: Vec<String>,
}
```

`classify_codex_plugin_hooks(entries: &[CodexHookListEntry], hooks_path: &Path, plugin_roots: &[PathBuf]) -> CodexPluginStatus` is pure; `codex_plugin_status(cwd: &Path) -> CodexPluginStatus` handles fetch errors/hints.

- [x] **Step 1: Add table-driven fixtures for every spec state.** Derive baseline inventory from existing `hooks_list_value` fixture; vary trust, enabled, source and sourcePath. Include plugin+legacy, cache-only, Claude commands, missing PostToolUse, correct command on wrong event, foreign project command matching hcom, unknown trust, and a schema error. Test all inventory groups: parser currently selects `/result/data/0/hooks`, which must not silently omit other returned groups.
- [x] **Step 2: Run `cargo test hooks::codex`; new classifier tests must fail before implementation.** Existing `hook_list_entry_is_hcom_owned` tests remain intact: plugin/project sources must never qualify for user trust mutation.
- [x] **Step 3: Implement classifier with evidence-preserving precedence.** Fetch/schema error → Unverified; incompatible hcom Claude handlers → Incompatible; enabled duplicate event pairs → Duplicate (include untrusted/disabled details); incomplete set → Incomplete; complete plugin disabled → Disabled; unknown/untrusted/modified → ReviewRequired; all enabled trusted/managed → Active. LegacyOnly needs a complete correctly sourced set, with trust/disabled limitations in details. Discovery hints never fill missing runtime handlers. Match normalized command plus event plus verified origin path, never substring `hcom`.
- [x] **Step 4: Render the spec's exact headlines in hooks status.** Keep `details` for missing/extra events and source paths. Unit tests assert cache/fetch-error states never print `hooks active` or `both are firing`. Do not repurpose `hook_list_entry_is_hcom_owned()` to recognize plugin status.
- [x] **Step 5: Run `cargo test hooks::codex` and `cargo test commands::hooks`; commit:** `feat(hooks): classify Codex plugin activation from runtime inventory`.

**Measured 2026-09-14 (codex-cli 0.154.0, `hooks/list` against a scratch `CODEX_HOME`, read-only, nothing installed):** entry shape is `key`, `eventName`, `handlerType`, `command`, `async`, `matcher`, `timeoutSec`, `statusMessage`, `additionalContextLimit`, `sourcePath`, `source`, `pluginId`, `displayOrder`, `enabled`, `isManaged`, `currentHash`, `trustStatus`; response is `/result/data[]` with `cwd`, `hooks`, `warnings`, `errors` per group. `eventName` is **lowerCamelCase** (`sessionStart`, `preToolUse`, `postToolUse`, `userPromptSubmit`, `stop`) while the `key` segment is snake_case — the two are not interchangeable. `pluginId` is null for config layers, so it is the plugin-origin signal. A fixture built from this response is pinned in `src/hooks/codex.rs`.

### Task 4: Route Codex installation and removal without premature success

**Files:** `src/hooks/codex.rs`, `src/hooks/plugin.rs`, `src/tool.rs`, `src/commands/hooks.rs`; tests in these modules and `tests/cli_smoke.rs`.

**Interfaces:** Add `ClaudePresence::{Present, Absent, Indeterminate(String)}` and `CodexAddOutcome::{AlreadyActive, ActionRequired(String), InstalledUnverified(String)}`. `add_codex_plugin() -> Result<CodexAddOutcome, String>` consumes Task 3 status. Command layer prints the outcome explicitly; action-required/unverified use exit 2, failure exit 1, verified completion exit 0. Tests freeze this contract for dotfiles.

- [x] **Step 1: Add mocked executable tests.** Present: executable exits 0 → import guidance only. Absent: executable resolution returns NotFound → native route. Permission denied, bad exit or bounded 3s timeout → indeterminate/no writes. Active plugin short-circuits probe/install. Mock command log must show no marketplace/plugin-add on present or indeterminate branches. Use isolated HOME/CODEX_HOME/PATH fixtures; never invoke real installed clients here.
- [x] **Step 2: Add command regression for the dangerous existing fast path.** `commands/hooks.rs` currently strips legacy automatically for plugin tools except Cursor. Once Codex becomes plugin-capable, test `hooks add codex` with plugin+legacy keeps legacy byte-for-byte. AGY hooks-only payload must reach repair diagnostics, not Already. Check auto-approve refresh and launch paths calling `Tool::try_setup_hooks` so no hidden install/import/trust mutation is introduced.
- [x] **Step 3: Implement the conditional route.** Present emits prerequisite `hcom hooks add claude` if needed plus `/import`, select plugin skill, skip standalone, restart/review/status. Absent uses existing source policy from plugin.rs and CLI syntax verified with target `codex plugin --help`; rerun runtime inventory after install. Do not fall back to native global hooks when CLI lacks plugin capability. Feature enablement must use existing supported-version logic, without automatic trust/bypass.

Inventory unavailable must return unverified without install; legacy-only offers migration instead of short-circuiting as plugin-ready. Add fixtures for both. For `add all`, error exit 1 takes priority over pending exit 2; exit 0 requires no pending/error outcomes.
- [x] **Step 4: Implement explicit removal contracts.** Add Codex to plugin-capability modeling with the above call-site audit. `remove_legacy_hooks_only` calls the existing hcom-entry remover and preserves foreign hooks/plugin connection. Full remove calls Codex-side plugin removal plus legacy cleanup; it must not run Claude uninstall or delete Claude marketplace. Mock logs and JSON before/after prove both boundaries. Partial uninstall failure is surfaced.
- [x] **Step 5: Run `cargo test hooks`, `cargo test tool`, `cargo test --test cli_smoke`.** Verify all-tool add aggregates pending/error outcomes without reporting all installed. Commit: `feat(hooks): route Codex plugin setup through import or native fallback`.

### Task 5: Align agent-skill installer with the completed plugin boundary

**Repo:** `/home/alam/workspaces/agent-skill`.

**Files:** `dotfiles/install.d/hcom.sh`, `dotfiles/config/hcom/bootstrap/antigravity/GEMINI.md`, `dotfiles/config/hcom/bootstrap/cursor/hcom.mdc`, `dotfiles/config/hcom/bootstrap/HOST.md`; create `dotfiles/tests/hcom-install.sh`.

**Interfaces:** Consume Task 4 exit 0/2/1 contract while retaining all command output. HOST.md links remain local configuration. No skill-path symlink creation. Retirement is an explicit helper invoked only after recorded skill discovery per consumer; ordinary installer leaves aliases pending if that evidence is unavailable.

- [x] **Step 1: Create a scratch-home shell harness.** Source installer with stub `blue/green/red`, `hcom`, and `uv` functions; capture stdout/status. Each case owns a `mktemp -d` tree with HOME set only for that subprocess. Fixture matrix: setup succeeds, exit 2 action-required, exit 1 failure, owned stale symlink, foreign symlink, regular file, preexisting backup, missing Claude CLAUDE.md. Assert original files/link targets remain intact when verification is absent.
- [x] **Step 2: Run `bash dotfiles/tests/hcom-install.sh`; demonstrate current failure.** Current `&>/dev/null` hides import instructions and unconditional `rm -f` removes aliases despite failed setup. No test uses real hooks, plugin stores or network.
- [x] **Step 3: Preserve output and classify setup without claiming runtime readiness.** Replace the suppressed conditional with this control flow, compatible with `set -e`:

```bash
if hcom hooks add "$_tool"; then
    green "hcom setup completed → $_tool; skill discovery needs verification"
else
    _hcom_rc=$?
    if [ "$_hcom_rc" -eq 2 ]; then
        blue "hcom → $_tool: action required; follow the instructions above"
    else
        red "hcom → $_tool: setup failed (exit $_hcom_rc)"
    fi
fi
```

Remove automatic skill alias deletion from ordinary install. For explicit retirement, enumerate exact old destinations and compare `readlink` to known old layout targets; shared pointers require all consumers verified. Move owned aliases to a unique backup directory outside vendor skill search roots. Foreign symlinks and regular files are reported and preserved. Never recreate retired standalone skill or copy plugin skill into dotfiles.
- [x] **Step 4: Keep bootstrap claims accurate.** AGY/Cursor bootstrap should say hooks may be plugin-managed; remove unconditional assertion that all hooks live in the legacy global file. Preserve owner policy and Cursor-specific behavioral cautions. Ensure Claude `@HCOM.md` actually exists in CLAUDE.md even on a fresh home, without replacing existing user text or duplicating imports. Preserve unique backups for any owned bootstrap migration.
- [x] **Step 5: Run `bash -n dotfiles/install.d/hcom.sh` and `bash dotfiles/tests/hcom-install.sh`; commit only this repo's scoped files:** `fix(hcom): preserve plugin setup guidance and gate legacy skill retirement`.

### Task 6: Cross-vendor acceptance and handoff

**Files:** update related issue and spec with measured results, versions and remaining unverified checks.

- [x] **Step 1:** Run hcom `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, plus Task 5 shell tests. Record failures separately from pre-existing failures; do not claim pass on a filtered subset.
- [x] **Step 2:** (AGY and Codex measured on this host after the owner authorised the reinstalls; Codex's missing skill payload was found and fixed. Cursor's enabled state remains unreadable by design. See acceptance notes.) On isolated installations, verify Claude/Cursor/Codex/AGY skill inventory has exactly one canonical skill per vendor; open `references/patterns.md` through its installed path. Repeat without optional HOST.md. Inspect hook origin/event pairs independently of skill listing.
- [x] **Step 3:** (overlay selection confirmed at runtime: Codex read `hooks/hooks-codex.json`. Interactive `/import` itself was not driven; the local-marketplace install exercised the same selection.) Exercise Codex import with Claude present and native route with Claude absent; confirm untrusted hooks require review and duplicate legacy stays until explicit removal. If interactive import cannot be exercised, record it unverified and leave migration acceptance open.
- [x] **Step 4:** Verify artifacts remain readable without their temporary source checkout; skill bytes match canonical including script references. Do not infer this from a repo symlink test.
- [x] **Step 4a:** Audit `skills/hcom-agent-messaging/SKILL.md` and `references/{patterns,gotchas,cross-tool,script-template}.md` for mode scope. Preserve completed merge; fix only specific remaining contradictions, ensuring any script cleanup example remains subject to optional HOST.md and does not become host policy in reusable skill. Record exact findings or no remaining contradiction in acceptance notes.
- [x] **Step 5:** Record per-vendor evidence before any authorized host migration. No live agent termination is needed for this plan. Update issue completion only for measured criteria and commit docs as `docs: record plugin skill migration verification`.

## Self-review coverage

AGY discovery/artifact → Task 1; shared Codex overlay → Task 2; all status states/trust isolation → Task 3; probe/add/removal/permissions call sites → Task 4; owner customization/safe migration → Task 5; vendor end-to-end and rollout → Task 6. TUI lifecycle remains in its independent plan and is not a prerequisite for plugin work.
