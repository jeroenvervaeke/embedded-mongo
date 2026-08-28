//! The one-time pass that repairs directories an older build left with unmaintained indexes.
//!
//! Until the engine started calling `DatabaseHolder::openDb` for databases already on disk,
//! every write into a reopened directory went into the record store and into no index at all.
//! The engine is fixed; a directory an older build already wrote to is not, and never becomes
//! so on its own. `Client::new` therefore checks such a directory once and repairs what it
//! finds, and this is where that is held to its word.
//!
//! One `#[test]` function, as in `tests/features.rs`: the engine is a process-global singleton
//! and `cargo test` would otherwise open several at once. The skip switch lives in its own
//! target -- `tests/repair_skip.rs` -- because it has to be in the environment before anything
//! opens an engine, and setting it from inside a process that already has one running is not a
//! thing a test may do.

mod fixture;
mod inspect;
mod logs;

use embedded_mongodb::{
    Client, Error,
    bson::{Bson, doc},
};
use inspect::{count, is_valid, lost_and_found, surviving_customers};
use logs::{
    DELETED, PASS_RAN, REPAIRED, REPAIRING, Recorder, STILL_DAMAGED, assert_absent,
    assert_contains, field,
};
use std::{collections::BTreeSet, fs, path::Path, time::Instant};

/// Documents behind the timing section. Enough that a full validation of the collection and
/// its two indexes is real work rather than noise around opening the engine.
const MEASURED_DOCUMENTS: i64 = 20_000;

