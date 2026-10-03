//! The terminal window: a client of the daemon, like the CLI. Closing it
//! leaves the music playing.

mod ansi;
mod art;
pub mod cover;
#[cfg(feature = "dev")]
mod dev;
mod graphics;
mod icons;
mod library;
mod spectrum;
mod view;

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::layout::Rect;

use crate::browse::Page;
use crate::ipc::Request;
use crate::model::{State, Status, Track};
use crate::paths::Paths;
pub use view::View;
use library::{Choice, Focus};
use view::{Cover, Prompt, PromptKind};

/// How often the window asks the daemon what it is doing.
const POLL: Duration = Duration::from_millis(500);
/// How long the first frame may wait for the player's state and its cover,
/// so the window opens complete instead of filling in. Both come from this
/// machine in a few milliseconds; a cover from the network is not waited for.
const FIRST_FRAME: Duration = Duration::from_millis(50);
const SEEK_STEP_MS: u32 = 10_000;
const VOLUME_STEP: u8 = 5;

/// What the link thread hears from the daemon.
enum Update {
    Snapshot { status: Box<Status>, upcoming: Vec<Track>, history: Vec<Track> },
    Answer(Result<String, String>),
    /// A cover loaded, or failed to.
    Art { art: String, loaded: Option<art::Art> },
    /// A Spotify page, or the part of one from `offset`.
    Page { target: String, offset: u32, page: Result<Page, String> },
    /// How many a shelf of the library holds.
    Count { shelf: String, total: u32 },
}

/// What wakes the window: the daemon or the terminal.
enum Wake {
    Update(Update),
    Input(Event),
    /// The terminal is gone: its window was closed without a hang-up signal.
    Closed,
}

impl From<Update> for Wake {
    fn from(update: Update) -> Self {
        Wake::Update(update)
    }
}

#[derive(Debug, PartialEq)]
enum Command {
    Send(Request),
    Quit,
}

pub fn run(paths: Paths) -> Result<()> {
    let (jobs, requests) = mpsc::channel();
    let (wakes, received) = mpsc::channel();
    let (covers, wanted) = mpsc::channel();
    let dir = paths.art();
    let (art_wakes, input_wakes) = (wakes.clone(), wakes.clone());
    #[cfg(feature = "dev")]
    let dev = dev::Dev::new(paths.log().with_file_name("tuning.txt"));
    let sound = Arc::new(Mutex::new(spectrum::Heard::default()));
    {
        let (paths, sound) = (paths.clone(), sound.clone());
        std::thread::Builder::new().name("sound".into()).spawn(move || spectrum::listen(paths, sound))?;
    }
    std::thread::Builder::new().name("daemon-link".into()).spawn(move || link(paths, requests, wakes))?;
    std::thread::Builder::new().name("cover-art".into()).spawn(move || load_art(&dir, wanted, art_wakes))?;
    crate::trace::mark("threads");
    let mut terminal = ratatui::init();
    crate::trace::mark("terminal");
    let mut graphics = graphics::Graphics::detect();
    crate::trace::mark(format!("graphics {}", if graphics.is_some() { "images" } else { "blocks" }));
    // After the image query, which reads its answers from the terminal itself.
    #[cfg(unix)]
    {
        let wakes = input_wakes.clone();
        std::thread::Builder::new().name("hang-up".into()).spawn(move || watch_hang_up(wakes))?;
    }
    std::thread::Builder::new().name("input".into()).spawn(move || read_input(input_wakes))?;
    let result = event_loop(
        &mut terminal,
        &mut graphics,
        &jobs,
        &covers,
        &received,
        &sound,
        #[cfg(feature = "dev")]
        dev,
    );
    // A closed terminal has nothing to restore or show the cursor in, and
    // the complaint about it would go to a closed stderr, which aborts.
    if matches!(result, Ok(Exit::Closed)) {
        std::mem::forget(terminal);
    } else {
        ratatui::restore();
    }
    crate::trace::flush();
    result.map(|_| ())
}

