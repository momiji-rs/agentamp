//! The library's database: what has played and what the Spotify library
//! holds, kept in SQLite so the CLI and agents can ask it anything in SQL.
//! The daemon is its only writer: plays on a thread of its own so the
//! player never waits for the disk, the library when it is synced.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use log::warn;
use rusqlite::{Connection, params};

use crate::model::{Source, Track};

/// Each step brings a database from the version before it to the next;
/// `user_version` says how many have run.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE plays (
        id INTEGER PRIMARY KEY,
        started_at TEXT NOT NULL,
        ms_played INTEGER NOT NULL,
        uri TEXT NOT NULL,
        source TEXT NOT NULL,
        title TEXT NOT NULL,
        artist TEXT NOT NULL DEFAULT '',
        album TEXT NOT NULL DEFAULT '',
        duration_ms INTEGER NOT NULL DEFAULT 0,
        origin TEXT NOT NULL DEFAULT 'agentamp'
    );
    CREATE INDEX plays_by_uri ON plays (uri);
    CREATE INDEX plays_by_time ON plays (started_at);
", "
    CREATE TABLE liked (
        uri TEXT PRIMARY KEY,
        added_at TEXT NOT NULL,
        title TEXT NOT NULL DEFAULT '',
        artist TEXT NOT NULL DEFAULT '',
        album TEXT NOT NULL DEFAULT '',
        album_uri TEXT,
        duration_ms INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX liked_by_time ON liked (added_at);
    CREATE INDEX liked_by_album ON liked (album_uri);
    CREATE TABLE albums (
        uri TEXT PRIMARY KEY,
        title TEXT NOT NULL DEFAULT '',
        artist TEXT NOT NULL DEFAULT '',
        released TEXT,
        label TEXT,
        kind TEXT
    );
"];

/// One listen to a track, written once it ends.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub track: Track,
    pub started: SystemTime,
    /// How long it was heard, pauses left out.
    pub ms_played: u32,
}

/// A song in the Spotify Liked Songs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Liked {
    pub uri: String,
    /// When it was liked, as Spotify gives it: `2026-10-01T19:55:02Z`.
    pub added_at: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_uri: Option<String>,
    pub duration_ms: u32,
}

/// An album of a liked song, with what a liked song does not carry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Album {
    pub uri: String,
    pub title: String,
    pub artist: String,
    /// As precisely as Spotify knows it: `2014-09-26`, `2014-09` or `2014`.
    pub released: Option<String>,
    pub label: Option<String>,
    /// `ALBUM`, `SINGLE`, `EP` or `COMPILATION`.
    pub kind: Option<String>,
}

/// Opens the database to write, making it and bringing it up to date.
pub fn open(path: &Path) -> Result<Connection> {
    let dir = path.parent().context("the database has no directory")?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let mut db = Connection::open(path)?;
    // Readers go on while the daemon writes.
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    migrate(&mut db)?;
    Ok(db)
}

