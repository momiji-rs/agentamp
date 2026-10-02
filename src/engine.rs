//! The player: one task owns the queue and the decks, and every request and
//! playback event passes through it in order.

use std::sync::Arc;

use anyhow::{Result, bail};
use librespot_metadata::audio::UniqueFields;
use librespot_playback::player::PlayerEvent;
use log::{info, warn};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::deck::Deck;
use crate::ipc::{Request, Response};
use crate::model::{Source, State, Status, Track};
use crate::queue::Queue;
use crate::spotify::{self, Spotify, SpotifyDeck};

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
    tx: mpsc::UnboundedSender<Msg>,
}

impl Engine {
    pub fn new(files: Box<dyn Deck>, volume: u8, spotify: Arc<Spotify>, tx: mpsc::UnboundedSender<Msg>) -> Self {
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
            tx,
        }
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
                Msg::Resolved(tracks) => {
                    for track in &tracks {
                        self.queue.update(track);
                    }
                }
                Msg::FileEnded(generation) => {
                    if generation == self.generation && self.state != State::Stopped {
                        self.play_next();
                    }
                }
                Msg::Spotify(event) => self.spotify_event(event),
                Msg::SessionReady(result) => self.session_ready(result),
            }
        }
        self.files.release();
        if let Some(deck) = &mut self.spotify_deck {
            deck.stop();
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
            Request::Stop => self.stop(),
            Request::Clear => self.queue.clear(),
            Request::Volume { percent } => {
                self.volume = percent.min(100);
                self.files.set_volume(self.volume);
                if let Some(deck) = &mut self.spotify_deck {
                    deck.set_volume(self.volume);
                }
            }
            Request::Seek { position_ms } => match self.deck() {
                Some(deck) => deck.seek(position_ms)?,
                None => bail!("nothing is playing"),
            },
            Request::Queue => return Ok(serde_json::to_value(&self.queue)?),
            Request::Status | Request::Shutdown => {}
            Request::Play { .. } | Request::Add { .. } => bail!("play and add are resolved first"),
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
    fn start(&mut self, track: &Track) -> Result<()> {
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
                    let (deck, mut events) = SpotifyDeck::new(session, self.volume)?;
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
                    self.queue.update(&track);
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
        let (tx, rx) = mpsc::unbounded_channel();
        let paths = crate::paths::Paths::under(&crate::testutil::scratch("engine"));
        tokio::spawn(Engine::new(Box::new(NullDeck::default()), 80, Spotify::new(paths), tx.clone()).run(rx));
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
        tx.send(Msg::Control(Request::Queue, reply)).unwrap();
        let queue = answer.await.unwrap().into_result().unwrap();
        assert_eq!(queue["upcoming"][0]["title"], "Song B");
    }
}
