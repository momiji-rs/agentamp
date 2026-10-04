//! `agentamp import`: the years of plays Spotify keeps, from the Extended
//! streaming history it sends on request (Account, Privacy, "Download your
//! data"), into the library's `plays`. Only the songs: podcasts, audiobooks
//! and videos are left out, and so are the IP address, country and device
//! each play came from.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::db::{self, Streamed};

/// One entry of a `Streaming_History_Audio_*.json` file, of the fields kept.
#[derive(Deserialize)]
struct Entry {
    /// When it stopped playing, UTC.
    ts: String,
    ms_played: u32,
    spotify_track_uri: Option<String>,
    master_metadata_track_name: Option<String>,
    master_metadata_album_artist_name: Option<String>,
    master_metadata_album_album_name: Option<String>,
}

/// What an import read and kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Imported {
    pub files: usize,
    /// Songs heard, in the files.
    pub songs: usize,
    /// Of them, plays not kept before.
    pub added: usize,
    /// Podcast episodes and audiobook chapters, left out.
    pub others: usize,
    /// The first and last song's end, UTC.
    pub from: Option<String>,
    pub to: Option<String>,
}

pub fn import(database: &Path, targets: &[PathBuf]) -> Result<Imported> {
    let (mut files, mut account) = (Vec::new(), false);
    for target in targets {
        gather(target, &mut files, &mut account)?;
    }
    files.sort();
    if files.is_empty() && account {
        bail!(
            "this is the account data's last year, which does not say which song each play was; \
             ask Spotify for the Extended streaming history instead"
        );
    }
    if files.is_empty() {
        bail!("there is no Streaming_History_Audio_*.json in {}; unzip what Spotify sent and give its folder", shown(targets));
    }
    let mut imported = Imported { files: files.len(), ..Imported::default() };
    let mut heard = Vec::new();
    for file in &files {
        let text = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
        let entries: Vec<Entry> =
            serde_json::from_slice(&text).with_context(|| format!("{} is not Spotify's streaming history", file.display()))?;
        for entry in entries {
            match streamed(entry) {
                Some(song) => heard.push(song),
                None => imported.others += 1,
            }
        }
    }
    imported.songs = heard.len();
    imported.from = heard.iter().map(|h| &h.ended_at).min().cloned();
    imported.to = heard.iter().map(|h| &h.ended_at).max().cloned();
    imported.added = db::import(&mut db::open(database)?, &heard)?;
    Ok(imported)
}

/// The history's files at `target`: it, or those in it and its folders.
/// `account` says the account data's history was there instead.
fn gather(target: &Path, files: &mut Vec<PathBuf>, account: &mut bool) -> Result<()> {
    if is_account_data(target) {
        *account = true;
    } else if target.is_dir() {
        let entries = std::fs::read_dir(target).with_context(|| format!("cannot read {}", target.display()))?;
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() || is_history(&path) || is_account_data(&path) {
                gather(&path, files, account)?;
            }
        }
    } else if !target.exists() {
        bail!("no such file or folder: {}", target.display());
    } else if target.extension().is_some_and(|e| e == "json") {
        files.push(target.to_path_buf());
    } else {
        bail!("{} is not Spotify's streaming history; unzip what Spotify sent and give its folder", file_name(target));
    }
    Ok(())
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
}

fn is_history(path: &Path) -> bool {
    let name = file_name(path);
    name.starts_with("Streaming_History_Audio_") && name.ends_with(".json")
}

/// `StreamingHistory_music_0.json`, or `StreamingHistory0.json` before 2023.
fn is_account_data(path: &Path) -> bool {
    let name = file_name(path);
    name.starts_with("StreamingHistory") && name.ends_with(".json")
}

fn shown(targets: &[PathBuf]) -> String {
    targets.iter().map(|t| t.display().to_string()).collect::<Vec<_>>().join(", ")
}

