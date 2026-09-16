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

## Publish release cá nhân cho fork

```sh
scripts/release-fork.sh                 # tag mặc định: v<version>-fork.<short-sha>
scripts/release-fork.sh v0.8.0-siras.2  # tự đặt tag
```

Yêu cầu trước khi chạy:
- Branch hiện tại đã `git push` lên `origin` (script tự kiểm tra, báo lỗi nếu chưa).
- `dist --version` khớp `cargo-dist-version` trong `dist-workspace.toml`.
- `gh auth status` đã đăng nhập đúng tài khoản của fork.

Script tự làm gì: build binary local (target hiện tại) + artifact global (`hcom-installer.sh`)
qua `dist`, tạm sửa `repository` trong `Cargo.toml` chỉ để build (tự revert ngay sau, không bao
giờ commit thay đổi đó), rồi `gh release create/upload` lên **đúng fork của bạn** (tự đọc từ
`git remote origin`, không hardcode) — không đụng các job publish homebrew/pypi mà
`dist-workspace.toml` cấu hình cho upstream.

Cài trên máy khác sau khi release xong:

```sh
curl -fsSL https://github.com/<fork>/releases/latest/download/hcom-installer.sh | sh
```

`/latest/` chỉ resolve nếu release không bị đánh dấu prerelease — `gh release edit <tag>
--prerelease=false` nếu cần (script không tự làm việc này).
