//! The spectrum analyser: the sound the daemon taps, before the volume,
//! turned into bars that rise with the music and fall back in time.
//!
//! The choices come from Winamp's classic analyser, cava (the formulas
//! of its cavacore.c) and broadcast peak meters (IEC 60268-10). Every
//! motion is timed in seconds, so the frame rate changes how smooth the
//! bars look, never how fast they move.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::paths::Paths;
use crate::tap::KEPT;

/// Below this the dual analysis reads the long FFT, as cava does.
const BASS_CUT_HZ: f32 = 100.0;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// The dB scale's floor, under full scale.
const DB_RANGE: f32 = 60.0;
/// Sound older than this has stopped: the bars fall to silence.
const STALE: Duration = Duration::from_millis(200);
/// Below this every bar and cap is at rest.
const REST: f32 = 1e-3;

// The other choices are for developer mode to switch to.
#[cfg_attr(not(feature = "dev"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// 20·log10, the floor 60 dB under full scale.
    Decibels,
    /// Magnitudes as they are, times a sensitivity that drops when a bar
    /// overshoots and creeps up otherwise (cava's autosens).
    Autosens,
}

// The other choices are for developer mode to switch to.
#[cfg_attr(not(feature = "dev"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tilt {
    Flat,
    /// The treble lifted in proportion to frequency^0.85 (cava's EQ),
    /// since music carries less energy the higher it goes.
    Cava,
}

// The other choices are for developer mode to switch to.
#[cfg_attr(not(feature = "dev"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fall {
    /// 1.6 heights a second.
    Steady,
    /// Winamp: twelve sixteenths of a row per sixtieth of a second over
    /// fifteen rows, three heights a second.
    Winamp,
    /// cava's gravity: the fall speeds up the longer a bar drops.
    Gravity,
    /// A broadcast peak meter: 20 dB in 1.7 s (IEC 60268-10 Type I).
    Meter,
}

// The other choices are for developer mode to switch to.
#[cfg_attr(not(feature = "dev"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peaks {
    /// Hang for 0.35 s, then drop under gravity.
    Hold,
    /// Winamp: start dropping at once, a tenth faster every sixtieth.
    Winamp,
    Off,
}

// The other choices are for developer mode to switch to.
#[cfg_attr(not(feature = "dev"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Analysis {
    /// 2048 samples: 46 ms at 44.1 kHz, 22 Hz bins.
    Short,
    /// 4096 samples: 93 ms, 11 Hz bins.
    Long,
    /// cava's: 8192 samples below 100 Hz, 4096 above.
    Dual,
}

/// Everything that shapes the bars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    pub fps: u32,
    /// How far behind the newest sample the bars look, for the sound still
    /// on its way from the device to the speaker.
    pub delay_ms: u32,
    pub scale: Scale,
    pub tilt: Tilt,
    pub fall: Fall,
    pub peaks: Peaks,
    pub analysis: Analysis,
    /// Time smoothing, cava's noise_reduction 0.77, timed for 66 fps.
    pub smooth: bool,
    /// Each bar lifts its neighbours, falling off by 1.5 a bar (cava's
    /// "monstercat").
    pub monstercat: bool,
    /// A bar's width and the gap after it, in cells.
    pub width: u16,
    pub gap: u16,
}

impl Tuning {
    pub const DEFAULT: Tuning = Tuning {
        fps: 60,
        delay_ms: 40,
        scale: Scale::Autosens,
        tilt: Tilt::Cava,
        fall: Fall::Winamp,
        peaks: Peaks::Hold,
        analysis: Analysis::Dual,
        smooth: true,
        monstercat: false,
        width: 2,
        gap: 1,
    };

    pub fn period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.fps.max(1)))
    }

    /// How many bars fit across `width` cells.
    pub fn bars(&self, width: u16) -> usize {
        usize::from((width + self.gap) / (self.width + self.gap).max(1))
    }
}

