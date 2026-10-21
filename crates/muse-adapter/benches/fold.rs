//! Fold cost (docs/51 P2: hold the line).
//!
//! Every Muse event a session view receives goes through `MuseFold::apply` on
//! the UI thread, so its cost per event — and whether that cost grows with
//! the session — is a frame-time budget. `stream.fold_apply_p99_ms` gates it
//! end to end in the app; these benches isolate the fold so a change to it
//! shows up in `cargo bench -p muse-adapter` with criterion's statistics.
//!
//! Inputs are the checked-in synthetic captures, parsed once up front: the
//! benches time the fold, not JSON parsing.

use std::hint::black_box;
use std::path::Path;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use muse_adapter::MuseFold;
use muse_client::{frame, MuseEvent};

/// The server→client events of one `fixtures/msp` capture.
fn events(name: &str) -> Vec<MuseEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp").join(name);
    let text = std::fs::read_to_string(&path).expect("capture is readable");
    text.lines()
        .filter_map(|line| line.strip_prefix("<-- "))
        .filter_map(|body| frame::parse_line(body).ok().flatten())
        .filter_map(MuseEvent::from_frame)
        .collect()
}

fn fold_all(events: &[MuseEvent]) -> MuseFold {
    let mut fold = MuseFold::new();
    for event in events {
        fold.apply(event.clone());
    }
    fold
}

/// A whole capture folded from empty: what opening a session from its
/// history costs the fold.
fn replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("fold/replay");
    for name in ["synthetic-stress-300.jsonl", "synthetic-stress-hetero.jsonl"] {
        let events = events(name);
        group.throughput(Throughput::Elements(events.len() as u64));
        group.bench_function(name.trim_end_matches(".jsonl"), |b| b.iter(|| fold_all(black_box(&events))));
    }
    group.finish();
}

/// The last 200 events applied to a fold already holding the rest of a
/// 300-turn session: the per-event streaming cost at length, which must not
/// grow with the transcript (docs/51 #6).
fn tail(c: &mut Criterion) {
    let events = events("synthetic-stress-300.jsonl");
    let split = events.len().saturating_sub(200);
    let (prefix, tail) = events.split_at(split);
    let mut group = c.benchmark_group("fold/tail");
    group.throughput(Throughput::Elements(tail.len() as u64));
    group.bench_function("stress-300_last_200", |b| {
        b.iter_batched(
            || fold_all(prefix),
            |mut fold| {
                for event in tail {
                    black_box(fold.apply(event.clone()));
                }
                fold
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

criterion_group!(benches, replay, tail);
criterion_main!(benches);
