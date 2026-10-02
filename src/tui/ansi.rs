//! A drawn frame as the bytes a terminal would receive: for `--frame`,
//! for termshot screenshots, and for agents that want to see the screen.

use std::fmt::Write;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// Clears the screen and writes every cell, one row per line.
pub fn encode(buffer: &Buffer) -> String {
    let mut out = String::from("\x1b[0m\x1b[2J\x1b[H");
    let blank = Buffer::empty(buffer.area);
    let mut last: Option<(u16, u16)> = None;
    let mut style = None;
    // diff() against an empty buffer yields every drawn cell and skips the
    // ones hidden behind a wide character.
    for (x, y, cell) in blank.diff(buffer) {
        if last != Some((x, y)) {
            let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
        }
        let now = (cell.fg, cell.bg, cell.modifier);
        if style != Some(now) {
            out.push_str(&sgr(cell.fg, cell.bg, cell.modifier));
            style = Some(now);
        }
        out.push_str(cell.symbol());
        let width = UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
        last = Some((x + width, y));
    }
    let _ = write!(out, "\x1b[0m\x1b[{};1H", buffer.area.height);
    out
}

fn sgr(fg: Color, bg: Color, modifier: Modifier) -> String {
    let mut codes = vec!["0".to_string()];
    for (flag, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if modifier.contains(flag) {
            codes.push(code.into());
        }
    }
    if let Some(c) = color(fg, 38) {
        codes.push(c);
    }
    if let Some(c) = color(bg, 48) {
        codes.push(c);
    }
    format!("\x1b[{}m", codes.join(";"))
}

fn color(color: Color, base: u8) -> Option<String> {
    let basic = |n: u8| Some(format!("{}", base - 8 + n));
    let bright = |n: u8| Some(format!("{}", base + 52 + n));
    match color {
        Color::Reset => None,
        Color::Black => basic(0),
        Color::Red => basic(1),
        Color::Green => basic(2),
        Color::Yellow => basic(3),
        Color::Blue => basic(4),
        Color::Magenta => basic(5),
        Color::Cyan => basic(6),
        Color::Gray => basic(7),
        Color::DarkGray => bright(0),
        Color::LightRed => bright(1),
        Color::LightGreen => bright(2),
        Color::LightYellow => bright(3),
        Color::LightBlue => bright(4),
        Color::LightMagenta => bright(5),
        Color::LightCyan => bright(6),
        Color::White => bright(7),
        Color::Indexed(i) => Some(format!("{base};5;{i}")),
        Color::Rgb(r, g, b) => Some(format!("{base};2;{r};{g};{b}")),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::*;

    #[test]
    fn cells_become_positioned_coloured_text() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        buffer.set_string(0, 0, "ab", Style::new().fg(Color::Rgb(1, 2, 3)));
        buffer.set_string(2, 1, "c", Style::new().bg(Color::Red).bold());
        let text = encode(&buffer);
        assert!(text.contains("\x1b[1;1H\x1b[0;38;2;1;2;3mab"), "{text:?}");
        assert!(text.contains("\x1b[2;3H\x1b[0;1;41mc"), "{text:?}");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer.set_string(0, 0, "日本x", Style::new().fg(Color::White));
        let text = encode(&buffer);
        // The x follows the two wide characters without a jump.
        assert!(text.contains("日本x"), "{text:?}");
        assert!(!text.contains("\x1b[1;5H"), "{text:?}");
    }

    #[test]
    fn bright_and_indexed_colours() {
        assert_eq!(color(Color::White, 38).unwrap(), "97");
        assert_eq!(color(Color::DarkGray, 48).unwrap(), "100");
        assert_eq!(color(Color::Red, 38).unwrap(), "31");
        assert_eq!(color(Color::Indexed(200), 48).unwrap(), "48;5;200");
    }
}
