# Plan: làm gọn skills/hcom-agent-messaging

Baseline: SKILL.md 261 dòng (gồm diff chưa commit về "message ngắn gọn"), references 755 dòng.

## Kết luận trước: KHÔNG tách skill thứ hai

Seam tự nhiên có sẵn (interactive vs script mode), nhưng giá một thư mục skill mới:
- `scripts/sync-plugin-skills.sh:43-44` hardcode `skills/hcom-agent-messaging` (1 dir, copy theo tên)
- `tests/plugin_payload.rs:134,176,234` hardcode đường dẫn SKILL.md + references
- thêm 1 `description` nữa nằm thường trực trong skill-list của MỌI agent

Thêm/bớt file trong `references/` thì free (sync copy cả cây). Nên: gọn bằng cách
cắt + đẩy xuống references, không tách skill.

## Hai ngân sách token khác nhau (đừng lẫn)

| nguồn | trả khi nào | dung lượng |
|---|---|---|
| `src/bootstrap.rs` UNIVERSAL | MỌI agent, lúc connect, luôn luôn | ~60 dòng |
| `SKILL.md` | chỉ khi skill trigger | 261 dòng |
| `references/*` | chỉ khi đọc | 755 dòng |

Cắt SKILL.md **không** giảm chi phí mỗi agent. Nếu ý "thông tin thừa" là cái mọi
agent phải gánh → đòn bẩy là bootstrap.rs, việc khác, không gộp vào lần này.
(Trùng lặp bootstrap↔SKILL là cố ý: agent có thể không bao giờ load skill.)

## Cắt gì (261 → ~125 dòng)

| dòng | mục | làm gì | -dòng |
|---|---|---|---|
| 16-19 + 141-143 | curl installer 2 lần | giữ 1 trong setup | -4 |
| 90-103 | `## what humans can do` | xoá — 6 câu ví dụ, 0 hành vi cho agent | -14 |
| 106-117 | `## what agents can do` | xoá 4 bullet (đã có trong bootstrap CAPABILITIES); GIỮ câu "message ngắn, plain text" | -10 |
| 120-134 | `## tool support` | bảng 11 dòng cột "delivery: automatic" → 1 câu + trỏ cross-tool.md; giữ dòng session-binding | -10 |
| 137-198 | `## setup` + `## troubleshooting` | chuyển nguyên sang `references/troubleshooting.md` (file mới), để lại pointer 3 dòng | -59 |
| 185-191 | `### intent system` | xoá hẳn, không move — trùng y hệt bootstrap MESSAGES | (trong -59) |
| 217-226 | bảng agent topologies | xoá — patterns.md đã có Summary Table | -10 |
| 229-239 | `## files` | rút còn 4 dòng | -7 |
| 254-261 | `## more info` | rút còn 2 dòng (`hcom --help` đã nhắc ở trên) | -6 |

GIỮ NGUYÊN (đây là phần đáng tiền, dạy failure-mode mà `--help` không dạy):
- `## interactive coordination` 24-87 trọn vẹn: tags vs threads, join the room
  before you talk, stop is not kill, HCOM_TAG vs `hcom config tag`.
- `## workflow scripting` key rules (207-216).

Nguyên tắc lọc: cái gì `hcom --help` đã trả lời thì xoá, không move.

## Thứ tự thực hiện

1. tạo `references/troubleshooting.md` (setup + troubleshooting - intent system)
2. cắt SKILL.md theo bảng trên, cập nhật bảng `## reference files` thêm 1 dòng
3. `scripts/sync-plugin-skills.sh`
4. `cargo test --locked --test plugin_payload`

Diff: 1 file mới, 1 file sửa, 2 cây generated. Không đụng src/.

---

# Phase 2: làm gọn src/bootstrap.rs

Giữ bootstrap — nó là thứ duy nhất nói cho agent biết mình đang ở trong room.
Gọn lại theo 2 đòn bẩy, xếp theo giá trị.

## Đo đạc

Ghép cho 1 agent claude hcom-launched có tag: `UNIVERSAL + TAG_NOTICE +
DELIVERY_AUTO + CLAUDE_ONLY` ≈ 3.5 KB ≈ **~890 token/lần inject**.

