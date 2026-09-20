# Cursor deferring to Claude's plugin install + verifier registry reads — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Khi hcom đã cài cho Claude, Cursor phải được coi là đã được phủ — không cài bản thứ hai, không báo sai, không sinh tiếng ồn khi gỡ. Chỉ khi Claude **chưa** cài thì mới tính đến bản cài riêng cho Cursor. Đồng thời các verifier trả lời "đã cài" dựa trên registry của tool chứ không dựa trên file còn sót trên đĩa.

**Architecture:** `cursor-agent` đọc thẳng plugin cache của Claude, nên "Cursor đã có hook" là một hàm của **trạng thái Claude** chứ không chỉ của trạng thái Cursor. Ta đưa quan hệ đó vào code bằng một khái niệm mới `cursor_hooks_covered()` = `verify_claude_plugin_installed() || verify_cursor_plugin_installed()`, rồi cho add/status/launcher dùng nó thay vì verifier hẹp. Verifier Cursor giữ nguyên nghĩa "Cursor có bản của riêng nó" — cần cho nhánh uninstall và cho ca không có Claude. AGY đổi từ "hỏi một file có tồn tại không" sang đọc `import_manifest.json` — file backing của `agy plugin list`.

**Tech Stack:** Rust 2024, `serde_json`, `cargo test` / `clippy` / `fmt`.

**Spec:** [docs/issues/2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md](../../issues/2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md)

## Đo đạc làm nền (2026-09-17 và 2026-09-19, host này: `cursor-agent 2026.09.10-fd3934a`, `agy 1.2.3`)

| # | Đo | Kết quả |
|---|---|---|
| M1 | Xoá sạch registry + cache + 3 checkout của Cursor, giữ bản Claude, spawn agent Cursor | `bindings: hooks, pty`; skill load từ `~/.claude/plugins/cache/hcom/hcom/1.0.1/…`; `ls ~/.cursor/plugins/cache/` rỗng |
| M2 | Cài cả hai (Claude + Cursor) rồi spawn | **Không duplicate**: skill xuất hiện đúng 1 lần, `deliver:1` / `start:1` y hệt ca chỉ có Claude |
| M3 | `cursor-agent plugin marketplace remove hcom` | exit 0, registry mất entry, **đĩa không đổi 1 byte** (diff rỗng trên 65 path) |
| M4 | `hcom hooks remove cursor` sau M3 | in `note: could not remove Cursor plugin: … No marketplace matches "hcom"`, lặp vô hạn |
| M5 | `hcom hooks add cursor` / `add antigravity` khi registry trống mà artifact còn | `already installed`, no-op → kẹt cả hai chiều |
| M6 | Payload skills trong cache Cursor | `skills -> ../../skills` là **symlink gãy**; verifier vẫn báo installed |
| M7 | `agy plugin list` | có thật, backing file `~/.gemini/config/import_manifest.json` (đọc được bằng `std::fs`) |

M1 + M2 là cơ sở cho toàn bộ hướng đi: Cursor dùng bản Claude được, và cài chồng cũng không hại — nên mặc định phải là **không cài chồng**.

## Global Constraints

- **`verify_*_plugin_installed` cấm gọi subprocess.** Chúng chạy ở `launcher.rs:652/792/798`, tức trước **mỗi** lần spawn agent. Chỉ `std::fs`. Ràng buộc này đã ghi trong doc của `verify_cursor_plugin_installed`. Hệ quả: registry marketplace của Cursor (account state, không có file) **không đọc được** trong verifier — đó chính là lý do phải mượn trạng thái Claude.
- **Đường uninstall thì được gọi CLI** (`uninstall_cursor_plugin` đã dùng `run_tool_cli`). Ràng buộc trên chỉ áp cho đường verify.
- **Không thêm dependency mới. Không thêm migration DB.**
- Không được làm hồi quy test `hooks_missing_warning_cursor_does_not_claim_not_installed` (`launcher_tests.rs:1549`) — nó cấm nói "not installed" cho Cursor, và vẫn đúng.

---

## Task 1 — `cursor_hooks_covered()`: Cursor mượn bản cài của Claude

**Vì sao:** M1. `cursor-agent` chạy hook hcom với **zero** artifact phía Cursor.

