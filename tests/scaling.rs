//! What grows with the queue and the play log, and how fast: each case is
//! timed at 1 000 and at 10 000 (the Liked Songs' size), and fails when ten
//! times the data costs far more than ten times the time, or when it
//! leaves its budget at 10 000. The benchmarks (`cargo bench`) measure
//! precisely; this catches a change that makes something quadratic.
//!
//! One test runs the cases one after another, so they do not time each
//! other. Each time is the least of many calls: noise only adds.

#[path = "../benches/common/mod.rs"]
mod common;

use std::hint::black_box;
use std::time::{Duration, Instant};

use agentamp::db;
use agentamp::ipc::Response;
use agentamp::model::Track;
use agentamp::queue::Queue;
use serde_json::Value;

use common::{liked, plays, scratch, tracks};

const SMALL: usize = 1_000;
const LARGE: usize = 10_000;

#[derive(Clone, Copy, Debug)]
enum Growth {
    /// The same time at any size, as an index or a fixed window gives.
    Constant,
    /// Time in step with the size: one walk, one scan.
    Linear,
}

impl Growth {
    /// How much longer ten times the data may take. Linear is 10, n log n
    /// about 13, quadratic 100.
    fn limit(self) -> f64 {
        match self {
            Growth::Constant => 3.0,
            Growth::Linear => 20.0,
        }
    }
}

/// The least time `f` takes on what `setup` makes, which is not timed.
fn least<S>(mut setup: impl FnMut() -> S, mut f: impl FnMut(&mut S)) -> Duration {
    let mut best = Duration::MAX;
    let mut spent = Duration::ZERO;
    let mut calls = 0;
    while calls < 5 || (spent < Duration::from_millis(200) && calls < 1_000) {
        let mut state = setup();
        let start = Instant::now();
        f(&mut state);
        let took = start.elapsed();
        drop(state);
        best = best.min(took);
        spent += took;
        calls += 1;
    }
    best
}

fn queue_of(n: usize) -> Queue {
    let mut queue = Queue::default();
    queue.append(tracks(n, 1));
    queue.advance();
    queue
}

/// A play log of `n` plays, written once.
fn log_of(n: usize) -> rusqlite::Connection {
    let path = scratch(&format!("scaling-db-{n}")).join("library.db");
    let mut conn = db::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    for play in plays(n, 10_000, 1) {
        db::insert(&tx, &play).unwrap();
    }
    tx.commit().unwrap();
    conn
}

fn count(conn: &rusqlite::Connection, sql: &str) -> usize {
    let mut statement = conn.prepare_cached(sql).unwrap();
    statement.query_map([], |_| Ok(())).unwrap().count()
}

struct Case {
    name: &'static str,
    growth: Growth,
    /// The most it may take at 10 000 in a debug build on a CI machine:
    /// four to five times what starship took (2026-10-04), more for the
    /// disk's flush.
    budget: Duration,
    time: fn(usize) -> Duration,
}

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

