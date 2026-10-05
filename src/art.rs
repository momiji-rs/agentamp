//! Covers downloaded once and kept in the cache, named after their URL:
//! the window reads them there, and the daemon fetches them for windows
//! of their own that ask for one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use librespot_core::http_client::HttpClient;

/// Where the cover at `url` is kept.
pub fn file(dir: &Path, url: &str) -> PathBuf {
    dir.join(format!("{:016x}", crate::tui::cover::hash(url)))
}

/// Writes the whole file or nothing, so a cut-off download is never read.
/// Each write has a partial file of its own: the window and the daemon can
/// fetch the same cover at once.
pub fn keep(file: &Path, bytes: &[u8]) -> Result<()> {
    static WRITES: AtomicU32 = AtomicU32::new(0);
    std::fs::create_dir_all(file.parent().context("no cover directory")?)?;
    let partial = file.with_extension(format!("{}-{}.part", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    Ok(())
}

/// A cover from Spotify's image servers or a YouTube thumbnail: the only
/// pictures the daemon fetches for a window.
pub fn allowed(url: &str) -> bool {
    let Ok(uri) = url.parse::<http::Uri>() else { return false };
    let host = uri.host().unwrap_or_default();
    uri.scheme_str() == Some("https")
        && uri.port().is_none()
        && (host == "i.ytimg.com" || host.ends_with(".scdn.co") || host.ends_with(".spotifycdn.com"))
}

/// The kept file of the cover at `url`, downloaded first when it is not
/// kept yet.
pub async fn fetch(url: &str, dir: &Path) -> Result<PathBuf> {
    static CLIENT: OnceLock<HttpClient> = OnceLock::new();
    if !allowed(url) {
        bail!("not a Spotify or YouTube cover: {url}");
    }
    let file = file(dir, url);
    if file.is_file() {
        return Ok(file);
    }
    let request = http::Request::get(url).body(Bytes::new())?;
    let client = CLIENT.get_or_init(|| HttpClient::new(None));
    let bytes = client.request_body(request).await.context("cannot fetch the cover")?;
    image::guess_format(&bytes).context("the cover is not a picture")?;
    let kept = file.clone();
    tokio::task::spawn_blocking(move || keep(&kept, &bytes)).await??;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_spotify_and_youtube_covers_are_fetched() {
        assert!(allowed("https://i.scdn.co/image/ab67616d00001e02b410256f0c9158425ee88463"));
        assert!(allowed("https://image-cdn-fa.spotifycdn.com/image/ab67706c0000da84430fa5b9812ec34e340d2614"));
        assert!(allowed("https://mosaic.scdn.co/300/ab67616d0000b273"));
        assert!(allowed("https://i.ytimg.com/vi/T_lC2O1oIew/hqdefault.jpg"));
        assert!(!allowed("http://i.scdn.co/image/ab67616d00001e02"), "not over TLS");
        assert!(!allowed("https://i.scdn.co:8443/image/x"));
        assert!(!allowed("https://example.com/i.scdn.co/image/x"));
        assert!(!allowed("https://scdn.co.example.com/image/x"));
        assert!(!allowed("/music/a.flac"));
    }

    #[test]
    fn a_kept_cover_is_not_fetched_again() {
        let dir = crate::testutil::scratch("art-kept");
        let url = "https://i.scdn.co/image/ab67616d00001e02";
        keep(&file(&dir, url), b"\x89PNG").unwrap();
        // No I/O driver: a fetch would fail, so this reads the cache.
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert_eq!(runtime.block_on(fetch(url, &dir)).unwrap(), file(&dir, url));
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no partial file is left: {names:?}");
        assert!(runtime.block_on(fetch("https://example.com/x.png", &dir)).is_err());
    }
}
