//! That the session pool is the concurrency level, and nothing else is.
//!
//! The design rests on one claim: N sessions run N commands at once. Everything above it --
//! `Concurrency`, the blocking pool, one async worker per session -- is sizing policy built on
//! that claim, so it is worth measuring rather than asserting. `elastic_concurrency.rs` covers
//! what the policy does with that claim; this covers the claim itself.
//!
//! The measurement varies the session count and holds everything else still. Eight caller
//! threads issue eight identical CPU-bound aggregations every time; only the pool size moves.
//! If sessions gate concurrency, wall-clock falls as the pool grows -- eight commands over one
//! session cost eight commands' time, over eight sessions cost one. If something else were the
//! gate (a lock in the engine, a strand shared behind our back) the wall-clock would barely
//! move, because the thread count never does. Fixed pools throughout, so the size under test is
//! the size the pool has.
//!
//! Deliberately the blocking API: it owns no threads, so the caller's eight are the only ones
//! in play and the session pool is unambiguously the variable. That is also exactly the shape
//! the Python and Android bindings run in.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{
    Concurrency, OpenOptions,
    blocking::Client,
    bson::{Document, doc},
};
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

/// Commands issued per round, and the largest pool measured -- so the widest pool runs all of
/// them at once and the narrowest serialises all of them.
const COMMANDS: usize = 8;

/// Pool sizes measured, ascending. Each divides `COMMANDS`, so the ideal wall-clock is exactly
/// `COMMANDS / sessions` command-times and the expected ratio against one session is the pool
/// size itself.
const POOLS: [u32; 4] = [1, 2, 4, 8];

/// Documents per collection. With the pipeline below this puts one command in the tens of
/// milliseconds -- long enough to dwarf the pool's own bookkeeping, short enough that the
/// slowest round here stays well inside a test's patience.
const DOCUMENTS: i64 = 1_500;

#[test]
fn the_session_pool_is_the_concurrency_level() {
    let cores = thread::available_parallelism().map_or(1, |cores| cores.get());
    if cores < COMMANDS {
        // Below one core per command the machine, not the pool, is the gate, and the ratios
        // this asserts would be measuring the scheduler.
        println!("skipped: {COMMANDS} concurrent commands need {COMMANDS} cores, found {cores}");
        return;
    }

    let temporary = scratch::directory("session-concurrency-");
    let path = temporary.path().join("database");
    seed(&path);

    let mut elapsed = Vec::new();
    for sessions in POOLS {
        elapsed.push(measure(&path, sessions));
    }

    let baseline = elapsed[0];
    println!("{COMMANDS} commands, {cores} cores:");
    for (index, sessions) in POOLS.iter().enumerate() {
        let speedup = baseline.as_secs_f64() / elapsed[index].as_secs_f64();
        println!(
            "  {sessions} session(s): {:?}  ({speedup:.2}x)",
            elapsed[index]
        );
    }

    // Half of ideal, which leaves room for scheduling and for the engine's own shared work
    // while still failing anything that merely trends in the right direction. One session
    // against eight is the load-bearing comparison: it is the difference between a pool that
    // gates concurrency and a pool that does not.
    for (index, sessions) in POOLS.iter().enumerate().skip(1) {
        let speedup = baseline.as_secs_f64() / elapsed[index].as_secs_f64();
        let floor = f64::from(*sessions) / 2.0;
        assert!(
            speedup >= floor,
            "{COMMANDS} commands over {sessions} sessions ran {speedup:.2}x faster than over \
             one, short of the {floor:.2}x a pool that gates concurrency would give: {:?} \
             against {baseline:?}",
            elapsed[index]
        );
    }

    // The other direction, and the one a flat result would fail: a single session must not run
    // two commands at once, so eight of them cost about eight command-times. Compared against
    // the widest pool rather than a measured single command, because that keeps the whole
    // assertion inside one round of the same work.
    let widest = elapsed[POOLS.len() - 1];
    assert!(
        baseline >= widest.mul_f64(COMMANDS as f64 / 2.0),
        "one session ran {COMMANDS} commands in {baseline:?}, close enough to the {widest:?} \
         that {COMMANDS} sessions took that it cannot be serialising them"
    );
}

/// Fills one collection per command with documents to crunch. Its own client, closed before
/// anything is measured, so seeding costs nothing in the rounds below.
fn seed(path: &Path) {
    let client = Client::new(path).expect("opening to seed");
    for index in 0..COMMANDS {
        let documents: Vec<Document> = (0..DOCUMENTS).map(|n| doc! { "n": n }).collect();
        client
            .database("concurrency")
            .collection::<Document>(&collection(index))
            .insert_many(documents)
            .expect("seeding a collection");
    }
    client.close().expect("closing the seeding client");
}

/// Runs [`COMMANDS`] aggregations at once over a pool of `sessions`, and answers how long the
/// fan-out took. Only the fan-out is timed: the open behind it does storage recovery and the
/// index-repair check, which have nothing to do with what is being measured.
fn measure(path: &Path, sessions: u32) -> Duration {
    let options = OpenOptions::new()
        .concurrency(Concurrency::from_count(sessions).expect("a pool size in range"));
    let client = Client::with_options(path, options).expect("opening with a sized pool");

    let started = Instant::now();
    thread::scope(|scope| {
        for index in 0..COMMANDS {
            let client = &client;
            scope.spawn(move || crunch(client, index));
        }
    });
    let elapsed = started.elapsed();

    client.close().expect("closing a measured client");
    elapsed
}

/// One command's work: arithmetic the engine has to compute rather than fetch, so the clock
/// measures execution and not I/O. Each command reads its own collection, so two running at
/// once contend only where MongoDB's own connections would.
fn crunch(client: &Client, index: usize) {
    let totals = client
        .database("concurrency")
        .collection::<Document>(&collection(index))
        .aggregate([
            doc! { "$project": { "crunched": { "$reduce": {
                "input": { "$range": [0, 1200] },
                "initialValue": 0,
                "in": { "$add": ["$$value", { "$multiply": ["$$this", "$$this"] }] },
            } } } },
            doc! { "$group": { "_id": null, "total": { "$sum": "$crunched" } } },
        ])
        .expect("running an aggregation")
        .try_collect()
        .expect("draining the cursor");
    assert_eq!(totals.len(), 1, "the aggregation should answer one group");
}

fn collection(index: usize) -> String {
    format!("numbers_{index}")
}
