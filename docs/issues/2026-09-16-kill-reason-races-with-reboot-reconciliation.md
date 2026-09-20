# `hcom kill` reason lost to reboot reconciliation under concurrent `hcom` calls

**Ngày:** 2026-09-16
**Mục đích:** Ghi nhận một race condition thật trong `instance_lifecycle::mark_dead_instances`,
phát hiện khi chạy lại `real_tool_claude` (bản pinned Claude Code 2.1.216) cục bộ sau khi vá
lỗ hổng plugin-install trong test harness (xem commit `f46552d`). Bug này có thể đã tồn tại từ
lâu — CI trước giờ chưa từng chạy tới đoạn code này vì luôn fail sớm hơn ở bước cài hook.

`type: BUG`

**TRẠNG THÁI: ĐÃ FIX** (2026-09-17, trên `feat/siras/develop`, **chưa push**).
Claim `<reason>|<initiated_by>|<created_at>` vào bảng `kv` trước khi gửi tín hiệu; guard đặt ở
`stop_instance_inner` nên phủ mọi caller, cộng 3 điểm trong `kill.rs` nơi tín hiệu đi trước.
Commit `b3bdb4d`, `f3d5305`, `08f018a`, và `b58c4a0` (mila: gộp việc dọn claim vào transaction
của `finalize_instance_stop`, và chặn stopper cũ ghi đè claim của lifetime mới).
Regression thật `real_claude_full_lifecycle_send_fork_kill_resume_and_cleanup` pass 3/3.

Còn nợ có chủ đích: một orphan claim mỗi tên khi row bị xoá thẳng (PTY exit) — xem comment
`ponytail:` tại `src/hooks/common.rs:1402`, có ghi trần và đường nâng cấp.

---

## Triệu chứng đo được

`real_claude_full_lifecycle_send_fork_kill_resume_and_cleanup` panic **deterministic 3/3 lần**
chạy cục bộ, luôn tại `tests/support/real_tool.rs:985`:

```
killed snapshot did not preserve session <uuid> and reason: Stopped: <name>
  Time:       ...
  By:         system
  Reason:     exit:reboot
  ...
  Resume: hcom r <name>
```

Test flow: launch Claude → fork → `hcom kill <parent>` (exit 0) → `hcom list --stopped <parent>`
phải thấy reason `"killed"`. Thay vào đó reason ghi được là `"exit:reboot"` — một reason hoàn
toàn khác, do một cơ chế khác ghi đè.

## Cơ chế

- `src/main.rs:72` gọi `instance_lifecycle::mark_dead_instances(&db)` ở **đầu mọi lệnh `hcom`**,
  trước khi dispatch — kể cả các lệnh đọc thuần như `hcom list`. Hàm này quét bảng `instances`,
  với mỗi PID không còn sống (`crate::pidtrack::is_alive(pid)` == false) thì tự ghi snapshot +
  life event với reason cố định `"exit:reboot"` rồi xoá row (`src/instance_lifecycle.rs:889-963`).
- `hcom kill <name>` (`src/commands/kill.rs::kill_tracked_instance`) làm 2 bước tuần tự trong
  **cùng một tiến trình**: `kill_instance()` gửi SIGTERM thật, rồi `stop_instance(db, name,
  initiator, "killed")` mới ghi snapshot + reason `"killed"` + xoá row.
- Vòng lặp `h.eventually(...)` trong test (và trong sử dụng thực tế: một TUI hoặc lệnh `hcom`
  khác đang chạy song song) gọi `hcom list` lặp lại mỗi ~100ms bằng **tiến trình `hcom` riêng**.
- Nếu OS thật sự giết tiến trình đủ nhanh (SIGTERM → chết trong vài ms), một lệnh `hcom list`
  chạy đồng thời — vẫn còn thấy row instance chưa bị `stop_instance` xoá — có thể tự chạy
  `mark_dead_instances`, thấy PID đã chết, và **thắng race**: ghi `"exit:reboot"` + xoá row trước.
  Khi `kill_tracked_instance`'s `stop_instance()` chạy tới, `db.get_instance_full(name)` trả về
  `None` → `StopOutcome::AlreadyStopped`, reason `"killed"` **không bao giờ được ghi**.

