mod daemon;
mod deck;
mod engine;
mod ipc;
mod library;
mod mcp;
mod model;
mod paths;
mod queue;
mod resolve;
mod spotify;
mod spotify_search;
mod tap;
mod target;
mod trace;
mod tui;
mod youtube;

use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::ipc::Request;
use crate::model::{Status, Track, clock};
use crate::paths::Paths;

/// A tiny music player for Spotify, YouTube and your own files.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Print the daemon's JSON answer instead of a line of text.
    #[arg(long, global = true)]
    json: bool,
    /// With no command, AgentAmp opens its window.
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Play now, replacing the queue: a Spotify or YouTube link,
    /// `yt:<search>`, or a file or folder.
    Play { target: Vec<String> },
    /// Add to the end of the queue, or after the current track with --next.
    Add {
        target: Vec<String>,
        #[arg(long)]
        next: bool,
    },
    Pause,
    Resume,
    /// Pause or resume.
    Toggle,
    /// Skip to the next track.
    Next,
    /// Back to the start of the track, or to the previous one.
    Prev,
    /// Stop playback; the queue stays.
    Stop,
    /// Empty the queue after the current track.
    Clear,
    /// Search Spotify for tracks, albums and playlists to play.
    Search {
        query: Vec<String>,
        /// How many of each kind, 1 to 10.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=10))]
        count: u8,
    },
    /// What is playing.
    Now,
    /// What plays next.
    Queue,
    /// Set the volume, 0 to 100.
    Volume { percent: u8 },
    /// Jump to a position in seconds, or m:ss.
    Seek { position: String },
    /// Open the window (the default). Closing it keeps the music playing.
    Tui {
        /// Print one frame of the window at this size, as terminal output, and exit.
        #[arg(long, value_name = "COLSxROWS")]
        frame: Option<String>,
    },
    /// Sign in to Spotify (Premium) in the browser.
    Login,
    /// Forget the Spotify sign-in.
    Logout,
    /// Serve the Model Context Protocol on stdin and stdout, for agents.
    Mcp,
    /// Run the player in the foreground (it starts by itself otherwise).
    Daemon,
    /// Stop the background player.
    Quit,
}

fn main() -> ExitCode {
    trace::mark("main");
    let cli = Cli::parse();
    trace::mark("arguments");
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("agentamp: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let paths = Paths::new()?;
    trace::mark("paths");
    let request = match cli.command.unwrap_or(Cmd::Tui { frame: None }) {
        Cmd::Tui { frame: None } => return tui::run(paths),
        Cmd::Tui { frame: Some(size) } => {
            let (cols, rows) = tui::parse_size(&size)?;
            print!("{}", tui::frame(&paths, cols, rows)?);
            return Ok(());
        }
        Cmd::Daemon => return run_daemon(paths),
        Cmd::Mcp => return mcp::run(paths),
        Cmd::Login => {
            let name = spotify::login(&paths)?;
            println!("Signed in to Spotify as {name}.");
            // A running daemon keeps its old session; the next start reads this one.
            if paths.socket().exists() {
                println!("Run `agentamp quit` so the player picks up the new sign-in.");
            }
            return Ok(());
        }
        Cmd::Logout => {
            let removed = spotify::logout(&paths)?;
            println!("{}", if removed { "Signed out of Spotify." } else { "Not signed in." });
            return Ok(());
        }
        Cmd::Play { target } => Request::Play { target: here(&target.join(" ")) },
        Cmd::Add { target, next } => Request::Add { target: here(&target.join(" ")), next },
        Cmd::Pause => Request::Pause,
        Cmd::Resume => Request::Resume,
        Cmd::Toggle => Request::Toggle,
        Cmd::Next => Request::Next,
        Cmd::Prev => Request::Previous,
        Cmd::Stop => Request::Stop,
        Cmd::Clear => Request::Clear,
        Cmd::Search { query, count } => Request::SearchSpotify { query: query.join(" "), count },
        Cmd::Now => Request::Status,
        Cmd::Queue => Request::Queue,
        Cmd::Volume { percent } => Request::Volume { percent },
        Cmd::Seek { position } => Request::Seek { position_ms: parse_position(&position)? },
        Cmd::Quit => {
            if !paths.socket().exists() {
                return Ok(());
            }
            Request::Shutdown
        }
    };
    let autostart = !matches!(request, Request::Shutdown | Request::Status | Request::Queue);
    let data = send(&paths, &request, autostart)?;
    if cli.json {
        println!("{data}");
    } else if let Some(text) = describe(&request, &data) {
        println!("{text}");
    }
    Ok(())
}

fn run_daemon(paths: Paths) -> Result<()> {
    env_logger::Builder::new()
        .parse_filters(&std::env::var("AGENTAMP_LOG").unwrap_or_else(|_| "agentamp=info,librespot=warn".into()))
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(daemon::run(paths))
}

/// Sends `request`, starting the daemon first when it is not running.
/// A target relative to this process's directory, for the daemon.
pub fn here(target: &str) -> String {
    std::env::current_dir().map_or_else(|_| target.to_string(), |dir| target::from_dir(target, &dir))
}

fn send(paths: &Paths, request: &Request, autostart: bool) -> Result<Value> {
    let socket = paths.socket();
    match ipc::call(&socket, request) {
        Err(e) if is_absent(&e) => {
            if !autostart {
                if let Request::Status = request {
                    return Ok(serde_json::to_value(Status {
                        state: model::State::Stopped,
                        track: None,
                        position_ms: 0,
                        // What the player will start at.
                        volume: daemon::DEFAULT_VOLUME,
                        queue_len: 0,
                        error: None,
                    })?);
                }
                if let Request::Queue = request {
                    return Ok(serde_json::json!({"current": null, "upcoming": []}));
                }
                return Ok(Value::Null);
            }
            spawn_daemon(paths)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match ipc::call(&socket, request) {
                    Err(e) if is_absent(&e) && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25))
                    }
                    result => return result,
                }
            }
        }
        result => result,
    }
}

