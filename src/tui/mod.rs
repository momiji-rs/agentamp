//! The terminal window: a client of the daemon, like the CLI. Closing it
//! leaves the music playing.

mod ansi;
mod art;
mod cover;
mod graphics;
mod icons;
mod view;

use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::ipc::Request;
use crate::model::{State, Status, Track};
use crate::paths::Paths;
pub use view::View;
use view::{Cover, Prompt, PromptKind};

/// How often the window asks the daemon what it is doing.
const POLL: Duration = Duration::from_millis(500);
/// How long a key press may wait for the window to notice it.
const INPUT: Duration = Duration::from_millis(100);
const SEEK_STEP_MS: u32 = 10_000;
const VOLUME_STEP: u8 = 5;

/// What the link thread hears from the daemon.
enum Update {
    Snapshot { status: Box<Status>, upcoming: Vec<Track>, history: Vec<Track> },
    Answer(Result<String, String>),
    /// A cover loaded, or failed to.
    Art { art: String, loaded: Option<art::Art> },
}

#[derive(Debug, PartialEq)]
enum Command {
    Send(Request),
    Quit,
}

pub fn run(paths: Paths) -> Result<()> {
    let (jobs, requests) = mpsc::channel();
    let (updates, received) = mpsc::channel();
    let (covers, wanted) = mpsc::channel();
    let dir = paths.art();
    let art_updates = updates.clone();
    std::thread::Builder::new().name("daemon-link".into()).spawn(move || link(paths, requests, updates))?;
    std::thread::Builder::new().name("cover-art".into()).spawn(move || load_art(&dir, wanted, art_updates))?;
    crate::trace::mark("threads");
    let mut terminal = ratatui::init();
    crate::trace::mark("terminal");
    let mut graphics = graphics::Graphics::detect();
    crate::trace::mark(format!("graphics {}", if graphics.is_some() { "images" } else { "blocks" }));
    let result = event_loop(&mut terminal, &mut graphics, &jobs, &covers, &received);
    ratatui::restore();
    crate::trace::flush();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    graphics: &mut Option<graphics::Graphics>,
    jobs: &mpsc::Sender<Request>,
    covers: &mpsc::Sender<String>,
    updates: &mpsc::Receiver<Update>,
) -> Result<()> {
    let mut view = View { icons: icons::from_env(), ..View::default() };
    let mut dirty = true;
    let mut frames = 0u32;
    let mut keys = 0u32;
    let mut listening = false;
    loop {
        while let Ok(mut update) = updates.try_recv() {
            if crate::trace::enabled() {
                crate::trace::mark(match &update {
                    Update::Snapshot { .. } => "applied snapshot",
                    Update::Answer(_) => "applied answer",
                    Update::Art { .. } => "applied art",
                });
            }
            if let (Some(graphics), Update::Art { art, loaded: Some(loaded) }) = (graphics.as_mut(), &mut update) {
                graphics.offer(art.clone(), std::mem::take(&mut loaded.image));
            }
            dirty |= view.apply(update);
        }
        for art in view.wanted() {
            covers.send(art)?;
        }
        if dirty {
            let start = std::time::Instant::now();
            terminal.draw(|frame| {
                view::draw(frame, &view);
                if let Some(graphics) = graphics.as_mut() {
                    graphics.draw(frame, &view);
                }
            })?;
            dirty = false;
            frames += 1;
            if crate::trace::enabled() {
                let what = if view.status.track.is_some() { "playing" } else { "idle" };
                crate::trace::mark(format!("frame {frames} {}us {what}", start.elapsed().as_micros()));
            }
        }
        if !listening {
            listening = true;
            crate::trace::mark("input ready");
        }
        if !event::poll(INPUT)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                keys += 1;
                crate::trace::mark(format!("key {keys}"));
                match view.key(key) {
                    Some(Command::Quit) => return Ok(()),
                    Some(Command::Send(request)) => jobs.send(request)?,
                    None => {}
                }
                dirty = true;
            }
            Event::Resize(..) => dirty = true,
            _ => {}
        }
    }
}