/// What the window draws: each bar's height and its cap, 0 to 1.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spectrum {
    pub levels: Vec<f32>,
    pub peaks: Vec<f32>,
}

/// A radix-2 FFT with a Hann window, for magnitudes only. A full-scale
/// sine reads about 1.
struct Fft {
    window: Vec<f32>,
    /// e^(-2πik/n) for k below n/2.
    twiddles: Vec<(f32, f32)>,
    re: Vec<f32>,
    im: Vec<f32>,
    mags: Vec<f32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let tau = std::f32::consts::TAU;
        let window = (0..n).map(|i| 0.5 - 0.5 * (tau * i as f32 / (n - 1) as f32).cos()).collect();
        let twiddles = (0..n / 2).map(|k| (-tau * k as f32 / n as f32).sin_cos()).map(|(s, c)| (c, s)).collect();
        Self { window, twiddles, re: vec![0.0; n], im: vec![0.0; n], mags: vec![0.0; n / 2] }
    }

    fn len(&self) -> usize {
        self.re.len()
    }

    /// The spectrum of the last `len()` samples of `wave`.
    fn run(&mut self, wave: &[f32]) {
        let n = self.len();
        let wave = &wave[wave.len() - n..];
        let bits = n.trailing_zeros();
        for (i, (&sample, &weight)) in wave.iter().zip(&self.window).enumerate() {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            self.re[j] = sample * weight;
            self.im[j] = 0.0;
        }
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let stride = n / size;
            for k in 0..half {
                let (wr, wi) = self.twiddles[k * stride];
                let mut i = k;
                while i < n {
                    let j = i + half;
                    let tr = wr * self.re[j] - wi * self.im[j];
                    let ti = wr * self.im[j] + wi * self.re[j];
                    self.re[j] = self.re[i] - tr;
                    self.im[j] = self.im[i] - ti;
                    self.re[i] += tr;
                    self.im[i] += ti;
                    i += size;
                }
            }
            size *= 2;
        }
        let scale = n as f32 / 4.0;
        for (i, m) in self.mags.iter_mut().enumerate() {
            *m = (self.re[i] * self.re[i] + self.im[i] * self.im[i]).sqrt() / scale;
        }
    }

    /// The mean of the bins between two frequencies, as cava takes it; a
    /// span narrower than two bins reads between bins, so the low end
    /// slopes instead of stepping.
    fn band(&self, rate: u32, from_hz: f32, to_hz: f32) -> f32 {
        let hz = rate as f32 / self.len() as f32;
        let (from, to) = (from_hz / hz, to_hz / hz);
        let last = self.mags.len() - 2;
        if to - from < 2.0 {
            let at = ((from + to) / 2.0).min(last as f32);
            let (i, t) = (at as usize, at.fract());
            self.mags[i] * (1.0 - t) + self.mags[i + 1] * t
        } else {
            let first = (from as usize).min(last);
            let bins = &self.mags[first..(to as usize).clamp(first + 1, last + 1)];
            bins.iter().sum::<f32>() / bins.len() as f32
        }
    }
}

/// Everything between the samples and the bars.
pub struct Analyser {
    short: Fft,
    long: Fft,
    bass: Fft,
    level: Vec<f32>,
    smoothed: Vec<f32>,
    /// cava's gravity: how far into its fall each bar is, and where from.
    falling: Vec<f32>,
    fall_from: Vec<f32>,
    /// Each bar's cap, how long it has hung, and how fast it drops.
    peak: Vec<f32>,
    peak_age: Vec<f32>,
    peak_speed: Vec<f32>,
    pub sens: f32,
    /// cava ramps sensitivity up fast until the first overshoot.
    sens_init: bool,
}

impl Default for Analyser {
    fn default() -> Self {
        Self {
            short: Fft::new(2048),
            long: Fft::new(4096),
            bass: Fft::new(8192),
            level: Vec::new(),
            smoothed: Vec::new(),
            falling: Vec::new(),
            fall_from: Vec::new(),
            peak: Vec::new(),
            peak_age: Vec::new(),
            peak_speed: Vec::new(),
            sens: 1.0,
            sens_init: true,
        }
    }
}

