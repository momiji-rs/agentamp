//! A library made up the same every run, shaped like a real one: about
//! 3.7 songs an artist (9688 Liked Songs by some 2600 artists, 2026-10-04),
//! a few artists holding most of them, and the fields a Spotify track has.

#![allow(dead_code)]

use std::time::{Duration, UNIX_EPOCH};

use agentamp::db::Play;
use agentamp::model::{Source, Track};

/// The sizes each benchmark runs at: a playlist, the Liked Songs, ten times them.
pub const SIZES: [usize; 3] = [1_000, 10_000, 100_000];

/// splitmix64: the same numbers for the same seed, without a crate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Below `n`, the small numbers far likelier: the few artists with many songs.
    pub fn skewed(&mut self, n: usize) -> usize {
        let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        ((u * u * u) * n as f64) as usize
    }

    fn base62(&mut self, len: usize) -> String {
        const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        (0..len).map(|_| DIGITS[(self.next() % 62) as usize] as char).collect()
    }
}

/// `n` Spotify tracks, as a queue holds them once their details are read.
pub fn tracks(n: usize, seed: u64) -> Vec<Track> {
    let mut rng = Rng::new(seed);
    let artists = (n * 10 / 37).max(1);
    (0..n)
        .map(|i| {
            let artist = rng.skewed(artists);
            let mut track = Track::placeholder(Source::Spotify, format!("spotify:track:{}", rng.base62(22)));
            track.title = format!("Song number {i}");
            track.artist = format!("Artist {artist}");
            track.album = format!("Album {} of artist {artist}", rng.next() % 4);
            track.duration_ms = 150_000 + (rng.next() % 250_000) as u32;
            let image = rng.next() as u128 * rng.next() as u128;
            track.art = Some(format!("https://i.scdn.co/image/ab67616d0000b273{image:040x}"));
            track
        })
        .collect()
}

/// `n` plays of a library of `songs`, a few minutes apart and oldest
/// first, some heard through and some skipped.
pub fn plays(n: usize, songs: usize, seed: u64) -> Vec<Play> {
    let library = tracks(songs, seed);
    let mut rng = Rng::new(seed ^ 0x5eed);
    let mut at = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    (0..n)
        .map(|_| {
            let track = library[rng.skewed(songs)].clone();
            at += Duration::from_secs(60 + rng.next() % 600);
            let whole = !rng.next().is_multiple_of(4);
            let ms_played = if whole { track.duration_ms } else { (rng.next() % 30_000) as u32 };
            Play { track, started: at, ms_played }
        })
        .collect()
}

/// A directory under `target/` for the benchmarks' files (never /tmp).
pub fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
