//! Covers, fetched, kept and decoded off the drawing thread.

use std::path::Path;

use anyhow::{Context, Result};
use bytes::Bytes;
use image::imageops::FilterType;
use librespot_core::http_client::HttpClient;

use super::cover::{Picture, hash};

/// Covers decode to this many pixels a side; the largest place one shows
/// is the Now playing panel, 32 columns of half blocks.
pub const SIZE: u32 = 64;
/// Terminals that show real images get the cover at most this large.
const LARGEST: u32 = 640;

/// A loaded cover: small for half blocks, and square at full size for
/// terminals that draw images.
pub struct Art {
    pub picture: Picture,
    pub image: image::RgbImage,
}

/// Loads the cover `art` names: an https URL, fetched once and then read
/// from `dir`, or an audio file whose tags hold the picture.
pub fn load(art: &str, dir: &Path, client: &HttpClient, runtime: &tokio::runtime::Runtime) -> Result<Art> {
    let bytes = if art.starts_with("https://") {
        let file = dir.join(format!("{:016x}", hash(art)));
        match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(_) => {
                let request = http::Request::get(art).body(Bytes::new())?;
                let bytes = runtime.block_on(client.request_body(request)).context("cannot fetch the cover")?;
                keep(&file, &bytes)?;
                bytes.to_vec()
            }
        }
    } else {
        embedded(Path::new(art))?
    };
    decode(&bytes)
}

/// Writes the whole file or nothing, so a cut-off download is never read.
fn keep(file: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(file.parent().context("no cover directory")?)?;
    let partial = file.with_extension("part");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file)?;
    Ok(())
}

/// The front cover in a file's tags, or else its first picture.
fn embedded(path: &Path) -> Result<Vec<u8>> {
    use lofty::picture::PictureType;
    use lofty::prelude::*;

    let file = lofty::read_from_path(path)?;
    let pictures = file.tags().iter().flat_map(|tag| tag.pictures()).collect::<Vec<_>>();
    let picture = pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or(pictures.first())
        .context("no picture in the file's tags")?;
    Ok(picture.data().to_vec())
}

/// A square picture from an image: black bars trimmed (YouTube's 4:3
/// thumbnails letterbox their videos), then the middle square.
pub fn decode(bytes: &[u8]) -> Result<Art> {
    let image = image::load_from_memory(bytes).context("cannot read the cover")?.into_rgb8();
    let (width, height) = image.dimensions();
    let dark = |y: u32| (0..width).all(|x| image.get_pixel(x, y).0.iter().all(|&c| c < 16));
    let top = (0..height).find(|&y| !dark(y)).unwrap_or(0);
    let bottom = (top..height).rev().find(|&y| !dark(y)).map_or(height, |y| y + 1);
    let rows = bottom - top;
    let side = width.min(rows);
    let square =
        image::imageops::crop_imm(&image, (width - side) / 2, top + (rows - side) / 2, side, side).to_image();
    let small = image::imageops::resize(&square, SIZE, SIZE, FilterType::Triangle);
    let picture = Picture { width: SIZE as u16, height: SIZE as u16, pixels: small.pixels().map(|p| p.0).collect() };
    let image = if side > LARGEST { image::imageops::resize(&square, LARGEST, LARGEST, FilterType::Triangle) } else { square };
    Ok(Art { picture, image })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    pub fn png(image: &RgbImage) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A 4:3 image letterboxing a 2:1 picture, red on the left and blue on
    /// the right.
    fn letterboxed() -> RgbImage {
        RgbImage::from_fn(16, 12, |x, y| match (y, x) {
            (0..2 | 10.., _) => Rgb([0, 0, 0]),
            (_, 0..8) => Rgb([220, 30, 30]),
            _ => Rgb([30, 30, 220]),
        })
    }

    #[test]
    fn covers_lose_their_bars_and_become_square() {
        let Art { picture, image } = decode(&png(&letterboxed())).unwrap();
        assert_eq!(image.dimensions(), (8, 8));
        assert_eq!((picture.width, picture.height), (SIZE as u16, SIZE as u16));
        let at = |x: usize, y: usize| picture.pixels[y * SIZE as usize + x];
        // No black rows survive at the top or bottom.
        assert!(at(32, 0)[0] > 100 || at(32, 0)[2] > 100, "{:?}", at(32, 0));
        assert!(at(0, 63)[0] > 200 && at(63, 63)[2] > 200);
    }

    #[test]
    fn embedded_covers_load_from_the_file() {
        let path = crate::testutil::wav_with_cover("embedded-art", png(&letterboxed()));
        let client = HttpClient::new(None);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let art = load(&path.to_string_lossy(), Path::new("unused"), &client, &runtime).unwrap();
        assert_eq!(art.picture.pixels.len(), (SIZE * SIZE) as usize);
    }

    #[test]
    fn fetched_covers_are_read_back_from_the_cache() {
        let dir = crate::testutil::scratch("art-cache");
        let url = "https://i.scdn.co/image/ab67616d00001e02";
        keep(&dir.join(format!("{:016x}", hash(url))), &png(&letterboxed())).unwrap();
        // The runtime has no I/O driver, so this reads the cache, not the network.
        let client = HttpClient::new(None);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert!(load(url, &dir, &client, &runtime).is_ok());
    }

    #[test]
    fn large_covers_are_kept_at_a_bounded_size() {
        let big = RgbImage::from_pixel(1000, 1000, Rgb([90, 90, 90]));
        assert_eq!(decode(&png(&big)).unwrap().image.dimensions(), (LARGEST, LARGEST));
    }

    #[test]
    fn missing_pictures_are_errors() {
        assert!(decode(b"not an image").is_err());
        let bare = crate::testutil::tagged_wav("bare-art", "Song", "Band", 100);
        assert!(embedded(&bare).is_err());
    }
}
