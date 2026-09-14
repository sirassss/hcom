# Committed Plugin Skill Copies Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the messaging skill as real committed directories inside each plugin adapter so a vendor installing from a pushed git repository gets the skill, instead of a symlink it may silently drop.

**Architecture:** `skills/hcom-agent-messaging/` stays the only authored copy. A script regenerates byte-identical copies into `plugin/hcom/skills/` and `plugin/hcom-agy/skills/`, both committed. A test fails on any drift or any symlink anywhere under `plugin/`. The Codex-specific staging workaround is then deleted.

**Tech Stack:** Rust (std::fs only), Bash, git.

**Spec:** [2026-09-14-committed-plugin-skill-copies.md](../specs/2026-09-14-committed-plugin-skill-copies.md)

## Global Constraints

- `skills/hcom-agent-messaging/` is the only file anyone edits by hand. Adapter copies are generated output.
- Copies must be **byte-identical** to the source. No generated headers, no "do not edit" banners inside the copied files — a banner would make the drift test impossible.
- No file anywhere under `plugin/` may be a symlink after this plan.
- `materialize_plugin_artifact()` in `src/hooks/plugin_stage.rs` stays — `install_agy_plugin` still uses it. It is **not** a guard against symlinks: `materialize_contents` (`src/hooks/plugin_stage.rs:79-101`) drops the adapter's own `skills` entry and substitutes the repository-root canonical tree, and it dereferences internal links rather than rejecting them. The guard is Task 2's no-symlink test. The same behaviour means a staged AGY artifact looks correct even when the committed adapter copy is stale, so no probe that goes through hcom's AGY install can verify the committed copy.
- The sync script and the acceptance probes are Bash. On Windows use Git Bash; do not write a second PowerShell implementation. Linux CI is the authoritative no-symlink gate: with `core.symlinks=false` a Windows checkout materializes a tracked link as an ordinary text file, so a filesystem check there would miss it.
- The marketplace source is a remote URL with no ref (`marketplace_source()`, `src/hooks/plugin.rs:503-517`), so vendors resolve the remote's default branch. Do not treat the live marketplace route as fixed until the consumed ref carries these commits.
- Do not change the content of the canonical skill, and do not change any hook manifest.
- Run tests with `HCOM_DIR=$(mktemp -d) cargo test`. Running the suite against the real `~/.hcom` deletes live hcom agents from the bus (`mark_dead_instances`, `src/instance_lifecycle.rs:774`).
- The known pre-existing failure `commands::start::tests::test_vanilla_start_never_installs_a_plugin_tool` is not caused by this work. Do not try to fix it; do not count it as a regression.

---

## File map

| File | Responsibility |
|---|---|
| `scripts/sync-plugin-skills.sh` (create) | Regenerate both adapter copies from the canonical tree. The only supported way to update them. |
| `plugin/hcom/skills/` (replace symlink with directory) | Claude / Cursor / Codex skill payload |
| `plugin/hcom-agy/skills/` (create) | Antigravity skill payload |
| `tests/plugin_payload.rs` | Drift test and no-symlink test over the committed tree |
| `src/hooks/plugin.rs` | Delete the Codex staging workaround; fix the Cursor path test's stale comment |
| `.github/workflows/payload.yml` (create) | Reusable workflow running the payload test; registered as a dist local-artifacts job |
| `dist-workspace.toml` | Registers that job so `dist generate` keeps it |
| `.github/workflows/release.yml` (regenerate, never hand-edit) | Generated output that gains the job |
| `docs/superpowers/plans/2026-09-14-plugin-skill-migration-acceptance.md` | Record the outcome |

Task order matters: Task 1 creates the copies, Task 2 pins them, Task 3 removes the workaround they replace, Task 4 proves the released route works.

---

### Task 1: Generate and commit the adapter skill copies

**Files:**
- Create: `scripts/sync-plugin-skills.sh`
- Replace: `plugin/hcom/skills` (symlink → directory)
- Create: `plugin/hcom-agy/skills/`

**Interfaces:**
- Produces: `scripts/sync-plugin-skills.sh`, run with no arguments from the repository root. Exits 0 on success. Task 2's test failure message tells the reader to run it.

- [x] **Step 1: Write the sync script**