fn streamed(entry: Entry) -> Option<Streamed> {
    let uri = entry.spotify_track_uri.filter(|u| u.starts_with("spotify:track:"))?;
    Some(Streamed {
        ended_at: entry.ts,
        ms_played: entry.ms_played,
        uri,
        title: entry.master_metadata_track_name.unwrap_or_default(),
        artist: entry.master_metadata_album_artist_name.unwrap_or_default(),
        album: entry.master_metadata_album_album_name.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Source, Track};
    use std::time::{Duration, UNIX_EPOCH};

    /// Two songs, one heard through AgentAmp too, and a podcast, in the
    /// export's own shape.
    const FIRST: &str = r#"[
      {"ts": "2015-08-10T09:00:00Z", "platform": "Android OS 5.0", "ms_played": 294493, "conn_country": "TW",
       "ip_addr": "203.0.113.7", "master_metadata_track_name": "Plastic Love",
       "master_metadata_album_artist_name": "Mariya Takeuchi", "master_metadata_album_album_name": "Variety",
       "spotify_track_uri": "spotify:track:7rU6Iebxzlvqy5t857bKFq", "episode_name": null, "episode_show_name": null,
       "spotify_episode_uri": null, "audiobook_title": null, "audiobook_uri": null, "audiobook_chapter_uri": null,
       "audiobook_chapter_title": null, "reason_start": "clickrow", "reason_end": "trackdone", "shuffle": false,
       "skipped": false, "offline": false, "offline_timestamp": 0, "incognito_mode": false},
      {"ts": "2026-09-21T14:18:20Z", "ms_played": 300000, "master_metadata_track_name": "Boston",
       "master_metadata_album_artist_name": "STELLA LEFTY", "master_metadata_album_album_name": "Long Way Home",
       "spotify_track_uri": "spotify:track:1"},
      {"ts": "2020-01-01T00:00:00Z", "ms_played": 1800000, "master_metadata_track_name": null,
       "spotify_track_uri": null, "episode_name": "Episode 1", "spotify_episode_uri": "spotify:episode:2"}
    ]"#;

    #[test]
    fn the_streaming_history_joins_the_plays_once() {
        let dir = crate::testutil::scratch("history");
        let export = dir.join("my_spotify_data/Spotify Extended Streaming History");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("Streaming_History_Audio_2015-2026_0.json"), FIRST).unwrap();
        std::fs::write(export.join("Streaming_History_Video_2020.json"), "[]").unwrap();
        std::fs::write(export.join("ReadMeFirst_ExtendedStreamingHistory.pdf"), "").unwrap();
        let database = dir.join("data/library.db");
        // Boston, heard in AgentAmp; Spotify's history has it too, 5 s apart.
        let mut boston = Track::placeholder(Source::Spotify, "spotify:track:1");
        boston.title = "Boston".into();
        let started = UNIX_EPOCH + Duration::from_secs(1_790_000_000 - 5);
        db::insert(&db::open(&database).unwrap(), &db::Play { track: boston, started, ms_played: 300_000 }).unwrap();

        let imported = import(&database, &[dir.join("my_spotify_data")]).unwrap();
        assert_eq!(imported, Imported {
            files: 1,
            songs: 2,
            added: 1,
            others: 1,
            from: Some("2015-08-10T09:00:00Z".into()),
            to: Some("2026-09-21T14:18:20Z".into()),
        });
        let again = import(&database, &[export.join("Streaming_History_Audio_2015-2026_0.json")]).unwrap();
        assert_eq!((again.songs, again.added), (2, 0), "importing twice adds nothing");

        let db = db::open(&database).unwrap();
        let row = db
            .query_row("SELECT started_at, ms_played, uri, source, title, artist, album FROM plays WHERE origin = 'spotify'", [], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
            })
            .unwrap();
        assert_eq!(row, (
            "2015-08-10T08:55:06Z".into(),
            294_493,
            "spotify:track:7rU6Iebxzlvqy5t857bKFq".into(),
            "spotify".into(),
            "Plastic Love".into(),
            "Mariya Takeuchi".into(),
            "Variety".into()
        ), "a play starts what it was heard before it stopped");
        let all: u32 = db.query_row("SELECT count(*) FROM plays", [], |r| r.get(0)).unwrap();
        assert_eq!(all, 2, "Boston is one play, AgentAmp's");
        let kept: String = db.query_row("SELECT group_concat(sql) FROM sqlite_schema", [], |r| r.get(0)).unwrap();
        assert!(!kept.contains("ip_addr") && !kept.contains("conn_country"));
    }

    #[test]
    fn what_is_not_the_extended_history_is_refused_plainly() {
        let dir = crate::testutil::scratch("history-refused");
        let database = dir.join("library.db");
        let account = dir.join("StreamingHistory_music_0.json");
        std::fs::write(&account, r#"[{"endTime": "2026-01-01 12:34", "artistName": "A", "trackName": "B", "msPlayed": 1}]"#).unwrap();
        let e = import(&database, &[account]).unwrap_err();
        assert!(format!("{e:#}").contains("Extended streaming history"), "{e:#}");
        let e = import(&database, &[dir.join("empty")]).unwrap_err();
        assert!(format!("{e:#}").contains("no such file"), "{e:#}");
        std::fs::create_dir_all(dir.join("unzipped")).unwrap();
        let e = import(&database, &[dir.join("unzipped")]).unwrap_err();
        assert!(format!("{e:#}").contains("Streaming_History_Audio"), "{e:#}");
        let broken = dir.join("Streaming_History_Audio_2020.json");
        std::fs::write(&broken, "{\"not\": \"a list\"}").unwrap();
        assert!(import(&database, &[broken]).is_err());
        assert!(!database.exists(), "a refused import makes no database");
    }
}
