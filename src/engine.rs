//! The player: one task owns the queue and the decks, and every request and
//! playback event passes through it in order.

use std::path::Path;

use anyhow::{Result, bail};
use log::{info, warn};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::deck::Deck;
use crate::ipc::{Request, Response};
use crate::model::{Source, State, Status};
use crate::model::Track;
use crate::queue::Queue;

pub enum Msg {
    /// A control request. `play` and `add` arrive as `Enqueue`.
    Control(Request, oneshot::Sender<Response>),
    Enqueue { tracks: Vec<Track>, mode: Mode, reply: oneshot::Sender<Response> },
    /// The file deck finished the load with this generation.
    FileEnded(u64),
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
    files: Box<dyn Deck>,
    /// Counts file loads, so a late end of an old track is ignored.
    generation: u64,
    tx: mpsc::UnboundedSender<Msg>,
}

impl Engine {
    pub fn new(files: Box<dyn Deck>, volume: u8, tx: mpsc::UnboundedSender<Msg>) -> Self {
        Self { queue: Queue::default(), state: State::Stopped, volume, files, generation: 0, tx }
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
                Msg::FileEnded(generation) => {
                    if generation == self.generation && self.state != State::Stopped {
                        self.play_next();
                    }
                }
            }
        }
        self.files.release();
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
            }
            Request::Seek { position_ms } => {
                if self.queue.current.is_none() {
                    bail!("nothing is playing");
                }
                self.files.seek(position_ms)?;
            }
            Request::Queue => return Ok(serde_json::to_value(&self.queue)?),
            Request::Status | Request::Shutdown => {}
            Request::Play { .. } | Request::Add { .. } => bail!("play and add are resolved first"),
        }
        Ok(serde_json::to_value(self.status())?)
    }

    pub fn status(&self) -> Status {
        Status {
            state: self.state,
            track: self.queue.current.clone(),
            position_ms: if self.queue.current.is_some() { self.files.position_ms() } else { 0 },
            volume: self.volume,
            queue_len: self.queue.upcoming.len(),
        }
    }

    fn pause(&mut self) {
        if self.state == State::Playing {
            self.files.pause();
            self.state = State::Paused;
        }
    }

    fn resume(&mut self) {
        match self.state {
            State::Paused => {
                self.files.resume();
                self.state = State::Playing;
            }
            State::Stopped => self.play_next(),
            State::Playing => {}
        }
    }

    fn stop(&mut self) {
        self.generation += 1;
        self.files.release();
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
                    return;
                }
                Err(e) => warn!("skipping {}: {e:#}", track.label()),
            }
        }
        self.stop();
    }

    fn start(&mut self, track: &Track) -> Result<()> {
        match track.source {
            Source::Local | Source::Youtube => {
                self.generation += 1;
                let generation = self.generation;
                let tx = self.tx.clone();
                self.files.set_volume(self.volume);
                self.files.load(
                    Path::new(&track.uri),
                    track.duration_ms,
                    Box::new(move || {
                        let _ = tx.send(Msg::FileEnded(generation));
                    }),
                )
            }
            Source::Spotify => bail!("Spotify playback is not available yet"),
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
        tokio::spawn(Engine::new(Box::new(NullDeck::default()), 80, tx.clone()).run(rx));
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
}
