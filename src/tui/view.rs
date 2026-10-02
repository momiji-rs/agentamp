//! What the terminal window shows, drawn from one snapshot of the player.
//! Drawing is pure: the same view and size always give the same cells.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table};

use crate::model::{Source, State, Status, Track, clock};

const BG: Color = Color::Rgb(0, 0, 0);
const PANEL: Color = Color::Rgb(18, 18, 18);
const RAISED: Color = Color::Rgb(40, 40, 40);
const BORDER: Color = Color::Rgb(40, 40, 40);
const TEXT: Color = Color::Rgb(255, 255, 255);
const SUBDUED: Color = Color::Rgb(167, 167, 167);
const FAINT: Color = Color::Rgb(100, 100, 100);
const GREEN: Color = Color::Rgb(30, 215, 96);
const RED: Color = Color::Rgb(241, 94, 108);
const YOUTUBE: Color = Color::Rgb(255, 51, 51);
const FILES: Color = Color::Rgb(80, 155, 245);

/// Below these widths the side panels give their room to the queue.
const WITH_BOTH_PANELS: u16 = 110;
const WITH_LIBRARY: u16 = 80;
/// Two bars from half blocks: drawn by every terminal font, unlike U+23F8.
const PAUSE: &str = "▐▌";

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

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub status: Status,
    pub upcoming: Vec<Track>,
    /// Played tracks, oldest first.
    pub history: Vec<Track>,
    pub prompt: Option<Prompt>,
    /// The answer to the last request: a confirmation, or an error.
    pub message: Option<Result<String, String>>,
    /// A play or add is still being resolved.
    pub busy: bool,
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
            history: Vec::new(),
            prompt: None,
            message: None,
            busy: false,
        }
    }
}

pub fn draw(frame: &mut Frame, view: &View) {
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::new().bg(BG)), area);
    let [main, player, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(4), Constraint::Length(1)]).areas(area);

    let panels: Vec<Constraint> = if area.width >= WITH_BOTH_PANELS {
        vec![Constraint::Length(28), Constraint::Min(40), Constraint::Length(34)]
    } else if area.width >= WITH_LIBRARY {
        vec![Constraint::Length(26), Constraint::Min(40)]
    } else {
        vec![Constraint::Min(0)]
    };
    let columns = Layout::horizontal(panels).spacing(1).split(main);
    match columns.len() {
        3 => {
            library(frame, columns[0], view);
            queue(frame, columns[1], view);
            now_playing(frame, columns[2], view);
        }
        2 => {
            library(frame, columns[0], view);
            queue(frame, columns[1], view);
        }
        _ => queue(frame, columns[0], view),
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

fn library(frame: &mut Frame, area: Rect, view: &View) {
    let dim = Style::new().fg(SUBDUED);
    let mut lines = vec![
        Line::styled("Sources", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        Line::from(vec![Span::styled("● ", Style::new().fg(GREEN)), Span::raw("Spotify")]),
        Line::styled("  links, spotify: URIs", dim),
        Line::from(vec![Span::styled("● ", Style::new().fg(YOUTUBE)), Span::raw("YouTube")]),
        Line::styled("  links, yt: searches", dim),
        Line::from(vec![Span::styled("● ", Style::new().fg(FILES)), Span::raw("Files")]),
        Line::styled("  files and folders", dim),
        Line::raw(""),
        Line::styled("Recently played", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
    ];
    if view.history.is_empty() {
        lines.push(Line::styled("Nothing yet", Style::new().fg(FAINT)));
    }
    for track in view.history.iter().rev() {
        lines.push(Line::raw(track.title.clone()));
        lines.push(byline(track));
    }
    let block = panel("Your Library");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines).style(Style::new().fg(TEXT)), inner);
}

fn queue(frame: &mut Frame, area: Rect, view: &View) {
    let block = panel("Queue");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let current = view.status.track.as_ref();
    if current.is_none() && view.upcoming.is_empty() {
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
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), inner);
        return;
    }

    // The album column only when some track has an album to show.
    let wide = inner.width >= 60 && current.into_iter().chain(&view.upcoming).any(|t| !t.album.is_empty());
    let mut rows = Vec::new();
    if let Some(track) = current {
        let mark = match view.status.state {
            State::Paused => "▌▐",
            _ => "▶",
        };
        rows.push(row(mark, track, true, wide));
    }
    for (i, track) in view.upcoming.iter().enumerate() {
        rows.push(row(&(i + 1).to_string(), track, false, wide));
    }
    let mut widths = vec![Constraint::Length(3), Constraint::Fill(3)];
    let mut header = vec![Cell::from("#"), Cell::from("Title")];
    if wide {
        widths.push(Constraint::Fill(2));
        header.push(Cell::from("Album"));
    }
    widths.push(Constraint::Length(5));
    header.push(Cell::from(Line::from("Time").alignment(Alignment::Right)));
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(Style::new().fg(SUBDUED)).bottom_margin(1))
        .column_spacing(2);
    frame.render_widget(table, inner);
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
    // Cells are about twice as tall as wide, so half the width is square.
    let side = (inner.width / 2).min(inner.height.saturating_sub(6));
    let [art, details] = Layout::vertical([Constraint::Length(side), Constraint::Min(0)]).spacing(1).areas(inner);
    let (_, color) = source_name(track.source);
    // Cover art comes later; until then a tile in the source's colour.
    frame.render_widget(Block::new().style(Style::new().bg(RAISED)), art);
    if side > 0 {
        let middle = Rect { y: art.y + side / 2, height: 1, ..art };
        let (name, _) = source_name(track.source);
        frame.render_widget(
            Paragraph::new(Line::styled(name, Style::new().fg(color).bg(RAISED)).centered()),
            middle,
        );
    }

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
    frame.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }), details);
}