Đây là race giữa hai tiến trình `hcom` độc lập cùng ghi vào SQLite, không phải lỗi trong logic
của riêng `kill` hay riêng `mark_dead_instances` — cả hai đúng khi chạy một mình.

## Vì sao trước giờ chưa lộ ra

CI trên fork này luôn fail sớm hơn (thiếu cài Claude plugin trong test harness — xem
`docs/issues` liên quan tới commit `f46552d`), nên test chưa từng chạy tới đoạn fork/kill/resume
để chạm race này. Sau khi vá lỗ hổng đó, đây là lỗi *tiếp theo* lộ ra — rất có thể đã tồn tại
trong code production từ trước, không liên quan tới bất kỳ commit gần đây nào của repo này.

## Mức độ ảnh hưởng thực tế (chưa đo ngoài test)

Trong dùng thực tế, cửa sổ race rất hẹp (phải có một lệnh `hcom` khác chạy đúng lúc PID chết) —
nhưng không phải chỉ xảy ra trong test: bất kỳ ai chạy TUI hcom hoặc một script poll trong lúc
`hcom kill` một agent khác đều có thể trúng. Hậu quả: reason "resume" hint sai (`exit:reboot`
thay vì `killed`), có thể ảnh hưởng logic downstream nào đang phân biệt hai reason này (chưa rà
soát hết).

## Hướng fix (đã rà xong, 2026-09-16)

### Trước hết: reason này có ai đọc không?

`grep -rn "exit:reboot" src/ tests/` → **đúng một hit**, chính chỗ ghi
(`instance_lifecycle.rs:963`). Chiều ngược lại, `grep -rn '"killed"' src/ tests/` cho hai chỗ
trông như logic nhưng **đọc field khác**:

- `instance_lifecycle.rs:796` — `matches!(context, "killed" | "closed" | ...)` đọc
  `computed.context`, tức cột `status_context` (do `delivery.rs:2730` ghi `"exit:killed"` /
  `"exit:closed"`), không phải reason của life event.
- `tui/db.rs:907/915` — nhánh theo **`action`** (`"stopped" | "killed"`), reason chỉ được nối
  vào chuỗi hiển thị.

Vậy field `reason` của life event thực sự **chỉ để hiển thị** (`hcom list --stopped`, resume
hint, TUI activity). Đây là bug hiển thị sai, không làm hỏng state — và fix đúng chỗ thì rẻ.

### Loại bỏ: "gộp vào một transaction / khoá DB"

Gợi ý cũ sai. Race này là **thứ tự**, không phải tính nguyên tử: `mark_dead_instances` đã
`log_life_event` + `delete_instance` xong *trước khi* `stop_instance` kịp `get_instance_full`.
Làm mỗi bên nguyên tử hơn không đổi gì — kẻ thắng vẫn ghi `exit:reboot`. Muốn hết race thì
phải **chiếm chỗ trạng thái kết thúc trước khi gửi tín hiệu**.

### Phạm vi thật: không chỉ `kill`

`grep -rn "stop_instance(\|stop_placeholder_instance(" src/` — `kill` chỉ là một trong số caller.
Mọi caller truyền reason riêng đều mất nó vào `exit:reboot` qua đúng cửa sổ đó, vì
`mark_dead_instances` chạy ở `main.rs:72` cho **mọi** lần gọi `hcom`:

| Caller | reason |
|---|---|
| `commands/kill.rs:145/367/371/477/481` | `killed` |
| `commands/stop.rs:94/172/231/314` | `stop_all` / `tag_stop` / `multi_stop` / (biến) |
| `hooks/claude.rs:2495/2581` | `idle` / (biến, teardown subagent) |
| `instance_lifecycle.rs:835` | `exit_cleanup` / `stale_cleanup` / `inactive_cleanup` |

⇒ Guard phải nằm ở chỗ mọi caller đi qua (`stop_instance_inner`), không phải vá riêng
`kill.rs`. `kill.rs` vẫn cần thêm một claim **sớm hơn nữa**, vì `kill_instance()` gửi SIGTERM
*trước khi* `stop_instance` được gọi.

