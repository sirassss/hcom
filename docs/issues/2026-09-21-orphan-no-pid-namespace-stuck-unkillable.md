# Orphan không có `pid_namespace` bị TUI show mãi mãi, nhưng `hcom kill <pid>` không bao giờ xoá được

**Ngày:** 2026-09-21
**Mục đích:** Ghi nhận vì sao TUI liệt kê "5 orphan processes" không mất đi, và vì sao bấm Kill
trên chúng luôn báo "not found" mà entry vẫn còn nguyên trên màn hình sau đó. Người dùng phát
hiện triệu chứng qua screenshot TUI; tôi (Claude) chẩn nguyên nhân bằng đọc code + kiểm tra trực
tiếp file pidtrack và `/proc` trên host.

`type: BUG`

**TRẠNG THÁI: ĐÃ FIX** (2026-09-21, trên `feat/siras/develop`, commit `5334d25`).
Chọn hướng A (kill-by-PID thật thay vì gate qua liveness suy luận), scoped chỉ cho nhánh
numeric-PID trong `kill_single` — không đụng `kill_all`/`kill_by_tag`, không đụng
`mark_dead_instances`/reconciliation tự động (fail-closed principle ở
[[2026-09-17-cross-namespace-liveness-reaps-live-agents]] giữ nguyên, khác code path). Bỏ qua
pane-close trên nhánh mới (rủi ro đóng nhầm pane bị tái sử dụng, xem `terminal::kill_process`
không verify pane còn đúng chủ). Thông điệp không claim "already terminated" (một suy luận) mà
nói đúng sự thật quan sát được: "PID not visible from this namespace; removed from tracking".
Thêm `pidtrack::get_tracked_entry` (read-only, không side-effect, không đua ghi với
`get_orphan_processes`). 2 test mới (`pidtrack::tests::test_unknown_namespace_orphan_excluded_but_still_tracked_by_pid`,
`commands::kill::tests::test_kill_single_removes_unknown_namespace_orphan`); verify thủ công
bằng chính 5 orphan thật trong `~/.hcom/.tmp/launched_pids.json` trên host này — cả 5 đã xoá
sạch. 2493 test pass / 0 fail, clippy + fmt sạch.

Còn lại có chủ đích, không vá trong lần này: `ORPHAN_CACHE`/`ORPHAN_CACHE_TTL` trong
`tui/db.rs:723-748` (TTL 5s) không bị `reload_data_force()` invalidate — sau khi kill thành công
trên đĩa, TUI có thể vẫn hiện entry cũ tối đa 5s trước khi tự cập nhật ở lần poll tiếp theo. Đây
là race thoáng qua (tự phục hồi), không phải bug "vĩnh viễn không xoá được" như triệu chứng gốc,
nên không mở rộng scope fix này để vá nó.

---

## Triệu chứng

TUI hcom hiển thị mục "5 orphan" (PID 569969, 423521, 168984, 833533, 355974) không biến mất
qua nhiều lần reload. Bấm Kill trên một PID trong số đó báo lỗi kiểu "not found" / "Kill PID
failed", và ngay sau đó PID đó vẫn còn trong danh sách orphan — không có cách nào clear khỏi màn
hình qua UI.

## Bằng chứng đo được (2026-09-21, n=1, một host — WSL2 Linux)

Đọc trực tiếp `~/.hcom/.tmp/launched_pids.json`: cả 5 entry orphan đang hiển thị trên TUI **đều
không có field `pid_namespace`** (JSON không có key này):

```json
"569969": { "tool": "claude", "names": ["neva"], ... "tag": "myapp" }
```
(tương tự cho 423521, 168984, 833533, 355974 — không entry nào có `"pid_namespace"`.)

Kiểm tra `/proc` trên host cho cả 5 PID:

```
$ for p in 569969 423521 168984 833533 355974; do ls /proc/$p; done
ls: cannot access '/proc/569969': No such file or directory   (x5, tất cả)
```

Tất cả 5 process đã chết thật — không phải vấn đề "process còn sống mà bị coi nhầm là chết".

## Cơ chế

`PidEntry.pid_namespace` (`src/pidtrack.rs:44`) dùng `#[serde(default)]`, nên entry cũ (ghi
trước khi field này tồn tại) đọc lên thành chuỗi rỗng `""`. Comment tại chỗ khai báo field nói
rõ ý định:

> Empty for entries written before this field existed, or on platforms without PID namespaces;
> such entries read as "liveness unknown" and are never pruned.

`is_alive_in` (`src/sys/process.rs:148-156`):

```rust
pub fn is_alive_in(pid: u32, observed_in: Option<&str>) -> Option<bool> {
    if observed_in != current_pid_namespace() {
        return None;
    }
    Some(is_alive(pid))
}
```

`observed_in = Some("")` (từ entry cũ) gần như chắc chắn khác `current_pid_namespace()` (một
chuỗi dạng `pid:[4026531836]`) → trả `None` ("không xác định") — **vĩnh viễn**, không phụ thuộc
process đó thật ra còn sống hay đã chết từ lâu.

Hai nơi dùng kết quả `None` này theo hai cách khác nhau, và đó là lỗ hổng:

1. **TUI hiển thị** — `load_orphans()` (`src/tui/db.rs:785`):
   ```rust
   if crate::sys::process::is_alive_in(pid, Some(pid_namespace)) == Some(false) {
       continue;   // chỉ loại khi CHẮC CHẮN chết
   }
   ```
   `None` không bị loại → entry cứ hiển thị mãi trên TUI, bất kể process đã chết từ lâu.

