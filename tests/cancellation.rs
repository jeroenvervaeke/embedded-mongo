//! That dropping a command's future stops the engine working, and not merely the waiting.
//!
//! The distinction is the whole point. A future that only stops waiting leaves its session held
//! for as long as the query would have run, so a handful of abandoned slow queries starve the
//! pool and every later command blocks behind them. A future that cancels gives the session
//! back. The tests below measure exactly that difference.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{
    Client, Concurrency, OpenOptions,
    bson::{Document, doc},
};
use std::time::{Duration, Instant};

/// Documents to grind through. Sized so one command takes comfortably longer than the timeout
/// below, which is what makes "did it stop early?" a question with a clear answer.
const DOCUMENTS: i64 = 40_000;

/// How long the abandoned command is allowed to run before its future is dropped.
const PATIENCE: Duration = Duration::from_millis(300);

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_command_frees_its_session() {
    let temporary = scratch::directory("cancellation-");
    let path = temporary.path().join("database");

    // One session, so the pool is either free or it is not -- there is nothing to hide behind.
    // If cancelling did not release it, the command after the timeout could never run.
    let client = Client::with_options(
        &path,
        OpenOptions::new()
            .concurrency(Concurrency::from_count(1).expect("one session is in range")),
    )
    .await
    .expect("opening");

    let documents: Vec<Document> = (0..DOCUMENTS).map(|n| doc! { "n": n }).collect();
    client
        .database("cancel")
        .collection::<Document>("numbers")
        .insert_many(documents)
        .await
        .expect("seeding");

    // How long the grind really takes, so the assertions below compare against measurement
    // rather than a guess.
    let started = Instant::now();
    grind(&client).await.expect("the uninterrupted command");
    let uninterrupted = started.elapsed();
    assert!(
        uninterrupted > PATIENCE * 2,
        "the workload finishes in {uninterrupted:?}, too fast for a {PATIENCE:?} timeout to \
         prove anything -- raise DOCUMENTS"
    );

    // Abandon one mid-flight.
    let started = Instant::now();
    let abandoned = tokio::time::timeout(PATIENCE, grind(&client)).await;
    let waited = started.elapsed();
    assert!(
        abandoned.is_err(),
        "the command should have outlived {PATIENCE:?}"
    );
    assert!(
        waited < uninterrupted,
        "the timeout waited {waited:?}, as long as the whole command took ({uninterrupted:?})"
    );

    // The session must be back. With a pool of one, a command that had not been cancelled would
    // still be holding it, and this would block until the abandoned grind finished on its own.
    // Allowing well under a full grind is what makes this an assertion about cancelling rather
    // than about queueing.
    let started = Instant::now();
    let ping = tokio::time::timeout(
        uninterrupted,
        client.database("admin").run_command(doc! { "ping": 1 }),
    )
    .await
    .expect("the session should be free, not held by the abandoned command")
    .expect("ping");
    assert_eq!(ping.get_f64("ok").unwrap(), 1.0);
    println!(
        "grind {uninterrupted:?}; abandoned after {waited:?}; session back in {:?}",
        started.elapsed()
    );

    client.close().await.expect("closing");
}

/// A command slow enough to be worth abandoning: arithmetic the engine must compute, over
/// enough documents that it runs for a good fraction of a second.
async fn grind(client: &Client) -> Result<Vec<Document>, embedded_mongodb::Error> {
    client
        .database("cancel")
        .collection::<Document>("numbers")
        .aggregate([
            doc! { "$project": { "crunched": { "$reduce": {
                "input": { "$range": [0, 400] },
                "initialValue": 0,
                "in": { "$add": ["$$value", { "$multiply": ["$$this", "$$this"] }] },
            } } } },
            doc! { "$group": { "_id": null, "total": { "$sum": "$crunched" } } },
        ])
        .await?
        .try_collect()
        .await
}
