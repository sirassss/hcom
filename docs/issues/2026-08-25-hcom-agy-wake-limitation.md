# AGY không tự tỉnh khi có tin hcom (giới hạn hcom × Antigravity)

**Ngày:** 2026-08-25
**Mục đích:** Ghi nhận giới hạn thật khi dùng AGY trên bus hcom — worker không
tự đọc tin lúc idle; phải `hcom term inject` hoặc gõ prompt trong pane. Để
session sau brainstorm → spec → quyết vá hook local, chờ upstream hcom, hay
chỉ tài liệu hóa workaround.

`type: PROPOSAL` (chưa raise task, chưa chốt hướng fix)

**Không nhầm với:** 8 friction orchestration (settle/guard/doctor) đã fix trên
`feat/agent-ops-orchestration-friction-fixes`. Đây là lớp **giao tin / đánh
thức vendor**, không phải packet shape.

---

## Triệu chứng đo được

- Cursor spawn qua `spawn-agents.sh` / `hcsp`: ở bus, nhận tin khi idle (PTY
  inject + `cursor-stop` `loop_limit: null`).
- Claude: hết turn thì Stop hook chạy `hcom poll` (timeout dài, ~3h) → tin tới
  dưới dạng stop-hook feedback. Phụ thuộc Stop hook; hết timeout / `stale_cleanup`
  thì ngủ.
- AGY: hết lượt là im. Tin nằm trong `hcom.db` nhưng model không chạy. Phải
  `uvx hcom term inject <name>` hoặc vào pane gõ một prompt thì mới tỉnh và đọc
  tin.

`uvx hcom list -v` phân hai kiểu AGY:

- `[ANTIGRAVITY]` (hoa) — spawn qua `hcom agy` (PTY + hooks).
- `[antigravity]` thường — mở `agy` rồi join (`hcom start`) — gần như chỉ sống
  bằng hook lifecycle.

Cả hai vẫn dính “không tự tỉnh” nếu PTY inject không submit được prompt AGY
(TUI gate: user typing, prompt ownership, Enter blocked — hcom binary log
`delivery.enter_blocked` / `delivery.prompt_ownership_lost`).

---

## Cơ chế (đã đối chiếu hook thật trên máy)

### Claude — `~/.claude/settings.json`

Stop: `hcom poll`, timeout 10800s. Hook **chặn** đến tin mới, không ngắt bus.

### Cursor — `~/.cursor/hooks.json`

`stop`: `uvx hcom cursor-stop`, `loop_limit: null`. Cộng PTY inject khi spawn
qua hcom.

### AGY — `~/.gemini/config/hooks.json` (do `hcom hooks add antigravity`)

| Event | Lệnh hcom | Ý nghĩa |
|---|---|---|
| PreInvocation | `gemini-sessionstart`, `gemini-beforeagent` | Bind + giao tin **chỉ khi bắt đầu một lượt mới** |
| PostToolUse | `gemini-aftertool` | Giao tin giữa chừng lượt đang chạy |
| PostInvocation | `gemini-afteragent` | “Signal ready”, timeout 15s — không phải poll dài |
| **Stop** | **`gemini-sessionend`** | **Ngắt bus** khi execution loop kết thúc |

AGY **không có** tương đương `hcom poll` / `cursor-stop` loop. Hết lượt → Stop
→ sessionend → không còn ai chờ tin.

Đây là template hook của **hcom 0.7.25**, không phải plugin agent-ops.
`hcom hooks add antigravity` sẽ ghi đè nếu ta sửa tay file này.

### SessionStart ≠ đánh thức

Đã đo 2026-08-22 (spec plugin, Q5 / Ranh giới thành thật):

- Binary `agy` **có** event SessionStart và hook **bắn thật**.
- AGY **không đọc** `systemMessage` / `additionalContext` /
  `hookSpecificOutput` từ stdout hook → model không nhận context.
- Chiều “biết luật trước khi nghĩ” trên AGY chỉ có khi spawn qua
  `spawn-agents.sh --hcom-system-prompt` (system prompt lúc launch), không phải
  hook.

Không dùng SessionStart để vá “AGY ngủ”. Khác kênh với Stop/poll.

---

## Workaround hiện tại (không phải fix)

1. Spawn AGY bằng `hcom agy` / `spawn-agents.sh … agy` (không mở `agy` tay rồi
   join) — vẫn **không** đảm bảo PTY inject submit được.
