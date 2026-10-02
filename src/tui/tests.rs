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
    let mut playing = track(Source::Spotify, "真夜中のドア〜stay with me", "Miki Matsubara", "Pocket Park", 334);
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
        track(Source::Spotify, "Stay With Me", "Hikaru Utada", "", 0),
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
        "80%",
    ] {
        assert!(screen.contains(expected), "missing {expected:?} in\n{screen}");
    }
    assert!(screen.contains("真夜中のドア"), "{screen}");
    keep("playing", &view, 140, 40);
}

#[test]
fn narrow_windows_keep_the_queue_and_player() {
    let view = demo();
    let medium = text(&view, 90, 30);
    assert!(medium.contains("Your Library") && !medium.contains("Now playing"), "{medium}");
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
        status: view.status.clone(),
        upcoming: view.upcoming.clone(),
        history: view.history.clone(),
    };
    assert!(!view.apply(same));
    let mut later = view.status.clone();
    later.position_ms += 1000;
    let moved = Update::Snapshot { status: later, upcoming: view.upcoming.clone(), history: view.history.clone() };
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
