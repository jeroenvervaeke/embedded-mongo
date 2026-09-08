//! That an elastic pool really opens sessions on demand, keeps to its ceiling, and closes them
//! again.
//!
//! Every claim here is checked against the engine rather than against this crate's own
//! bookkeeping. A session is a `mongo::Client` named `embedded-mongodb-session`, and
//! `$currentOp` with `idleConnections` lists every one of them, running or not -- so counting
//! them is asking mongod how many clients this crate has open, which is the thing
//! [`Concurrency`] claims to control. The unit tests in `pool::sessions` cover the clock; this
//! covers whether the pool and the engine agree.
//!
//! One test function, phase by phase, because a process may hold only one open engine at a
//! time: each phase opens its own client and closes it before the next.

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

/// Commands fanned out at once, and the widest ceiling measured.
const COMMANDS: usize = 8;

/// Documents per collection. With the aggregation below this puts one command in the tens of
/// milliseconds -- long enough that eight of them issued at once overlap, which is what makes
/// the pool grow at all.
const DOCUMENTS: i64 = 1_500;

/// How mongod names the clients this crate opens, set in `engine_runtime.cpp`.
const SESSION: &str = "embedded-mongodb-session";

/// Long enough that nothing is reaped while a phase is measuring growth.
const NEVER: Duration = Duration::from_secs(600);

#[test]
fn an_elastic_pool_opens_sessions_on_demand_and_closes_them_again() {
    let temporary = scratch::directory("elastic-concurrency-");
    let path = temporary.path().join("database");
    seed(&path);

    a_fixed_pool_opens_every_session_at_once_and_keeps_them(&path);
    an_elastic_pool_opens_only_its_floor_until_callers_ask_for_more(&path);
    an_elastic_pool_never_opens_more_than_its_ceiling(&path);
    sessions_idle_past_the_timeout_are_closed_back_down_to_the_floor(&path);
    an_async_client_grows_its_workers_with_the_pool(&path);
}

/// The behaviour that was here before the policy existed, and the one a fixed pool must keep:
/// every session opened up front, and none of them ever closed.
fn a_fixed_pool_opens_every_session_at_once_and_keeps_them(path: &Path) {
    let client = open(
        path,
        Concurrency::from_count(5).expect("5 sessions is in range"),
    );

    assert_eq!(
        sessions(&client),
        5,
        "a fixed pool opens every one of its sessions before the first command"
    );

    // Well past the shortest timeout any phase here uses, so a fixed pool that had a reaper
    // would have shown it by now.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        sessions(&client),
        5,
        "a fixed pool has no idle timeout, so it closes nothing"
    );

    client.close().expect("closing the fixed client");
}

fn an_elastic_pool_opens_only_its_floor_until_callers_ask_for_more(path: &Path) {
    let client = open(
        path,
        Concurrency::dynamic(1, COMMANDS as u32, NEVER).expect("1..8 is in range"),
    );

    assert_eq!(
        sessions(&client),
        1,
        "an elastic pool opens its floor and nothing more"
    );

    fan_out(&client, COMMANDS);

    let grown = sessions(&client);
    assert!(
        grown > 1,
        "{COMMANDS} commands issued at once left the pool at {grown} session(s): it did not \
         grow to serve callers that would otherwise have waited"
    );
    assert!(
        grown <= COMMANDS,
        "the pool grew to {grown} sessions, past the {COMMANDS} it was allowed"
    );

    client.close().expect("closing the elastic client");
}

/// The ceiling is the promise `max` makes: callers past it wait rather than opening a session
/// each, which is what stops a fan-out from turning into unbounded engine clients.
fn an_elastic_pool_never_opens_more_than_its_ceiling(path: &Path) {
    let ceiling = 3;
    let client = open(
        path,
        Concurrency::dynamic(1, ceiling, NEVER).expect("1..3 is in range"),
    );

    fan_out(&client, COMMANDS * 2);

    let grown = sessions(&client);
    assert!(
        grown <= ceiling as usize,
        "{} commands over a ceiling of {ceiling} left {grown} sessions open",
        COMMANDS * 2
    );

    client.close().expect("closing the capped client");
}

