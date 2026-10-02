use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::model::Source;

fn track(source: Source, title: &str, artist: &str, album: &str, seconds: u32) -> Track {
    let mut t = Track::placeholder(source, format!("{title}.uri"));
    t.title = title.into();
    t.artist = artist.into();
    t.album = album.into();
    t.duration_ms = seconds * 1000;
    t
}

/// A full queue from all three sources, partway through a song.
fn demo() -> View {
    let mut view = View::default();
    let mut playing = track(Source::Spotify, "Stay With Me", "Miki Matsubara", "Pocket Park", 334);
    playing.uri = "spotify:track:2BHj31ufdEqVK5CkYDp9mA".into();
    view.status = Status {
        state: State::Playing,
        track: Some(playing),
        position_ms: 83_000,
        volume: 80,
        queue_len: 4,
        error: None,
    };
    view.upcoming = vec![
        track(Source::Youtube, "Plastic Love", "Mariya Takeuchi", "", 308),
        track(Source::Spotify, "Midnight Pretenders", "Tomoko Aran", "Fuyü-Kükan", 342),
        track(Source::Local, "September", "Earth, Wind & Fire", "The Best of Earth, Wind & Fire", 215),
        track(Source::Spotify, "First Love", "Hikaru Utada", "", 0),
    ];
    view.history = vec![
        track(Source::Local, "Ride on Time", "Tatsuro Yamashita", "Ride on Time", 357),
        track(Source::Youtube, "Fly-Day Chinatown", "Yasuha", "", 251),
    ];
    view
}

fn text(view: &View, cols: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|frame| view::draw(frame, view)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut lines = Vec::new();
    for y in 0..rows {
        let mut line = String::new();
        let mut x = 0;
        while x < cols {
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            // A wide character covers the cell after it.
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Writes the frame where `scripts/screens.sh` turns it into a PNG.
fn keep(name: &str, view: &View, cols: u16, rows: u16) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/screens");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.{cols}x{rows}.ansi")), render(view, cols, rows).unwrap()).unwrap();
}

