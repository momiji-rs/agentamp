//! Times the window's start in real terminals, on a Hyprland desktop.
//!
//!     cargo run --release --example terminal_bench -- [--runs 10] [--terminals ghostty,ghostty-warm,foot,alacritty] [BINARY]
//!
//! Each run opens a terminal running the window on a headless output, out
//! of the user's sight (workspace 6, silently), and reads its AGENTAMP_TRACE
//! marks. Times are milliseconds from asking Hyprland to launch the terminal:
//! the median and the slowest run. The terminal's own start counts, since a
//! user waits for it too; what it takes the terminal to put the last frame on
//! glass is not measured. `cover drawn` is the last frame with the cover,
//! after any resize the terminal made once the window was up.
//!
//! `ghostty-warm` opens each window in a Ghostty that is already running,
//! through its `new-window-command` D-Bus action: `ghostty -e` always starts
//! a new process, single instance or not. The harness starts that Ghostty
//! itself, under its own class, and leaves it running.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const OUTPUT: &str = "agentamp-shot";
const CLASS: &str = "agentamp.shot";
// The running Ghostty's class: one per application, so the cold runs of
// `ghostty` must not share it.
const WARM: &str = "agentamp.warm";
const SINGLE: &str = "--gtk-single-instance=true";
// How long the trace must stay quiet after the cover is drawn: a terminal
// may resize the window after its first frame, and the frame after the
// last resize is the one the user sees.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Parser)]
struct Args {
    #[arg(default_value_t = format!("{ROOT}/target/release/agentamp"))]
    binary: String,
    #[arg(long, default_value_t = 10)]
    runs: usize,
    #[arg(long, default_value = "ghostty,foot,alacritty")]
    terminals: String,
}