- [x] Test đỏ trước: `cursor_is_covered_when_claude_plugin_installed` — Claude installed + Cursor cache vắng ⇒ `cursor_hooks_covered()` true, trong khi `verify_cursor_plugin_installed()` vẫn false.
- [x] Test: `cursor_not_covered_when_neither_installed`.
- [x] Test: `cursor_covered_by_its_own_cache_without_claude` — giữ được ca không có Claude.
- [x] Thêm `pub(crate) fn cursor_hooks_covered() -> bool` trong `src/hooks/plugin.rs`, = `verify_claude_plugin_installed() || verify_cursor_plugin_installed()`. Doc phải dẫn M1 và nói rõ nó **không** thay `verify_cursor_plugin_installed` (nhánh uninstall vẫn cần nghĩa hẹp).
- [x] `cargo test`, `clippy -D warnings`, `fmt`.

## Task 2 — `hooks add cursor` không cài chồng khi Claude đã phủ

**Vì sao:** M2 cho thấy cài chồng không sinh duplicate, nhưng nó tạo ra đúng mớ artifact thừa gây ra M3–M6. Mặc định phải là không cài.

- [x] Test đỏ: `add_cursor_short_circuits_when_claude_covers` — Claude installed ⇒ `hcom hooks add cursor` **không** gọi CLI nào, thoát 0, in thông điệp giải thích Cursor đang dùng bản của Claude và cách ép cài riêng.
- [x] Test: `add_cursor_still_installs_without_claude`.
- [x] Sửa `plugin_add_can_short_circuit` (`src/commands/hooks.rs`) nhận thêm nhánh cursor-covered.
- [x] Thông điệp phải nêu được: đang dùng bản của Claude; gỡ hcom khỏi Claude sẽ mất hook Cursor; muốn bản riêng thì dùng cờ ép (Task 3).
- [x] `cargo test`, `clippy`, `fmt`.

## Task 3 — Cờ ép cài riêng cho Cursor

**Vì sao:** Task 2 lấy mất khả năng cài riêng; phải trả lại một đường rõ ràng, vì người dùng có thể muốn Cursor độc lập với Claude.

- [x] Test đỏ: `add_cursor_force_installs_despite_claude`.
- [x] Thêm cờ (`--own` hoặc `--force`, chọn theo convention có sẵn trong `hooks.rs`) bỏ qua short-circuit của Task 2.
- [x] Cập nhật help text.
- [x] `cargo test`, `clippy`, `fmt`.

## Task 4 — Gate uninstall của Cursor đọc registry, không đọc checkout

**Vì sao:** M3 + M4. `cursor_marketplace_checkout_exists` match substring trên thư mục không bao giờ bị xoá ⇒ kẹt `true` vĩnh viễn ⇒ tiếng ồn vô hạn. Host này có 3 checkout mồ côi trong khi registry chỉ từng khai 1.

- [x] Test đỏ: `cursor_uninstall_does_not_attempt_when_registry_lacks_hcom`.
- [x] Thay `cursor_marketplace_checkout_exists` bằng một hàm chạy `cursor-agent plugin marketplace list` và parse cột URL (được phép: đây là đường uninstall, không phải verify). Fail-open nếu CLI lỗi/không có — giữ nguyên tinh thần "còn gì gỡ nấy", nhưng không được lấy sự tồn tại của thư mục làm bằng chứng.
- [x] Xoá `cursor_marketplace_checkout_exists` và test của nó.
- [x] Kiểm tay: sau `cursor-agent plugin marketplace remove hcom`, `hcom hooks remove cursor` **không** còn in `note: could not remove…`. **Lần đầu 2026-09-20 vẫn in note, lặp lại mọi lần gọi** — bug thật (M4 tái diễn qua cache thay vì checkout), xem `cursor_uninstall_should_attempt` trong `src/hooks/plugin.rs`. Đã fix (bỏ nhánh `verify_cursor_plugin_installed()` khỏi OR); verify lại trên máy thật: hết note.
- [x] `cargo test`, `clippy`, `fmt`.

## Task 5 — Gỡ bẫy "không cài lại được"

**Vì sao:** M5. Registry bị gỡ ngoài hcom ⇒ `hooks add` báo `already installed` và no-op ⇒ người dùng kẹt, không lệnh hcom nào cứu được.

