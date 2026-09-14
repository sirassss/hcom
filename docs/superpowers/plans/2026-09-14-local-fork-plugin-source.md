# Local Fork Plugin Source Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one owner run hcom from their own fork with no GitHub release and no remote-URL resolution: the plugin marketplace points at the local checkout, so every `hooks add` picks up the newest hooks and skill straight from working tree.

**Architecture:** `marketplace_source()` gains a local-checkout preference. Claude, Codex and Antigravity all accept a directory, so when `dev_root` resolves they get the path. Cursor cannot take a path (measured) and keeps the git URL, which is also the only route that still needs SSH-alias normalization.

**Tech Stack:** Rust (std::process::Command, std::fs), git, ssh.

**Spec:** none — the decisions are recorded inline below; this plan is small enough that a separate spec would only restate it.

## Decisions taken (the "chốt")

1. **No GitHub release is needed and no packaging config changes.** `release.yml` publishes only on `push: tags`; a `pull_request` run sets `publishing=false` and publishes nothing. Never pushing a tag leaves `tap = "aannoo/homebrew-hcom"`, `./publish-pypi`, and the upstream installer URLs completely dormant. The owner installs with `cargo install --path .`.
2. **SUPERSEDED 2026-09-14.** The owner meant the fork on GitHub, not the local
   checkout, so Task 1 was reverted in `2a43d27`. The marketplace source stays the
   git remote the branch tracks (`origin` = the fork). Tasks 2 and 3 stand and are
   what make that route work. The original wording follows for the record.

   ~~**The plugin source becomes the local checkout** for Claude, Codex and Antigravity.~~ Measured support for a local path: Claude yes, Antigravity yes (module doc table, `src/hooks/plugin.rs:11-16`), Codex yes (0.154.0 `marketplace add --help`: "a local path, owner/repo[@ref], HTTPS Git URL, or SSH Git URL", exercised 2026-09-14).
3. **Cursor keeps the git URL.** It coerces a local path into an unresolvable `https://<first segment>.git` (measured, module doc). Cursor is therefore the only consumer of `marketplace_source()`'s URL branch, and the only reason the SSH-alias fix below matters.
4. **`repository` in `Cargo.toml` stays upstream.** It only feeds cargo-dist's release hosting, which never runs without a tag. Changing it is a release-identity decision, not needed here.

## Global Constraints

- Run tests with `HCOM_DIR=$(mktemp -d) cargo test`. The real `~/.hcom` loses live agents to `mark_dead_instances` (`src/instance_lifecycle.rs:774`).
- `commands::start::tests::test_vanilla_start_never_installs_a_plugin_tool` fails already and is unrelated. Do not fix it, do not count it.
- Do not change hook manifests, the canonical skill, or any packaging config.
- Adapter skill copies are generated: any edit under `skills/hcom-agent-messaging/` must be followed by `scripts/sync-plugin-skills.sh`.

---

## File map

| File | Responsibility |
|---|---|
| `src/hooks/plugin.rs` | Local-checkout preference for the marketplace source; SSH-alias normalization for the Cursor URL branch |
| `src/launcher.rs` | Stop reinstalling Codex's native hooks when the plugin is already running them |

---

### Task 1: Prefer the local checkout as the marketplace source

**Files:**
- Modify: `src/hooks/plugin.rs` — `marketplace_source()` and its three callers' doc comments
- Test: `src/hooks/plugin.rs` tests module

**Interfaces:**
- Produces: `fn marketplace_source_for(tool: MarketplaceConsumer) -> String`, where `MarketplaceConsumer` is `{ AcceptsPath, UrlOnly }`. `install_claude_plugin` and `install_codex_plugin` pass `AcceptsPath`; `install_cursor_plugin` passes `UrlOnly`.

- [x] **Step 1: Write the failing test**

```rust
    /// Claude, Codex and Antigravity all install from a directory, so a local
    /// checkout is both the freshest source and the one that needs no URL to
    /// resolve. Cursor cannot: it coerces a path into an unresolvable
    /// `https://<first segment>.git` (module doc), so it keeps the remote URL.
    #[test]
    fn a_local_checkout_is_preferred_by_every_consumer_that_accepts_one() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let path_consumer = super::marketplace_source_from(
            Some(root),
            "https://github.com/someone/hcom",
            super::MarketplaceConsumer::AcceptsPath,
        );
        assert_eq!(path_consumer, root.to_string_lossy());

        let url_consumer = super::marketplace_source_from(
            Some(root),
            "https://github.com/someone/hcom",
            super::MarketplaceConsumer::UrlOnly,
        );
        assert_eq!(url_consumer, "https://github.com/someone/hcom");

        // No checkout: everyone falls back to the URL.
        assert_eq!(
            super::marketplace_source_from(
                None,
                "https://github.com/someone/hcom",
                super::MarketplaceConsumer::AcceptsPath,
            ),
            "https://github.com/someone/hcom"
        );
    }
