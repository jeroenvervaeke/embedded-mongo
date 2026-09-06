//! The two cancellation behaviours that need a real engine to prove.
//!
//! Both live in one test, and one test binary, because only one engine may exist per process --
//! the same reason `nonblocking.rs` is written as a single function with phases. They are kept
//! apart from `cancellation.rs` so that the measurement there stays about one thing.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{
    Client, CommandStrands, FreeDiskFloor, OpenOptions,
    bson::{Document, doc},
};
use std::time::Duration;

/// Long enough that the session is still busy when the second command is dispatched, so that
/// command is provably still queued when its future is dropped.
const DOCUMENTS: i64 = 40_000;

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_edges() {
    let temporary = scratch::directory("cancellation-edges-");
    let path = temporary.path().join("database");

    // One session, so a command dispatched while another is running cannot start.
    let client = Client::with_options(
        &path,
        OpenOptions::new()
            .command_strands(CommandStrands::from_count(1).expect("one session is in range")),
    )
    .await
    .expect("opening");

    let documents: Vec<Document> = (0..DOCUMENTS).map(|n| doc! { "n": n }).collect();
    client
        .database("edges")
        .collection::<Document>("numbers")
        .insert_many(documents)
        .await
        .expect("seeding");

    queued_commands_are_never_run(&client).await;
    moving_a_floor_completes_even_if_abandoned(&client).await;

    client.close().await.expect("closing");
}

/// A command whose future is dropped while it is still waiting for a session must never reach
/// the engine at all -- not run-and-discard. Proven by giving it a side effect and looking for
/// it afterwards, rather than by timing.
async fn queued_commands_are_never_run(client: &Client) {
    let marker = doc! { "_id": "queued-then-cancelled" };

    let (_grind, _abandoned) = tokio::join!(
        // Holds the only session for the whole of the block below.
        grind(client),
        async {
            // Long enough for the grind to have taken the session, short enough to be well
            // inside it.
            tokio::time::sleep(Duration::from_millis(100)).await;
            // Polled once -- which dispatches the insert -- and then dropped while it is still
            // queued behind the grind.
            let _ = tokio::time::timeout(
                Duration::from_millis(20),
                client
                    .database("edges")
                    .collection::<Document>("markers")
                    .insert_one(marker.clone()),
            )
            .await;
        }
    );

    let found = client
        .database("edges")
        .collection::<Document>("markers")
        .find_one(doc! { "_id": "queued-then-cancelled" })
        .await
        .expect("looking for the marker");
    assert!(
        found.is_none(),
        "a command cancelled while queued was run anyway: it should never have reached the engine"
    );
}

/// The documented exception. A floor is two `setParameter` commands, so abandoning the pair
/// half-way would leave the engine on floors nobody chose -- these futures deliberately do not
/// cancel. Dropping one stops the waiting; the pair still completes.
async fn moving_a_floor_completes_even_if_abandoned(client: &Client) {
    let limits = client.process_limits();
    let before = limits.free_disk_floors().await.expect("reading the floors");

    let asked = FreeDiskFloor::from_mebibytes(32).expect("32 MiB is in range");
    // Dropped as soon as it is dispatched, exactly as a cancellable command would be.
    let _ = tokio::time::timeout(Duration::from_millis(1), limits.set_free_disk_floor(asked)).await;

    // Give the workers a moment to finish the pair the caller stopped waiting for.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let after = limits
        .free_disk_floors()
        .await
        .expect("reading the floors again");
    assert_ne!(
        before, after,
        "the floor move was abandoned rather than completed; process limits must not cancel"
    );
    assert_eq!(
        after.index_build().mebibytes(),
        32,
        "the abandoned move should still have landed on the floor it asked for"
    );
}

/// A command slow enough to keep the single session busy.
async fn grind(client: &Client) -> Vec<Document> {
    client
        .database("edges")
        .collection::<Document>("numbers")
        .aggregate([
            doc! { "$project": { "crunched": { "$reduce": {
                "input": { "$range": [0, 400] },
                "initialValue": 0,
                "in": { "$add": ["$$value", { "$multiply": ["$$this", "$$this"] }] },
            } } } },
            doc! { "$group": { "_id": null, "total": { "$sum": "$crunched" } } },
        ])
        .await
        .expect("the grind")
        .try_collect()
        .await
        .expect("draining")
}
