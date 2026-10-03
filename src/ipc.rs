//! The daemon's protocol: one JSON request per line, one JSON response per
//! line, over a Unix socket only the owner can open.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Replace the queue with `target` and start it.
    Play { target: String },
    /// Queue `target`, after the current track when `next` is set.
    Add {
        target: String,
        #[serde(default)]
        next: bool,
    },
    Pause,
    Resume,
    Toggle,
    Next,
    /// Back to the start of the track, or to the one before near its start.
    Previous,
    Stop,
    Clear,
    Status,
    Queue,
    Volume { percent: u8 },
    Seek { position_ms: u32 },
    /// Search Spotify for up to `count` tracks, albums and playlists.
    SearchSpotify { query: String, count: u8 },
    Shutdown,
    /// Stream the sound as it plays, for drawing it: after the answer the
    /// connection carries `tap` chunks, not JSON.
    Listen,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(data: impl Serialize) -> Self {
        Self { ok: true, data: serde_json::to_value(data).ok(), error: None }
    }

    pub fn error(error: impl std::fmt::Display) -> Self {
        Self { ok: false, data: None, error: Some(format!("{error:#}")) }
    }

    pub fn into_result(self) -> Result<Value> {
        if self.ok {
            Ok(self.data.unwrap_or(Value::Null))
        } else {
            Err(anyhow!(self.error.unwrap_or_else(|| "the daemon refused".into())))
        }
    }
}

/// Sends one request and waits for its answer. Resolving a YouTube video
/// can take a while, so the read waits up to two minutes.
pub fn call(socket: &Path, request: &Request) -> Result<Value> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut writer = &stream;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).context("the daemon did not answer")?;
    let response: Response = serde_json::from_str(&answer).context("the daemon's answer is not JSON")?;
    response.into_result()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_tagged_by_command() {
        let add: Request = serde_json::from_str(r#"{"cmd":"add","target":"yt:lofi"}"#).unwrap();
        assert_eq!(add, Request::Add { target: "yt:lofi".into(), next: false });
        assert_eq!(serde_json::to_string(&Request::Pause).unwrap(), r#"{"cmd":"pause"}"#);
        assert_eq!(
            serde_json::to_string(&Request::Volume { percent: 40 }).unwrap(),
            r#"{"cmd":"volume","percent":40}"#
        );
    }

    #[test]
    fn errors_carry_their_message() {
        let err = Response::error("no such file").into_result().unwrap_err();
        assert_eq!(err.to_string(), "no such file");
        assert_eq!(Response::ok(3).into_result().unwrap(), Value::from(3));
    }
}
