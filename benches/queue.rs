//! The queue at the sizes it reaches: what each change and each look at it
//! costs with a playlist, the Liked Songs and ten times them in it.

mod common;

use std::hint::black_box;

use agentamp::ipc::Response;
use agentamp::model::Track;
use agentamp::queue::Queue;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde_json::Value;

use common::{SIZES, tracks};

fn queue_of(n: usize) -> Queue {
    let mut queue = Queue::default();
    queue.append(tracks(n, 1));
    queue.advance();
    queue
}

/// One look of the window at the queue, every half second while it is open:
/// the daemon's answer written as the socket carries it, then read back.
fn snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue/snapshot");
    for n in SIZES {
        let queue = queue_of(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &queue, |b, queue| {
            b.iter(|| {
                let data = serde_json::to_value(queue).unwrap();
                let line = serde_json::to_string(&Response::ok(data)).unwrap();
                let answer: Response = serde_json::from_str(black_box(&line)).unwrap();
                let queue: Value = answer.into_result().unwrap();
                let upcoming: Vec<Track> = serde_json::from_value(queue["upcoming"].clone()).unwrap();
                black_box(upcoming)
            })
        });
    }
}

/// The details read ahead after a play landing: 200 tracks, each found in
/// the queue.
fn details(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue/details");
    for n in SIZES {
        let queue = queue_of(n);
        let resolved: Vec<Track> = queue.upcoming.iter().take(200).cloned().collect();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched_ref(
                || queue_of(n),
                |queue| {
                    for track in &resolved {
                        queue.update(track);
                    }
                },
                BatchSize::LargeInput,
            )
        });
        drop(queue);
    }
}

/// What the engine asks after every message: which downloads the queue waits for.
fn downloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue/downloads");
    for n in SIZES {
        let queue = queue_of(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &queue, |b, queue| {
            b.iter(|| black_box(queue.downloads()))
        });
    }
}

/// `play` replacing the queue with `n` tracks.
fn replace(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue/replace");
    for n in SIZES {
        let fresh = tracks(n, 2);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched_ref(
                || (queue_of(n), fresh.clone()),
                |(queue, fresh)| {
                    queue.clear();
                    queue.append(std::mem::take(fresh));
                    black_box(queue.advance())
                },
                BatchSize::LargeInput,
            )
        });
    }
}

criterion_group!(benches, snapshot, details, downloads, replace);
criterion_main!(benches);