```bash
cat > scripts/sync-plugin-skills.sh <<'SH'
#!/usr/bin/env bash
# Regenerate each plugin adapter's copy of the canonical messaging skill.
#
# Vendors copy a plugin package into their own cache at install time and do not
# agree on what to do with a symlink: Claude dereferences it, Codex skips it
# (measured 0.154.0). So every adapter carries real files. This script is the
# only supported way to update them — edit skills/hcom-agent-messaging/ and rerun.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_tree="$root/skills/hcom-agent-messaging"
[ -d "$source_tree" ] || { echo "missing $source_tree" >&2; exit 1; }

for adapter in hcom hcom-agy; do
    dest="$root/plugin/$adapter/skills"
    rm -rf "$dest"
    mkdir -p "$dest"
    # -RL dereferences any link in the source so the output is always real files.
    cp -RL "$source_tree" "$dest/hcom-agent-messaging"
    echo "synced plugin/$adapter/skills"
done
SH
chmod +x scripts/sync-plugin-skills.sh
```

- [x] **Step 2: Remove the symlink and run the script**

```bash
git rm --cached plugin/hcom/skills
rm -f plugin/hcom/skills
bash scripts/sync-plugin-skills.sh
```

- [x] **Step 3: Verify the output is real files, not links**

```bash
find plugin -type l
```

Expected: no output. Then:

```bash
diff -r skills/hcom-agent-messaging plugin/hcom/skills/hcom-agent-messaging
diff -r skills/hcom-agent-messaging plugin/hcom-agy/skills/hcom-agent-messaging
```

Expected: no output from either.

- [x] **Step 4: Commit**

```bash
git add scripts/sync-plugin-skills.sh plugin/hcom/skills plugin/hcom-agy/skills
git commit -m "feat(plugin): ship each adapter a real copy of the messaging skill"
```

---

### Task 2: Fail the build when a copy drifts or a link returns

**Files:**
- Modify: `tests/plugin_payload.rs`
- Modify: `src/hooks/plugin.rs:1590-1606` (the `cursor_declared_skills_path_resolves` doc comment now describes a symlink that no longer exists)

**Interfaces:**
- Consumes: the committed trees from Task 1.
- Produces: nothing other tasks call.

- [x] **Step 1: Write the failing tests**

Append to `tests/plugin_payload.rs`. `files_below` and `copy_tree` already exist in that file — reuse them, do not redefine.

```rust
/// Each adapter carries its own real copy because vendors disagree about
/// symlinks: Claude dereferences `plugin/hcom/skills`, Codex skips it
/// (measured 0.154.0), which shipped Codex hooks with no skill. The copies are
/// generated, so drift means someone hand-edited one or forgot to regenerate.
#[test]
fn every_adapter_carries_a_byte_identical_copy_of_the_skill() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let canonical = root.join("skills/hcom-agent-messaging");
    let expected = files_below(&canonical);
    assert!(
        expected.contains(&PathBuf::from("references/scripts/basic-messaging.sh")),
        "canonical tree walk missed references/scripts"
    );

    for adapter in ["hcom", "hcom-agy"] {
        let copy = root
            .join("plugin")
            .join(adapter)
            .join("skills/hcom-agent-messaging");
        assert_eq!(
            files_below(&copy),
            expected,
            "plugin/{adapter}/skills is out of sync — run scripts/sync-plugin-skills.sh"
        );
        for relative in &expected {
            assert_eq!(
                std::fs::read(canonical.join(relative)).unwrap(),
                std::fs::read(copy.join(relative)).unwrap(),
                "plugin/{adapter}/skills/hcom-agent-messaging/{} differs — run scripts/sync-plugin-skills.sh",
                relative.display()
            );
        }
    }
}

/// A link anywhere under `plugin/` is the bug this plan removed: the vendor that
/// skips it ships a package missing whatever it pointed at.
#[test]
fn no_plugin_file_is_a_symlink() {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
            {
                found.push(path);
            } else if path.is_dir() {
                walk(&path, found);
            }
        }
    }

    let mut found = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"), &mut found);
    assert!(found.is_empty(), "symlinks under plugin/: {found:?}");
}
```

- [x] **Step 2: Run them and watch the drift test catch a real edit**

```bash
printf '\n' >> plugin/hcom/skills/hcom-agent-messaging/SKILL.md
HCOM_DIR=$(mktemp -d) cargo test --test plugin_payload every_adapter_carries
```

Expected: FAIL naming `SKILL.md differs — run scripts/sync-plugin-skills.sh`. Then undo the edit and rerun:

```bash
bash scripts/sync-plugin-skills.sh
HCOM_DIR=$(mktemp -d) cargo test --test plugin_payload
```

Expected: all tests PASS.

- [x] **Step 3: Fix the stale comment on the Cursor path test**

In `src/hooks/plugin.rs`, replace the doc comment above `cursor_declared_skills_path_resolves`:

```rust
    /// `plugin/hcom/skills` is a committed directory, so the declared path
    /// resolves. It used to be a symlink to the repo-root `skills/`; Codex's
    /// installer skipped that link and shipped a package with hooks and no
    /// skill, so every adapter now carries real files (see
    /// tests/plugin_payload.rs).
```

- [x] **Step 4: Run the full suite**

```bash
HCOM_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Expected: only `commands::start::tests::test_vanilla_start_never_installs_a_plugin_tool` fails. Clippy and fmt clean.

- [x] **Step 5: Commit**

```bash
git add tests/plugin_payload.rs src/hooks/plugin.rs
git commit -m "test(plugin): fail the build on adapter skill drift or a symlink"
```

---

### Task 3: Delete the Codex staging workaround

**Files:**
- Modify: `src/hooks/plugin.rs` — delete `codex_marketplace_source()` and `stage_marketplace()`, restore `install_codex_plugin()` to the shared source policy
- Modify: `src/hooks/plugin.rs` tests — delete `the_staged_marketplace_carries_a_real_skills_directory`

**Interfaces:**
- Consumes: Task 1's committed copies, which make the staging unnecessary.
- Produces: `install_codex_plugin() -> Result<(), String>`, unchanged signature.

- [x] **Step 1: Restore the plain install**

Replace the body of `install_codex_plugin` and delete both helper functions:

```rust
/// Codex: add the marketplace, then install the plugin from it.
///
/// `codex plugin marketplace add` takes "a local path, owner/repo[@ref], HTTPS
/// Git URL, or SSH Git URL" (0.154.0 `--help`), so it uses the same source
/// policy Claude does — a developer's Codex indexes the fork their checkout
/// tracks rather than upstream.
///
/// Unlike Claude's install, **this never strips the legacy hook entries.**
/// Codex's hooks require an explicit trust step hcom cannot perform, so the
/// native entries are the only thing firing until the user reviews the plugin.
/// They go on an explicit `hcom hooks remove codex --legacy-only`.
pub(crate) fn install_codex_plugin() -> Result<(), String> {
    let source = marketplace_source();
    run_tool_cli("codex", &["plugin", "marketplace", "add", &source])?;
    run_tool_cli("codex", &["plugin", "add", CLAUDE_PLUGIN_ID])
}
```

- [x] **Step 2: Delete the staging test**

Remove `the_staged_marketplace_carries_a_real_skills_directory` from the tests module in `src/hooks/plugin.rs`. Its subject no longer exists; Task 2's tests cover the property it was checking.

- [x] **Step 3: Verify nothing else referenced the deleted code**

```bash
grep -rn "stage_marketplace\|codex_marketplace_source" src/ tests/
```

Expected: no output.

- [x] **Step 4: Run the suite**

```bash
HCOM_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Expected: only the known pre-existing failure. No dead-code warnings.

- [x] **Step 5: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "refactor(plugin): drop the Codex staging workaround the committed skill replaces"
```

---

### Task 4: Prove the released route works from a clone

**Files:**
- Modify: `docs/superpowers/plans/2026-09-14-plugin-skill-migration-acceptance.md`

**Interfaces:** none.

This task changes the host. Do not run it without the owner's go-ahead; ask, then proceed.

- [x] **Step 1: Clone the repository and record what you cloned**

A clone reproduces the *tree contents* a vendor's git marketplace would check out. It does not reproduce the vendor's git acquisition or its ref selection — record that limitation with the result, and record the revision so the measurement is attributable:

```bash
tmp=$(mktemp -d)
git clone --depth 1 "file://$(pwd)" "$tmp/hcom"
git -C "$tmp/hcom" rev-parse HEAD
find "$tmp/hcom/plugin" -type l
diff -r skills/hcom-agent-messaging "$tmp/hcom/plugin/hcom/skills/hcom-agent-messaging"
diff -r skills/hcom-agent-messaging "$tmp/hcom/plugin/hcom-agy/skills/hcom-agent-messaging"
```

Expected: `find` prints nothing, both `diff -r` print nothing. Quote the URL: an unquoted `file://$(pwd)` breaks on a path containing spaces.

- [x] **Step 2: Install into Codex from that clone and measure**

```bash
codex plugin remove hcom@hcom || true
codex plugin marketplace remove hcom || true
codex plugin marketplace add "$tmp/hcom"
codex plugin add hcom@hcom
ls -a ~/.codex/plugins/cache/hcom/hcom/1.0.0/
ls ~/.codex/plugins/cache/hcom/hcom/1.0.0/skills/hcom-agent-messaging/references/
hcom hooks status
```

