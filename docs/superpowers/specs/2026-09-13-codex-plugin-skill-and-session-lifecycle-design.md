# Shared plugin skills for Codex/AGY, host customization, and TUI dead-agent reconciliation

**Date:** 2026-09-13

**Related issue:** `docs/issues/2026-09-10-standalone-hcom-skill-vs-plugin.md`

**Status:** Revised after review of both 2026-09-13 commits; spec and plans requested by owner. Skill consolidation is implemented; plugin distribution and lifecycle work remain.

## Review baseline và findings

Review đối chiếu HEAD của hai repo:

- hcom `1ea5bc667b38b3a36483a1bf66fdfee6f9a8b07b`: đã merge interactive/script mechanics vào `skills/hcom-agent-messaging/SKILL.md`; chỉ tham chiếu optional `~/.hcom/HOST.md` cho policy riêng.
- agent-skill `e8965cb51262f2e7db99c55c2c303dfdf3ecf448`: đã xóa `hcom/SKILL.md`, chuyển bootstrap/CHEATSHEET về dotfiles, bỏ pointer skill và giữ HOST.md. Đánh giá theo final diff, không theo các bước trung gian trong commit message.

| Mức | Finding và bằng chứng | Hướng xử lý |
|---|---|---|
| P1 | `plugin/hcom-agy/` không có `skills/`; manifest chỉ mô tả lifecycle hooks. `verify_agy_plugin_installed()` chỉ kiểm tra `hooks/hooks.json`. Bỏ standalone vì “plugin ships it” chưa tạo đường skill thay thế cho AGY. | Bundle canonical skill và toàn bộ references vào AGY artifact; verify skill riêng với hooks. |
| P1 | Codex vẫn đi qua `try_setup_codex_hooks()` trong `src/tool.rs`, không có Codex overlay trong package. Bootstrap mới chỉ đọc HOST.md nên commit agent-skill chưa hoàn thành skill discovery cho Codex. | Hoàn tất shared-package Codex route trước khi tuyên bố migration đủ bốn vendor. |
| P1 | `dotfiles/install.d/hcom.sh` xóa pointer skill dù hook setup có thể thất bại; output setup bị redirect hết. Command action-required có thể bị báo là cài xong nếu chỉ dựa exit 0. | Giữ output, phân biệt installed/action-required/error; retire alias chỉ sau verification và chỉ khi xác định đúng ownership. |
| P2 | Spec cũ yêu cầu repoint `~/.hcom/SKILL.md`, merge skill lần nữa và ghi Herdr/tag/kill policy vào canonical skill. Các việc này trái final diff và yêu cầu owner. | Giữ hcom là mechanism/distribution; agent-skill là customization. Không tạo lại pointer skill. |
| P2 | `mark_dead_instances()` log stopped rồi delete row ngoài transaction, bỏ qua lỗi delete; gọi định kỳ làm tăng khả năng race và event lặp. | Dùng atomic finalization có identity guard trước khi tích hợp cadence. |

Đây là review source và design; chưa đo skill discovery trong một phiên AGY/Codex mới. `agy plugin --help` hiện quảng bá cả `plugin@marketplace`, khác comment cũ “directory only”; không mở rộng install route AGY trong change này dựa riêng help text.

## Tóm tắt quyết định

1. **Không tạo một plugin hcom riêng cho Codex.** Codex dùng lại cùng package `plugin/hcom/` đang phục vụ Claude. Package dùng chung có một lớp tương thích Codex nhỏ để Codex chọn đúng hook manifest của mình.
2. **Cách cài Codex phụ thuộc vào Claude:**
   - nếu `claude --version` chạy được, `hcom` chỉ hướng dẫn người dùng mở Codex và dùng `/import` từ Claude Code;
   - chỉ khi xác định Claude không có trên máy, `hcom` mới cài cùng package hcom trực tiếp bằng Codex plugin CLI;
   - nếu không xác định được Claude có hay không, không tự chuyển sang cài native.
3. **`hcom hooks status` cho Codex theo nguyên tắc giống Cursor:** thấy marketplace, cache hoặc plugin record không đồng nghĩa hook đang chạy. Trạng thái runtime từ `codex app-server hooks/list` mới là bằng chứng chính.
4. **Một nguồn skill, phân phối qua plugin:** `skills/hcom-agent-messaging/SKILL.md` cùng `references/`. Claude/Cursor/Codex dùng `plugin/hcom/`; AGY giữ adapter `plugin/hcom-agy/` và bundle cùng nội dung. Không import hoặc giữ active standalone skill `hcom` bên cạnh skill trong plugin. Customize cho máy này chỉ ở repo `agent-skill`.
5. **Bug agent chết nhưng TUI vẫn hiện:** TUI chạy dead-PID reconciliation theo chu kỳ khoảng một giây trước khi reload dữ liệu. Không thêm daemon và không phụ thuộc event riêng của Herdr.
6. **Không thêm Codex `SessionEnd` trong scope này.** Quan sát “thoát/mở lại TUI thì agent biến mất” khớp với dead PID chỉ được dọn ở lần khởi động `hcom` tiếp theo, không phải thiếu session-end hook.

