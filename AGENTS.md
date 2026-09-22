# This file provides guidance to coding agents (Claude Code, Codex, Cursor, …) when working with code in this repository.

## Commands

```bash
just ci                      # full local gate: dist-check, typecheck, fmt, clippy, test, msrv, real-tool tests
just ci fmt clippy test      # only the named steps
just ci-logs <step>          # full log of one step (ci prints only ok/FAILED per step)
cargo test --locked          # unit + integration tests
cargo test --locked <name>   # single test by substring
cargo test --locked --test cli_smoke -- --exact <test_fn>   # single test in one integration binary
just fmt                     # rustfmt (the ci fmt step only checks)
```

Real-tool tests drive genuine `claude`/`codex` binaries, are `#[ignore]`d, and need the pinned CLIs on PATH:

```bash
just mock-tools              # installs pinned CLIs into target/mock-tools (cached)
PATH="$PWD/target/mock-tools/bin:$PATH" cargo test --locked --test real_tool_claude -- --ignored --nocapture --test-threads=1
```

Run the local build: `hcom config dev_root $(pwd)`. Concurrent worktrees need their own DB: `HCOM_DIR=$PWD/.hcom HCOM_DEV_ROOT=$PWD hcom claude`.

MSRV is 1.88 (`Cargo.toml`); CI lints on a pinned newer toolchain.

## Test file layout

**When a source file exceeds 1000 lines and still carries an inline `#[cfg(test)]` module, extract the tests to a sibling file.** Precedent: commit `2bd3bfa`.

```rust
// bottom of src/router.rs
#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
```

```rust
// src/router_tests.rs — first line
use super::*;
```

- Naming is by file, not module: `mod.rs` → `mod_tests.rs` (`src/db/mod.rs`, `src/relay/mod.rs`), `claude.rs` → `claude_tests.rs`.
- This is **not** a 1000-line cap on files. Extracted test files are routinely larger than their parent (`src/hooks/claude_tests.rs` is ~3400 lines) — do not shard them further.
- Files under 1000 lines keep tests inline; splitting them adds noise.

## Architecture

Single Rust binary, no background services. Hooks installed into each tool's own config dir write activity to a local SQLite DB; delivery reads from it and injects into the target agent's PTY.

```
agent → hooks → db → hooks/PTY delivery → other agent
```

**`src/router.rs` — dispatch.** `argv[1]` is matched against known hook names *before* clap parses anything (`Action::Hook`), so hook invocations never hit the CLI grammar. Everything else is clap-derived.

**`src/integration_spec.rs` — the per-tool config plane.** One `IntegrationSpec` const per `Tool` variant holds facts that used to be scattered across `tool.rs`, `delivery.rs`, `commands/help.rs`, `hooks/family.rs`, `launcher.rs`, `commands/{launch,resume}.rs`. It is deliberately *not* "one file to add a tool" — behavioral integration still lives elsewhere, and the spec's own doc comment lists what stays out: `HOOK_REGISTRY` (Claude-only, `src/hooks/utils.rs`), tool env detection (`shared::tool_detection`), transcript parsers (`transcript::TranscriptBackend`), system-prompt env keys (`config.rs::FIELD_TO_ENV`). Adding a tool means touching the spec *and* those modules.

**`src/db/` — three loosely-coupled state planes in one SQLite file.**
- `instances`: live per-agent state (TUI display, gating, delivery cursors)
- `events`: append-only history / message log / relay replication source
- `process_bindings`, `session_bindings`, `notify_endpoints`, `kv`: routing and control plane

`SCHEMA_VERSION` (`src/db/mod.rs:40`) is currently 19; schema changes need a migration step, not just a DDL edit.

**`src/hooks/` — per-tool hook handlers** (`claude.rs`, `codex.rs`, `gemini.rs`, `cursor.rs`, …) over shared infrastructure in `common.rs`. These are the largest and most tool-quirk-dense files in the tree.

**`src/pty/` + `src/delivery.rs` — message injection.** The PTY wrapper spawns the tool under a vt100-tracked screen with a TCP injection server; delivery injects a message and confirms it by watching the instance's cursor advance. Tool-specific injection quirks (prompt readiness, spinner glyphs, Enter handling) live per-tool, not in the loop.

**`src/instance_lifecycle.rs` + `src/pidtrack.rs` — liveness.** Orphan detection and reboot reconciliation are PID-namespace-aware; liveness checks must not reap agents from another namespace.

**`src/relay/` — cross-device MQTT.** Retained per-device state topics plus a non-retained control topic; payloads are XChaCha20-Poly1305 under a shared PSK. See README's relay security section for the trust model before changing anything here.

**`src/tui/` — ratatui dashboard** (`hcom` with no args), reading the same DB.

## Plugin packages

`plugin/hcom/` and `plugin/hcom-agy/` carry **generated** copies of `skills/hcom-agent-messaging/`. Vendors disagree on symlink handling (Claude dereferences, Codex skips), so every adapter holds real files.

Edit `skills/hcom-agent-messaging/` and run `scripts/sync-plugin-skills.sh` (add `--publish` to also push `plugin/` to the standalone plugin repo). Never hand-edit files under `plugin/*/skills/` — `tests/plugin_payload.rs` fails on drift, and Linux CI is the authoritative no-symlink gate.

## Docs

Bug write-ups go in `docs/issues/` as `YYYY-MM-DD-slug.md`.