fn press(view: &mut View, code: KeyCode) -> Option<Command> {
    view.key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn wide_windows_show_library_queue_and_now_playing() {
    let view = demo();
    let screen = text(&view, 140, 40);
    for expected in [
        "Your Library",
        "Queue",
        "Now playing",
        "Recently played",
        "Fly-Day Chinatown",
        "Midnight Pretenders",
        "Fuyü-Kükan",
        "Spotify · Tomoko Aran",
        "YouTube · Mariya Takeuchi",
        "File · Earth, Wind & Fire",
        "5:42",
        "1:23",
        "5:34",
        icons::NERD.pause,
        icons::NERD.volume_high,
    ] {
        assert!(screen.contains(expected), "missing {expected:?} in\n{screen}");
    }
    assert!(screen.contains("Stay With Me"), "{screen}");
    keep("playing", &view, 140, 40);
}

#[test]
fn narrow_windows_keep_the_queue_and_player() {
    let view = demo();
    let medium = text(&view, 90, 30);
    // No Now playing panel: its cover is the only wide run of half blocks.
    assert!(medium.contains("Your Library") && !medium.contains(&"▀".repeat(12)), "{medium}");
    let small = text(&view, 60, 24);
    assert!(!small.contains("Your Library"), "{small}");
    assert!(small.contains("Plastic Love") && small.contains("1:23"), "{small}");
    // Too narrow for the album column.
    assert!(!small.contains("Album"), "{small}");
    keep("playing", &view, 90, 30);
    keep("playing", &view, 60, 24);
}

#[test]
fn an_empty_player_says_how_to_start() {
    let view = View::default();
    let screen = text(&view, 120, 32);
    assert!(screen.contains("Nothing queued"), "{screen}");
    assert!(screen.contains("Not playing"), "{screen}");
    assert!(screen.contains("Nothing yet"), "{screen}");
    keep("empty", &view, 120, 32);
}

#[test]
fn the_prompt_collects_a_target_and_sends_it() {
    let mut view = demo();
    assert_eq!(press(&mut view, KeyCode::Char('/')), None);
    for c in "yt: lofi".chars() {
        press(&mut view, KeyCode::Char(c));
    }
    press(&mut view, KeyCode::Backspace);
    press(&mut view, KeyCode::Char('i'));
    let screen = text(&view, 120, 32);
    assert!(screen.contains("Play ▸ yt: lofi"), "{screen}");
    keep("prompt", &view, 120, 32);
    assert_eq!(
        press(&mut view, KeyCode::Enter),
        Some(Command::Send(Request::Play { target: "yt: lofi".into() }))
    );
    assert!(view.busy && view.prompt.is_none());
    assert!(text(&view, 120, 32).contains("Finding it"));

    press(&mut view, KeyCode::Char('a'));
    press(&mut view, KeyCode::Char('x'));
    assert_eq!(press(&mut view, KeyCode::Esc), None);
    assert!(view.prompt.is_none());
    // q typed into a prompt is text, not quit.
    press(&mut view, KeyCode::Char('a'));
    assert_eq!(press(&mut view, KeyCode::Char('q')), None);
    assert_eq!(
        press(&mut view, KeyCode::Enter),
        Some(Command::Send(Request::Add { target: "q".into(), next: false }))
    );
}

#[test]
fn controls_show_their_result_at_once() {
    let mut view = demo();
    assert_eq!(press(&mut view, KeyCode::Char(' ')), Some(Command::Send(Request::Toggle)));
    assert_eq!(view.status.state, State::Paused);
    assert_eq!(press(&mut view, KeyCode::Right), Some(Command::Send(Request::Seek { position_ms: 93_000 })));
    assert_eq!(press(&mut view, KeyCode::Left), Some(Command::Send(Request::Seek { position_ms: 83_000 })));
    assert_eq!(press(&mut view, KeyCode::Char('+')), Some(Command::Send(Request::Volume { percent: 85 })));
    view.status.volume = 98;
    assert_eq!(press(&mut view, KeyCode::Char('+')), Some(Command::Send(Request::Volume { percent: 100 })));
    assert_eq!(press(&mut view, KeyCode::Char('n')), Some(Command::Send(Request::Next)));
    assert_eq!(press(&mut view, KeyCode::Char('b')), Some(Command::Send(Request::Previous)));
    assert_eq!(press(&mut view, KeyCode::Char('s')), Some(Command::Send(Request::Stop)));
    assert_eq!(view.status.state, State::Stopped);
    assert_eq!(press(&mut view, KeyCode::Char('q')), Some(Command::Quit));

    // Nothing to seek in.
    let mut idle = View::default();
    assert_eq!(press(&mut idle, KeyCode::Right), None);
}

#[test]
fn answers_and_errors_reach_the_footer() {
    let mut view = demo();
    view.busy = true;
    assert!(view.apply(Update::Answer(Err("no such file: /nope.mp3".into()))));
    assert!(!view.busy);
    assert!(text(&view, 120, 32).contains("no such file: /nope.mp3"));
    keep("error", &view, 120, 32);
    // Any key clears it.
    press(&mut view, KeyCode::Char('x'));
    assert!(view.message.is_none());

    let mut stopped = View::default();
    stopped.status.error = Some("Spotify is unavailable: not signed in".into());
    assert!(text(&stopped, 100, 24).contains("Spotify is unavailable"));
}

#[test]
fn an_unchanged_snapshot_needs_no_redraw() {
    let mut view = demo();
    let same = Update::Snapshot {
        status: Box::new(view.status.clone()),
        upcoming: view.upcoming.clone(),
        history: view.history.clone(),
    };
    assert!(!view.apply(same));
    let mut later = view.status.clone();
    later.position_ms += 1000;
    let moved = Update::Snapshot { status: Box::new(later), upcoming: view.upcoming.clone(), history: view.history.clone() };
    assert!(view.apply(moved));
}

#[test]
fn frame_sizes_are_checked() {
    assert_eq!(parse_size("120x36").unwrap(), (120, 36));
    assert!(parse_size("120").is_err());
    assert!(parse_size("5x5").is_err());
}

#[test]
fn the_album_column_waits_for_an_album() {
    let mut view = demo();
    for track in view.status.track.iter_mut().chain(view.upcoming.iter_mut()) {
        track.album.clear();
    }
    let screen = text(&view, 140, 40);
    assert!(!screen.contains("Album"), "{screen}");
    assert!(screen.contains("Midnight Pretenders"), "{screen}");
}

#[test]
fn plain_icons_for_fonts_without_nerd_glyphs() {
    let mut view = demo();
    view.icons = &icons::PLAIN;
    view.status.state = State::Paused;
    view.status.volume = 30;
    let screen = text(&view, 120, 32);
    assert!(screen.contains("|◀") && screen.contains("▶|") && screen.contains(" ▶ "), "{screen}");
    assert!(!screen.contains(icons::NERD.play), "{screen}");
    keep("plain-paused", &view, 120, 32);
}

#[test]
fn now_playing_shows_a_square_cover() {
    let view = demo();
    let screen = text(&view, 140, 40);
    let row = "▀".repeat(32);
    // The panel is 32 columns inside; two columns per row make a square.
    assert_eq!(screen.lines().filter(|l| l.contains(&row)).count(), 16, "{screen}");
}

#[test]
fn a_loaded_cover_replaces_the_stand_in() {
    let mut view = demo();
    let art = "https://i.scdn.co/image/pocket-park".to_string();
    view.status.track.as_mut().unwrap().art = Some(art.clone());
    assert_eq!(view.wanted(), vec![art.clone()]);
    // Asked once, however many snapshots follow.
    assert!(view.wanted().is_empty());

    let teal = [20, 160, 150];
    let picture = cover::Picture { width: 4, height: 4, pixels: vec![teal; 16] };
    let loaded = art::Art { picture, image: image::RgbImage::new(4, 4) };
    assert!(view.apply(Update::Art { art: art.clone(), loaded: Some(loaded) }));
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|frame| view::draw(frame, &view)).unwrap();
    let buffer = terminal.backend().buffer();
    let teal = ratatui::style::Color::Rgb(20, 160, 150);
    // The Now playing panel and the player bar both show it.
    assert_eq!((buffer[(110, 5)].fg, buffer[(110, 5)].bg), (teal, teal));
    assert_eq!(buffer[(3, 35)].fg, teal);

    // A cover that fails keeps the stand-in and is not asked for again.
    view.apply(Update::Art { art: art.clone(), loaded: None });
    terminal.draw(|frame| view::draw(frame, &view)).unwrap();
    assert_ne!(terminal.backend().buffer()[(110, 5)].fg, teal);
    assert!(view.wanted().is_empty());
}

