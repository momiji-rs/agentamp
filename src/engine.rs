//! The player: one task owns the queue and the decks, and every request and
//! playback event passes through it in order.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, bail};
use librespot_metadata::audio::UniqueFields;
use librespot_playback::player::PlayerEvent;
use log::{info, warn};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

use crate::db::{Log, Play};
use crate::deck::Deck;
use crate::ipc::{Request, Response};
use crate::model::{Source, State, Status, Track};
use crate::queue::Queue;
use crate::spotify::{self, Spotify, SpotifyDeck};
use crate::tap::Tap;

/// Within this much of a track's start, Previous goes to the track before.
const RESTART_WINDOW_MS: u32 = 3_000;
/// YouTube downloads at a time: the current track and the next, so the next
/// is ready by its turn without yt-dlp crowding a slow connection.
const DOWNLOADS: usize = 2;

/// Downloads a YouTube link or `ytsearch1:` query into a track that plays
/// from a file. The daemon's runs yt-dlp; the tests' pretend.
pub type Fetch = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Track>> + Send>> + Send + Sync>;

pub enum Msg {
    /// A control request. `play` and `add` arrive as `Enqueue`.
    Control(Request, oneshot::Sender<Response>),
    Enqueue { tracks: Vec<Track>, mode: Mode, reply: oneshot::Sender<Response> },
    /// Details for queued tracks, found after they were queued.
    Resolved(Vec<Track>),
    /// The file deck finished the load with this generation.
    FileEnded(u64),
    Spotify(PlayerEvent),
    /// The Spotify session the current track waited for is ready, or failed.
    SessionReady(Result<(), String>),
    /// The download for the queued tracks waiting on `key` finished, or failed.
    Fetched { key: String, result: Result<Track, String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Replace,
    Next,
    Append,
}

pub struct Engine {
    queue: Queue,
    state: State,
    volume: u8,
    error: Option<String>,
    files: Box<dyn Deck>,
    /// Counts file loads, so a late end of an old track is ignored.
    generation: u64,
    spotify: Arc<Spotify>,
    spotify_deck: Option<SpotifyDeck>,
    connecting: bool,
    fetch: Fetch,
    /// The downloads under way, by the key the waiting tracks have as `uri`.
    fetching: HashMap<String, AbortHandle>,
    tx: mpsc::UnboundedSender<Msg>,
    tap: Arc<Tap>,
    /// Where finished plays go, when they are kept.
    log: Option<Log>,
    listening: Option<Listening>,
}

/// The track being heard, and for how long so far.
struct Listening {
    /// As it is now: its details can arrive after it starts.
    track: Track,
    started: SystemTime,
    heard: Duration,
    /// Since when it plays, while it is not paused.
    since: Option<Instant>,
}

impl Engine {
    pub fn new(
        files: Box<dyn Deck>,
        volume: u8,
        spotify: Arc<Spotify>,
        fetch: Fetch,
        tx: mpsc::UnboundedSender<Msg>,
        tap: Arc<Tap>,
    ) -> Self {
        Self {
            queue: Queue::default(),
            state: State::Stopped,
            volume,
            error: None,
            files,
            generation: 0,
            spotify,
            spotify_deck: None,
            connecting: false,
            fetch,
            fetching: HashMap::new(),
            tx,
            tap,
            log: None,
            listening: None,
        }
    }

    /// Keeps each finished play in `log`.
    pub fn logging(mut self, log: Log) -> Self {
        self.log = Some(log);
        self
    }

