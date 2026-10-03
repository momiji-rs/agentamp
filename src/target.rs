//! What `play` and `add` were asked for: a Spotify link, the account's
//! Liked Songs, a YouTube link or search, or a file or folder.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A canonical `spotify:{kind}:{id}` URI, or `spotify:collection` for
    /// the signed-in account's Liked Songs.
    Spotify { kind: SpotifyKind, uri: String },
    /// A video URL, or `ytsearch1:{query}` for yt-dlp.
    Youtube(String),
    File(PathBuf),
    Folder(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotifyKind {
    Track,
    Album,
    Playlist,
    Liked,
}

impl SpotifyKind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "track" => Some(Self::Track),
            "album" => Some(Self::Album),
            "playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}

/// File extensions rodio's decoders read.
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "mp4", "aac", "ogg", "oga", "wav"];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn parse(input: &str) -> Result<Target> {
    let input = input.trim();
    if let Some(query) = input.strip_prefix("yt:") {
        let query = query.trim();
        if query.is_empty() {
            bail!("`yt:` needs something to search for");
        }
        return Ok(Target::Youtube(format!("ytsearch1:{query}")));
    }
    // The CLI sends a file called `liked` as an absolute path.
    if input.eq_ignore_ascii_case("liked") {
        return Ok(liked());
    }
    if let Some(rest) = input.strip_prefix("spotify:") {
        if rest == "collection" || rest.strip_prefix("user:").is_some_and(|r| r.ends_with(":collection") && r.matches(':').count() == 1) {
            return Ok(liked());
        }
        let mut parts = rest.split(':');
        // spotify:user:{name}:playlist:{id} is the old playlist form.
        let (kind, id) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("user"), Some(_), Some(kind), Some(id)) => (kind, id),
            (Some(kind), Some(id), None, None) => (kind, id),
            _ => bail!("not a Spotify URI: {input}"),
        };
        return spotify(kind, id, input);
    }
    if let Some(path) = web_path(input, &["open.spotify.com", "play.spotify.com"]) {
        let mut segments = path.split('/').filter(|s| !s.is_empty());
        let mut kind = segments.next().unwrap_or_default();
        // Localised links: open.spotify.com/intl-ja/track/{id}
        if kind.starts_with("intl-") {
            kind = segments.next().unwrap_or_default();
        }
        let id = segments.next().unwrap_or_default();
        if (kind, id) == ("collection", "tracks") {
            return Ok(liked());
        }
        return spotify(kind, id, input);
    }
    if web_path(input, &["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be"]).is_some() {
        return Ok(Target::Youtube(input.to_string()));
    }
    let path = expand_home(input);
    if path.is_dir() {
        return Ok(Target::Folder(path));
    }
    if path.is_file() {
        if !is_audio(&path) {
            bail!("not an audio file AgentAmp can play: {}", path.display());
        }
        return Ok(Target::File(path));
    }
    bail!("nothing to play at {input}: not a Spotify or YouTube link, and no such file")
}

/// The target as the daemon must hear it: a path relative to the asker's
/// directory `from` made absolute, since the daemon runs in another one.
/// Links, searches and anything that is no file here pass unchanged.
pub fn from_dir(input: &str, from: &Path) -> String {
    let trimmed = input.trim();
    let path = Path::new(trimmed);
    if !trimmed.is_empty() && path.is_relative() && !trimmed.starts_with('~') && from.join(path).exists() {
        return from.join(path).to_string_lossy().into_owned();
    }
    input.to_string()
}

fn liked() -> Target {
    Target::Spotify { kind: SpotifyKind::Liked, uri: "spotify:collection".into() }
}

fn spotify(kind: &str, id: &str, input: &str) -> Result<Target> {
    let Some(kind_value) = SpotifyKind::parse(kind) else {
        bail!("AgentAmp plays Spotify tracks, albums and playlists, not this: {input}");
    };
    let id = id.split(['?', '#']).next().unwrap_or_default();
    if id.len() != 22 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("not a Spotify ID: {input}");
    }
    Ok(Target::Spotify { kind: kind_value, uri: format!("spotify:{kind}:{id}") })
}

/// The path of an http(s) URL on one of `hosts`.
fn web_path<'a>(input: &'a str, hosts: &[&str]) -> Option<&'a str> {
    let rest = input.strip_prefix("https://").or_else(|| input.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    hosts.contains(&host).then_some(path)
}