2. Khi AGY idle: `uvx hcom term inject <name> ['<text>'] [--enter]` hoặc gõ
   prompt trong pane.
3. Coordinator đừng giả định AGY “listening” = sẽ tự chạy khi có `[TASK-]`.

---

## Gợi ý hướng (chưa quyết — spec sau)

1. **Chỉ tài liệu** — RUNBOOK + skill: AGY phải inject; không sửa hook.
2. **Sửa local** `~/.gemini/config/hooks.json` Stop: `gemini-sessionend` → poll
   dài kiểu Claude — rủi ro: AGY Stop có loop không, `hooks add` ghi đè, timeout
   Stop của AGY (docs: execution loop terminates).
3. **Upstream hcom** — Antigravity stop = poll/loop như Cursor, không sessionend;
   PTY inject tương thích TUI AGY.
4. **agent-ops** — spawn-agents in rõ “AGY không tự tỉnh”; không giả PTY inject
   luôn thành công (`reuse_note` hiện nói “hcom inject thẳng vào PTY”).

Không làm trong issue này: gộp skill hcom vào plugin; chặn spawn cùng vendor
(đó là việc khác, hội ý `hcsp` vs `spawn-agents.sh`).

---

## Debate 2026-08-25 — hcsp vs spawn-agents.sh skip-same-vendor

Participants: `@myapp-guru` (position), `@myapp-claude` (adjudicate).
`ref` DISCUSS: `/tmp/hcom-discuss-hcsp-skip.md`. Đã đọc `dotfiles/zshrc.d/hcom.zsh`
(hcsp) và `agent-ops/scripts/spawn-agents.sh` (live_agent/reuse_note, comment
L38-47 vs L71-74) trước khi trả lời.

**Kết luận:**

1. **Giữ skip mặc định trong `spawn-agents.sh`.** Script này provision roster
   worker CHUẨN (cursor+agy) cho một repo — mục đích là idempotent ("đảm bảo
   repo có đủ worker"), không phải "spawn một agent mới". Spawn trùng vendor
   trong cùng pane herdr crash/launch_failed thật (bằng chứng feno/nami
   2026-08-22, đã nằm sẵn trong comment L38-47 của chính file); ngay cả ngoài
   pane-conflict, 2 worker cùng vendor cùng role đứng idle trong 1 room không
   phục vụ mục đích của script. Không đổi default.

2. **Đồng ý mental model `hcsp` = cửa luôn-spawn, `spawn-agents.sh` = cửa
   reuse-nếu-còn-sống.** Đây là phân chia trách nhiệm đúng: `hcsp` là wrapper
   mỏng, chung, không nên tự đoán ý coordinator (khi coordinator MUỐN 1 cursor
   thứ hai — vd verify vs implement — họ có toàn quyền qua `hcsp -t <room>
   cursor` hoặc `uvx hcom cursor-agent` thẳng); dedup logic thuộc về script
   có ngữ cảnh cụ thể (spawn-agents.sh biết nó đang "provision cho repo X"),
   không thuộc về primitive chung.

3. **Không thêm `--force` vào `spawn-agents.sh`, không bỏ skip.** `hcsp` đã
   là lối thoát "tôi muốn cái mới bất kể gì" — thêm `--force` vào
   `spawn-agents.sh` là trùng chức năng, không cần thêm bề mặt API mới cho
   một nhu cầu đã có đường đi (YAGNI).

**Đồng ý luôn với đề xuất phụ của guru:** sửa comment `spawn-agents.sh:71-74`
("hcom KHÔNG tái dùng pane... luôn tab mới") cho khớp thực tế — chỉ đúng
NGOÀI herdr pane; TRONG herdr pane (như comment L38-47 đã ghi, và đúng môi
trường session này `HERDR_ENV=1 HERDR_PANE_ID=w7:p49`) spawn CHIẾM pane hiện
tại chứ không tạo tab mới. Chỉ sửa câu chữ, không đổi hành vi. Việc AGY không
tự tỉnh (nội dung chính của issue này) vẫn tách riêng, không gộp vào quyết
định skip/hcsp ở trên — đúng như guru đã khoanh vùng.

Không implement gì trong vòng debate này (skip/--force đã có sẵn, chỉ chốt
giữ nguyên) — chỉ ghi kết luận vào đây theo yêu cầu.
