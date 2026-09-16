# `hcom kill` reason lost to reboot reconciliation under concurrent `hcom` calls

**Ngày:** 2026-09-16
**Mục đích:** Ghi nhận một race condition thật trong `instance_lifecycle::mark_dead_instances`,
phát hiện khi chạy lại `real_tool_claude` (bản pinned Claude Code 2.1.216) cục bộ sau khi vá
lỗ hổng plugin-install trong test harness (xem commit `f46552d`). Bug này có thể đã tồn tại từ
lâu — CI trước giờ chưa từng chạy tới đoạn code này vì luôn fail sớm hơn ở bước cài hook.

`type: BUG` (đã reproduce cục bộ 3/3 lần, chưa raise task, chưa quyết hướng fix)

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

## Gợi ý hướng fix (chưa quyết, chưa raise task)

- `mark_dead_instances` nên bỏ qua/deprioritize instance khi có một `kill`/`stop` đang chạy dở —
  cần cờ "in-flight stop" (ví dụ ghi trạng thái tạm trước khi gửi SIGTERM) mà `mark_dead_instances`
  kiểm tra trước khi tự ý ghi đè.
- Hoặc: gộp bước "kill" + "record reason" thành một transaction/khoá ở tầng DB để loại race
  triệt để, thay vì chỉ giảm cửa sổ.
- Cần hiểu rõ hơn các nơi khác đang đọc field reason này trước khi chọn hướng, nên để nguyên
  chưa tự vá.

## Cách tái hiện

```
just mock-tools   # hoặc: ./scripts/install-mock-tools.sh
export PATH="$PWD/target/mock-tools/bin:$PATH"
cargo test --locked --test real_tool_claude \
  -- real_claude_full_lifecycle_send_fork_kill_resume_and_cleanup --ignored --nocapture --test-threads=1
```

(Yêu cầu commit `f46552d` — nếu chưa có, test sẽ fail sớm hơn ở bước cài plugin thay vì chạm
race này.)
