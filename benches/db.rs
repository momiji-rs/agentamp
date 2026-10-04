//! The library's database as it grows: keeping a play, opening it, and the
//! questions the CLI and agents will ask, at 1k, 10k and 100k plays.

mod common;

use std::hint::black_box;
use std::path::PathBuf;

use agentamp::db;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use common::{SIZES, plays, scratch};

/// A database of `n` plays of a library of 10 000 songs, made once a run.
fn filled(n: usize) -> PathBuf {
    let path = scratch(&format!("db-{n}")).join("library.db");
    let mut conn = db::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    for play in plays(n, 10_000, 1) {
        db::insert(&tx, &play).unwrap();
    }
    tx.commit().unwrap();
    path
}

fn query(conn: &rusqlite::Connection, sql: &str) -> usize {
    let mut statement = conn.prepare(sql).unwrap();
    let rows = statement.query_map([], |_| Ok(())).unwrap();
    rows.count()
}

/// What the daemon does as each track ends: one play, in a transaction of its own.
fn insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("db/insert");
    let next = plays(1, 10_000, 2).remove(0);
    for n in SIZES {
        let conn = db::open(&filled(n)).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| db::insert(&conn, black_box(&next)).unwrap())
        });
    }
}

/// Opening it, as every `agentamp sql` will.
fn open(c: &mut Criterion) {
    let mut group = c.benchmark_group("db/open");
    for n in SIZES {
        let path = filled(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &path, |b, path| {
            b.iter(|| black_box(db::open(path).unwrap()))
        });
    }
}

/// Questions over every play, and ones an index answers.
fn questions(c: &mut Criterion) {
    let asked = [
        ("top_artists", "SELECT artist, count(*), sum(ms_played) FROM plays GROUP BY artist ORDER BY 2 DESC LIMIT 20"),
        ("by_month", "SELECT substr(started_at, 1, 7), count(*) FROM plays GROUP BY 1"),
        ("recent", "SELECT title, artist, started_at FROM plays ORDER BY started_at DESC LIMIT 20"),
        ("one_song", "SELECT started_at, ms_played FROM plays WHERE uri = (SELECT uri FROM plays LIMIT 1)"),
    ];
    for (name, sql) in asked {
        let mut group = c.benchmark_group(format!("db/{name}"));
        for n in SIZES {
            let conn = db::open(&filled(n)).unwrap();
            group.bench_with_input(BenchmarkId::from_parameter(n), &conn, |b, conn| {
                b.iter(|| black_box(query(conn, sql)))
            });
        }
    }
}

/// Many plays at once, as an import of Spotify's streaming history will write them.
fn import(c: &mut Criterion) {
    let mut group = c.benchmark_group("db/import");
    group.sample_size(10);
    for n in SIZES {
        let all = plays(n, 10_000, 3);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || db::open(&scratch(&format!("import-{n}")).join("library.db")).unwrap(),
                |mut conn| {
                    let tx = conn.transaction().unwrap();
                    for play in &all {
                        db::insert(&tx, play).unwrap();
                    }
                    tx.commit().unwrap();
                },
                BatchSize::PerIteration,
            )
        });
    }
}

criterion_group!(benches, insert, open, questions, import);
criterion_main!(benches);
