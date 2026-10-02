//! Real covers for terminals that draw images: kitty's graphics protocol
//! (kitty, Ghostty), Sixel (foot) or iTerm2's (WezTerm, iTerm2). Elsewhere,
//! and in tests, the view's half blocks are the cover.

use std::collections::HashMap;

use image::{DynamicImage, RgbImage};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Clear};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, FontSize, Image, Resize};

use super::view::{self, View};

/// Covers kept at full size: the playing track's and a few before it.
const KEEP: usize = 8;

pub struct Graphics {
    picker: Picker,
    images: HashMap<String, RgbImage>,
    /// Encoded covers by place, so a redraw sends nothing new.
    shown: Vec<(String, Rect, Protocol)>,
}

impl Graphics {
    /// Asks the terminal what it can show. None when it draws no images or
    /// `AGENTAMP_COVERS=blocks` asks for half blocks. Call after entering
    /// the alternate screen and before reading events.
    pub fn detect() -> Option<Self> {
        if std::env::var("AGENTAMP_COVERS").as_deref() == Ok("blocks") {
            return None;
        }
        Self::with(Picker::from_query_stdio().ok()?)
    }

    pub fn with(picker: Picker) -> Option<Self> {
        (picker.protocol_type() != ProtocolType::Halfblocks).then(|| Self {
            picker,
            images: HashMap::new(),
            shown: Vec::new(),
        })
    }

    pub fn offer(&mut self, art: String, image: RgbImage) {
        if self.images.len() >= KEEP {
            self.images.clear();
        }
        self.images.insert(art, image);
    }

    /// Draws the playing track's cover over the half blocks the view drew.
    pub fn draw(&mut self, frame: &mut Frame, view: &View) {
        let art = view.status.track.as_ref().and_then(|t| t.art.as_ref());
        let areas = view::cover_areas(frame.area(), view);
        self.shown.retain(|(shown, area, _)| Some(shown) == art && areas.iter().any(|(a, _)| a == area));
        let Some((art, image)) = art.and_then(|art| Some((art, self.images.get(art)?))) else { return };
        for (area, behind) in areas {
            // Scaled to fit, the picture leaves part of the area when cells
            // are not exactly twice as tall as wide; that part shows the
            // background, not half blocks.
            frame.render_widget(Clear, area);
            frame.render_widget(Block::new().style(Style::new().bg(behind)), area);
            let area = footprint(area, self.picker.font_size());
            if !self.shown.iter().any(|(_, shown, _)| *shown == area) {
                let fitted = self.picker.new_protocol(DynamicImage::ImageRgb8(image.clone()), area.into(), Resize::Scale(Some(FilterType::Triangle)));
                let Ok(protocol) = fitted else { continue };
                self.shown.push((art.clone(), area, protocol));
            }
            let Some((_, _, protocol)) = self.shown.iter().find(|(_, shown, _)| *shown == area) else { continue };
            frame.render_widget(Image::new(protocol), area);
        }
    }
}

/// The cells a square picture covers at its largest in `area`, so the
/// terminal pads none of them.
fn footprint(area: Rect, font: FontSize) -> Rect {
    let (width, height) = (u32::from(font.width.max(1)), u32::from(font.height.max(1)));
    let side = (u32::from(area.width) * width).min(u32::from(area.height) * height);
    Rect { width: (side / width).max(1) as u16, height: (side / height).max(1) as u16, ..area }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::{Source, State, Track};

    fn kitty() -> Graphics {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        Graphics::with(picker).unwrap()
    }

    fn playing(art: &str) -> View {
        let mut view = View::default();
        let mut track = Track::placeholder(Source::Spotify, "spotify:track:x");
        track.art = Some(art.into());
        view.status.track = Some(track);
        view.status.state = State::Playing;
        view
    }

    fn symbols(graphics: &mut Graphics, view: &View) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| {
                view::draw(frame, view);
                graphics.draw(frame, view);
            })
            .unwrap();
        terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn terminals_without_images_keep_half_blocks() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Halfblocks);
        assert!(Graphics::with(picker).is_none());
    }

    #[test]
    fn kitty_terminals_get_the_cover_as_an_image() {
        let mut graphics = kitty();
        let view = playing("https://i.scdn.co/image/a");
        // Nothing to show until the cover arrives.
        assert!(!symbols(&mut graphics, &view).contains("\x1b_G"));

        graphics.offer("https://i.scdn.co/image/a".into(), RgbImage::from_pixel(64, 64, image::Rgb([200, 40, 40])));
        let screen = symbols(&mut graphics, &view);
        // Sent with kitty's graphics protocol, then placed with its
        // placeholder character, in the panel and the player bar.
        assert!(screen.contains("\x1b_G"), "no kitty image");
        assert!(screen.contains('\u{10EEEE}'));
        assert_eq!(graphics.shown.len(), 2);

        // In cells three times as tall as wide, a square is 3 columns per row.
        assert_eq!(footprint(Rect::new(0, 0, 32, 16), FontSize::new(10, 30)), Rect::new(0, 0, 32, 10));
        assert_eq!(footprint(Rect::new(0, 0, 32, 16), FontSize::new(10, 20)), Rect::new(0, 0, 32, 16));

        // Another track's cover replaces it.
        let other = playing("https://i.scdn.co/image/b");
        symbols(&mut graphics, &other);
        assert!(graphics.shown.is_empty());
    }
}
