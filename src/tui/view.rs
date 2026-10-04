//! What the terminal window shows, drawn from one snapshot of the player.
//! Drawing is pure: the same view and size always give the same cells.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table};

use super::cover::Picture;
use super::icons::{self, Icons};
use super::library::{self, Focus, Library, Opened, SHELVES};
use super::spectrum::{Spectrum, Tuning};
use crate::model::{Source, State, Status, Track, clock};
use crate::spotify_search::Hit;

const BG: Color = Color::Rgb(0, 0, 0);
const PANEL: Color = Color::Rgb(18, 18, 18);
const TRACK: Color = Color::Rgb(77, 77, 77);
const TEXT: Color = Color::Rgb(255, 255, 255);
const SUBDUED: Color = Color::Rgb(167, 167, 167);
const FAINT: Color = Color::Rgb(100, 100, 100);
const GREEN: Color = Color::Rgb(30, 215, 96);
const RED: Color = Color::Rgb(241, 94, 108);
const YOUTUBE: Color = Color::Rgb(255, 51, 51);
const FILES: Color = Color::Rgb(80, 155, 245);
const RULE: Color = Color::Rgb(90, 90, 90);
const CAP: Color = Color::Rgb(220, 220, 220);
const SELECTED: Color = Color::Rgb(42, 42, 42);

/// Below these widths the side panels give their room to the queue.
const WITH_BOTH_PANELS: u16 = 110;
const WITH_LIBRARY: u16 = 80;
/// Spotify caps its progress bar's width; so does the window.
const MAX_PROGRESS: u16 = 72;
const VOLUME_WIDTH: u16 = 14;
/// How far down the queue the cover's tint reaches.
const FADE_ROWS: u16 = 18;
/// The spectrum's tallest, and the rows it leaves above it for the details.
const SPECTRUM_ROWS: u16 = 8;
const SPECTRUM_ABOVE: u16 = 7;
/// Bars behind the playing row's mark when the spectrum is not shown.
const MARK_BARS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Play,
    Add,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
}

/// A track's cover, by its `art`.
#[derive(Clone, Debug, PartialEq)]
pub enum Cover {
    Loading,
    /// It could not be fetched or read; the stand-in stays.
    Missing,
    Ready(Picture),
}

/// What the whole queue holds when the window has only its start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Queued {
    /// The current track and every upcoming one.
    pub tracks: usize,
    /// Their length together, when every one is known.
    pub length_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub status: Status,
    /// The start of what plays next.
    pub upcoming: Vec<Track>,
    /// The whole queue's size, when `upcoming` may be only its start.
    pub queued: Option<Queued>,
    /// Played tracks, oldest first.
    pub history: Vec<Track>,
    pub prompt: Option<Prompt>,
    /// The answer to the last request: a confirmation, or an error.
    pub message: Option<Result<String, String>>,
    /// A play or add is still being resolved.
    pub busy: bool,
    pub icons: &'static Icons,
    pub covers: HashMap<String, Cover>,
    /// How long ago the status was read, so the clock runs between reads.
    pub since_ms: u32,
    /// The bars of the sound as it plays, from the window's analyser.
    pub spectrum: Spectrum,
    /// What shapes them. Only developer mode changes it.
    pub tuning: Tuning,
    /// The library's shelves and the pages opened from them.
    pub library: Library,
}

impl Default for View {
    fn default() -> Self {
        Self {
            status: Status {
                state: State::Stopped,
                track: None,
                position_ms: 0,
                volume: 0,
                queue_len: 0,
                error: None,
            },
            upcoming: Vec::new(),
            queued: None,
            history: Vec::new(),
            prompt: None,
            message: None,
            busy: false,
            icons: &icons::NERD,
            covers: HashMap::new(),
            since_ms: 0,
            spectrum: Spectrum::default(),
            tuning: Tuning::DEFAULT,
            library: Library::default(),
        }
    }
}

impl View {
    /// Where the song is now: the status's position, moved on by the time
    /// since it was read while playing, and never past the end.
    pub fn position_ms(&self) -> u32 {
        let status = &self.status;
        if status.state != State::Playing {
            return status.position_ms;
        }
        let now = status.position_ms.saturating_add(self.since_ms);
        match status.track.as_ref().map_or(0, |t| t.duration_ms) {
            0 => now,
            length => now.min(length),
        }
    }
}