/// Talks to the daemon off the drawing thread: runs requests and polls the
/// player's state. Play and add can take seconds (a YouTube download), so
/// they run on their own threads and controls stay quick meanwhile.
fn link(paths: Paths, requests: mpsc::Receiver<Request>, updates: mpsc::Sender<Update>) {
    loop {
        match requests.recv_timeout(POLL) {
            Ok(request) => {
                let slow = matches!(request, Request::Play { .. } | Request::Add { .. });
                let (paths, updates) = (paths.clone(), updates.clone());
                let job = move || {
                    let _ = updates.send(Update::Answer(answer(&paths, &request)));
                };
                if slow {
                    std::thread::spawn(job);
                } else {
                    job();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let Ok(update) = snapshot(&paths) else { continue };
        crate::trace::mark("snapshot");
        if updates.send(update).is_err() {
            return;
        }
    }
}

/// Loads covers one at a time as the window asks for them.
fn load_art(dir: &std::path::Path, wanted: mpsc::Receiver<String>, updates: mpsc::Sender<Update>) {
    let client = librespot_core::http_client::HttpClient::new(None);
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
    for art in wanted {
        crate::trace::mark("art wanted");
        let loaded = art::load(&art, dir, &client, &runtime).inspect_err(|e| log::debug!("cover {art}: {e:#}")).ok();
        if updates.send(Update::Art { art, loaded }).is_err() {
            return;
        }
        crate::trace::mark("art loaded");
    }
}

/// What the footer says about a request: what play and add found, and any
/// error. The window already shows what a control did.
fn answer(paths: &Paths, request: &Request) -> Result<String, String> {
    let data = crate::send(paths, request, true).map_err(|e| format!("{e:#}"))?;
    Ok(match request {
        Request::Play { .. } | Request::Add { .. } => crate::describe(request, &data).unwrap_or_default(),
        _ => String::new(),
    })
}

/// The player's state, without starting it.
fn snapshot(paths: &Paths) -> Result<Update> {
    let status = Box::new(serde_json::from_value(crate::send(paths, &Request::Status, false)?)?);
    let queue = crate::send(paths, &Request::Queue, false)?;
    let tracks = |key: &str| -> Vec<Track> { serde_json::from_value(queue[key].clone()).unwrap_or_default() };
    Ok(Update::Snapshot { status, upcoming: tracks("upcoming"), history: tracks("history") })
}

/// One frame of the window at `cols`×`rows`, as terminal bytes, with the
/// playing track's cover loaded first.
pub fn frame(paths: &Paths, cols: u16, rows: u16) -> Result<String> {
    let mut view = View { icons: icons::from_env(), ..View::default() };
    view.apply(snapshot(paths)?);
    let client = librespot_core::http_client::HttpClient::new(None);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    for art in view.wanted() {
        let loaded = art::load(&art, &paths.art(), &client, &runtime).ok();
        view.apply(Update::Art { art, loaded });
    }
    render(&view, cols, rows)
}

fn render(view: &View, cols: u16, rows: u16) -> Result<String> {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows))?;
    terminal.draw(|frame| view::draw(frame, view))?;
    Ok(ansi::encode(terminal.backend().buffer()))
}

impl View {
    /// Takes in what the daemon said; true when the window must redraw.
    fn apply(&mut self, update: Update) -> bool {
        match update {
            Update::Snapshot { status, upcoming, history } => {
                let status = *status;
                let changed = status != self.status || upcoming != self.upcoming || history != self.history;
                self.status = status;
                self.upcoming = upcoming;
                self.history = history;
                changed
            }
            Update::Art { art, loaded } => {
                self.covers.insert(art, loaded.map_or(Cover::Missing, |loaded| Cover::Ready(loaded.picture)));
                true
            }
            Update::Answer(answer) => {
                self.busy = false;
                self.message = match answer {
                    Ok(text) if text.is_empty() => None,
                    answer => Some(answer),
                };
                true
            }
        }
    }

    /// Covers to load: the playing track's, once. Covers of tracks no longer
    /// in the queue are forgotten; the disk cache keeps them.
    fn wanted(&mut self) -> Vec<String> {
        let Some(art) = self.status.track.as_ref().and_then(|t| t.art.clone()) else { return Vec::new() };
        if self.covers.contains_key(&art) {
            return Vec::new();
        }
        let keep: std::collections::HashSet<&String> =
            self.upcoming.iter().chain(&self.history).filter_map(|t| t.art.as_ref()).collect();
        self.covers.retain(|key, _| keep.contains(key));
        self.covers.insert(art.clone(), Cover::Loading);
        vec![art]
    }

    /// Acts on a key. Controls show their result at once; the next
    /// snapshot confirms it.
    fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if let Some(prompt) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let prompt = self.prompt.take()?;
                    let target = prompt.text.trim().to_string();
                    if target.is_empty() {
                        return None;
                    }
                    self.busy = true;
                    self.message = None;
                    return Some(Command::Send(match prompt.kind {
                        PromptKind::Play => Request::Play { target },
                        PromptKind::Add => Request::Add { target, next: false },
                    }));
                }
                KeyCode::Backspace => {
                    prompt.text.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => prompt.text.clear(),
                KeyCode::Char(c) => prompt.text.push(c),
                _ => {}
            }
            return None;
        }
        self.message = None;
        let status = &mut self.status;
        let request = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Command::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Some(Command::Quit),
            KeyCode::Char(' ') | KeyCode::Char('p') => {
                status.state = match status.state {
                    State::Playing => State::Paused,
                    State::Paused => State::Playing,
                    State::Stopped => State::Stopped,
                };
                Request::Toggle
            }
            KeyCode::Char('n') => Request::Next,
            KeyCode::Char('b') => Request::Previous,
            KeyCode::Char('s') => {
                status.state = State::Stopped;
                Request::Stop
            }
            KeyCode::Right | KeyCode::Left => {
                status.track.as_ref()?;
                let length = status.track.as_ref().map_or(0, |t| t.duration_ms);
                status.position_ms = if key.code == KeyCode::Right {
                    let ahead = status.position_ms + SEEK_STEP_MS;
                    if length > 0 { ahead.min(length) } else { ahead }
                } else {
                    status.position_ms.saturating_sub(SEEK_STEP_MS)
                };
                Request::Seek { position_ms: status.position_ms }
            }
            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Up => {
                status.volume = (status.volume + VOLUME_STEP).min(100);
                Request::Volume { percent: status.volume }
            }
            KeyCode::Char('-') | KeyCode::Down => {
                status.volume = status.volume.saturating_sub(VOLUME_STEP);
                Request::Volume { percent: status.volume }
            }
            KeyCode::Char('/') => {
                self.prompt = Some(Prompt { kind: PromptKind::Play, text: String::new() });
                return None;
            }
            KeyCode::Char('a') => {
                self.prompt = Some(Prompt { kind: PromptKind::Add, text: String::new() });
                return None;
            }
            _ => return None,
        };
        Some(Command::Send(request))
    }
}

/// Parses `--frame 120x36`.
pub fn parse_size(text: &str) -> Result<(u16, u16)> {
    let (cols, rows) = text.split_once('x').ok_or_else(|| anyhow::anyhow!("give the size as COLSxROWS"))?;
    let (cols, rows) = (cols.parse()?, rows.parse()?);
    anyhow::ensure!((20..=500).contains(&cols) && (8..=200).contains(&rows), "size {text} is out of range");
    Ok((cols, rows))
}

#[cfg(test)]
mod tests;
