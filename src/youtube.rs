//! YouTube audio through an installed yt-dlp. AgentAmp downloads the audio
//! track once into its cache and plays the file, so a song heard again uses
//! no network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::model::{Source, Track, plain_integers};

/// symphonia decodes AAC in MP4 but not Opus, so only m4a audio will do.
const FORMAT: &str = "bestaudio[ext=m4a]";
const TIMEOUT: Duration = Duration::from_secs(120);
const RETRY_AFTER: Duration = Duration::from_secs(1);
/// A search reads one page of results and downloads nothing.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Info {
    id: String,
    title: String,
    #[serde(default)]
    uploader: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    filepath: PathBuf,
    #[serde(default)]
    webpage_url: Option<String>,
}

/// The yt-dlp to run: `AGENTAMP_YTDLP`, or `yt-dlp` on the PATH.
fn program() -> PathBuf {
    std::env::var_os("AGENTAMP_YTDLP").map_or_else(|| PathBuf::from("yt-dlp"), PathBuf::from)
}

/// What the queue shows for `url` (a video link or `ytsearch1:` query)
/// until `fetch` has the file: the search, or the link.
pub fn pending(url: &str) -> Track {
    let mut track = Track::placeholder(Source::Youtube, url);
    match url.strip_prefix("ytsearch1:") {
        Some(query) => track.title = query.to_string(),
        None => track.link = Some(url.to_string()),
    }
    track.downloading = true;
    track
}

/// Downloads `url` (a video link or `ytsearch1:` query) into `dir`.
pub async fn fetch(url: &str, dir: &Path) -> Result<Track> {
    std::fs::create_dir_all(dir)?;
    let mut output = download(url, dir).await?;
    // YouTube refuses some requests for the audio and lets the same one
    // through a moment later, so a refusal is asked again once.
    if !output.status.success() && String::from_utf8_lossy(&output.stderr).contains("HTTP Error 403") {
        tokio::time::sleep(RETRY_AFTER).await;
        output = download(url, dir).await?;
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|l| l.contains("ERROR")).unwrap_or(stderr.trim());
        bail!("yt-dlp could not get {url}: {reason}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().rev().find(|l| l.starts_with('{')).context("yt-dlp found nothing")?;
    parse(line)
}

async fn download(url: &str, dir: &Path) -> Result<std::process::Output> {
    let mut command = Command::new(program());
    command
        .args(["--no-playlist", "--no-progress", "-f", FORMAT, "-o"])
        .arg(dir.join("%(id)s.%(ext)s"))
        .args(["--print", "after_move:%(.{id,title,uploader,duration,filepath,webpage_url})j", "--", url]);
    run(command, TIMEOUT).await
}

/// A video a search found, not yet downloaded.
#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Found {
    pub title: String,
    /// The channel that posted it.
    pub artist: String,
    /// 0 when YouTube does not say.
    pub duration_ms: u32,
    /// The video's link, for `play` or `add`.
    pub target: String,
}

/// The first `count` videos YouTube finds for `query`. Live streams are left
/// out: they never end, so there is no file to download and play.
pub async fn search(query: &str, count: u8) -> Result<Vec<Found>> {
    let mut command = Command::new(program());
    command.args(["--flat-playlist", "--dump-json", "--no-warnings", "--"]).arg(format!("ytsearch{count}:{query}"));
    let output = run(command, SEARCH_TIMEOUT).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|l| l.contains("ERROR")).unwrap_or(stderr.trim());
        bail!("yt-dlp could not search for {query}: {reason}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter_map(found).collect())
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    title: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    duration: Option<f64>,
    url: Option<String>,
    live_status: Option<String>,
}

fn found(line: &str) -> Option<Found> {
    let entry: Entry = serde_json::from_str(line).ok()?;
    if matches!(entry.live_status.as_deref(), Some("is_live" | "is_upcoming")) {
        return None;
    }
    Some(Found {
        title: entry.title?,
        artist: entry.uploader.or(entry.channel).unwrap_or_default(),
        duration_ms: entry.duration.map_or(0, |d| (d * 1000.0) as u32),
        target: entry.url.unwrap_or_else(|| format!("https://www.youtube.com/watch?v={}", entry.id)),
    })
}