    pub async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Control(request, reply) => {
                    let shutdown = request == Request::Shutdown;
                    let response = match self.control(request) {
                        Ok(data) => Response::ok(data),
                        Err(e) => Response::error(e),
                    };
                    let _ = reply.send(response);
                    if shutdown {
                        break;
                    }
                }
                Msg::Enqueue { tracks, mode, reply } => {
                    let added = tracks.len();
                    let response = match self.enqueue(tracks, mode) {
                        Ok(()) => Response::ok(json!({"added": added, "status": self.status()})),
                        Err(e) => Response::error(e),
                    };
                    let _ = reply.send(response);
                }
                Msg::Resolved(tracks) => self.queue.update(&tracks),
                Msg::FileEnded(generation) => {
                    if generation == self.generation && self.state != State::Stopped {
                        self.play_next();
                    }
                }
                Msg::Spotify(event) => self.spotify_event(event),
                Msg::SessionReady(result) => self.session_ready(result),
                Msg::Fetched { key, result } => self.fetched(&key, result),
            }
            self.fetch_pending();
            self.listen();
        }
        self.finish_listening();
        for task in self.fetching.values() {
            task.abort();
        }
        self.files.release();
        if let Some(deck) = &mut self.spotify_deck {
            deck.stop();
        }
    }

    /// Follows what is heard: a play begins when a track sounds and ends
    /// when another takes its place or the player stops. A track still
    /// downloading is not heard yet.
    fn listen(&mut self) {
        let heard = self.queue.current.clone().filter(|t| !t.downloading && self.state != State::Stopped);
        if self.listening.as_ref().is_some_and(|l| heard.as_ref().is_none_or(|t| t.uri != l.track.uri)) {
            self.finish_listening();
        }
        let Some(track) = heard else { return };
        let listening = self.listening.get_or_insert_with(|| Listening {
            track: track.clone(),
            started: SystemTime::now(),
            heard: Duration::ZERO,
            since: None,
        });
        listening.track = track;
        match (self.state, listening.since) {
            (State::Playing, None) => listening.since = Some(Instant::now()),
            (State::Paused, Some(since)) => {
                listening.heard += since.elapsed();
                listening.since = None;
            }
            _ => {}
        }
    }

    fn finish_listening(&mut self) {
        let Some(listening) = self.listening.take() else { return };
        let heard = listening.heard + listening.since.map_or(Duration::ZERO, |s| s.elapsed());
        if let Some(log) = &self.log
            && !heard.is_zero()
        {
            let ms_played = heard.as_millis() as u32;
            log.record(Play { track: listening.track, started: listening.started, ms_played });
        }
    }

    fn enqueue(&mut self, tracks: Vec<Track>, mode: Mode) -> Result<()> {
        if tracks.is_empty() {
            bail!("found nothing to play there");
        }
        match mode {
            Mode::Replace => {
                self.queue.clear();
                self.queue.append(tracks);
                self.play_next();
            }
            Mode::Next => self.queue.insert_next(tracks),
            Mode::Append => self.queue.append(tracks),
        }
        // Adding to a stopped player starts it.
        if mode != Mode::Replace && self.queue.current.is_none() {
            self.play_next();
        }
        Ok(())
    }

    fn control(&mut self, request: Request) -> Result<serde_json::Value> {
        match request {
            Request::Pause => self.pause(),
            Request::Resume => self.resume(),
            Request::Toggle => match self.state {
                State::Playing => self.pause(),
                _ => self.resume(),
            },
            Request::Next => self.play_next(),
            Request::Previous => self.previous(),
            Request::Stop => self.stop(),
            Request::Clear => self.queue.clear(),
            Request::Volume { percent } => {
                self.volume = percent.min(100);
                self.files.set_volume(self.volume);
                if let Some(deck) = &mut self.spotify_deck {
                    deck.set_volume(self.volume);
                }
            }
            Request::Seek { .. } if self.queue.current.as_ref().is_some_and(|t| t.downloading) => {
                bail!("the track is still downloading")
            }
            Request::Seek { position_ms } => match self.deck() {
                Some(deck) => deck.seek(position_ms)?,
                None => bail!("nothing is playing"),
            },
            Request::Queue { offset, count } => return Ok(serde_json::to_value(self.queue.stretch(offset, count))?),
            Request::Status | Request::Shutdown => {}
            Request::Play { .. } | Request::Add { .. } => bail!("play and add are resolved first"),
            Request::Listen => bail!("listening is answered by the connection"),
            Request::SearchSpotify { .. } | Request::Browse { .. } => bail!("Spotify's pages are answered by the daemon"),
        }
        Ok(serde_json::to_value(self.status())?)
    }

    pub fn status(&self) -> Status {
        let position_ms = match self.queue.current.as_ref().map(|t| t.source) {
            Some(Source::Spotify) => self.spotify_deck.as_ref().map_or(0, |d| d.position_ms()),
            Some(_) => self.files.position_ms(),
            None => 0,
        };
        Status {
            state: self.state,
            track: self.queue.current.clone(),
            position_ms,
            volume: self.volume,
            queue_len: self.queue.upcoming.len(),
            error: self.error.clone(),
        }
    }

    /// The deck playing the current track.
    fn deck(&mut self) -> Option<&mut dyn Deck> {
        match self.queue.current.as_ref()?.source {
            Source::Spotify => self.spotify_deck.as_mut().map(|d| d as &mut dyn Deck),
            _ => Some(self.files.as_mut()),
        }
    }

    fn pause(&mut self) {
        if self.state == State::Playing {
            if let Some(deck) = self.deck() {
                deck.pause();
            }
            self.state = State::Paused;
        }
    }

    fn resume(&mut self) {
        match self.state {
            State::Paused => {
                if let Some(deck) = self.deck() {
                    deck.resume();
                }
                self.state = State::Playing;
            }
            State::Stopped => self.play_next(),
            State::Playing => {}
        }
    }

    fn stop(&mut self) {
        self.generation += 1;
        self.files.release();
        if let Some(deck) = &mut self.spotify_deck {
            deck.stop();
        }
        self.queue.stop();
        self.state = State::Stopped;
    }

    /// Restarts the track, or goes back one when it has only just begun,
    /// as Spotify does.
    fn previous(&mut self) {
        let restart = self.queue.current.is_some()
            && (self.status().position_ms > RESTART_WINDOW_MS || self.queue.history.is_empty());
        if restart {
            if let Some(deck) = self.deck()
                && let Err(e) = deck.seek(0)
            {
                warn!("cannot restart: {e:#}");
            }
            return;
        }
        let Some(track) = self.queue.back() else { return };
        match self.start(&track) {
            Ok(()) => {
                info!("playing {}", track.label());
                self.state = State::Playing;
                self.error = None;
            }
            Err(e) => {
                warn!("cannot go back to {}: {e:#}", track.label());
                self.error = Some(format!("cannot play {}: {e:#}", track.label()));
                self.play_next();
            }
        }
    }

    /// Starts the next track that will play, skipping any that fail.
    fn play_next(&mut self) {
        while let Some(track) = self.queue.advance() {
            match self.start(&track) {
                Ok(()) => {
                    info!("playing {}", track.label());
                    self.state = State::Playing;
                    self.error = None;
                    return;
                }
                Err(e) => {
                    warn!("skipping {}: {e:#}", track.label());
                    self.error = Some(format!("cannot play {}: {e:#}", track.label()));
                }
            }
        }
        self.stop();
    }

    /// Starts `track` on its deck. A Spotify track shows as playing at once;
    /// when the session has to connect first, it starts when it is ready.
    /// So does a YouTube track still downloading, silent until its file is here.
    fn start(&mut self, track: &Track) -> Result<()> {
        if track.downloading {
            if let Some(deck) = &mut self.spotify_deck {
                deck.stop();
            }
            self.generation += 1;
            self.files.release();
            return Ok(());
        }
        match track.source {
            Source::Local | Source::Youtube => {
                if let Some(deck) = &mut self.spotify_deck {
                    deck.stop();
                }
                self.generation += 1;
                let generation = self.generation;
                let tx = self.tx.clone();
                self.files.set_volume(self.volume);
                self.files.load(
                    track,
                    Box::new(move || {
                        let _ = tx.send(Msg::FileEnded(generation));
                    }),
                )
            }
            Source::Spotify => {
                self.generation += 1;
                self.files.release();
                if self.spotify_deck.as_ref().is_some_and(|d| d.is_invalid()) {
                    self.spotify_deck = None;
                }
                if self.spotify_deck.is_none() {
                    let Some(session) = self.spotify.ready() else {
                        self.connect();
                        return Ok(());
                    };
                    let (deck, mut events) = SpotifyDeck::new(session, self.volume, self.tap.clone())?;
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        while let Some(event) = events.recv().await {
                            if tx.send(Msg::Spotify(event)).is_err() {
                                break;
                            }
                        }
                    });
                    self.spotify_deck = Some(deck);
                }
                let deck = self.spotify_deck.as_mut().expect("just made");
                deck.set_volume(self.volume);
                deck.load(track, Box::new(|| {}))
            }
        }
    }

    fn connect(&mut self) {
        if self.connecting {
            return;
        }
        self.connecting = true;
        let (spotify, tx) = (self.spotify.clone(), self.tx.clone());
        tokio::spawn(async move {
            let result = spotify.session().await.map(|_| ()).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::SessionReady(result));
        });
    }

    fn session_ready(&mut self, result: Result<(), String>) {
        self.connecting = false;
        let Some(track) = self.queue.current.clone().filter(|t| t.source == Source::Spotify) else {
            return;
        };
        if self.spotify_deck.is_some() || self.state == State::Stopped {
            return;
        }
        let started = match result {
            Ok(()) => self.start(&track).map_err(|e| format!("{e:#}")),
            Err(e) => Err(e),
        };
        match started {
            Ok(()) if self.state == State::Paused => {
                if let Some(deck) = self.deck() {
                    deck.pause();
                }
            }
            Ok(()) => {}
            Err(e) => {
                warn!("Spotify is unavailable: {e}");
                self.stop();
                self.error = Some(format!("Spotify is unavailable: {e}"));
            }
        }
    }

    /// Keeps the first downloads in play order under way, and cancels the
    /// ones nothing waits for any more or that something sooner overtook.
    /// Cancelling drops the task, and with it kills its yt-dlp.
    fn fetch_pending(&mut self) {
        let mut wanted = self.queue.downloads();
        wanted.truncate(DOWNLOADS);
        self.fetching.retain(|key, task| {
            let keep = wanted.contains(key);
            if !keep {
                task.abort();
            }
            keep
        });
        for key in wanted {
            if self.fetching.contains_key(&key) {
                continue;
            }
            info!("downloading {key}");
            let (fetch, tx, done) = (self.fetch.clone(), self.tx.clone(), key.clone());
            let task = tokio::spawn(async move {
                let result = fetch(done.clone()).await.map_err(|e| format!("{e:#}"));
                let _ = tx.send(Msg::Fetched { key: done, result });
            });
            self.fetching.insert(key, task.abort_handle());
        }
    }

    fn fetched(&mut self, key: &str, result: Result<Track, String>) {
        self.fetching.remove(key);
        let current = self.queue.current.as_ref().is_some_and(|t| t.downloading && t.uri == key);
        match result {
            Ok(track) => {
                self.queue.downloaded(key, &track);
                if !current || self.state == State::Stopped {
                    return;
                }
                match self.start(&track) {
                    Ok(()) if self.state == State::Paused => self.files.pause(),
                    Ok(()) => {}
                    Err(e) => {
                        warn!("skipping {}: {e:#}", track.label());
                        self.play_next();
                        self.error = Some(format!("cannot play {}: {e:#}", track.label()));
                    }
                }
            }
            Err(e) => {
                if self.queue.drop_download(key) == 0 {
                    return;
                }
                warn!("cannot download {key}: {e}");
                let shown = key.strip_prefix("ytsearch1:").unwrap_or(key);
                if current && self.state != State::Stopped {
                    self.play_next();
                }
                // Said after the next track starts, which clears the last error.
                self.error = Some(format!("cannot play {shown}: {e}"));
            }
        }
    }

    fn spotify_event(&mut self, event: PlayerEvent) {
        if let Some(deck) = &mut self.spotify_deck {
            deck.observe(&event);
        }
        let current = self.queue.current.as_ref().filter(|t| t.source == Source::Spotify).map(|t| t.uri.clone());
        match &event {
            PlayerEvent::TrackChanged { audio_item } => {
                if let UniqueFields::Track { artists, album, .. } = &audio_item.unique_fields
                    && let Some(mut track) = self.queue.current.clone().filter(|t| t.uri == audio_item.uri)
                {
                    track.title = audio_item.name.clone();
                    track.artist = artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
                    track.album = album.clone();
                    track.duration_ms = audio_item.duration_ms;
                    track.art = crate::spotify::cover(audio_item.covers.iter().map(|c| (c.width, c.url.clone())))
                        .or(track.art);
                    self.queue.update(&[track]);
                }
            }
            PlayerEvent::TimeToPreloadNextTrack { .. } => {
                let next = self.queue.upcoming.front().filter(|t| t.source == Source::Spotify);
                if let (Some(deck), Some(next)) = (&self.spotify_deck, next) {
                    deck.preload(&next.uri);
                }
            }
            _ => {}
        }
        if let Some((uri, finished)) = spotify::ended(&event)
            && current.as_deref() == Some(uri.as_str())
            && self.state != State::Stopped
        {
            if !finished {
                warn!("Spotify cannot play {uri}");
            }
            self.play_next();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::deck::NullDeck;
    use crate::model::Source;

    fn track(name: &str, ms: u32) -> Track {
        let mut t = Track::placeholder(Source::Local, name);
        t.duration_ms = ms;
        t
    }

    fn start() -> mpsc::UnboundedSender<Msg> {
        start_with(Arc::new(|_| Box::pin(async { bail!("these tests download nothing") })))
    }

    fn start_with(fetch: Fetch) -> mpsc::UnboundedSender<Msg> {
        start_logging(fetch, None)
    }

    fn start_logging(fetch: Fetch, log: Option<Log>) -> mpsc::UnboundedSender<Msg> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Tests run in parallel and each clears its directory, so each has its own.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let paths = crate::paths::Paths::under(&crate::testutil::scratch(&format!("engine-{n}")));
        let mut engine = Engine::new(Box::new(NullDeck::default()), 80, Spotify::new(paths), fetch, tx.clone(), Default::default());
        engine.log = log;
        tokio::spawn(engine.run(rx));
        tx
    }

    async fn enqueue(tx: &mpsc::UnboundedSender<Msg>, tracks: Vec<Track>, mode: Mode) -> Response {
        let (reply, answer) = oneshot::channel();
        tx.send(Msg::Enqueue { tracks, mode, reply }).unwrap();
        answer.await.unwrap()
    }

    async fn status(tx: &mpsc::UnboundedSender<Msg>, request: Request) -> Status {
        let (reply, answer) = oneshot::channel();
        tx.send(Msg::Control(request, reply)).unwrap();
        serde_json::from_value(answer.await.unwrap().into_result().unwrap()).unwrap()
    }

    fn title(s: &Status) -> &str {
        s.track.as_ref().map_or("", |t| t.uri.as_str())
    }

    #[tokio::test]
    async fn play_replaces_the_queue_and_next_skips() {
        let tx = start();
        enqueue(&tx, vec![track("a", 60_000), track("b", 60_000)], Mode::Replace).await;
        let s = status(&tx, Request::Status).await;
        assert_eq!((title(&s), s.state, s.queue_len), ("a", State::Playing, 1));
        let s = status(&tx, Request::Next).await;
        assert_eq!((title(&s), s.queue_len), ("b", 0));
        let s = status(&tx, Request::Next).await;
        assert_eq!((s.state, s.track), (State::Stopped, None));
    }

    #[tokio::test]
    async fn a_play_is_kept_once_it_ends_without_its_pauses() {
        let (log, plays) = Log::sink();
        let tx = start_logging(Arc::new(|_| Box::pin(async { bail!("these tests download nothing") })), Some(log));
        enqueue(&tx, vec![track("a", 60_000), track("b", 60_000)], Mode::Replace).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        status(&tx, Request::Pause).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        status(&tx, Request::Resume).await;
        assert!(plays.try_recv().is_err(), "a play is kept when it ends");
        status(&tx, Request::Next).await;
        let a = plays.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(a.track.uri, "a");
        assert!((200..450).contains(&a.ms_played), "{} ms, the pause left out", a.ms_played);

        status(&tx, Request::Stop).await;
        assert_eq!(plays.recv_timeout(Duration::from_secs(1)).unwrap().track.uri, "b");
        // Stopped, the next resume is a play of its own.
        status(&tx, Request::Resume).await;
        assert!(plays.try_recv().is_err());
    }

    #[tokio::test]
    async fn tracks_advance_by_themselves_when_they_end() {
        let tx = start();
        enqueue(&tx, vec![track("short", 50), track("after", 60_000)], Mode::Replace).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(title(&status(&tx, Request::Status).await), "after");
    }

    #[tokio::test]
    async fn adding_to_a_stopped_player_starts_it() {
        let tx = start();
        enqueue(&tx, vec![track("a", 60_000)], Mode::Append).await;
        enqueue(&tx, vec![track("c", 60_000)], Mode::Append).await;
        enqueue(&tx, vec![track("b", 60_000)], Mode::Next).await;
        let s = status(&tx, Request::Status).await;
        assert_eq!((title(&s), s.state, s.queue_len), ("a", State::Playing, 2));
        assert_eq!(title(&status(&tx, Request::Next).await), "b");
    }

    #[tokio::test]
    async fn pause_resume_and_toggle() {
        let tx = start();
        enqueue(&tx, vec![track("a", 60_000)], Mode::Replace).await;
        assert_eq!(status(&tx, Request::Pause).await.state, State::Paused);
        assert_eq!(status(&tx, Request::Toggle).await.state, State::Playing);
        assert_eq!(status(&tx, Request::Toggle).await.state, State::Paused);
        assert_eq!(status(&tx, Request::Resume).await.state, State::Playing);
    }

    #[tokio::test]
    async fn stop_keeps_the_queue_and_resume_continues_it() {
        let tx = start();
        enqueue(&tx, vec![track("a", 60_000), track("b", 60_000)], Mode::Replace).await;
        let s = status(&tx, Request::Stop).await;
        assert_eq!((s.state, s.queue_len), (State::Stopped, 1));
        assert_eq!(title(&status(&tx, Request::Resume).await), "b");
    }

    #[tokio::test]
    async fn seek_and_volume() {
        let tx = start();
        let (reply, answer) = oneshot::channel();
        tx.send(Msg::Control(Request::Seek { position_ms: 1000 }, reply)).unwrap();
        assert!(!answer.await.unwrap().ok, "seeking with nothing playing is an error");
        enqueue(&tx, vec![track("a", 60_000)], Mode::Replace).await;
        let s = status(&tx, Request::Seek { position_ms: 30_000 }).await;
        assert!((30_000..31_000).contains(&s.position_ms), "{}", s.position_ms);
        assert_eq!(status(&tx, Request::Volume { percent: 150 }).await.volume, 100);
    }

    #[tokio::test]
    async fn an_empty_target_is_an_error() {
        let tx = start();
        assert!(!enqueue(&tx, vec![], Mode::Replace).await.ok);
    }

    #[tokio::test]
    async fn a_spotify_track_shows_at_once_and_reports_a_missing_sign_in() {
        let tx = start();
        let song = Track::placeholder(Source::Spotify, "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
        // The answer to play shows the song playing, before any connection.
        let answer = enqueue(&tx, vec![song], Mode::Replace).await.into_result().unwrap();
        let shown: Status = serde_json::from_value(answer["status"].clone()).unwrap();
        assert_eq!(shown.state, State::Playing);
        assert_eq!(title(&shown), "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
        let mut now = status(&tx, Request::Status).await;
        for _ in 0..50 {
            if now.state == State::Stopped {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            now = status(&tx, Request::Status).await;
        }
        assert_eq!(now.state, State::Stopped);
        assert!(now.error.unwrap().contains("agentamp login"));
    }

    #[tokio::test]
    async fn resolved_details_replace_placeholders() {
        let tx = start();
        enqueue(&tx, vec![track("a", 60_000), track("b", 60_000)], Mode::Replace).await;
        let mut b = track("b", 60_000);
        b.title = "Song B".into();
        tx.send(Msg::Resolved(vec![b])).unwrap();
        let (reply, answer) = oneshot::channel();
        tx.send(Msg::Control(Request::Queue { offset: 0, count: None }, reply)).unwrap();
        let queue = answer.await.unwrap().into_result().unwrap();
        assert_eq!(queue["upcoming"][0]["title"], "Song B");
    }

    #[tokio::test]
    async fn previous_restarts_then_goes_back() {
        let tx = start();
        enqueue(&tx, vec![track("a", 60_000), track("b", 60_000)], Mode::Replace).await;
        // With nothing played before, it restarts.
        assert_eq!(title(&status(&tx, Request::Previous).await), "a");
        status(&tx, Request::Next).await;
        status(&tx, Request::Seek { position_ms: 10_000 }).await;
        let s = status(&tx, Request::Previous).await;
        assert_eq!((title(&s), s.position_ms / 1000), ("b", 0));
        let s = status(&tx, Request::Previous).await;
        assert_eq!((title(&s), s.queue_len, s.state), ("a", 1, State::Playing));
    }

    /// A pretend yt-dlp: each download waits until the test finishes it.
    #[derive(Default)]
    struct Downloads {
        started: std::sync::Mutex<Vec<String>>,
        waiting: std::sync::Mutex<HashMap<String, oneshot::Sender<Result<Track>>>>,
    }

    impl Downloads {
        fn fetch(self: &Arc<Self>) -> Fetch {
            let downloads = self.clone();
            Arc::new(move |key| {
                let (done, result) = oneshot::channel();
                downloads.started.lock().unwrap().push(key.clone());
                downloads.waiting.lock().unwrap().insert(key, done);
                Box::pin(async move { result.await? })
            })
        }

        /// The downloads under way, in key order. A cancelled one has dropped its end.
        fn running(&self) -> Vec<String> {
            let mut keys: Vec<String> =
                self.waiting.lock().unwrap().iter().filter(|(_, done)| !done.is_closed()).map(|(k, _)| k.clone()).collect();
            keys.sort();
            keys
        }

        fn finish(&self, key: &str, result: Result<Track>) {
            let done = self.waiting.lock().unwrap().remove(key).expect("downloading");
            let _ = done.send(result);
        }
    }

    /// What `fetch` makes of the search `ytsearch1:<name>`.
    fn file(name: &str) -> Track {
        let mut t = Track::placeholder(Source::Youtube, format!("/cache/{name}.m4a"));
        t.title = name.to_uppercase();
        t.duration_ms = 60_000;
        t
    }

    fn search(name: &str) -> Track {
        crate::youtube::pending(&format!("ytsearch1:{name}"))
    }

    fn searches(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| format!("ytsearch1:{n}")).collect()
    }

    /// Waits for `done`, which the engine makes true in its own time.
    async fn eventually(what: &str, mut done: impl AsyncFnMut() -> bool) {
        for _ in 0..300 {
            if done().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never: {what}");
    }

    #[tokio::test]
    async fn a_youtube_track_answers_at_once_and_plays_when_downloaded() {
        let downloads = Arc::new(Downloads::default());
        let tx = start_with(downloads.fetch());
        let answer = enqueue(&tx, vec![search("plastic love")], Mode::Replace).await.into_result().unwrap();
        let shown: Status = serde_json::from_value(answer["status"].clone()).unwrap();
        let track = shown.track.unwrap();
        assert_eq!((shown.state, track.title.as_str(), track.downloading), (State::Playing, "plastic love", true));
        eventually("the download starts", async || downloads.running() == searches(&["plastic love"])).await;
        let (reply, answer) = oneshot::channel();
        tx.send(Msg::Control(Request::Seek { position_ms: 1000 }, reply)).unwrap();
        assert!(answer.await.unwrap().into_result().unwrap_err().to_string().contains("still downloading"));

        downloads.finish("ytsearch1:plastic love", Ok(file("plastic love")));
        eventually("it plays the file", async || title(&status(&tx, Request::Status).await) == "/cache/plastic love.m4a").await;
        let s = status(&tx, Request::Status).await;
        assert_eq!((s.state, s.track.unwrap().downloading, s.error), (State::Playing, false, None));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(status(&tx, Request::Status).await.position_ms > 0, "the clock runs from the file's start");
    }

    #[tokio::test]
    async fn downloads_go_two_at_a_time_in_play_order() {
        let downloads = Arc::new(Downloads::default());
        let tx = start_with(downloads.fetch());
        enqueue(&tx, vec![search("a"), search("b"), search("c"), search("a")], Mode::Replace).await;
        eventually("a and b download", async || downloads.running() == searches(&["a", "b"])).await;
        // A track put next overtakes b, which waits its turn again.
        enqueue(&tx, vec![search("d")], Mode::Next).await;
        eventually("d overtakes b", async || downloads.running() == searches(&["a", "d"])).await;
        downloads.finish("ytsearch1:a", Ok(file("a")));
        eventually("b goes again", async || downloads.running() == searches(&["b", "d"])).await;
        let (reply, answer) = oneshot::channel();
        tx.send(Msg::Control(Request::Queue { offset: 0, count: None }, reply)).unwrap();
        let queue = answer.await.unwrap().into_result().unwrap();
        assert_eq!(queue["current"]["uri"], "/cache/a.m4a");
        assert_eq!(queue["upcoming"][3]["uri"], "/cache/a.m4a", "one download for both");
        assert_eq!(downloads.started.lock().unwrap().iter().filter(|k| *k == "ytsearch1:a").count(), 1);
    }

    #[tokio::test]
    async fn downloads_stop_once_nothing_waits_for_them() {
        let downloads = Arc::new(Downloads::default());
        let tx = start_with(downloads.fetch());
        enqueue(&tx, vec![search("a"), search("b")], Mode::Replace).await;
        eventually("both download", async || downloads.running() == searches(&["a", "b"])).await;
        status(&tx, Request::Clear).await;
        eventually("clear cancels b", async || downloads.running() == searches(&["a"])).await;
        enqueue(&tx, vec![track("x", 60_000)], Mode::Replace).await;
        eventually("play cancels a", async || downloads.running().is_empty()).await;
    }

    #[tokio::test]
    async fn a_failed_download_leaves_the_queue_and_says_why() {
        let downloads = Arc::new(Downloads::default());
        let tx = start_with(downloads.fetch());
        enqueue(&tx, vec![search("a"), track("b", 60_000), search("c")], Mode::Replace).await;
        eventually("a and c download", async || downloads.running() == searches(&["a", "c"])).await;
        downloads.finish("ytsearch1:c", Err(anyhow::anyhow!("no such video")));
        eventually("c leaves", async || status(&tx, Request::Status).await.queue_len == 1).await;
        let s = status(&tx, Request::Status).await;
        assert_eq!((title(&s), s.error.as_deref()), ("ytsearch1:a", Some("cannot play c: no such video")));

        downloads.finish("ytsearch1:a", Err(anyhow::anyhow!("refused")));
        eventually("b plays", async || title(&status(&tx, Request::Status).await) == "b").await;
        let s = status(&tx, Request::Status).await;
        assert_eq!((s.state, s.queue_len, s.error.as_deref()), (State::Playing, 0, Some("cannot play a: refused")));
    }

    #[tokio::test]
    async fn a_track_paused_while_downloading_stays_paused() {
        let downloads = Arc::new(Downloads::default());
        let tx = start_with(downloads.fetch());
        enqueue(&tx, vec![search("a")], Mode::Replace).await;
        assert_eq!(status(&tx, Request::Pause).await.state, State::Paused);
        eventually("a downloads", async || downloads.running() == searches(&["a"])).await;
        downloads.finish("ytsearch1:a", Ok(file("a")));
        eventually("a is here", async || title(&status(&tx, Request::Status).await) == "/cache/a.m4a").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let s = status(&tx, Request::Status).await;
        assert_eq!((s.state, s.position_ms), (State::Paused, 0));
        assert_eq!(status(&tx, Request::Resume).await.state, State::Playing);
    }

    #[tokio::test]
    async fn going_back_to_a_downloaded_track_plays_its_file() {
        let downloads = Arc::new(Downloads::default());
        let tx = start_with(downloads.fetch());
        enqueue(&tx, vec![search("a"), track("b", 60_000)], Mode::Replace).await;
        eventually("a downloads", async || downloads.running() == searches(&["a"])).await;
        downloads.finish("ytsearch1:a", Ok(file("a")));
        eventually("a is here", async || title(&status(&tx, Request::Status).await) == "/cache/a.m4a").await;
        status(&tx, Request::Next).await;
        let s = status(&tx, Request::Previous).await;
        assert_eq!((title(&s), s.state), ("/cache/a.m4a", State::Playing));
        assert_eq!(downloads.started.lock().unwrap().len(), 1);
    }
}
