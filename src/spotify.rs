//! Spotify through librespot: sign-in, the session, playback and catalogue
//! details. Playback needs a Premium account.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Metadata, Playlist};
use librespot_playback::audio_backend::{self, SinkError, SinkResult};
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::{self, Mixer, MixerConfig, NoOpVolume, VolumeGetter};
use librespot_playback::player::{Player, PlayerEvent};
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use rodio::buffer::SamplesBuffer;
use log::info;
use tokio::sync::Mutex;

use crate::deck::{Deck, OnEnd, Output};
use crate::model::{Source, Track};
use crate::paths::Paths;
use crate::tap::Tap;
use crate::target::SpotifyKind;

/// Spotify's own desktop client, the identity librespot streams as.
const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
/// Spotify's encrypted audio cache: songs heard again are not downloaded
/// again.
const AUDIO_CACHE_BYTES: u64 = 2 << 30;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

fn cache(paths: &Paths) -> Result<Cache> {
    // The credential file inherits nothing from the directory, so the
    // directory itself is closed to other users.
    std::fs::create_dir_all(&paths.config)?;
    std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o700))?;
    Ok(Cache::new(Some(&paths.config), None, Some(&paths.spotify_audio()), Some(AUDIO_CACHE_BYTES))?)
}

/// Signs in through the browser and keeps librespot's reusable credential.
/// Returns the account name.
pub fn login(paths: &Paths) -> Result<String> {
    let client = librespot_oauth::OAuthClientBuilder::new(CLIENT_ID, REDIRECT_URI, vec!["streaming"])
        .open_in_browser()
        .with_custom_message("AgentAmp is signed in. You can close this tab.")
        .build()?;
    let token = client.get_access_token().map_err(|e| anyhow!("sign-in failed: {e}"))?;
    let cache = cache(paths)?;
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let session = Session::new(SessionConfig::default(), Some(cache));
        tokio::time::timeout(CONNECT_TIMEOUT, session.connect(Credentials::with_access_token(token.access_token), true))
            .await
            .context("Spotify did not answer")??;
        let name = session.username();
        session.shutdown();
        Ok(name)
    })
}

