# Committed plugin skill copies

Decision date: 2026-09-14. Owner decision, recorded here because it reverses a
constraint in the [plugin skills plan](../plans/2026-09-13-plugin-skills-and-host-customization.md)
("Không commit generated SKILL copies").

## Problem

`plugin/hcom/skills` is a symlink to the repository's canonical
`skills/hcom-agent-messaging`, so one skill source serves every adapter. Vendors
copy a plugin package into their own cache at install time, and they do not agree
on what to do with that link.

Measured 2026-09-14 on this host:

| Vendor | Installed cache carries `skills/` |
|---|---|
| Claude (`claude plugin install`, git-URL marketplace) | yes — the link is dereferenced |
| Codex (`codex plugin add`, 0.154.0, local-path marketplace) | **no** — the link is skipped |
| Antigravity (`agy plugin install <dir>`) | only because hcom stages a materialized copy first |

A Codex install therefore ends up with working hooks and no messaging skill: the
agent receives messages and has not been taught to use them.

hcom works around this today by staging a materialized tree before invoking the
vendor CLI (`src/hooks/plugin_stage.rs`). That only works where hcom drives the
install from a local checkout. It cannot help the route the owner intends to use
from now on — a pushed repository consumed as a git marketplace, where the vendor
clones and copies without hcom in the loop.

## Decision

Stop shipping the skill as a link. Each plugin adapter carries its own real copy
of the canonical skill, committed to the repository:

- `plugin/hcom/skills/hcom-agent-messaging/` (Claude, Cursor, Codex)
- `plugin/hcom-agy/skills/hcom-agent-messaging/` (Antigravity)

`skills/hcom-agent-messaging/` at the repository root stays the single authored
source. The adapter copies are **generated**, never hand-edited, and a test fails
the build when any of them drifts from the source by a single byte.

This accepts duplicated bytes in the tree to buy an install that works on every
vendor without hcom present. The duplication is machine-maintained, so the
failure mode the original constraint guarded against — two hand-maintained
SKILL.md files — does not return.

## Consequences

- Marketplace installs (Claude, Cursor, Codex) work from a plain `git clone` of
  the repository with no staging step. Antigravity still needs a local directory
  — `agy plugin install` takes a path, not a URL — so `install_agy_plugin` keeps
  both its checkout requirement and its staging. The checkout requirement does
  not go away; only the Codex workaround does.
- `codex_marketplace_source()` and `stage_marketplace()` in `src/hooks/plugin.rs`
  exist only to work around the link and are removed. Codex uses the same
  `marketplace_source()` policy as Claude.
- `materialize_plugin_artifact()` stays, but **it is not the guard against a
  symlink returning** — an earlier draft of this spec claimed it was, and that is
  wrong. `materialize_contents` (`src/hooks/plugin_stage.rs:79-101`) drops the
  adapter's own `skills` entry and copies the repository-root canonical tree in
  its place, and it dereferences links inside the source rather than rejecting
  them. Two consequences:
  - The guard is the new test asserting no file under `plugin/` is a symlink.
  - A staged AGY artifact is built from the canonical root, so it looks correct
    even when the committed adapter copy is absent or stale. Any acceptance probe
    that goes through hcom's own AGY install therefore cannot detect adapter
    drift; only the byte comparison against the committed tree can.
- Every edit to the canonical skill — content, rename, addition, deletion — needs
  a regeneration step before commit. A script performs it and a test enforces it.
- The regeneration script and the acceptance probes are Bash. On Windows they
  need Git Bash; no second PowerShell implementation is added. Linux CI is the
  authoritative no-symlink gate, because with `core.symlinks=false` a Windows
  checkout materializes a tracked link as an ordinary text file and a
  filesystem-only check would not see it.
- **Release discipline.** `release.yml` triggers on a tag push, runs no
  `cargo test`, and does not depend on `ci.yml`, so today a revision whose
  payload test never ran can still be tagged and published. Either the release
  workflow gains that gate or the release procedure requires
  `cargo test --test plugin_payload` on the exact revision being tagged.

## Publication order

The marketplace source is a git remote URL with no branch or ref
(`marketplace_source()`, `src/hooks/plugin.rs:503-517`), so a vendor resolves the
remote's default branch. Committing locally, or pushing only
`feat/siras/develop`, does not put these copies on the ref vendors consume. The
live route must not be switched to the plain marketplace source until the
consumed ref carries them, and a local-path marketplace proves only the tree's
contents — never the vendor's git acquisition or ref selection.

## Non-goals

- Changing how hooks are declared. The Codex overlay is measured working and is
  untouched.
- Changing the canonical skill's content.
- Publishing to any external marketplace registry.