Expected: the cache contains `skills/`; the references listing shows `patterns.md`, `gotchas.md`, `cross-tool.md`, `script-template.md`, `scripts`; `hooks status` reports `codex: installed; hook review required` (or `installed (plugin hooks active)` once the hooks are trusted in Codex), never `incompatible Claude handlers`.

- [x] **Step 3: Install into Antigravity directly from the cloned adapter**

hcom's own AGY route stages the artifact from the repository-root canonical tree and ignores `plugin/hcom-agy/skills` entirely, so it would pass with the committed copy absent or stale. Install the cloned adapter directory directly, which is what the committed copy has to stand on:

```bash
agy plugin install "$tmp/hcom/plugin/hcom-agy"
installed=~/.gemini/config/plugins/hcom/skills/hcom-agent-messaging
diff -r skills/hcom-agent-messaging "$installed"
```

Expected: `diff -r` prints nothing — same file set, same bytes. A bare `ls` is not enough; it cannot see a stale or truncated copy.

- [x] **Step 3a: Re-run hcom's own AGY route as a separate regression**

```bash
HCOM_DEV_ROOT="$tmp/hcom" hcom hooks add antigravity
hcom hooks status
```

Expected: Antigravity reports with no payload warning. Record this as a check of hcom's staging path, explicitly not as evidence about the committed adapter copy.

- [x] **Step 4: Record the measurement**

Append to the acceptance notes, under a new `## Released route` heading: the cloned revision from Step 1, the clone's symlink count, the `diff -r` results, what each vendor's installed cache contained, and the `hooks status` line for each. State plainly that a local-path marketplace proves tree contents only — vendor git acquisition and ref selection remain unmeasured until Step 6. Record anything that did not match expectations as unverified rather than assumed.

- [x] **Step 5: Commit**

```bash
git add docs/superpowers/plans/2026-09-14-plugin-skill-migration-acceptance.md
git commit -m "docs: record the released-route install measurement"
```

- [ ] **Step 6: After publication, verify a real git-URL marketplace** (BLOCKED: nothing is pushed yet)

Blocked until the owner pushes these commits to the ref vendors consume — the remote's default branch, since `marketplace_source()` carries no ref. Only then does the production route exist to measure. Ask the owner whether that has happened; do not assume it from a local commit or a feature-branch push.

```bash
codex plugin remove hcom@hcom || true
codex plugin marketplace remove hcom || true
codex plugin marketplace add <the published git URL>
codex plugin add hcom@hcom
ls ~/.codex/plugins/cache/hcom/hcom/1.0.0/
diff -r skills/hcom-agent-messaging ~/.codex/plugins/cache/hcom/hcom/1.0.0/skills/hcom-agent-messaging
hcom hooks status
```

Expected: the cache carries `skills/`, `diff -r` prints nothing, and `hooks status` never says `incompatible Claude handlers`. Append the result and the resolved revision to the acceptance notes, then commit.

---

### Task 5: Gate the release on the payload test

**Files:**
- Create: `.github/workflows/payload.yml`
- Modify: `dist-workspace.toml`
- Regenerate: `.github/workflows/release.yml`

**Interfaces:** none.

`release.yml` triggers on a tag push, runs no `cargo test`, and does not depend on `ci.yml`, so a revision whose payload test never ran can be tagged and published — shipping exactly the stale skill this plan exists to prevent.

**Do not hand-edit `release.yml`.** Its first line reads "This file was autogenerated by dist"; it is produced by cargo-dist 0.32.0 from `dist-workspace.toml`, `ci.yml:86-96` fails the build when the committed file drifts from the generator, and the next `dist generate` would drop a hand-written job. Custom jobs are registered in `dist-workspace.toml` as reusable workflows — the repository already does this three times.

**Register it under `local-artifacts-jobs`, not `plan-jobs`.** The phase matters for whether a failure actually blocks publication, and the answer is visible in the generated file rather than inferred:

- `custom-build-wheels` is a local-artifacts job. `release.yml:189-197` calls it with `with: plan: ...` and `secrets: inherit`; `release.yml:199-203` makes `build-global-artifacts` depend on it; `release.yml:246-253` lists it in `host`'s `needs` and in `host`'s condition, which permits only `skipped` or `success`. A failed job is neither, so `host` does not run and nothing is published. That is the gate this task needs.
- Plan jobs do not get that treatment: they run beside `plan` rather than under it, and `host`'s `always()` condition does not mention them, so a failed plan-job can leave publication to proceed. They also receive no `plan` input, so a reusable workflow declaring one would make the generated call invalid.

- [x] **Step 1: Confirm the gap, the generator, and the gate before changing anything**