impl Analyser {
    /// Moves the bars on by `dt` seconds towards the sound in `wave`, the
    /// last `KEPT` samples at `rate`. `sounding` is false in silence, which
    /// leaves the sensitivity alone.
    pub fn step(&mut self, wave: &[f32], rate: u32, count: usize, tuning: &Tuning, dt: f32, sounding: bool) {
        if self.level.len() != count {
            for v in [
                &mut self.level,
                &mut self.smoothed,
                &mut self.falling,
                &mut self.fall_from,
                &mut self.peak,
                &mut self.peak_age,
                &mut self.peak_speed,
            ] {
                *v = vec![0.0; count];
            }
        }
        if count == 0 {
            return;
        }
        match tuning.analysis {
            Analysis::Short => self.short.run(wave),
            Analysis::Long => self.long.run(wave),
            Analysis::Dual => {
                self.long.run(wave);
                self.bass.run(wave);
            }
        }
        // cava times its filters for its own frame rate (66 / fps); this
        // is that ratio for the time since the last frame.
        let framerate_mod = (dt * 66.0).max(1e-3);
        let (low, high) = (LOW_HZ.ln(), HIGH_HZ.ln());
        let edge = |b: usize| (low + (high - low) * b as f32 / count as f32).exp();
        let mut target: Vec<f32> = (0..count)
            .map(|b| {
                let (from, to) = (edge(b), edge(b + 1));
                let fft = match tuning.analysis {
                    Analysis::Short => &self.short,
                    Analysis::Long => &self.long,
                    Analysis::Dual if to <= BASS_CUT_HZ => &self.bass,
                    Analysis::Dual => &self.long,
                };
                let mut v = fft.band(rate.max(1), from, to);
                if tuning.tilt == Tilt::Cava {
                    v *= (to / 400.0).powf(0.85);
                }
                match tuning.scale {
                    Scale::Decibels => ((20.0 * v.max(1e-9).log10() + DB_RANGE) / DB_RANGE).max(0.0),
                    Scale::Autosens => v * self.sens,
                }
            })
            .collect();
        if tuning.monstercat {
            let raw = target.clone();
            for (z, &value) in raw.iter().enumerate() {
                for (m, t) in target.iter_mut().enumerate() {
                    *t = t.max(value / 1.5f32.powi((z as i32 - m as i32).abs()));
                }
            }
        }
        if tuning.smooth {
            // An exponential version of cava's integral filter: as much of
            // the last value as noise_reduction 0.77 keeps at 66 fps.
            let keep = 0.77f32.powf(framerate_mod);
            for (s, t) in self.smoothed.iter_mut().zip(&mut target) {
                *s = *s * keep + *t * (1.0 - keep);
                *t = *s;
            }
        } else {
            self.smoothed.copy_from_slice(&target);
        }
        if tuning.scale == Scale::Autosens && sounding {
            if target.iter().any(|&t| t > 1.0) {
                self.sens *= 1.0 - 0.02 * framerate_mod;
                self.sens_init = false;
            } else if target.iter().any(|&t| t > 0.01) {
                self.sens *= 1.0 + if self.sens_init { 0.1 } else { 0.001 } * framerate_mod;
            }
            self.sens = self.sens.clamp(0.05, 500.0);
        }

        for (i, &target) in target.iter().enumerate() {
            let t = target.min(1.0);
            let level = &mut self.level[i];
            if t >= *level {
                *level = t;
                self.falling[i] = 0.0;
                self.fall_from[i] = t;
            } else {
                *level = match tuning.fall {
                    Fall::Steady => (*level - 1.6 * dt).max(t),
                    Fall::Winamp => (*level - 3.0 * dt).max(t),
                    Fall::Gravity => {
                        // cava: peak·(1 − fall²·gravity), fall += 0.028 a frame.
                        let gravity = framerate_mod.powf(2.5) * 2.0 / 0.77;
                        self.falling[i] += 0.028 * framerate_mod;
                        (self.fall_from[i] * (1.0 - self.falling[i] * self.falling[i] * gravity)).max(t)
                    }
                    Fall::Meter => match tuning.scale {
                        Scale::Decibels => (*level - 20.0 / 1.7 / DB_RANGE * dt).max(t),
                        // 20 dB is a tenth in amplitude.
                        Scale::Autosens => (*level * 0.1f32.powf(dt / 1.7)).max(t),
                    },
                };
            }
            let level = *level;
            match tuning.peaks {
                Peaks::Off => self.peak[i] = 0.0,
                Peaks::Hold => {
                    if level >= self.peak[i] {
                        (self.peak[i], self.peak_age[i], self.peak_speed[i]) = (level, 0.0, 0.0);
                    } else if self.peak_age[i] < 0.35 {
                        self.peak_age[i] += dt;
                    } else {
                        self.peak_speed[i] += 2.4 * dt;
                        self.peak[i] = (self.peak[i] - self.peak_speed[i] * dt).max(level);
                    }
                }
                Peaks::Winamp => {
                    // Three 256ths of a row each sixtieth, over fifteen
                    // rows, a tenth faster every sixtieth.
                    if level >= self.peak[i] {
                        (self.peak[i], self.peak_speed[i]) = (level, 3.0 / 256.0 / 15.0);
                    } else {
                        self.peak_speed[i] *= 1.1f32.powf(dt * 60.0);
                        self.peak[i] = (self.peak[i] - self.peak_speed[i] * dt * 60.0).max(level);
                    }
                }
            }
        }
    }