fn player_bar(frame: &mut Frame, area: Rect, view: &View) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(BORDER))
        .style(Style::new().bg(BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [left, middle, right] =
        Layout::horizontal([Constraint::Percentage(28), Constraint::Percentage(48), Constraint::Percentage(24)])
            .spacing(2)
            .areas(inner);

    let status = &view.status;
    let now = match &status.track {
        Some(track) => vec![
            Line::styled(track.title.clone(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            byline(track),
        ],
        None => vec![Line::styled("Not playing", Style::new().fg(SUBDUED))],
    };
    frame.render_widget(Paragraph::new(now), left);

    let toggle = if status.state == State::Playing { PAUSE } else { "▶ " };
    let controls = Line::from(vec![
        Span::styled("■", Style::new().fg(SUBDUED)),
        Span::raw("    "),
        Span::styled(format!(" {toggle} "), Style::new().fg(BG).bg(TEXT).add_modifier(Modifier::BOLD)),
        Span::raw("    "),
        Span::styled("▶▶", Style::new().fg(SUBDUED)),
    ])
    .centered();
    let length = status.track.as_ref().map_or(0, |t| t.duration_ms);
    let progress = bar(middle.width, status.position_ms, length);
    frame.render_widget(Paragraph::new(vec![controls, progress]), middle);

    let [level, rest] = meter(right.width.saturating_sub(9).min(16), u32::from(status.volume), 100);
    let volume = Line::from(vec![
        Span::styled("vol ", Style::new().fg(SUBDUED)),
        level,
        rest,
        Span::styled(format!(" {:>3}%", status.volume), Style::new().fg(SUBDUED)),
    ])
    .right_aligned();
    frame.render_widget(Paragraph::new(vec![Line::raw(""), volume]), right);
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
        Span::styled("━".repeat(empty), Style::new().fg(RAISED)),
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
        let mut spans = vec![Span::raw(" ")];
        for (k, what) in [
            ("space", "play/pause"),
            ("n", "next"),
            ("s", "stop"),
            ("←→", "seek"),
            ("+-", "volume"),
            ("/", "play"),
            ("a", "add"),
            ("q", "quit"),
        ] {
            spans.push(Span::styled(k, key));
            spans.push(Span::styled(format!(" {what}   "), dim));
        }
        Line::from(spans)
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(line).style(Style::new().bg(BG)), area);
}
