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
    assert_eq!(home.json(&["now"])["volume"], 80, "the volume the player will start at");
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
    let (ok, _, err) = home.run(&["search", "plastic", "love"]);
    assert!(!ok);
    assert!(err.contains("agentamp login"), "{err}");
    let (ok, _, err) = home.run(&["search", "x", "--count", "11"]);
    assert!(!ok && err.contains("1..=10"), "{err}");
    let (ok, _, err) = home.run(&["browse", "https://open.spotify.com/artist/3WwGRA2o4Ux1RRMYaYDh7N"]);
    assert!(!ok && err.contains("agentamp login"), "{err}");
    let (ok, _, err) = home.run(&["browse", "top", "--count", "51"]);
    assert!(!ok && err.contains("1..=50"), "{err}");
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
case "$last" in *slow*) sleep 3;; esac
case "$last" in *missing*) echo "ERROR: [youtube] missing: Video unavailable" >&2; exit 1;; esac
case "$last" in *refused*) echo "ERROR: unable to download video data: HTTP Error 403: Forbidden" >&2; exit 1;; esac
case "$last" in *flaky*) [ -e "{log}.refused" ] || {{ touch "{log}.refused"; echo "ERROR: unable to download video data: HTTP Error 403: Forbidden" >&2; exit 1; }};; esac
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
    let now = || -> serde_json::Value {
        let (ok, out, err) = run(&["--json", "now"]);
        assert!(ok, "{err}");
        serde_json::from_str(&out).unwrap()
    };
    let tries = |url: &str| std::fs::read_to_string(home.0.join("yt-dlp.log")).unwrap_or_default().matches(url).count();
    let eventually = |what: &str, done: &dyn Fn() -> bool| {
        for _ in 0..200 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("never: {what}");
    };

    // Play answers with the search playing, before yt-dlp has the file.
    let (ok, out, err) = run(&["--json", "play", "yt:", "night", "tempo"]);
    assert!(ok, "{err}");
    let status: serde_json::Value = serde_json::from_str(&out).unwrap();
    let track = &status["status"]["track"];
    assert_eq!((&track["source"], &track["title"], &track["downloading"]), (&"youtube".into(), &"night tempo".into(), &true.into()));
    eventually("the file plays", &|| now()["track"]["title"] == "City Pop Mix");
    let track = &now()["track"];
    assert_eq!(track["link"], "https://www.youtube.com/watch?v=abc123");
    assert!(track.get("downloading").is_none(), "{track}");
    assert!(home.0.join("cache/youtube/abc123.m4a").exists());
    let calls = std::fs::read_to_string(home.0.join("yt-dlp.log")).unwrap();
    assert!(calls.contains("-f bestaudio[ext=m4a]"), "{calls}");
    assert!(calls.trim_end().ends_with("-- ytsearch1:night tempo"), "{calls}");

    // The same search again finds the file without asking yt-dlp.
    run(&["add", "yt:", "night", "tempo"]);
    eventually("the search is remembered", &|| {
        let (_, out, _) = run(&["--json", "queue"]);
        out.matches("abc123.m4a").count() == 2
    });
    assert_eq!(tries("ytsearch1:night tempo"), 1);
    run(&["clear"]);

    // Add answers at once too, and clearing the queue stops the download.
    let asked = std::time::Instant::now();
    let (ok, _, err) = run(&["add", "https://youtu.be/slow"]);
    assert!(ok, "{err}");
    assert!(asked.elapsed() < Duration::from_millis(1500), "add waited {:?} for a 3 s download", asked.elapsed());
    eventually("the slow download starts", &|| tries("youtu.be/slow") == 1);
    run(&["clear"]);
    assert_eq!(now()["queue_len"], 0);

    // A video that cannot be had leaves the queue, and the player says why.
    let (ok, _, err) = run(&["add", "https://youtu.be/missing"]);
    assert!(ok, "{err}");
    eventually("the missing video leaves", &|| now()["queue_len"] == 0 && tries("youtu.be/missing") == 1);
    assert!(now()["error"].as_str().unwrap().contains("Video unavailable"), "{}", now());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(tries("youtu.be/missing"), 1, "an unavailable video is not asked for again");

    // YouTube refuses a download now and then and lets it through a moment later.
    run(&["add", "https://youtu.be/flaky"]);
    eventually("the flaky video arrives", &|| {
        let (_, out, _) = run(&["--json", "queue"]);
        out.contains("abc123.m4a") && !out.contains("youtu.be/flaky\"")
    });
    assert_eq!(tries("youtu.be/flaky"), 2);
    run(&["clear"]);
    run(&["add", "https://youtu.be/refused"]);
    eventually("the refused video leaves", &|| now()["queue_len"] == 0 && tries("youtu.be/refused") == 2);
    assert!(now()["error"].as_str().unwrap().contains("HTTP Error 403"), "{}", now());
    assert_eq!(tries("youtu.be/refused"), 2, "asked again once, not forever");
}

#[test]
fn windows_can_listen_to_the_sound() {
    use std::io::{BufRead, BufReader, Read, Write};
    let home = Home::new("listen");
    wav(&home.0.join("music/Quiet.wav"), 60);
    home.ok(&["play", home.0.join("music").to_str().unwrap()]);

    let mut socket = std::os::unix::net::UnixStream::connect(home.0.join("run/agentamp.sock")).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    socket.write_all(b"{\"cmd\":\"listen\"}\n").unwrap();
    let mut reader = BufReader::new(socket);
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    assert!(answer.starts_with("{\"ok\":true"), "{answer}");
    // Silent audio taps nothing, so what comes is the chunk of none that
    // checks the window is still there: no rate yet, no samples.
    let mut head = [0u8; 8];
    reader.read_exact(&mut head).unwrap();
    assert_eq!(head, [0; 8]);
    // Other connections are answered meanwhile.
    assert_eq!(home.json(&["now"])["state"], "playing");
}