#[test]
fn a_directory_an_older_build_damaged_is_repaired_once() {
    let recorder = Recorder::install();
    let scratch = fixture::directory();

    eprintln!("--- a directory this build creates is marked without being scanned");
    let fresh = scratch.path().join("fresh");
    Client::new(&fresh).unwrap().close().unwrap();
    assert!(
        fixture::marker_exists(&fresh),
        "a new directory was left unmarked, so its next open would scan it for damage it \
         cannot have"
    );
    assert_absent(&recorder.take(), PASS_RAN);

    eprintln!("--- the marker survives a reopen, and the pass does not run again");
    Client::new(&fresh).unwrap().close().unwrap();
    assert!(fixture::marker_exists(&fresh));
    assert_absent(&recorder.take(), PASS_RAN);

    eprintln!("--- a damaged directory is repaired on the next open");
    let damaged = scratch.path().join("damaged");
    fixture::unpack_damaged(&damaged);
    let client = Client::new(&damaged).unwrap();
    let repair_log = recorder.take();

    assert_contains(&repair_log, PASS_RAN);
    assert_contains(&repair_log, REPAIRING);
    assert_contains(&repair_log, REPAIRED);
    // The fixture holds missing index entries and a duplicate `_id`, nothing malformed, so the
    // repair's one destructive branch must stay shut and every collection must come out sound.
    assert_absent(&repair_log, DELETED);
    assert_absent(&repair_log, STILL_DAMAGED);
    // The engine's counts, reported per collection: five entries were missing from `_id_` and
    // `customer_1` in shop.orders, one from catalog.items, and one document had to move to
    // make `_id_` unique again.
    assert_contains(
        &repair_log,
        "collection=shop.orders inserted_index_entries=5 documents_moved=1 \
         moved_to=local.lost_and_found.",
    );
    assert_contains(
        &repair_log,
        "collection=catalog.items inserted_index_entries=1 documents_moved=0",
    );
    // A sound collection in a damaged directory is not repaired, and nothing is said about it.
    assert_absent(&repair_log, "shop.untouched");
    assert!(fixture::marker_exists(&damaged));

    eprintln!("--- the documents the damaged indexes hid are reachable through them again");
    let shop = client.database("shop");
    // 0 before the repair: `customer` 5 and 6 were written after the reopen and never reached
    // `customer_1`, so an indexed lookup skipped them while a scan returned them.
    assert_eq!(
        count(&client, "shop", "orders", doc! { "customer": "c5" }),
        1
    );
    assert_eq!(count(&client, "shop", "orders", doc! { "_id": 5 }), 1);
    assert_eq!(count(&client, "catalog", "items", doc! { "_id": 3 }), 1);
    // Six, not seven: the seventh record was the second copy of `_id` 1, and it is in
    // local.lost_and_found rather than gone.
    assert_eq!(count(&client, "shop", "orders", doc! {}), 6);
    assert_eq!(count(&client, "shop", "orders", doc! { "_id": 1 }), 1);
    assert_eq!(count(&client, "shop", "untouched", doc! {}), 1);

    eprintln!("--- neither copy of the duplicate _id was destroyed");
    assert_eq!(
        surviving_customers(&client),
        BTreeSet::from(["c1".to_owned(), "duplicate".to_owned()]),
        "one of the two documents that shared _id 1 was lost instead of being moved"
    );
    // The crate composes `local.lost_and_found.<uuid>` itself, from the collection UUID rather
    // than from the prose warning that also carries it. Anyone following the log has to arrive
    // at the collection that exists, so the two are compared rather than pattern-matched.
    assert_eq!(
        field(&repair_log, "moved_to"),
        lost_and_found(&client),
        "the log sent the reader to a collection the engine did not create\n{repair_log}"
    );

    eprintln!("--- the repaired collections validate clean");
    for (database, collection) in [
        ("shop", "orders"),
        ("shop", "untouched"),
        ("catalog", "items"),
    ] {
        assert!(
            is_valid(&client, database, collection),
            "{database}.{collection} is still invalid after the repair pass"
        );
    }

    eprintln!("--- the _id index refuses a duplicate again");
    let refused = shop
        .run_command(&doc! {
            "insert": "orders",
            "documents": [ { "_id": 1, "customer": "another duplicate" } ],
        })
        .unwrap_err();
    assert!(
        matches!(
            &refused,
            Error::Server {
                code: Some(11000),
                ..
            }
        ),
        "a duplicate _id was accepted after the repair: {refused}"
    );
    client.close().unwrap();

    eprintln!("--- a repaired directory is not repaired a second time");
    let client = Client::new(&damaged).unwrap();
    assert_absent(&recorder.take(), PASS_RAN);
    assert_eq!(count(&client, "shop", "orders", doc! {}), 6);
    client.close().unwrap();

    eprintln!("--- deleting the marker runs the pass again, and it finds nothing to do");
    remove_marker(&damaged);
    Client::new(&damaged).unwrap().close().unwrap();
    let second_log = recorder.take();
    assert_contains(&second_log, PASS_RAN);
    // `REPAIRING` as well as `REPAIRED`: the second is gated on the repair having changed
    // something, so on its own it cannot tell a pass that repaired nothing from one that ran a
    // pointless repair over an already-sound collection.
    assert_absent(&second_log, REPAIRING);
    assert_absent(&second_log, REPAIRED);
    assert_absent(&second_log, STILL_DAMAGED);
    assert!(fixture::marker_exists(&damaged));

    eprintln!("--- a healthy directory is untouched, and pays for one scan and no more");
    let healthy = scratch.path().join("healthy");
    load(&healthy);
    let marked = time_open(&healthy);
    remove_marker(&healthy);
    let scanned = time_open(&healthy);
    let cost_log = recorder.take();

    assert_contains(&cost_log, PASS_RAN);
    assert_absent(&cost_log, REPAIRING);
    assert_absent(&cost_log, REPAIRED);

    // What "cheap" can be held to without asserting on a clock: the scan happens once. The
    // open after it must not run the pass again, or the cost would be paid on every open for
    // the life of the directory.
    let client = Client::new(&healthy).unwrap();
    assert_absent(&recorder.take(), PASS_RAN);
    assert_eq!(
        count(&client, "bench", "rows", doc! {}),
        MEASURED_DOCUMENTS,
        "the pass changed a healthy collection"
    );
    assert!(is_valid(&client, "bench", "rows"));
    client.close().unwrap();

    // Reported rather than asserted: a wall clock on a shared machine is not a test.
    eprintln!(
        "open of a healthy {MEASURED_DOCUMENTS}-document directory: {marked:?} with the marker, \
         {scanned:?} with the pass"
    );
}

/// Puts a directory back to how it looked before the pass ran, so the next open checks it
/// again. The nearest thing there is to a user upgrading into it.
fn remove_marker(path: &Path) {
    fs::remove_file(path.join(fixture::MARKER)).unwrap();
}

/// A healthy directory with enough in it that scanning it is measurable.
fn load(path: &Path) {
    let client = Client::new(path).unwrap();
    let bench = client.database("bench");
    let mut written = 0;
    while written < MEASURED_DOCUMENTS {
        let batch = (written..(written + 500).min(MEASURED_DOCUMENTS))
            .map(|id| Bson::Document(doc! { "_id": id, "bucket": id % 64, "payload": PAYLOAD }))
            .collect::<Vec<_>>();
        bench
            .run_command(&doc! { "insert": "rows", "documents": batch })
            .unwrap();
        written += 500;
    }
    bench
        .run_command(&doc! {
            "createIndexes": "rows",
            "indexes": [ { "key": { "bucket": 1 }, "name": "bucket_1" } ],
        })
        .unwrap();
    client.close().unwrap();
}

const PAYLOAD: &str = "a payload with enough weight that a document is worth a page of its own";

fn time_open(path: &Path) -> std::time::Duration {
    let started = Instant::now();
    let client = Client::new(path).unwrap();
    let elapsed = started.elapsed();
    client.close().unwrap();
    elapsed
}