## Bối cảnh và chẩn đoán

### Codex có thể dùng package tương thích Claude, nhưng hook không thể dùng nguyên trạng

Codex hỗ trợ import cấu hình, plugin, skill và hook từ Claude Code qua lệnh tương tác `/import`. Codex cũng chấp nhận package/manifest tương thích Claude. Tuy nhiên, khả năng đọc cùng package không có nghĩa hai runtime dùng cùng hook command hoặc payload contract.

Package hiện tại có:

- `plugin/hcom/.claude-plugin/plugin.json`;
- `plugin/hcom/hooks/hooks.json` gọi các handler Claude như `hcom sessionstart`, `hcom pre`, `hcom post`, `hcom poll`;
- `plugin/hcom/hooks/hooks-cursor.json` cho Cursor;
- `plugin/hcom/skills` trỏ về skill trong repo.

Trong khi đó, Codex integration hiện dùng các handler riêng:

- `hcom codex-sessionstart`;
- `hcom codex-userpromptsubmit`;
- `hcom codex-pretooluse`;
- `hcom codex-posttooluse`;
- `hcom codex-stop`.

Vì vậy, Codex được phép **dùng lại package**, nhưng phải được điều hướng tới `hooks-codex.json`. Cho Codex chạy thẳng `hooks/hooks.json` của Claude là trạng thái lỗi, không phải compatibility.

Nguồn nền tảng:

