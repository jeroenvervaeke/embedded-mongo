//! The async API, end to end, and the reason it exists: parallel commands.
//!
//! One test function, deliberately, like `helpers.rs`: only one engine runtime may exist per
//! process, so the sections run in sequence inside it rather than racing to open as separate
//! tests would.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{
    Client, CommandStrands, Error, OpenOptions,
    bson::{Bson, Document, doc, oid::ObjectId},
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Serialize)]
struct Item {
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    name: String,
}

/// How many queries the speedup measurement fans out -- the default strand pool, so every
/// one of them can hold a strand at once.
const QUERIES: usize = 8;

#[tokio::test(flavor = "multi_thread")]
async fn the_async_api_works_end_to_end_and_in_parallel() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Client>();

    let temporary = scratch::directory("nonblocking-");
    let path = temporary.path().join("database");

    let client = Client::new(&path).await.unwrap();
    let ping = client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await
        .unwrap();
    assert_eq!(ping.get_f64("ok").unwrap(), 1.0);

    // The same shapes helpers.rs proves of the blocking API: a typed insert whose id comes
    // back, a batch big enough that reading it back crosses a getMore, and a find_one.
    let items = client.database("test").collection::<Item>("items");
    let inserted = items
        .insert_one(Item {
            id: None,
            name: "persisted".to_owned(),
        })
        .await
        .unwrap();
    let persisted_id = match inserted.inserted_id {
        Bson::ObjectId(id) => id,
        id => panic!("expected ObjectId, got {id:?}"),
    };

    let inserted = items
        .insert_many((0..110).map(|index| Item {
            id: None,
            name: format!("batch-{index}"),
        }))
        .await
        .unwrap();
    assert_eq!(inserted.inserted_ids.len(), 110);

    let documents = items
        .find(doc! {})
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(documents.len(), 111);

    let mut cursor = items.find(doc! {}).await.unwrap();
    let mut streamed = 0;
    while let Some(document) = cursor.next().await {
        document.unwrap();
        streamed += 1;
    }
    assert_eq!(streamed, 111);

    let found = items
        .find_one(doc! { "_id": persisted_id })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.name, "persisted");

    let error = items
        .insert_one(Item {
            id: Some(persisted_id),
            name: "duplicate".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::Server { .. }));

    let totals = items
        .aggregate([doc! { "$count": "documents" }])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(totals[0].get_i32("documents").unwrap(), 111);

    // The async handle reaches the same process-wide floors the blocking one does;
    // storage_limits.rs pins their values, this only proves the async route to them.
    let floors = client.process_limits().free_disk_floors().await.unwrap();
    let _ = (floors.index_build(), floors.query_spilling());

    // Four insert futures in flight at once against one borrowed client: the concurrency
    // smoke the blocking test runs with scoped threads, run here with tasks.
    let insert = |index: usize| {
        let items = &items;
        async move {
            items
                .insert_one(Item {
                    id: None,
                    name: format!("task-{index}"),
                })
                .await
                .unwrap();
        }
    };
    tokio::join!(insert(0), insert(1), insert(2), insert(3));
    let documents = items
        .find(doc! {})
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(documents.len(), 115);

    let (sequential, parallel) = measure_speedup(&client).await;
    let speedup = sequential.as_secs_f64() / parallel.as_secs_f64();
    println!(
        "{QUERIES} queries: {sequential:?} sequentially, {parallel:?} in parallel -- {speedup:.2}x"
    );
    let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());
    if cores >= 2 {
        assert!(
            parallel < sequential.mul_f64(0.75),
            "{QUERIES} parallel queries took {parallel:?} against {sequential:?} run one at a \
             time on {cores} cores -- the strand pool is not running commands in parallel"
        );
    }

    client.close().await.unwrap();

    // A cursor's getMore after close answers Closed rather than hanging on a queue nobody
    // serves. The cursor comes from a fresh open so it has batches left to want.
    let client = Client::with_options(
        &path,
        OpenOptions::new()
            .command_strands(CommandStrands::from_count(2).expect("2 strands is in range")),
    )
    .await
    .unwrap();
    let item = client
        .database("test")
        .collection::<Item>("items")
        .find_one(doc! { "_id": persisted_id })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.name, "persisted");
    let mut leftover = client
        .database("test")
        .collection::<Item>("items")
        .find(doc! {})
        .await
        .unwrap();
    leftover.next().await.unwrap().unwrap();
    client.close().await.unwrap();
    // The first batch is already buffered in the cursor and drains fine; it is the getMore
    // after it that must answer Closed rather than hang on a queue nobody serves.
    let refused = loop {
        match leftover.next().await {
            Some(Ok(_)) => continue,
            outcome => break outcome,
        }
    };
    assert!(
        matches!(refused, Some(Err(Error::Closed))),
        "a cursor that outlived its client answered {refused:?}"
    );
}

/// The same CPU-bound aggregation run [`QUERIES`] times over, once one at a time and once all
/// at once. Each query crunches its own collection so the parallel run contends only in the
/// engine, not on a single collection's cache pages.
async fn measure_speedup(client: &Client) -> (Duration, Duration) {
    for index in 0..QUERIES {
        let documents: Vec<Document> = (0..1000).map(|n| doc! { "n": n as i64 }).collect();
        client
            .database("parallel")
            .collection::<Document>(&format!("numbers_{index}"))
            .insert_many(documents)
            .await
            .unwrap();
    }

    let query = |index: usize| async move {
        let total = client
            .database("parallel")
            .collection::<Document>(&format!("numbers_{index}"))
            .aggregate([
                // Per document, a 1500-step arithmetic reduce: work the engine has to
                // compute, not fetch, so wall clock measures execution rather than I/O.
                doc! { "$project": { "crunched": { "$reduce": {
                    "input": { "$range": [0, 1500] },
                    "initialValue": 0,
                    "in": { "$add": ["$$value", { "$multiply": ["$$this", "$$this"] }] },
                } } } },
                doc! { "$group": { "_id": null, "total": { "$sum": "$crunched" } } },
            ])
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        assert_eq!(total.len(), 1);
    };

    // One untimed round first, so the timed ones compare compute against compute rather
    // than against first-touch page faults and cache fills.
    for index in 0..QUERIES {
        query(index).await;
    }

    let started = Instant::now();
    for index in 0..QUERIES {
        query(index).await;
    }
    let sequential = started.elapsed();

    let started = Instant::now();
    tokio::join!(
        query(0),
        query(1),
        query(2),
        query(3),
        query(4),
        query(5),
        query(6),
        query(7),
    );
    let parallel = started.elapsed();

    (sequential, parallel)
}
