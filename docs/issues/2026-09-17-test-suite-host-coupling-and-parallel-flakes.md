# Test suite phụ thuộc trạng thái host và lệch nhau dưới parallel run

**Ngày:** 2026-09-17
**Mục đích:** Ghi nhận ba cách test suite nói dối, phát hiện rải rác trong lúc làm hai việc
khác. Không cái nào là lỗi production, nhưng cả ba đều làm `cargo test` mất giá trị làm cổng
kiểm — mà đó chính là thứ mọi review trong repo này dựa vào.

`type: BUG` (B1 hoá ra không phải test-only — xem cập nhật; B2/B3 vẫn test-only, cả hai reproduce được)

**TRẠNG THÁI: CẢ BA ĐÃ FIX (2026-09-20).** Phát hiện phụ ngoài scope ba mục này: cụm flake
sqlite-lock riêng trên `tui::db::tests::*`, xem cuối mục B2 — chưa sửa, cần issue riêng.

---

## B1 — `shell_env` fail cứng trên host có `~/.local/bin/env`

**ĐÃ FIX (2026-09-20).** Không phải test-only: `resolve_shell_env_uncached()`
(`src/shell_env.rs`) — code production, chạy mỗi lần resolve shell env cho nested launch —
gọi `env -0` giống hệt test. Trên host có `~/.local/bin/env` (do uv/rye cài) đứng trước
`/usr/bin/env` trong PATH, việc resolve env thật **cũng hỏng trong production**, không chỉ
trong test.

`shell_env::tests::resolver_discards_stderr_without_breaking_env_resolution` từng fail **100%**
trên máy dev này, kể cả khi chạy một mình và `--test-threads=1`.

**Nguyên nhân (mila chẩn, tôi xác nhận):** test spawn một login shell thật và mong `env` in ra
môi trường. Nhưng login shell resolve `env` thành `~/.local/bin/env` — một script 3 dòng chỉ
`export PATH="$HOME/.local/bin:$PATH"` (do uv/rye cài) — chứ không phải `/usr/bin/env`. Script
đó không in gì, nên `PATH` không bao giờ xuất hiện trong output bị bắt.

```
$ head -3 ~/.local/bin/env
#!/bin/sh
        export PATH="$HOME/.local/bin:$PATH"
```

**Vì sao đây là bug thật, không chỉ của test:** cả `resolve_shell_env_uncached()` lẫn test đều
gọi `env` qua PATH resolution của login shell, nên bất kỳ shim nào tên `env` đứng trước trong
PATH đều làm cả hai fail giống nhau trên host này.

**Đã loại trừ:** không phải race, không phải flake, không do branch nào gây ra. Tôi checkout
worktree ở base `0f8e3dd` (trước mọi thay đổi gần đây) và chạy riêng test này — fail y hệt,
0.03s.

**Fix:** thêm `env_binary()` — trả `/usr/bin/env` nếu tồn tại (đúng trên mọi platform hỗ trợ),
fallback `env` nếu không. Dùng ở cả `resolve_shell_env_uncached()` và test.

**Bẫy phụ phát hiện khi fix:** sửa command literal cần thêm named arg `{env_bin}` vào
`format!`, và escape nhầm `${MARKER_VAR}` thành `${{MARKER_VAR}}` — tắt mất tính năng
"captured identifier" của Rust `format!` (single-brace `{MARKER_VAR}` nội suy thẳng hằng số
`MARKER_VAR` vào chuỗi lệnh shell). Escape kép biến nó thành text chết `MARKER_VAR` trong shell,
khiến marker không bao giờ khớp. Đã sửa lại về single-brace. Bài học: `${MARKER_VAR}` trong
`format!` không phải cú pháp shell thuần — nó vừa là biến shell vừa là placeholder Rust, chỉ
escape các cặp brace thật sự cần literal.

**Verify:** `cargo test --bin hcom shell_env` 12/12 pass; full suite 2488 pass/0 fail (chạy cô
lập, không parallel); `cargo fmt --check` và `cargo clippy --all-targets -- -D warnings` sạch.

## B2 — Hai test chỉ fail dưới full parallel run

**ĐÃ FIX (2026-09-20)** cho 2 test named dưới đây. Nghi vấn trong bản ghi cũ được xác nhận
đúng, và thu hẹp về đúng một thủ phạm.

- `codex_preprocessing::test_local_scan_bypass_suppresses_workspace_trust`
- `launcher::tests::omp_extension_args_are_injected_once`

Cả hai **pass khi chạy riêng** (đã kiểm 2026-09-17), chỉ thỉnh thoảng fail trong full suite
chạy song song.

**Chẩn ra:** `omp_extension_args_are_injected_once` (`src/launcher_tests.rs`) gọi
`inject_omp_extension_args` hai lần liên tiếp và so sánh kết quả phải giống nhau.
`inject_omp_extension_args → get_omp_plugin_path → current_home_dir()` đọc thẳng
`std::env::var("HOME")`, **không giữ** `crate::hooks::test_helpers::EnvGuard` (mutex
`TEST_ENV_LOCK` dùng chung cho mọi test mutate `HOME`/`HCOM_DIR`), và bản thân test cũng không
đánh dấu `#[serial]`. Mọi test khác trong repo mutate `HOME` đều tuân thủ quy ước "giữ
`EnvGuard::new()` + `#[serial]`" — riêng test này đọc `HOME` "trần", nên khi một test khác đang
giữ lock và tạm đặt `HOME` vào một tempdir, lần đọc thứ hai của test này có thể thấy một giá trị
`HOME` khác lần đầu, làm hai lời gọi cho ra hai đường dẫn khác nhau → assert idempotency fail.
Đây đúng là biến thể của giả thuyết cũ ("test đọc `HOME` mà không `#[serial]`"), chỉ khác là cơ
chế bảo vệ thật sự trong repo này là mutex `TEST_ENV_LOCK`, không phải bản thân `#[serial]` (một
số test hợp lệ khác trong `launcher_tests.rs`, ví dụ `test_runner_script_strips_instance_state_vars`,
chỉ giữ `EnvGuard::new()` mà không có `#[serial]` và vẫn an toàn).

