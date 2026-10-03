//! Audio output for files: local music and downloaded YouTube audio.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use rodio::{Decoder, OutputStreamBuilder, Sink, mixer::Mixer};

use crate::model::Track;
use crate::tap::Tap;

/// Called once when a loaded track finishes or is stopped.
pub type OnEnd = Box<dyn FnOnce() + Send>;

pub trait Deck: Send {
    /// Starts `track` from the beginning, replacing whatever played.
    fn load(&mut self, track: &Track, on_end: OnEnd) -> Result<()>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn stop(&mut self);
    fn seek(&mut self, position_ms: u32) -> Result<()>;
    fn set_volume(&mut self, percent: u8);
    fn position_ms(&self) -> u32;
    /// Gives the audio device back while nothing plays.
    fn release(&mut self) {
        self.stop();
    }
}

/// Plays through the default output device with rodio.
pub struct RodioDeck {
    output: Option<Output>,
    sink: Option<Arc<Sink>>,
    volume: f32,
    tap: Arc<Tap>,
}

/// The default output device, open.
pub struct Output {
    pub mixer: Mixer,
    /// Dropping this ends the thread that owns the device stream.
    _hold: std::sync::mpsc::Sender<()>,
}

impl Output {
    /// The stream lives on its own thread because it cannot move between
    /// threads on every platform.
    pub fn open() -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (hold_tx, hold_rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new().name("audio-out".into()).spawn(move || {
            match OutputStreamBuilder::open_default_stream() {
                Ok(mut stream) => {
                    stream.log_on_drop(false);
                    let _ = ready_tx.send(Ok(stream.mixer().clone()));
                    let _ = hold_rx.recv();
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            }
        })?;
        let mixer = ready_rx
            .recv()?
            .map_err(|e| anyhow!("no audio output device: {e}"))?;
        Ok(Self { mixer, _hold: hold_tx })
    }
}

impl RodioDeck {
    pub fn new(volume: u8, tap: Arc<Tap>) -> Self {
        Self { output: None, sink: None, volume: f32::from(volume) / 100.0, tap }
    }

    /// Opens the device on first use.
    fn mixer(&mut self) -> Result<Mixer> {
        if self.output.is_none() {
            self.output = Some(Output::open()?);
        }
        Ok(self.output.as_ref().expect("just opened").mixer.clone())
    }
}

impl Deck for RodioDeck {
    fn load(&mut self, track: &Track, on_end: OnEnd) -> Result<()> {
        self.stop();
        let path = Path::new(&track.uri);
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let decoder = Decoder::try_from(file).with_context(|| format!("cannot decode {}", path.display()))?;
        let sink = Arc::new(Sink::connect_new(&self.mixer()?));
        sink.set_volume(self.volume);
        sink.append(self.tap.wrap(decoder));
        let waiter = sink.clone();
        std::thread::Builder::new().name("track-end".into()).spawn(move || {
            waiter.sleep_until_end();
            on_end();
        })?;
        self.sink = Some(sink);
        Ok(())
    }

    fn pause(&mut self) {
        if let Some(sink) = &self.sink {
            sink.pause();
        }
    }

    fn resume(&mut self) {
        if let Some(sink) = &self.sink {
            sink.play();
        }
    }

    fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn seek(&mut self, position_ms: u32) -> Result<()> {
        let sink = self.sink.as_ref().context("nothing is playing")?;
        sink.try_seek(Duration::from_millis(position_ms.into()))
            .map_err(|e| anyhow!("cannot seek: {e}"))
    }

    fn set_volume(&mut self, percent: u8) {
        self.volume = f32::from(percent) / 100.0;
        if let Some(sink) = &self.sink {
            sink.set_volume(self.volume);
        }
    }

    fn position_ms(&self) -> u32 {
        self.sink.as_ref().map_or(0, |s| s.get_pos().as_millis() as u32)
    }

    fn release(&mut self) {
        self.stop();
        self.output = None;
    }
}

/// Plays nothing and keeps time, for tests and demos without a sound card.
/// A track lasts its duration, or a second when the duration is unknown.
#[derive(Default)]
pub struct NullDeck {
    generation: Arc<AtomicU64>,
    started: Option<Instant>,
    /// Position when paused or seeked, counted from `started`.
    offset_ms: u32,
    paused: bool,
}

impl Deck for NullDeck {
    fn load(&mut self, track: &Track, on_end: OnEnd) -> Result<()> {
        let duration_ms = track.duration_ms;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.started = Some(Instant::now());
        self.offset_ms = 0;
        self.paused = false;
        let current = self.generation.clone();
        let length = Duration::from_millis(if duration_ms == 0 { 1000 } else { duration_ms.into() });
        std::thread::spawn(move || {
            std::thread::sleep(length);
            if current.load(Ordering::SeqCst) == generation {
                on_end();
            }
        });
        Ok(())
    }

    fn pause(&mut self) {
        self.offset_ms = self.position_ms();
        self.paused = true;
    }

    fn resume(&mut self) {
        self.started = Some(Instant::now());
        self.paused = false;
    }

    fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.started = None;
        self.offset_ms = 0;
    }

    fn seek(&mut self, position_ms: u32) -> Result<()> {
        self.offset_ms = position_ms;
        self.started = Some(Instant::now());
        Ok(())
    }

    fn set_volume(&mut self, _percent: u8) {}

    fn position_ms(&self) -> u32 {
        match (self.started, self.paused) {
            (Some(started), false) => self.offset_ms + started.elapsed().as_millis() as u32,
            _ => self.offset_ms,
        }
    }
}