| block | dòng | ~token | ai trả |
|---|---|---|---|
| UNIVERSAL | 53 | 692 | mọi agent |
| DELIVERY_ADHOC | 24 | 242 | chỉ tool không hcom-launched (fleet hiện tại: gần như không ai) |
| SUBAGENT_BOOTSTRAP | 29 | 325 | mỗi subagent |
| CURSOR/COPILOT_DELIVERY | 7 | 129/153 | cursor, copilot |
| DELIVERY_AUTO | 9 | 95 | hầu hết |
| CLAUDE_ONLY | 7 | 65 | claude |
| RELAY/TAG/HEADLESS/UVX | 1 mỗi | 65/17/21/20 | có điều kiện |

Bên trong UNIVERSAL:

| phần | ~token |
|---|---|
| header (tên, authority, marker) | 82 |
| `## MESSAGES` | 69 |
| `## CAPABILITIES` | 422 |
| `## RULES` | 118 |

CAPABILITIES theo bullet: events 92, spawn 90, send 78, other-commands 41,
transcript 27, list 22, term 22, run 14, bundle 8.

Tần suất thực tế (archive `session-2026-09-21_232003`, 8936 event):
send 339 · events 105 · list 64 · transcript 16 · term 9 · listen 7 ·
r/kill 5 · run 3 · **bundle 0** · spawn ~2.

## Đòn bẩy 1 (lớn hơn nhiều): đừng inject lại full bootstrap mỗi SessionStart

`src/hooks/claude.rs` gọi `get_bootstrap()` ở 3 chỗ, và SessionStart bắn lại
với `source` = startup / resume / fork / compact (`:785`, `:890`,
`handle_compact_recovery:1096`, `:1201` đã có sẵn biến `is_resume`).
→ một phiên dài trả ~890 token × N lần, không phải 1 lần.

Tiền lệ đã có trong chính file: nhánh vanilla của compact (`:1131-1136`) chỉ
phát 2 dòng `[HCOM RECOVERY] ... hcom start --as <name>` thay vì full block.

Đề xuất: dùng lại đúng hình dạng đó cho **resume/fork** (agent replay cả
transcript cũ → bootstrap cũ vẫn nằm trong context, inject lại là trùng 100%):

    [HCOM SESSION] Ban van la '<name>' trong room hcom<, tag X>.
    Cu phap: hcom <cmd> --help --name <name>.

~25 token thay vì ~890. **Giữ full block cho `source=compact`** — sau nén thì
bootstrap cũ mất thật, re-anchor ngắn sẽ đẩy agent đi gọi `--help`
(`hcom events --help` một mình đã 880 token, đắt hơn cả bootstrap).

Plumbing: thêm 1 tham số `source`/`is_resume` vào `get_bootstrap()` (hoặc hàm
`get_reanchor()` riêng), `:1201` đã tính sẵn `is_resume`.

## Đòn bẩy 2: cắt text trong UNIVERSAL (~150-180 token, ~20%)

Xếp theo "agent có đoán sai được không nếu thiếu", không theo tần suất.

Cắt được — tra lại bằng `--help` là ra, agent không làm sai âm thầm:

| chỗ | làm gì | −tok |
|---|---|---|
| `bundle prepare` | xoá, 0 lượt dùng | 8 |
| `list [-v] [--json] [--names] [--format ...]` | còn `list [-v] [--json] [name]` | ~12 |
| `transcript` ranges/flags | còn `transcript [name] [--last N] \| search 'text'` | ~12 |
| events filter parentheticals `(contains, ^prefix, =exact)`, `(*.py for glob...)` | xoá giải thích, GIỮ tên filter và `--sql EXPR` | ~35 |
| `Other commands:` prose | rút còn tên lệnh | ~20 |
| RULES #3 (`Use --intent on sends: ...`) | xoá — trùng nguyên văn `## MESSAGES` + flag list ở bullet Message | ~20 |

KHÔNG cắt — đây là failure-mode, thiếu là hỏng im lặng chứ không phải gọi `--help`:
- `--name` bắt buộc (lý do ở comment `bootstrap.rs:18-33`: thiếu thì identity
  thành None và lỗi ra placeholder `<name>` agent không điền được)
- semantics intent (request/inform/ack) và authority
- "end your turn to receive" (comment `:113-120` giải thích: thiếu là agent đi `sleep`)
- bullet `Message` (60% lưu lượng) và `Spawn agents` nguyên vẹn
- `events --sql EXPR` — không đoán ra được