- [x] Test đỏ: `add_cursor_reinstalls_when_only_a_stale_cache_remains`.
- [x] Test đỏ tương ứng cho AGY: `add_antigravity_reinstalls_over_orphan_dir`.
- [x] Sửa điều kiện short-circuit của add: chỉ short-circuit khi **registry** xác nhận, không khi chỉ có artifact trên đĩa. Với Cursor sau Task 2 phần lớn ca đã do nhánh Claude nuốt; ca còn lại là "không Claude, cache Cursor mồ côi".
- [x] `cargo test`, `clippy`, `fmt`.

## Task 6 — `verify_agy_plugin_installed` đọc `import_manifest.json`

**Vì sao:** M7 + M5. Ghi chú cũ trong issue ("AGY không có registry để đối chiếu") **sai**; registry có, lại còn có file backing nên không phạm ràng buộc cấm subprocess.

- [x] Test đỏ: `agy_verifier_rejects_orphan_dir_absent_from_manifest` — có `hooks/hooks.json` trên đĩa nhưng `import_manifest.json` không có entry `hcom` ⇒ false.
- [x] Test: `agy_verifier_accepts_manifest_entry_with_hooks_component`.
- [x] Sửa `verify_agy_plugin_installed` (`plugin.rs:417`): đọc `~/.gemini/config/import_manifest.json`, tìm `imports[].name == "hcom"` và `components` chứa `"hooks"`, **và** file hook vẫn tồn tại. Cả hai điều kiện, không phải một.
- [x] Sửa ghi chú sai trong `docs/issues/2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md` mục 3 (đã sửa một phần ở `2a503fc`, đối chiếu lại cho khớp code mới).
- [x] `cargo test`, `clippy`, `fmt`.

## Task 7 — Verifier Cursor kiểm payload skills

**Vì sao:** M6. `.cache-complete` báo xanh trên cache có `skills` là symlink gãy. `PLUGIN_SKILL_FILES` (`plugin.rs:118`) đã tồn tại đúng cho cái bẫy này, doc của nó nói thẳng ra, chỉ là verifier Cursor chưa dùng.

- [x] Test đỏ: `cursor_verifier_rejects_cache_with_dangling_skills_symlink`.
- [x] Cho `verify_cursor_plugin_installed` kiểm `PLUGIN_SKILL_FILES` như đường AGY đang làm.
- [x] Lưu ý ca hồi quy: sau thay đổi này cache thật trên host dev (`a1511e68…`) sẽ thành false — đúng ý đồ, và Task 1 giữ cho người dùng vẫn được coi là đã phủ nhờ Claude.
- [x] `cargo test`, `clippy`, `fmt`.

## Task 8 — Viết lại 4 nhánh status line của Cursor

**Vì sao:** Sau Task 1–7, các chuỗi hiện tại (`hooks.rs:136–155`) mô tả sai mô hình mới. Chúng đang bắt người dùng tự đi xác minh bằng cách spawn agent — thứ giờ hcom tự trả lời được qua trạng thái Claude.

- [x] Test: mỗi nhánh có một assert về nội dung, gồm nhánh mới "được Claude phủ".
- [x] Nhánh Claude-covered nói rõ: hook đang chạy từ bản của Claude, không cần cài riêng, và gỡ khỏi Claude sẽ mất.
- [x] Giữ `hooks_missing_warning_cursor_does_not_claim_not_installed` xanh.
- [x] Rà `launcher.rs:558-575` cho khớp.
- [x] `cargo test`, `clippy`, `fmt`.

## Task 9 — Acceptance thủ công trên host thật

**Vì sao:** Bảy task trên đều test bằng fixture; M1–M6 chỉ lộ ra khi chạy CLI thật.

