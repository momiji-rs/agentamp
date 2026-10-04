//! The window ends with its terminal.
//!
//! Each test opens the window in a pseudo-terminal, waits for its first
//! frame, then closes the terminal: with the hang-up signal a terminal sends
//! its controlling process, without it, as when the terminal goes away some
//! other way, and without it while stderr is that terminal too. The window
//! must be gone within a second, having used no CPU meanwhile. It runs
//! against an empty AGENTAMP_HOME, so nothing plays and no redraw can notice
//! the closed terminal for it.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Primary attributes, 10x20 cells, then the status: a plain terminal's answer to the image query.
const ANSWER: &[u8] = b"\x1b[?62;22c\x1b[6;20;10t\x1b[0n";

fn openpty() -> (File, OwnedFd) {
    let (mut main, mut child) = (0, 0);
    let size = libc::winsize { ws_row: 40, ws_col: 140, ws_xpixel: 1400, ws_ypixel: 800 };
    // SAFETY: valid pointers for the call; no name or termios asked for.
    let done = unsafe { libc::openpty(&mut main, &mut child, std::ptr::null_mut(), std::ptr::null(), &size) };
    assert_eq!(done, 0, "openpty: {}", std::io::Error::last_os_error());
    // openpty leaves both ends to every child: the window would hold its
    // own terminal open and never see it hang up.
    for fd in [main, child] {
        // SAFETY: a descriptor just opened.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both descriptors were just opened and nothing else owns them.
    unsafe { (File::from_raw_fd(main), OwnedFd::from_raw_fd(child)) }
}

/// What went wrong after the terminal closed, if anything.
fn close_terminal(name: &str, signal: bool, stderr_on_terminal: bool) -> Option<String> {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let (mut main, child) = openpty();
    let stderr = if stderr_on_terminal { Stdio::from(child.try_clone().unwrap()) } else { Stdio::null() };
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentamp"));
    command
        .stdin(child.try_clone().unwrap())
        .stdout(child.try_clone().unwrap())
        .stderr(stderr)
        .env("TERM", "xterm-256color")
        .env("AGENTAMP_HOME", &home)
        .env_remove("TMUX");
    // SAFETY: setsid and ioctl are async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || (signal && libc::ioctl(0, libc::TIOCSCTTY, 0) < 0) {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut process = command.spawn().unwrap();
    // The command keeps its copies of the terminal open, and the terminal
    // only hangs up once every copy outside the window is closed.
    drop(command);
    drop(child);

    let (mut output, mut answered) = (Vec::new(), false);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffer = [0; 1 << 16];
    while !contains(&output, b"quit") && Instant::now() < deadline {
        let mut ready = libc::pollfd { fd: main.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd, for as long as the call.
        if unsafe { libc::poll(&mut ready, 1, 50) } > 0 {
            match main.read(&mut buffer) {
                Ok(n) if n > 0 => output.extend_from_slice(&buffer[..n]),
                _ => break,
            }
        }
        if !answered && contains(&output, b"\x1b[5n") {
            main.write_all(ANSWER).unwrap();
            answered = true;
        }
    }
    let framed = contains(&output, b"quit");
    drop(main);
    std::thread::sleep(Duration::from_secs(1));
    let problem = match process.try_wait().unwrap() {
        // Then the terminal closed on a window that never got going, which proves nothing.
        _ if !framed => {
            let _ = process.kill();
            let _ = process.wait();
            Some("no first frame before the terminal closed".to_string())
        }
        None => {
            let ticks = cpu_ticks(process.id());
            let _ = process.kill();
            let _ = process.wait();
            Some(format!("still running after a second, {ticks} CPU ticks used"))
        }
        // A clean end, or ended by the hang-up signal.
        Some(status) if status.code() == Some(0) || status.signal() == Some(libc::SIGHUP) => None,
        Some(status) => Some(format!("ended with {status}")),
    };
    let _ = std::fs::remove_dir_all(&home);
    problem
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// User and system time, from `/proc/<pid>/stat`: the fields after the command's closing parenthesis.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    after.split_whitespace().skip(11).take(2).filter_map(|x| x.parse::<u64>().ok()).sum()
}

/// One test, the cases in turn: a window spawned while another case's
/// terminal is open would inherit it and keep it from hanging up.
#[test]
fn the_window_ends_with_its_terminal() {
    let problems: Vec<_> = [
        ("hang-up signal", "hangup_signal", true, false),
        ("no signal", "hangup_no_signal", false, false),
        ("no signal, stderr on the terminal", "hangup_no_signal_stderr", false, true),
    ]
    .into_iter()
    .filter_map(|(case, home, signal, stderr)| close_terminal(home, signal, stderr).map(|problem| format!("{case}: {problem}")))
    .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
