//! `agentamp sql` and the agents' questions: SQL over the library's
//! database, read only, answered as columns and rows. The schema says what
//! each table and column holds, so a question can be written without
//! reading the code.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::db;
use crate::model::plain_integers;

/// A table's name, what it holds, and each column's name and what it holds.
type About = (&'static str, &'static str, &'static [(&'static str, &'static str)]);

/// What each table and column holds, beyond its name and type.
const ABOUT: &[About] = &[
    ("plays", "Each track heard, written when the next takes its place or the player stops.", &[
        ("id", "The row's own number."),
        ("started_at", "When it started, UTC: 2026-10-01T19:55:02Z."),
        ("ms_played", "How long it was heard, pauses left out."),
        ("uri", "Its Spotify URI, YouTube link or file path."),
        ("source", "spotify, youtube or local."),
        ("title", ""),
        ("artist", "Its artists, comma-separated."),
        ("album", ""),
        ("duration_ms", "Its length; 0 when unknown."),
        ("origin", "What recorded the play: agentamp."),
    ]),
    ("liked", "The Spotify Liked Songs as of the last `agentamp sync`.", &[
        ("uri", "Its spotify:track: URI."),
        ("added_at", "When it was liked, UTC: 2026-10-01T19:55:02Z."),
        ("title", ""),
        ("artist", "Its artists, comma-separated."),
        ("album", ""),
        ("album_uri", "Its album's spotify:album: URI, the uri of a row of albums."),
        ("duration_ms", "Its length."),
    ]),
    ("albums", "The albums of the liked songs, as Spotify's catalogue describes them.", &[
        ("uri", "Its spotify:album: URI."),
        ("title", ""),
        ("artist", "Its artists, comma-separated."),
        ("released", "Its release date as precisely as Spotify knows it: 2014-09-26, 2014-09 or 2014. NULL when unknown."),
        ("label", "Its record label."),
        ("kind", "ALBUM, SINGLE, EP or COMPILATION."),
    ]),
];

/// The answer to a question: its columns, and its rows in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    pub columns: Vec<String>,
    /// Each row's values in the columns' order: a number, a string or null.
    pub rows: Vec<Vec<Value>>,
    /// More rows followed than were asked for.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Table {
    pub name: String,
    pub about: String,
    /// How many rows it holds now.
    pub rows: u64,
    pub columns: Vec<Column>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Column {
    pub name: String,
    /// SQLite's type: TEXT or INTEGER.
    #[serde(rename = "type")]
    pub kind: String,
    pub about: String,
}

/// Answers `sql` from the database at `path`: at most `most` rows, and
/// given up after `time`.
pub fn ask(path: &Path, sql: &str, most: Option<usize>, time: Option<Duration>) -> Result<Answer> {
    if sql.trim().trim_end_matches(';').trim().is_empty() {
        bail!("say what to ask, as one SELECT");
    }
    let db = db::read_only(path)?;
    let (done, waiting) = mpsc::channel::<()>();
    if let Some(time) = time {
        let handle = db.get_interrupt_handle();
        std::thread::spawn(move || {
            if waiting.recv_timeout(time) == Err(mpsc::RecvTimeoutError::Timeout) {
                handle.interrupt();
            }
        });
    }
    let answer = answer(&db, sql, most);
    drop(done);
    answer.map_err(|e| match e {
        rusqlite::Error::InvalidQuery => anyhow!("only questions are answered: the player and `agentamp sync` keep the library"),
        rusqlite::Error::MultipleStatement => anyhow!("ask one statement at a time"),
        e if e.sqlite_error_code() == Some(ErrorCode::OperationInterrupted) => {
            anyhow!("the question took longer than {}s; ask a narrower one", time.unwrap_or_default().as_secs())
        }
        e => e.into(),
    })
}

fn answer(db: &Connection, sql: &str, most: Option<usize>) -> rusqlite::Result<Answer> {
    let mut statement = db.prepare(sql)?;
    if !statement.readonly() {
        // The read-only connection would refuse it too, in less plain words.
        return Err(rusqlite::Error::InvalidQuery);
    }
    let columns: Vec<String> = statement.column_names().iter().map(|c| c.to_string()).collect();
    let mut answer = Answer { columns, ..Answer::default() };
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if most.is_some_and(|most| answer.rows.len() == most) {
            answer.truncated = true;
            break;
        }
        answer.rows.push((0..answer.columns.len()).map(|i| row.get_ref(i).map(value)).collect::<rusqlite::Result<_>>()?);
    }
    Ok(answer)
}

