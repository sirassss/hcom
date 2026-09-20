# `is_alive` không phân biệt "tiến trình đã chết" với "tôi không thấy nó", và một lệnh hcom trong sandbox xoá identity của cả roster

**Ngày:** 2026-09-17
**Mục đích:** Ghi nhận vì sao agent đang sống bị `mark_dead_instances` reap với reason
`exit:reboot`, mất identity giữa lượt, và peer gửi tin nhận "Instance not found". Người dùng
phát hiện triệu chứng; mila (agent Codex) chẩn ra nguyên nhân bằng cách đo từ **ngoài** sandbox
của chính nó; tôi (kume) đối chiếu lại code.

`type: BUG` (reproduce 2 lần trong một giờ trên hai tool khác nhau; `ps` trên host xác nhận cả
hai PID bị reap vẫn đang chạy)

**TRẠNG THÁI: ĐANG FIX** — agent `mila` (Codex) làm trong worktree riêng
`.hcom/worktrees/mila-live-session`, nhánh `fix/mila-live-session` (nhánh từ `b58c4a0`). Hướng
đã chốt: reconciliation fail closed khi PID namespace vắng mặt hoặc khác caller, phủ luôn nhánh
stale-cleanup anh em. Chưa merge vào `feat/siras/develop`.

---

## Triệu chứng

Một agent đang chạy bình thường đột nhiên mất identity trên bus. Nó không biết. Lệnh
`hcom send` của chính nó bắt đầu trả:

```
Error: Instance 'lori' not found. Run 'hcom start --as lori' to reclaim your identity.
```

Peer gửi tin cho nó nhận "not found" và định tuyến đi chỗ khác. Không mất tin nhắn, nhưng agent
biến mất khỏi roster trong im lặng và chỉ quay lại nếu có ai đó chạy `hcom start --as <name>`
bằng tay.

## Bằng chứng đo được (2026-09-17)

**Ca 1 — `kume` (claude).** Bị reap lúc 08:12:37, `By: system`, `Reason: exit:reboot`. Snapshot
ghi `Session: bff86235-839e-4fd9-b954-a5360156ba26` — **đúng session đang chạy**. 4 lần stop
trong ~45 phút.

**Ca 2 — `lori` (codex), quyết định.** Vừa spawn, đang giữa lượt:

```
09:08:02  ready    by: kume
09:09:06  stopped  by: system   reason: exit:reboot      ← 64 giây sau
09:09:19  created  by: kume
```

Ca 2 loại bỏ mọi giả thuyết về restart tiến trình hay đặc thù một tool: agent 64 giây tuổi,
khác tool (codex vs claude), không có gì restart.

**Xác minh trên host:** `ps` chạy **ngoài** sandbox cho thấy cả hai PID bị reap vẫn sống —
44459 (claude) và 153428 (Codex MainThread).

## Cơ chế

`mark_dead_instances` (`src/instance_lifecycle.rs:891`) chạy ở `src/main.rs:72`, tức **đầu mọi
lệnh `hcom`**, kể cả lệnh chỉ đọc như `hcom list`. Nó duyệt **toàn bộ** instance, chỉ bỏ qua
`ST_INACTIVE`, `ST_LAUNCHING`, instance remote, và row không có pid. Phần còn lại chỉ phụ thuộc
một câu hỏi:

```rust
if crate::pidtrack::is_alive(pid) { continue; }
```

`is_alive` (`src/sys/process.rs:119`) trên Unix:

```rust
let ret = unsafe { libc::kill(pid as i32, 0) };
if ret == 0 { return true; }
std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
```

Hàm này **đã** xử lý `EPERM` là "còn sống" — nghĩa là tác giả biết rõ *kill trả lỗi* không đồng
nghĩa *tiến trình đã chết*. Nhưng `ESRCH` thì bị coi thẳng là chết, và đó chính là lỗ hổng:

**Khi `hcom` chạy bên trong sandbox của một tool, nó ở một PID namespace khác.** `/proc` chỉ
liệt kê PID namespace-local (1, 2, …) và `kill(host_pid, 0)` trả `ESRCH` — không phải vì tiến
trình chết, mà vì **namespace này không nhìn thấy nó**. Hai tình huống cho cùng một errno, và
code không phân biệt được.

PID lưu trong DB **đúng**: PTY launcher ghi `child.id()`, là host PID thật. Người ghi đúng;
**người đọc đứng sai namespace**.

## Bán kính ảnh hưởng (đã xác nhận, không phải suy đoán)

Vòng lặp không giới hạn ở instance của caller. Nó duyệt **mọi** instance local có PID được
track. Nên:

> Một lệnh `hcom` duy nhất chạy trong sandbox — kể cả `hcom list` — có thể finalize identity
> của **nhiều agent đang sống cùng lúc**, không riêng của chính nó.

Đây không phải "thỉnh thoảng agent mất tên". Đây là **tự xoá roster, kích hoạt bằng lệnh chỉ
đọc**. Khớp với dữ kiện: kume và lori chết trong cùng một giờ, cả hai đều đang chạy.

