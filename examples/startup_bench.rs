//! Times the window's start, step by step, in a pseudo-terminal.
//!
//!     cargo run --release --example startup_bench -- [--runs 20] [--terminal kitty|sixel|plain|silent] [--size 140x40] [BINARY]
//!
//! The harness plays the terminal: it answers the image query the way kitty
//! (images), foot (Sixel), a plain 24-bit terminal (half blocks) or a terminal that answers
//! nothing would. It reads the window's AGENTAMP_TRACE marks, presses `/` as
//! soon as the first frame has arrived and times how long the prompt takes to
//! show, then quits with `q`. Times are milliseconds from the spawn; each row
//! is the median and the slowest run.
//!
//! The daemon is whatever AGENTAMP_HOME points at: run it with and without a
//! playing track to see both starts.

#[cfg(not(unix))]
fn main() {
    eprintln!("startup_bench needs a Unix pseudo-terminal");
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use clap::{Parser, ValueEnum};

    const ROOT: &str = env!("CARGO_MANIFEST_DIR");

    #[derive(Clone, Copy, ValueEnum)]
    enum Terminal {
        Kitty,
        Sixel,
        Plain,
        Silent,
    }

    impl Terminal {
        fn answer(self) -> &'static [u8] {
            match self {
                // Kitty graphics OK, primary attributes, 10x20 cells, then the status.
                Terminal::Kitty => b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c\x1b[6;20;10t\x1b[0n",
                // Sixel graphics (attribute 4), as foot answers.
                Terminal::Sixel => b"\x1b[?62;4;22c\x1b[6;20;10t\x1b[0n",
                Terminal::Plain => b"\x1b[?62;22c\x1b[6;20;10t\x1b[0n",
                Terminal::Silent => b"",
            }
        }

        fn name(self) -> &'static str {
            match self {
                Terminal::Kitty => "kitty",
                Terminal::Sixel => "sixel",
                Terminal::Plain => "plain",
                Terminal::Silent => "silent",
            }
        }
    }

    #[derive(Parser)]
    struct Args {
        #[arg(default_value_t = format!("{ROOT}/target/release/agentamp"))]
        binary: String,
        #[arg(long, default_value_t = 20)]
        runs: usize,
        #[arg(long, value_enum, default_value_t = Terminal::Kitty)]
        terminal: Terminal,
        #[arg(long, default_value = "140x40")]
        size: String,
    }

    /// Steps in the order they first appeared, each with its value in one run.
    #[derive(Default)]
    struct Times(Vec<(String, Option<f64>)>);

    impl Times {
        fn set(&mut self, key: String, value: Option<f64>) {
            match self.0.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => self.0.push((key, value)),
            }
        }

        fn set_default(&mut self, key: String, value: Option<f64>) {
            if !self.0.iter().any(|(k, _)| *k == key) {
                self.0.push((key, value));
            }
        }
    }

    fn now_us() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
    }

    fn ms(us: i64) -> Option<f64> {
        Some(us as f64 / 1000.0)
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn openpty(cols: u16, rows: u16) -> (File, OwnedFd) {
        let (mut main, mut child) = (0, 0);
        let mut size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: cols * 10, ws_ypixel: rows * 20 };
        // Mutable pointers, which macOS asks for and Linux takes as const.
        // SAFETY: valid pointers for the call; no name or termios asked for.
        let done = unsafe { libc::openpty(&mut main, &mut child, std::ptr::null_mut(), std::ptr::null_mut(), &raw mut size) };
        assert_eq!(done, 0, "openpty: {}", std::io::Error::last_os_error());
        // openpty leaves both ends to every child: the window would hold its
        // own terminal open.
        for fd in [main, child] {
            // SAFETY: a descriptor just opened.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        // SAFETY: both descriptors were just opened and nothing else owns them.
        unsafe { (File::from_raw_fd(main), OwnedFd::from_raw_fd(child)) }
    }

    fn run_once(binary: &str, terminal: Terminal, cols: u16, rows: u16, trace: &Path) -> Times {
        let _ = std::fs::remove_file(trace);
        let (mut main, child) = openpty(cols, rows);
        let mut command = Command::new(binary);
        command
            .stdin(child.try_clone().unwrap())
            .stdout(child.try_clone().unwrap())
            .stderr(child.try_clone().unwrap())
            .env("AGENTAMP_TRACE", trace)
            .env("TERM", "xterm-256color")
            // Inside tmux the image query is wrapped for tmux; this harness is the terminal.
            .env_remove("TMUX");
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            command.pre_exec(|| if libc::setsid() < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
        }
        let spawned = now_us();
        let mut process = command.spawn().unwrap();
        // The command keeps its copies of the terminal open.
        drop(command);
        drop(child);

        let mut first_byte = None;
        let mut output = Vec::new();
        let (mut answered, mut first_frame, mut prompt_at, mut quit_sent) = (false, None, None, false);
        let mut mark = 0;
        let mut pending: Vec<(i64, &[u8])> = Vec::new();
        let mut buffer = [0; 1 << 16];
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let mut ready = libc::pollfd { fd: main.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd, for as long as the call.
            if unsafe { libc::poll(&mut ready, 1, 50) } > 0 {
                match main.read(&mut buffer) {
                    Ok(n) if n > 0 => {
                        first_byte.get_or_insert(now_us());
                        output.extend_from_slice(&buffer[..n]);
                    }
                    _ => break,
                }
            }
            if !answered && contains(&output, b"\x1b[5n") {
                main.write_all(terminal.answer()).unwrap();
                answered = true;
            }
            // The first frame is in once the footer's last key hint has arrived.
            if first_frame.is_none() && contains(&output, b"quit") {
                first_frame = Some(now_us());
                main.write_all(b"/").unwrap();
                mark = output.len();
            }
            if first_frame.is_some() && prompt_at.is_none() && contains(&output[mark..], "Play ▸".as_bytes()) {
                let at = now_us();
                prompt_at = Some(at);
                // Let the snapshot and the cover arrive, then leave. Keep
                // reading meanwhile: a full pty would stall the window.
                pending = vec![(at + 600_000, b"\x1b"), (at + 700_000, b"q")];
            }
            while prompt_at.is_some() && !pending.is_empty() && now_us() >= pending[0].0 {
                main.write_all(pending.remove(0).1).unwrap();
                quit_sent = pending.is_empty();
            }
            if quit_sent && process.try_wait().unwrap().is_some() {
                break;
            }
        }
        let waited = Instant::now();
        while process.try_wait().unwrap().is_none() {
            if waited.elapsed() > Duration::from_secs(5) {
                let _ = process.kill();
                let _ = process.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(main);

        let mut times = Times::default();
        for line in std::fs::read_to_string(trace).unwrap_or_default().lines() {
            let Some((at, step)) = line.split_once(' ') else { continue };
            let at: i64 = at.parse().unwrap();
            let words: Vec<&str> = step.split(' ').collect();
            let mut key = if step.starts_with("frame") || step.starts_with("image") { words[0].to_string() } else { step.to_string() };
            if step.starts_with("frame") {
                key = format!("frame {} {}", words[1], words[3]);
                times.set(format!("{key} draw"), cost(words[2]));
            }
            if step.starts_with("image encoded") {
                key = format!("image encoded {}", words[2]);
                times.set(format!("{key} cost"), cost(words[3]));
            }
            times.set_default(key, ms(at - spawned));
        }
        times.set("terminal: first byte".into(), first_byte.and_then(|at| ms(at - spawned)));
        times.set("terminal: first frame in".into(), first_frame.and_then(|at| ms(at - spawned)));
        times.set("terminal: key to prompt".into(), prompt_at.zip(first_frame).and_then(|(prompt, frame)| ms(prompt - frame)));
        times.set("bytes to first frame".into(), first_frame.map(|_| mark as f64));
        times.set("bytes in total".into(), Some(output.len() as f64));
        times
    }

    /// A cost in the trace, `<µs>us`, in ms.
    fn cost(word: &str) -> Option<f64> {
        word.strip_suffix("us").and_then(|us| us.parse().ok()).and_then(ms)
    }

    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        if values.len() % 2 == 1 { values[middle] } else { (values[middle - 1] + values[middle]) / 2.0 }
    }

    pub fn main() -> ExitCode {
        let args = Args::parse();
        let Some((cols, rows)) = args.size.split_once('x').and_then(|(c, r)| Some((c.parse().ok()?, r.parse().ok()?))) else {
            eprintln!("--size takes COLSxROWS, like 140x40");
            return ExitCode::FAILURE;
        };
        let trace = PathBuf::from(ROOT).join("target/startup-bench.trace");

        let mut runs: HashMap<String, Vec<f64>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for _ in 0..args.runs {
            for (key, value) in run_once(&args.binary, args.terminal, cols, rows, &trace).0 {
                if !order.contains(&key) {
                    order.push(key.clone());
                }
                runs.entry(key).or_default().extend(value);
            }
        }
        let _ = std::fs::remove_file(&trace);
        println!("{} runs, {} terminal, {}: median / slowest", args.runs, args.terminal.name(), args.size);
        for key in order {
            let values = runs.get_mut(&key).unwrap();
            if values.is_empty() {
                println!("  {key:<34} never");
                continue;
            }
            let (median, slowest) = (median(values), values.iter().copied().fold(f64::MIN, f64::max));
            if key.starts_with("bytes") {
                println!("  {key:<34} {median:>9.0} {slowest:>9.0}");
            } else {
                println!("  {key:<34} {median:>9.2} {slowest:>9.2}  ms");
            }
        }
        ExitCode::SUCCESS
    }
}