2. **`hcom kill <pid>` (đường TUI dùng để kill orphan)** — `KillOrphan` action
   (`src/tui/actions.rs:324`) gọi `RpcOp::KillPid{pid}` → chạy native
   `commands::run_native(&["kill", pid])` (`src/tui/rpc_async.rs:166`) → vào
   `kill_single()` (`src/commands/kill.rs:573`), hàm này tìm PID trong
   `pidtrack::get_orphan_processes(hcom_dir, None)`. Hàm đó (`src/pidtrack.rs:298-313`)
   chỉ đẩy entry vào `result` khi `entry_liveness == Some(true)`; nhánh `None` chỉ được
   `retain` lại trên đĩa (`return true`), **không bao giờ vào `result`**.

   Vì `entry_liveness` của cả 5 PID trên là `None`, chúng không nằm trong list mà
   `kill_single` duyệt qua → `orphans.iter().find(...)` không khớp → rơi xuống
   `bail!("Agent '{}' not found", target)` (`src/commands/kill.rs:621`) — đúng thông điệp
   "not found" người dùng thấy. `pidtrack::remove_pid()` (dòng 611, chỉ chạy trong nhánh
   tìm thấy) **không bao giờ được gọi** → entry ở lại `launched_pids.json` vĩnh viễn.

Tóm lại: TUI cố tình show entry "không rõ sống chết" để tránh giấu nhầm agent còn sống (đúng
nguyên tắc fail-closed đã áp dụng ở [[2026-09-17-cross-namespace-liveness-reaps-live-agents]]) —
nhưng đường kill lại loại bỏ đúng những entry đó khỏi danh sách có thể kill, nên UI show ra một
thứ mà lệnh kill declare là "không tồn tại", và không route nào xoá được nó khỏi pidfile.

## Vì sao chưa lộ ra trước đây

`pid_namespace` là field mới (theo comment tại chỗ khai báo và trace hcom gần đây về
`PidRecord.pid_namespace` / migration liveness cross-namespace). Trước khi field này tồn tại,
mọi entry orphan đều "không rõ namespace" như nhau, và `get_orphan_processes` hẳn dùng logic
khác (không phân biệt `None`/`Some`) nên không có sự lệch giữa hai nơi. Sau khi thêm
`pid_namespace` để fix bug reap-nhầm-agent-sống, các entry legacy (ghi trước migration) bị đẩy
vĩnh viễn vào nhánh "unknown" — và lệch giữa hai implementation orphan-listing (TUI vs kill) mới
lộ ra.

## Ruled out

- **Không phải race giữa lúc TUI liệt kê và lúc bấm Kill** (process chết đúng lúc đó): cả 5 PID
  đã chết hẳn từ trước (nhiều `launched_at` cách nhau hàng giờ/ngày dựa trên timestamp trong
  file), và liveness check trả `None` (không xác định) chứ không phải `Some(true)` rồi đổi
  thành `Some(false)` — không có cửa sổ race ở đây, đây là trạng thái ổn định.
- **Không phải do `pid_namespace` khác host/sandbox thật** — 5 entry này thiếu hẳn field, không
  phải có giá trị namespace khác đang hoạt động (khác với ca ở
  [[2026-09-17-cross-namespace-liveness-reaps-live-agents]], nơi PID đang **sống** trong sandbox
  khác).

## Hướng fix khả dĩ (chưa chốt)

Hai đường hợp lệ với cùng bằng chứng, cần chọn:

- **A. Cho `kill_single`/`get_orphan_processes` chấp nhận kill theo PID số dù liveness
  `None`** — nếu người dùng gõ đúng số PID (không phải match theo tên), họ đang chủ động khẳng
  định "tôi biết PID này", nên có thể bỏ qua gate liveness cho riêng đường match-by-PID và cứ
  gọi `kill(2)` thật; nếu OS trả ESRCH thì coi là AlreadyDead và `remove_pid` luôn (thay vì bail
  "not found" mà không dọn state).
- **B. Thêm hành động "clear"/"dismiss" riêng trong TUI** cho orphan có liveness `None` và tuổi
  entry đã cũ — không gọi `kill(2)` (vì không biết có an toàn không), chỉ `remove_pid` sau khi
  người dùng xác nhận thủ công đó không phải agent sống.

Không tự chọn giữa A/B ở đây — A đơn giản hơn nhưng làm yếu nguyên tắc fail-closed đang bảo vệ
[[2026-09-17-cross-namespace-liveness-reaps-live-agents]] nếu path đó vô tình dùng chung code;
B giữ nguyên tắc đó nhưng cần UI mới. Cần spec riêng nếu chọn hướng.

## Reproduction

1. Có một entry trong `~/.hcom/.tmp/launched_pids.json` thiếu field `pid_namespace` (entry cũ từ
   trước migration, hoặc tự xoá field này khỏi một entry để test), ứng với PID đã chết thật trên
   host.
2. Mở hcom TUI → entry đó xuất hiện trong danh sách "N orphan".
3. Bấm Kill trên entry đó (hoặc chạy `hcom kill <pid>` trực tiếp) → nhận lỗi
   `Agent '<pid>' not found`.
4. Reload TUI (hoặc đợi tick tiếp theo) → entry vẫn còn nguyên trong danh sách orphan.

## Scope note

Không đụng tới nguyên tắc fail-closed liveness (`is_alive_in`) — đó là fix đúng và cần thiết cho
[[2026-09-17-cross-namespace-liveness-reaps-live-agents]]. Issue này chỉ nói về việc hai call
site tiêu thụ kết quả `None` của nó không nhất quán với nhau, khiến state legacy không có đường
thoát nào cả (không hiện được là "chắc chắn sống" để user yên tâm, không xoá được để user dọn
màn hình).
