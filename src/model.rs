//! What the daemon plays and reports.

use std::time::Duration;

use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};

/// Drops the `uint32`-style formats schemars gives unsigned integers: they
/// are no JSON Schema format, and validators warn about each one. Their
/// `minimum: 0` stays.
pub fn plain_integers(schema: &mut Schema) {
    if schema.get("format").and_then(|f| f.as_str()).is_some_and(|f| f.starts_with("uint")) {
        schema.remove("format");
    }
    schemars::transform::transform_subschemas(&mut plain_integers, schema);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Spotify,
    Local,
    Youtube,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Track {
    pub source: Source,
    /// A `spotify:track:` URI, or the path of an audio file.
    pub uri: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artist: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub album: String,
    #[serde(default)]
    pub duration_ms: u32,
    /// Where the track came from, when that is not `uri` (a YouTube page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// The cover: an https image URL, or for a local file the file itself,
    /// whose tags hold the picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art: Option<String>,
    /// A YouTube track still downloading: `uri` is its link or search until
    /// the file is here, and it plays as soon as it is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub downloading: bool,
}

impl Track {
    pub fn placeholder(source: Source, uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            source,
            title: uri.clone(),
            uri,
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            link: None,
            art: None,
            downloading: false,
        }
    }

    /// "Title · Artist", or the title alone.
    pub fn label(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} · {}", self.title, self.artist)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Status {
    pub state: State,
    /// What is playing or paused; none once stopped.
    pub track: Option<Track>,
    pub position_ms: u32,
    /// 0 to 100.
    pub volume: u8,
    /// How many tracks wait after this one.
    pub queue_len: usize,
    /// Why playback stopped by itself, until the next track starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `m:ss`, or `h:mm:ss` from an hour up.
pub fn clock(ms: u32) -> String {
    let secs = Duration::from_millis(ms.into()).as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Status {
    /// One line for a terminal or a status bar.
    pub fn line(&self) -> String {
        let Some(track) = &self.track else {
            return match &self.error {
                Some(error) => format!("■ Stopped: {error}"),
                None => "■ Nothing playing".into(),
            };
        };
        let icon = match self.state {
            State::Playing => "▶",
            State::Paused => "⏸",
            State::Stopped => "■",
        };
        if track.downloading {
            return format!("{icon} {}  downloading", track.label());
        }
        let mut line = format!("{icon} {}  {}", track.label(), clock(self.position_ms));
        if track.duration_ms > 0 {
            line.push_str(&format!(" / {}", clock(track.duration_ms)));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(83_999), "1:23");
        assert_eq!(clock(3_725_000), "1:02:05");
    }

    #[test]
    fn status_line_names_the_track_and_its_length() {
        let mut track = Track::placeholder(Source::Spotify, "spotify:track:x");
        track.title = "晴天".into();
        track.artist = "周杰倫".into();
        track.duration_ms = 269_000;
        let status = Status {
            state: State::Playing,
            track: Some(track),
            position_ms: 83_000,
            volume: 80,
            queue_len: 0,
            error: None,
        };
        assert_eq!(status.line(), "▶ 晴天 · 周杰倫  1:23 / 4:29");
        let status = Status { track: Some(crate::youtube::pending("ytsearch1:plastic love")), position_ms: 0, ..status };
        assert_eq!(status.line(), "▶ plastic love  downloading", "a frozen 0:00 would look stuck");
    }

    #[test]
    fn status_line_without_a_track() {
        let status = Status {
            state: State::Stopped,
            track: None,
            position_ms: 0,
            volume: 80,
            queue_len: 0,
            error: None,
        };
        assert_eq!(status.line(), "■ Nothing playing");
    }

    #[test]
    fn track_json_omits_empty_fields() {
        let track = Track::placeholder(Source::Local, "/music/a.mp3");
        let json = serde_json::to_value(&track).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"source": "local", "uri": "/music/a.mp3", "title": "/music/a.mp3", "duration_ms": 0})
        );
    }
}