pub fn logout(paths: &Paths) -> Result<bool> {
    match std::fs::remove_file(paths.config.join("credentials.json")) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// One session for the daemon, connected on first use and again after it
/// drops.
pub struct Spotify {
    paths: Paths,
    session: Mutex<Option<Session>>,
}

impl Spotify {
    pub fn new(paths: Paths) -> Arc<Self> {
        Arc::new(Self { paths, session: Mutex::new(None) })
    }

    pub async fn session(&self) -> Result<Session> {
        let mut current = self.session.lock().await;
        if let Some(session) = current.as_ref().filter(|s| !s.is_invalid()) {
            return Ok(session.clone());
        }
        let cache = cache(&self.paths)?;
        let credentials = cache.credentials().context("not signed in to Spotify; run `agentamp login`")?;
        let session = Session::new(SessionConfig::default(), Some(cache));
        tokio::time::timeout(CONNECT_TIMEOUT, session.connect(credentials, true))
            .await
            .context("Spotify did not answer")??;
        info!("signed in to Spotify as {}", session.username());
        *current = Some(session.clone());
        Ok(session)
    }

    /// The connected session, without waiting for one.
    pub fn ready(&self) -> Option<Session> {
        self.session.try_lock().ok()?.as_ref().filter(|s| !s.is_invalid()).cloned()
    }

    /// The tracks of a track, album or playlist URI, without details yet.
    pub async fn expand(&self, kind: SpotifyKind, uri: &str) -> Result<Vec<Track>> {
        let session = self.session().await?;
        let id = SpotifyUri::from_uri(uri)?;
        let uris: Vec<SpotifyUri> = match kind {
            SpotifyKind::Track => vec![id],
            SpotifyKind::Album => Album::get(&session, &id).await?.tracks().cloned().collect(),
            SpotifyKind::Playlist => Playlist::get(&session, &id).await?.tracks().cloned().collect(),
        };
        Ok(uris
            .iter()
            .filter_map(|u| u.to_uri().ok())
            .filter(|u| u.starts_with("spotify:track:"))
            .map(|u| Track::placeholder(Source::Spotify, u))
            .collect())
    }

    /// Title, artists, album and length.
    pub async fn details(&self, uri: &str) -> Result<Track> {
        let session = self.session().await?;
        let track = librespot_metadata::Track::get(&session, &SpotifyUri::from_uri(uri)?).await?;
        Ok(Track {
            source: Source::Spotify,
            uri: uri.to_string(),
            title: track.name,
            artist: track.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
            album: track.album.name,
            duration_ms: track.duration.max(0) as u32,
            link: None,
            art: cover(track.album.covers.iter().map(|c| (c.width, format!("https://i.scdn.co/image/{}", c.id)))),
        })
    }
}

/// The cover to show from an album's sizes, given as (width, URL): the
/// smallest that is still sharp in a terminal, or else the largest.
pub fn cover(sizes: impl IntoIterator<Item = (i32, String)>) -> Option<String> {
    const ENOUGH: i32 = 300;
    let mut sizes: Vec<_> = sizes.into_iter().collect();
    sizes.sort_by_key(|(width, _)| *width);
    let pick = sizes.iter().position(|(width, _)| *width >= ENOUGH).unwrap_or(sizes.len().saturating_sub(1));
    (!sizes.is_empty()).then(|| sizes.swap_remove(pick).1)
}

/// Plays Spotify tracks. librespot reports their end as a player event,
/// which the engine reads, so `load` does not keep `on_end`.
pub struct SpotifyDeck {
    player: Arc<Player>,
    /// librespot's software mixer, for its volume curve only: the speaker
    /// applies the volume, after the tap.
    mixer: Arc<dyn Mixer>,
    curve: Box<dyn VolumeGetter + Send>,
    speaker: Arc<rodio::Sink>,
    _output: Output,
    pub session: Session,
    clock: Clock,
}

impl SpotifyDeck {
    pub fn new(
        session: Session,
        volume: u8,
        tap: Arc<Tap>,
    ) -> Result<(Self, tokio::sync::mpsc::UnboundedReceiver<PlayerEvent>)> {
        let mixer = mixer::find(None).context("no software mixer")?(MixerConfig::default())?;
        let output = Output::open()?;
        let speaker = Arc::new(rodio::Sink::connect_new(&output.mixer));
        let config = PlayerConfig { bitrate: Bitrate::Bitrate320, ..PlayerConfig::default() };
        let sink = Speaker { sink: speaker.clone(), tap };
        let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || Box::new(sink));
        let events = player.get_player_event_channel();
        let curve = mixer.get_soft_volume();
        let mut deck = Self { player, mixer, curve, speaker, _output: output, session, clock: Clock::default() };
        deck.set_volume(volume);
        Ok((deck, events))
    }

    pub fn is_invalid(&self) -> bool {
        self.player.is_invalid() || self.session.is_invalid()
    }

    pub fn preload(&self, uri: &str) {
        if let Ok(uri) = SpotifyUri::from_uri(uri) {
            self.player.preload(uri);
        }
    }

    /// Follows the player's reports of where it is.
    pub fn observe(&mut self, event: &PlayerEvent) {
        self.clock.observe(event);
    }
}

impl Deck for SpotifyDeck {
    fn load(&mut self, track: &Track, _on_end: OnEnd) -> Result<()> {
        let uri = SpotifyUri::from_uri(&track.uri)?;
        self.clock = Clock { base_ms: 0, since: Some(Instant::now()) };
        self.player.load(uri, true, 0);
        Ok(())
    }

    fn pause(&mut self) {
        self.player.pause();
    }

    fn resume(&mut self) {
        self.player.play();
    }

    fn stop(&mut self) {
        self.player.stop();
        self.clock = Clock::default();
    }

    fn seek(&mut self, position_ms: u32) -> Result<()> {
        self.player.seek(position_ms);
        self.clock.base_ms = position_ms;
        if self.clock.since.is_some() {
            self.clock.since = Some(Instant::now());
        }
        Ok(())
    }

    fn set_volume(&mut self, percent: u8) {
        self.mixer.set_volume((u32::from(percent.min(100)) * u32::from(u16::MAX) / 100) as u16);
        self.speaker.set_volume(self.curve.attenuation_factor() as f32);
    }