    /// Whether every bar and cap has come to rest, so nothing needs drawing.
    pub fn settled(&self) -> bool {
        self.level.iter().chain(&self.peak).chain(&self.smoothed).all(|&v| v < REST)
    }

    pub fn spectrum(&self) -> Spectrum {
        Spectrum { levels: self.level.clone(), peaks: self.peak.clone() }
    }
}

/// The sound as heard from the daemon, newest last.
#[derive(Default)]
pub struct Heard {
    samples: VecDeque<f32>,
    rate: u32,
    /// How many samples have arrived, and when the last did.
    received: u64,
    at: Option<Instant>,
}

impl Heard {
    fn add(&mut self, rate: u32, samples: &[f32], now: Instant) {
        if samples.is_empty() {
            return;
        }
        if rate != self.rate {
            self.samples.clear();
            self.rate = rate;
        }
        let over = (self.samples.len() + samples.len()).saturating_sub(KEPT).min(self.samples.len());
        self.samples.drain(..over);
        self.samples.extend(&samples[samples.len().saturating_sub(KEPT)..]);
        self.received += samples.len() as u64;
        self.at = Some(now);
    }

    /// Whether sound came in lately.
    pub fn sounding(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now.duration_since(at) < STALE)
    }

    /// Fills `wave` with the `KEPT` samples that end `delay` before what is
    /// playing now, and gives their rate. The sound comes in bursts, as
    /// the device asks for it, so the playing point runs on smoothly from
    /// the last burst; it never passes the newest sample until the sound
    /// stops, then it runs into silence.
    pub fn wave(&self, now: Instant, delay: Duration, wave: &mut [f32]) -> u32 {
        let rate = self.rate.max(1);
        let ran = self.at.map_or(0, |at| (now.duration_since(at).as_secs_f64() * f64::from(rate)) as u64);
        let behind = (delay.as_secs_f64() * f64::from(rate)) as u64;
        let ahead = self.received + ran;
        let end = if self.sounding(now) { ahead.min(self.received + behind) } else { ahead }.saturating_sub(behind);
        let oldest = self.received - self.samples.len() as u64;
        let first = end.saturating_sub(wave.len() as u64);
        for (i, slot) in wave.iter_mut().enumerate() {
            let at = first + i as u64;
            *slot = if (oldest..self.received).contains(&at) { self.samples[(at - oldest) as usize] } else { 0.0 };
        }
        rate
    }
}