```

- [x] **Step 2: Run it and watch it fail**

Run: `HCOM_DIR=$(mktemp -d) cargo test hooks::plugin::tests::a_local_checkout_is_preferred`
Expected: compile error — `marketplace_source_from` and `MarketplaceConsumer` do not exist.

- [x] **Step 3: Implement**

Split the decision from the lookups so it is testable without a git repo or a database:

```rust
/// Whether a consumer can install a plugin from a directory.
///
/// Measured (module doc, and `codex plugin marketplace add --help` 0.154.0):
/// Claude, Codex and Antigravity accept a local path; Cursor coerces one into
/// an unresolvable `https://<first path segment>.git` and must get a git URL.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MarketplaceConsumer {
    AcceptsPath,
    UrlOnly,
}

/// Pure half of [`marketplace_source_for`], so every branch is reachable from a
/// fixture.
fn marketplace_source_from(
    dev_root: Option<&Path>,
    remote_url: &str,
    consumer: MarketplaceConsumer,
) -> String {
    match (dev_root, consumer) {
        // The working tree is newer than anything pushed, and it needs no
        // network, no host resolution and no published release.
        (Some(root), MarketplaceConsumer::AcceptsPath) => root.to_string_lossy().into_owned(),
        _ => remote_url.to_string(),
    }
}

/// Marketplace source for one consumer: the local checkout where that is
/// supported, otherwise the git remote the checkout's branch tracks.
fn marketplace_source_for(consumer: MarketplaceConsumer) -> String {
    let db_path = crate::paths::db_path();
    let dev_root = crate::router::resolve_effective_dev_root(&db_path).map(|(root, _)| root);
    let remote = dev_root
        .as_deref()
        .and_then(checkout_remote_url)
        .unwrap_or_else(|| HCOM_REPOSITORY_URL.to_string());
    marketplace_source_from(dev_root.as_deref(), &remote, consumer)
}
```

Then replace the three call sites:

- `install_claude_plugin`: `let source = marketplace_source_for(MarketplaceConsumer::AcceptsPath);`
- `install_codex_plugin`: `let source = marketplace_source_for(MarketplaceConsumer::AcceptsPath);`
- `install_cursor_plugin`: `let source = marketplace_source_for(MarketplaceConsumer::UrlOnly);`

Delete the now-unused `marketplace_source()`, and update `install_claude_plugin`'s doc comment, which currently argues for the URL — the argument it makes ("a developer's work lives on a fork, and that fork is what Claude must index") is exactly the case a local checkout serves better when nothing is pushed.

- [x] **Step 4: Run the tests**

```bash
HCOM_DIR=$(mktemp -d) cargo test hooks::plugin
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Expected: all pass, no dead-code warning for a leftover `marketplace_source`.

- [x] **Step 5: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "feat(plugin): install from the local checkout where the vendor accepts one"
```

---

### Task 2: Resolve SSH host aliases in the Cursor URL

**Files:**
- Modify: `src/hooks/plugin.rs` — `normalize_git_url`
- Test: `src/hooks/plugin.rs` tests module

**Interfaces:**
- Consumes: Task 1's `MarketplaceConsumer::UrlOnly` path, the only remaining caller.
- Produces: `normalize_git_url` unchanged in signature.

`git@sirassss:sirassss/hcom.git` becomes `https://sirassss/sirassss/hcom` today. `sirassss` is an SSH host alias from `~/.ssh/config` whose real `HostName` is `github.com`, so the result resolves nowhere. `ssh -G <host>` prints the effective configuration, including the real hostname.

- [x] **Step 1: Write the failing test**