async fn run(mut command: Command, timeout: Duration) -> Result<std::process::Output> {
    let output = command.kill_on_drop(true).output();
    match tokio::time::timeout(timeout, output).await {
        Err(_) => bail!("yt-dlp took longer than {} seconds", timeout.as_secs()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("YouTube needs yt-dlp; install it (pacman -S yt-dlp, brew install yt-dlp)")
        }
        Ok(result) => result.context("cannot run yt-dlp"),
    }
}

fn parse(line: &str) -> Result<Track> {
    let info: Info = serde_json::from_str(line).context("unexpected yt-dlp output")?;
    let mut track = Track::placeholder(Source::Youtube, info.filepath.to_string_lossy());
    track.title = info.title;
    track.artist = info.uploader.unwrap_or_default();
    track.duration_ms = info.duration.map_or(0, |d| (d * 1000.0) as u32);
    track.link = Some(info.webpage_url.unwrap_or_else(|| format!("https://www.youtube.com/watch?v={}", info.id)));
    // Every video has this one, as a JPEG.
    track.art = Some(format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", info.id));
    Ok(track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yt_dlp_output() {
        let track = parse(
            r#"{"id": "T_lC2O1oIew", "title": "Plastic Love", "uploader": "Mariya Takeuchi", "duration": 309.5, "filepath": "/c/T_lC2O1oIew.m4a", "webpage_url": null}"#,
        )
        .unwrap();
        assert_eq!(track.source, Source::Youtube);
        assert_eq!(track.uri, "/c/T_lC2O1oIew.m4a");
        assert_eq!(track.label(), "Plastic Love · Mariya Takeuchi");
        assert_eq!(track.duration_ms, 309_500);
        assert_eq!(track.link.as_deref(), Some("https://www.youtube.com/watch?v=T_lC2O1oIew"));
        assert_eq!(track.art.as_deref(), Some("https://i.ytimg.com/vi/T_lC2O1oIew/hqdefault.jpg"));
    }

    #[test]
    fn searches_list_videos_and_leave_out_live_streams() {
        let video = r#"{"_type": "url", "id": "T_lC2O1oIew", "title": "Plastic Love", "uploader": "Mariya Takeuchi", "channel": "Mariya Takeuchi", "duration": 309, "url": "https://www.youtube.com/watch?v=T_lC2O1oIew", "live_status": null}"#;
        assert_eq!(
            found(video),
            Some(Found {
                title: "Plastic Love".into(),
                artist: "Mariya Takeuchi".into(),
                duration_ms: 309_000,
                target: "https://www.youtube.com/watch?v=T_lC2O1oIew".into(),
            })
        );
        let bare = found(r#"{"id": "abc", "title": "Mix", "channel": "Night Tempo"}"#).unwrap();
        assert_eq!((bare.artist.as_str(), bare.duration_ms), ("Night Tempo", 0));
        assert_eq!(bare.target, "https://www.youtube.com/watch?v=abc");
        assert_eq!(found(r#"{"id": "radio", "title": "24/7 radio", "live_status": "is_live"}"#), None);
        assert_eq!(found(r#"{"id": "soon", "title": "Premiere", "live_status": "is_upcoming"}"#), None);
        assert_eq!(found("not json"), None);
    }

    #[test]
    fn a_pending_track_shows_its_search_or_link() {
        let search = pending("ytsearch1:plastic love");
        assert_eq!((search.title.as_str(), search.uri.as_str()), ("plastic love", "ytsearch1:plastic love"));
        assert!(search.downloading && search.link.is_none());
        let link = pending("https://youtu.be/T_lC2O1oIew");
        assert_eq!(link.title, "https://youtu.be/T_lC2O1oIew");
        assert_eq!(link.link.as_deref(), Some("https://youtu.be/T_lC2O1oIew"));
        assert_eq!(link.source, Source::Youtube);
    }

    #[test]
    fn unexpected_output_is_an_error() {
        assert!(parse("{}").is_err());
    }
}
