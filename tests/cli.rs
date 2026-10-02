//! The CLI against a real daemon with silent audio.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_agentamp"))
            .args(args)
            .env("AGENTAMP_HOME", &self.0)
            .env("AGENTAMP_AUDIO", "null")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.run(args);
        assert!(ok, "agentamp {args:?} failed: {err}");
        out
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&self.ok(&all)).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        self.run(&["quit"]);
    }
}

fn wav(path: &Path, seconds: u32) {
    let rate = 8000u32;
    let data = rate * seconds * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    for v in [2u16, 16] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.resize(out.len() + data as usize, 0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, out).unwrap();
}

#[test]
fn nothing_runs_until_asked() {
    let home = Home::new("nothing_runs");
    assert_eq!(home.ok(&["now"]), "■ Nothing playing");
    assert!(!home.0.join("run/agentamp.sock").exists(), "`now` must not start the daemon");
}

#[test]
fn plays_a_folder_and_controls_it() {
    let home = Home::new("plays_a_folder");
    let music = home.0.join("music");
    wav(&music.join("1 First.wav"), 60);
    wav(&music.join("2 Second.wav"), 60);
    wav(&music.join("sub/3 Third.wav"), 60);

    let line = home.ok(&["play", music.to_str().unwrap()]);
    assert!(line.starts_with("▶ 1 First"), "{line}");
    assert!(line.ends_with("/ 1:00"), "{line}");

    let queue = home.ok(&["queue"]);
    assert_eq!(queue, "▶ 1 First\n  1. 2 Second  1:00\n  2. 3 Third  1:00");

    assert!(home.ok(&["pause"]).starts_with("⏸ 1 First"));
    assert!(home.ok(&["toggle"]).starts_with("▶ 1 First"));
    assert!(home.ok(&["next"]).starts_with("▶ 2 Second"));

    let status = home.json(&["seek", "0:30"]);
    let position = status["position_ms"].as_u64().unwrap();
    assert!((30_000..32_000).contains(&position), "{position}");
    assert_eq!(home.json(&["volume", "35"])["volume"], 35);

    assert_eq!(home.ok(&["add", "--next", music.join("1 First.wav").to_str().unwrap()]), "Added 1 track");
    assert!(home.ok(&["next"]).starts_with("▶ 1 First"));

    let status = home.json(&["now"]);
    assert_eq!(status["state"], "playing");
    assert_eq!(status["track"]["source"], "local");
    assert_eq!(status["queue_len"], 1);

    home.ok(&["quit"]);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!home.0.join("run/agentamp.sock").exists(), "quit removes the socket");
}

#[test]
fn bad_targets_are_reported() {
    let home = Home::new("bad_targets");
    let (ok, _, err) = home.run(&["play", "/no/such/song.mp3"]);
    assert!(!ok);
    assert!(err.contains("no such file"), "{err}");
    let (ok, _, err) = home.run(&["add"]);
    assert!(!ok);
    assert!(err.contains("nothing to play"), "{err}");
}

#[test]
fn spotify_asks_for_a_sign_in_first() {
    let home = Home::new("spotify_sign_in");
    let (ok, _, err) = home.run(&["play", "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC"]);
    assert!(!ok);
    assert!(err.contains("agentamp login"), "{err}");
    let (ok, out, _) = home.run(&["logout"]);
    assert!(ok);
    assert_eq!(out.trim(), "Not signed in.");
}

/// A stand-in for yt-dlp that "downloads" a silent WAV and prints what the
/// real one prints, so the test needs no network.
fn fake_yt_dlp(home: &Home) -> PathBuf {
    let audio = home.0.join("fixture.wav");
    wav(&audio, 60);
    let script = home.0.join("yt-dlp");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
echo "$@" >> "{log}"
for last; do :; done
case "$last" in *missing*) echo "ERROR: [youtube] missing: Video unavailable" >&2; exit 1;; esac
while [ "$1" != "-o" ]; do shift; done
out=$(echo "$2" | sed 's/%(id)s/abc123/; s/%(ext)s/m4a/')
cp "{audio}" "$out"
printf '{{"id": "abc123", "title": "City Pop Mix", "uploader": "Night Tempo", "duration": 60, "filepath": "%s", "webpage_url": "https://www.youtube.com/watch?v=abc123"}}\n' "$out"
"#,
            log = home.0.join("yt-dlp.log").display(),
            audio = audio.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[test]
fn youtube_searches_download_into_the_cache() {
    let home = Home::new("youtube");
    let script = fake_yt_dlp(&home);
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_agentamp"))
            .args(args)
            .env("AGENTAMP_HOME", &home.0)
            .env("AGENTAMP_AUDIO", "null")
            .env("AGENTAMP_YTDLP", &script)
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (ok, out, err) = run(&["--json", "play", "yt:", "night", "tempo"]);
    assert!(ok, "{err}");
    let status: serde_json::Value = serde_json::from_str(&out).unwrap();
    let track = &status["status"]["track"];
    assert_eq!(track["source"], "youtube");
    assert_eq!(track["title"], "City Pop Mix");
    assert_eq!(track["link"], "https://www.youtube.com/watch?v=abc123");
    assert!(home.0.join("cache/youtube/abc123.m4a").exists());

    let calls = std::fs::read_to_string(home.0.join("yt-dlp.log")).unwrap();
    assert!(calls.contains("-f bestaudio[ext=m4a]"), "{calls}");
    assert!(calls.trim_end().ends_with("-- ytsearch1:night tempo"), "{calls}");

    let (ok, _, err) = run(&["add", "https://youtu.be/missing"]);
    assert!(!ok);
    assert!(err.contains("Video unavailable"), "{err}");
}