```rust
    /// An SSH host alias is not a DNS name. `git@myalias:owner/repo.git` must
    /// become the alias's real host, or Cursor gets a marketplace URL that
    /// resolves nowhere — the same class of failure as handing it a path.
    #[test]
    fn an_ssh_host_alias_resolves_to_its_real_hostname() {
        assert_eq!(
            super::normalize_git_url_with("git@myalias:owner/repo.git", |host| {
                assert_eq!(host, "myalias");
                Some("github.com".to_string())
            }),
            "https://github.com/owner/repo"
        );

        // Unresolvable alias: keep what git gave us rather than inventing one.
        assert_eq!(
            super::normalize_git_url_with("git@myalias:owner/repo.git", |_| None),
            "https://myalias/owner/repo"
        );

        // A real hostname that resolves to itself is unchanged.
        assert_eq!(
            super::normalize_git_url_with("git@github.com:owner/repo.git", |_| Some(
                "github.com".to_string()
            )),
            "https://github.com/owner/repo"
        );

        // Already an HTTPS URL: untouched, and no ssh call.
        assert_eq!(
            super::normalize_git_url_with("https://github.com/owner/repo.git", |_| panic!(
                "must not resolve a host for an https remote"
            )),
            "https://github.com/owner/repo"
        );
    }
```

- [x] **Step 2: Run it and watch it fail**

Run: `HCOM_DIR=$(mktemp -d) cargo test hooks::plugin::tests::an_ssh_host_alias`
Expected: compile error — `normalize_git_url_with` does not exist.

- [x] **Step 3: Implement**

```rust
/// `git@host:owner/repo.git` → `https://host/owner/repo`, resolving SSH host
/// aliases. Neither Claude nor Cursor accepts an SSH remote as a marketplace
/// source, and a `~/.ssh/config` alias is not a DNS name — a fork cloned through
/// one would otherwise produce a URL that resolves nowhere.
fn normalize_git_url(url: &str) -> String {
    normalize_git_url_with(url, resolve_ssh_hostname)
}

/// Testable half: `resolve` maps an SSH host alias to its real hostname.
fn normalize_git_url_with(
    url: &str,
    resolve: impl Fn(&str) -> Option<String>,
) -> String {
    let url = url.trim().trim_end_matches(".git");
    if let Some(rest) = url.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        let host = resolve(host).unwrap_or_else(|| host.to_string());
        return format!("https://{host}/{path}");
    }
    url.to_string()
}

/// Ask ssh for the effective `HostName` of a host pattern. `ssh -G` prints the
/// resolved configuration without connecting to anything.
fn resolve_ssh_hostname(host: &str) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .args(["-G", host])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}
```

Keep the rest of the existing `normalize_git_url` body (the non-`git@` branch) intact.

- [x] **Step 4: Run the tests and check the real remote**

```bash
HCOM_DIR=$(mktemp -d) cargo test hooks::plugin
ssh -G sirassss | grep '^hostname '
```

Expected: tests pass; `ssh -G` prints `hostname github.com`, which is what the alias branch now produces.

- [x] **Step 5: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "fix(plugin): resolve SSH host aliases in the marketplace URL"
```

---

### Task 3: Stop reinstalling Codex's native hooks at launch

**Files:**
- Modify: `src/launcher.rs` — the `LaunchTool::Codex` arm of `ensure_hooks_installed`
- Test: `src/launcher.rs` tests module

**Interfaces:**
- Consumes: `crate::hooks::codex::codex_plugin_status`, `CodexPluginState`.
- Produces: nothing other tasks call.

`ensure_hooks_installed` checks only the native hooks file, so every `hcom codex` launch rewrites `~/.codex/hooks.json` — which is how the legacy set returned minutes after `hooks remove codex --legacy-only`, recreating the double-fire state. Claude's arm in the same function only warns; Codex needs the same restraint once its hooks can come from a plugin.

- [x] **Step 1: Write the failing test**

```rust
    /// A plugin already running hcom's Codex handlers must not be shadowed by a
    /// fresh native install: that is how `hooks remove codex --legacy-only`
    /// got undone by the next agent launch, back into a double-fire.
    #[test]
    fn codex_launch_leaves_hooks_alone_when_the_plugin_runs_them() {
        use crate::hooks::codex::CodexPluginState;

        for state in [
            CodexPluginState::Active,
            CodexPluginState::ReviewRequired,
            CodexPluginState::Duplicate,
        ] {
            assert!(
                !super::codex_launch_needs_native_hooks(state),
                "{state:?} would reinstall the legacy hooks at launch"
            );
        }

        for state in [
            CodexPluginState::Missing,
            CodexPluginState::Incomplete,
            CodexPluginState::LegacyOnly,
            CodexPluginState::Unverified,
        ] {
            assert!(
                super::codex_launch_needs_native_hooks(state),
                "{state:?} must still get working hooks"
            );
        }
    }
```

