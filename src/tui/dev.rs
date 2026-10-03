//! Developer mode, built with `--features dev`: an overlay that switches the
//! spectrum's choices while the music plays and shows what the window costs.
//! F12 or ` opens and closes it; while it is open its keys come first.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};

use super::spectrum::{Analysis, Fall, Peaks, Scale, Tilt, Tuning};

const KEY: Color = Color::Rgb(30, 215, 96);
const NAME: Color = Color::Rgb(167, 167, 167);
const VALUE: Color = Color::Rgb(255, 255, 255);
const RATES: [u32; 8] = [15, 24, 30, 45, 60, 90, 120, 144];
const BARS: [(u16, u16); 4] = [(1, 0), (1, 1), (2, 1), (3, 1)];

pub struct Dev {
    pub open: bool,
    /// Where `y` writes the choices.
    dump: PathBuf,
    note: Option<String>,
    /// When the last second's frames were drawn, and how long each took.
    frames: VecDeque<(Instant, Duration)>,
    /// The window's and its terminal's CPU, as of the last reading.
    cpu: Option<Cpu>,
}

struct Cpu {
    at: Instant,
    own: f64,
    terminal: f64,
    shown: (f64, f64),
}

impl Dev {
    pub fn new(dump: PathBuf) -> Self {
        Self { open: false, dump, note: None, frames: VecDeque::new(), cpu: None }
    }

    /// Takes a key that is the overlay's, and says whether it was.
    pub fn key(&mut self, key: KeyEvent, tuning: &mut Tuning) -> bool {
        if matches!(key.code, KeyCode::F(12) | KeyCode::Char('`')) {
            self.open = !self.open;
            return true;
        }
        if self.open && key.code == KeyCode::Esc {
            self.open = false;
            return true;
        }
        if !self.open {
            return false;
        }
        let KeyCode::Char(c) = key.code else { return false };
        self.note = None;
        match c {
            'd' => tuning.scale = cycle(&[Scale::Autosens, Scale::Decibels], tuning.scale),
            't' => tuning.tilt = cycle(&[Tilt::Cava, Tilt::Flat], tuning.tilt),
            'f' => tuning.fall = cycle(&[Fall::Winamp, Fall::Gravity, Fall::Meter, Fall::Steady], tuning.fall),
            'p' => tuning.peaks = cycle(&[Peaks::Hold, Peaks::Winamp, Peaks::Off], tuning.peaks),
            'a' => tuning.analysis = cycle(&[Analysis::Dual, Analysis::Long, Analysis::Short], tuning.analysis),
            's' => tuning.smooth = !tuning.smooth,
            'm' => tuning.monstercat = !tuning.monstercat,
            'w' => (tuning.width, tuning.gap) = cycle(&BARS, (tuning.width, tuning.gap)),
            '+' | '=' => tuning.fps = RATES.into_iter().find(|&r| r > tuning.fps).unwrap_or(tuning.fps),
            '-' => tuning.fps = RATES.into_iter().rev().find(|&r| r < tuning.fps).unwrap_or(tuning.fps),
            ']' => tuning.delay_ms = (tuning.delay_ms + 10).min(500),
            '[' => tuning.delay_ms = tuning.delay_ms.saturating_sub(10),
            'r' => *tuning = Tuning::DEFAULT,
            'y' => {
                let written = std::fs::write(&self.dump, format!("{tuning:#?}\n"));
                self.note = Some(match written {
                    Ok(()) => format!("wrote {}", self.dump.display()),
                    Err(e) => format!("{}: {e}", self.dump.display()),
                });
            }
            _ => return false,
        }
        true
    }

