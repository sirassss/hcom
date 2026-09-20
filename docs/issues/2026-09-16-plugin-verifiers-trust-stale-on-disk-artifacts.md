# Plugin verifier tin vào artifact còn sót trên đĩa, không đọc registry của tool

**Ngày:** 2026-09-16
**Mục đích:** Ghi nhận lý do `hcom hooks add cursor` / `hooks status` báo "already installed"
(resp. "marketplace ready") khi marketplace đã bị gỡ thủ công bên ngoài hcom. Phát hiện phụ
trong lúc làm việc khác; verify phải chạy thẳng CLI của tool mới biết sự thật.

`type: BUG`

**TRẠNG THÁI: FIX MỘT PHẦN — MỞ LẠI** (cập nhật 2026-09-17 sau khi đo thật).
D2–D5 xong. D1 **chưa đóng**, và việc đo phát sinh thêm 2 defect chưa từng ghi (mục 5, 6 dưới):
`hooks add` từ chối cài lại khiến người dùng kẹt cả hai chiều, và `.cache-complete` của Cursor
báo xanh trên một payload skills là symlink gãy. Trên `feat/siras/develop`, **chưa push**.

| Defect | Trạng thái |
|---|---|
| D1 — Cursor false positive, checkout repo khác cũng tính | **CHƯA ĐÓNG.** `marketplace add` trần không còn tính là "đã cài", nhưng lòng tin chỉ dời sang `.cache-complete` — cache sống sót qua `marketplace remove` y hệt checkout (đo 2026-09-17). Xem "Còn treo" mục 2, 5 |
| D2 — Cursor false negative, path `plugin/hcom/...` lỗi thời | **Xong** (`9f5c287`) — đọc plugin cache của Cursor |
| D3 — Cursor có plugin cache đọc được mà verifier không dùng | **Xong** (`9f5c287`) |
| D4 — Claude cache mồ côi sống sót qua `marketplace remove` | **Xong** (`4042519`) — đọc `known_marketplaces.json` + `installed_plugins.json` |
| D5 — 4 manifest khai báo repo upstream | **Xong** (`1b767a5`) **và đã publish** lên `sirassss/hcom-plugin` qua `scripts/sync-plugin-repo.sh` |

Kèm theo: gate của `uninstall_claude_plugin` / `uninstall_cursor_plugin` được tách **rộng hơn**
gate của add/spawn ("còn gì gỡ nấy") — `be2e592`; chuỗi status Cursor mô tả đúng cơ chế cache —
`d858c9d`, `ead2841`.

## Còn treo sau khi fix

> **ĐÃ ĐO 2026-09-17** (diro, trên host này: `cursor-agent 2026.09.10-fd3934a`, `agy 1.2.3`).
> Mục 2 và 3 bên dưới đã có câu trả lời thực nghiệm; mục 1 được xác nhận là hoãn đúng. Phát
> sinh thêm hai defect chưa từng ghi — xem mục 5 và 6.

**1. Scope Cursor verifier theo repo — hoãn đúng, đã xác nhận bằng số.**
Lý do hoãn ban đầu là `repository` trong manifest vô dụng làm discriminator (D5). D5 đã fix và
đã publish, nên rào cản đó hết. Nhưng **cache đã materialize trên máy người dùng vẫn mang
manifest cũ cho tới khi họ reinstall trong `/plugins`** — đo trên host này, cache
`a1511e68…/.cursor-plugin/plugin.json` vẫn ghi `"repository": "https://github.com/aannoo/hcom"`
và `"version": "1.0.0"` trong khi registry đã trỏ `sirassss/hcom-plugin`. Bật scope ngay đúng là
làm user nhảy sang "not installed". **Giữ nguyên quyết định hoãn.** Khi làm: đọc
`<cache>/.cursor-plugin/plugin.json` → `repository`, so với `HCOM_PLUGIN_REPOSITORY_URL`. Đó là
lúc D1 đóng hoàn toàn.

**2. ĐÃ ĐO — `marketplace remove` KHÔNG xoá gì trên đĩa. Khả năng xấu là thật.**
Chụp `find ~/.cursor/plugins -maxdepth 6` trước và sau `cursor-agent plugin marketplace remove
hcom` (exit 0, in `✓ Removed marketplace hcom`): **diff rỗng trên cả 65 path**. Registry mất
entry; checkout `marketplaces/github.com/sirassss/hcom-plugin/4fd2136…` và cache
`cache/hcom/hcom/a1511e68…` còn nguyên. Manh mối "account state" trong
`docs/superpowers/specs/2026-09-03-hcom-hooks-as-plugin-design.md:94` là đúng.