/// Why the window ended.
enum Exit {
    Quit,
    Closed,
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    graphics: &mut Option<graphics::Graphics>,
    jobs: &mpsc::Sender<Request>,
    covers: &mpsc::Sender<String>,
    wakes: &mpsc::Receiver<Wake>,
    sound: &Mutex<spectrum::Heard>,
    #[cfg(feature = "dev")] mut dev: dev::Dev,
) -> Result<Exit> {
    let mut view = View { icons: icons::from_env(), ..View::default() };
    let mut dirty = true;
    let mut frames = 0u32;
    let mut keys = 0u32;
    let mut listening = false;
    let opened = std::time::Instant::now();
    let mut heard = false;
    // When the status was read: the clock runs on from it while playing.
    let mut read_at = opened;
    let mut analyser = spectrum::Analyser::default();
    let mut wave = vec![0.0; crate::tap::KEPT];
    // When the bars last moved, and when they move next.
    let mut stepped = opened;
    let mut next_frame = opened;
    loop {
        for art in view.wanted() {
            covers.send(art)?;
        }
        let waiting = frames == 0 && !heard_all(heard, &view) && opened.elapsed() < FIRST_FRAME;
        view.since_ms = read_at.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        let now = Instant::now();
        let animating = view.status.state == State::Playing || !analyser.settled();
        if animating && now >= next_frame && !waiting {
            let tuning = view.tuning;
            let size = terminal.size()?;
            let count = view::spectrum_bars(Rect::new(0, 0, size.width, size.height), &view);
            let (rate, sounding) = {
                let sound = sound.lock().expect("sound");
                let delay = Duration::from_millis(u64::from(tuning.delay_ms));
                (sound.wave(now, delay, &mut wave), sound.sounding(now))
            };
            let playing = view.status.state == State::Playing;
            if !playing {
                wave.fill(0.0);
            }
            // A long sleep is not a long fall: the bars move at most a tenth of a second.
            let dt = now.duration_since(stepped).as_secs_f32().min(0.1);
            analyser.step(&wave, rate, count, &tuning, dt, playing && sounding);
            view.spectrum = analyser.spectrum();
            stepped = now;
            next_frame = (next_frame + view.tuning.period()).max(now);
            dirty = true;
        }
        if dirty && !waiting {
            let start = std::time::Instant::now();
            // The whole frame at once, so the terminal never shows half of it.
            ratatui::crossterm::queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
            terminal.draw(|frame| {
                view::draw(frame, &view);
                if let Some(graphics) = graphics.as_mut() {
                    graphics.draw(frame, &view);
                }
                #[cfg(feature = "dev")]
                dev.draw(frame, &view.tuning, analyser.sens);
            })?;
            ratatui::crossterm::execute!(terminal.backend_mut(), EndSynchronizedUpdate)?;
            // From before the bars moved, so the analysis counts too.
            #[cfg(feature = "dev")]
            dev.frame(Instant::now(), now.elapsed());
            dirty = false;
            frames += 1;
            if crate::trace::enabled() {
                let what = if view.status.track.is_some() { "playing" } else { "idle" };
                crate::trace::mark(format!("frame {frames} {}us {what}", start.elapsed().as_micros()));
                // Now, while nothing waits: a window closed by its terminal never returns.
                crate::trace::flush();
            }
        }
        if !listening {
            listening = true;
            crate::trace::mark("input ready");
        }
        // Sleeps until the daemon or the terminal says something, or the
        // clock turns a second, then takes all that came in, so one frame
        // shows it all.
        let wake = if waiting {
            match wakes.recv_timeout(FIRST_FRAME.saturating_sub(opened.elapsed())) {
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                wake => wake?,
            }
        } else if view.status.state == State::Playing || !analyser.settled() {
            match wakes.recv_timeout(next_frame.saturating_duration_since(Instant::now())) {
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                wake => wake?,
            }
        } else if let Some(tick) = next_second(&view) {
            match wakes.recv_timeout(tick) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    dirty = true;
                    continue;
                }
                wake => wake?,
            }
        } else {
            wakes.recv()?
        };
        for wake in std::iter::once(wake).chain(std::iter::from_fn(|| wakes.try_recv().ok())) {
            match wake {
                Wake::Update(mut update) => {
                    if crate::trace::enabled() {
                        crate::trace::mark(match &update {
                            Update::Snapshot { .. } => "applied snapshot",
                            Update::Answer(_) => "applied answer",
                            Update::Art { .. } => "applied art",
                            Update::Page { .. } => "applied page",
                            Update::Count { .. } => "applied count",
                        });
                    }
                    if let (Some(graphics), Update::Art { art, loaded: Some(loaded) }) = (graphics.as_mut(), &mut update) {
                        graphics.offer(art.clone(), std::mem::take(&mut loaded.image));
                    }
                    if matches!(update, Update::Snapshot { .. }) {
                        heard = true;
                        read_at = std::time::Instant::now();
                    }
                    dirty |= view.apply(update);
                }
                Wake::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    keys += 1;
                    crate::trace::mark(format!("key {keys}"));
                    // A control starts from where the song is now.
                    view.since_ms = read_at.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
                    view.settle_clock();
                    read_at = std::time::Instant::now();
                    #[cfg(feature = "dev")]
                    if dev.key(key, &mut view.tuning) {
                        dirty = true;
                        continue;
                    }
                    match view.key(key) {
                        Some(Command::Quit) => return Ok(Exit::Quit),
                        Some(Command::Send(request)) => jobs.send(request)?,
                        None => {}
                    }
                    dirty = true;
                }
                Wake::Input(Event::Resize(cols, rows)) => {
                    crate::trace::mark(format!("resize {cols}x{rows}"));
                    dirty = true;
                }
                Wake::Input(_) => {}
                Wake::Closed => return Ok(Exit::Closed),
            }
        }
    }
}

