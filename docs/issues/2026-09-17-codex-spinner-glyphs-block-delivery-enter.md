# Glyph spinner của Codex lọt vào prompt text, `prompt_ownership` ra `Mixed` và delivery không bấm Enter

**Ngày:** 2026-09-17
**Mục đích:** Ghi nhận lý do một agent Codex thỉnh thoảng bị treo với `<hcom>` nằm lì trong ô
input mà không bao giờ chạy lượt để đọc tin. Người dùng phát hiện; mila (agent Codex) tự chẩn
trên PTY và log của chính nó; tôi (kume) đối chiếu lại code.

`type: BUG`

**TRẠNG THÁI: ĐÃ FIX** (2026-09-17, commit `601d468` trên `feat/siras/develop`, **chưa push**).
`get_codex_input_text` cắt một chuỗi đuôi gồm whitespace + Braille (U+2800–U+28FF).
`prompt_ownership` **không** bị đụng. Do agent Codex `lori` thực hiện.

Hai câu hỏi để ngỏ trong doc này đã được đo và trả lời:
- Retry **không** tích luỹ `<hcom>`: gate `prompt_has_text` chặn lần inject kế khi ô input còn
  chữ (log cho thấy `inject_attempt` 3 rồi 4 **sau khi** prompt đã sạch).
- Dòng prompt **chỉ** có Braille làm nhiễu: parse toàn bộ `phase1_poll` input bắt đầu bằng
  `<hcom>` trong log — 7 ca sạch, 4 ca đuôi chỉ whitespace + Braille, 0 ca có ký tự khác. Text
  model/status của Codex nằm ở dòng riêng.

Tính fail-safe được chốt bằng test `codex_keeps_unknown_prompt_suffix`: glyph lạ không được cắt
thì chuỗi vẫn không khớp tuyệt đối và `Mixed` từ chối submit — tức hành vi hiện tại, không phải
hồi quy.

---

## Triệu chứng

Tin nhắn gửi tới một agent Codex "được giao" nhưng agent không bao giờ đọc. Nhìn màn hình
Codex thấy `› <hcom>` đã nằm sẵn trong ô input và đứng yên. Không mất tin — tin vẫn `pending`
trong DB — nhưng agent im lặng cho tới một lượt deliver sau đó may mắn thành công.

Không xảy ra mọi lần. Đây là lý do nó khó bị bắt.

## Bằng chứng đo được (2026-09-17, `~/.hcom/.tmp/logs/hcom.log`)

**Lượt hỏng — message #7418, 08:27:30Z:**

```
delivery.injected      <hcom>
phase1_poll            input = "<hcom>  ⠄  ⠠⡀ ..."      ← glyph spinner Codex dính vào
delivery.mixed_prompt  "refusing automatic submission"
(không có delivery.send_enter)
```

`gate_pass` cho thấy `user_active=false` và `approval=false`. TUI history còn lại một dòng
`› <hcom>` từ đúng lượt đó.

**Lượt tốt — message #7433, 08:31:35Z, cùng agent, cùng phiên:**

```
phase1_poll            input = "<hcom>"                 ← đọc sạch
delivery.text_rendered exclusive
delivery.send_enter
codex-userpromptsubmit (hook chạy)
delivery.success
```

Cùng một đường code, khác nhau duy nhất ở chuỗi mà screen reader đọc ra.

## Cơ chế

Delivery gửi text và Enter bằng **hai** lần ghi TCP riêng biệt — `inject_text`
(`src/delivery.rs:1414`) rồi `inject_enter` (`src/delivery.rs:1431`). Đây là **cố ý**, không
phải quên: Enter chỉ được bấm sau khi xác minh màn hình.

Cổng quyết định là `prompt_ownership` (`src/delivery.rs:1459`):

```rust
Some(input) if input == injected_text  => Exclusive   // được submit
Some(input) if input.contains(injected_text) => Mixed // TỪ CHỐI submit
_ => Other
```