#[test]
fn tracks_without_art_ask_for_nothing() {
    let mut view = demo();
    assert!(view.wanted().is_empty());
    assert!(View::default().wanted().is_empty());
}

#[test]
fn the_queue_reads_like_an_album_page() {
    let view = demo();
    let screen = text(&view, 90, 30);
    for expected in ["Queue", "5 tracks", "Now playing", "Next in queue", "Stay With Me", "Plastic Love"] {
        assert!(screen.contains(expected), "missing {expected:?} in\n{screen}");
    }
    // First Love's length is unknown, so no total.
    assert!(!screen.contains("5 tracks ·"), "{screen}");
    keep("queue", &view, 90, 30);

    let idle = View { upcoming: vec![track(Source::Local, "September", "Earth, Wind & Fire", "", 215)], ..View::default() };
    let screen = text(&idle, 90, 30);
    assert!(screen.contains("1 track · 4 min") && !screen.contains("Now playing"), "{screen}");
}

#[test]
fn queue_totals_round_to_minutes_and_hours() {
    let long = track(Source::Spotify, "A", "", "", 3_890);
    let short = track(Source::Spotify, "B", "", "", 100);
    assert_eq!(view::summary(&[&long, &short]), "2 tracks · 1 hr 7 min");
    assert_eq!(view::summary(&[&short]), "1 track · 2 min");
}

#[test]
fn the_playing_row_dances_and_rests() {
    let moving = view::equaliser(State::Playing, 83_000);
    assert_eq!(moving.chars().count(), 3);
    assert_ne!(moving, view::equaliser(State::Playing, 83_500));
    assert_eq!(view::equaliser(State::Paused, 83_000), "▂▂▂");
    assert!(text(&demo(), 140, 40).contains(&moving));
}

#[test]
fn the_cover_tints_the_queue_header() {
    let mut view = demo();
    let art = "https://i.scdn.co/image/teal".to_string();
    view.status.track.as_mut().unwrap().art = Some(art.clone());
    let picture = cover::Picture { width: 2, height: 2, pixels: vec![[20, 160, 150]; 4] };
    view.covers.insert(art, view::Cover::Ready(picture));
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|frame| view::draw(frame, &view)).unwrap();
    let buffer = terminal.backend().buffer();
    // The queue panel starts at column 29: tinted at the top, panel grey lower down.
    let ratatui::style::Color::Rgb(r, g, b) = buffer[(60, 0)].bg else { panic!() };
    assert!(g > r + 50 && b > r + 50, "{:?}", (r, g, b));
    assert_eq!(buffer[(60, 30)].bg, ratatui::style::Color::Rgb(18, 18, 18));
}

#[test]
fn the_first_snapshot_does_not_wait_for_the_poll() {
    let paths = crate::paths::Paths::under(&crate::testutil::scratch("tui-first-snapshot"));
    let (_jobs, requests) = std::sync::mpsc::channel();
    let (wakes, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || link(paths, requests, wakes));
    // With no daemon, the snapshot is a stopped player, made at once.
    let first = received.recv_timeout(POLL / 5).expect("no snapshot before the first poll");
    assert!(matches!(first, Wake::Update(Update::Snapshot { .. })));
}
