# Kill-reason race + plugin verifier staleness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `hcom` ghi đúng lý do dừng của một instance kể cả khi một tiến trình `hcom` khác chạy song song, và các verifier plugin trả lời "đã cài" dựa trên sổ đăng ký của tool chứ không dựa trên file rác còn sót trên đĩa.

**Architecture:** Hai bug độc lập trên cùng branch. **B1** đóng một race ghi-DB bằng cách cho bên sắp dừng instance "đặt chỗ" lý do vào bảng `kv` **trước** khi gửi tín hiệu, rồi `mark_dead_instances` fallback vào chỗ đặt đó thay vì hardcode `"exit:reboot"`. **B2** sửa hai verifier plugin đang suy ra trạng thái cài đặt từ sự tồn tại của một file/thư mục, chuyển sang đọc registry mà chính tool ghi (`known_marketplaces.json` / `installed_plugins.json` của Claude; plugin cache của Cursor) — vẫn thuần đọc file, không gọi CLI — và sửa metadata trong manifest plugin đang khai báo sai repo, thứ chặn việc phân biệt fork với upstream.

**Tech Stack:** Rust 2024, `rusqlite` (SQLite), `serde_json`, `cargo test` / `clippy` / `fmt`, test harness `tests/real_tool_claude.rs` + mock tools qua `just mock-tools`.

**Spec:**
- [docs/issues/2026-09-16-kill-reason-races-with-reboot-reconciliation.md](../../issues/2026-09-16-kill-reason-races-with-reboot-reconciliation.md)
- [docs/issues/2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md](../../issues/2026-09-16-plugin-verifiers-trust-stale-on-disk-artifacts.md)

## Global Constraints

