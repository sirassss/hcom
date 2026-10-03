# Cheatsheet: real-tool CI cục bộ + release cho fork

## Chạy real-tool CI ở local (thay vì đợi GitHub Actions)

```sh
just mock-tools                 # cài claude/codex pinned vào target/mock-tools (một lần, cache lại)
just ci                         # chạy hết: dist-check, typecheck, fmt, clippy, unit tests, real-tool tests
just ci real_tool_claude        # chỉ một step (xem tên step trong Justfile: real_tool_claude/real_tool_codex/test_relay_roundtrip/...)
just ci-logs                    # liệt kê log từng step
just ci-logs real_tool_claude   # xem log đầy đủ của một step
```

Yêu cầu: `cargo`, `npm`/node đã cài (xem `.node-version`). Không cần `sudo`, không cài gì
ngoài `target/mock-tools` + `target/npm-cache` (project-local, gitignored, xoá tự do bằng
`rm -rf target/mock-tools target/npm-cache`).

## Real-tool test trong Docker (không cần tài khoản/API key)

```sh
scripts/tests/docker-real-tool.sh                   # kịch bản Claude -> Cursor
scripts/tests/docker-real-tool.sh real_tool_claude  # test binary khác
scripts/tests/docker-real-tool.sh real_tool_claude_claude  # Claude <-> Claude (khứ hồi)
scripts/tests/docker-real-tool.sh real_tool_claude_codex   # Claude <-> Codex (khứ hồi)
scripts/tests/docker-real-tool.sh real_tool_claude_agy     # Claude -> agy thật (opt-in, xem bên dưới)
```

Phase prepare có mạng (build image, cài Claude pin, biên dịch), phase chạy dùng `--network none`.
Kịch bản agy là ngoại lệ của "không cần tài khoản": agy không có backend mock (đòi OAuth Google), nên script mount binary `agy` của host (read-only) cùng một BẢN SAO login (chỉ `antigravity-oauth-token` + `installation_id`, lấy từ `$HCOM_RT_AGY_AUTH_DIR` hoặc `~/.gemini/antigravity-cli`, xoá khi script thoát), chạy phase run có mạng và tốn một ít quota model thật. Test bắt đúng bản `agy` đã pin (`PINNED_AGY` trong `tests/support/agy_real.rs`), không nằm trong `just ci`.
Kịch bản Cursor cần `cursor-agent` đã cài trên host (thư mục phiên bản được mount read-only);
đặt `HCOM_RT_CURSOR_DIR` nếu không ở `~/.local/share/cursor-agent/versions/<phiên bản>`.

## Publish release cá nhân cho fork

```sh
scripts/release-fork.sh                 # tag mặc định: v<version>-fork.<short-sha>
scripts/release-fork.sh v0.8.0-siras.2  # tự đặt tag
scripts/release-fork.sh --build-only v0.8.0-siras.2  # chỉ build vào target/distrib, không gate CI, không upload
```

Yêu cầu trước khi chạy:
- Branch hiện tại đã `git push` lên `origin` (script tự kiểm tra, báo lỗi nếu chưa).
- `docker` chạy được không cần `sudo`.
- `dist --version` khớp `cargo-dist-version` trong `dist-workspace.toml`.
- `gh auth status` đã đăng nhập đúng tài khoản của fork.

Script tự làm gì: build `x86_64-unknown-linux-gnu` trong `rust:<version>-bullseye` (glibc 2.31,
khớp ngưỡng `check_glibc 2.31` của installer) và `x86_64-unknown-linux-musl` (static) trong
`rust:<version>-alpine`, dùng bản `dist` musl tĩnh tải về `target/fork-release/`. Không build
trên host: binary sẽ đòi glibc của host (2.39 trên Ubuntu 24.04) và không chạy được trên
Ubuntu 22.04/Debian 12/RHEL 9, trong khi installer vẫn báo cài thành công. Sau đó build
artifact global (`hcom-installer.sh`, có nhúng checksum) qua `dist`, tạm sửa `repository` trong `Cargo.toml` chỉ để build (tự revert ngay sau, không bao
giờ commit thay đổi đó), rồi `gh release create/upload` lên **đúng fork của bạn** (tự đọc từ
`git remote origin`, không hardcode) — không đụng các job publish homebrew/pypi mà
`dist-workspace.toml` cấu hình cho upstream.

Cài trên máy khác sau khi release xong:

```sh
curl -fsSL https://github.com/<fork>/releases/latest/download/hcom-installer.sh | sh
```

`/latest/` chỉ resolve nếu release không bị đánh dấu prerelease — `gh release edit <tag>
--prerelease=false` nếu cần (script không tự làm việc này).

## Plugin Antigravity — khi nào gỡ

Fork chỉ đóng gói plugin cho **Antigravity (AGY)** (`plugin/hcom-agy`), vì upstream
mới nạp hook per-run cho Claude, Codex, Copilot, Pi, OMP, OpenCode, Kilo. AGY vẫn dùng
hook cài global (`hcom hooks add antigravity`).

Cursor **không** có plugin hook: đo trên cursor-agent 2026.09.28 và 2026.10.01, hook của
plugin chỉ bắn cho event đã được khai trong một `hooks.json`, và khi đứng cạnh `hooks.json`
của hcom thì mỗi hook bắn hai lần. Hook Cursor chỉ ghi vào `~/.cursor/hooks.json`.

Khi upstream ra bản hỗ trợ per-run hook cho AGY (kiểm tra `HookMode::of` trong
`src/hooks/runtime.rs`), gỡ phần này:

```bash
git tag -l 'siras/agy-plugin*'            # commit gói plugin AGY
git revert <commit của tag>               # hoặc: git show <tag> --stat để xem cần xoá gì
```

`scripts/sync-plugin-skills.sh` chỉ sinh `plugin/hcom-agy/skills`;
`tests/plugin_payload.rs` báo lỗi khi bản sao lệch khỏi `skills/hcom-agent-messaging`.
