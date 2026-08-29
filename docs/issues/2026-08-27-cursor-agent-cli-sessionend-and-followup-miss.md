# cursor-agent CLI: `sessionEnd` unregisters a live PTY + idle delivery miss

**Moved from:** `agent-skill` `docs/superpowers/issues/2026-08-27-hcom-cursor-agent-sessionend-kills-live-pty.md` (2026-08-29)

**Phạm vi sửa:** **chỉ hcom + Cursor CLI hooks** (`src/hooks/cursor.rs`, identity bind, `~/.cursor/hooks.json`). Không agent-ops, không đòi Cursor product “inbox trong turn”.

**Ngày:** 2026-08-27 (sessionEnd) · follow-up miss đo thêm 2026-08-29  
**Severity:** High  
**Component:** `cursor-sessionend`, `cursor-stop`, process bind / dual session UUID  
**Máy / phiên:** `review-zilo` herdr `w3:p13` 2026-08-27; `myapp-kali` operating-model dispatch 2026-08-29  
**Không phải:** `hcom stop` / `hcom kill` từ agent hay user

Hai lỗi cùng lớp tích hợp. Sửa một cái không hết cái kia.

| # | Triệu chứng | Hậu quả |
|---|-------------|---------|
| A | `sessionEnd reason=completed` khi PTY/PID còn | Instance `exit:completed` → `hcom send` không giao |
| B | Idle nhận tin qua follow-up; agent idle / cần nudge | Tin có trên bus nhưng lượt harness hỏng (wake rỗng + Stop không nhét body) |

Harness Cursor **không** giữ turn poll như Claude. Inbox vẫn là `hcom.db`. Delivery idle **cố ý** hai bước (PTY inject `<hcom>` → Stop `followup_message`). Miss = bước 2 / identity chết, không phải thiếu queue.

---

## A — `sessionEnd` unregister PTY còn sống

### Triệu chứng

- `uvx hcom cursor-agent` join (`bindings: hooks, pty`), notify OK.
- Pane herdr còn, turn sau vẫn chạy, nhưng `hcom list`: `(not participating)` → `Recently stopped`, `By: session` / `Reason: exit:completed`.
- omp cùng room không rớt.

Kỳ vọng: identity sống đến `hcom stop` / `hcom kill` / process thật sự thoát.

### Evidence (`~/.hcom/.tmp/logs/hcom.log`, zilo)

1. Bind `process_id=68cdc2b9-…`, `tool=cursor`, `tcp_mode=true`.
2. `sessionstart` `session_id=5321d1cd-…`, **transcript trống**, `fresh_process_placeholder=true`.
3. Transcript khác `session_id=6d705df0-…` (`~/.cursor/projects/.../agent-transcripts/`).
4. `historical_process_rejected` / `historical_root_hook_rejected` (hai UUID).
5. `sessionend` `instance=zilo reason=completed` **`validated=false`**; cùng lúc `pidtrack_recorded` pane còn.
6. `identity.resolve.process_binding_expired`.
7. `cursor.parse_error` `hook=cursor-stop err=EOF while parsing a value`.

### Root cause

hcom: một name = một session UUID; `sessionEnd` → `finalize_session` (`src/hooks/common.rs`) xóa instance.

cursor-agent CLI: **hai UUID** cùng process (PTY bind vs transcript `primary_session`). Lệch → historical reject. `sessionEnd` trên một UUID `completed` **cả** instance dù PID còn.

Code hiện tại: `handle_sessionend` gọi `finalize_session` không nhìn pidtrack (`src/hooks/cursor.rs`).

### Cách sửa (hcom)

1. **`handle_sessionend`:** không `finalize_session` khi `pidtrack` / herdr pane còn, hoặc `validated=false`, hoặc stdin rỗng. Log warn.
2. **Bind:** hai session UUID cùng `process_id` = một instance; đừng `historical_root_hook_rejected` giữa chúng.
3. `hcom stop` / process exit vẫn unregister.

### Reproduce / verify (A)

Reproduce: spawn `hcom cursor-agent` herdr → vài turn → idle, không `hcom stop` → `hcom list` vs pane.

- [ ] Idle vài phút vẫn `◉ listening`
- [ ] `hcom send` inject được, không cần `hcom start --as` lại
- [ ] Hết `historical_root_hook_rejected` giữa 2 UUID cùng `process_id`
- [ ] `sessionEnd` sớm không thành `exit:completed` nếu PID còn
- [ ] `hcom stop` / process chết vẫn unregister

---

## B — Follow-up miss / nudge (idle delivery)

Session: `agent-skill` `docs/superpowers/issues/2026-08-29-agent-ops-dispatch-friction-session.md` mục 1 (`kali`, 7/7 TASK `went idle without responding`).

### Đây không phải “hcom thiếu inbox”

Đã thiết kế (`tests/test_pty_delivery.rs`, `skills/.../cross-tool.md`): idle Cursor = inject **chỉ** `<hcom>` → hết turn → `cursor-stop` nhét body vào `followup_message`. UI “follows-up” là đúng Cursor CLI.

`handle_stop` đã trả `followup_message` **nhưng chỉ khi** `payload.status == "completed"`:

```604:613:src/hooks/cursor.rs
    if payload.raw.get("status").and_then(Value::as_str) != Some("completed") {
        return (json!({}), None);
    }
    match common::prepare_pending_messages(db, &instance.name) {
        Some(prepared) => (
            json!({ "followup_message": prepared.formatted }),
            Some(prepared.ack),
        ),
```

`~/.cursor/hooks.json` `stop.timeout: 15` vs `HCOM_TIMEOUT=10800`. Hook bị giết / stdin rỗng → `cursor.parse_error` → **không** followup. Agent được dạy: prompt chỉ `<hcom>` thì hết turn không ACK. Nếu Stop không đính packet → miss / nudge.

### Cách sửa (hcom + hooks.json)

1. **`handle_stop`:** nếu còn unread → luôn `followup_message` = packet đủ (`<hcom>…</hcom>`), **không** phụ thuộc `status==completed`. Stdin rỗng / parse fail: không crash im; retry hoặc log, đừng nuốt queue.
2. **Install hook:** `stop.timeout` đủ cho `cursor-stop` (vài chục giây + notify; không cần 10800 nếu không poll block). `cursor-stop` chịu stdin rỗng.
3. Phụ: một follow-up / một unread (`loop_limit`). Không loop khi hết queue.
4. **Không** đổi product Cursor; **không** `hcom listen` trong turn agent; **không** skill “nhớ ACK” làm fix chính.

Bước 1 của A bắt buộc: Stop followup vô nghĩa nếu instance đã `exit:completed`.

### Verify (B)

- [ ] Một `hcom send` lúc idle → follow-up **có packet**, không kẹt lượt chỉ `<hcom>` rồi im
- [ ] Worker ACK + làm **không** cần nudge thứ hai
- [ ] Hết unread → Stop không loop
- [ ] A vẫn đúng (`hcom stop` unregister)

---

## Ngoài phạm vi

- agent-ops / kaban / settle identity (`kali` vs `myapp-kali`)
- Cursor IDE GUI
- Poll trong-turn kiểu Claude Stop (harness không hỗ trợ; đừng thiết kế)