## Vì sao trước giờ chưa lộ

Chỉ xảy ra khi có một tiến trình `hcom` chạy trong PID namespace khác với namespace đã sinh ra
các agent. Đó đúng là điều xảy ra khi một agent bị sandbox (Codex) tự chạy lệnh `hcom` — một
cấu hình phổ biến hơn hẳn kể từ khi agent tự điều phối lẫn nhau.

## Nguyên tắc cho fix: reconciliation phải fail closed

Lỗi **không phải** là phát hiện liveness chưa hoàn hảo. Lỗi là **một quan sát bất định đang bị
coi là cái chết đã xác nhận**. `ESRCH` từ namespace lạ nghĩa là "tôi không thấy nó", không phải
"nó chết rồi".

Bất biến cần có: **chỉ reap khi xác nhận được cái chết**. Khi quan sát không đáng tin — namespace
vắng mặt hoặc khác caller — thì không làm gì và để nguyên row.

Cân đối thiệt hại không đối xứng, và repo này đã tự viết ra điều đó ở nhánh stale-cleanup
(`src/instance_lifecycle.rs:808-814`):

> Staleness is a clock inference, not an observed death: a wedged heartbeat (system sleep, a
> starved delivery loop) is indistinguishable from an exited tool by timestamps alone. Losing
> that bet is unrecoverable for the session.

Một row cũ còn sót chỉ tốn một dòng sai trong `hcom list`. Một lần reap nhầm lấy mất identity
của agent đang chạy và không tự phục hồi được. Reconciliation phải tuân với **namespace** đúng
luật mà cleanup đã tuân với **đồng hồ** — cùng hiểm hoạ, cùng câu trả lời.

Hướng khả dĩ: ghi PID namespace lúc launch và chỉ tin một kết quả âm khi namespace khớp. Nhánh
stale-cleanup anh em cũng cần phủ cùng lúc.

## Ghi chú phụ — reason `exit:reboot` gây hiểu nhầm

Row bị reap được gắn `exit:reboot`, nhưng không có lần reboot nào. Đây là fallback đúng theo
thiết kế hiện tại (không có claim thì dùng mặc định — xem
`2026-09-16-kill-reason-races-with-reboot-reconciliation.md`), nhưng khi đọc log để chẩn lỗi
này thì nó dẫn người ta đi sai hướng mất một lúc. Đáng cân nhắc một reason riêng cho "PID không
quan sát được" khi fix.

## Ghi chú phạm vi

Phát hiện khi kiểm một triệu chứng khác (agent rớt khỏi bus). Không liên quan tới branch
kill-race/plugin-verifier (`0f8e3dd..b58c4a0`) và cố ý không vá kèm — khác subsystem.

## Ghi chú bổ sung (2026-09-17) — plan TUI dead-agent reconciliation mở rộng cửa sổ rủi ro

Plan `docs/superpowers/plans/2026-09-13-tui-dead-agent-reconciliation.md` (thực thi cùng ngày,
worktree riêng `worktree-tui-dead-agent-reconciliation`, dựa trên `feat/siras/develop@0a8c412` —
**chưa có fix namespace của mila**, vì nhánh đó chưa merge) tách phần lõi của
`mark_dead_instances` sang `reconcile_dead_instances(db, detector)`, giữ `mark_dead_instances`
làm wrapper startup, nhưng **không đổi** cơ chế `is_alive`/`ESRCH` mô tả ở
trên — đã audit trong preflight của plan này và xác nhận hành vi giữ nguyên, cố ý không đụng vì
ngoài phạm vi 4 task của plan đó.

Điều thay đổi là **tần suất gọi**: trước đây `mark_dead_instances` chỉ chạy một lần mỗi lệnh
`hcom` (`main.rs:72`). Plan trên thêm một cadence độc lập trong TUI (`src/tui/app.rs`, hàm
`tick_reconcile`) gọi `reconcile_dead_instances(db, DeadProcessDetector::Tui)` **mỗi ~1 giây**
trong suốt thời gian một phiên TUI đang mở. Nếu phiên TUI đó chạy trong một PID namespace khác
với namespace đã spawn các agent đang track (đúng kịch bản mô tả ở trên — ví dụ TUI mở bên trong
sandbox của một tool khác), cửa sổ rủi ro "reap nhầm agent còn sống" tăng từ *một lần mỗi lệnh
hcom* lên *liên tục mỗi giây trong suốt phiên TUI* — cùng lỗi, bán kính ảnh hưởng lớn hơn nhiều.

Không vá trong plan TUI dead-agent reconciliation (đúng nguyên tắc "khác subsystem" đã nêu ở
trên) — nhưng fix namespace ở nhánh `fix/mila-live-session` nên tính luôn call site mới này
(`DeadProcessDetector::Tui`) khi merge, không chỉ đường `Startup` cũ.