/// The daemon is not running (no socket, or nobody behind it).
fn is_absent(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
    })
}

fn spawn_daemon(paths: &Paths) -> Result<()> {
    use std::os::unix::process::CommandExt;
    std::fs::create_dir_all(&paths.cache)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log())?;
    Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        // Its own process group: closing the terminal does not stop the music.
        .process_group(0)
        .spawn()
        .context("cannot start the player")?;
    Ok(())
}

fn parse_position(text: &str) -> Result<u32> {
    let seconds = match text.split_once(':') {
        Some((m, s)) => m.parse::<u32>()? * 60 + s.parse::<u32>()?,
        None => text.parse::<u32>()?,
    };
    Ok(seconds * 1000)
}

fn describe(request: &Request, data: &Value) -> Option<String> {
    match request {
        Request::Shutdown | Request::Clear => None,
        Request::Queue => {
            let current: Option<Track> = serde_json::from_value(data["current"].clone()).ok().flatten();
            let upcoming: Vec<Track> = serde_json::from_value(data["upcoming"].clone()).unwrap_or_default();
            let mut lines = vec![match &current {
                Some(t) => format!("▶ {}", t.label()),
                None => "Nothing playing".into(),
            }];
            lines.extend(upcoming.iter().enumerate().map(|(i, t)| {
                let length = if t.duration_ms > 0 { format!("  {}", clock(t.duration_ms)) } else { String::new() };
                format!("{:>3}. {}{length}", i + 1, t.label())
            }));
            Some(lines.join("\n"))
        }
        Request::Add { .. } => {
            let added = data["added"].as_u64().unwrap_or(0);
            Some(format!("Added {added} track{}", if added == 1 { "" } else { "s" }))
        }
        Request::Play { .. } => status_line(&data["status"]),
        Request::SearchSpotify { .. } => serde_json::from_value(data.clone()).ok().map(|f| found(&f)),
        _ => status_line(data),
    }
}

/// Search results by kind, each line ending with the target to play.
fn found(found: &spotify_search::Found) -> String {
    let mut lines = Vec::new();
    for (kind, hits) in [("Tracks", &found.tracks), ("Albums", &found.albums), ("Playlists", &found.playlists)] {
        if hits.is_empty() {
            continue;
        }
        lines.push(kind.to_string());
        lines.extend(hits.iter().map(|h| {
            let by = if h.artist.is_empty() { String::new() } else { format!(" · {}", h.artist) };
            let year = h.year.map_or(String::new(), |y| format!(" ({y})"));
            let length = h.duration_ms.filter(|ms| *ms > 0).map_or(String::new(), |ms| format!("  {}", clock(ms)));
            format!("  {}{by}{year}{length}  {}", h.title, h.target)
        }));
    }
    if lines.is_empty() { "Nothing found".into() } else { lines.join("\n") }
}

fn status_line(data: &Value) -> Option<String> {
    serde_json::from_value::<Status>(data.clone()).ok().map(|s| s.line())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_in_seconds_or_minutes() {
        assert_eq!(parse_position("90").unwrap(), 90_000);
        assert_eq!(parse_position("1:30").unwrap(), 90_000);
        assert!(parse_position("x").is_err());
    }
}

#[cfg(test)]
mod testutil {
    use std::path::PathBuf;

    /// A fresh directory under `target/` (never /tmp, a small tmpfs).
    pub fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A silent mono WAV of `ms` milliseconds with a title and artist.
    pub fn tagged_wav(name: &str, title: &str, artist: &str, ms: u32) -> PathBuf {
        use lofty::config::WriteOptions;
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagType};

        let path = scratch(name).join("song.wav");
        std::fs::write(&path, silent_wav(ms)).unwrap();
        let mut file = lofty::read_from_path(&path).unwrap();
        let mut tag = Tag::new(TagType::RiffInfo);
        tag.set_title(title.into());
        tag.set_artist(artist.into());
        file.insert_tag(tag);
        file.save_to_path(&path, WriteOptions::default()).unwrap();
        path
    }

    /// A silent WAV file whose ID3 tag holds `cover` as its front cover.
    pub fn wav_with_cover(name: &str, cover: Vec<u8>) -> PathBuf {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagType};

        let path = scratch(name).join("covered.wav");
        std::fs::write(&path, silent_wav(100)).unwrap();
        let mut file = lofty::read_from_path(&path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Covered".into());
        tag.push_picture(Picture::unchecked(cover).pic_type(PictureType::CoverFront).mime_type(MimeType::Png).build());
        file.insert_tag(tag);
        file.save_to_path(&path, WriteOptions::default()).unwrap();
        path
    }

    pub fn silent_wav(ms: u32) -> Vec<u8> {
        let rate = 8000u32;
        let samples = rate * ms / 1000;
        let data = samples * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        out.resize(out.len() + data as usize, 0);
        out
    }
}