/// The panels across the top, the player bar and the footer.
fn regions(area: Rect) -> (std::rc::Rc<[Rect]>, Rect, Rect) {
    let [main, player, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3), Constraint::Length(1)]).spacing(1).areas(area);
    let panels: Vec<Constraint> = if area.width >= WITH_BOTH_PANELS {
        vec![Constraint::Length(28), Constraint::Min(40), Constraint::Length(34)]
    } else if area.width >= WITH_LIBRARY {
        vec![Constraint::Length(26), Constraint::Min(40)]
    } else {
        vec![Constraint::Min(0)]
    };
    (Layout::horizontal(panels).spacing(1).split(main), player, footer)
}

/// How many bars the analyser should make for a window of `area`: the
/// spectrum's in the Now playing panel, or enough for the playing row's
/// mark when the panel is not shown.
pub fn spectrum_bars(area: Rect, view: &View) -> usize {
    match spectrum_area(area, view) {
        Some(rect) => view.tuning.bars(rect.width),
        None if view.status.track.is_some() => MARK_BARS,
        None => 0,
    }
}

fn spectrum_area(area: Rect, view: &View) -> Option<Rect> {
    view.status.track.as_ref()?;
    let (columns, _, _) = regions(area);
    if columns.len() != 3 {
        return None;
    }
    let (_, details) = now_playing_layout(panel("").inner(columns[2]));
    let height = details.height.saturating_sub(SPECTRUM_ABOVE).min(SPECTRUM_ROWS);
    (height >= 3).then(|| Rect { y: details.bottom() - height, height, ..details })
}

/// Where the playing track's cover goes, with the colour around it: the
/// Now playing panel, when there is room for it, and the player bar.
pub fn cover_areas(area: Rect, view: &View) -> Vec<(Rect, Color)> {
    if view.status.track.is_none() {
        return Vec::new();
    }
    let (columns, player, _) = regions(area);
    let mut areas = Vec::new();
    if columns.len() == 3 {
        areas.push((now_playing_layout(panel("").inner(columns[2])).0, PANEL));
    }
    areas.push((player_bar_layout(player)[0].0, BG));
    areas.into_iter().filter(|(rect, _)| !rect.is_empty()).collect()
}

pub fn draw(frame: &mut Frame, view: &View) {
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::new().bg(BG)), area);
    let (columns, player, footer) = regions(area);
    // An open page takes the queue's place.
    let middle = |frame: &mut Frame, area| match view.library.pages.last() {
        Some(opened) => page(frame, area, view, opened),
        None => queue(frame, area, view),
    };
    match columns.len() {
        3 => {
            library(frame, columns[0], view);
            middle(frame, columns[1]);
            now_playing(frame, columns[2], view);
        }
        2 => {
            library(frame, columns[0], view);
            middle(frame, columns[1]);
        }
        // Too narrow for the library beside the rest: it shows while it has the keys.
        _ if view.library.focus == Focus::Shelves => library(frame, columns[0], view),
        _ => middle(frame, columns[0]),
    }
    player_bar(frame, player, view);
    footer_line(frame, footer, view);
}

/// A surface lighter than the background, with its title in the first row,
/// as Spotify draws its panels.
fn panel(title: &str) -> Block<'_> {
    Block::new()
        .title(Span::styled(format!(" {title}"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)))
        .padding(Padding::new(1, 1, 1, 0))
        .style(Style::new().bg(PANEL))
}

fn source_name(source: Source) -> (&'static str, Color) {
    match source {
        Source::Spotify => ("Spotify", GREEN),
        Source::Youtube => ("YouTube", YOUTUBE),
        Source::Local => ("File", FILES),
    }
}

/// "Spotify · Artist": where a track comes from, then who made it.
fn byline(track: &Track) -> Line<'static> {
    let (name, color) = source_name(track.source);
    let mut spans = vec![Span::styled(name, Style::new().fg(color))];
    if !track.artist.is_empty() {
        spans.push(Span::styled(format!(" · {}", track.artist), Style::new().fg(SUBDUED)));
    }
    Line::from(spans)
}