/// Listens to the daemon's sound for as long as the window is open, and
/// tries again each second while there is no daemon to hear.
pub fn listen(paths: Paths, heard: Arc<Mutex<Heard>>) {
    loop {
        if let Err(e) = listen_once(&paths, &heard) {
            log::debug!("listening: {e}");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn listen_once(paths: &Paths, heard: &Mutex<Heard>) -> std::io::Result<()> {
    let mut stream = std::os::unix::net::UnixStream::connect(paths.socket())?;
    stream.write_all(b"{\"cmd\":\"listen\"}\n")?;
    let mut reader = BufReader::new(stream);
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    if !answer.starts_with("{\"ok\":true") {
        return Err(std::io::Error::other(answer.trim().to_string()));
    }
    let mut samples = Vec::new();
    loop {
        samples.clear();
        let rate = decode(&mut reader, &mut samples)?;
        heard.lock().expect("heard").add(rate, &samples, Instant::now());
    }
}

/// Reads one chunk of the daemon's sound: its rate, with its samples
/// appended to `out`.
fn decode(reader: &mut impl Read, out: &mut Vec<f32>) -> std::io::Result<u32> {
    let mut head = [0u8; 8];
    reader.read_exact(&mut head)?;
    let rate = u32::from_le_bytes(head[..4].try_into().expect("four bytes"));
    let count = u32::from_le_bytes(head[4..].try_into().expect("four bytes")) as usize;
    if count > KEPT {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "a chunk larger than the tap keeps"));
    }
    let mut body = vec![0u8; count * 4];
    reader.read_exact(&mut body)?;
    out.extend(body.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
    Ok(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, rate: u32, amplitude: f32) -> Vec<f32> {
        (0..KEPT).map(|i| amplitude * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin()).collect()
    }

    fn run(analyser: &mut Analyser, wave: &[f32], count: usize, frames: usize) {
        for _ in 0..frames {
            analyser.step(wave, 44_100, count, &Tuning::DEFAULT, 1.0 / 60.0, true);
        }
    }

    #[test]
    fn a_full_scale_sine_reads_about_one() {
        let mut fft = Fft::new(4096);
        // On a bin, so the window's loss is all there is.
        let hz = 44_100.0 / 4096.0 * 100.0;
        fft.run(&sine(hz, 44_100, 1.0));
        let peak = fft.mags.iter().cloned().fold(0.0, f32::max);
        assert!((0.9..1.1).contains(&peak), "{peak}");
        assert_eq!(fft.mags.iter().position(|&m| m == peak), Some(100));
    }

    #[test]
    fn a_tone_lifts_its_own_bar() {
        let mut analyser = Analyser::default();
        run(&mut analyser, &sine(1000.0, 44_100, 0.5), 11, 120);
        let levels = analyser.spectrum().levels;
        let loudest = levels.iter().cloned().fold(0.0, f32::max);
        let bar = levels.iter().position(|&l| l == loudest).unwrap();
        // 40 Hz to 16 kHz in 11 even steps of log frequency: 1 kHz is the sixth.
        assert_eq!(bar, 5, "{levels:?}");
        assert!(loudest > 0.5, "autosens brings it up: {levels:?}");
        assert!(levels[0] < loudest / 4.0 && levels[10] < loudest / 4.0, "{levels:?}");
    }

    #[test]
    fn a_bass_note_reads_the_long_analysis() {
        let mut analyser = Analyser::default();
        run(&mut analyser, &sine(55.0, 44_100, 0.5), 20, 120);
        let levels = analyser.spectrum().levels;
        let loudest = levels.iter().cloned().fold(0.0, f32::max);
        assert!(levels.iter().position(|&l| l == loudest).unwrap() <= 1, "{levels:?}");
    }

    #[test]
    fn silence_brings_everything_to_rest_in_time() {
        let mut analyser = Analyser::default();
        run(&mut analyser, &sine(1000.0, 44_100, 0.5), 11, 60);
        assert!(!analyser.settled());
        let silence = vec![0.0; KEPT];
        // Winamp's fall is three heights a second, the caps hang 0.35 s
        // then drop: everything is down within two seconds.
        for _ in 0..120 {
            analyser.step(&silence, 44_100, 11, &Tuning::DEFAULT, 1.0 / 60.0, false);
        }
        assert!(analyser.settled(), "{:?}", analyser.spectrum());
    }

    #[test]
    fn the_fall_takes_the_same_time_at_any_frame_rate() {
        let fall = |fps: u32| {
            let mut analyser = Analyser::default();
            run(&mut analyser, &sine(1000.0, 44_100, 0.5), 11, 60);
            let silence = vec![0.0; KEPT];
            let dt = 1.0 / fps as f32;
            for _ in 0..fps / 4 {
                analyser.step(&silence, 44_100, 11, &Tuning { fps, ..Tuning::DEFAULT }, dt, false);
            }
            analyser.spectrum().levels[5]
        };
        let (slow, fast) = (fall(30), fall(120));
        assert!((slow - fast).abs() < 0.05, "a quarter second in: {slow} at 30 fps, {fast} at 120");
    }

    #[test]
    fn bars_can_be_made_before_any_sound_has_come() {
        let mut analyser = Analyser::default();
        let mut wave = vec![0.0; KEPT];
        let rate = Heard::default().wave(Instant::now(), Duration::ZERO, &mut wave);
        analyser.step(&wave, rate, 11, &Tuning::DEFAULT, 1.0 / 60.0, false);
        assert!(analyser.settled());
    }

    #[test]
    fn bars_fit_the_width_with_their_gaps() {
        let tuning = Tuning::DEFAULT;
        // 2 wide and 1 apart: the last bar needs no gap after it.
        assert_eq!(tuning.bars(32), 11);
        assert_eq!(tuning.bars(2), 1);
        assert_eq!(tuning.bars(1), 0);
    }

    #[test]
    fn the_wave_runs_on_between_bursts_and_into_silence() {
        let t0 = Instant::now();
        let mut heard = Heard::default();
        heard.add(1000, &(0..2000).map(|i| i as f32).collect::<Vec<_>>(), t0);
        let mut wave = vec![0.0; 4];
        // Right after a burst the bars look the delay behind its end.
        assert_eq!(heard.wave(t0, Duration::from_millis(100), &mut wave), 1000);
        assert_eq!(wave, [1896.0, 1897.0, 1898.0, 1899.0]);
        // 50 ms later the playing point has run on 50 samples.
        heard.wave(t0 + Duration::from_millis(50), Duration::from_millis(100), &mut wave);
        assert_eq!(wave, [1946.0, 1947.0, 1948.0, 1949.0]);
        // It never passes the newest sample while sound comes in.
        heard.wave(t0 + Duration::from_millis(150), Duration::from_millis(100), &mut wave);
        assert_eq!(wave, [1996.0, 1997.0, 1998.0, 1999.0]);
        // Once the sound has stopped, it runs into silence.
        assert!(!heard.sounding(t0 + STALE));
        heard.wave(t0 + Duration::from_millis(300), Duration::from_millis(100), &mut wave);
        assert_eq!(wave, [0.0; 4]);
    }

    #[test]
    fn chunks_survive_the_socket() {
        let mut wire = Vec::new();
        crate::tap::encode(48_000, &[0.25, -1.0], &mut wire);
        crate::tap::encode(48_000, &[], &mut wire);
        let mut reader = &wire[..];
        let mut samples = Vec::new();
        assert_eq!(decode(&mut reader, &mut samples).unwrap(), 48_000);
        assert_eq!(decode(&mut reader, &mut samples).unwrap(), 48_000);
        assert_eq!(samples, vec![0.25, -1.0]);
        assert!(decode(&mut reader, &mut samples).is_err(), "the end of the stream is an error");
    }
}
