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

## Tests

Keep unit tests inline in `#[cfg(test)] mod tests` beside the code, as upstream does. Do not extract them to sibling `*_tests.rs` files: the fork did once, and every upstream edit to an inline test module then conflicted.

## Architecture

Single Rust binary, no background services. Hooks (loaded per launch for most tools, installed persistently for Cursor, Gemini, Kimi and Antigravity) write activity to a local SQLite DB; delivery reads from it and injects into the target agent's PTY.

```
agent → hooks → db → hooks/PTY delivery → other agent
```

**`src/router.rs` — dispatch.** `argv[1]` is matched against known hook names *before* clap parses anything (`Action::Hook`), so hook invocations never hit the CLI grammar. Everything else is clap-derived.

**`src/integration_spec.rs` — the per-tool config plane.** One `IntegrationSpec` const per `Tool` variant holds facts that used to be scattered across `tool.rs`, `delivery.rs`, `commands/help.rs`, `hooks/family.rs`, `launcher.rs`, `commands/{launch,resume}.rs`. It is deliberately *not* "one file to add a tool" — behavioral integration still lives elsewhere, and the spec's own doc comment lists what stays out: `HOOK_REGISTRY` (Claude-only, `src/hooks/utils.rs`), tool env detection (`shared::tool_detection`), transcript parsers (`transcript::TranscriptBackend`), system-prompt env keys (`config.rs::FIELD_TO_ENV`). Adding a tool means touching the spec *and* those modules.

**`src/db/` — three loosely-coupled state planes in one SQLite file.**
- `instances`: live per-agent state (TUI display, gating, delivery cursors)
- `events`: append-only history / message log / relay replication source
- `process_bindings`, `session_bindings`, `notify_endpoints`, `kv`: routing and control plane

`SCHEMA_VERSION` (`src/db/mod.rs`) tracks upstream; schema changes need a migration step, not just a DDL edit. Prefer a `launch_context` JSON key (as `pid_identity` does) over a column when the data is per-run metadata: it needs no bump, so the fork never collides with upstream's next schema version.

**`src/hooks/` — per-tool hook handlers** (`claude.rs`, `codex.rs`, `gemini.rs`, `cursor.rs`, …) over shared infrastructure in `common.rs`. These are the largest and most tool-quirk-dense files in the tree.

**`src/pty/` + `src/delivery.rs` — message injection.** The PTY wrapper spawns the tool under a vt100-tracked screen with a TCP injection server; delivery injects a message and confirms it by watching the instance's cursor advance. Tool-specific injection quirks (prompt readiness, spinner glyphs, Enter handling) live per-tool, not in the loop.

**`src/instance_lifecycle.rs` + `src/pidtrack.rs` — liveness.** Orphan detection and reboot reconciliation.

**`src/relay/` — cross-device MQTT.** Retained per-device state topics plus a non-retained control topic; payloads are XChaCha20-Poly1305 under a shared PSK. See README's relay security section for the trust model before changing anything here.

**`src/tui/` — ratatui dashboard** (`hcom` with no args), reading the same DB.

## Plugin packages

Upstream loads hooks per launch (`src/hooks/runtime.rs`) for Claude, Codex, Copilot, Pi, OMP, OpenCode and Kilo. Cursor, Gemini, Kimi and Antigravity still use persistent hooks. Only **Antigravity** is packaged as a plugin here: `plugin/hcom-agy` (a generated real copy of `skills/hcom-agent-messaging/`, since `agy` does not dereference symlinks). `plugin/hcom` is upstream's Claude manifest plus the `skills` symlink; Cursor has no plugin hooks. Measured on cursor-agent 2026.09.28 and 2026.10.01: a plugin's hook for an event fires only if a `hooks.json` also declares that event, and a plugin beside hcom's own `hooks.json` fires every hook twice, so Cursor hooks live only in `~/.cursor/hooks.json` (`src/hooks/cursor.rs`).

Edit `skills/hcom-agent-messaging/` and run `scripts/sync-plugin-skills.sh` (add `--publish` to also push `plugin/` to the standalone plugin repo). Never hand-edit `plugin/hcom-agy/skills/` — `tests/plugin_payload.rs` fails on drift.

When upstream gains per-run hooks for Antigravity (`HookMode::of` in `src/hooks/runtime.rs`), revert the commit tagged `siras/agy-plugin`; see `scripts/README.md`. The tag follows the commit only until the next rebase onto a newer upstream: find the commit again with `git log --grep='REVERT MARKER'` and move the tag. The revert does not touch this section, so delete it by hand afterwards.

## Docs

Bug write-ups go in `docs/issues/` as `YYYY-MM-DD-slug.md`.