- **Verifier plugin cấm gọi subprocess.** `verify_*_plugin_installed` chạy ở `launcher.rs:652/792/798`, tức trước **mỗi** lần spawn agent. Chỉ được `std::fs`. Ràng buộc này ghi sẵn trong doc của `verify_cursor_plugin_installed`.
- **Không thêm migration DB.** Bảng `kv` đã tồn tại (`src/db/mod.rs:360`) với helper `kv_get` / `kv_set` (`src/db/kv.rs:18,31`).
- **Không thêm dependency mới.**
- Repo plugin hiện tại: `HCOM_PLUGIN_REPOSITORY_URL = "https://github.com/sirassss/hcom-plugin"` (`src/hooks/plugin.rs:515`).
- Định danh plugin: `PLUGIN_NAME = "hcom"`, `CLAUDE_PLUGIN_ID = "hcom@hcom"`, `CLAUDE_MARKETPLACE = "hcom"` (`src/hooks/plugin.rs:68,82,85`).
- Test phải chạy được không cần mạng. Mọi fixture dựng cây thư mục giả qua `plugin_test_env()` (`src/hooks/plugin.rs:1177`), không clone repo thật.
- Baseline hiện tại: `cargo test --locked` → 2404 pass. Sau mỗi task, con số chỉ được tăng.
- Commit message theo convention đang dùng: `fix(<scope>): <mô tả>`, kết bằng dòng `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.

---

## File Structure

| File | Trách nhiệm sau khi xong |
|---|---|
| `src/hooks/common.rs` | Thêm `claim_stop_reason()` + `take_stop_reason()`; `stop_instance_inner` đặt chỗ lý do trước khi gửi tín hiệu |
| `src/commands/kill.rs` | `kill_tracked_instance` đặt chỗ lý do trước `kill_instance()` (tín hiệu đi trước `stop_instance` ở đường này) |
| `src/instance_lifecycle.rs` | `mark_dead_instances` đọc chỗ đặt thay vì hardcode `"exit:reboot"` |
| `src/hooks/plugin.rs` | `verify_cursor_plugin_installed` đọc plugin cache của Cursor; `verify_claude_plugin_installed` đọc `known_marketplaces.json` + `installed_plugins.json`; sửa module doc lỗi thời |
| `plugin/hcom/.{cursor,claude,codex}-plugin/plugin.json`, `plugin/hcom-agy/.claude-plugin/plugin.json` | Khai báo đúng repo đang ship chúng (`sirassss/hcom-plugin`) thay vì upstream |
| `plugin/.claude-plugin/marketplace.json` | Version khớp với plugin manifest |

Không tạo file source mới: mọi file đều đã tồn tại đều đã tồn tại và mỗi thay đổi thuộc đúng trách nhiệm sẵn có của file đó. Repo này để test cạnh code trong `mod tests` — giữ nguyên nếp đó.

---

## Task 1: Đặt chỗ lý do dừng trước khi gửi tín hiệu

**Files:**
- Modify: `src/hooks/common.rs` (thêm helper cạnh `stop_instance`, ~dòng 1057-1075; và trong `stop_instance_inner`, ~dòng 1122 ngay sau khi `instance_data` đọc xong)
- Modify: `src/commands/kill.rs:143-145`
- Modify: `src/instance_lifecycle.rs:958-965`
- Test: `src/instance_lifecycle.rs` (`mod tests` ở cuối file)

**Interfaces:**
- Produces:
  - `pub(crate) fn claim_stop_reason(db: &HcomDb, name: &str, session_id: Option<&str>, initiated_by: &str, reason: &str)` — ghi `kv["stop_reason:<name>"] = "<reason>|<initiated_by>|<session_id>"`. Best-effort, nuốt lỗi.
  - `pub(crate) fn take_stop_reason(db: &HcomDb, name: &str, session_id: Option<&str>) -> Option<(String, String)>` — đọc key, xoá nó, trả `Some((reason, initiated_by))` **chỉ khi** phần session_id khớp; lệch thì trả `None` (và vẫn xoá).

**Vì sao có cả `initiated_by`:** `mark_dead_instances` hardcode `"system"`. Nếu chỉ khôi phục
reason thì `hcom list --stopped` sau một lần `hcom kill` của người dùng sẽ hiện
`By: system, Reason: killed` — đúng một nửa. Cùng kích thước diff, lấy luôn cả hai.

- Cả hai helper ở `crate::hooks::common`.
- Consumes: `crate::db::kv::{kv_get, kv_set}` đã có sẵn.

**Vì sao `session_id` nằm trong value:** tên agent là từ CVCV 4 chữ và được tái sử dụng. Nếu tiến trình `hcom` chết giữa lúc đặt chỗ và lúc ghi life event, key còn lại; không có session_id thì một lần dừng sau đó của agent trùng tên sẽ bị gán nhầm lý do.

- [ ] **Step 1: Viết test thất bại**

`mod tests` ở `src/instance_lifecycle.rs:981` đã có sẵn mọi thứ cần — **dùng lại, đừng
dựng cái mới**:

- `setup_test_db() -> (HcomDb, PathBuf)` (`:986`) + `cleanup(path)` (`:1002`)
- `const DEAD_PID: i64 = 4_194_305` (`:1020`) — PID trên pid_max của mọi nền tảng, không cần
  fork gì, chạy được cả trên Windows

Chỉ cần thêm hai helper (`insert_stale_active` ở `:1024` không nhận `session_id`, mà test này
cần nó để kiểm phần so khớp session):

```rust
    fn insert_active_with_session(db: &HcomDb, name: &str, session_id: &str, pid: i64) {
        let now = now_epoch_i64();
        db.conn()
            .execute(
                "INSERT INTO instances
                    (name, tool, session_id, status, status_context, status_time, \
                     last_stop, created_at, pid, tcp_mode)
                 VALUES (?, 'claude', ?, ?, '', ?, ?, ?, ?, 1)",
                rusqlite::params![name, session_id, ST_ACTIVE, now, now, now as f64, pid],
            )
            .unwrap();
    }

    fn last_life_field(db: &HcomDb, name: &str, field: &str) -> String {
        let data: String = db
            .conn()
            .query_row(
                "SELECT data FROM events WHERE type = 'life' AND instance = ? \
                 ORDER BY id DESC LIMIT 1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&data).unwrap()[field]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn last_life_reason(db: &HcomDb, name: &str) -> String {
        last_life_field(db, name, "reason")
    }
```

Rồi ba test:

```rust
    #[test]
    fn mark_dead_prefers_claimed_stop_reason() {
        let (db, path) = setup_test_db();
        insert_active_with_session(&db, "kume", "sess-kume", DEAD_PID);

        crate::hooks::common::claim_stop_reason(&db, "kume", Some("sess-kume"), "alam", "killed");
        assert_eq!(mark_dead_instances(&db), 1);
        assert_eq!(last_life_reason(&db, "kume"), "killed");
        assert_eq!(last_life_field(&db, "kume", "by"), "alam");

        cleanup(path);
    }

    #[test]
    fn mark_dead_ignores_claim_from_a_different_session() {
        let (db, path) = setup_test_db();
        insert_active_with_session(&db, "kume", "sess-new", DEAD_PID);

        // Chỗ đặt còn sót lại từ một instance cũ trùng tên.
        crate::hooks::common::claim_stop_reason(&db, "kume", Some("sess-old"), "alam", "killed");
        assert_eq!(mark_dead_instances(&db), 1);
        assert_eq!(last_life_reason(&db, "kume"), "exit:reboot");

        cleanup(path);
    }

    #[test]
    fn mark_dead_without_a_claim_still_reports_reboot() {
        let (db, path) = setup_test_db();
        insert_active_with_session(&db, "kume", "sess-kume", DEAD_PID);

        assert_eq!(mark_dead_instances(&db), 1);
        assert_eq!(last_life_reason(&db, "kume"), "exit:reboot");

        cleanup(path);
    }
```

> **Nếu tên cột trong `INSERT` không khớp:** schema ở `src/db/mod.rs:308-342` là nguồn sự thật,
> sửa theo nó. Cũng kiểm tên bảng/cột event: `log_life_event` ghi đi đâu thì
> `last_life_reason` đọc ở đó (đọc `db.log_life_event` trước khi viết).

- [ ] **Step 2: Chạy test, xác nhận nó fail**

```bash
cargo test --locked --bin hcom instance_lifecycle::tests::mark_dead -- --nocapture
```

Kỳ vọng: `mark_dead_prefers_claimed_stop_reason` FAIL với `assertion failed: left == right` / `left: "exit:reboot"`, `right: "killed"`. Hai test kia PASS ngay (chúng mô tả hành vi hiện tại) — đó là chủ ý, chúng khoá lại phần không được đổi.

Nếu nó fail vì `claim_stop_reason` chưa tồn tại (lỗi biên dịch) thì cũng đúng — sang Step 3.

- [ ] **Step 3: Thêm hai helper vào `src/hooks/common.rs`**

Đặt ngay trên `pub fn stop_instance` (~dòng 1057):

```rust
/// Chỗ đặt trước lý do dừng, để một tiến trình `hcom` khác chạy
/// `mark_dead_instances` (main.rs:72, chạy ở MỌI lệnh) không ghi đè bằng
/// "exit:reboot" khi nó thấy PID chết trước lúc ta kịp ghi life event.
///
/// Value là `<reason>|<session_id>`: tên agent là từ CVCV và được tái sử dụng,
/// nên một key sót lại từ lần crash trước không được phép gán lý do cho một
/// instance khác trùng tên.
fn stop_reason_key(name: &str) -> String {
    format!("stop_reason:{name}")
}

pub(crate) fn claim_stop_reason(
    db: &HcomDb,
    name: &str,
    session_id: Option<&str>,
    initiated_by: &str,
    reason: &str,
) {
    let value = format!("{reason}|{initiated_by}|{}", session_id.unwrap_or(""));
    let _ = db.kv_set(&stop_reason_key(name), Some(&value));
}

/// Đọc và xoá chỗ đặt. Trả `None` khi không có, hoặc khi session lệch —
/// nhưng vẫn xoá trong cả hai trường hợp, để key sót không sống mãi.
pub(crate) fn take_stop_reason(
    db: &HcomDb,
    name: &str,
    session_id: Option<&str>,
) -> Option<(String, String)> {
    let key = stop_reason_key(name);
    let raw = db.kv_get(&key).ok().flatten()?;
    let _ = db.kv_set(&key, None);
    // splitn(3): session_id là phần cuối và không được phép bị cắt tiếp.
    let mut parts = raw.splitn(3, '|');
    let reason = parts.next()?;
    let initiated_by = parts.next()?;
    let claimed_session = parts.next()?;
    if claimed_session == session_id.unwrap_or("") {
        Some((reason.to_string(), initiated_by.to_string()))
    } else {
        None
    }
}
```

- [ ] **Step 4: Đặt chỗ ở `stop_instance_inner` — guard dùng chung cho mọi caller**

Trong `src/hooks/common.rs::stop_instance_inner`, ngay **sau** khối đọc `instance_data` (~dòng 1122, chỗ `let instance_data = match db.get_instance_full(...)`) và **trước** khối `if let Some(pid_val) = pid` (chỗ bắt đầu gửi tín hiệu):

```rust
    // Đặt chỗ lý do trước khi có bất kỳ tín hiệu nào được gửi. Từ đây trở đi
    // PID có thể chết bất cứ lúc nào, và mark_dead_instances của một tiến trình
    // hcom khác có thể thắng cuộc ghi.
    claim_stop_reason(
        db,
        instance_name,
        instance_data.session_id.as_deref(),
        initiated_by,
        reason,
    );
```

Đây là chỗ duy nhất cần cho `commands/stop.rs` (`stop_all` / `tag_stop` / `multi_stop`), `hooks/claude.rs` (`idle`, teardown subagent) và `instance_lifecycle.rs:835` (`exit_cleanup` / `stale_cleanup` / `inactive_cleanup`).

- [ ] **Step 5: Đặt chỗ sớm hơn ở đường `kill`**

`src/commands/kill.rs::kill_tracked_instance` gửi SIGTERM **trước** khi gọi `stop_instance`, nên guard ở Step 4 là quá muộn cho đường này. Sửa đoạn `:143-145`:

```rust
    // kill_instance() gửi SIGTERM ngay bên dưới; đặt chỗ trước đó, vì
    // stop_instance() chỉ chạy sau khi tiến trình đã có thể chết.
    crate::hooks::common::claim_stop_reason(
        db,
        name,
        inst.session_id.as_deref(),
        initiator,
        "killed",
    );
    let (result, pane_closed, pane_retry_command, preset_name, pane_id) =
        kill_instance(db, name, pid, &inst, is_headless);
    stop_instance(db, name, initiator, "killed");
```

Các caller `kill.rs:367/371/477/481` gọi thẳng `stop_instance` nên đã được Step 4 phủ — không sửa.

- [ ] **Step 6: Cho `mark_dead_instances` đọc chỗ đặt**

`src/instance_lifecycle.rs`, thay literal ở lời gọi `log_life_event` (~dòng 958-965):

```rust
        // Một hcom khác có thể đang dừng instance này có chủ đích; lý do của nó
        // đúng hơn phỏng đoán "reboot" của ta.
        let (reason, initiated_by) = crate::hooks::common::take_stop_reason(
            db,
            &inst.name,
            inst.session_id.as_deref(),
        )
        .unwrap_or_else(|| ("exit:reboot".to_string(), "system".to_string()));

        if db
            .log_life_event(&inst.name, "stopped", &initiated_by, &reason, Some(snapshot))
            .is_ok()
```

- [ ] **Step 7: Dọn chỗ đặt ở đường thắng cuộc bình thường**

`stop_instance_inner` phải xoá key của chính nó sau khi ghi life event xong, nếu không mọi lần dừng đều để lại một hàng `kv`. Tìm lời gọi `log_life_event` trong `stop_instance_inner` và thêm ngay sau nó:

```rust
    let _ = take_stop_reason(db, instance_name, instance_data.session_id.as_deref());
```

- [ ] **Step 8: Chạy lại test, xác nhận pass**

```bash
cargo test --locked --bin hcom instance_lifecycle::tests::mark_dead -- --nocapture
```

Kỳ vọng: cả ba PASS.

- [ ] **Step 9: Chạy toàn bộ suite + lint**

```bash
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Kỳ vọng: ≥ 2404 pass (2404 baseline + 3 test mới), clippy và fmt sạch.

- [ ] **Step 10: Commit**

```bash
git add src/hooks/common.rs src/commands/kill.rs src/instance_lifecycle.rs
git commit -m "$(cat <<'EOF'
fix(lifecycle): keep the intended stop reason when reboot reconciliation wins the race

mark_dead_instances runs at the top of every hcom invocation, so a concurrent
`hcom list` could delete the row and log "exit:reboot" before the process that
actually stopped the instance wrote its own reason. Claim the reason in `kv`
before any signal is sent, and let mark_dead_instances fall back to it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Regression thật cho race kill/reboot

**Files:**
- Test: `tests/real_tool_claude.rs` (chạy, không sửa) — assertion ở `tests/support/real_tool.rs:985`

**Interfaces:**
- Consumes: hành vi từ Task 1.
- Produces: không có code mới. Đây là cổng chứng minh Task 1 sửa được đúng triệu chứng đã báo cáo.

- [ ] **Step 1: Dựng mock tools**

```bash
just mock-tools
export PATH="$PWD/target/mock-tools/bin:$PATH"
```

Nếu `just` không có: `./scripts/install-mock-tools.sh`.

- [ ] **Step 2: Chạy test race ba lần liên tiếp**

```bash
for i in 1 2 3; do
  echo "=== run $i ==="
  cargo test --locked --test real_tool_claude -- \
    real_claude_full_lifecycle_send_fork_kill_resume_and_cleanup \
    --ignored --nocapture --test-threads=1 || break
done
```

Kỳ vọng: **3/3 PASS.** Trước Task 1, test này panic deterministic 3/3 tại `tests/support/real_tool.rs:985` với `killed snapshot did not preserve session <uuid> and reason: … Reason: exit:reboot`.

Một lần pass không chứng minh gì với bug race — phải đủ ba lần.

- [ ] **Step 3: Nếu vẫn fail, dừng lại và báo cáo**

Đừng nới assertion trong `tests/support/real_tool.rs`. Nếu reason ghi được vẫn là `exit:reboot`, nghĩa là có một đường thứ ba ghi life event mà Task 1 chưa phủ. Tìm nó bằng:

```bash
grep -rn "log_life_event" src/
```

rồi báo lại trước khi sửa tiếp.

- [ ] **Step 4: Commit (chỉ khi có thay đổi)**

Task này thường không sinh diff. Nếu phải sửa gì để 3/3 pass thì commit riêng với `fix(lifecycle): …`.

---

## Task 3: Manifest plugin phải khai báo repo của chính nó

**Files:**
- Modify: `plugin/hcom/.cursor-plugin/plugin.json`, `plugin/hcom/.claude-plugin/plugin.json`, `plugin/hcom/.codex-plugin/plugin.json`, `plugin/hcom-agy/.claude-plugin/plugin.json`
- Modify: `plugin/.claude-plugin/marketplace.json`
- Test: `src/hooks/plugin.rs` (`mod tests`)

**Interfaces:**
- Produces: manifest đã publish mang `repository`/`homepage` trỏ về
  `https://github.com/sirassss/hcom-plugin`.
- Consumes: không. Task 4 và 5 **không** phụ thuộc task này — nó mở đường cho việc scope
  verifier theo repo ở một release sau (xem "Ngoài phạm vi").

**Vì sao** (issue doc D5): `e9ea02b` chuyển plugin sang repo riêng và đã sửa **marketplace
descriptor** cho đúng (`plugin/.claude-plugin/marketplace.json`: `owner.name = "sirassss"`,
`homepage = "https://github.com/sirassss/hcom-plugin"`), nhưng bỏ sót cả 4 file `plugin.json`.
`scripts/sync-plugin-repo.sh` copy nguyên `plugin/hcom/.` sang `hcom/` của repo đã publish, nên
`sirassss/hcom-plugin` đang quảng cáo chính nó là `aannoo/hcom`.

Hệ quả kép: người dùng mở plugin trong Cursor/Claude thấy link trỏ về upstream (báo bug sai
chỗ), và không trường nào trên đĩa phân biệt được một bản cài từ fork với một bản từ upstream.

**Giữ nguyên** `"author": { "name": "aannoo" }` và `"license": "MIT"` — đó là ghi công tác giả
gốc của một dự án MIT, đúng đạo lý và đúng luật. Chỉ `homepage`/`repository` — "code này sống ở
đâu" — là sai.

- [ ] **Step 1: Viết test thất bại**

Thêm vào `mod tests` của `src/hooks/plugin.rs`. Test này đọc manifest **đã commit trong repo**,
không đọc `$HOME`, nên không cần `plugin_test_env()` hay `#[serial]`.

```rust
    /// Mọi manifest ta ship phải trỏ về repo thật sự chứa chúng. Trước đây cả
    /// bốn cái đều ghi upstream, nên không có trường nào trên đĩa phân biệt
    /// được một bản cài từ fork với một bản cài từ upstream.
    #[test]
    fn shipped_plugin_manifests_point_at_our_own_repo() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifests = [
            "plugin/hcom/.cursor-plugin/plugin.json",
            "plugin/hcom/.claude-plugin/plugin.json",
            "plugin/hcom/.codex-plugin/plugin.json",
            "plugin/hcom-agy/.claude-plugin/plugin.json",
        ];

        for relative in manifests {
            let path = repo_root.join(relative);
            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

            for field in ["homepage", "repository"] {
                assert_eq!(
                    json[field].as_str().unwrap(),
                    super::HCOM_PLUGIN_REPOSITORY_URL,
                    "{relative} field `{field}` must name the repo that ships it"
                );
            }
            // Ghi công tác giả gốc không được xoá cùng lúc.
            assert_eq!(json["author"]["name"].as_str().unwrap(), "aannoo");
            assert_eq!(json["license"].as_str().unwrap(), "MIT");
        }
    }

    /// Marketplace descriptor và plugin được publish cùng một lần bởi
    /// `scripts/sync-plugin-repo.sh`, nên version của chúng phải khớp.
    #[test]
    fn marketplace_and_plugin_versions_agree() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |relative: &str| -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(repo_root.join(relative)).unwrap())
                .unwrap()
        };

        assert_eq!(
            read("plugin/.claude-plugin/marketplace.json")["version"]
                .as_str()
                .unwrap(),
            read("plugin/hcom/.claude-plugin/plugin.json")["version"]
                .as_str()
                .unwrap(),
        );
    }
```

- [ ] **Step 2: Chạy test, xác nhận fail**

```bash
cargo test --locked --bin hcom hooks::plugin::tests::shipped_plugin_manifests -- --nocapture
cargo test --locked --bin hcom hooks::plugin::tests::marketplace_and_plugin_versions -- --nocapture
```

Kỳ vọng: cả hai FAIL. Test đầu báo `left: "https://github.com/aannoo/hcom"`,
`right: "https://github.com/sirassss/hcom-plugin"`. Test sau báo `left: "1.0.0"`, `right: "1.0.1"`.

- [ ] **Step 3: Sửa bốn manifest**

Trong mỗi file, đổi **đúng hai dòng**:

```json
  "homepage": "https://github.com/sirassss/hcom-plugin",
  "repository": "https://github.com/sirassss/hcom-plugin",
```

Không động vào `author`, `license`, `name`, `version`, `description`, `skills`, `hooks`.

```bash
grep -rn "aannoo" plugin/ --include=plugin.json
```

Sau khi sửa, lệnh trên chỉ còn in các dòng `"author"` — đúng 4 dòng, không hơn.

- [ ] **Step 4: Đồng bộ version của marketplace descriptor**

`plugin/.claude-plugin/marketplace.json` đang `"version": "1.0.0"` trong khi cả 4 plugin là
`1.0.1`. Đổi thành:

```json
  "version": "1.0.1",
```

- [ ] **Step 5: Chạy test, xác nhận pass**

```bash
cargo test --locked --bin hcom hooks::plugin::tests -- --nocapture
```

Kỳ vọng: toàn bộ PASS.

- [ ] **Step 6: Commit**

```bash
git add plugin/ src/hooks/plugin.rs
git commit -m "$(cat <<'EOF'
fix(plugin): point the shipped manifests at the repo that actually ships them

e9ea02b moved the plugin to its own repo and updated the marketplace
descriptor, but left all four plugin.json manifests advertising `aannoo/hcom`.
sync-plugin-repo.sh copies them verbatim, so the published sirassss/hcom-plugin
claimed to be upstream — and nothing on disk could tell a fork install from an
upstream one. Original authorship and the MIT license stay as they are; only
the repo location was wrong.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 7: Publish**

```bash
./scripts/sync-plugin-repo.sh
```

Kỳ vọng: `hcom-plugin: published`. Cần quyền push vào `sirassss/hcom-plugin` — nếu không có,
**báo lại thay vì bỏ qua**: manifest chưa publish thì chưa ai thấy, và bước "scope theo repo"
ở release sau sẽ dựa vào nó.


---

## Task 4: Cursor verifier — đọc plugin cache thay vì quét marketplace checkout

**Files:**
- Modify: `src/hooks/plugin.rs:342-348` (`cursor_marketplaces_dir`), `:388-417` (`verify_cursor_plugin_installed`), `:56-63` (module doc), `:531-551` (doc của `install_cursor_plugin`)
- Test: `src/hooks/plugin.rs` (`mod tests`, mở rộng quanh `:1300-1335`)

**Interfaces:**
- Produces:
  - `pub(crate) fn cursor_plugin_cache_dir() -> PathBuf` — `<cursor hooks dir>/plugins/cache/<CLAUDE_MARKETPLACE>/<PLUGIN_NAME>`.
  - `verify_cursor_plugin_installed() -> bool` giữ nguyên chữ ký; đổi tiêu chí.
- Consumes: `crate::hooks::cursor::get_cursor_hooks_path()`, `PLUGIN_NAME`, `CLAUDE_MARKETPLACE`.

**Tiêu chí mới** (đo trên máy thật 2026-09-16, xem issue doc D1–D3):

Cursor materialize plugin đã cài vào `~/.cursor/plugins/cache/<marketplace>/<plugin>/<id>/`, song song hệt Claude:

```
~/.cursor/plugins/cache/hcom/hcom/a1511e68…/
  .cache-complete
  .cursor-plugin/plugin.json
  hooks/hooks-cursor.json
```

Đó là tín hiệu đúng. `plugins/marketplaces/<host>/<owner>/<repo>/<sha>/` thì chỉ chứng minh `marketplace add` đã clone — và hiện tại verifier bắt vào một checkout của **repo cũ** (`sirassss/hcom`) nên không bao giờ chuyển sang false khi marketplace hiện tại bị gỡ.

Verifier mới: có ít nhất một thư mục con của `cursor_plugin_cache_dir()` chứa cả `.cache-complete` **và** `hooks/hooks-cursor.json`.

Những gì task này đạt được:
- **D2: triệt để.** Cache dùng layout `hooks/hooks-cursor.json`, không có tiền tố `plugin/`.
- **D1: một phần.** Một lần `marketplace add` trần (clone repo, chưa vào `/plugins`) không còn
  được tính là "đã cài". Chỉ một lần materialize thật mới tính.

**Tại sao không scope theo repo ngay ở task này.** Cả ba repo đặt tên marketplace là `hcom`
⇒ cache trùng đường dẫn bất kể nguồn, và trường `repository` đáng lẽ phân biệt được thì hiện
đang hỏng — **Task 3 sửa nó**. Nhưng cache đã materialize trên máy người dùng vẫn mang metadata
cũ cho tới khi họ reinstall trong `/plugins`. Bật scope ngay bây giờ sẽ làm **mọi người** nhảy
sang "not installed" ngay sau khi nâng cấp hcom. Để lại cho một release sau, khi manifest mới đã
lan đủ — ghi trong "Ngoài phạm vi".

**Đừng bịa check dựa trên `version`** — nó đúng trên máy này và sai ngay khi upstream bump.

- [ ] **Step 1: Viết test thất bại**

Trong `mod tests` của `src/hooks/plugin.rs`, cạnh các test `verify_cursor_*` sẵn có (`:1300-1335`). Copy cách dựng env từ test hàng xóm (`plugin_test_env()` ở `:1177`, kèm `#[serial]`).

```rust
#[test]
#[serial]
fn cursor_verifier_reads_the_plugin_cache_not_a_marketplace_checkout() {
    let (_dir, home, _guard) = plugin_test_env();

    // Checkout marketplace của repo CŨ, đúng layout mà verifier cũ chấp nhận.
    let stale = home
        .join(".cursor/plugins/marketplaces/github.com/sirassss/hcom/60dc686")
        .join("plugin/hcom/hooks");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("hooks-cursor.json"), "{}").unwrap();

    assert!(
        !super::verify_cursor_plugin_installed(),
        "a stale marketplace checkout must not count as an installed plugin"
    );
}

#[test]
#[serial]
fn cursor_verifier_accepts_a_completed_plugin_cache() {
    let (_dir, home, _guard) = plugin_test_env();

    let cache = home.join(".cursor/plugins/cache/hcom/hcom/a1511e68");
    std::fs::create_dir_all(cache.join("hooks")).unwrap();
    std::fs::write(cache.join("hooks/hooks-cursor.json"), "{}").unwrap();
    std::fs::write(cache.join(".cache-complete"), "").unwrap();

    assert!(super::verify_cursor_plugin_installed());
}

#[test]
#[serial]
fn cursor_verifier_rejects_a_half_written_plugin_cache() {
    let (_dir, home, _guard) = plugin_test_env();

    // Không có .cache-complete: Cursor chưa materialize xong.
    let cache = home.join(".cursor/plugins/cache/hcom/hcom/a1511e68");
    std::fs::create_dir_all(cache.join("hooks")).unwrap();
    std::fs::write(cache.join("hooks/hooks-cursor.json"), "{}").unwrap();

    assert!(!super::verify_cursor_plugin_installed());
}
```

> **Lưu ý:** `plugin_test_env()` trỏ HOME vào tempdir. Kiểm lại `crate::hooks::cursor::get_cursor_hooks_path()` có thật sự resolve qua HOME đó không (đọc hàm đó trước); nếu nó dùng biến môi trường khác, set thêm trong test giống các test Cursor sẵn có.

Đồng thời **sửa** các test cũ ở `:1308-1330` đang dựng layout `plugin/hcom/...` và assert `true` — chúng mã hoá đúng hành vi ta đang bỏ. Viết lại chúng theo cache layout.

- [ ] **Step 2: Chạy test, xác nhận fail**

```bash
cargo test --locked --bin hcom hooks::plugin::tests::cursor_verifier -- --nocapture
```

Kỳ vọng: `cursor_verifier_reads_the_plugin_cache_not_a_marketplace_checkout` FAIL (verifier cũ trả true), `cursor_verifier_accepts_a_completed_plugin_cache` FAIL (verifier cũ không nhìn cache).

- [ ] **Step 3: Thêm `cursor_plugin_cache_dir` và viết lại verifier**

Trong `src/hooks/plugin.rs`, ngay dưới `cursor_marketplaces_dir()` (~:348):

```rust
/// Nơi Cursor materialize một plugin đã cài: `plugins/cache/<marketplace>/<plugin>/<id>/`.
/// Cùng hình dạng với cache của Claude. Segment cuối là một hash thay đổi, nên
/// caller duyệt thư mục con.
pub(crate) fn cursor_plugin_cache_dir() -> PathBuf {
    crate::hooks::cursor::get_cursor_hooks_path()
        .parent()
        .map(|d| {
            d.join("plugins")
                .join("cache")
                .join(CLAUDE_MARKETPLACE)
                .join(PLUGIN_NAME)
        })
        .unwrap_or_default()
}
```

Rồi thay toàn bộ thân `verify_cursor_plugin_installed` (:388-417) bằng:

```rust
/// True khi Cursor đã materialize plugin hcom vào cache của nó.
///
/// Đo 2026-09-16: một plugin đã cài nằm ở
/// `~/.cursor/plugins/cache/hcom/hcom/<id>/`, có `.cache-complete` và
/// `hooks/hooks-cursor.json` — song song hệt cache của Claude.
///
/// Cố ý KHÔNG dùng `plugins/marketplaces/<host>/<owner>/<repo>/<sha>/`: thư mục
/// đó chỉ chứng minh `marketplace add` đã clone một repo nào đó. Verifier cũ
/// quét mọi repo, nên một checkout còn sót của repo cũ `sirassss/hcom` khiến nó
/// trả true ngay cả khi marketplace hiện tại đã bị gỡ (issue 2026-09-16, D1),
/// và path cứng `<sha>/plugin/hcom/...` của nó đã lỗi thời từ khi plugin dọn
/// sang repo riêng với layout `<sha>/hcom/...` (D2).
///
/// Vẫn KHÔNG chứng minh người dùng đã bật plugin trong `/plugins`: chưa đo được
/// cache này có bị xoá khi plugin bị disable hay không. Đừng dùng nó để mở khoá
/// việc strip legacy hooks (xem doc của `install_cursor_plugin`).
///
/// Chỉ đọc file — hàm này chạy trước mỗi lần spawn agent.
pub(crate) fn verify_cursor_plugin_installed() -> bool {
    let Ok(entries) = std::fs::read_dir(cursor_plugin_cache_dir()) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let root = entry.path();
        root.join(".cache-complete").is_file()
            && root
                .join("hooks")
                .join("hooks-cursor.json")
                .is_file()
    })
}
```

- [ ] **Step 4: Sửa hai khối doc đã lỗi thời**

`src/hooks/plugin.rs:56-63` còn ghi `"source": "./plugin/hcom"`. Repo plugin hiện tại khai báo `"source": "./hcom"` (đo được trong `.claude-plugin/marketplace.json` của checkout). Sửa thành:

```rust
//! Cursor does resolve a plugin declared in a repo subdirectory
//! (`marketplace.json` → `"source": "./hcom"`), so the plugin body can stay
//! where it is.
```

`src/hooks/plugin.rs:531-551` (doc của `install_cursor_plugin`) khẳng định "Cursor's enabled marker is not readable from disk". Đo lại thì cache materialize đọc được — nhưng **chưa đo** được nó do `/plugins install` hay do `marketplace add` sinh ra, và có mất khi disable không. Thêm một đoạn nói đúng chừng đó, và **không** đổi hành vi không-strip:

```rust
/// Đo lại 2026-09-16: Cursor CÓ một plugin cache đọc được
/// (`verify_cursor_plugin_installed`), mạnh hơn checkout marketplace. Nhưng
/// chưa đo được nó sinh ra ở bước nào và có biến mất khi plugin bị disable
/// không, nên nó vẫn KHÔNG đủ để mở khoá việc strip ở đây: đánh sai cuộc này
/// là Cursor không còn hook nào.
```

- [ ] **Step 5: Chạy test, xác nhận pass**

```bash
cargo test --locked --bin hcom hooks::plugin::tests::cursor -- --nocapture
```

Kỳ vọng: tất cả test `cursor*` PASS.

- [ ] **Step 6: Đối chiếu máy thật**

```bash
cargo build --release
./target/release/hcom hooks status | grep -i cursor
```

`cmd_hooks_status` (`src/commands/hooks.rs:236`) in `"marketplace ready"` khi `verify_cursor_plugin_installed()` true, `"no marketplace"` khi false — nên dòng này là phép thử trực tiếp.

Máy này có `~/.cursor/plugins/cache/hcom/hcom/a1511e68…/` với cả `.cache-complete` và
`hooks/hooks-cursor.json` (đo 2026-09-16) ⇒ kết quả phải vẫn là `marketplace ready`.
Nếu đổi thành `no marketplace`: `get_cursor_hooks_path()` không resolve ra chỗ ta nghĩ —
điều tra trước khi đi tiếp, đừng nới verifier.

> Cache đó là phiên bản **1.0.0** trong khi fork ship **1.0.1** — Cursor trên máy này đang
> chạy plugin cũ. Đó là việc của `/plugins` reinstall, không phải của task này. Ghi nhận,
> đừng sửa ở đây.

- [ ] **Step 7: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "$(cat <<'EOF'
fix(plugin): verify the Cursor plugin from its cache, not a marketplace checkout

The verifier scanned marketplace checkouts for
`<sha>/plugin/hcom/hooks/hooks-cursor.json`. That path stopped existing when the
plugin moved to its own repo, so a clean install verified as false; a leftover
clone of the old `sirassss/hcom` was the only thing keeping it true. Read the
plugin cache Cursor materializes on a real install instead, which proves more
than a bare `marketplace add` does.

Not scoped to one repo yet: the marketplace name is `hcom` everywhere, and an
already-materialized cache keeps its old manifest until the user reinstalls.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Claude verifier — đọc registry thay vì đoán từ cache dir

**Files:**
- Modify: `src/hooks/plugin.rs:355-369` (`verify_claude_plugin_installed`), thêm helper cạnh `claude_plugin_dir()` (:87-99)
- Test: `src/hooks/plugin.rs` (`mod tests`, cạnh `:1255-1280`)

**Interfaces:**
- Produces:
  - `pub(crate) fn claude_plugins_root() -> PathBuf` — `<claude settings dir>/plugins`.
  - `verify_claude_plugin_installed() -> bool` giữ nguyên chữ ký; thêm hai vế.
- Consumes: `crate::hooks::claude::{get_claude_settings_path, load_claude_settings}`, `CLAUDE_PLUGIN_ID`, `CLAUDE_MARKETPLACE`.

**Vì sao** (issue doc D4): `~/.claude/plugins/cache/hcom/hcom/1.0.0/.orphaned_at` = `1789492223821` — Claude **đánh dấu** bản cache mồ côi chứ không xoá. Verifier hiện tại chỉ hỏi "thư mục `cache/hcom/hcom/` có tồn tại không", nên một cache mồ côi giữ nó ở true mãi. Registry thật nằm sẵn trên đĩa, hcom chưa đọc.

**Đã đo 2026-09-16, hai file registry đều có mặt và đúng hình dạng** — tiêu chí dưới đây
không làm hỏng máy đang chạy tốt:

```jsonc
// known_marketplaces.json → "hcom"
{ "source": { "source": "git", "url": "https://github.com/sirassss/hcom-plugin" } }

// installed_plugins.json → plugins["hcom@hcom"]
[ { "scope": "user",
    "installPath": "/home/alam/.claude/plugins/cache/hcom/hcom/1.0.1",
    "version": "1.0.1",
    "gitCommitSha": "4fd21368ee881fb7265190631ddc1e3668280ce8" } ]
```

Lưu ý `"source": "git"` + `url` — khác các marketplace khác dùng `"source": "github"` + `repo`.
Verifier chỉ hỏi **khoá `"hcom"` có tồn tại không**, không đọc vào trong `source`, nên khác
biệt này không ảnh hưởng.

**Tiêu chí mới** — cả ba vế, thuần đọc file:
1. `settings.json → enabledPlugins["hcom@hcom"] == true` (giữ nguyên)
2. `plugins/known_marketplaces.json` có khoá `"hcom"`
3. `plugins/installed_plugins.json → plugins["hcom@hcom"]` là mảng không rỗng và có ít nhất một phần tử mà `installPath` là thư mục tồn tại

Vế 3 thay cho `claude_plugin_dir().is_dir()`: `installPath` trỏ vào **bản đang dùng**, thay vì "thư mục cha có con nào đó". Ưu tiên nó hơn việc tự bỏ qua thư mục có `.orphaned_at` — `.orphaned_at` là chi tiết nội bộ, `installed_plugins.json` là hợp đồng công khai hơn.

- [ ] **Step 1: Viết test thất bại**

Trong `mod tests` của `src/hooks/plugin.rs`, cạnh các test `verify_claude_plugin_installed` sẵn có (`:1255-1280`). Đọc chúng trước để dùng lại đúng helper dựng settings.

```rust
/// Dựng một cài đặt Claude đầy đủ và hợp lệ dưới `home`.
/// Trả về đường dẫn installPath để test tự làm hỏng từng phần.
fn write_healthy_claude_install(home: &std::path::Path) -> std::path::PathBuf {
    let plugins = home.join(".claude/plugins");
    let install_path = plugins.join("cache/hcom/hcom/1.0.1");
    std::fs::create_dir_all(&install_path).unwrap();

    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"enabledPlugins":{"hcom@hcom":true}}"#,
    )
    .unwrap();
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        r#"{"hcom":{"source":{"source":"git","url":"https://github.com/sirassss/hcom-plugin"}}}"#,
    )
    .unwrap();
    std::fs::write(
        plugins.join("installed_plugins.json"),
        format!(
            r#"{{"plugins":{{"hcom@hcom":[{{"scope":"user","installPath":"{}","version":"1.0.1"}}]}}}}"#,
            install_path.display()
        ),
    )
    .unwrap();
    install_path
}

#[test]
#[serial]
fn claude_verifier_accepts_a_healthy_install() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    write_healthy_claude_install(&home);
    assert!(super::verify_claude_plugin_installed());
}

#[test]
#[serial]
fn claude_verifier_rejects_a_removed_marketplace() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    write_healthy_claude_install(&home);

    // Người dùng chạy `claude plugin marketplace remove hcom`; cache và
    // enabledPlugins vẫn còn.
    std::fs::write(
        home.join(".claude/plugins/known_marketplaces.json"),
        r#"{"superpowers-marketplace":{}}"#,
    )
    .unwrap();

    assert!(
        !super::verify_claude_plugin_installed(),
        "an orphaned cache must not read as an installed plugin"
    );
}

#[test]
#[serial]
fn claude_verifier_rejects_a_dangling_install_path() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let install_path = write_healthy_claude_install(&home);
    std::fs::remove_dir_all(&install_path).unwrap();

    assert!(!super::verify_claude_plugin_installed());
}
```

- [ ] **Step 2: Chạy test, xác nhận fail**

```bash
cargo test --locked --bin hcom hooks::plugin::tests::claude_verifier -- --nocapture
```

Kỳ vọng: `claude_verifier_rejects_a_removed_marketplace` và `claude_verifier_rejects_a_dangling_install_path` FAIL (verifier cũ trả true). `claude_verifier_accepts_a_healthy_install` PASS ngay.

- [ ] **Step 3: Thêm helper và viết lại verifier**

Dưới `claude_plugin_dir()` (~:99) trong `src/hooks/plugin.rs`:

```rust
/// Thư mục Claude giữ registry plugin của nó.
pub(crate) fn claude_plugins_root() -> PathBuf {
    crate::hooks::claude::get_claude_settings_path()
        .parent()
        .map(|d| d.join("plugins"))
        .unwrap_or_default()
}

/// Đọc một file JSON, `None` nếu thiếu hoặc hỏng.
fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}
```

Rồi thay thân `verify_claude_plugin_installed` (:355-369):

```rust
/// True khi Claude vừa BẬT plugin, vừa còn đăng ký marketplace, vừa có một bản
/// cài đặt thật trên đĩa.
///
/// Đo 2026-09-16: Claude ĐÁNH DẤU bản cache mồ côi
/// (`cache/hcom/hcom/<ver>/.orphaned_at`) chứ không xoá, nên câu hỏi cũ
/// "`cache/hcom/hcom/` có phải thư mục không" vẫn true rất lâu sau khi
/// marketplace bị gỡ tay. `installed_plugins.json` trỏ vào bản ĐANG dùng, và
/// `known_marketplaces.json` là thứ duy nhất chứng kiến việc gỡ marketplace.
///
/// Chỉ đọc file — hàm này chạy trước mỗi lần spawn agent.
pub(crate) fn verify_claude_plugin_installed() -> bool {
    let settings_path = crate::hooks::claude::get_claude_settings_path();
    let Some(settings) = crate::hooks::claude::load_claude_settings(&settings_path) else {
        return false;
    };
    let enabled = settings
        .get("enabledPlugins")
        .and_then(|p| p.get(CLAUDE_PLUGIN_ID))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return false;
    }

    let root = claude_plugins_root();

    let marketplace_known = read_json(&root.join("known_marketplaces.json"))
        .and_then(|v| Some(v.get(CLAUDE_MARKETPLACE)?.is_object()))
        .unwrap_or(false);
    if !marketplace_known {
        return false;
    }

    read_json(&root.join("installed_plugins.json"))
        .and_then(|v| {
            Some(
                v.get("plugins")?
                    .get(CLAUDE_PLUGIN_ID)?
                    .as_array()?
                    .iter()
                    .any(|entry| {
                        entry
                            .get("installPath")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|p| Path::new(p).is_dir())
                    }),
            )
        })
        .unwrap_or(false)
}
```

`Path` đã được import ở đầu file (`use std::path::{Path, PathBuf};`).

- [ ] **Step 4: Chạy test, xác nhận pass**

```bash
cargo test --locked --bin hcom hooks::plugin::tests::claude -- --nocapture
```

Kỳ vọng: tất cả PASS. Các test cũ ở `:1255-1280` sẽ hỏng vì chúng chỉ dựng `enabledPlugins` + cache dir — **sửa chúng** để dựng thêm hai file registry (dùng `write_healthy_claude_install`), đừng nới verifier.

- [ ] **Step 5: Đối chiếu máy thật**

```bash
cargo build --release
./target/release/hcom hooks status | head -3
```

Kỳ vọng: `Claude:  installed    (plugin)` — máy này có đủ cả ba vế (đã kiểm 2026-09-16). Nếu ra `not installed`, đọc `~/.claude/plugins/installed_plugins.json` xem `hcom@hcom` có mặt không rồi điều tra, đừng nới verifier.

- [ ] **Step 6: Commit**

```bash
git add src/hooks/plugin.rs
git commit -m "$(cat <<'EOF'
fix(plugin): verify the Claude plugin against the registry, not an orphaned cache

Claude marks an orphaned plugin cache with `.orphaned_at` instead of deleting
it, so `cache/hcom/hcom/` outlived a hand-removed marketplace and the verifier
kept answering "installed". Check `known_marketplaces.json` and the installPath
recorded in `installed_plugins.json` as well.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Rà lại các consumer bị siết

**Files:**
- Đọc/sửa nếu cần: `src/hooks/plugin.rs:667-705` (`remove_claude_plugin`, `remove_cursor_plugin`, `remove_agy_plugin`)
- Đọc: `src/launcher.rs:652,792,798`; `src/commands/hooks.rs:369` (`plugin_add_can_short_circuit`); `src/commands/start.rs:687`; `src/commands/status.rs:106`

**Interfaces:**
- Consumes: verifier từ Task 4 và Task 5.
- Produces: không có API mới. Sản phẩm là một quyết định có ghi lại: gate của `remove` có phải tách khỏi gate của `add` không.

Verifier chặt hơn không chỉ đổi `hooks add`. Task này kiểm từng consumer.

- [ ] **Step 1: Kiểm gate của `hooks remove`**

`remove_claude_plugin` (:671), `remove_cursor_plugin` (:690), `remove_agy_plugin` (:700) đều bắt đầu bằng `if !verify_*_plugin_installed() { ... }`. Đọc cả ba và trả lời trên giấy: sau Task 4/5, một cài đặt **nửa vời** (marketplace bị gỡ tay nhưng cache + `enabledPlugins` còn) có bị `hcom hooks remove claude` từ chối dọn không?

Nếu có: `remove` phải rộng hơn `add` — còn gì thì gỡ nấy. Sửa gate của `remove` sang một điều kiện lỏng hơn (ví dụ: `enabledPlugins` có entry **hoặc** cache dir tồn tại), giữ nguyên verifier chặt cho `add`/`launcher`. Thêm một test cho đúng case đó.

Nếu không: ghi một dòng comment nói vì sao, để lần sau khỏi phải suy lại.

- [ ] **Step 2: Kiểm `hooks add` rơi đúng nhánh**

```bash
cargo build --release
```

Đọc `src/commands/hooks.rs:369-400`. Xác nhận: khi verifier trả false, `plugin_add_can_short_circuit` trả false ⇒ chạy `tool.try_setup_hooks(...)` ⇒ **không** in "already installed". Đây chính là bug ban đầu được báo cáo.

Thêm một unit test cho `plugin_add_can_short_circuit` nếu chưa có case `hooks_installed == false`:

```rust
#[test]
fn add_does_not_short_circuit_when_the_verifier_says_no() {
    assert!(!plugin_add_can_short_circuit(Tool::Cursor, false, true));
    assert!(!plugin_add_can_short_circuit(Tool::Claude, false, true));
}
```

- [ ] **Step 3: Ghi nhận thay đổi hành vi ở đường spawn**

`launcher.rs:652/792/798` chạy verifier mỗi lần spawn agent. Người dùng đang ở trạng thái "cài lỗi nhưng im lặng chạy được" sẽ bắt đầu thấy cảnh báo missing hooks. Đó là **đúng**, nhưng là thay đổi người dùng thấy được.

Đọc thông điệp cảnh báo tại ba chỗ đó và xác nhận nó nói được cách khắc phục (`hcom hooks add <tool>`). Nếu không, sửa.

- [ ] **Step 4: Chạy toàn bộ suite + lint**

```bash
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Kỳ vọng: ≥ 2404 pass, clippy và fmt sạch.

- [ ] **Step 5: Đối chiếu toàn bộ máy thật**

```bash
./target/release/hcom hooks status
```

So với ảnh chụp trước khi sửa (2026-09-16):

```
Claude:  installed    (plugin)
codex:  installed (plugin hooks active)
Antigravity:  installed    (plugin)
Cursor:  marketplace ready (plugin)
```

Ba dòng đầu phải giữ nguyên. Dòng Cursor giờ phải phản ánh plugin cache thật, không phải checkout của repo cũ.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix(plugin): keep `hooks remove` able to clean up a half-installed plugin

Tightening the add/spawn verifiers also tightened the gate `hooks remove` sits
behind. Split them so removal stays able to clean up whatever is left.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

Nếu Step 1 kết luận không cần sửa gì, bỏ qua commit này.

---

## Ngoài phạm vi (cố ý)

- **`verify_agy_plugin_installed`** có cùng hình dạng defect (chỉ hỏi "file hook có tồn tại không"), nhưng AGY cài bằng `agy plugin install <dir|url>` copy thẳng vào config dir — **không có registry marketplace để đối chiếu**. `import_manifest.json` ghi lại import nhưng doc của `agy_imported_hcom_source` (`plugin.rs:170-180`) nói rõ một entry ở đó không chứng minh nguồn gốc. Không có tín hiệu tốt hơn để đọc ⇒ để nguyên.
- **Codex** không cùng root cause: `codex_plugin_status` hỏi inventory sống qua RPC `codex hooks/list`, fetch lỗi → `Unverified` → `ActionRequired`, không bao giờ ra "already installed". `cmd_hooks_add` cũng short-circuit Codex trước khi chạm `verify_hooks_installed`. Ghi chú gốc gộp nhầm codex với cursor.
- **Scope verifier theo repo** — hoãn sang release sau. Task 3 làm `repository` trở thành
  discriminator thật, nhưng cache đã materialize trên máy người dùng vẫn mang manifest cũ cho
  tới khi họ reinstall. Bật scope ngay là mọi người nhảy sang "not installed" sau khi nâng cấp
  hcom. Khi bật: đọc `<cache>/.cursor-plugin/plugin.json` → `repository`, so với
  `HCOM_PLUGIN_REPOSITORY_URL`. Đó là lúc D1 đóng hoàn toàn.
- **Strip legacy hooks cho Cursor** vẫn không đổi. Plugin cache đọc được (Task 4) mạnh hơn checkout marketplace, nhưng chưa đo được nó sinh ra ở bước nào và có mất khi disable không. Đánh sai cuộc đó là Cursor không còn hook nào.