### Phương án A (khuyến nghị) — đặt sẵn reason vào `kv` trước SIGTERM

`kill_tracked_instance` (`src/commands/kill.rs:130`) hiện làm:

```rust
let (result, …) = kill_instance(db, name, pid, &inst, is_headless);   // gửi SIGTERM
stop_instance(db, name, initiator, "killed");                          // mới ghi reason
```

Chèn một claim trước tín hiệu, và cho `mark_dead_instances` fallback vào đó:

- `hooks/common.rs::stop_instance_inner`: ngay sau khi đọc được row, **trước** mọi việc gửi
  tín hiệu, ghi `stop_reason:<name>` = `<reason>|<session_id>`. Một chỗ này phủ hết caller
  trong bảng trên.
- `commands/kill.rs::kill_tracked_instance`: ghi cùng key **trước** `kill_instance`, vì tín hiệu
  đi trước `stop_instance` ở đường này.
- `instance_lifecycle.rs:963`: thay literal `"exit:reboot"` bằng
  `db.kv_get(&key).ok().flatten().unwrap_or_else(|| "exit:reboot".into())`.
- Ai ghi xong thì xoá key (`kv_set(key, None)`) — cả `mark_dead_instances` lẫn
  `kill_tracked_instance` sau khi `stop_instance` trả về.

Helper đã có sẵn: `src/db/kv.rs::kv_get` / `kv_set`. Không cần migration, không cần cờ mới.
Bên nào thắng race cũng ghi ra reason đúng.

Rủi ro đã biết: nếu tiến trình `hcom kill` chết giữa hai bước, key `stop_reason:<name>` còn
lại; tên agent là CVCV nên có thể tái sử dụng và một lần stop sau đó bị gán nhầm reason
`killed`. Giảm bằng cách nhét `session_id` vào value và chỉ dùng khi khớp.

### Phương án B (đơn giản hơn, rủi ro hơn) — đảo thứ tự trong `kill_tracked_instance`

`kill_instance` không chạm DB (nhận `_db: &HcomDb`, không dùng), nên về mặt kỹ thuật có thể gọi
`stop_instance` trước rồi mới `kill_instance`. Nhưng:

- `stop_instance` đã xoá row + ghi life event; nếu `kill_instance` sau đó **thất bại** (pane
  không đóng được, tiến trình sống sót) thì agent còn chạy mà hcom không còn theo dõi — tệ hơn
  bug hiện tại.
- Với instance headless, `stop_instance_inner` tự `terminate_group` + poll 2s + `kill_group`
  (`src/hooks/common.rs`), nên gọi trước rồi lại `kill_instance` là làm hai lần.
- Với PTY, `stop_instance` đăng ký tiến trình còn sống vào `pidtrack`; giết nó ngay sau đó để
  lại entry chết.

Chỉ chọn B nếu A vướng gì đó không lường được.

### Không chọn: "chỉ chạy `mark_dead_instances` trên lệnh ghi"

Thu hẹp cửa sổ chứ không đóng. Một lệnh `hcom` *ghi* chạy song song, hoặc TUI, vẫn trúng race.
Và reconciliation vẫn phải chạy trên đường đọc để TUI thấy đúng sự thật sau reboot.

### Kiểm chứng

Test `real_claude_full_lifecycle_send_fork_kill_resume_and_cleanup` (deterministic 3/3) là
regression test sẵn có. Thêm một unit test ở tầng DB: dựng row instance với PID chết, set
`stop_reason:<name>`, gọi `mark_dead_instances`, assert life event reason == `killed`.

## Cách tái hiện

```
just mock-tools   # hoặc: ./scripts/install-mock-tools.sh
export PATH="$PWD/target/mock-tools/bin:$PATH"
cargo test --locked --test real_tool_claude \
  -- real_claude_full_lifecycle_send_fork_kill_resume_and_cleanup --ignored --nocapture --test-threads=1
```

(Yêu cầu commit `f46552d` — nếu chưa có, test sẽ fail sớm hơn ở bước cài plugin thay vì chạm
race này.)
