//! The library's database: what has played, kept in SQLite so the CLI and
//! agents can ask it anything in SQL. The daemon is its only writer, on a
//! thread of its own so the player never waits for the disk.

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
"];

/// One listen to a track, written once it ends.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub track: Track,
    pub started: SystemTime,
    /// How long it was heard, pauses left out.
    pub ms_played: u32,
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
}
