use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::browse::{Page, Section};
use crate::model::Source;
use crate::spotify_search::Hit;
use library::Focus;

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
    // A moment of a song: heavy bass, a vocal in the middle, falling treble.
    let levels = vec![0.55, 0.85, 0.7, 0.4, 0.5, 0.75, 0.6, 0.45, 0.35, 0.25, 0.15];
    let peaks = levels.iter().zip([0.2, 0.05, 0.15, 0.2, 0.1, 0.1, 0.2, 0.1, 0.15, 0.1, 0.1]).map(|(l, p)| l + p).collect();
    view.spectrum = spectrum::Spectrum { levels, peaks };
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
fn the_clock_runs_between_snapshots() {
    let mut view = demo();
    view.since_ms = 2_400;
    assert_eq!(view.position_ms(), 85_400);
    assert!(text(&view, 140, 40).contains("1:25"), "the shown time moves on");
    assert_eq!(next_second(&view), Some(Duration::from_millis(600)));
    // Never past the end.
    view.since_ms = 1_000_000;
    assert_eq!(view.position_ms(), 334_000);
    // Paused, it stands still and nothing needs waking.
    view.since_ms = 2_400;
    view.status.state = State::Paused;
    assert_eq!(view.position_ms(), 83_000);
    assert_eq!(next_second(&view), None);
}

#[test]
fn controls_start_from_where_the_song_is() {
    let mut view = demo();
    view.since_ms = 5_000;
    view.settle_clock();
    assert_eq!((view.status.position_ms, view.since_ms), (88_000, 0));
    assert_eq!(press(&mut view, KeyCode::Right), Some(Command::Send(Request::Seek { position_ms: 98_000 })));
    // Pausing keeps the time shown, not the time last read.
    view.since_ms = 1_500;
    view.settle_clock();
    press(&mut view, KeyCode::Char(' '));
    assert_eq!(view.position_ms(), 99_500);
}