    fn position_ms(&self) -> u32 {
        self.clock.now()
    }
}

/// Where librespot's sound goes: to the device through rodio, as
/// librespot's own rodio backend sends it, but through the tap first.
struct Speaker {
    sink: Arc<rodio::Sink>,
    tap: Arc<Tap>,
}

impl audio_backend::Sink for Speaker {
    fn start(&mut self) -> SinkResult<()> {
        self.sink.play();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.sink.sleep_until_end();
        self.sink.pause();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet.samples().map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let source = SamplesBuffer::new(u16::from(NUM_CHANNELS), SAMPLE_RATE, converter.f64_to_f32(samples));
        self.sink.append(self.tap.wrap(source));
        // About half a second queued, as librespot's backend keeps.
        while self.sink.len() > 26 {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

/// Position from the last report, moving on while playing.
#[derive(Clone, Copy, Debug, Default)]
struct Clock {
    base_ms: u32,
    since: Option<Instant>,
}

impl Clock {
    fn now(&self) -> u32 {
        self.base_ms + self.since.map_or(0, |s| s.elapsed().as_millis() as u32)
    }

    fn observe(&mut self, event: &PlayerEvent) {
        match *event {
            PlayerEvent::Playing { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. }
            | PlayerEvent::PositionChanged { position_ms, .. } => {
                *self = Self { base_ms: position_ms, since: Some(Instant::now()) };
            }
            PlayerEvent::Seeked { position_ms, .. } => {
                self.base_ms = position_ms;
                self.since = self.since.map(|_| Instant::now());
            }
            PlayerEvent::Paused { position_ms, .. } | PlayerEvent::Loading { position_ms, .. } => {
                *self = Self { base_ms: position_ms, since: None };
            }
            PlayerEvent::Stopped { .. } | PlayerEvent::EndOfTrack { .. } => *self = Self::default(),
            _ => {}
        }
    }
}

/// The URI of a track the player says it is done with or cannot play.
pub fn ended(event: &PlayerEvent) -> Option<(String, bool)> {
    match event {
        PlayerEvent::EndOfTrack { track_id, .. } => Some((track_id.to_uri().ok()?, true)),
        PlayerEvent::Unavailable { track_id, .. } => Some((track_id.to_uri().ok()?, false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cover_is_the_smallest_sharp_size() {
        let sizes = |list: &[i32]| list.iter().map(|w| (*w, format!("{w}px"))).collect::<Vec<_>>();
        assert_eq!(cover(sizes(&[640, 64, 300])).as_deref(), Some("300px"));
        assert_eq!(cover(sizes(&[64, 120])).as_deref(), Some("120px"));
        assert_eq!(cover(sizes(&[])), None);
    }

    fn uri() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:4iV5W9uYEdYUVa79Axb7Rh").unwrap()
    }

    #[test]
    fn the_clock_follows_player_reports() {
        let mut clock = Clock::default();
        clock.observe(&PlayerEvent::Playing { play_request_id: 1, track_id: uri(), position_ms: 5000 });
        assert!((5000..5100).contains(&clock.now()));
        clock.observe(&PlayerEvent::Paused { play_request_id: 1, track_id: uri(), position_ms: 7000 });
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(clock.now(), 7000, "a paused clock stands still");
        clock.observe(&PlayerEvent::Seeked { play_request_id: 1, track_id: uri(), position_ms: 1000 });
        assert_eq!(clock.now(), 1000, "seeking while paused stays paused");
        clock.observe(&PlayerEvent::EndOfTrack { play_request_id: 1, track_id: uri() });
        assert_eq!(clock.now(), 0);
    }

    #[test]
    fn ends_and_failures_name_their_track() {
        let end = PlayerEvent::EndOfTrack { play_request_id: 1, track_id: uri() };
        assert_eq!(ended(&end), Some(("spotify:track:4iV5W9uYEdYUVa79Axb7Rh".into(), true)));
        let gone = PlayerEvent::Unavailable { play_request_id: 1, track_id: uri() };
        assert_eq!(ended(&gone).map(|e| e.1), Some(false));
        assert_eq!(ended(&PlayerEvent::VolumeChanged { volume: 1 }), None);
    }
}
