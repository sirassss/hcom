use super::{is_cmd_script, wait_child_blocking};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// Spawn `argv` under a ConPTY. Returns the master too: dropping it closes
/// the ConPTY and kills the child, so tests must keep it alive while
/// waiting.
///
/// A service thread plays the role of the outer terminal, which this
/// headless test doesn't have — without it the child hangs before ever
/// exiting (even `cmd /c exit 42`; both tests once ran 60s+ in CI):
/// - It drains the master output so the ConPTY output pipe never fills.
/// - It answers conhost's cursor-position query. portable-pty creates the
///   ConPTY with `PSEUDOCONSOLE_INHERIT_CURSOR`, so conhost emits `ESC[6n`
///   and blocks console initialization — and with it every console API
///   call the child makes — until an `ESC[<r>;<c>R` report arrives on the
///   input pipe. In production the real terminal answers automatically.
fn spawn_in_conpty(
    argv: &[&str],
) -> (
    Box<dyn portable_pty::MasterPty + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty (ConPTY) failed");
    let mut cmd = CommandBuilder::new(argv[0]);
    cmd.args(&argv[1..]);
    let child = pair.slave.spawn_command(cmd).expect("spawn failed");
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("try_clone_reader failed");
    let mut writer = pair.master.take_writer().expect("take_writer failed");
    std::thread::spawn(move || {
        let mut sink = [0u8; 8192];
        let mut replied = false;
        loop {
            match reader.read(&mut sink) {
                Ok(n) if n > 0 => {
                    if !replied && sink[..n].windows(4).any(|w| w == b"\x1b[6n") {
                        let _ = writer.write_all(b"\x1b[1;1R");
                        let _ = writer.flush();
                        replied = true;
                    }
                }
                _ => break,
            }
        }
    });
    (pair.master, child)
}

/// Run `wait_child_blocking` on a helper thread with a hard deadline, so a
/// hung child fails the test in bounded time instead of stalling CI until
/// the job timeout.
fn wait_with_deadline(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    deadline: Duration,
) -> i32 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(wait_child_blocking(child.as_mut()));
    });
    rx.recv_timeout(deadline)
        .expect("child did not exit within the test deadline")
}

#[test]
fn wait_child_blocking_returns_the_child_exit_code() {
    let (_master, child) = spawn_in_conpty(&["cmd.exe", "/c", "exit 42"]);
    assert_eq!(wait_with_deadline(child, Duration::from_secs(30)), 42);
}

#[test]
fn wait_child_blocking_does_not_kill_a_healthy_long_running_child() {
    // Regression: a 5s watchdog in run()'s child wait used to kill_group
    // every session a few seconds after launch, forcing the kill sentinel
    // (130) as the exit code. A child that outlives that window must still
    // exit on its own, with its own code.
    let (_master, child) = spawn_in_conpty(&["ping", "-n", "7", "127.0.0.1"]);
    let start = Instant::now();
    let code = wait_with_deadline(child, Duration::from_secs(60));
    let elapsed = start.elapsed();
    assert_eq!(code, 0, "child should exit on its own, not be killed");
    assert!(
        elapsed >= Duration::from_secs(5),
        "child exited after {elapsed:?}; expected it to outlive the former 5s watchdog window"
    );
}

#[test]
fn is_cmd_script_matches_cmd_and_bat_case_insensitively() {
    assert!(is_cmd_script("gemini.cmd"));
    assert!(is_cmd_script(r"C:\Users\me\AppData\Roaming\npm\codex.CMD"));
    assert!(is_cmd_script("run.bat"));
    assert!(is_cmd_script("RUN.BAT"));
}

#[test]
fn is_cmd_script_rejects_other_extensions() {
    assert!(!is_cmd_script("claude.exe"));
    assert!(!is_cmd_script("gemini"));
    assert!(!is_cmd_script("script.ps1"));
    assert!(!is_cmd_script("noext."));
}

// OutputModeFilter and its tests now live in `super::shared` so they run on
// the (non-Windows) host gate rather than being cross-compiled only.