const CASES: &[Case] = &[
    Case {
        name: "the window's look at the queue",
        growth: Growth::Constant,
        budget: ms(30),
        time: |n| {
            let queue = queue_of(n);
            least(
                || (),
                |_| {
                    let data = serde_json::to_value(queue.stretch(0, Some(200))).unwrap();
                    let line = serde_json::to_string(&Response::ok(data)).unwrap();
                    let answer: Value = serde_json::from_str::<Response>(&line).unwrap().into_result().unwrap();
                    black_box(serde_json::from_value::<Vec<Track>>(answer["upcoming"].clone()).unwrap());
                },
            )
        },
    },
    Case {
        name: "200 tracks' details, in batches of 10",
        growth: Growth::Linear,
        budget: ms(500),
        time: |n| {
            let resolved: Vec<Track> = queue_of(n).upcoming.iter().take(200).cloned().collect();
            least(
                || queue_of(n),
                |queue| {
                    for batch in resolved.chunks(10) {
                        queue.update(batch);
                    }
                },
            )
        },
    },
    Case {
        name: "the downloads the queue waits for",
        growth: Growth::Linear,
        budget: ms(2),
        time: |n| {
            let queue = queue_of(n);
            least(|| (), |_| drop(black_box(queue.downloads())))
        },
    },
    Case {
        name: "play replacing the queue",
        growth: Growth::Linear,
        budget: ms(10),
        time: |n| {
            let fresh = tracks(n, 2);
            least(
                || (queue_of(n), fresh.clone()),
                |(queue, fresh)| {
                    queue.clear();
                    queue.append(std::mem::take(fresh));
                    black_box(queue.advance());
                },
            )
        },
    },
    Case {
        name: "keeping one play",
        growth: Growth::Constant,
        budget: ms(50),
        time: |n| {
            let conn = log_of(n);
            let next = plays(1, 10_000, 2).remove(0);
            least(|| (), |_| db::insert(&conn, &next).unwrap())
        },
    },
    Case {
        name: "the last 20 plays",
        growth: Growth::Constant,
        budget: ms(1),
        time: |n| {
            let conn = log_of(n);
            least(|| (), |_| {
                black_box(count(&conn, "SELECT title FROM plays ORDER BY started_at DESC LIMIT 20"));
            })
        },
    },
    Case {
        name: "one song's plays",
        growth: Growth::Constant,
        budget: ms(1),
        time: |n| {
            let conn = log_of(n);
            let sql = "SELECT started_at FROM plays WHERE uri = (SELECT uri FROM plays LIMIT 1)";
            least(|| (), |_| {
                black_box(count(&conn, sql));
            })
        },
    },
    Case {
        name: "the top artists of every play",
        growth: Growth::Linear,
        budget: ms(60),
        time: |n| {
            let conn = log_of(n);
            let sql = "SELECT artist, count(*) FROM plays GROUP BY artist ORDER BY 2 DESC LIMIT 20";
            least(|| (), |_| {
                black_box(count(&conn, sql));
            })
        },
    },
    Case {
        name: "a sync of the Liked Songs, all kept",
        growth: Growth::Linear,
        budget: ms(1_000),
        time: |n| {
            let songs = liked(n, 1);
            let mut conn = db::open(&scratch(&format!("scaling-liked-{n}")).join("library.db")).unwrap();
            db::replace_liked(&mut conn, &songs).unwrap();
            least(|| (), |_| {
                black_box(db::replace_liked(&mut conn, &songs).unwrap());
                black_box(db::albums_missing(&conn).unwrap());
            })
        },
    },
];

#[test]
fn nothing_grows_faster_than_it_should() {
    let mut failures = Vec::new();
    // Also on the CI run's page, where a passing test's output is not.
    let mut summary = format!(
        "### Scaling, {}\n\n| case | {SMALL} | {LARGE} | growth | allowed | budget at {LARGE} | used |\n|---|---|---|---|---|---|---|\n",
        std::env::consts::OS
    );
    println!("{:<40} {:>10} {:>10} {:>7} {:>7} {:>10} {:>5}", "case", SMALL, LARGE, "growth", "allowed", "budget", "used");
    for case in CASES {
        let small = (case.time)(SMALL);
        let large = (case.time)(LARGE);
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        let limit = case.growth.limit();
        let used = 100.0 * large.as_secs_f64() / case.budget.as_secs_f64();
        println!(
            "{:<40} {:>10} {:>10} {:>7} {:>7} {:>10} {:>4.0}%",
            case.name,
            format!("{small:.2?}"),
            format!("{large:.2?}"),
            format!("×{ratio:.1}"),
            format!("×{limit}"),
            format!("{:?}", case.budget),
            used
        );
        summary += &format!(
            "| {} | {small:.2?} | {large:.2?} | ×{ratio:.1} | ×{limit} | {:?} | {used:.0}% |\n",
            case.name, case.budget
        );
        if ratio > limit {
            failures.push(format!(
                "{}: {small:.2?} at {SMALL}, {large:.2?} at {LARGE}, ×{ratio:.1} where {:?} allows ×{limit}",
                case.name, case.growth
            ));
        }
        if large > case.budget {
            failures.push(format!("{}: {large:.2?} at {LARGE}, over its budget of {:?}", case.name, case.budget));
        }
    }
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(path).unwrap();
        writeln!(file, "{summary}").unwrap();
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
