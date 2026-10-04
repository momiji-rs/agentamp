//! Audio output for files: local music and downloaded YouTube audio.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use librespot_playback::config::VolumeCtrl;
use librespot_playback::mixer::mappings::MappedCtrl;
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

/// The factor the samples are multiplied by at `percent` of the volume, for
/// every source alike: librespot's curve, logarithmic over 60 dB, so a step
/// sounds the same size anywhere on the scale.
pub fn gain(percent: u8) -> f32 {
    let volume = u32::from(percent.min(100)) * u32::from(u16::MAX) / 100;
    VolumeCtrl::default().to_mapped(volume as u16) as f32
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
        Self { output: None, sink: None, volume: gain(volume), tap }
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
        self.volume = gain(percent);
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
        let current = self.generation.clone();
        let length = Duration::from_millis(if duration_ms == 0 { 1000 } else { duration_ms.into() });
        std::thread::spawn(move || {
            std::thread::sleep(length);
            if current.load(Ordering::SeqCst) == generation {
                on_end();
            }
        });
        // The clock starts once the track is ready, as a device's does: on a
        // busy machine the spawn takes milliseconds a track paused at once
        // has not played.
        self.started = Some(Instant::now());
        self.offset_ms = 0;
        self.paused = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_playback::mixer::{self, MixerConfig};

    #[test]
    fn the_volume_follows_the_ear() {
        assert_eq!(gain(0), 0.0, "zero is silence");
        assert_eq!(gain(100), 1.0, "full is the sound as decoded");
        assert_eq!(gain(150), 1.0);
        let db = |percent| 20.0 * gain(percent).log10();
        assert!((db(50) + 30.0).abs() < 0.5, "half is 30 dB down, not 6: {}", db(50));
        assert!((1..=100).all(|p| gain(p) > gain(p - 1)), "every step is louder");
    }

    #[test]
    fn files_play_a_percentage_as_spotify_does() {
        let spotify = mixer::find(None).unwrap()(MixerConfig::default()).unwrap();
        let attenuation = spotify.get_soft_volume();
        for percent in [1, 10, 35, 50, 80, 100] {
            spotify.set_volume((u32::from(percent) * u32::from(u16::MAX) / 100) as u16);
            assert_eq!(gain(percent), attenuation.attenuation_factor() as f32, "{percent}%");
        }
    }
}