fn value(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => n.into(),
        ValueRef::Real(x) => serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
        ValueRef::Blob(b) => format!("{} bytes", b.len()).into(),
    }
}

/// The tables the database at `path` holds, or would once it is made.
pub fn schema(path: &Path) -> Result<Vec<Table>> {
    let db = if path.exists() { db::read_only(path)? } else { db::empty()? };
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY rowid")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut tables = Vec::new();
    for name in names {
        let about = ABOUT.iter().find(|(table, ..)| *table == name);
        let columns = db
            .prepare("SELECT name, type FROM pragma_table_info(?1) ORDER BY cid")?
            .query_map([&name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|column| {
                let (column, kind) = column?;
                let said = about.and_then(|(.., columns)| columns.iter().find(|(c, _)| *c == column));
                Ok(Column { about: said.map_or("", |(_, a)| a).into(), name: column, kind })
            })
            .collect::<rusqlite::Result<_>>()?;
        let rows = db.query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |r| r.get::<_, i64>(0))? as u64;
        tables.push(Table { about: about.map_or("", |(_, a, _)| a).into(), name, rows, columns });
    }
    Ok(tables)
}

/// An answer as a terminal shows it: aligned columns, numbers to the right.
pub fn text(answer: &Answer) -> String {
    let cell = |v: &Value| match v {
        Value::Null => String::new(),
        Value::String(s) => s.replace(['\n', '\t'], " "),
        v => v.to_string(),
    };
    let cells: Vec<Vec<String>> = answer.rows.iter().map(|row| row.iter().map(cell).collect()).collect();
    let widths: Vec<usize> = (0..answer.columns.len())
        .map(|i| cells.iter().map(|row| row[i].width()).chain([answer.columns[i].width()]).max().unwrap_or(0))
        .collect();
    let numeric: Vec<bool> =
        (0..answer.columns.len()).map(|i| answer.rows.iter().all(|row| row[i].is_number() || row[i].is_null())).collect();
    let line = |row: &[String]| {
        let padded = row.iter().enumerate().map(|(i, c)| {
            let pad = " ".repeat(widths[i] - c.width());
            if numeric[i] { format!("{pad}{c}") } else { format!("{c}{pad}") }
        });
        padded.collect::<Vec<_>>().join("  ").trim_end().to_string()
    };
    let mut lines = vec![line(&answer.columns), widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  ")];
    lines.extend(cells.iter().map(|row| line(row)));
    let n = answer.rows.len();
    lines.push(format!("{n} row{}", if n == 1 { "" } else { "s" }));
    lines.join("\n")
}

/// The schema as a terminal shows it.
pub fn schema_text(tables: &[Table]) -> String {
    let mut lines = Vec::new();
    for table in tables {
        lines.push(format!("{} ({} rows): {}", table.name, table.rows, table.about));
        let width = table.columns.iter().map(|c| c.name.len()).max().unwrap_or(0);
        for column in &table.columns {
            let line = format!("  {:width$}  {:7}  {}", column.name, column.kind, column.about);
            lines.push(line.trim_end().to_string());
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(name: &str) -> std::path::PathBuf {
        let path = crate::testutil::scratch(name).join("library.db");
        let mut db = db::open(&path).unwrap();
        let songs = [("spotify:track:a", "竹内まりや", "2014-09-26"), ("spotify:track:b", "Tatsuro Yamashita", "1982-01-21")];
        let liked: Vec<db::Liked> = songs
            .iter()
            .map(|(uri, artist, _)| db::Liked {
                uri: (*uri).into(),
                added_at: "2026-10-01T19:55:02Z".into(),
                artist: (*artist).into(),
                album_uri: Some(format!("spotify:album:{uri}")),
                duration_ms: 200_000,
                ..db::Liked::default()
            })
            .collect();
        db::replace_liked(&mut db, &liked).unwrap();
        let albums: Vec<db::Album> = songs
            .iter()
            .map(|(uri, _, released)| db::Album { uri: format!("spotify:album:{uri}"), released: Some((*released).into()), ..db::Album::default() })
            .collect();
        db::insert_albums(&mut db, &albums).unwrap();
        path
    }

    #[test]
    fn a_question_is_answered_in_columns_and_rows() {
        let path = library("sql-answer");
        let sql = "SELECT l.artist, substr(a.released, 1, 4) AS year, l.duration_ms FROM liked l
                   JOIN albums a ON a.uri = l.album_uri ORDER BY year;";
        let answer = ask(&path, sql, None, None).unwrap();
        assert_eq!(answer.columns, ["artist", "year", "duration_ms"]);
        assert_eq!(answer.rows, [
            vec![Value::from("Tatsuro Yamashita"), "1982".into(), 200_000.into()],
            vec!["竹内まりや".into(), "2014".into(), 200_000.into()],
        ]);
        assert_eq!(text(&answer), [
            "artist             year  duration_ms",
            "-----------------  ----  -----------",
            "Tatsuro Yamashita  1982       200000",
            "竹内まりや         2014       200000",
            "2 rows",
        ].join("\n"), "wide characters take two columns");

        let first = ask(&path, "SELECT uri FROM liked ORDER BY uri", Some(1), None).unwrap();
        assert_eq!((first.rows.len(), first.truncated), (1, true));
        let all = ask(&path, "SELECT uri FROM liked", Some(2), None).unwrap();
        assert!(!all.truncated, "exactly as many as asked for is all of them");
        let none = ask(&path, "SELECT uri FROM liked WHERE 0", None, None).unwrap();
        assert_eq!((none.columns.len(), none.rows.len()), (1, 0));
    }

    #[test]
    fn a_question_cannot_change_the_library() {
        let path = library("sql-read-only");
        for sql in [
            "DELETE FROM liked",
            "UPDATE albums SET released = NULL",
            "DROP TABLE plays",
            "CREATE TABLE notes (x)",
            "PRAGMA user_version = 0",
            "SELECT 1; DELETE FROM liked",
            "WITH gone AS (SELECT 1) DELETE FROM liked",
        ] {
            assert!(ask(&path, sql, None, None).is_err(), "{sql} was let through");
        }
        let attach = format!("ATTACH '{}' AS other", path.with_file_name("other.db").display());
        assert!(ask(&path, &attach, None, None).is_err(), "no other database is opened");
        assert!(!path.with_file_name("other.db").exists());
        assert!(ask(&path, "  ;", None, None).is_err());
        let left = ask(&path, "SELECT count(*) FROM liked", None, None).unwrap();
        assert_eq!(left.rows, [vec![Value::from(2)]]);
    }

    #[test]
    fn a_long_question_is_given_up() {
        let path = library("sql-slow");
        let forever = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n";
        let started = std::time::Instant::now();
        let e = ask(&path, forever, None, Some(Duration::from_millis(100))).unwrap_err();
        assert!(e.to_string().contains("took longer"), "{e:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn every_column_says_what_it_holds() {
        let tables = schema(&crate::testutil::scratch("sql-schema").join("absent.db")).unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["plays", "liked", "albums"], "before there is a database, the tables it will have");
        for (table, _, columns) in ABOUT {
            let held = tables.iter().find(|t| t.name == *table).unwrap();
            let names: Vec<&str> = held.columns.iter().map(|c| c.name.as_str()).collect();
            let said: Vec<&str> = columns.iter().map(|(c, _)| *c).collect();
            assert_eq!(names, said, "{table}: the schema and what it says of it differ");
        }
        let library = schema(&library("sql-schema-rows")).unwrap();
        assert_eq!(library.iter().map(|t| t.rows).collect::<Vec<_>>(), [0, 2, 2]);
        assert!(schema_text(&library).starts_with("plays (0 rows): Each track heard"));
    }
}