## Đòn bẩy 3 (nhỏ, nhưng đúng pattern file đang có)

`- Run workflows: run <script>` in vô điều kiện trong khi `{scripts}` render
rỗng khi không có script nào. Gate cả bullet giống cách `TAG_NOTICE` được gate
ở `:461`. −14 token cho host không có script.

`SUBAGENT_BOOTSTRAP` (325 tok/subagent) lặp lại response rules + ví dụ `send`
đầy đủ mà parent đã mang. Rút được ~100 token; để sau, mức dùng thấp hơn.

## Thứ tự

1. Đòn bẩy 1 trước (lợi lớn nhất, đụng 1 hàm + call site claude.rs)
2. Đòn bẩy 2 + 3 sau, cùng 1 commit
3. `cargo test --locked` — `src/bootstrap_tests.rs` (614 dòng, 32 test) sẽ đỏ theo, cập nhật kèm

Không đụng Phase 1; hai phase độc lập.

---

# Quyết định: làm gì trước

| việc | lợi | rủi ro | công |
|---|---|---|---|
| P2 đòn bẩy 1 — bỏ re-inject khi resume/fork | ~865 token mỗi lần resume | trung bình, còn 1 điều chưa chắc | 1 hàm + call site claude.rs |
| P1 — gọn SKILL.md 261→125 | 0 token/agent; chỉ lợi khi skill trigger | thấp | 2 file + sync + test payload |
| P2 đòn bẩy 2 — cắt chữ UNIVERSAL | ~170 token mỗi lần inject | thấp | rải rác, 32 test trong `bootstrap_tests.rs` đỏ theo |
| P2 đòn bẩy 3 — gate bullet `Run workflows` | 14 token | không | 3 dòng |

Chọn **P2 đòn bẩy 1**. Nó cắt bội số; ba việc kia cắt hằng số.

## Bằng chứng cho tiền đề của đòn bẩy 1

Bootstrap nằm trong transcript dưới dạng `attachment`, không phải chỉ trong
context runtime. Xem `~/.claude/projects/<proj>/<session>.jsonl` dòng 5:
`"attachment":{"type":"hook_success","hookName":"SessionStart:startup",...}`
chứa nguyên khối `<hcom_system_context>`. Resume đọc lại chính file đó.

## Review của review-dino (codex) — 2026-09-22

### Đã đo, nhưng đo sai tầng

Đếm khối `<hcom_system_context>` trong transcript `.jsonl` cũ dưới
`~/.claude/projects/` (36 file có bootstrap):

| transcript | số khối | ~token | TB/khối |
|---|---|---|---|
| `3c3f7f91` | 9 | 8261 | 917 |
| `bff86235` | 6 | 5419 | 903 |
| `5983d1d7` | 4 | 3862 | 965 |

Mọi `SessionStart` ghi thêm 1 khối, kể cả `source=clear` và `compact`.
~917 token/lần inject — khớp ước lượng.

Nhưng số này **chỉ chứng minh persistence trong file, không chứng minh replay
vào context**. JSONL không đồng nhất với request thực gửi model.

### Tiền đề của đòn bẩy 1 sập theo cả hai hướng

