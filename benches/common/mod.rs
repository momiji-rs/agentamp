//! A library made up the same every run, shaped like a real one: about
//! 3.7 songs an artist (9688 Liked Songs by some 2600 artists, 2026-10-04),
//! a few artists holding most of them, and the fields a Spotify track has.

#![allow(dead_code)]

use std::time::{Duration, UNIX_EPOCH};

use agentamp::db::{Liked, Play, Streamed};
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

/// `n` Liked Songs, liked a few hours apart, newest first as Spotify lists them.
pub fn liked(n: usize, seed: u64) -> Vec<Liked> {
    let mut rng = Rng::new(seed ^ 0x11ed);
    let mut at = 1_600_000_000 + n as u64 * 3 * 3600;
    tracks(n, seed)
        .into_iter()
        .map(|track| {
            at -= 3600 + rng.next() % (4 * 3600);
            Liked {
                added_at: format!("@{at}"),
                album_uri: Some(format!("spotify:album:{:022}", rng.skewed(n / 8 + 1))),
                uri: track.uri,
                title: track.title,
                artist: track.artist,
                album: track.album,
                duration_ms: track.duration_ms,
                art: track.art,
            }
        })
        .collect()
}

/// `n` songs of Spotify's streaming history, as `plays` makes them.
pub fn streamed(n: usize, seed: u64) -> Vec<Streamed> {
    plays(n, 10_000, seed)
        .into_iter()
        .map(|play| {
            let ended = play.started.duration_since(UNIX_EPOCH).unwrap().as_secs() + u64::from(play.ms_played / 1000);
            Streamed {
                ended_at: iso(ended),
                ms_played: play.ms_played,
                uri: play.track.uri,
                title: play.track.title,
                artist: play.track.artist,
                album: play.track.album,
            }
        })
        .collect()
}

/// Seconds since 1970 as UTC ISO 8601, as Spotify writes its times
/// (Howard Hinnant's days-to-civil).
pub fn iso(seconds: u64) -> String {
    let (days, time) = ((seconds / 86_400) as i64, seconds % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", time / 3600, time / 60 % 60, time % 60)
}

/// A directory under `target/` for the benchmarks' files (never /tmp).
pub fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench-scratch").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