/// How long until the shown clock turns its next second, while it runs.
fn next_second(view: &View) -> Option<Duration> {
    (view.status.state == State::Playing && view.status.track.is_some())
        .then(|| Duration::from_millis(u64::from(1000 - view.position_ms() % 1000)))
}

/// Whether the window has what its first frame should show: the player's
/// state and, when something plays, its cover or word that there is none.
fn heard_all(heard: bool, view: &View) -> bool {
    let art = view.status.track.as_ref().and_then(|t| t.art.as_ref());
    heard && !art.is_some_and(|art| matches!(view.covers.get(art), Some(Cover::Loading) | None))
}

/// Hands the terminal's events to the window as they come, and says when
/// the terminal is gone, so the window does not outlive it.
fn read_input(wakes: mpsc::Sender<Wake>) {
    while let Ok(event) = event::read() {
        if wakes.send(Wake::Input(event)).is_err() {
            return;
        }
    }
    let _ = wakes.send(Wake::Closed);
}

/// Sleeps until the terminal hangs up. A terminal closed without sending
/// SIGHUP leaves crossterm's reader spinning instead of failing
/// (crossterm-rs/crossterm#793), so the window would live on at full CPU.
#[cfg(unix)]
fn watch_hang_up(wakes: mpsc::Sender<Wake>) {
    // No events asked for: poll still reports a hang-up or an error, and
    // ignores input, which is the reader's.
    let mut terminal = libc::pollfd { fd: libc::STDIN_FILENO, events: 0, revents: 0 };
    loop {
        // SAFETY: one valid pollfd, for as long as the call.
        let ready = unsafe { libc::poll(&mut terminal, 1, -1) };
        if ready > 0 && terminal.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            let _ = wakes.send(Wake::Closed);
            return;
        }
        if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// Talks to the daemon off the drawing thread: runs requests and polls the
/// player's state. Play and add can take seconds (a YouTube download), so
/// they run on their own threads and controls stay quick meanwhile.
fn link(paths: Paths, requests: mpsc::Receiver<Request>, updates: mpsc::Sender<Wake>) {
    let mut counted = false;
    loop {
        // First at once, so the window opens on the player's state.
        if let Ok(update) = snapshot(&paths) {
            crate::trace::mark("snapshot");
            if updates.send(update.into()).is_err() {
                return;
            }
            // Once the player is there: the window does not start it.
            if !counted {
                counted = true;
                let (paths, updates) = (paths.clone(), updates.clone());
                std::thread::spawn(move || count_shelves(&paths, &updates));
            }
        }
        match requests.recv_timeout(POLL) {
            Ok(request) => {
                let slow = matches!(request, Request::Play { .. } | Request::Add { .. } | Request::Browse { .. });
                let (paths, updates) = (paths.clone(), updates.clone());
                let job = move || {
                    let update = match &request {
                        Request::Browse { target, offset, .. } => {
                            Update::Page { target: target.clone(), offset: *offset, page: browsed(&paths, &request, true) }
                        }
                        _ => Update::Answer(answer(&paths, &request)),
                    };
                    let _ = updates.send(update.into());
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
    }
}

/// Loads covers one at a time as the window asks for them.
fn load_art(dir: &std::path::Path, wanted: mpsc::Receiver<String>, updates: mpsc::Sender<Wake>) {
    let client = librespot_core::http_client::HttpClient::new(None);
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
    for art in wanted {
        crate::trace::mark("art wanted");
        let loaded = art::load(&art, dir, &client, &runtime).inspect_err(|e| log::debug!("cover {art}: {e:#}")).ok();
        if updates.send(Update::Art { art, loaded }.into()).is_err() {
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

/// A Spotify page, as the daemon gives it.
fn browsed(paths: &Paths, request: &Request, start: bool) -> Result<Page, String> {
    let data = crate::send(paths, request, start).map_err(|e| format!("{e:#}"))?;
    serde_json::from_value(data).map_err(|e| format!("{e:#}"))
}

/// The size of each counted shelf, one item asked of each. Without a
/// Spotify sign-in they stay without one.
fn count_shelves(paths: &Paths, updates: &mpsc::Sender<Wake>) {
    for shelf in library::COUNTED {
        let request = Request::Browse { target: shelf.into(), offset: 0, count: 1 };
        let Ok(page) = browsed(paths, &request, false) else { return };
        let Some(section) = page.sections.first() else { continue };
        if updates.send(Update::Count { shelf: shelf.into(), total: section.total }.into()).is_err() {
            return;
        }
    }
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
                self.since_ms = 0;
                changed
            }
            Update::Art { art, loaded } => {
                self.covers.insert(art, loaded.map_or(Cover::Missing, |loaded| Cover::Ready(loaded.picture)));
                true
            }
            Update::Page { target, offset, page } => {
                if let Some(error) = self.library.loaded(&target, offset, page) {
                    self.message = Some(Err(error));
                }
                true
            }
            Update::Count { shelf, total } => {
                self.library.counts.insert(shelf, total);
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

    /// Folds the time run since the status was read into its position.
    fn settle_clock(&mut self) {
        self.status.position_ms = self.position_ms();
        self.since_ms = 0;
    }

    /// Acts on a key. Controls show their result at once; the next
    /// snapshot confirms it.
    fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if let Some(prompt) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let prompt = self.prompt.take()?;
                    let target = crate::here(prompt.text.trim());
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
        if let Some(command) = self.library_key(key) {
            return command;
        }
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

impl View {
    /// Acts on a key the library takes: Tab and Esc move between the
    /// player, the shelves and the pages, and while the shelves or a page
    /// have the keys, the arrows select and Enter opens or plays. None
    /// leaves the key to the player.
    fn library_key(&mut self, key: KeyEvent) -> Option<Option<Command>> {
        let library = &mut self.library;
        let by = match key.code {
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            KeyCode::PageUp => -library::LEAP,
            KeyCode::PageDown => library::LEAP,
            _ => 0,
        };
        match (library.focus, key.code) {
            (_, KeyCode::Tab) => library.focus = library.next_focus(true),
            (_, KeyCode::BackTab) => library.focus = library.next_focus(false),
            (_, KeyCode::Esc) => return library.back().then_some(None),
            (Focus::Player, _) => return None,
            // Near the end of what has loaded, more of the page is asked for.
            (_, _) if by != 0 => return Some(library.step(by).map(Command::Send)),
            (Focus::Shelves, KeyCode::Enter) => return Some(Some(Command::Send(library.open_shelf()))),
            (Focus::Page, KeyCode::Enter) => match library.choice()? {
                Choice::Open(target, title) => return Some(Some(Command::Send(library.open(&target, &title)))),
                Choice::Play(target) => {
                    self.busy = true;
                    return Some(Some(Command::Send(Request::Play { target })));
                }
            },
            (Focus::Page, KeyCode::Char('a')) => {
                let target = library.pages.last()?.hit()?.target.clone();
                if !library::plays(&target) {
                    return Some(None);
                }
                self.busy = true;
                return Some(Some(Command::Send(Request::Add { target, next: false })));
            }
            _ => return None,
        }
        Some(None)
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