- [x] Kịch bản A (Claude có, Cursor không): xoá artifact Cursor ⇒ `hooks status` nói "được Claude phủ"; `hooks add cursor` short-circuit; spawn agent Cursor ⇒ `bindings: hooks, pty`. **PASS** (2026-09-20).
- [x] Kịch bản B (cả hai): `hooks add cursor --own` ⇒ cài được; spawn ⇒ vẫn không duplicate (đối chiếu M2). **PASS** — CLI chỉ đăng ký marketplace, materialize cache cần bấm `/plugins` trong Cursor (đúng Task 10b); skill đọc đúng 1 lần.
- [x] Kịch bản C (gỡ ngoài hcom): `cursor-agent plugin marketplace remove hcom` ⇒ `hooks remove cursor` im lặng; `hooks add cursor --own` cài lại được. **PASS cho add/remove** (không lặp vô hạn, cbcd5a6 không regress) — nhưng lộ ra `hooks status` vẫn báo "plugin cache ready" ngay sau khi registry mất entry ⇒ **D1 vẫn chưa đóng**, xem issue doc.
- [x] Kịch bản D (AGY): dựng thư mục mồ côi ⇒ `hooks status` báo not installed; `hooks add antigravity` cài lại được. **PASS.**
- [x] Backup `~/.cursor/plugins` và `~/.gemini/config/plugins/hcom` (+ `import_manifest.json`) trước mỗi kịch bản; khôi phục sau — trạng thái cuối giống hệt trạng thái đầu, không cần restore từ backup. Backup ở `~/hcom-task9-backups/*-20260920-212223.*`.
- [x] Cập nhật issue doc với kết quả acceptance ([2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md](../../issues/2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md), mục "Task 9 — Acceptance thủ công"). **D1 KHÔNG đóng** — Kịch bản C tái hiện đúng false positive gốc trên `hooks status` (chỉ `hooks add` có registry check, `hooks status` thì không); đóng D1 vẫn cần fix mục 1 ("Còn treo") — so `repository` trong cache với `HCOM_PLUGIN_REPOSITORY_URL`.

---

## Task 10 — Đo nhánh không-có-Claude (CHẶN Task 11)

**Vì sao:** Toàn bộ M1–M7 đo trên host **đã** cài hcom cho Claude. Nhánh no-Claude là đường dự phòng duy nhất sau Task 1–2, nhưng chưa ai đo nó. Và ẩn số lớn nhất — materialize **mới** từ repo hiện tại có sinh `skills` đúng không — quyết định Task 11 có cần tồn tại hay không.

Repo nguồn đã đúng: `hcom/skills/hcom-agent-messaging/` là thư mục thật (không symlink), manifest mang `sirassss/hcom-plugin`. Symlink gãy `skills -> ../../skills` chỉ nằm trong cache materialize ngày 2026-09-09, theo layout cũ. Nếu materialize mới sạch thì cache trên host dev chỉ là rác layout cũ, và cách sửa đúng là "cài lại" — không cần code mới.

- [x] **10a — phía verify, `HOME` giả (an toàn, làm trước).** XONG 2026-09-19, test `cursor_and_claude_verifier_truth_table` (`plugin.rs`). Bảng chân trị đúng như dự đoán; hàng `(claude=true, cursor=false)` xác nhận khoảng trống: hook Cursor sống nhờ Claude nhưng `verify_cursor_plugin_installed` trả false. Mutation-check đã giết mutant đúng hàng đó.
  - [x] Gốc: `user_home()` tôn trọng `HOME`; dựng cây `~/.claude` + `~/.cursor` sạch trong tempdir, kiểm `cursor_hooks_covered()` / `verify_cursor_plugin_installed()` trả đúng ở cả 4 tổ hợp (Claude có/không × Cursor có/không). Không đụng máy thật.
- [x] **10b — materialize thật (ẩn số chính).** XONG 2026-09-19. Backup `~/.cursor/plugins` → `~/.cursor-plugins-backup-20260919-200327.tar.gz`, xoá cache cũ (mang symlink gãy `a1511e68…`), `hcom hooks add cursor --own` (đăng ký lại marketplace), cài thủ công qua `/plugins` trong Cursor. Cache mới `4fd21368ee881fb7265190631ddc1e3668280ce8`: `skills/` là **thư mục thật** (không symlink), đủ cả 11 file `PLUGIN_SKILL_FILES`. Symlink gãy xác nhận là rác layout cũ (2026-09-09) — materialize hiện tại sạch.
- [ ] **10c — agent thật, không Claude.** Cần gỡ hcom khỏi Claude ⇒ giết hook phiên đang chạy và các agent khác. **Chỉ chạy khi người dùng rảnh tay và đồng ý.** Backup trước. Spawn agent Cursor, kiểm `bindings` và đường dẫn SKILL.md.
- [ ] Ghi kết quả vào issue doc.