Hệ quả đã reproduce, nguyên văn, 3 lần liên tiếp không đổi:

```
$ hcom hooks remove cursor
note: could not remove Cursor plugin: cursor-agent plugin marketplace remove hcom failed: No marketplace matches "hcom". Available: cursor-public, i-have-adhd, ...
Removed Cursor hooks
```

**CẬP NHẬT 2026-09-20 — ĐÃ ĐÓNG hoàn toàn.** Task 4 (`e99379a`) thay
`cursor_marketplace_checkout_exists` bằng `cursor_registry_lists_hcom()` (đọc registry qua
`cursor-agent plugin marketplace list`), đóng đúng nửa checkout. Nhưng
`cursor_uninstall_should_attempt() = verify_cursor_plugin_installed() || cursor_registry_lists_hcom()`
vẫn OR với verifier đọc **cache** (`.cache-complete` + `hooks-cursor.json` + skill payload) — mà
đoạn đo ở trên đã ghi rõ cache cũng sống sót y hệt checkout. Nên bug tái diễn nguyên văn qua
nhánh cache: reproduce lại 2026-09-20 trên host này, lặp lại **mọi lần** gọi `hcom hooks remove
cursor`, không tự hết. Fix: bỏ nhánh `verify_cursor_plugin_installed()` khỏi OR —
`cursor_uninstall_should_attempt()` giờ chỉ đọc `cursor_registry_lists_hcom()` (hàm này đã tự
fail-open đúng khi CLI lỗi/vắng mặt, nên tinh thần "còn gì gỡ nấy" không mất). Thêm test
`cursor_uninstall_does_not_attempt_when_cache_survives_but_registry_lacks_hcom`
(`src/hooks/plugin.rs`). Verify lại trên máy thật: hết note lỗi.

`cursor_marketplace_checkout_exists` match theo substring `hcom` nên kẹt `true` vĩnh viễn —
trên host này có tới 3 checkout mồ côi (`aannoo/hcom`, `sirassss/hcom`, `sirassss/hcom-plugin`)
trong khi registry chỉ từng khai 1. Gate sinh ra để tránh tiếng ồn, nhưng chính nó tạo tiếng ồn
vĩnh viễn. **Hướng sửa:** gate không được hỏi "checkout có tồn tại không" (không bao giờ sai đi
được) mà phải hỏi registry — `cursor-agent plugin marketplace list`, parse cột URL.

**3. ĐÃ ĐO — AGY *có* registry đọc được. Ghi chú cũ trong doc này SAI.**
Câu "AGY không có registry để đối chiếu… không có tín hiệu tốt hơn để đọc" không đúng trên
`agy 1.2.3`. `agy plugin list` trả JSON:

```json
{"imports":[{"name":"hcom","source":"claude-code","importedAt":"...","components":["skills","hooks"]}]}
```

Đủ `name` + `components` để đối chiếu, đúng thứ `verify_agy_plugin_installed` đang thiếu.

Đường uninstall của AGY thì **sạch**, không dính defect shape của Cursor: `agy plugin uninstall
hcom` xoá cả `~/.gemini/config/plugins/hcom/` lẫn toàn bộ entry registry, verifier lật đúng sang
"not installed". Nhưng defect shape vẫn tồn tại ở chiều ngược lại — dựng artifact mồ côi bằng
cách giải nén lại thư mục **không qua CLI**:

| | kết quả |
|---|---|
| `agy plugin list` | không có `hcom` |
| `hcom hooks status` | `Antigravity: installed (plugin)` ← **false positive** |
| `hcom hooks add antigravity` | `already installed`, no-op ← **kẹt, không cài lại được** |