#[test]
fn a_snapshot_restarts_the_clock() {
    let mut view = demo();
    view.since_ms = 900;
    let status = Box::new(view.status.clone());
    view.apply(Update::Snapshot { status, upcoming: Vec::new(), history: Vec::new() });
    assert_eq!(view.since_ms, 0);
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
    // The queue's header, right of the library's Albums shelf.
    let header = screen.lines().find_map(|l| l.split_once("Title")).unwrap().1;
    assert!(!header.contains("Album"), "{screen}");
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
fn hour_long_tracks_keep_their_hours_in_the_queue() {
    // Nothing playing, so the player bar shows no length: this one is the row's.
    let view = View { upcoming: vec![track(Source::Local, "Mix", "DJ", "Set", 7_200)], ..View::default() };
    let screen = text(&view, 90, 30);
    assert!(screen.contains("2:00:00"), "{screen}");
    keep("queue-hours", &view, 90, 30);
}

#[test]
fn the_playing_row_dances_and_rests() {
    let spectrum = spectrum::Spectrum { levels: vec![0.0, 1.0, 0.2, 0.3, 0.5, 0.4], peaks: vec![0.0; 6] };
    // The loudest of each third: bass, middle, treble.
    assert_eq!(view::equaliser(State::Playing, &spectrum), "█▃▅");
    assert_eq!(view::equaliser(State::Paused, &spectrum), "▂▂▂");
    assert_eq!(view::equaliser(State::Playing, &spectrum::Spectrum::default()), "▁▁▁", "nothing heard yet");
    let mut view = demo();
    view.spectrum = spectrum;
    assert!(text(&view, 140, 40).contains("█▃▅"));
}

#[test]
fn the_spectrum_fills_the_foot_of_now_playing() {
    let mut view = demo();
    let area = Rect::new(0, 0, 140, 40);
    // 32 columns inside the panel: bars two wide, one apart.
    assert_eq!(view::spectrum_bars(area, &view), 11);
    // One cell wide and one apart, 16 fit.
    assert_eq!(view::spectrum_bars(area, &View { tuning: spectrum::Tuning { width: 1, ..view.tuning }, ..view.clone() }), 16);
    let mut levels = vec![0.0; 11];
    levels[0] = 1.0;
    levels[1] = 0.5;
    let mut peaks = levels.clone();
    peaks[1] = 0.9;
    view.spectrum = spectrum::Spectrum { levels, peaks };
    let screen = text(&view, 140, 40);
    let rows: Vec<&str> = screen.lines().collect();
    let column = |y: usize| rows[y].chars().skip(107).take(5).collect::<String>();
    // Eight rows high, ending at the panel's last row: the first bar full,
    // the second half, its cap hanging higher.
    assert_eq!(column(33), "██ ██");
    assert_eq!(column(30), "██ ██");
    assert_eq!(column(29), "██   ");
    assert_eq!(column(26), "██ ▔▔");
    assert_eq!(column(25), "     ", "the row above is the details'");
    assert!(screen.contains("Stay With Me"));
    // A narrow window has no panel, so the bars only feed the mark.
    assert_eq!(view::spectrum_bars(Rect::new(0, 0, 100, 40), &view), 12);
    view.status.track = None;
    assert_eq!(view::spectrum_bars(area, &view), 0);
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

#[test]
fn the_first_frame_waits_for_the_state_and_its_cover() {
    // Nothing heard yet: an empty frame would only flash.
    assert!(!heard_all(false, &View::default()));
    // Nothing plays, or it has no art: the state is all there is.
    assert!(heard_all(true, &View::default()));
    assert!(heard_all(true, &demo()));

    let mut view = demo();
    let art = "https://i.scdn.co/image/pocket-park".to_string();
    view.status.track.as_mut().unwrap().art = Some(art.clone());
    assert!(!heard_all(true, &view));
    view.wanted();
    assert!(!heard_all(true, &view), "the cover is still loading");
    // Loaded or failed, the frame can go.
    view.apply(Update::Art { art, loaded: None });
    assert!(heard_all(true, &view));
}

fn song(title: &str, artist: &str, album: &str, uri: &str, seconds: u32) -> Hit {
    Hit {
        title: title.into(),
        artist: artist.into(),
        album: Some(album.into()),
        duration_ms: Some(seconds * 1000),
        ..Hit::new(uri)
    }
}

fn section(name: &str, total: u32, items: Vec<Hit>) -> Section {
    Section { name: name.into(), total, items }
}

/// Mariya Takeuchi's page as `browse` gave it on 2026-10-03, trimmed.
fn artist_page() -> Page {
    let release = |title: &str, year, id: &str| Hit { title: title.into(), year: Some(year), ..Hit::new(&format!("spotify:album:{id}")) };
    let artist = |name: &str, id: &str| Hit { title: name.into(), ..Hit::new(&format!("spotify:artist:{id}")) };
    Page {
        title: "Mariya Takeuchi".into(),
        by: String::new(),
        sections: vec![
            section("Popular", 3, vec![
                song("Plastic Love", "Mariya Takeuchi", "Variety", "spotify:track:7rU6Iebxzlvqy5t857bKFq", 294),
                song("September", "Mariya Takeuchi", "Love Songs", "spotify:track:2BHj31ufdEqVK5CkYDp9mA", 247),
                song("Stay With Me", "Mariya Takeuchi, Tatsuro Yamashita", "Viva Yo", "spotify:track:0Gp3Zl2lWAmhDNQi4C2W3U", 334),
            ]),
            section("Releases", 10, vec![
                release("TRAD", 2014, "4h4LKGYNKz2gXaohJZKXY0"),
                release("Expressions", 2013, "3lBX7AtzE4JoZaAIBLptRx"),
            ]),
            section("Fans also like", 40, vec![
                artist("Yumi Matsutoya", "1LQQtqc1vQ1neUgZrjYlEU"),
                artist("Anri", "1QdBbxHvdw8bWbHDYKx6JC"),
            ]),
        ],
    }
}

fn playlists_page() -> Page {
    let playlist = |title: &str, owner: &str, id: &str| Hit { title: title.into(), artist: owner.into(), ..Hit::new(id) };
    Page {
        title: "Your playlists".into(),
        by: String::new(),
        sections: vec![section("Playlists", 88, vec![
            Hit { title: "Liked Songs".into(), ..Hit::new("liked") },
            playlist("Jazz Mix", "Spotify", "spotify:playlist:37i9dQZF1EQqA6klNdJvwx"),
            Hit { title: "Grene".into(), ..Hit::new("spotify:user:12572401:folder:7e65e58f6872a79a") },
            playlist("80/90s Japanese City Pop", "mert.uslu13 on Instagram", "spotify:playlist:0nz3cRJG7ZdCzdWQmIPp56"),
        ])],
    }
}

fn browse(target: &str, offset: u32) -> Option<Command> {
    Some(Command::Send(Request::Browse { target: target.into(), offset, count: 50 }))
}

fn counted(mut view: View) -> View {
    for (shelf, total) in [("liked", 9688), ("playlists", 88), ("albums", 675), ("artists", 260)] {
        view.apply(Update::Count { shelf: shelf.into(), total });
    }
    view
}

#[test]
fn the_library_opens_its_shelves_in_the_queues_place() {
    let mut view = counted(demo());
    let screen = text(&view, 140, 40);
    for expected in ["♥ Liked Songs", "9688", "Playlists", "675", "Top this month", "Recently played", "tab library"] {
        assert!(screen.contains(expected), "missing {expected:?} in\n{screen}");
    }
    assert_eq!(press(&mut view, KeyCode::Tab), None);
    assert_eq!(view.library.focus, Focus::Shelves);
    // The arrows select now, and leave the volume alone.
    assert_eq!(press(&mut view, KeyCode::Down), None);
    assert_eq!(view.status.volume, 80);
    assert!(text(&view, 140, 40).contains("▸  Playlists"), "{}", text(&view, 140, 40));
    assert_eq!(press(&mut view, KeyCode::Enter), browse("playlists", 0));
    let loading = text(&view, 140, 40);
    assert!(loading.contains("← Playlists") && loading.contains("Loading…") && !loading.contains("Next in queue"), "{loading}");

    view.apply(Update::Page { target: "playlists".into(), offset: 0, page: Ok(playlists_page()) });
    let screen = text(&view, 140, 40);
    for expected in ["← Your playlists", "Your Library", "Playlists (88 in all)", "Jazz Mix", "mert.uslu13 on Instagram", "Folder", "enter open/play"] {
        assert!(screen.contains(expected), "missing {expected:?} in\n{screen}");
    }
    assert_eq!(view.library.counts["playlists"], 88);
    // A folder opens; it cannot be queued.
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Down);
    assert_eq!(press(&mut view, KeyCode::Char('a')), None);
    assert!(!view.busy && view.prompt.is_none());
    assert_eq!(press(&mut view, KeyCode::Enter), browse("spotify:user:12572401:folder:7e65e58f6872a79a", 0));
    assert!(text(&view, 140, 40).contains("← Grene"));
    // Back to the playlists, where they were left.
    assert_eq!(press(&mut view, KeyCode::Esc), None);
    assert_eq!(view.library.pages.len(), 1);
    assert_eq!(view.library.pages[0].selected, 2);
    // A page that comes after it was left is let go.
    view.apply(Update::Page { target: "spotify:user:12572401:folder:7e65e58f6872a79a".into(), offset: 0, page: Ok(playlists_page()) });
    assert_eq!(view.library.pages.len(), 1);
    // The queue comes back.
    assert_eq!(press(&mut view, KeyCode::Esc), None);
    assert_eq!(view.library.focus, Focus::Shelves);
    assert!(text(&view, 140, 40).contains("Next in queue"));
    assert_eq!(press(&mut view, KeyCode::Esc), None);
    assert_eq!(view.library.focus, Focus::Player);
    assert_eq!(press(&mut view, KeyCode::Down), Some(Command::Send(Request::Volume { percent: 75 })));
    assert_eq!(press(&mut view, KeyCode::Esc), Some(Command::Quit));
}

#[test]
fn a_page_plays_and_adds_what_is_selected() {
    let mut view = counted(demo());
    view.library.focus = Focus::Shelves;
    view.library.shelf = 3;
    assert_eq!(press(&mut view, KeyCode::Enter), browse("artists", 0));
    view.apply(Update::Page {
        target: "artists".into(),
        offset: 0,
        page: Ok(Page { title: "Your artists".into(), by: String::new(), sections: vec![section("Artists", 260, vec![
            Hit { title: "Mariya Takeuchi".into(), ..Hit::new("spotify:artist:3WwGRA2o4Ux1RRMYaYDh7N") },
        ])] }),
    });
    assert_eq!(press(&mut view, KeyCode::Enter), browse("spotify:artist:3WwGRA2o4Ux1RRMYaYDh7N", 0));
    view.apply(Update::Page { target: "spotify:artist:3WwGRA2o4Ux1RRMYaYDh7N".into(), offset: 0, page: Ok(artist_page()) });
    let screen = text(&view, 140, 40);
    for expected in ["← Mariya Takeuchi", "  Artist", "Popular", "Releases (10 in all)", "Fans also like (40 in all)", "2014", "4:54"] {
        assert!(screen.contains(expected), "missing {expected:?} in\n{screen}");
    }
    // An artist's own songs do not repeat whose they are; a duet says so.
    assert!(!screen.contains("Plastic Love · Mariya"), "{screen}");
    assert!(screen.contains("Stay With Me · Mariya Takeuchi, Tats"), "{screen}");
    keep("library", &view, 140, 40);
    keep("library", &view, 90, 30);

    assert_eq!(press(&mut view, KeyCode::Enter), Some(Command::Send(Request::Play { target: "spotify:track:7rU6Iebxzlvqy5t857bKFq".into() })));
    assert!(view.busy);
    view.apply(Update::Answer(Ok(String::new())));
    press(&mut view, KeyCode::Down);
    assert_eq!(
        press(&mut view, KeyCode::Char('a')),
        Some(Command::Send(Request::Add { target: "spotify:track:2BHj31ufdEqVK5CkYDp9mA".into(), next: false }))
    );
    // Tab leaves the page for the player, which keeps it open.
    press(&mut view, KeyCode::Tab);
    assert_eq!(view.library.focus, Focus::Player);
    assert!(text(&view, 140, 40).contains("← Mariya Takeuchi"));
    assert_eq!(press(&mut view, KeyCode::Char('a')), None);
    assert!(view.prompt.is_some(), "a is the Add prompt again");
    press(&mut view, KeyCode::Esc);
    // Esc from the player closes the pages before it ever quits.
    assert_eq!(press(&mut view, KeyCode::Esc), None);
    assert!(view.library.pages.is_empty());
}

#[test]
fn a_long_page_loads_more_as_it_is_read() {
    let mut view = demo();
    view.library.focus = Focus::Shelves;
    assert_eq!(press(&mut view, KeyCode::Enter), browse("liked", 0));
    let fifty = |from: usize| (from..from + 50).map(|i| song(&format!("Song {i}"), "Someone", "", &format!("spotify:track:{i:022}"), 200)).collect();
    view.apply(Update::Page { target: "liked".into(), offset: 0, page: Ok(Page { title: "Liked Songs".into(), by: String::new(), sections: vec![section("Tracks", 9688, fifty(0))] }) });
    assert_eq!(view.library.counts["liked"], 9688);
    assert_eq!(press(&mut view, KeyCode::PageDown), None);
    assert_eq!(press(&mut view, KeyCode::PageDown), None);
    assert_eq!(press(&mut view, KeyCode::PageDown), None);
    assert_eq!(press(&mut view, KeyCode::PageDown), browse("liked", 50), "ten from the end");
    assert_eq!(press(&mut view, KeyCode::Down), None, "asked for once");
    let screen = text(&view, 120, 32);
    // Numbered as wide as the 9688th will be.
    assert!(screen.contains("▸  42  Song 41"), "the selection stays in sight\n{screen}");
    assert!(view.library.pages[0].more);
    view.apply(Update::Page { target: "liked".into(), offset: 50, page: Ok(Page { title: "Liked Songs".into(), by: String::new(), sections: vec![section("Tracks", 9688, fifty(50))] }) });
    assert_eq!(view.library.pages[0].items().count(), 100);
    // The bottom stops at the end of what has loaded.
    for _ in 0..20 {
        press(&mut view, KeyCode::PageDown);
    }
    assert_eq!(view.library.pages[0].selected, 99);
    // A failure says why, and lets a later step ask again.
    view.apply(Update::Page { target: "liked".into(), offset: 100, page: Err("Spotify did not answer".into()) });
    assert_eq!(view.message, Some(Err("Spotify did not answer".into())));
    assert!(!view.library.pages[0].more);
}

#[test]
fn more_of_an_artist_grows_only_the_releases() {
    let mut view = demo();
    let uri = "spotify:artist:3WwGRA2o4Ux1RRMYaYDh7N";
    view.library.open(uri, "Mariya Takeuchi");
    view.apply(Update::Page { target: uri.into(), offset: 0, page: Ok(artist_page()) });
    // The Fans also like section has more too, but it is not paged.
    let mut more = artist_page();
    more.sections[1].items.truncate(1);
    more.sections[2].items.clear();
    view.apply(Update::Page { target: uri.into(), offset: 2, page: Ok(more) });
    let page = view.library.pages[0].page.clone().unwrap().unwrap();
    let sizes: Vec<usize> = page.sections.iter().map(|s| s.items.len()).collect();
    assert_eq!(sizes, [3, 3, 2]);
}

#[test]
fn a_narrow_window_shows_the_library_while_it_has_the_keys() {
    let mut view = counted(demo());
    assert!(!text(&view, 60, 24).contains("Liked Songs"));
    press(&mut view, KeyCode::Tab);
    let screen = text(&view, 60, 24);
    assert!(screen.contains("♥ Liked Songs") && !screen.contains("Next in queue"), "{screen}");
    press(&mut view, KeyCode::Enter);
    assert!(text(&view, 60, 24).contains("← Liked Songs"));
}