fn sessions_idle_past_the_timeout_are_closed_back_down_to_the_floor(path: &Path) {
    let timeout = Duration::from_millis(100);
    let client = open(
        path,
        Concurrency::dynamic(1, COMMANDS as u32, timeout).expect("1..8 is in range"),
    );

    fan_out(&client, COMMANDS);
    assert!(
        sessions(&client) > 1,
        "nothing to reap unless the pool grew first"
    );

    // Generous against a loaded machine: the reaper closes what is due at its next wake, and
    // this only has to be longer than that, not exactly it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while sessions(&client) > 1 && Instant::now() < deadline {
        thread::sleep(timeout);
    }

    assert_eq!(
        sessions(&client),
        1,
        "sessions idle past the timeout should have been closed back down to the floor"
    );

    // The bookkeeping the reaper touched has to still be true: a pool that lost count of what
    // it had open would either refuse to grow again or wait on a session it had closed.
    fan_out(&client, COMMANDS);
    assert!(
        sessions(&client) > 1,
        "a reaped pool has to open sessions again when callers come back"
    );

    client.close().expect("closing the reaped client");
}

/// Async parallelism is thread-bound -- a session only runs a command while a worker is inside
/// the FFI driving it -- so more than one session in use here is more than one worker, which is
/// the only way the async side gets anything out of an elastic pool.
fn an_async_client_grows_its_workers_with_the_pool(path: &Path) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("building a runtime for the async phase");

    runtime.block_on(async {
        let client = embedded_mongodb::Client::with_options(
            path,
            OpenOptions::new()
                .concurrency(Concurrency::dynamic(1, 4, NEVER).expect("1..4 is in range")),
        )
        .await
        .expect("opening an async client on an elastic pool");

        // `join!` rather than a task per command: it polls every one of them before any of
        // them can finish, which is exactly the shape that leaves callers waiting on a pool
        // that has to grow. Written out because there is no `join_all` without a futures crate.
        let command = |index: usize| {
            let client = &client;
            async move {
                client
                    .run_command("concurrency", crunch(index))
                    .await
                    .expect("running an aggregation");
            }
        };
        tokio::join!(
            command(0),
            command(1),
            command(2),
            command(3),
            command(4),
            command(5),
            command(6),
            command(7),
        );

        let reply = client
            .run_command("admin", current_op())
            .await
            .expect("running $currentOp");
        let grown = count_sessions(&reply);
        assert!(
            grown > 1,
            "{COMMANDS} commands awaited at once left {grown} session(s) open: the async worker \
             pool did not grow with them"
        );

        client.close().await.expect("closing the async client");
    });
}

/// Runs `count` CPU-bound aggregations at once, each on its own collection, so they contend
/// only where mongod's own connections would -- and so that they overlap, which is what makes a
/// caller find the pool empty and open a session.
fn fan_out(client: &Client, count: usize) {
    thread::scope(|scope| {
        for index in 0..count {
            scope.spawn(move || {
                client
                    .database("concurrency")
                    .run_command(&crunch(index % COMMANDS))
                    .expect("running an aggregation");
            });
        }
    });
}

/// Fills one collection per command with documents to crunch.
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

fn open(path: &Path, concurrency: Concurrency) -> Client {
    Client::with_options(path, OpenOptions::new().concurrency(concurrency))
        .expect("opening with a concurrency policy")
}

/// How many sessions this crate has open on the engine, as the engine sees it.
fn sessions(client: &Client) -> usize {
    let reply = client
        .database("admin")
        .run_command(&current_op())
        .expect("running $currentOp");
    count_sessions(&reply)
}

fn count_sessions(reply: &Document) -> usize {
    reply
        .get_document("cursor")
        .expect("a cursor in the $currentOp reply")
        .get_array("firstBatch")
        .expect("a first batch of operations")
        .iter()
        .filter_map(|operation| operation.as_document())
        .filter(|operation| operation.get_str("desc").is_ok_and(|desc| desc == SESSION))
        .count()
}

/// Every client the engine holds, idle ones included -- an idle session runs no operation, so
/// without this it would not be listed. The batch is sized past anything this suite opens, so
/// the count never needs a `getMore`.
fn current_op() -> Document {
    doc! {
        "aggregate": 1,
        "pipeline": [ { "$currentOp": { "allUsers": true, "idleConnections": true } } ],
        "cursor": { "batchSize": 1000 },
    }
}

/// One command's work: arithmetic the engine has to compute rather than fetch, so a command
/// costs execution time rather than I/O and two of them issued at once really do overlap.
fn crunch(index: usize) -> Document {
    doc! {
        "aggregate": collection(index),
        "pipeline": [
            { "$project": { "crunched": { "$reduce": {
                "input": { "$range": [0, 1200] },
                "initialValue": 0,
                "in": { "$add": ["$$value", { "$multiply": ["$$this", "$$this"] }] },
            } } } },
            { "$group": { "_id": null, "total": { "$sum": "$crunched" } } },
        ],
        "cursor": {},
    }
}

fn collection(index: usize) -> String {
    format!("numbers_{index}")
}