⇒ Sửa được, và nên sửa. **ĐÃ FIX (2026-09-19, Task 6, `plugin.rs::verify_agy_plugin_installed`).**
Không gọi `agy plugin list` — verifier này chạy trước mỗi lần spawn agent nên không được
`std::process::Command` (ràng buộc ghi ở trên). Thay vào đó đọc thẳng file JSON đứng sau lệnh
đó, `~/.gemini/config/import_manifest.json` (qua `agy_import_manifest()`, `std::fs`, không
subprocess), tìm entry `imports[].name == "hcom"` có `components` chứa `"hooks"`. Điều kiện này
**cộng thêm** vào, không thay thế, check file hook cũ — cả hai phải đúng cùng lúc: entry manifest
mà không còn hook file là mid-import hoặc uninstall-nửa-vời; hook file mà không có entry manifest
đúng là ca mồ côi ở bảng trên. `agy_imported_hcom_source()` (đã có từ trước, cùng file) từng đọc
đúng file này để lấy `source`, sửa này chỉ thêm cùng cách đọc cho `components`.

**4. Minor đã triage "ship as is"** ở final review: mảng `manifests` trong test hardcode 4 path;
test của gate assert helper chứ không assert wiring; `cursor_marketplace_checkout_exists`
hardcode đúng 3 tầng `host/owner/repo` và không kiểm `is_dir()`.

**5. MỚI — verifier tin cache nên `hooks add` từ chối cài lại, cả Cursor lẫn AGY.**
Đây là hệ quả nặng nhất và chưa từng ghi. Sau khi registry bị gỡ ngoài hcom (mục 2, 3), người
dùng **kẹt cả hai chiều**: `hooks remove` in note lỗi rồi bỏ cuộc, `hooks add` báo
`already installed (plugin)` và no-op. Không lệnh hcom nào đưa máy về trạng thái cài được.
Chỉ thoát được bằng `cursor-agent plugin marketplace add <url>` thủ công.

Lưu ý bản fix D1 **không** phủ ca này: nó chỉ ngưng tính "checkout trần" là đã cài, nhưng lại
chuyển lòng tin sang `.cache-complete` — mà cache sống sót qua `remove` y hệt checkout. Lòng tin
đổi chỗ, không đổi bản chất. **D1 chưa đóng.**

**6. MỚI — `.cache-complete` của Cursor nói dối: payload skills là symlink gãy.**
Phát hiện bởi agent Cursor `ruva` (spawn qua hcom để verify), tôi đối chiếu lại trên đĩa:

```
$ ls -la <cache>/a1511e68…/skills
skills -> ../../skills
$ readlink -f <cache>/a1511e68…/skills
/home/alam/.cursor/plugins/cache/hcom/skills      ← không tồn tại
```

