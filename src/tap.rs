//! The sound as the device takes it, before the volume, for the windows
//! that draw it. Nothing is kept while no window listens.
//!
//! A listening window gets the samples as binary chunks after the answer
//! to `listen`: the sample rate and the count as little-endian u32s, then
//! that many little-endian f32 mono samples. A chunk of none says the
//! player is still there while nothing plays.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::Source;
use rodio::source::SeekError;

/// Samples kept for a listener that falls behind: over a third of a
/// second, more than the window's longest analysis.
pub const KEPT: usize = 16_384;
/// Samples gathered before they are shared: 6 ms at 44.1 kHz.
const BLOCK: usize = 256;

#[derive(Default)]
pub struct Tap {
    ring: Mutex<Ring>,
    listeners: AtomicUsize,
}

#[derive(Default)]
struct Ring {
    samples: VecDeque<f32>,
    /// How many samples have ever been pushed.
    end: u64,
    rate: u32,
}

impl Tap {
    /// Starts keeping samples for a new listener, from now on.
    pub fn listen(self: &Arc<Self>) -> Listener {
        self.listeners.fetch_add(1, Ordering::SeqCst);
        let cursor = self.ring.lock().expect("tap").end;
        Listener { tap: self.clone(), cursor }
    }

    fn wanted(&self) -> bool {
        self.listeners.load(Ordering::Relaxed) > 0
    }

    fn push(&self, rate: u32, block: &[f32]) {
        let mut ring = self.ring.lock().expect("tap");
        if ring.rate != rate {
            ring.samples.clear();
            ring.rate = rate;
        }
        let over = (ring.samples.len() + block.len()).saturating_sub(KEPT).min(ring.samples.len());
        ring.samples.drain(..over);
        ring.samples.extend(&block[block.len().saturating_sub(KEPT)..]);
        ring.end += block.len() as u64;
    }

    /// Wraps a source so its sound reaches the listeners as it plays.
    pub fn wrap<S: Source>(self: &Arc<Self>, inner: S) -> Tapped<S> {
        Tapped { inner, tap: self.clone(), sum: 0.0, index: 0, block: Vec::with_capacity(BLOCK) }
    }
}

/// One window's place in the tap. Dropping it stops the keeping once no
/// one else listens.
pub struct Listener {
    tap: Arc<Tap>,
    cursor: u64,
}

impl Listener {
    /// The samples pushed since the last take, and their rate. A listener
    /// too far behind skips to what is kept.
    pub fn take(&mut self, out: &mut Vec<f32>) -> u32 {
        let ring = self.tap.ring.lock().expect("tap");
        let new = (ring.end - self.cursor.min(ring.end)).min(ring.samples.len() as u64) as usize;
        out.extend(ring.samples.range(ring.samples.len() - new..));
        self.cursor = ring.end;
        ring.rate
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.tap.listeners.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A source that hands its sound, downmixed to mono, to the tap as the
/// device pulls it. The volume is applied after, by the sink.
pub struct Tapped<S> {
    inner: S,
    tap: Arc<Tap>,
    sum: f32,
    index: u16,
    block: Vec<f32>,
}

impl<S: Source> Iterator for Tapped<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;
        if !self.tap.wanted() {
            return Some(sample);
        }
        self.sum += sample;
        self.index += 1;
        let channels = self.inner.channels().max(1);
        if self.index >= channels {
            self.block.push(self.sum / f32::from(channels));
            (self.sum, self.index) = (0.0, 0);
            if self.block.len() >= BLOCK {
                self.tap.push(self.inner.sample_rate(), &self.block);
                self.block.clear();
            }
        }
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for Tapped<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        (self.sum, self.index) = (0.0, 0);
        self.block.clear();
        self.inner.try_seek(position)
    }
}

/// One chunk as it goes over the socket.
pub fn encode(rate: u32, samples: &[f32], out: &mut Vec<u8>) {
    out.extend(rate.to_le_bytes());
    out.extend((samples.len() as u32).to_le_bytes());
    for sample in samples {
        out.extend(sample.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn stereo(frames: usize) -> SamplesBuffer {
        // Left and right differ, so the mono is their mean.
        let samples: Vec<f32> = (0..frames).flat_map(|i| [i as f32, i as f32 + 2.0]).collect();
        SamplesBuffer::new(2, 44_100, samples)
    }

    #[test]
    fn nothing_is_kept_without_a_listener() {
        let tap = Arc::new(Tap::default());
        let played: Vec<f32> = tap.wrap(stereo(BLOCK * 2)).collect();
        assert_eq!(played.len(), BLOCK * 4, "the sound passes through untouched");
        assert_eq!(tap.ring.lock().unwrap().end, 0);
    }

    #[test]
    fn listeners_get_mono_samples_as_they_play() {
        let tap = Arc::new(Tap::default());
        let mut listener = tap.listen();
        let played: Vec<f32> = tap.wrap(stereo(BLOCK * 2 + 10)).collect();
        assert_eq!(&played[..4], &[0.0, 2.0, 1.0, 3.0]);
        let mut heard = Vec::new();
        assert_eq!(listener.take(&mut heard), 44_100);
        // Whole blocks only: the rest waits for more sound.
        assert_eq!(heard.len(), BLOCK * 2);
        assert_eq!(&heard[..3], &[1.0, 2.0, 3.0]);
        heard.clear();
        listener.take(&mut heard);
        assert!(heard.is_empty(), "each sample is taken once");
        drop(listener);
        assert!(!tap.wanted());
    }

    #[test]
    fn a_listener_far_behind_skips_ahead() {
        let tap = Arc::new(Tap::default());
        let mut listener = tap.listen();
        let _: Vec<f32> = tap.wrap(stereo(KEPT * 2)).collect();
        let mut heard = Vec::new();
        listener.take(&mut heard);
        assert_eq!(heard.len(), KEPT);
        assert_eq!(heard.last(), Some(&((KEPT * 2 - 1) as f32 + 1.0)));
    }

    #[test]
    fn a_new_rate_starts_afresh() {
        let tap = Arc::new(Tap::default());
        let mut listener = tap.listen();
        let _: Vec<f32> = tap.wrap(stereo(BLOCK)).collect();
        let _: Vec<f32> = tap.wrap(SamplesBuffer::new(1, 48_000, vec![0.5; BLOCK])).collect();
        let mut heard = Vec::new();
        assert_eq!(listener.take(&mut heard), 48_000);
        assert_eq!(heard, vec![0.5; BLOCK]);
    }
}