- [Import settings and data into Codex](https://developers.openai.com/codex/import)
- [Build plugins](https://developers.openai.com/plugins/build/plugins)

### Vì sao agent chết chỉ biến mất sau khi mở lại TUI

Code hiện tại có hai hành vi liên quan:

- `src/main.rs` gọi `mark_dead_instances()` đúng một lần khi mỗi tiến trình `hcom` khởi động;
- vòng lặp trong `src/tui/app.rs` reload dữ liệu mỗi khoảng 120–350 ms nhưng không gọi dead-PID reconciliation.

Khi agent thoát bình thường và PTY wrapper còn sống đủ lâu, cleanup trong PTY/delivery có thể xóa instance ngay. Khi người dùng đóng tab/pane và terminal giết cả process group, agent và PTY wrapper có thể chết cùng lúc; wrapper không còn cơ hội chạy cleanup. Row trong database vì thế còn nguyên trong TUI đang mở.

Thoát TUI rồi mở lại tạo một tiến trình `hcom` mới, startup reaper chạy và row biến mất. Quan sát của người dùng khớp chính xác với đường này. Đây là bug reconciliation của TUI và áp dụng cho mọi local PID-backed agent, không riêng Codex.

Trạng thái bị giữ có thể là:

- `active` nếu process chết giữa một turn;
- `listening` nếu hook `Stop` đã chạy trước khi process chết;
- một trạng thái live khác nếu process chết ở thời điểm tương ứng.

Fix không được phụ thuộc row đang là `active` hay `listening`; tiêu chí quyết định là PID local thực sự không còn sống.

## Goals

1. Dùng một package hcom chung cho Claude và Codex, không fork thành hai plugin tree.
2. Khi Claude có mặt, hướng người dùng dùng `/import` thay vì cài Codex native.
3. Khi Claude chắc chắn không có, cho phép cài cùng package trực tiếp bằng Codex plugin CLI.
4. Báo trạng thái Codex hook dựa trên hook thực sự được Codex discover, enable và trust.
5. Không xóa legacy Codex hooks chỉ vì tìm thấy plugin/cache.
6. Chỉ expose một hcom skill cho mỗi Codex environment.
7. TUI loại bỏ local instance có PID đã chết trong tối đa khoảng hai giây mà không cần restart TUI.
8. Không xóa instance nếu PID vẫn sống, kể cả khi tab Herdr chỉ bị detach.
9. AGY discover đúng một canonical skill từ plugin và đọc được references sau khi cài, không cần checkout nguồn.
10. Không tạo lại `~/.hcom/SKILL.md`, `~/.hcom/skill` hoặc skill riêng trong agent-skill.

## Non-goals

- Không xây một plugin Codex độc lập hoặc copy toàn bộ `plugin/hcom/` sang cây thứ hai.
- Không tự động điều khiển menu `/import`; đây là luồng tương tác trong một local Codex session.
- Không coi plugin cache, marketplace checkout hoặc import record là bằng chứng hook đang active.
- Không tự trust hook import từ plugin và không bật trust bypass cho hook không thuộc hcom.
- Không thêm `SessionEnd` handler hoặc thay đổi nghĩa của `Stop` trong change này.
- Không thêm daemon/background watcher.
- Không phụ thuộc Herdr pane-close API để đảm bảo correctness.
- Không thay đổi lifecycle của remote hoặc PID-less agents.
- Không xóa standalone skill hay host config trước khi đường canonical đã được verify.

## Kiến trúc package dùng chung

```text
plugin/hcom/
├── .claude-plugin/plugin.json
├── .codex-plugin/plugin.json       # compatibility overlay nhỏ, cùng package
├── .cursor-plugin/plugin.json
├── hooks/
│   ├── hooks.json                  # Claude commands/schema
│   ├── hooks-codex.json            # Codex commands/schema
│   └── hooks-cursor.json           # Cursor commands/schema
└── skills/
    └── hcom-agent-messaging/
```

`plugin/hcom/` vẫn là một package có một version và một nguồn phát hành. `.codex-plugin/plugin.json` không tạo một sản phẩm thứ hai; nhiệm vụ duy nhất của lớp này là khai báo metadata Codex cần và chọn rõ `hooks/hooks-codex.json`.

Overlay cũng khai báo `"skills": "./skills/"`. Package hiện là legacy compatibility layout; không chuyển sang portable root manifest trong scope này. Tài liệu [Package your plugin](https://developers.openai.com/plugins/build/plugins) xác nhận legacy overlay hỗ trợ `skills` và override `hooks`; vẫn cần fixture/runtime test cho CLI mục tiêu, đặc biệt import từ Claude.

### AGY adapter và artifact tự chứa

AGY tiếp tục dùng `plugin/hcom-agy/.claude-plugin/plugin.json` và `hooks/hooks.json` riêng: theo measurement trong `src/hooks/plugin.rs`, AGY và Claude cùng đọc tên hook file mặc định nhưng cần payload khác nhau. Một nguồn skill không có nghĩa ép hai hook adapter dùng chung thư mục.

```text
skills/hcom-agent-messaging/        # nguồn duy nhất: SKILL.md + references/**
plugin/hcom/skills/                # Claude/Cursor/Codex distribution
plugin/hcom-agy/skills/            # AGY distribution, cùng nội dung
```

Khai báo discovery AGY qua `skills/hcom-agent-messaging/SKILL.md` trong package; kiểm chứng loader/validator trước khi thêm field manifest. Không mặc định AGY hiểu field `skills` của Cursor/Codex. Manifest AGY mô tả cả messaging skill và hooks.

`plugin/hcom/skills -> ../../skills` hiện chỉ bảo đảm checkout đọc được. Artifact cài đặt phải có skill tree tự chứa (materialize từ nguồn canonical nếu installer không dereference symlink); không được có link trỏ ra ngoài artifact. Cùng contract áp dụng AGY. Không duy trì hai bản SKILL.md chỉnh tay. Nếu cần staging, staging thuộc hcom packaging và phải được dùng thật bởi install/release routes, không thuộc agent-skill.

Tách ba mức bằng chứng: hooks present, skill payload complete, skill discovered by runtime. `verify_agy_plugin_installed()` có thể giữ nghĩa hook presence cho compatibility, nhưng install/status cần báo riêng thiếu skill và hướng dẫn reinstall; không xóa legacy hoặc báo migration complete dựa riêng bool này. Runtime phải liệt kê đúng một skill và mở được `references/patterns.md`; file tồn tại chưa chứng minh discovery.

Các invariant bắt buộc:

1. Claude tiếp tục load `hooks/hooks.json`.
2. Codex chỉ load `hooks/hooks-codex.json`.
3. Cursor tiếp tục load `hooks/hooks-cursor.json`.
4. Mỗi vendor chỉ thấy một bộ hcom hooks.
5. Package chỉ expose một skill `hcom-agent-messaging`.
6. Skill package được sinh từ một nguồn tác giả duy nhất; generated copy được phép, hai bản sửa tay không được phép.

Symlink `plugin/hcom/skills -> ../../skills` phải được test trong cache/install thật. Nếu Codex materialize chỉ thư mục plugin và làm hỏng symlink ra ngoài package, bước đóng gói phải copy/generate skill từ nguồn canonical vào artifact. Không đổi canonical authoring source vì giới hạn của packager.

## Chính sách `hcom hooks add codex`

### Bước 1: kiểm tra trạng thái hiện có

Trước khi đề xuất bất kỳ install route nào, command phải phân loại runtime hooks theo state machine ở phần sau.

- Nếu bộ Codex hcom hooks đúng, enabled và trusted: báo đã hoạt động, không cài lại.
- Nếu có duplicate, untrusted, disabled hoặc manifest sai: báo đúng trạng thái và hành động sửa tương ứng.
- Chỉ đi tới chọn install route khi chưa có bộ plugin hooks hoạt động.
- Nếu runtime inventory không dùng được: báo unverified và giữ nguyên cài đặt; không tự đoán rằng cần install. Legacy-only được đề xuất migrate, không coi là plugin đã hoàn tất.

### Bước 2: phát hiện Claude theo ba trạng thái

| Kết quả probe | Phân loại | Hành vi |
|---|---|---|
| `claude --version` resolve và exit 0 trong timeout ngắn | `present` | Chỉ hướng dẫn import từ Claude. |
| Không resolve được executable (`NotFound`) | `absent` | Cho phép native Codex install. |
| Có executable nhưng timeout, permission error hoặc exit lỗi không giải thích được | `indeterminate` | Dừng và báo lỗi probe; không native-install. |

Không dùng riêng sự tồn tại của `~/.claude`, cache hoặc file cấu hình làm bằng chứng Claude đang được cài và chạy được.

### Route A — Claude có mặt: hướng dẫn `/import`

Nếu Claude có nhưng plugin hcom chưa được cài/enable trong Claude, in prerequisite:

```text
hcom hooks add claude
```

Sau đó in hướng dẫn ngắn:

```text
1. Mở một local Codex session.
2. Chạy /import.
3. Chọn Claude Code.
4. Import plugin hcom cùng hooks và skill của plugin.
5. Không chọn standalone skill "hcom" nếu nó xuất hiện riêng.
6. Restart Codex, review/trust hooks, rồi chạy hcom hooks status.
```

`hcom hooks add codex` trả về trạng thái **action required**, không in “Added Codex hooks”, vì command chưa quan sát được việc người dùng hoàn tất menu `/import`.

Command exit contract cho Codex add: `0` khi hooks đã verify hoạt động; `2` khi action-required hoặc activation-unverified; `1` khi lỗi. Skill discovery được báo/verify riêng; exit 0 của hooks không phải bằng chứng skill đã được runtime discover. `add all` giữ thông tin từng vendor, lỗi ưu tiên hơn pending trong aggregate exit status.

### Route B — Claude không có: native Codex install

Chỉ trong trạng thái `absent`, hcom dùng Codex plugin CLI để cài **chính package chung**:

```text
codex plugin marketplace add <hcom-source>
codex plugin add hcom@hcom --json
```

Source cụ thể phải theo cùng dev/release source policy của plugin hcom hiện tại. Không tạo marketplace/package thứ hai chỉ cho Codex.

Sau khi CLI thành công, hcom vẫn phải chạy runtime verification. Marketplace/install success mà `hooks/list` chưa thấy đúng handlers chỉ được báo là “plugin installed/discovered; activation unverified”, không được báo hooks active.

### Legacy global hooks và migration

Codex `/import` giữ nguyên cấu hình đang có, nên plugin hooks và `$CODEX_HOME/hooks.json` có thể cùng tồn tại. Điều này có nguy cơ mỗi event chạy hai lần.

Quy tắc migration giống Cursor:

1. Không tự xóa legacy hooks khi mới thấy marketplace/cache/plugin record.
2. Chỉ xác nhận duplicate khi `hooks/list` thấy cả bộ plugin và bộ legacy đang enabled.
3. Hướng dẫn `hcom hooks remove codex --legacy-only` sau khi plugin hooks đã được enable và trust.
4. `--legacy-only` chỉ xóa hcom entries trong global Codex config; không gỡ plugin và không đụng foreign hooks.
5. Full `hcom hooks remove codex` mới gỡ cả Codex-side plugin connection và legacy hcom entries; không ảnh hưởng Claude source plugin.

## Codex hook status state machine

### Nguồn dữ liệu và độ tin cậy

Thứ tự ưu tiên:

1. **Authoritative:** `codex app-server hooks/list` với `command`, `source`, `sourcePath`, `enabled`, `trustStatus`, `currentHash`.
2. **Discovery hint:** Codex marketplace/plugin store/cache và Claude plugin presence.
3. **Runtime hint phụ:** một agent mới có `bindings: hooks` sau turn đầu tiên.

Nguồn 2 và 3 không được nâng thành bằng chứng đầy đủ khi nguồn 1 không dùng được. Nếu `hooks/list` lỗi hoặc timeout, headline phải là `state unverified`, kèm dữ liệu phụ và hành động kiểm tra tiếp theo.

Status classifier phải tách khỏi ownership/trust mutation hiện có. Plugin hook có command hcom không tự động trở thành “hcom-owned user hook” để hcom ghi trust state hoặc bật invocation-wide trust bypass.

### Bộ handler đúng

Một Codex hcom hook set hợp lệ phải khớp các command Codex-specific do integration registry định nghĩa, hiện gồm:

- `hcom codex-sessionstart`;
- `hcom codex-userpromptsubmit`;
- `hcom codex-pretooluse`;
- `hcom codex-posttooluse`;
- `hcom codex-stop`.

Kiểm tra dùng normalized command identity hiện có, không chỉ substring `hcom`.

### Trạng thái hiển thị

| Điều quan sát được | Headline | Hành động |
|---|---|---|
| Đủ handler đúng từ plugin, enabled, `trusted`/`managed` | `installed (plugin hooks active)` | Không cần hành động. |
| Đủ handler đúng từ plugin nhưng untrusted/modified | `installed; hook review required` | Mở Codex và review/trust hooks. |
| Đủ handler đúng nhưng có handler disabled | `installed; hooks disabled` | Enable trong Codex rồi verify lại. |
| Handler plugin đúng và legacy đúng cùng enabled | `duplicate hooks; double-fire risk` | Sau khi plugin trusted, chạy `hcom hooks remove codex --legacy-only`. |
| Chỉ có bộ đúng ở `$CODEX_HOME/hooks.json` | `installed (legacy native hooks)` | Vẫn hoạt động; đề xuất import/migrate theo policy. |
| Có plugin/cache nhưng không có handler tương ứng trong `hooks/list` | `plugin discovered; activation unverified` | Restart/review plugin, rồi status lại. |
| Codex đang thấy handler Claude như `hcom sessionstart`, `hcom pre`, `hcom poll` | `incompatible Claude handlers` | Sửa overlay/manifest; không coi hooks là working. |
| Thiếu một phần handler đúng | `incomplete hook set` | In danh sách missing/extra handlers. |
| Không có hooks và Claude `present` | `not active; import from Claude required` | In hướng dẫn `/import`. |
| Không có hooks và Claude `absent` | `not installed` | In `hcom hooks add codex` để native-install. |
| `hooks/list` không dùng được | `state unverified` | Nêu lỗi và hints; không tuyên bố installed/not installed. |

Nếu origin “imported from Claude” không được Codex giữ dưới dạng metadata đáng tin, status không được đoán route cài. Nó chỉ mô tả source path/type và trạng thái thực tế của handlers.

## Quyết định một canonical skill

Ranh giới ownership sau hai commit:

- Giữ `skills/hcom-agent-messaging/SKILL.md` làm nguồn duy nhất.
- Việc gộp interactive/script mechanics đã hoàn thành ở `1ea5bc6`; chỉ sửa tiếp nếu review phát hiện mâu thuẫn cụ thể.
- Interactive là mode mặc định. Herdr, tag bắt buộc, hạn chế headless/stop/kill trên máy này thuộc `agent-skill/dotfiles/config/hcom/bootstrap/HOST.md`, không hardcode vào reusable skill.
- Script/headless là mode opt-in: có thể dùng `--thread`, `--headless` và tự cleanup agent tạm do script tạo.
- Chuẩn hóa lệnh thành `hcom`, không dùng `uvx hcom` làm mặc định.
- Canonical skill chỉ đọc optional `~/.hcom/HOST.md` nếu có; host không có file này vẫn dùng được plugin.
- Giữ bootstrap/rule file riêng của vendor nếu nó chỉ trỏ tới canonical skill hoặc chứa vendor-specific behavior; nó không được trở thành skill thứ hai có trigger trùng.
- `CHEATSHEET.md` là tài liệu người dùng, không nhét nguyên vào agent skill.

Trong `/import`, chọn plugin hcom và skill của plugin; không chọn standalone `hcom`. Bootstrap của agent-skill chỉ trỏ HOST.md hoặc giữ vendor quirks, không nạp skill bằng checkout path hay `~/.hcom/SKILL.md`. CHEATSHEET vẫn là tài liệu cho người dùng trong agent-skill.

`agent-skill/hcom` đã bị xóa trong commit được review; không tạo lại và không lên task xóa lần nữa. Với máy chưa migrate, chỉ retire alias cũ sau khi vendor đang dùng đã verify canonical skill. Shared alias chỉ được retire sau khi mọi consumer của nó đã verify. Không đụng regular file hoặc symlink không xác định thuộc layout cũ; backup phải nằm ngoài skill discovery roots và có tên duy nhất. Máy đã mất alias thì sửa plugin distribution, không khôi phục skill song song.

Installer agent-skill phải hiển thị output `hcom hooks add`, giữ hướng dẫn import/review/restart và không coi exit success là runtime-ready. Hook activation và skill discovery là hai acceptance riêng. Không chạy installer lên home thật trong unit test hoặc trong lượt review/spec này.

## TUI dead-agent reconciliation

### Thiết kế

Giữ startup reaper hiện có và bổ sung một cadence độc lập trong TUI:

```text
TUI event loop
    ├─ input/RPC/animation polling như hiện tại
    ├─ mỗi ~1 giây: reconcile local dead PIDs
    │       ├─ PID còn sống → no-op
    │       ├─ remote/PID-less/launching/inactive → bỏ qua theo contract hiện có
    │       └─ PID đã chết → snapshot + cleanup idempotent + remove row
    └─ nếu có thay đổi: reload data rồi redraw
```

Cadence dead-PID không gắn với data reload 120–350 ms để tránh kiểm tra process quá dày. Mốc mặc định một giây cho thời gian hiển thị stale tối đa khoảng hai giây, kể cả poll và redraw jitter.

Reaper dùng một DB handle được giữ hoặc một lifecycle abstraction có thể test; không reopen database ở mỗi frame. Cleanup primitive phải dùng chung với startup path để giữ cùng cascade:

- session/process bindings;
- notify endpoints;
- subscriptions;
- stopped snapshot/life event;
- instance row.

Ưu tiên reuse `HcomDb::finalize_instance_stop()` trong `src/db/instances.rs`: primitive hiện có transaction và identity guard `(name, created_at, session_id, agent_id)`. Audit thêm PID/status recheck để không xóa row vừa rebind/live lại cùng identity; giữ toàn bộ snapshot và event schema hiện có. Không chỉ bọc lời gọi reaper cũ vào timer. Error phải log/retry, số row dọn chỉ tăng khi transaction thắng.

### Race và idempotency

PTY wrapper cleanup và TUI reaper có thể cùng phát hiện process kết thúc.

- Bên thắng xóa/finalize instance.
- Bên thua thấy row không còn và trả success/no-op.
- Không ghi hai stopped snapshots có nội dung mâu thuẫn.
- Không biến race bình thường thành lỗi đỏ trong TUI.

### Phân biệt kill và detach

Reconciliation chỉ dựa vào PID death:

- nếu đóng Herdr tab giết Codex/PTY, row được dọn trong cadence;
- nếu Herdr chỉ detach nhưng process vẫn sống, row phải được giữ;
- không suy luận session chết chỉ từ việc UI tab biến mất.

Vì vậy fix độc lập với Herdr và cũng xử lý terminal khác, crash, `kill`, hoặc process group bị đóng.

### Life-event reason

Reason `exit:reboot` hiện tại quá hẹp vì cùng reaper cũng xử lý process chết trong lúc máy vẫn chạy. Implementation đổi semantic thành reason chung `exit:dead_process` và ghi detector (`startup` hoặc `tui`) trong log/snapshot. Tài liệu hoặc test nào dựa vào reason cũ phải được cập nhật trong cùng change.

## Error handling

| Tình huống | Hành vi bắt buộc |
|---|---|
| Claude probe không chắc chắn | Không native-install; in lỗi cụ thể và cách người dùng kiểm tra. |
| `/import` chưa hoàn tất | Trả `action required`, không báo thành công. |
| Plugin tồn tại nhưng hooks/list không thấy handlers | Báo discovery-only/unverified. |
| Plugin hook untrusted hoặc modified | Không tự trust; chỉ dẫn Codex review flow. |
| Claude handlers bị load trong Codex | Báo incompatible; không fallback im lặng sang working. |
| Plugin và legacy cùng active | Báo double-fire; chỉ xóa legacy bằng lệnh explicit. |
| `hooks/list` timeout/lỗi schema | Báo state unverified và giữ config nguyên vẹn. |
| TUI reaper không mở/đọc được DB | Log warning, giữ TUI hoạt động, thử lại ở cadence sau. |
| PID check lỗi/không chắc chắn | Fail safe: giữ instance, không xóa. |
| Instance đã được PTY cleanup | TUI reaper no-op thành công. |
| Remote hoặc PID-less instance | Không áp dead-PID cleanup này. |

## Verification và acceptance criteria

### Plugin/import

1. Claude vẫn load `hooks/hooks.json`; Codex load `hooks-codex.json`; Cursor load `hooks-cursor.json`.
2. Import từ Claude expose đúng một `hcom-agent-messaging` skill.
3. Native Codex install khi không có Claude dùng cùng package và cho kết quả runtime tương đương.
4. Máy có Claude không chạy native Codex install; output chỉ rõ `/import` và prerequisite nếu thiếu Claude hcom plugin.
5. Probe Claude lỗi không bị phân loại nhầm thành absent.
6. Import không làm hcom tự trust foreign/plugin hooks.
7. AGY package và installed copy có canonical SKILL.md cùng tất cả references; runtime discover một skill, hooks vẫn là AGY handlers.
8. Các artifact đọc được khi checkout nguồn không còn accessible; không có external/dangling skill symlink.
9. HOST.md vắng mặt không làm plugin lỗi; có mặt thì bootstrap đọc customization từ agent-skill.
10. Installer không giấu action-required, không xóa alias khi verification fail, không xóa symlink/file người dùng không thuộc migration.

### Hook status

Có fixture/test riêng cho từng row trong state table, tối thiểu:

- active plugin;
- untrusted;
- disabled;
- plugin + legacy duplicate;
- legacy-only;
- cache-only;
- incompatible Claude commands;
- partial/missing handlers;
- hooks/list unavailable.

Output test phải bảo đảm các state không quan sát được không dùng từ khẳng định như “active”, “firing” hoặc “installed” sai ngữ cảnh, giống nguyên tắc đang áp dụng cho Cursor.

### TUI lifecycle

1. Local `active` instance với dead PID biến mất khỏi TUI đang mở mà không restart. (verified: fixture test, `src/tui/db.rs::db_source_reconcile_removes_dead_active_and_listening_rows_via_trait`; also `src/instance_lifecycle.rs::reconcile_dead_instances_removes_only_dead_local_active_or_listening_rows`)
2. Local `listening` instance với dead PID cũng biến mất. (verified: same two tests as (1), which cover the listening row alongside the active row)
3. Live PID không bị xóa qua nhiều cadence. (verified: fixture test, `src/tui/db.rs::db_source_reconcile_keeps_live_pid_row_across_repeated_calls` — calls `reconcile_dead_instances()` through the `DataSource` trait 3 times, row survives every pass)
4. Remote, PID-less, `launching` và `inactive` rows giữ contract hiện có. (verified: full matrix at Task 1's `src/instance_lifecycle.rs::reconcile_dead_instances_removes_only_dead_local_active_or_listening_rows`; a lighter representative pass through the `DataSource` interface at `src/tui/db.rs::db_source_reconcile_skips_remote_and_pidless_rows_via_trait` covers remote + PID-less)
5. PTY cleanup trước TUI reaper và thứ tự ngược lại đều idempotent. (verified: Task 1 unit tests, `src/instance_lifecycle.rs` — identity/liveness-guarded `finalize_instance_stop_guarded` cascade tests; not re-tested through the `DataSource` interface in Task 4, same coverage applies)
6. Reaper failure không làm TUI crash. (verified: Task 3 unit test, `src/tui/app.rs::tick_reconcile_failure_keeps_ui_alive_and_retries_a_full_second_later` — an `Err` from the data source does not panic or force a reload, and retries a full second later)
7. Dead row được loại khỏi visible roster trong mục tiêu tối đa khoảng hai giây. (PARTIALLY verified: fixture test `src/tui/db.rs::db_source_reconcile_dead_pid_row_completes_well_under_budget` times a real `reconcile_dead_instances()` fixture-DB call at well under 500ms — this only demonstrates the DB-layer half of the budget is cheap; it does NOT measure real wall-clock TUI behavior (terminal I/O, the 1s cadence timer, redraw jitter, real process-death latency). The full <=2s budget as observed in a live TUI is UNVERIFIED — no live-terminal test run this session; no owner go-ahead was given to kill a real process, per this plan's Global Constraint)
8. Life event dùng `exit:dead_process` và cho biết detector. (verified: `src/instance_lifecycle.rs::reconcile_dead_instances_removes_only_dead_local_active_or_listening_rows` asserts `reason == "exit:dead_process"` and `detector == "tui"`; also asserted directly through the `DataSource` interface at `src/tui/db.rs::db_source_reconcile_removes_dead_active_and_listening_rows_via_trait`)

Acceptance end-to-end:

1. Mở hcom TUI và một Codex agent. (UNVERIFIED this session — no real terminal/live Codex agent was used; not attempted)
2. Xác nhận cả trường hợp agent đang `active` và `listening`. (UNVERIFIED this session in a live TUI — the equivalent DB-level outcome is verified by fixture tests per criteria 1-2 above, but not observed in a real running TUI)
3. Kill terminal/process group. (UNVERIFIED — no live-process-kill test run this session; no owner go-ahead was given to kill a real process, per this plan's Global Constraint against stopping/killing real agents without explicit go-ahead)
4. Xác nhận PID đã chết. (UNVERIFIED — depends on step 3, not attempted for the same reason)
5. Xác nhận row tự biến mất khỏi TUI mà không thoát/mở lại TUI. (UNVERIFIED in a live TUI — depends on steps 3-4; the underlying reconcile-and-reload mechanism is verified by fixture tests, see criteria 1-2 and 7 above, and Task 3's `tick_reconcile` tests in `src/tui/app.rs`)
6. Lặp lại với tab detach nhưng PID còn sống; row phải còn. (UNVERIFIED in a live TUI for the same reason as steps 3-5; the underlying live-PID-survives behavior is verified by fixture test, see criterion 3 above)

## File touch list cho implementation plan sau này

| File/path | Thay đổi dự kiến |
|---|---|
| `plugin/hcom/.codex-plugin/plugin.json` | Thêm compatibility overlay cho cùng package. |
| `plugin/hcom/hooks/hooks-codex.json` | Khai báo Codex-specific handlers. |
| `plugin/hcom-agy/.claude-plugin/plugin.json`, `plugin/hcom-agy/skills/` | Khai báo/distribute canonical skill qua AGY adapter, verify loader contract. |
| hcom package staging/release code (nếu installer không dereference) | Materialize canonical skills; artifact độc lập checkout, không copy chỉnh tay. |
| `src/hooks/codex.rs` | Runtime inventory/classifier, install route, native fallback, legacy-only removal. |
| `src/hooks/plugin.rs` hoặc module package tương ứng | Reuse hcom source/marketplace và validate shared package. |
| `src/tool.rs` | Mô hình Codex là plugin-capable nhưng có conditional install policy. |
| `src/commands/hooks.rs` | Status wording/state machine và action-required outcome. |
| `skills/hcom-agent-messaging/SKILL.md` | Đã merge; chỉ audit regressions và optional HOST reference. |
| `skills/hcom-agent-messaging/references/*` | Audit script/headless scope; chỉ sửa mâu thuẫn cụ thể còn lại sau commit merge. |
| `src/tui/app.rs` | Thêm dead-PID cadence trước reload/redraw. |
| `src/instance_lifecycle.rs` | Reuse/idempotency và generic dead-process reason/detector. |
| `src/db/instances.rs`, `src/tui/data.rs` | Atomic identity-guarded cleanup và testable local data-source maintenance. |
| `agent-skill: dotfiles/install.d/hcom.sh`, `dotfiles/config/hcom/bootstrap/*` | Giữ host customization; migration có ownership/verification guard, output không che action-required. |
| Hook/plugin/TUI lifecycle tests | Bao phủ state table, install branches và reconciliation race. |
| `docs/issues/2026-09-10-standalone-hcom-skill-vs-plugin.md` | Sau implementation, đánh dấu quyết định một skill đã được thực thi. |

Host paths như `~/.claude`, `~/.codex`, `~/.cursor` và `~/.hcom` là migration targets, không phải test fixtures mặc định. Test phải dùng isolated homes và không mutate cấu hình thật.

## Rollout order

1. Thêm shared-package Codex overlay và `hooks-codex.json`; validate package artifact.
2. Thêm hooks/list classifier và output status trung thực.
3. Thêm conditional add flow: Claude import trước, native fallback chỉ khi absent.
4. Verify plugin path rồi mới hỗ trợ explicit legacy-only cleanup.
5. Bundle canonical skill cho AGY và verify artifact trên cả hai adapter; giữ merge đã có.
6. Verify một skill trên từng vendor rồi mới retire alias cũ còn sót; bootstrap chỉ đọc HOST.md, không repoint skill.
7. Thêm TUI reaper và lifecycle tests.
8. Chạy cross-vendor regression để bảo đảm package chung không đổi hook file của Claude/Cursor.

Không xóa legacy hooks hoặc standalone skill ở đầu rollout.

## Alternatives considered

1. **Tạo plugin Codex riêng.** Loại: nhân đôi package và skill mà không cần thiết.
2. **Cho Codex chạy nguyên `hooks/hooks.json` của Claude.** Loại: handler names và payload contract không tương thích.
3. **Luôn native-install Codex plugin.** Loại: trái policy import từ Claude khi Claude đã có.
4. **Nếu probe Claude lỗi thì coi như absent.** Loại: có thể tạo duplicate install ngoài ý muốn.
5. **Tin marketplace/cache là hooks active.** Loại: lặp lại sai lầm đã biết ở Cursor.
6. **Giữ standalone skill làm fallback song song.** Loại: trigger trùng và rule mâu thuẫn.
7. **Chỉ dựa vào Codex SessionEnd.** Loại khỏi scope: không giải quyết crash/process-group kill khi hook không thể chạy.
8. **Bắt event đóng pane của Herdr.** Không chọn làm correctness path: vendor-specific và không xử lý terminal/process death khác.
9. **Thêm daemon reaper.** Loại: TUI cadence đủ cho vấn đề hiển thị; CLI startup reaper vẫn xử lý khi không có TUI.

## Approval boundary

Owner đã yêu cầu review, cập nhật spec và viết plan trong cùng lượt. Deliverables là tài liệu; không thực hiện code/host migration trong lượt này. Hai workstream có thể triển khai độc lập: [plugin skills và customization](../plans/2026-09-13-plugin-skills-and-host-customization.md), [TUI lifecycle](../plans/2026-09-13-tui-dead-agent-reconciliation.md). Runtime verification chưa chạy phải được ghi rõ, không chuyển thành claim đã pass.
