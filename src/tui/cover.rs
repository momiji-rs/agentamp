//! Cover art in half blocks: each cell shows two pixels, the upper one as
//! the glyph `▀` and the lower one as the background, so a picture shows in
//! any terminal with 24-bit colour.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// A small RGB picture, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<[u8; 3]>,
}

impl Picture {
    /// A stand-in until real art loads: a diagonal blend of two colours
    /// picked from `seed`, so one album always gets the same tile.
    pub fn placeholder(seed: &str, width: u16, height: u16) -> Self {
        let hash = hash(seed);
        let hue = (hash % 360) as f32;
        let from = hsl(hue, 0.55, 0.52);
        let to = hsl((hue + 40.0 + (hash >> 16) as f32 % 80.0) % 360.0, 0.6, 0.22);
        let span = f32::from(width + height).max(1.0);
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| mix(from, to, f32::from(x + y) / span))
            .collect();
        Self { width, height, pixels }
    }

    /// The mean colour of the source pixels under output pixel (`x`, `y`)
    /// of a `width`×`height` picture, so shrinking a cover keeps its detail
    /// smooth instead of dropping pixels.
    fn sample(&self, x: u32, y: u32, width: u32, height: u32) -> [u8; 3] {
        let span = |i: u32, out: u32, size: u16| {
            let size = u32::from(size);
            let start = i * size / out;
            start..((i + 1) * size / out).max(start + 1).min(size)
        };
        let (xs, ys) = (span(x, width, self.width), span(y, height, self.height));
        let mut sum = [0u32; 3];
        let mut count = 0;
        for sy in ys {
            for sx in xs.clone() {
                let pixel = self.pixels[sy as usize * usize::from(self.width) + sx as usize];
                for (total, channel) in sum.iter_mut().zip(pixel) {
                    *total += u32::from(channel);
                }
                count += 1;
            }
        }
        sum.map(|total| (total / count.max(1)) as u8)
    }

    /// The colour the picture is mostly, dark enough for white text: the mean
    /// weighted to its vivid pixels, so a grey frame around a red face
    /// gives red, as Spotify tints a page by its cover.
    pub fn tint(&self) -> [u8; 3] {
        let mut sum = [0u64; 3];
        let mut weight = 0u64;
        for pixel in &self.pixels {
            let (high, low) = (pixel.iter().max().unwrap_or(&0), pixel.iter().min().unwrap_or(&0));
            let w = u64::from(high - low) + 1;
            for (total, channel) in sum.iter_mut().zip(pixel) {
                *total += u64::from(*channel) * w;
            }
            weight += w;
        }
        let mean = sum.map(|total| total / weight.max(1));
        let high = mean.iter().copied().max().unwrap_or(0).max(1);
        mean.map(|channel| (channel * 100 / high) as u8)
    }

    /// Draws the picture scaled to fill `area`, two pixels per cell.
    pub fn draw(&self, area: Rect, buffer: &mut Buffer) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let (width, height) = (u32::from(area.width), u32::from(area.height) * 2);
        for cy in 0..area.height {
            for cx in 0..area.width {
                let [r, g, b] = self.sample(cx.into(), u32::from(cy) * 2, width, height);
                let [r2, g2, b2] = self.sample(cx.into(), u32::from(cy) * 2 + 1, width, height);
                if let Some(cell) = buffer.cell_mut((area.x + cx, area.y + cy)) {
                    cell.set_symbol("▀").set_fg(Color::Rgb(r, g, b)).set_bg(Color::Rgb(r2, g2, b2));
                }
            }
        }
    }
}

/// FNV-1a: a stable name for a seed or URL.
pub fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8)
}

fn hsl(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_stable_per_seed() {
        let a = Picture::placeholder("Pocket Park", 8, 8);
        assert_eq!(a, Picture::placeholder("Pocket Park", 8, 8));
        assert_ne!(a.pixels, Picture::placeholder("Ride on Time", 8, 8).pixels);
        assert_eq!(a.pixels.len(), 64);
    }

    #[test]
    fn each_cell_holds_two_pixels() {
        let picture = Picture { width: 1, height: 2, pixels: vec![[255, 0, 0], [0, 0, 255]] };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        picture.draw(Rect::new(1, 0, 1, 1), &mut buffer);
        let cell = &buffer[(1, 0)];
        assert_eq!((cell.symbol(), cell.fg, cell.bg), ("▀", Color::Rgb(255, 0, 0), Color::Rgb(0, 0, 255)));
        assert_eq!(buffer[(0, 0)].symbol(), " ");
    }

    #[test]
    fn shrinking_averages_the_pixels_it_covers() {
        let picture = Picture { width: 2, height: 2, pixels: vec![[200, 0, 0], [0, 0, 200], [200, 0, 0], [0, 0, 200]] };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        picture.draw(Rect::new(0, 0, 1, 1), &mut buffer);
        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(100, 0, 100));
    }

    #[test]
    fn the_tint_follows_the_vivid_pixels() {
        let grey = [90, 90, 90];
        let mut pixels = vec![grey; 12];
        pixels.extend([[200, 30, 30]; 4]);
        let tint = Picture { width: 4, height: 4, pixels }.tint();
        assert_eq!(tint[0], 100);
        assert!(tint[1] < 30 && tint[2] < 30, "{tint:?}");
        assert_eq!(Picture { width: 1, height: 1, pixels: vec![[0, 0, 0]] }.tint(), [0, 0, 0]);
    }

    #[test]
    fn colours_convert() {
        assert_eq!(hsl(0.0, 1.0, 0.5), [255, 0, 0]);
        assert_eq!(hsl(120.0, 1.0, 0.5), [0, 255, 0]);
        assert_eq!(mix([0, 0, 0], [200, 100, 50], 0.5), [100, 50, 25]);
    }
}
