//! A real daemon, with silent audio, under a queue of 100 000 files and
//! clients asking it at once: how long `play` and `add` take with 10 000
//! files each, what the queue holds in memory, and what eight windows
//! polling it while an agent adds to it wait for their answers.
//!
//! Slow and loud, so it is left out of `cargo test`:
//!
//! ```sh
//! cargo test --release --test stress -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use agentamp::ipc::{self, Request};

const FILES: usize = 10_000;
const ADDS: usize = 9;
const WINDOWS: usize = 8;
const POLLING: Duration = Duration::from_secs(10);

/// A WAV file of silence: `samples` at 8 kHz, mono.
fn wav(path: &Path, samples: u32) {
    let data = samples * 2;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.resize(out.len() + data as usize, 0);
    std::fs::write(path, out).unwrap();
}

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    fn start(home: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_agentamp"))
            .arg("daemon")
            .env("AGENTAMP_HOME", home)
            .env("AGENTAMP_AUDIO", "null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let socket = home.join("run/agentamp.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while ipc::call(&socket, &Request::Status).is_err() {
            assert!(Instant::now() < deadline, "the daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child, socket }
    }

    fn ask(&self, request: &Request) -> serde_json::Value {
        ipc::call(&self.socket, request).unwrap()
    }

    /// Its resident memory in MiB, where /proc says.
    fn memory(&self) -> Option<f64> {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id())).ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        Some(line.split_whitespace().nth(1)?.parse::<f64>().ok()? / 1024.0)
    }

    /// Its CPU time so far in clock ticks, where /proc says.
    fn ticks(&self) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id())).ok()?;
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = ipc::call(&self.socket, &Request::Shutdown);
        let _ = self.child.wait();
    }
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = f();
    (out, start.elapsed())
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn mib(memory: Option<f64>) -> String {
    memory.map_or("n/a".into(), |m| format!("{m:.0} MiB"))
}

/// Eight windows poll as fast as they can, the way each asks every 500 ms,
/// while an agent adds a track every 100 ms and, when `whole`, reads the
/// whole queue after each. Prints what they waited; gives the windows' p99.
fn poll(daemon: &Daemon, one: &str, whole: bool) -> Duration {
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(WINDOWS + 2));
    let socket = Arc::new(daemon.socket.clone());
    let windows: Vec<_> = (0..WINDOWS)
        .map(|_| {
            let (stop, start, socket) = (stop.clone(), start.clone(), socket.clone());
            std::thread::spawn(move || {
                let mut waits = Vec::new();
                start.wait();
                while !stop.load(Ordering::Relaxed) {
                    let (_, took) = timed(|| {
                        ipc::call(&socket, &Request::Status).unwrap();
                        ipc::call(&socket, &Request::Queue { offset: 0, count: Some(200) }).unwrap();
                    });
                    waits.push(took);
                }
                waits
            })
        })
        .collect();
    let agent = {
        let (stop, start, socket, one) = (stop.clone(), start.clone(), socket.clone(), one.to_string());
        std::thread::spawn(move || {
            let (mut adds, mut wholes) = (Vec::new(), Vec::new());
            start.wait();
            while !stop.load(Ordering::Relaxed) {
                adds.push(timed(|| ipc::call(&socket, &Request::Add { target: one.clone(), next: true }).unwrap()).1);
                if whole {
                    wholes.push(timed(|| ipc::call(&socket, &Request::Queue { offset: 0, count: None }).unwrap()).1);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            (adds, wholes)
        })
    };
    let before = daemon.ticks();
    start.wait();
    std::thread::sleep(POLLING);
    stop.store(true, Ordering::Relaxed);
    let ticks = daemon.ticks().zip(before).map(|(after, before)| after - before);
    let mut waits: Vec<Duration> = windows.into_iter().flat_map(|w| w.join().unwrap()).collect();
    let (mut adds, mut wholes) = agent.join().unwrap();
    waits.sort();
    adds.sort();
    wholes.sort();

    println!("{}:", if whole { "an agent adding and reading the whole queue" } else { "an agent adding" });
    println!(
        "  {WINDOWS} windows, status + 200 of the queue, {} asks in {POLLING:?}: median {:.2?}, p99 {:.2?}, slowest {:.2?}",
        waits.len(),
        percentile(&waits, 0.5),
        percentile(&waits, 0.99),
        waits[waits.len() - 1]
    );
    println!("  one track added next {} times: median {:.2?}, slowest {:.2?}", adds.len(), percentile(&adds, 0.5), adds[adds.len() - 1]);
    if whole {
        println!("  the whole queue read {} times: median {:.2?}, slowest {:.2?}", wholes.len(), percentile(&wholes, 0.5), wholes[wholes.len() - 1]);
    }
    if let Some(ticks) = ticks {
        println!("  the daemon's CPU: {ticks} ticks in {POLLING:?}");
    }
    percentile(&waits, 0.99)
}

#[test]
#[ignore = "slow: run with --ignored, in release"]
fn a_hundred_thousand_tracks_and_eight_windows() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stress");
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    // The first plays the whole test; the rest are 50 ms, to keep the disk small.
    wav(&music.join("00000.wav"), 8000 * 600);
    for i in 1..FILES {
        wav(&music.join(format!("{i:05}.wav")), 400);
    }
    let daemon = Daemon::start(&root.join("home"));
    let idle = daemon.memory();
    let folder = music.to_str().unwrap().to_string();

    let (_, play) = timed(|| daemon.ask(&Request::Play { target: folder.clone() }));
    let after_play = daemon.memory();
    let mut adds = Vec::new();
    for _ in 0..ADDS {
        adds.push(timed(|| daemon.ask(&Request::Add { target: folder.clone(), next: false })).1);
    }
    adds.sort();
    let status = daemon.ask(&Request::Status);
    let queued = status["queue_len"].as_u64().unwrap() as usize;
    assert_eq!(queued, FILES * (ADDS + 1) - 1, "every file is queued");
    let full = daemon.memory();

    println!("play, {FILES} files:                      {play:.2?}");
    println!("add, {FILES} files, median of {ADDS}:          {:.2?} (slowest {:.2?})", adds[ADDS / 2], adds[ADDS - 1]);
    println!("memory: idle {}, {FILES} queued {}, {queued} queued {}", mib(idle), mib(after_play), mib(full));
    let one = music.join("00001.wav").to_str().unwrap().to_string();
    let adding = poll(&daemon, &one, false);
    // Read whole, 100 000 tracks hold up everything the daemon answers;
    // only `agentamp queue` asks for that. Printed, not judged.
    poll(&daemon, &one, true);

    // A window redraws every 500 ms: a look must fit well inside that,
    // while an agent adds to the queue.
    assert!(adding < Duration::from_millis(50), "the windows waited {adding:.2?}");
    drop(daemon);
    std::fs::remove_dir_all(&root).unwrap();
}