    /// Counts a frame that took `took` to make and draw.
    pub fn frame(&mut self, now: Instant, took: Duration) {
        self.frames.push_back((now, took));
        while self.frames.front().is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(1)) {
            self.frames.pop_front();
        }
        let (own, terminal) = (cpu_seconds("self"), parent().map_or(0.0, |p| cpu_seconds(&p)));
        match &mut self.cpu {
            Some(cpu) if now.duration_since(cpu.at) >= Duration::from_secs(1) => {
                let span = now.duration_since(cpu.at).as_secs_f64();
                cpu.shown = ((own - cpu.own) / span * 100.0, (terminal - cpu.terminal) / span * 100.0);
                (cpu.at, cpu.own, cpu.terminal) = (now, own, terminal);
            }
            Some(_) => {}
            None => self.cpu = Some(Cpu { at: now, own, terminal, shown: (0.0, 0.0) }),
        }
    }

    pub fn draw(&self, frame: &mut Frame, tuning: &Tuning, sens: f32) {
        if !self.open {
            return;
        }
        let item = |key: &str, name: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("{key:>3} "), Style::new().fg(KEY).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{name:<11}"), Style::new().fg(NAME)),
                Span::styled(value, Style::new().fg(VALUE)),
            ])
        };
        let on = |b: bool| if b { "on" } else { "off" }.to_string();
        let scale = match tuning.scale {
            Scale::Decibels => "dB, 60 below full".into(),
            Scale::Autosens => format!("autosens ×{sens:.2}"),
        };
        let tilt = match tuning.tilt {
            Tilt::Flat => "flat",
            Tilt::Cava => "f^0.85",
        };
        let fall = match tuning.fall {
            Fall::Steady => "steady 1.6/s",
            Fall::Winamp => "Winamp 3/s",
            Fall::Gravity => "cava gravity",
            Fall::Meter => "meter 20 dB/1.7 s",
        };
        let peaks = match tuning.peaks {
            Peaks::Hold => "hold 0.35 s",
            Peaks::Winamp => "Winamp",
            Peaks::Off => "off",
        };
        let analysis = match tuning.analysis {
            Analysis::Short => "FFT 2048",
            Analysis::Long => "FFT 4096",
            Analysis::Dual => "8192 bass + 4096",
        };
        let count = self.frames.len();
        let mean = self.frames.iter().map(|(_, took)| took.as_secs_f64()).sum::<f64>() / count.max(1) as f64;
        let (own, terminal) = self.cpu.as_ref().map_or((0.0, 0.0), |cpu| cpu.shown);
        let mut lines = vec![
            item("d", "scale", scale),
            item("t", "tilt", tilt.into()),
            item("f", "fall", fall.into()),
            item("p", "peaks", peaks.into()),
            item("a", "analysis", analysis.into()),
            item("s", "smooth", on(tuning.smooth)),
            item("m", "monstercat", on(tuning.monstercat)),
            item("w", "bars", format!("{} wide, {} apart", tuning.width, tuning.gap)),
            item("+-", "fps", tuning.fps.to_string()),
            item("[]", "delay", format!("{} ms", tuning.delay_ms)),
            item("r", "defaults", String::new()),
            item("y", "write", String::new()),
            Line::raw(""),
            Line::styled(format!("{count} fps, {:.2} ms a frame", mean * 1000.0), Style::new().fg(VALUE)),
            Line::styled(
                if cfg!(target_os = "linux") {
                    format!("CPU {own:.1}% window, {terminal:.1}% terminal")
                } else {
                    "CPU is read on Linux only".into()
                },
                Style::new().fg(VALUE),
            ),
        ];
        if let Some(note) = &self.note {
            lines.push(Line::styled(note.clone(), Style::new().fg(NAME)));
        }
        let area = frame.area();
        let (width, height) = (44.min(area.width), (lines.len() as u16 + 2).min(area.height));
        let rect = Rect { x: area.width.saturating_sub(width + 2) / 2, y: 1.min(area.height), width, height };
        let block = Block::bordered()
            .title(" Developer mode · F12 closes ")
            .border_style(Style::new().fg(KEY))
            .padding(Padding::horizontal(1))
            .style(Style::new().bg(Color::Rgb(24, 24, 24)));
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

fn cycle<T: Copy + PartialEq>(all: &[T], now: T) -> T {
    all[(all.iter().position(|&x| x == now).map_or(0, |i| i + 1)) % all.len()]
}

/// Seconds of CPU a process has used, from Linux's /proc; 0 elsewhere.
fn cpu_seconds(pid: &str) -> f64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
    let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / clock_ticks()
}

fn parent() -> Option<String> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    stat.rsplit(')').next()?.split_whitespace().nth(1).map(str::to_string)
}

fn clock_ticks() -> f64 {
    #[cfg(unix)]
    {
        // SAFETY: sysconf only reads a constant.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 {
            return ticks as f64;
        }
    }
    100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn press(dev: &mut Dev, tuning: &mut Tuning, code: KeyCode) -> bool {
        dev.key(KeyEvent::new(code, KeyModifiers::NONE), tuning)
    }

    #[test]
    fn the_overlay_takes_its_keys_only_while_open() {
        let mut dev = Dev::new(PathBuf::from("unused"));
        let mut tuning = Tuning::DEFAULT;
        assert!(!press(&mut dev, &mut tuning, KeyCode::Char('s')), "closed, s is the player's");
        assert!(press(&mut dev, &mut tuning, KeyCode::F(12)));
        assert!(dev.open);
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('s')));
        assert!(!tuning.smooth);
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('w')));
        assert_eq!((tuning.width, tuning.gap), (3, 1));
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('+')));
        assert_eq!(tuning.fps, 90);
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('[')));
        assert_eq!(tuning.delay_ms, 30);
        assert!(!press(&mut dev, &mut tuning, KeyCode::Char(' ')), "space still plays and pauses");
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('r')));
        assert_eq!(tuning, Tuning::DEFAULT);
        assert!(press(&mut dev, &mut tuning, KeyCode::Esc));
        assert!(!dev.open);
        assert!(!press(&mut dev, &mut tuning, KeyCode::Esc), "closed, Esc is the player's");
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('`')));
        assert!(dev.open);
        assert!(press(&mut dev, &mut tuning, KeyCode::Char('`')));
        assert!(!dev.open);
    }

    #[test]
    fn the_choices_can_be_written_down() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/dev-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut dev = Dev::new(dir.join("tuning.txt"));
        let mut tuning = Tuning::DEFAULT;
        press(&mut dev, &mut tuning, KeyCode::F(12));
        press(&mut dev, &mut tuning, KeyCode::Char('f'));
        press(&mut dev, &mut tuning, KeyCode::Char('y'));
        let written = std::fs::read_to_string(dir.join("tuning.txt")).unwrap();
        assert!(written.contains("fall: Gravity"), "{written}");
        assert!(dev.note.as_ref().unwrap().starts_with("wrote "));
    }

    #[test]
    fn the_rates_step_through_the_usual_ones() {
        let mut dev = Dev::new(PathBuf::from("unused"));
        let mut tuning = Tuning { fps: 144, ..Tuning::DEFAULT };
        press(&mut dev, &mut tuning, KeyCode::F(12));
        press(&mut dev, &mut tuning, KeyCode::Char('+'));
        assert_eq!(tuning.fps, 144, "the top stays the top");
        for _ in 0..10 {
            press(&mut dev, &mut tuning, KeyCode::Char('-'));
        }
        assert_eq!(tuning.fps, 15);
    }
}