`verify_cursor_plugin_installed` chỉ kiểm `.cache-complete` + `hooks/hooks-cursor.json`, không
kiểm skills — nên nó báo "installed" trên một cache entry mà toàn bộ skill payload không resolve
được. Mỉa mai: hằng số `PLUGIN_SKILL_FILES` nằm ngay trong cùng file (`plugin.rs:118`) và doc của
nó cảnh báo đúng cái bẫy này ("checking only `SKILL.md` let an apparently installed plugin fail
as soon as the skill followed one of its bundled references") — verifier Cursor chỉ là chưa dùng.

Lý do chưa ai thấy: `ruva` vẫn load được skill, nhưng từ **cache của Claude**
(`~/.claude/plugins/cache/hcom/hcom/1.0.1/skills/…`), không phải cache Cursor. Trên máy không cài
hcom plugin cho Claude thì agent Cursor sẽ không có skill mà verifier vẫn báo xanh.

Đối chiếu: payload AGY **đủ cả 11 file** trong `PLUGIN_SKILL_FILES`. Lỗi này riêng của Cursor.

**Đã đo và KHÔNG phải vấn đề:** "cache ready ≠ đã enable" — doc của
`verify_cursor_plugin_installed` ghi là chưa đo. Spawn agent Cursor thật → `bindings: hooks, pty`.
Trên host này `.cache-complete` đúng là ứng với plugin đang enable.

## Task 9 — Acceptance thủ công trên host thật (2026-09-20, sau Task 1–8)

Đo trên `cursor-agent 2026.09.18-9a7762b`, `agy` (import_manifest), binary `hcom` build lại từ
HEAD `3925c86` trước khi đo (`cargo build --release`: no-op, đã sạch). Backup trước khi đo:
`~/hcom-task9-backups/{cursor-plugins,gemini-hcom-plugin,import_manifest}-20260920-212223.*`.
Trạng thái cuối giống hệt trạng thái đầu (`hcom hooks status` diff rỗng) — không cần restore
từ backup.

- **A (Claude có, Cursor không đặt lại từ đầu):** xoá `~/.cursor/plugins/cache/hcom` sạch.
  `hooks status` → dòng Cursor đổi đúng sang "currently running Claude's installed hcom plugin —
  no separate Cursor install needed…" (nhánh `claude_covers`, Task 1/8). `hooks add cursor`
  (không `--own`) → in đúng thông báo đó, exit 0, **không** tạo lại cache (short-circuit thật,
  không phải no-op im lặng). Spawn agent Cursor thật (`riko`) → `hcom list -v` báo
  `Bindings: hooks, pty` dù Cursor không có cache riêng nào. **PASS cả 3 vế.**
- **B (cả hai):** `hcom hooks add cursor --own` chỉ đăng ký lại marketplace (`marketplace added`,
  exit 1, in "Finish inside Cursor: run /plugins…") — CLI không tự materialize được cache, đúng
  như Task 10b đã ghi. Người dùng bấm `/plugins → install "hcom"` thủ công → cache materialize
  lại đúng hash `4fd21368ee881fb7265190631ddc1e3668280ce8` (giống Task 10b), `hooks status` →
  "plugin cache ready". Spawn agent Cursor thật (`meme`) → `Bindings: hooks, pty`; transcript báo
  `FILES: HOST.md, SKILL.md` — skill đọc đúng 1 lần, **không duplicate** (đối chiếu M2).
  **PASS — nửa CLI (registry) verify được trực tiếp, nửa materialize verify được qua người dùng.**
- **C (gỡ ngoài hcom — đây là phép thử D1):** `cursor-agent plugin marketplace remove hcom` →
  `✓ Removed marketplace hcom`, exit 0; đĩa **không đổi** (cache `4fd21368…` còn nguyên, đối
  chiếu M3). **`hcom hooks status` NGAY SAU ĐÓ vẫn in "plugin cache ready" / "cursor: plugin cache
  shows an install…"** — false positive M3 mô tả vẫn tái hiện y hệt trên code Task 1–8, vì
  `cursor_status_line` (`hooks.rs:159`) chỉ khớp trên `(installed, legacy)` từ
  `verify_cursor_plugin_installed`, không bao giờ hỏi registry — chỉ `plugin_add_can_short_circuit`
  mới dùng `cursor_registry_confirms` (`hooks.rs:270-292`, D1-một-phần). `timeout 60 hcom hooks
  remove cursor` → "Removed Cursor hooks", exit 0, **không lặp vô hạn** (M4/cbcd5a6 đã fix, không
  regress), đĩa vẫn không đổi (hcom không ghi vào `~/.cursor/`, đúng quyết định Task 11).
  `hcom hooks add cursor --own` sau đó → đăng ký lại marketplace thành công (`hcom` xuất hiện lại
  trong `cursor-agent plugin marketplace list`), cùng dạng CLI-half như B. **Kết luận: D1 vẫn
  CHƯA ĐÓNG** — không phải vì `hooks add`/`hooks remove` sai (cả hai đều đúng, đã fix), mà vì
  **`hooks status` vẫn có thể nói "cache ready" ngay sau khi registry đã mất entry**, đúng góc
  D1 mà mục 1 ("Còn treo") nói phải chờ discriminator theo `repository` mới đóng triệt để.
- **D (AGY, thư mục mồ côi):** xoá entry `"name": "hcom"` khỏi `imports[]` trong
  `~/.gemini/config/import_manifest.json`, giữ nguyên `~/.gemini/config/plugins/hcom/` (hooks.json,
  plugin.json, hooks/, skills/, .claude-plugin/) — mô phỏng đúng "thư mục mồ côi không qua `agy
  plugin install`". `hooks status` → "Antigravity: not installed", dòng chi tiết "hooks not
  installed. Run: hcom hooks add antigravity" — **đúng, không bị file mồ côi đánh lừa** (Task 6:
  cần cả entry manifest **và** file, thiếu một là false). `hcom hooks add antigravity` → "Added
  Antigravity hooks (plugin)", exit 0, entry `hcom` xuất hiện lại trong `imports[]`, `hooks status`
  → "installed". **PASS cả 2 vế.**

**Kết luận Task 9:** A, B, D không phát sinh phát hiện mới — xác nhận Task 1, 6, 7, 8, 11 đúng
như thiết kế trên host thật, không chỉ trên fixture. C xác nhận lại nguyên trạng của D1 (không
phải regression, không phải fix mới che lấp) — **D1 vẫn mở**, đóng đúng như mục 1 đã ghi: cần
`repository` trong cache so với `HCOM_PLUGIN_REPOSITORY_URL` làm discriminator ở đường
`hooks status`, không chỉ ở đường `hooks add`.

---

## Nguồn gốc

Ba verifier plugin đều trả lời "đã cài" bằng cách **tìm một file/thư mục trên đĩa**, chứ không
đọc sổ đăng ký (registry) mà tool thật sự dùng:

| Tool | Hàm | Bằng chứng nó chấp nhận |
|---|---|---|
| Claude | `plugin.rs::verify_claude_plugin_installed` | `settings.json → enabledPlugins["hcom@hcom"] == true` **và** `~/.claude/plugins/cache/hcom/hcom/` là thư mục |
| Cursor | `plugin.rs::verify_cursor_plugin_installed` | tồn tại **bất kỳ** `plugins/marketplaces/*/*/*/<sha>/plugin/hcom/hooks/hooks-cursor.json` |
| AGY | `plugin.rs::verify_agy_plugin_installed` | tồn tại `<agy config>/plugins/hcom/hooks/hooks.json` |

Cả ba nhánh này không có cách nào phát hiện "marketplace đã bị gỡ" vì marketplace registry
(`known_marketplaces.json` của Claude, danh sách marketplace của Cursor) **không nằm trong
điều kiện kiểm tra**.

Ràng buộc quan trọng: doc của `verify_cursor_plugin_installed` nói rõ verifier chạy **trước mọi
lần spawn agent**, nên không được `std::process::Command` sang CLI của tool. Fix bắt buộc phải
là đọc file.

## Năm defect cụ thể (đo trên máy này, 2026-09-16)

### D1 — Cursor: false positive, checkout của repo *khác* cũng tính

`verify_cursor_plugin_installed` duyệt mọi `host/owner/repo/sha` dưới
`~/.cursor/plugins/marketplaces/`, không so với `HCOM_PLUGIN_REPOSITORY_URL`. Trên máy này có
ba checkout hcom:

```
github.com/sirassss/hcom/60dc686…/plugin/hcom/hooks/hooks-cursor.json   ← CÓ (repo cũ, đã bỏ)
github.com/sirassss/hcom-plugin/4fd2136…                                 ← repo hiện tại
github.com/aannoo/hcom/79ebde1…                                          ← upstream
```

Chỉ checkout **cũ** thoả điều kiện. Gỡ marketplace hiện tại không đổi kết quả — verifier vẫn
true. Đây đúng là triệu chứng được báo cáo.

**Đo thêm 2026-09-16 — danh tính repo không đọc được từ đĩa, VÌ METADATA CỦA CHÍNH REPO NÀY SAI.** Cả ba repo đều đặt tên
marketplace là `hcom`, nên cache của Cursor (`cache/hcom/hcom/…`) trùng đường dẫn bất kể
nguồn. Và manifest của plugin trong **chính fork** vẫn ghi upstream:

```
# checkout fork hiện tại: sirassss/hcom-plugin/4fd21368…/hcom/.cursor-plugin/plugin.json
{ "version": "1.0.1", "repository": "https://github.com/aannoo/hcom" }
# cache Cursor đã materialize: cache/hcom/hcom/a1511e68…/.cursor-plugin/plugin.json
{ "version": "1.0.0", "repository": "https://github.com/aannoo/hcom" }
```

`repository` giống nhau ⇒ không phân biệt được fork với upstream. Nhưng đây **không phải
giới hạn của Cursor** — xem D5: chính repo này đang ship metadata sai. Sửa D5 xong thì
`repository` trở thành discriminator thật và D1 gỡ được triệt để.

*Không suy ra được:* cache Cursor trên máy này là **1.0.0** còn fork ship **1.0.1**, nên nó
là một bản cũ — nhưng **không kết luận được nó đến từ upstream hay từ fork**, vì cả hai
nguồn đều ghi `aannoo`. Chỉ biết là cũ.

### D2 — Cursor: false negative, path cứng đã lỗi thời sau `e9ea02b`

Verifier tìm `<sha>/plugin/hcom/hooks/hooks-cursor.json`. Repo plugin chuyên dụng
(`sirassss/hcom-plugin`, commit `e9ea02b` "consolidate … onto one dedicated hcom-plugin repo")
có layout `<sha>/hcom/hooks/hooks-cursor.json` — **không có** thư mục `plugin/`:

```
4fd21368…/hcom/hooks/hooks-cursor.json   ← thật
4fd21368…/hcom/.cursor-plugin/plugin.json
4fd21368…/.claude-plugin/marketplace.json
```

`.claude-plugin/marketplace.json` trong checkout nói rõ điều đó — không cần đoán path:

```json
{ "name": "hcom", "plugins": [ { "name": "hcom", "source": "./hcom" } ] }
```

Trên một máy sạch, `hcom hooks add cursor` **thành công** rồi vẫn verify ra false. Hôm nay chỉ
"hoạt động" nhờ checkout cũ ở D1 che lấp. Module doc `src/hooks/plugin.rs:62` cũng còn ghi
`"source": "./plugin/hcom"` — lỗi thời cùng lý do.

**D1 và D2 kéo ngược chiều nhau: sửa mỗi D1 sẽ biến verifier thành false trên máy sạch.
Phải sửa cùng lúc.**

### D3 — Cursor: có sẵn tín hiệu tốt hơn trên đĩa mà verifier không đọc

Cursor có cả một **plugin cache** song song với Claude, hcom chưa hề đụng tới:

```
~/.cursor/plugins/cache/hcom/hcom/a1511e68…/
  .cache-complete            ← marker
  .cursor-plugin/plugin.json
  hooks/hooks-cursor.json
  hooks/hooks.json
  skills/
```

Cấu trúc `cache/<marketplace>/<plugin>/<id>/` giống hệt Claude. Đây là **plugin đã được
materialize**, khác hẳn `marketplaces/…/<sha>/` — thứ mà `marketplace add` tạo ra ngay cả khi
người dùng chưa mở `/plugins`. Tín hiệu này mạnh hơn hẳn cái verifier đang dùng.

Phạm vi nó sửa được: **D2 triệt để** (cache dùng layout `hooks/hooks-cursor.json`, không có
tiền tố `plugin/`) và **D1 một phần** (một lần `marketplace add` trần không còn được tính là
"đã cài"; chỉ một lần materialize thật mới tính). Không sửa được phần còn lại của D1 —
xem đoạn "danh tính repo không đọc được từ đĩa" ở D1.

Hệ quả: module doc `plugin.rs:531-551` khẳng định "Cursor's enabled marker is not readable from
disk — Task 1 measured it as unavailable" — **đo lại 2026-09-16 thì không đúng hẳn**: cache
materialize đọc được. Chưa đo được: cache này do `/plugins install` tạo hay do
`marketplace add` tạo (không có máy sạch để thử), và nó có biến mất khi plugin bị disable
không. Chừng nào chưa đo được hai điều đó thì **không** được dùng nó để mở khoá việc
strip legacy hooks trong `install_cursor_plugin` — đánh sai cuộc đó là Cursor không còn hook nào.

### D4 — Claude: cache dir sống sót khi marketplace bị gỡ

`~/.claude/plugins/cache/hcom/hcom/1.0.0/.orphaned_at` = `1789492223821` — Claude **đánh dấu**
bản cache mồ côi chứ không xoá. `claude_plugin_dir().is_dir()` vì thế vẫn true sau khi
marketplace biến mất, và `enabledPlugins` là file settings do người dùng sở hữu, `marketplace
remove` không nhất thiết dọn. Hai vế của verifier đều sống sót ⇒ báo "installed".

Registry thật sự có sẵn trên đĩa và hcom chưa hề đọc:
- `~/.claude/plugins/known_marketplaces.json` → khoá `"hcom"`
- `~/.claude/plugins/installed_plugins.json` → `plugins["hcom@hcom"][].installPath` (+ version)

**Trạng thái đã đo:** cache mồ côi có thật (`.orphaned_at`). **Chưa đo:** `claude plugin
marketplace remove hcom` cụ thể để lại gì — không chạy trên HOME thật. Defect ở phía code
chứng minh được bằng fixture (`plugin_test_env()`, `plugin.rs:1177`).

### D5 — Manifest plugin trong repo này vẫn khai báo repo upstream

`e9ea02b` chuyển plugin sang repo riêng `sirassss/hcom-plugin` và đã cập nhật **marketplace
descriptor** cho đúng (`plugin/.claude-plugin/marketplace.json`: `owner.name = "sirassss"`,
`homepage = "https://github.com/sirassss/hcom-plugin"`), nhưng **bỏ sót 4 file plugin manifest**:

```
plugin/hcom/.cursor-plugin/plugin.json      author aannoo, homepage/repository aannoo/hcom
plugin/hcom/.claude-plugin/plugin.json      ″
plugin/hcom/.codex-plugin/plugin.json       ″
plugin/hcom-agy/.claude-plugin/plugin.json  ″
```

`scripts/sync-plugin-repo.sh` copy nguyên `plugin/hcom/.` sang `hcom/` của repo đã publish, nên
`sirassss/hcom-plugin` đang quảng cáo chính nó là `aannoo/hcom`. Đây là metadata kế thừa từ
lúc fork, chưa ai sửa — không phải dấu hiệu ai đó cài bản upstream.

Hệ quả:
- `repository` vô dụng làm discriminator (chặn D1, xem trên).
- Người dùng mở plugin trong Cursor/Claude thấy link trỏ về upstream, báo bug sai chỗ.

Phụ: `marketplace.json` ghi `"version": "1.0.0"` trong khi cả 4 `plugin.json` ghi `1.0.1` — lệch
phiên bản giữa descriptor và plugin, cũng nên dọn cùng lúc.

Giữ nguyên `"author": { "name": "aannoo" }` và `"license": "MIT"` — đó là ghi công tác giả gốc,
đúng. Chỉ `homepage`/`repository` (nơi code này thực sự sống) là sai.

## Codex thì sao?

**Không cùng root cause.** `codex_plugin_status` hỏi inventory sống của Codex qua
`codex hooks/list` (RPC), và `fetch_codex_hook_list` lỗi → `Unverified` → `ActionRequired`,
không bao giờ ra "already installed". `cmd_hooks_add` cũng short-circuit Codex **trước** khi
chạm `verify_hooks_installed`. Ghi chú gốc gộp codex chung với cursor, nhưng đo lại thì
`hcom hooks status` cho codex trả lời từ inventory thật:

```
codex:  installed (plugin hooks active)
Cursor:  marketplace ready (plugin)     ← cái này mới sai
```

Codex vẫn có đường `Tool::Codex => verify_codex_hooks_installed(...)` trong `tool.rs:113`, nhưng
đó là check **file hook legacy**, do `commands/start.rs:687` và `commands/status.rs:106` gọi —
ngoài phạm vi bug này.

## Ai chịu ảnh hưởng khi siết verifier

`grep -rn verify_.*_plugin_installed src/` — không chỉ `hooks add`:

- `launcher.rs:652/792/798` — chạy **mỗi lần spawn agent**. Siết verifier ⇒ người dùng đang
  ở trạng thái "cài lỗi nhưng im lặng chạy được" sẽ bắt đầu thấy cảnh báo missing hooks. Đúng,
  nhưng là thay đổi hành vi phải nói rõ trong changelog.
- `plugin.rs:671/690/700` — `remove_*_plugin` gate trên chính verifier này. Verifier chặt hơn
  ⇒ `hooks remove` có thể từ chối gỡ một cài đặt nửa vời. Cần kiểm lại.
- `commands/hooks.rs:369` → `plugin_add_can_short_circuit` — chỗ sinh ra "already installed".
- `codex.rs:1634` — `add_codex_plugin` truyền `verify_claude_plugin_installed()` vào
  `plan_codex_add`. Ảnh hưởng vô hại: chỉ đổi câu chữ khuyến nghị (ví dụ gợi ý `/import` từ
  Claude) khi `hcom hooks add codex` chạy, không chặn cài đặt hay gỡ. Vẫn nên biết trước khi
  siết thêm verifier này, để không bỏ sót đường gọi.

## Cách tái hiện (Cursor, D1+D2)

```
ls ~/.cursor/plugins/marketplaces/github.com/*/*/*/plugin/hcom/hooks/hooks-cursor.json
ls ~/.cursor/plugins/marketplaces/github.com/*/*/*/hcom/hooks/hooks-cursor.json
hcom hooks status | grep -i cursor
```

Checkout duy nhất thoả điều kiện thứ nhất là repo **cũ**; repo hiện tại chỉ thoả điều kiện thứ
hai. `hooks status` vẫn báo "marketplace ready".