fn expand_home(input: &str) -> PathBuf {
    match (input.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(input),
    }
}

/// Every audio file under `folder`, in path order.
pub fn audio_files(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(path),
                Ok(_) if is_audio(&path) => files.push(path),
                _ => {}
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4iV5W9uYEdYUVa79Axb7Rh";

    fn track() -> Target {
        Target::Spotify { kind: SpotifyKind::Track, uri: format!("spotify:track:{ID}") }
    }

    #[test]
    fn spotify_uris_and_links_become_one_uri() {
        assert_eq!(parse(&format!("spotify:track:{ID}")).unwrap(), track());
        assert_eq!(parse(&format!("https://open.spotify.com/track/{ID}?si=abc")).unwrap(), track());
        assert_eq!(parse(&format!("https://open.spotify.com/intl-ja/track/{ID}")).unwrap(), track());
        assert_eq!(
            parse(&format!("spotify:user:someone:playlist:{ID}")).unwrap(),
            Target::Spotify { kind: SpotifyKind::Playlist, uri: format!("spotify:playlist:{ID}") }
        );
        assert_eq!(
            parse(&format!("https://open.spotify.com/album/{ID}")).unwrap(),
            Target::Spotify { kind: SpotifyKind::Album, uri: format!("spotify:album:{ID}") }
        );
    }

    #[test]
    fn liked_songs_by_name_uri_or_link() {
        for input in ["liked", "Liked", "spotify:collection", "spotify:user:12572401:collection", "https://open.spotify.com/collection/tracks"] {
            assert_eq!(parse(input).unwrap(), liked(), "{input}");
        }
        assert!(parse("spotify:user:a:b:collection").is_err());
        assert!(parse("https://open.spotify.com/collection/albums").is_err());
    }

    #[test]
    fn unsupported_spotify_links_are_refused() {
        assert!(parse(&format!("spotify:artist:{ID}")).is_err());
        assert!(parse("spotify:track:short").is_err());
        assert!(parse("spotify:track").is_err());
    }

    #[test]
    fn youtube_links_and_searches() {
        let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
        assert_eq!(parse(url).unwrap(), Target::Youtube(url.into()));
        assert_eq!(parse("https://youtu.be/dQw4w9WgXcQ").unwrap(), Target::Youtube("https://youtu.be/dQw4w9WgXcQ".into()));
        assert_eq!(parse("yt: city pop 1983 ").unwrap(), Target::Youtube("ytsearch1:city pop 1983".into()));
        assert!(parse("yt:").is_err());
    }

    #[test]
    fn relative_paths_are_made_absolute_for_the_daemon() {
        let dir = crate::testutil::scratch("relative_paths");
        std::fs::create_dir_all(dir.join("album")).unwrap();
        std::fs::write(dir.join("album/song.flac"), b"").unwrap();
        let absolute = dir.join("album/song.flac").to_string_lossy().into_owned();
        assert_eq!(from_dir("album/song.flac", &dir), absolute);
        assert_eq!(from_dir(&absolute, Path::new("/")), absolute);
        assert_eq!(from_dir("album", &dir), dir.join("album").to_string_lossy());
        // Not files here: the daemon reads them as they are.
        for target in ["", " ", "yt:album", "spotify:track:6rqhFgbbKwnb9MLmUQDhG6", "https://youtu.be/x", "~/Music", "no-such-file.mp3"] {
            assert_eq!(from_dir(target, &dir), target);
        }
    }

    #[test]
    fn files_and_folders() {
        let dir = crate::testutil::scratch("files_and_folders");
        let song = dir.join("b/Song.MP3");
        std::fs::create_dir_all(song.parent().unwrap()).unwrap();
        std::fs::write(&song, b"").unwrap();
        std::fs::write(dir.join("a.flac"), b"").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"").unwrap();

        assert_eq!(parse(song.to_str().unwrap()).unwrap(), Target::File(song.clone()));
        assert_eq!(parse(dir.to_str().unwrap()).unwrap(), Target::Folder(dir.clone()));
        assert!(parse(dir.join("cover.jpg").to_str().unwrap()).is_err());
        assert_eq!(audio_files(&dir), vec![dir.join("a.flac"), song]);
        assert!(parse(dir.join("missing.mp3").to_str().unwrap()).is_err());
    }
}