/// The shell command that opens `terminal` on the window, and the class
/// its window gets. A running Ghostty starts the window's command in its
/// own environment, so the warm command carries the variables itself.
fn launch(terminal: &str, env: &[(&str, String)], binary: &str) -> Option<(String, &'static str)> {
    let cmd = quote(binary);
    let (command, class) = match terminal {
        "ghostty" => (format!("ghostty --class={CLASS} -e {cmd}"), CLASS),
        "ghostty-warm" => {
            let mut argv = vec!["-e".to_string(), "env".to_string()];
            argv.extend(env.iter().map(|(name, value)| format!("{name}={value}")));
            argv.push(binary.to_string());
            let argv = format!("{} {}", argv.len(), argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "));
            let path = WARM.replace('.', "/");
            (format!("busctl --user call -- {WARM} /{path} org.gtk.Actions Activate 'sava{{sv}}' new-window-command 1 as {argv} 0"), WARM)
        }
        "foot" => (format!("foot --app-id={CLASS} {cmd}"), CLASS),
        "alacritty" => (format!("alacritty --class {CLASS} -e {cmd}"), CLASS),
        _ => return None,
    };
    // Hyprland starts the terminal with its own environment, not ours. Only
    // the value is quoted: a quoted name is no assignment to the shell.
    let env: Vec<String> = env.iter().map(|(name, value)| format!("{name}={}", quote(value))).collect();
    Some((format!("{} {command}", env.join(" ")), class))
}

/// A word for the shell, quoted only when it needs to be.
fn quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !word.is_empty() && word.chars().all(safe) { word.to_string() } else { format!("'{}'", word.replace('\'', r#"'"'"'"#)) }
}

/// A Lua string literal.
fn lua_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn hypr(args: &[&str]) -> String {
    let out = Command::new("hyprctl").args(args).output().expect("hyprctl");
    assert!(out.status.success(), "hyprctl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn lua(code: &str) -> String {
    hypr(&["eval", code])
}

fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
}

fn setup(terminals: &[&str]) {
    if !hypr(&["monitors", "all"]).contains(OUTPUT) {
        hypr(&["output", "create", "headless", OUTPUT]);
    }
    for class in [CLASS, WARM] {
        lua(&format!("hl.window_rule({{ match = {{ class = '{}' }}, workspace = '6 silent' }})", class.replace('.', "[.]")));
    }
    let running = Command::new("pgrep").args(["-f", &format!("class={WARM}")]).output().is_ok_and(|out| out.status.success());
    if terminals.contains(&"ghostty-warm") && !running {
        lua(&format!("hl.exec_cmd('ghostty --class={WARM} {SINGLE} --initial-window=false --quit-after-last-window-closed=false')"));
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn close(class: &str) {
    lua(&format!("hl.dispatch(hl.dsp.window.close({{ window = 'class:{class}' }}))"));
    for _ in 0..100 {
        if !hypr(&["clients"]).contains(&format!("class: {class}")) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The last frame after the cover was applied, if any.
fn drawn<'a>(lines: &[&'a str]) -> Option<&'a str> {
    let applied = lines.iter().position(|line| line.ends_with("applied art"))?;
    lines[applied..].iter().rev().find(|line| line.split(' ').nth(1) == Some("frame")).copied()
}

fn ms(us: i64) -> f64 {
    us as f64 / 1000.0
}

/// A cost in the trace, `<µs>us`, in ms.
fn cost(word: &str) -> Option<f64> {
    word.strip_suffix("us")?.parse().ok().map(ms)
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

fn run_once(binary: &str, terminal: &str, trace: &Path) -> Times {
    let _ = std::fs::remove_file(trace);
    let mut env = vec![("AGENTAMP_TRACE", trace.display().to_string())];
    if let Some(home) = std::env::var_os("AGENTAMP_HOME") {
        env.push(("AGENTAMP_HOME", std::path::absolute(home).unwrap().display().to_string()));
    }
    let Some((command, class)) = launch(terminal, &env, binary) else {
        panic!("no terminal called {terminal}: ghostty, ghostty-warm, foot or alacritty");
    };
    let launched = now_us();
    lua(&format!("hl.exec_cmd({})", lua_string(&command)));
    let (mut text, mut changed) = (String::new(), Instant::now());
    let deadline = changed + Duration::from_secs(6);
    while Instant::now() < deadline {
        let now = std::fs::read_to_string(trace).unwrap_or_default();
        if now != text {
            (text, changed) = (now, Instant::now());
        }
        if drawn(&text.lines().collect::<Vec<_>>()).is_some() && changed.elapsed() > SETTLE {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    close(class);
    let lines: Vec<&str> = text.lines().collect();
    let mut times = Times::default();
    for line in &lines {
        let Some((at, step)) = line.split_once(' ') else { continue };
        let at: i64 = at.parse().unwrap();
        let words: Vec<&str> = step.split(' ').collect();
        let step = match words[0] {
            "frame" => {
                let step = format!("frame {} {}", words[1], words[3]);
                times.set(format!("{step} draw"), cost(words[2]));
                step
            }
            "image" => {
                let step = format!("image {}", words[2]);
                times.set(format!("{step} cost"), cost(words[3]));
                step
            }
            "resize" => "resize".to_string(),
            _ => step.to_string(),
        };
        times.set_default(step, Some(ms(at - launched)));
    }
    let resizes = lines.iter().filter(|line| line.split(' ').nth(1) == Some("resize")).count();
    times.set("resizes".into(), Some(resizes as f64));
    let last = drawn(&lines).map(|line| line.split(' ').next().unwrap().parse::<i64>().unwrap());
    times.set("cover drawn".into(), last.map(|at| ms(at - launched)));
    times
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 1 { values[middle] } else { (values[middle - 1] + values[middle]) / 2.0 }
}

/// The user's runtime directory, which Tailscale SSH leaves out of the environment.
fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return dir.into();
    }
    #[cfg(unix)]
    // SAFETY: getuid cannot fail.
    return PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
    #[cfg(not(unix))]
    panic!("no XDG_RUNTIME_DIR");
}

fn main() {
    let args = Args::parse();
    // Hyprland starts the terminal in its own directory, not ours.
    let binary = std::path::absolute(&args.binary).unwrap().display().to_string();
    let trace = PathBuf::from(ROOT).join("target/terminal-bench.trace");
    // Run from an SSH session, find the desktop's Wayland and Hyprland.
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        // SAFETY: no other thread runs yet.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", "wayland-1") };
    }
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        let runtime = runtime_dir().join("hypr");
        let mut instances: Vec<_> = std::fs::read_dir(runtime).expect("no Hyprland running").flatten().map(|e| e.file_name()).collect();
        instances.sort();
        // SAFETY: no other thread runs yet.
        unsafe { std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", &instances[0]) };
    }
    let terminals: Vec<&str> = args.terminals.split(',').collect();
    setup(&terminals);
    for terminal in terminals {
        let mut runs: HashMap<String, Vec<f64>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for _ in 0..args.runs {
            for (key, value) in run_once(&binary, terminal, &trace).0 {
                if !order.contains(&key) {
                    order.push(key.clone());
                }
                runs.entry(key).or_default().extend(value);
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        println!("{terminal}, {} runs: ms from launch, median / slowest", args.runs);
        for key in order {
            let values = runs.get_mut(&key).unwrap();
            if values.is_empty() {
                println!("  {key:<28} never");
                continue;
            }
            let count = values.len();
            let (median, slowest) = (median(values), values.iter().copied().fold(f64::MIN, f64::max));
            if key == "resizes" {
                println!("  {key:<28} {median:>9.0} {slowest:>9.0}");
            } else {
                println!("  {key:<28} {median:>9.2} {slowest:>9.2}  ({count})");
            }
        }
    }
    let _ = std::fs::remove_file(&trace);
}