```bash
head -2 .github/workflows/release.yml
grep -n "cargo test" .github/workflows/release.yml
grep -n "jobs\|dist-version" dist-workspace.toml
sed -n '189,197p;246,253p' .github/workflows/release.yml
```

Expected: the autogenerated banner; no `cargo test` anywhere in `release.yml`; job lists in `dist-workspace.toml` naming `./`-prefixed workflows; and `host`'s condition permitting `custom-build-wheels` only as `skipped` or `success`. That last line is the mechanism this task attaches to — read it before writing anything.

- [x] **Step 2: Write the reusable workflow**

Match `.github/workflows/build-wheels.yml`, the local-artifacts job already proven in this repository: `workflow_call` with a required `plan` input, because `release.yml:193-196` passes `with: plan:` to jobs in this phase. The job ignores the value; it is declared so the generated call is valid. Pin `actions/checkout` to the SHA in `dist-workspace.toml` under `[dist.github-action-commits]`.

```yaml
name: "Plugin payload gate"

on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string

jobs:
  payload:
    runs-on: "ubuntu-22.04"
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd
        with:
          persist-credentials: false
      # Pinned like ci.yml rather than left to the runner image: Cargo.toml
      # sets rust-version = "1.88", and a gate whose toolchain drifts with the
      # image is not a gate. Copy whatever ci.yml pins at implementation time.
      - uses: dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c # 1.97.1
        with:
          toolchain: "1.97.1"
      # Each adapter's skills/ is generated from skills/hcom-agent-messaging.
      # A tag whose copies drifted would publish a plugin carrying the wrong
      # skill, and this workflow otherwise runs no tests at all. Linux is the
      # authoritative no-symlink gate: core.symlinks=false on Windows
      # materializes a tracked link as ordinary text.
      - run: cargo test --locked --test plugin_payload
```

- [x] **Step 3: Register it as a local-artifacts job and regenerate**

Add to the `[dist]` table in `dist-workspace.toml`, beside the existing job lists:

```toml
local-artifacts-jobs = ["./build-wheels", "./payload"]
```

replacing the existing `local-artifacts-jobs` line. Order does not matter; both run after `plan`.

Then regenerate with the pinned version — `ci.yml:86-96` runs `dist generate --check` and fails the build if the committed `release.yml` differs from what cargo-dist 0.32.0 would produce, so the version must match `cargo-dist-version` in `dist-workspace.toml`:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-installer.sh | sh
dist generate
git diff --stat .github/workflows/release.yml
dist generate --check
```

Expected: `release.yml` changes to include the new job, and `dist generate --check` then exits 0. If `dist` cannot be installed, stop and say so rather than editing `release.yml` by hand — a hand-edit both fails that CI check and disappears on the next regeneration.

- [x] **Step 4: Validate the workflows, not just their syntax**

YAML parsing says nothing about job dependencies or action inputs. Use the repository's own two mechanisms:

```bash
dist generate --check
```

Expected: exit 0 — the committed `release.yml` matches the generator.

Then run the workflow linter the way `.github/workflows/lint-workflows.yml` does (actionlint over `.github/workflows/`), and confirm the generated `release.yml` actually contains the job and the dependency:

```bash
grep -n "custom-payload" .github/workflows/release.yml
```

Expected: `custom-payload` appears as a job, in `build-global-artifacts`'s `needs`, and in both `host`'s `needs` and `host`'s `if` condition — the last one is what makes a payload failure stop publication. If it is absent from `host`'s condition, the gate does not exist: stop and report rather than proceeding.

- [x] **Step 4a: Reason through the failure case explicitly**

Write the expected outcome into the commit message or the acceptance notes: if `custom-payload` fails, its result is `failure`, so `host`'s condition — which permits only `skipped` or `success` for each custom job — is false, `host` is skipped, and no release is created or published. Do not claim this is verified by a real failing run unless one has been observed.

- [x] **Step 5: Commit**

```bash
git add .github/workflows/payload.yml .github/workflows/release.yml dist-workspace.toml
git commit -m "ci: gate the release on the plugin payload test"
```

---

## Self-review coverage

Spec's "each adapter carries its own real copy" → Task 1. "A test fails on drift" and "no symlink under plugin/" → Task 2. "Remove `codex_marketplace_source`/`stage_marketplace`, keep `materialize_plugin_artifact`" → Task 3. "Marketplace installs work from a plain git clone" → Task 4 Steps 1-5, with the AGY copy verified directly rather than through hcom's staging. "Publication order" → Task 4 Step 6, owner-gated. "Release discipline" → Task 5. The spec's non-goals (hook declarations, skill content, external registries) are touched by no task.