**Nếu 10b cho ra `skills` là thư mục thật và đủ 11 file:** symlink gãy là rác layout cũ, Task 11 rút về đúng một dòng status "cài lại trong /plugins", không viết thêm code nào.

## Task 11 — Cursor-owned cache thiếu skill: chỉ đường, không tự ghi

**Vì sao:** Sau Task 7, cache thiếu skill ⇒ verifier false ⇒ no-Claude thành `not installed`, mà `hooks add` chỉ add lại marketplace; materialize vẫn phải người dùng tự làm trong `/plugins`. Phải có thông điệp dẫn đúng đường.

**Quyết định: hướng (a) — hcom in câu lệnh, Cursor tự materialize. KHÔNG chọn (b) ghi thẳng vào cache của Cursor.**

Bốn chỗ (b) vỡ mà (a) không:
1. **Hash dir.** Path chứa `<sha>` do Cursor sinh. Host này từng có nhiều hash cùng lúc — hcom không biết ghi vào cái nào, và đoán đúng hôm nay không đảm bảo đúng ngày mai.
2. **Cursor ghi đè.** Cursor materialize lại là xoá sạch thứ hcom copy vào. (a) bền vì chính Cursor tạo ra.
3. **`.cache-complete` ngoài tầm kiểm soát của hcom.** Chưa đo Cursor validate cache entry theo tiêu chí gì; copy skills vào mà Cursor vẫn coi entry hỏng thì công cốc.
4. **Layout đổi là gãy im lặng.** Chính symlink đang gãy là bằng chứng layout Cursor đã đổi ít nhất một lần.

Đường AGY **không** phải tiền lệ cho (b): `~/.gemini/config/plugins/hcom/` là thư mục hcom **cài** qua `agy plugin install`, hcom là bên ghi hợp lệ. Cache Cursor do Cursor materialize, hcom chưa bao giờ là bên ghi.

- [x] Chờ Task 10b — XONG, materialize mới sạch (xem kết quả 10b ở trên) ⇒ chỉ làm gạch đầu dòng dưới, không thêm code.
- [x] Test: status line khi Cursor-owned cache thiếu skill payload phải nêu đúng `/plugins → install "hcom"`, và nói rõ hook có thể vẫn chạy trong khi skill vắng.
- [x] Không thêm code ghi vào `~/.cursor/`.
- [x] `cargo test`, `clippy`, `fmt`.

## Thứ tự và phụ thuộc

Task 10a chạy được ngay, độc lập — nên làm sớm vì nó rẻ và chặn Task 11.

Task 1 là nền, phải xong trước 2/3/8. Task 4, 5, 6, 7 độc lập nhau, chạy song song được sau Task 1. Task 10b **chặn** Task 11: chưa đo materialize mới thì chưa biết Task 11 cần viết code hay chỉ cần một dòng status. Task 8 cần 1–7 và 11 xong mới viết đúng chuỗi. Task 9 cuối cùng. Task 10c tách riêng, chạy khi người dùng đồng ý.

## Rủi ro

- **Task 7 làm cache thật hoá false.** Nếu Task 1 chưa vào thì user có Claude sẽ thấy "not installed". Cấm merge Task 7 trước Task 1.
- **Task 4 chuyển sang gọi CLI ở đường uninstall.** Nếu `cursor-agent` vắng mặt, phải fail-open chứ không được chặn gỡ.
- **Nhánh no-Claude không có registry đọc bằng file.** Marketplace Cursor là account state, nên verify không thể phát hiện người dùng `marketplace remove` ngoài hcom. Giới hạn cứng, không vá được trong verifier (cấm subprocess). Nhánh Claude không dính vì `installed_plugins.json` là file thật. Ghi nhận, không sửa.
- **Cám dỗ chọn (b) ở Task 11.** Ghi vào cache của tool khác là thêm code để chữa một triệu chứng có thể đã tự hết. Đo Task 10b trước.
- **Scope theo repo vẫn hoãn.** Cache trên máy người dùng còn mang manifest `aannoo/hcom` (đo 2026-09-17). Plan này **không** động vào, và sau Task 1 thì nó gần như hết quan trọng vì Cursor đã mượn bản Claude.