- [x] **Step 2: Run it and watch it fail**

Run: `HCOM_DIR=$(mktemp -d) cargo test launcher::tests::codex_launch_leaves_hooks_alone`
Expected: compile error — `codex_launch_needs_native_hooks` does not exist.

- [x] **Step 3: Implement**

```rust
/// Whether a Codex launch should install the native hooks.
///
/// Only when the plugin is not the thing delivering messages. An installed
/// plugin whose handlers Codex reports — even untrusted, even already
/// duplicated — must not be shadowed by a fresh native install: that turns
/// `hooks remove codex --legacy-only` into a no-op undone by the next launch.
/// `Unverified` installs, because an unreadable inventory is not evidence that
/// something else is delivering messages, and a session with no hooks is silent.
fn codex_launch_needs_native_hooks(state: crate::hooks::codex::CodexPluginState) -> bool {
    use crate::hooks::codex::CodexPluginState as S;
    !matches!(state, S::Active | S::ReviewRequired | S::Duplicate | S::Disabled)
}
```

Then in the `LaunchTool::Codex` arm, before the existing native check:

```rust
        LaunchTool::Codex => {
            let codex_home = codex_home.expect("Codex launch must resolve CODEX_HOME");
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            if !codex_launch_needs_native_hooks(
                crate::hooks::codex::codex_plugin_status(&cwd).state,
            ) {
                return Ok(());
            }
```

leaving the rest of the arm unchanged.

Add `Disabled` to the test's first list to match the implementation.

- [x] **Step 4: Run the tests**

```bash
HCOM_DIR=$(mktemp -d) cargo test launcher
HCOM_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Expected: only the known pre-existing failure.

- [x] **Step 5: Commit**

```bash
git add src/launcher.rs
git commit -m "fix(launcher): do not reinstall Codex native hooks behind a live plugin"
```

---

### Task 4: Re-point this host at the local checkout and measure

**Files:**
- Modify: `docs/superpowers/plans/2026-09-14-plugin-skill-migration-acceptance.md`

This task changes the host. The owner has authorised it for this work.

- [x] **Step 1: Reinstall each vendor through hcom's own route**

```bash
cargo build --release
./target/release/hcom hooks remove codex --legacy-only
./target/release/hcom hooks add codex
./target/release/hcom hooks add claude
./target/release/hcom hooks status
```

Expected: the Codex install now names the checkout directory as its marketplace source, not a URL. Record what each command printed.

- [ ] **Step 2: Confirm the launch path no longer resurrects the legacy hooks** (NOT RUN: a Codex agent was already on the roster and host policy forbids spawning a duplicate vendor; the guard is unit-tested instead)

```bash
./target/release/hcom hooks status | grep -A2 codex
```

Then launch a Codex agent through hcom, let it bind, and re-check:

```bash
./target/release/hcom hooks status | grep -A2 codex
```

Expected: the status is the same before and after, and `~/.codex/hooks.json` does not regain hcom's entries. If it does, stop and report rather than deleting them again.

- [x] **Step 3: Verify the installed payload is the working tree's**

```bash
diff -r skills/hcom-agent-messaging ~/.codex/plugins/cache/hcom/hcom/1.0.0/skills/hcom-agent-messaging
```

Expected: no output. Edit a line in `skills/hcom-agent-messaging/SKILL.md`, run `scripts/sync-plugin-skills.sh`, reinstall, and confirm the change reaches the installed copy — then revert the edit and reinstall once more.

- [x] **Step 4: Record and commit**

Append the measurements under a `## Local-checkout plugin source` heading, then:

```bash
git add docs/superpowers/plans/2026-09-14-plugin-skill-migration-acceptance.md
git commit -m "docs: record the local-checkout plugin source measurement"
```

---

## Self-review coverage

Decision 1 (no release) needs no code and is recorded above. Decision 2 (local checkout for path-accepting vendors) → Task 1. Decision 3 (Cursor keeps the URL, which must resolve) → Task 2. The Codex native-hook resurrection found while measuring → Task 3. End-to-end evidence on this host → Task 4.