**Fix:** thêm `let _env_lock = crate::hooks::test_helpers::EnvGuard::new();` vào đầu
`omp_extension_args_are_injected_once`, giữ đúng mutex mà mọi mutator `HOME` khác đã giữ.

`test_local_scan_bypass_suppresses_workspace_trust` đã `#[serial]` sẵn (nên không thể race với
một mutator `#[serial]` khác); nó là nạn nhân gián tiếp của cùng con đường trên. Không sửa gì
thêm ở test này — sau khi khoá `omp_extension_args_are_injected_once`, chạy 5 lần full parallel
suite không thấy nó fail lại lần nào (xem Verify).

**Verify:** `cargo test --bin hcom` (parallel mặc định) chạy 5 lần liên tiếp — cả hai test named
trên **không fail lần nào**. `cargo fmt --check` và `cargo clippy --all-targets -- -D warnings`
sạch.

**Phát hiện phụ, CHƯA SỬA — nằm ngoài scope B2:** cả 5 lần chạy trên đều có 0–2 test khác fail,
không phải 2 test này, mà là cụm `tui::db::tests::*` (vd.
`db_source_reconcile_removes_dead_row_then_returns_zero`,
`write_db_open_success_clears_a_stale_last_error`, `load_orphans_lists_processes_from_a_foreign_namespace`)
với lỗi `database is locked`, và một lần `commands::start::tests::test_vanilla_claude_rebind_binds_session_and_drops_old_identity`.
Tất cả pass khi chạy riêng lẻ. Đây là một lớp race **khác** (sqlite lock trên fixture DB dùng
chung giữa test chạy song song), không phải HOME/PATH — không nằm trong 2 test B2 named ban đầu.
Ghi nhận ở đây để không lẫn với B2 đã đóng; cần issue riêng nếu muốn chẩn/sửa.

## B3 — Ba test không thể pass khi repo nằm trong `/tmp`

**ĐÃ FIX (2026-09-20).**

- `db::tests::test_open_raw_rejects_non_temp_path`
- `db::tests::test_open_raw_rejects_temp_symlink_to_non_temp_path`
- `paths::tests::test_is_test_temp_path_rejects_non_temp`

Cả ba khẳng định "đường dẫn **không** phải temp thì phải bị từ chối". Nếu chính repo được
checkout vào `/tmp` — chuyện bình thường khi một agent tạo worktree — thì đường dẫn repo *là*
temp path và ba test này fail.

**Cách tôi gặp:** một agent tạo worktree ở `/tmp/hcom-lori-spinner`, chạy full suite, thấy 3
lỗi này và tưởng mình gây ra. Tôi checkout `HEAD~1` trong chính worktree đó, thấy 3 lỗi y hệt,
nên loại được. Ở checkout thật (không nằm trong `/tmp`) cả ba pass.

**Vì sao đáng sửa:** nó trừng phạt đúng một thói quen tốt (dùng worktree tách biệt) bằng những
lỗi trông như hồi quy, và tốn của người/agent tiếp theo đúng chu trình điều tra mà tôi vừa tốn.

**Fix:** cả 3 test đổi fixture "non-temp path" từ `CARGO_MANIFEST_DIR` (vị trí repo, có thể nằm
trong `/tmp`) sang một đường dẫn suy ra thuần từ `std::env::temp_dir()`:
- `paths::tests::test_is_test_temp_path_rejects_non_temp` +
  `db::tests::test_open_raw_rejects_non_temp_path`: dùng `temp_dir().parent()` — ancestor của
  chính temp dir, chắc chắn không nằm trong nó, luôn tồn tại (là một phần đường dẫn tuyệt đối
  đang sống), không phụ thuộc repo checkout ở đâu.
- `db::tests::test_open_raw_rejects_temp_symlink_to_non_temp_path`: đổi symlink target từ
  `CARGO_MANIFEST_DIR` sang `/` — luôn tồn tại, không bao giờ nằm trong temp dir.

**Verify:** dựng `git worktree add /tmp/hcom-b3-verify` (đúng kịch bản báo cáo) — cả 3 test
reproduce fail y hệt mô tả trên code cũ; áp fix vào worktree đó, cả 3 pass. Đã xoá worktree
tạm sau khi verify.

## Vì sao gộp chung một issue

Ba triệu chứng khác nhau nhưng cùng một lớp: **test khẳng định về hành vi của hcom nhưng lại
phụ thuộc vào trạng thái của máy chạy nó** — PATH của login shell (B1), biến môi trường toàn
cục dùng chung (B2), vị trí checkout (B3). Hệ quả chung: người chạy suite phải nhớ một danh
sách "lỗi này không tính" mới đọc được kết quả, và mỗi mục trong danh sách đó là một chỗ để
một hồi quy thật lẩn vào.

## Đã tránh được gì nhờ ghi lại

Trong phiên này, B1 và B3 đều từng bị nghi oan cho code mới trước khi bị loại. B3 cụ thể suýt
được báo cáo như hồi quy của một fix hoàn toàn sạch.