/// The shelves, then what played last.
fn library(frame: &mut Frame, area: Rect, view: &View) {
    let block = panel("Your Library");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let shelves = &view.library;
    let open = shelves.pages.first().map(|o| o.target.as_str());
    // From the panel's edge, so the selection's mark sits in its padding.
    let rows = Rect { x: area.x, width: inner.right() - area.x, ..inner };
    let mut lines: Vec<Line> = SHELVES
        .iter()
        .enumerate()
        .map(|(i, (target, name))| {
            let marked = shelves.focus == Focus::Shelves && i == shelves.shelf;
            let name_style = if open == Some(*target) { Style::new().fg(GREEN) } else { Style::new().fg(TEXT) };
            let count = shelves.counts.get(*target).map(|n| n.to_string()).unwrap_or_default();
            let used = 3 + unicode_width::UnicodeWidthStr::width(*name) + count.len();
            Line::from(vec![
                Span::styled(if marked { "▸" } else { " " }, Style::new().fg(GREEN)),
                Span::styled(if *target == "liked" { "♥ " } else { "  " }, Style::new().fg(TEXT)),
                Span::styled(*name, name_style),
                Span::raw(" ".repeat(usize::from(rows.width).saturating_sub(used))),
                Span::styled(count, Style::new().fg(SUBDUED)),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(" Recently played", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
    if view.history.is_empty() {
        lines.push(Line::styled(" Nothing yet", Style::new().fg(FAINT)));
    }
    for track in view.history.iter().rev() {
        lines.push(Line::raw(format!(" {}", track.title)));
        let mut by = byline(track);
        by.spans.insert(0, Span::raw(" "));
        lines.push(by);
    }
    frame.render_widget(Paragraph::new(lines).style(Style::new().fg(TEXT)), rows);
}

/// What a page is, under its title: "Album · Mariya Takeuchi".
fn kind(opened: &Opened, by: &str) -> String {
    let target = opened.target.as_str();
    let kind = if target.contains(":folder:") {
        "Folder"
    } else if target == "liked" {
        "Playlist"
    } else if let Some(rest) = target.strip_prefix("spotify:") {
        match rest.split(':').next() {
            Some("artist") => "Artist",
            Some("album") => "Album",
            Some("playlist") => "Playlist",
            _ => "",
        }
    } else if target == "top" {
        ""
    } else {
        "Your Library"
    };
    [kind, by].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// How a section's rows are laid out: the width of its numbers, and
/// whether it has a middle column.
#[derive(Clone, Copy)]
struct Columns {
    number: u16,
    wide: bool,
}

/// One row of a page.
enum Entry<'a> {
    Heading(String),
    Note(String, Color),
    Blank,
    /// An item, its place across the page, its number in its section, and
    /// its section's columns.
    Item(&'a Hit, usize, usize, Columns),
}

/// A Spotify page in the queue's place: its title, then each section's
/// items, one a row, scrolled to keep the selection in sight.
fn page(frame: &mut Frame, area: Rect, view: &View, opened: &Opened) {
    frame.render_widget(Block::new().style(Style::new().bg(PANEL)), area);
    fade(frame.buffer_mut(), area, [40, 40, 40]);
    let inner = area.inner(Margin::new(1, 0));
    let loaded = opened.page.as_ref().and_then(|p| p.as_ref().ok());
    let title = loaded.map_or(opened.title.as_str(), |p| p.title.as_str());
    let by = loaded.map_or("", |p| p.by.as_str());
    let header = vec![
        Line::from(vec![
            Span::styled("← ", Style::new().fg(SUBDUED)),
            Span::styled(title.to_string(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        ]),
        Line::styled(format!("  {}", kind(opened, by)), Style::new().fg(SUBDUED)),
    ];
    let mut rest = inner;
    frame.render_widget(Paragraph::new(header), take(&mut rest, 3));

    let mut entries = Vec::new();
    match &opened.page {
        None => entries.push(Entry::Note("Loading…".into(), SUBDUED)),
        Some(Err(error)) => entries.push(Entry::Note(error.clone(), RED)),
        Some(Ok(page)) => {
            let mut at = 0;
            for (i, section) in page.sections.iter().enumerate() {
                if i > 0 {
                    entries.push(Entry::Blank);
                }
                let shown = section.items.len() as u32;
                entries.push(Entry::Heading(if section.total > shown {
                    format!("{} ({} in all)", section.name, section.total)
                } else {
                    section.name.clone()
                }));
                if section.items.is_empty() {
                    entries.push(Entry::Note("none here".into(), FAINT));
                }
                // The middle column only when some item has something to put in it.
                let wide = rest.width >= 60 && section.items.iter().any(|hit| !detail(hit).is_empty());
                // As wide as the last number of all, so the column holds still while it loads.
                let columns = Columns { number: (section.total.max(shown).to_string().len() as u16).max(3), wide };
                for (n, hit) in section.items.iter().enumerate() {
                    entries.push(Entry::Item(hit, at, n + 1, columns));
                    at += 1;
                }
            }
            if opened.more {
                entries.push(Entry::Note("Loading more…".into(), SUBDUED));
            }
        }
    }
    let focused = view.library.focus == Focus::Page;
    let selected = entries.iter().position(|e| matches!(e, Entry::Item(_, at, _, _) if *at == opened.selected));
    // The selection stays a row clear of the bottom; the top stays put until it must move.
    let height = usize::from(rest.height);
    let first = selected.map_or(0, |s| (s + 2).saturating_sub(height));
    for (row, entry) in entries.iter().skip(first).take(height).enumerate() {
        let line = Rect { y: rest.y + row as u16, height: 1, ..rest };
        match entry {
            Entry::Heading(text) => {
                frame.render_widget(Paragraph::new(Line::styled(text.clone(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))), line)
            }
            Entry::Note(text, color) => frame.render_widget(Paragraph::new(Line::styled(format!(" {text}"), Style::new().fg(*color))), line),
            Entry::Blank => {}
            Entry::Item(hit, at, n, columns) => {
                let chosen = focused && *at == opened.selected;
                if chosen {
                    let band = Rect { x: area.x, width: area.width, ..line };
                    frame.buffer_mut().set_style(band, Style::new().bg(SELECTED));
                    frame.buffer_mut()[(area.x, line.y)].set_char('▸').set_fg(GREEN);
                }
                frame.render_widget(item(hit, *n, title, *columns), line);
            }
        }
    }
}

/// What an item's middle column says: a track's album, an album's
/// artists, a playlist's maker.
fn detail(hit: &Hit) -> String {
    if hit.target.starts_with("spotify:track:") {
        hit.album.clone().unwrap_or_default()
    } else if hit.target.contains(":folder:") {
        "Folder".into()
    } else {
        hit.artist.clone()
    }
}

/// An item's row: a track with its number, artists, album and length; an
/// album with its artists and year; a playlist with who made it.
fn item(hit: &Hit, n: usize, page_title: &str, columns: Columns) -> Table<'static> {
    let dim = Style::new().fg(SUBDUED);
    let track = hit.target.starts_with("spotify:track:");
    let mut title = vec![Span::styled(hit.title.clone(), Style::new().fg(TEXT))];
    // An artist's own songs need not say whose they are.
    if track && !hit.artist.is_empty() && hit.artist != page_title {
        title.push(Span::styled(format!(" · {}", hit.artist), dim));
    }
    let end = if track {
        hit.duration_ms.map(clock).unwrap_or_default()
    } else {
        hit.year.map(|y| y.to_string()).unwrap_or_default()
    };
    let number = if track { n.to_string() } else { String::new() };
    let mut cells = vec![Cell::from(Line::styled(number, dim).alignment(Alignment::Right)), Cell::from(Line::from(title))];
    let mut widths = vec![Constraint::Length(columns.number), Constraint::Fill(3)];
    if columns.wide {
        cells.push(Cell::from(Line::styled(detail(hit), dim)));
        widths.push(Constraint::Fill(2));
    }
    cells.push(Cell::from(Line::styled(end, dim).alignment(Alignment::Right)));
    widths.push(Constraint::Length(5));
    Table::new(vec![Row::new(cells)], widths).column_spacing(2)
}

/// The queue as Spotify draws an album page: a header tinted by the
/// playing cover that fades into the panel, then what plays now and what
/// plays next.
fn queue(frame: &mut Frame, area: Rect, view: &View) {
    frame.render_widget(Block::new().style(Style::new().bg(PANEL)), area);
    let current = view.status.track.as_ref();
    let tint = current.map_or([40, 40, 40], |track| cover_picture(view, track).tint());
    fade(frame.buffer_mut(), area, tint);
    let inner = area.inner(Margin::new(1, 0));
    let tracks: Vec<&Track> = current.into_iter().chain(&view.upcoming).collect();
    let mut rest = inner;
    let mut header = vec![Line::raw(""), Line::styled("Queue", Style::new().fg(TEXT).add_modifier(Modifier::BOLD))];
    if !tracks.is_empty() {
        let line = match view.queued {
            Some(queued) => totals(queued.tracks, queued.length_ms),
            None => summary(&tracks),
        };
        header.push(Line::styled(line, Style::new().fg(SUBDUED)));
    }
    frame.render_widget(Paragraph::new(header), take(&mut rest, 4));
    if tracks.is_empty() {
        let hint = Text::from(vec![
            Line::raw(""),
            Line::styled("Nothing queued", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Press ", Style::new().fg(SUBDUED)),
                Span::styled("/", Style::new().fg(GREEN).add_modifier(Modifier::BOLD)),
                Span::styled(" and give a link, a yt: search or a folder.", Style::new().fg(SUBDUED)),
            ]),
        ]);
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), rest);
        return;
    }

    // The album column only when some track has an album to show.
    let wide = inner.width >= 60 && tracks.iter().any(|t| !t.album.is_empty());
    let mut widths = vec![Constraint::Length(3), Constraint::Fill(3)];
    let mut labels = vec![Cell::from("#"), Cell::from("Title")];
    if wide {
        widths.push(Constraint::Fill(2));
        labels.push(Cell::from("Album"));
    }
    // As wide as the longest length, so an hour-long track keeps its hours.
    let time = tracks.iter().filter(|t| t.duration_ms > 0).map(|t| clock(t.duration_ms).len()).max().unwrap_or(0);
    widths.push(Constraint::Length(time.max(5) as u16));
    labels.push(Cell::from(Line::from("Time").alignment(Alignment::Right)));
    let table = |rows: Vec<Row<'static>>| Table::new(rows, widths.clone()).column_spacing(2);
    frame.render_widget(table(vec![Row::new(labels).style(Style::new().fg(SUBDUED))]), take(&mut rest, 1));
    let rule = Line::styled("─".repeat(usize::from(rest.width)), Style::new().fg(RULE));
    frame.render_widget(Paragraph::new(rule), take(&mut rest, 2));

    let heading = |text: &'static str| Paragraph::new(Line::styled(text, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
    if let Some(track) = current {
        frame.render_widget(heading("Now playing"), take(&mut rest, 1));
        let mark = equaliser(view.status.state, &view.spectrum);
        frame.render_widget(table(vec![row(&mark, track, true, wide)]), take(&mut rest, 3));
    }
    if !view.upcoming.is_empty() {
        frame.render_widget(heading("Next in queue"), take(&mut rest, 1));
        let rows = view.upcoming.iter().enumerate().map(|(i, track)| row(&(i + 1).to_string(), track, false, wide));
        frame.render_widget(table(rows.collect()), rest);
    }
}

/// Splits the top `height` rows off `rest`.
fn take(rest: &mut Rect, height: u16) -> Rect {
    let height = height.min(rest.height);
    let top = Rect { height, ..*rest };
    *rest = Rect { y: rest.y + height, height: rest.height - height, ..*rest };
    top
}

/// Tints the top rows of `area` from `tint` down to the panel's colour.
fn fade(buffer: &mut Buffer, area: Rect, tint: [u8; 3]) {
    let Color::Rgb(r, g, b) = PANEL else { return };
    let rows = area.height.min(FADE_ROWS);
    for i in 0..rows {
        let [r, g, b] = super::cover::mix(tint, [r, g, b], f32::from(i) / f32::from(rows));
        buffer.set_style(Rect { y: area.y + i, height: 1, ..area }, Style::new().bg(Color::Rgb(r, g, b)));
    }
}

/// "5 tracks · 21 min", the time only when every length is known.
pub fn summary(tracks: &[&Track]) -> String {
    let lengths = tracks.iter().map(|t| (t.duration_ms > 0).then_some(u64::from(t.duration_ms)));
    totals(tracks.len(), lengths.sum())
}

/// "5 tracks · 21 min" for `count` tracks lasting `length_ms`.
pub fn totals(count: usize, length_ms: Option<u64>) -> String {
    let count = if count == 1 { "1 track".to_string() } else { format!("{count} tracks") };
    let Some(length_ms) = length_ms else { return count };
    let minutes = (length_ms + 30_000) / 60_000;
    match minutes / 60 {
        0 => format!("{count} · {minutes} min"),
        hours => format!("{count} · {hours} hr {} min", minutes % 60),
    }
}

/// The playing row's mark: three bars, the loudest of each third of the
/// spectrum, which rest when the song pauses.
pub fn equaliser(state: State, spectrum: &Spectrum) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if state != State::Playing {
        return "▂▂▂".into();
    }
    let levels = &spectrum.levels;
    (0..3)
        .map(|third| {
            let part = &levels[levels.len() * third / 3..levels.len() * (third + 1) / 3];
            let loudest = part.iter().cloned().fold(0.0, f32::max).clamp(0.0, 1.0);
            LEVELS[(loudest * 7.0).round() as usize]
        })
        .collect()
}

fn row(index: &str, track: &Track, current: bool, wide: bool) -> Row<'static> {
    let title_style = if current { Style::new().fg(GREEN) } else { Style::new().fg(TEXT) };
    let index_style = if current { Style::new().fg(GREEN) } else { Style::new().fg(SUBDUED) };
    let length = if track.duration_ms > 0 { clock(track.duration_ms) } else { String::new() };
    let mut cells = vec![
        Cell::from(Line::styled(index.to_string(), index_style).alignment(Alignment::Right)),
        Cell::from(Text::from(vec![Line::styled(track.title.clone(), title_style), byline(track)])),
    ];
    if wide {
        cells.push(Cell::from(Line::styled(track.album.clone(), Style::new().fg(SUBDUED))));
    }
    cells.push(Cell::from(Line::styled(length, Style::new().fg(SUBDUED)).alignment(Alignment::Right)));
    Row::new(cells).height(2).bottom_margin(1)
}

fn now_playing(frame: &mut Frame, area: Rect, view: &View) {
    let block = panel("Now playing");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(track) = view.status.track.as_ref() else {
        frame.render_widget(Paragraph::new(Line::styled("Nothing playing", Style::new().fg(FAINT))), inner);
        return;
    };
    let (art, details) = now_playing_layout(inner);
    cover(view, track, art, frame.buffer_mut());

    let mut lines = vec![
        Line::styled(track.title.clone(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        byline(track),
    ];
    if !track.album.is_empty() {
        lines.push(Line::styled(track.album.clone(), Style::new().fg(SUBDUED)));
    }
    if let Some(link) = &track.link {
        lines.push(Line::raw(""));
        lines.push(Line::styled(link.clone(), Style::new().fg(FAINT)));
    }
    let spectrum = spectrum_area(frame.area(), view);
    // The details stop a row above the spectrum.
    let text = spectrum.map_or(details, |rect| Rect { height: rect.y.saturating_sub(details.y + 1), ..details });
    frame.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }), text);
    if let Some(rect) = spectrum {
        bars(rect, frame.buffer_mut(), &view.spectrum, &view.tuning);
    }
}

/// Bars in eighth blocks coloured by height, green to yellow to red as
/// Winamp's were, with a cap where each bar last peaked.
fn bars(area: Rect, buffer: &mut Buffer, spectrum: &Spectrum, tuning: &Tuning) {
    const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let rows = f32::from(area.height);
    for (i, (&level, &peak)) in spectrum.levels.iter().zip(&spectrum.peaks).enumerate() {
        let x = area.x + i as u16 * (tuning.width + tuning.gap);
        if x + tuning.width > area.right() {
            break;
        }
        let height = level * rows;
        for row in 0..area.height {
            let fill = (height - f32::from(row)).clamp(0.0, 1.0);
            let symbol = EIGHTHS[(fill * 8.0).round() as usize];
            let (y, color) = (area.bottom() - 1 - row, heat((f32::from(row) + 0.5) / rows));
            for dx in 0..tuning.width {
                buffer[(x + dx, y)].set_char(symbol).set_fg(color);
            }
        }
        let cap = (peak * rows).floor() as u16;
        if peak > level + 0.5 / rows && cap < area.height {
            for dx in 0..tuning.width {
                buffer[(x + dx, area.bottom() - 1 - cap)].set_char('▔').set_fg(CAP);
            }
        }
    }
}

/// Green at the floor, yellow, then red at the top.
fn heat(t: f32) -> Color {
    let mix = |a: [u8; 3], b: [u8; 3], t: f32| {
        let [r, g, b] = super::cover::mix(a, b, t);
        Color::Rgb(r, g, b)
    };
    if t < 0.6 {
        mix([20, 120, 50], [30, 215, 96], t / 0.6)
    } else if t < 0.85 {
        mix([30, 215, 96], [240, 220, 60], (t - 0.6) / 0.25)
    } else {
        mix([240, 220, 60], [240, 70, 50], (t - 0.85) / 0.15)
    }
}

/// The cover and the details below it. Cells are about twice as tall as
/// wide, so a square is twice as many columns as rows, and each row holds
/// two pixels.
fn now_playing_layout(inner: Rect) -> (Rect, Rect) {
    let rows = (inner.width / 2).min(inner.height.saturating_sub(6));
    let [art, details] = Layout::vertical([Constraint::Length(rows), Constraint::Min(0)]).spacing(1).areas(inner);
    (Rect { width: rows * 2, ..art }, details)
}

/// The bar's thirds, the left one split into its cover and its text.
/// Three rows of half blocks are six pixels: square at 6 columns.
fn player_bar_layout(area: Rect) -> [(Rect, Rect); 3] {
    let inner = area.inner(ratatui::layout::Margin::new(1, 0));
    let [left, middle, right] =
        Layout::horizontal([Constraint::Fill(3), Constraint::Fill(4), Constraint::Fill(3)]).spacing(2).areas(inner);
    let [art, text] = Layout::horizontal([Constraint::Length(6), Constraint::Min(0)]).spacing(2).areas(left);
    [(art, text), (middle, middle), (right, right)]
}

/// Spotify's player bar: what plays on the left with its cover, the
/// transport and progress in the middle, the volume on the right.
fn player_bar(frame: &mut Frame, area: Rect, view: &View) {
    frame.render_widget(Block::new().style(Style::new().bg(BG)), area);
    let [(art, text), (middle, _), (right, _)] = player_bar_layout(area);
    let left = art.union(text);
    let status = &view.status;
    let icons = view.icons;

    match &status.track {
        Some(track) => {
            cover(view, track, art, frame.buffer_mut());
            let lines = vec![
                Line::styled(track.title.clone(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                Line::styled(track.artist.clone(), Style::new().fg(SUBDUED)),
                source_line(track.source),
            ];
            frame.render_widget(Paragraph::new(lines), text);
        }
        None => frame.render_widget(Paragraph::new(Line::styled("Not playing", Style::new().fg(SUBDUED))), left),
    }

    let idle = status.track.is_none();
    let side = Style::new().fg(if idle { FAINT } else { SUBDUED });
    let toggle = if status.state == State::Playing { icons.pause } else { icons.play };
    let button = Style::new().fg(BG).bg(TEXT).add_modifier(Modifier::BOLD);
    let controls = Line::from(vec![
        Span::styled(icons.previous, side),
        Span::raw("     "),
        Span::styled(icons.cap_left, Style::new().fg(TEXT)),
        Span::styled(format!(" {toggle} "), button),
        Span::styled(icons.cap_right, Style::new().fg(TEXT)),
        Span::raw("     "),
        Span::styled(icons.next, side),
    ])
    .centered();
    let length = status.track.as_ref().map_or(0, |t| t.duration_ms);
    let progress = bar(middle.width.min(MAX_PROGRESS), view.position_ms(), length);
    frame.render_widget(Paragraph::new(vec![controls, progress]), middle);

    let icon = match status.volume {
        0 => icons.volume_off,
        1..=50 => icons.volume_low,
        _ => icons.volume_high,
    };
    let [level, rest] = meter(right.width.saturating_sub(4).min(VOLUME_WIDTH), u32::from(status.volume), 100);
    let volume = Line::from(vec![Span::styled(format!("{icon}  "), Style::new().fg(SUBDUED)), level, rest]).right_aligned();
    frame.render_widget(Paragraph::new(vec![Line::raw(""), volume]), right);
}

/// "● Spotify", in the source's colour.
fn source_line(source: Source) -> Line<'static> {
    let (name, color) = source_name(source);
    Line::from(vec![Span::styled("● ", Style::new().fg(color)), Span::styled(name, Style::new().fg(FAINT))])
}

/// The track's cover once it has loaded, and a stand-in until then.
fn cover(view: &View, track: &Track, area: Rect, buffer: &mut Buffer) {
    match track.art.as_ref().and_then(|art| view.covers.get(art)) {
        Some(Cover::Ready(picture)) => picture.draw(area, buffer),
        _ => Picture::placeholder(cover_seed(track), area.width, area.height * 2).draw(area, buffer),
    }
}

/// The picture the cover shows, small: the loaded one or the stand-in.
fn cover_picture(view: &View, track: &Track) -> Picture {
    match track.art.as_ref().and_then(|art| view.covers.get(art)) {
        Some(Cover::Ready(picture)) => picture.clone(),
        _ => Picture::placeholder(cover_seed(track), 8, 8),
    }
}

/// One album shares one stand-in cover.
fn cover_seed(track: &Track) -> &str {
    if track.album.is_empty() { &track.uri } else { &track.album }
}

/// "1:23 ━━━━━━━━━──────── 4:29", filling `width` cells.
fn bar(width: u16, position_ms: u32, length_ms: u32) -> Line<'static> {
    let left = clock(position_ms);
    let right = if length_ms > 0 { clock(length_ms) } else { "-:--".into() };
    let room = width.saturating_sub((left.len() + right.len() + 2) as u16);
    let [done, rest] = meter(room, position_ms, length_ms);
    Line::from(vec![
        Span::styled(format!("{left} "), Style::new().fg(SUBDUED)),
        done,
        rest,
        Span::styled(format!(" {right}"), Style::new().fg(SUBDUED)),
    ])
    .centered()
}

/// A filled run in white on a grey track, `width` cells long.
fn meter(width: u16, value: u32, full: u32) -> [Span<'static>; 2] {
    let filled = if full == 0 { 0 } else { (u64::from(width) * u64::from(value.min(full)) / u64::from(full)) as usize };
    let empty = usize::from(width) - filled;
    [
        Span::styled("━".repeat(filled), Style::new().fg(TEXT)),
        Span::styled("━".repeat(empty), Style::new().fg(TRACK)),
    ]
}

fn footer_line(frame: &mut Frame, area: Rect, view: &View) {
    let key = Style::new().fg(TEXT).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(SUBDUED);
    let line = if let Some(prompt) = &view.prompt {
        let label = match prompt.kind {
            PromptKind::Play => "Play",
            PromptKind::Add => "Add",
        };
        Line::from(vec![
            Span::styled(format!(" {label} ▸ "), Style::new().fg(GREEN).add_modifier(Modifier::BOLD)),
            Span::styled(prompt.text.clone(), Style::new().fg(TEXT)),
            Span::styled("█", Style::new().fg(GREEN)),
            Span::styled("   enter go · esc cancel", dim),
        ])
    } else if view.busy {
        Line::styled(" Finding it…", dim)
    } else if let Some(message) = &view.message {
        match message {
            Ok(text) => Line::styled(format!(" {text}"), dim),
            Err(text) => Line::styled(format!(" {text}"), Style::new().fg(RED)),
        }
    } else if let Some(error) = &view.status.error {
        Line::styled(format!(" {error}"), Style::new().fg(RED))
    } else {
        let library = &view.library;
        let tab = match library.next_focus(true) {
            Focus::Player => "player",
            Focus::Shelves => "library",
            Focus::Page => "page",
        };
        let player = [("space", "play/pause"), ("n", "next"), ("b", "back"), ("s", "stop"), ("←→", "seek"), ("+-", "volume")];
        let hints: Vec<(&str, &str)> = match library.focus {
            Focus::Player => player.into_iter().chain([("/", "play"), ("a", "add"), ("tab", tab), ("q", "quit")]).collect(),
            Focus::Shelves => [("↑↓", "select"), ("enter", "open"), ("esc", "back"), ("tab", tab), ("space", "play/pause"), ("q", "quit")].into(),
            Focus::Page => {
                let add = library.pages.last().and_then(Opened::hit).is_some_and(|h| library::plays(&h.target));
                let mut keys = vec![("↑↓", "select"), ("enter", "open/play")];
                if add {
                    keys.push(("a", "add"));
                }
                keys.extend([("esc", "back"), ("tab", tab), ("space", "play/pause"), ("q", "quit")]);
                keys
            }
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, what) in hints {
            spans.push(Span::styled(k, key));
            spans.push(Span::styled(format!(" {what}   "), dim));
        }
        Line::from(spans)
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(line).style(Style::new().bg(BG)), area);
}