`Mixed` **không phải bug** — nó tồn tại để Enter không submit nhầm chữ người dùng đang gõ dở.
Đừng nới nó.

Vấn đề nằm ở phía đọc. `get_codex_input_text` (`src/pty/screen.rs:893`) tìm ngược từ dưới lên
tới dòng bắt đầu bằng `› `, lấy **toàn bộ** phần còn lại của dòng, rồi `trim_with_nbsp`
(`src/pty/screen.rs:145`):

```rust
s.trim_matches(|c: char| c.is_whitespace() || c == '\u{00A0}')
```

Codex vẽ spinner **trên cùng dòng với prompt**. Glyph spinner là Braille Patterns
(U+2800–U+28FF), và chúng **không phải whitespace** theo `char::is_whitespace()` — kể cả
U+2800 BRAILLE PATTERN BLANK, vốn trông như khoảng trắng nhưng Unicode không xếp là whitespace.
Nên `trim_with_nbsp` không cắt được, spinner sống sót vào chuỗi trả về, `input != injected` mà
`input.contains(injected)` ⇒ `Mixed` ⇒ không bấm Enter.

Tính ngắt quãng là do spinner có đang ở khung vẽ đúng lúc `phase1_poll` đọc hay không.

## Vì sao không tự khỏi hẳn

Nhánh `MixedPrompt` (`src/delivery.rs:2364`) log cảnh báo rồi `delivery_state = State::Pending`,
`inject_attempt += 1`, `continue` — tức là **thử lại**. Nên tin cuối cùng thường vẫn tới được,
chỉ là trễ và kèm một khoảng agent đứng hình.

**Chưa đo:** mỗi lần thử lại có inject thêm một `<hcom>` nữa vào ô input không. Nếu có thì ô
input tích luỹ `<hcom><hcom>…`, và chuỗi đọc ra càng lệch khỏi chuỗi inject, làm `Mixed` càng
dai. Cần xác nhận trước khi sửa, vì nó đổi hình dạng của fix.

## Không phải nguyên nhân (đã loại)

- **Không phải "quên gửi Enter".** Hai lần ghi TCP tách rời là thiết kế.
- **Không phải `user_active` hay `approval`.** Cả hai `false` trong `gate_pass` của lượt hỏng.
- **Không phải placeholder/dim misclassification.** Giả thuyết ban đầu của tôi, và sai. Đường
  `is_codex_placeholder` (`src/pty/screen.rs:58`) và bộ phát hiện dim (`:561-754`, từ commit
  `bfd4fc8`) hoạt động đúng; chuỗi đọc ra có chứa `<hcom>` thật, không bị coi là ô rỗng.

## Hướng fix (chưa quyết)

Sửa ở screen reader, không sửa ở `prompt_ownership`.

Trong `get_codex_input_text`, cắt **một chuỗi đuôi** gồm whitespace và ký tự Braille Patterns
(U+2800–U+28FF) trước khi trả về. Cắt ở đuôi chứ không lọc toàn chuỗi, để text Braille mà người
dùng thật sự gõ ở giữa câu không bị nuốt, và để bảo vệ `Mixed` cho chữ user vẫn còn nguyên.

Cần trước khi làm:
- Xác nhận câu hỏi "có inject lặp không" ở trên.
- Kiểm xem Codex còn vẽ gì khác trên dòng prompt (đếm token? thời gian?) — nếu có, cắt riêng
  Braille là chưa đủ và cần một cách khoanh vùng ô input chắc hơn là "phần còn lại của dòng".
- Một test dựng screen có spinner trên dòng `› ` và assert `get_codex_input_text` trả đúng
  `<hcom>`. `src/pty/screen.rs` đã có sẵn nếp test dựng màn hình bằng `t.process(b"…")`.

## Ghi chú phạm vi

Phát hiện trong lúc kiểm một việc khác, không liên quan tới branch kill-race/plugin-verifier
(`0f8e3dd..b58c4a0`) và cố ý không vá kèm vào đó — khác subsystem (PTY screen reader).