fn migrate(db: &mut Connection) -> Result<()> {
    let done: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (n, step) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let tx = db.transaction()?;
        tx.execute_batch(step).with_context(|| format!("cannot bring the database to version {}", n + 1))?;
        tx.pragma_update(None, "user_version", n as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

pub fn insert(db: &Connection, play: &Play) -> Result<()> {
    let track = &play.track;
    let source = match track.source {
        Source::Spotify => "spotify",
        Source::Youtube => "youtube",
        Source::Local => "local",
    };
    // A YouTube track's page names it; its `uri` is the downloaded file.
    let uri = track.link.as_deref().unwrap_or(&track.uri);
    let started = play.started.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    db.execute(
        "INSERT INTO plays (started_at, ms_played, uri, source, title, artist, album, duration_ms)
         VALUES (strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch'), ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![started, play.ms_played, uri, source, track.title, track.artist, track.album, track.duration_ms],
    )?;
    Ok(())
}

/// Makes the `liked` table hold `songs`, in one transaction. Gives how
/// many of them are new to it and how many it held that are gone.
pub fn replace_liked(db: &mut Connection, songs: &[Liked]) -> Result<(usize, usize)> {
    let tx = db.transaction()?;
    let held: HashSet<String> = tx.prepare("SELECT uri FROM liked")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let now: HashSet<&str> = songs.iter().map(|s| s.uri.as_str()).collect();
    let new = now.iter().filter(|uri| !held.contains(**uri)).count();
    let gone = held.iter().filter(|uri| !now.contains(uri.as_str())).count();
    tx.execute("DELETE FROM liked", [])?;
    {
        // A song liked while the pages were read can show on two of them.
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO liked (uri, added_at, title, artist, album, album_uri, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for s in songs {
            insert.execute(params![s.uri, s.added_at, s.title, s.artist, s.album, s.album_uri, s.duration_ms])?;
        }
    }
    tx.commit()?;
    Ok((new, gone))
}

/// The albums of liked songs not in the `albums` table yet. A release
/// date does not change, so a kept album is never read again.
pub fn albums_missing(db: &Connection) -> Result<Vec<String>> {
    let mut statement = db.prepare(
        "SELECT DISTINCT album_uri FROM liked WHERE album_uri IS NOT NULL
         AND album_uri NOT IN (SELECT uri FROM albums)",
    )?;
    Ok(statement.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
}

pub fn insert_albums(db: &mut Connection, albums: &[Album]) -> Result<()> {
    let tx = db.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO albums (uri, title, artist, released, label, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for a in albums {
            insert.execute(params![a.uri, a.title, a.artist, a.released, a.label, a.kind])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Where the engine sends what has played.
#[derive(Clone)]
pub struct Log(mpsc::Sender<Play>);

impl Log {
    /// Writes plays to `path` on a thread of its own, which opens the
    /// database on the first play.
    pub fn start(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Play>();
        std::thread::spawn(move || {
            let mut db = None;
            for play in rx {
                if db.is_none() {
                    match open(&path) {
                        Ok(opened) => db = Some(opened),
                        Err(e) => {
                            warn!("cannot open the library's database {}: {e:#}", path.display());
                            continue;
                        }
                    }
                }
                if let Some(db) = &db
                    && let Err(e) = insert(db, &play)
                {
                    warn!("cannot keep a play of {}: {e:#}", play.track.label());
                }
            }
        });
        Self(tx)
    }

    /// The plays as they are sent, for tests.
    #[cfg(test)]
    pub fn sink() -> (Self, mpsc::Receiver<Play>) {
        let (tx, rx) = mpsc::channel();
        (Self(tx), rx)
    }

    pub fn record(&self, play: Play) {
        let _ = self.0.send(play);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_play_is_kept_with_its_time_in_utc() {
        let path = crate::testutil::scratch("db-plays").join("data/library.db");
        let mut track = Track::placeholder(Source::Youtube, "/cache/youtube/x.m4a");
        (track.title, track.artist, track.duration_ms) = ("Plastic Love".into(), "Mariya Takeuchi".into(), 294_493);
        track.link = Some("https://www.youtube.com/watch?v=3bNITQR4Uso".into());
        let started = UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000);
        insert(&open(&path).unwrap(), &Play { track, started, ms_played: 61_000 }).unwrap();

        // Opened again, the migrations do not run twice.
        let db = open(&path).unwrap();
        let row = db
            .query_row("SELECT started_at, ms_played, uri, source, title, album, origin FROM plays", [], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
            })
            .unwrap();
        assert_eq!(row, (
            "2026-09-21T14:13:20Z".into(),
            61_000,
            "https://www.youtube.com/watch?v=3bNITQR4Uso".into(),
            "youtube".into(),
            "Plastic Love".into(),
            String::new(),
            "agentamp".into()
        ));
        let mode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "plays are personal");
    }

    #[test]
    fn a_database_of_plays_gains_the_library_and_keeps_its_plays() {
        let path = crate::testutil::scratch("db-upgrade").join("library.db");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(MIGRATIONS[0]).unwrap();
            db.pragma_update(None, "user_version", 1).unwrap();
            let track = Track::placeholder(Source::Local, "/music/a.flac");
            insert(&db, &Play { track, started: UNIX_EPOCH, ms_played: 1_000 }).unwrap();
        }
        let db = open(&path).unwrap();
        let plays: u32 = db.query_row("SELECT count(*) FROM plays", [], |r| r.get(0)).unwrap();
        let liked: u32 = db.query_row("SELECT count(*) FROM liked", [], |r| r.get(0)).unwrap();
        assert_eq!((plays, liked), (1, 0));
    }

    fn song(uri: &str, album: Option<&str>) -> Liked {
        Liked { uri: uri.into(), added_at: "2026-10-01T19:55:02Z".into(), album_uri: album.map(str::to_string), ..Liked::default() }
    }

    #[test]
    fn a_sync_says_what_is_new_and_gone_and_keeps_each_album_once() {
        let mut db = open(&crate::testutil::scratch("db-liked").join("library.db")).unwrap();
        let first = [song("spotify:track:a", Some("spotify:album:x")), song("spotify:track:b", Some("spotify:album:x"))];
        assert_eq!(replace_liked(&mut db, &first).unwrap(), (2, 0));
        assert_eq!(albums_missing(&db).unwrap(), ["spotify:album:x"]);
        let album = Album { uri: "spotify:album:x".into(), released: Some("2014-09-26".into()), ..Album::default() };
        insert_albums(&mut db, &[album]).unwrap();
        assert!(albums_missing(&db).unwrap().is_empty(), "a kept album is not read again");

        // b was unliked and c liked, which showed on two pages.
        let second = [song("spotify:track:a", Some("spotify:album:x")), song("spotify:track:c", None), song("spotify:track:c", None)];
        assert_eq!(replace_liked(&mut db, &second).unwrap(), (1, 1));
        let uris: Vec<String> =
            db.prepare("SELECT uri FROM liked ORDER BY uri").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(uris, ["spotify:track:a", "spotify:track:c"]);
        assert!(albums_missing(&db).unwrap().is_empty(), "a song without an album asks for none");
    }
}
