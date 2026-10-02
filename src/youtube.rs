//! YouTube audio through an installed yt-dlp. AgentAmp downloads the audio
//! track once into its cache and plays the file, so a song heard again uses
//! no network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

use crate::model::{Source, Track};

/// symphonia decodes AAC in MP4 but not Opus, so only m4a audio will do.
const FORMAT: &str = "bestaudio[ext=m4a]";
const TIMEOUT: Duration = Duration::from_secs(120);

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

/// Downloads `url` (a video link or `ytsearch1:` query) into `dir`.
pub async fn fetch(url: &str, dir: &Path) -> Result<Track> {
    std::fs::create_dir_all(dir)?;
    let output = Command::new(program())
        .args(["--no-playlist", "--no-progress", "-f", FORMAT, "-o"])
        .arg(dir.join("%(id)s.%(ext)s"))
        .args(["--print", "after_move:%(.{id,title,uploader,duration,filepath,webpage_url})j", "--", url])
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(TIMEOUT, output).await {
        Err(_) => bail!("yt-dlp took longer than {} seconds", TIMEOUT.as_secs()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("YouTube needs yt-dlp; install it (pacman -S yt-dlp, brew install yt-dlp)")
        }
        Ok(result) => result.context("cannot run yt-dlp")?,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|l| l.contains("ERROR")).unwrap_or(stderr.trim());
        bail!("yt-dlp could not get {url}: {reason}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().rev().find(|l| l.starts_with('{')).context("yt-dlp found nothing")?;
    parse(line)
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
    fn unexpected_output_is_an_error() {
        assert!(parse("{}").is_err());
    }
}
