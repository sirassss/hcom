# cursor-agent: hook của plugin chỉ bắn cho event đã khai trong hooks.json

**Ngày:** 2026-10-02 · **Phát hiện:** người dùng (máy B, tin không tới Cursor) · **Chẩn đoán:** probe trong Docker, cùng ngày
**Component:** `plugin/hcom` (đã gỡ), `src/hooks/cursor.rs`, `src/hooks/runtime.rs:129`

type: bug · **TRẠNG THÁI:** FIXED — gỡ plugin Cursor trong `3485d41` (`build(plugin): package hcom for Antigravity`); hook Cursor chỉ ghi vào `~/.cursor/hooks.json`.

## Triệu chứng

Máy B (được người dùng báo, chưa tự đo): tin gửi tới instance Cursor được ghi là unread và `<hcom>` đã được inject vào màn hình cursor-agent, nhưng cursor-agent không nhận nội dung tin. Theo người dùng, máy B không có `beforeSubmitPrompt`/`stop` từ plugin; khi hai event này nằm trong `~/.cursor/hooks.json` thì chạy bình thường. Máy A (máy dev này) không bị.

## Bằng chứng đo được (2026-10-02)

Container Ubuntu 24.04 sạch, `cursor-agent` 2026.10.01-e373342 và 2026.09.28-64d2043, backend mock (giống `tests/support/cursor_mock.rs`, không có agent run service), hook giả chỉ ghi tên hook vào file. Mỗi cấu hình: một phiên TUI, gõ một prompt, thoát.

| Cấu hình | Hook bắn | n |
|---|---|---|
| không hook | không | 1 (10.01) |
| chỉ `--plugin-dir` | `sessionStart`, `sessionEnd` | 3 (10.01), 1 (09.28) |
| plugin + `~/.cursor/hooks.json` rỗng `{"hooks":{}}` | `sessionStart`, `sessionEnd` | 1 (10.01) |
| plugin + `~/.cursor/hooks.json` chỉ khai `beforeSubmitPrompt` | + `beforeSubmitPrompt`, không có `stop` | 1 (10.01) |
| chỉ `~/.cursor/hooks.json` đủ 6 event | đủ 6 event | 1 (10.01), 1 (09.28) |
| chỉ `<ws>/.cursor/hooks.json` đủ 6 event | đủ 6 event | 1 (10.01) |
| plugin + `~/.cursor/hooks.json` đủ 6 event | đủ 6 event, **mỗi hook bắn 2 lần** (một của plugin, một của hooks.json) | 1 (10.01), 1 (09.28) |

## Cơ chế

Suy ra từ hành vi trên, chưa đọc mã cursor-agent: hook của plugin chỉ được gắn cho event mà một `hooks.json` (home hoặc project) đã khai. Delivery của hcom cần `beforeSubmitPrompt` (lấy nội dung tin sau khi `<hcom>` được inject) và `stop` (follow-up), nên plugin đứng một mình không giao được tin.

Upstream đã ghi nhận cùng hiện tượng ở `src/hooks/runtime.rs:129`: cursor-agent nạp hook `--plugin-dir` bất đồng bộ và `beforeSubmitPrompt`/`stop` thường không bắn từ đó. Probe cho thấy không phải do nạp muộn (n=3 ổn định) mà do thiếu khai báo event.

## Vì sao máy A không bị

Suy luận, chưa kiểm chứng: `~/.cursor/hooks.json` trên máy A có hook của tool khác (`agentpet-hook`, `herdr-agent-state.sh`) đã khai `beforeSubmitPrompt`/`stop`, nên hook plugin chạy nhờ. Máy B được người dùng xác nhận là không có hai event này.

## Chưa biết

- Lượt trả lời thật của model chưa đo (mock không có agent run service). `stop` vẫn bắn trong mock.
- Antigravity (`plugin/hcom-agy`) chưa probe; có thể có cùng hạn chế.
- `~/.claude/settings.json` không được cursor-agent đọc trong container sạch (n=1), trái với ghi nhận ngày 2026-09-03 trên máy A. Điều kiện chưa rõ.

## Hướng sửa

Cursor không đóng gói plugin; hook ghi bởi `try_setup_cursor_hooks` vào `~/.cursor/hooks.json` là nguồn duy nhất. Đã thực hiện trong `3485d41`. Bắn đôi (hàng cuối bảng) là lý do thêm để không giữ plugin Cursor cạnh hooks.json.

## Tái hiện

1. Tạo plugin có `.cursor-plugin/plugin.json` với `"hooks": "./hooks/h.json"`, trong `h.json` khai 6 event, mỗi lệnh ghi tên event vào một file log.
2. `HOME` sạch (không có `~/.cursor/hooks.json`), chạy `cursor-agent --plugin-dir <plugin>`, gõ một prompt, thoát. Log chỉ có `sessionStart`/`sessionEnd`.
3. Thêm `~/.cursor/hooks.json` khai `beforeSubmitPrompt` và `stop`, chạy lại: log có đủ event.

Các bước trên là bản tương đương dùng tài khoản thật; số liệu ở bảng đo bằng mock trong container. Script probe nằm ở thư mục tạm của phiên, chưa commit.