Sửa lại chỗ tôi viết sai trước đó ("nếu không replay thì vẫn đủ để đòn bẩy 1
thắng"). Không phải:

- **Nếu attachment cũ replay** → inject lại là trùng → đòn bẩy 1 đúng.
- **Nếu không replay** → resume không còn bootstrap trong context → inject lại
  là **bắt buộc**, không phải lãng phí → đòn bẩy 1 chết hẳn.

Docs chính thức (code.claude.com/docs/en/hooks#add-context-for-claude) chỉ cam
kết replay saved context cho hook giữa phiên, không cam kết cho attachment
`SessionStart` cũ. Chưa có cơ sở kết luận.

Phép grep 3 bước cũ không phân biệt được hai giả thuyết → **bỏ, không dùng làm
cổng triển khai**.

### Phép đo đúng (spike riêng, trước khi code đòn bẩy 1)

1. Pin phiên bản CLI.
2. Hook startup phát nonce A **chỉ trong `additionalContext`**, tuyệt đối không
   echo A ra turn nào. Hook resume phát nonce B.
3. Capture request thực gửi model (messages/system sau khi load và chuyển đổi
   attachment), kiểm tra A và B ở tầng đó.
4. Không capture được thì probe model đọc nonce từ context, cấm tool/file read,
   không để lộ nonce trong câu hỏi — đây là bằng chứng hành vi, yếu hơn capture.
5. Chạy riêng 3 đường: `resume`, `fork`, `compact→resume`. Một happy path không
   đủ xác nhận `fork`.
6. Không kill agent đang trên roster để thử; dùng fixture tự thoát.

### Nếu sau này đòn bẩy 1 được duyệt, scope phải rộng hơn bản nháp

- Phải cover **cả hai nhánh**: `bootstrap_for_existing_owner` (`claude.rs:1057`,
  vào từ `:995`) và `bind_and_bootstrap` (`:1148`). Không chỉ thêm tham số ở `:1203`.
- `is_resume` (`:1201`) chỉ là `instance.name_announced != 0` — không phải
  `source` trong payload, và không phải bằng chứng bootstrap còn trong context.
- Fork có thể đổi identity; text cũ mang tên/`--name`/marker của cha.
- Bootstrap còn chứa dữ kiện **động**: NOTES, `active_instances`, `scripts`,
  headless, alternate command (`bootstrap.rs:432-540`). Re-anchor 2 dòng không
  refresh được chúng. Tối thiểu: fast path chỉ cho same-identity, full fallback
  cho fork / identity mới / không rõ.
- Nhánh vanilla compact (`:1131`) **không phải tiền lệ bỏ bootstrap** — nó reset
  `name_announced` và bắt chạy `hcom start` để rebind. Tôi trích sai chỗ này.
- ~917 × N là token text được inject, chưa phải billing thực (cache, lượt sau).

### Chốt lại sau review

| việc | trạng thái |
|---|---|
| P1 gọn SKILL.md | làm |
| P2 đòn bẩy 2 (cắt chữ UNIVERSAL) | làm, có sửa (xem dưới) |
| P2 đòn bẩy 3 (gate bullet `Run workflows`) | **bỏ** — YAGNI. `get_scripts` (`:297`) luôn nạp `crate::scripts::SCRIPTS`, hiện có 3 script bundled, nên `{scripts}` không bao giờ rỗng. Lợi = 0 |
| P2 đòn bẩy 1 | **hoãn** — spike đo replay trước |

### Sửa danh sách cắt theo review

- **RULES #3 không xoá.** `## MESSAGES` quy định *người nhận* làm gì; RULES #3
  quy định *người gửi* luôn gắn `--intent`. Bullet send ghi `[--intent ...]` là
  optional và `src/commands/send.rs:85,757` chấp nhận thiếu → xoá là silent
  behavioral regression. Rút gọn thành một dòng:
  `Always set --intent: request=reply needed, inform=FYI, ack=receipt.`
- **Events filter parentheticals**: cắt được, nhưng không phải vì "fail loud".
  Pattern sai trả 0 kết quả im lặng. Cắt được là nhờ dòng
  "If unsure about syntax, always run `hcom <command> --help` FIRST" ngay bên dưới.
  Giữ tên filter, quy tắc OR/AND, và `--sql`.
- Phần còn lại giữ nguyên: bỏ `bundle`, bớt flag `list`/`transcript`, rút prose
  `Other commands`.

### Sửa Phase 1 theo review

- Chuyển setup/troubleshooting sang reference **phải kèm pointer theo tình
  huống**, nhất là "skill invoked without arguments → đọc setup/troubleshooting".
  Bảng reference trơ tên file làm mất hành vi mặc định đó.
- Xoá hẳn section `intent system` khỏi skill chỉ an toàn với agent đã connect.
  Skill còn phục vụ lúc **chưa** connect → giữ một câu yêu cầu `--intent` ngay
  chỗ hướng dẫn send, không lặp cả section.
- Giữ kết luận chưa tách skill, nhưng đổi lý do: hardcode trong
  `sync-plugin-skills.sh` / `plugin_payload.rs` là chi phí nhỏ, không phải lý do
  kiến trúc. Lý do thật: `references/` đã đủ progressive loading, và chưa có số
  liệu agent áp dụng nhầm mode. Tách khi nào có evidence đó.

## Thứ tự chốt (sau review)

1. P1 + P2 cắt chữ, cùng đợt
2. Spike đo replay (mục "Phép đo đúng")
3. P2 đòn bẩy 1 — chỉ khi spike cho kết quả replay, và scope theo identity/fork
