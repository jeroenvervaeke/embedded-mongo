//! What a reopened database can do.
//!
//! Every probe here used to pin a defect. They shared one cause: `DatabaseHolder::openDb` was
//! never called for a database that already existed on disk, and it is the only code that both
//! registers the `Database` in `_dbs` and calls `init()` down to `IndexCatalog::init()`. A
//! reopened database therefore listed no collections, hid every index from the planner, kept no
//! index up to date on write — `_id_` included — and aborted the process on any command that
//! acquired a collection by UUID.
//!
//! The engine now runs `catalog_repair::reconcileCatalogAndIdents` and then opens every database
//! on disk, which is what mongod's startup recovery does. These probes assert the result: a
//! directory that comes back from disk is indistinguishable from one this process created.

use crate::probe::{Role, harness::Child, index::SAMPLED_BUCKETS, scratch, workload::PER_CYCLE};

/// Documents behind the index in [`secondary_indexes_answer_queries_after_reopening`]. Enough
/// that a planner with a working index has every reason to prefer it, so a `COLLSCAN` there
/// means the index is unusable rather than merely unattractive.
const INDEXED_DOCUMENTS: i64 = 400_000;

/// MongoDB's duplicate key error.
const DUPLICATE_KEY: &str = "11000";

/// `listCollections` enumerates the database holder, which is populated only by `openDb`. It is
/// the cheapest, sharpest check that a database restored from disk was actually opened rather
/// than merely present in the collection catalog — `find`, `count` and `listIndexes` all read
/// the catalog and answered correctly even when this returned an empty batch.
#[test]
fn every_collection_is_listable_after_reopening() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let reopened = Child::spawn(Role::VerifyInserts, &path, PER_CYCLE - 1).finish();
    reopened.assert_exited_cleanly().report();

    assert_eq!(
        reopened.get("collections"),
        "records",
        "listCollections did not report the collection the previous process left behind\n{}",
        reopened.transcript()
    );
    assert_eq!(
        reopened.get("indexes"),
        "_id_",
        "the collection is reachable by every route\n{}",
        reopened.transcript()
    );
    assert_eq!(reopened.number("count"), PER_CYCLE);
}

/// A secondary index built in one session is used by the next one.
///
/// Three counts per sampled bucket have to agree: through the index, through a hint that demands
/// it, and through a forced collection scan of the same field. The access path is asserted
/// first, because counts taken "through the index" and counts from a scan would agree trivially
/// if both were scans.
#[test]
fn secondary_indexes_answer_queries_after_reopening() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::BuildIndex, &path, INDEXED_DOCUMENTS)
        .finish()
        .assert_exited_cleanly();

    let reopened = Child::spawn(Role::VerifyIndex, &path, INDEXED_DOCUMENTS).finish();
    reopened.assert_exited_cleanly().report();

    assert_eq!(
        reopened.get("has_index"),
        "true",
        "the index did not survive a clean close\n{}",
        reopened.transcript()
    );
    // The counts of results first, not just their contents: `all` answers an empty slice for a
    // key that was never reported, and a probe that stopped reporting would sail through every
    // predicate below without ever having looked at an index.
    assert_eq!(
        reopened.all("indexed_plan").len(),
        SAMPLED_BUCKETS.len(),
        "the reopen reported {} access paths, not the {} it samples\n{}",
        reopened.all("indexed_plan").len(),
        SAMPLED_BUCKETS.len(),
        reopened.transcript()
    );
    assert!(
        reopened
            .all("indexed_plan")
            .iter()
            .all(|plan| plan == "INDEXED"),
        "the planner fell back to a collection scan on an indexed field\n{}",
        reopened.transcript()
    );
    assert!(
        reopened.all("hinted").iter().all(|hint| hint == "ok"),
        "a hint onto the index was refused\n{}",
        reopened.transcript()
    );
    // Anchors the two comparisons below, which are equalities between slices and would hold
    // vacuously if the probe had reported no counts at all.
    assert_eq!(
        reopened.all("scanned_count").len(),
        SAMPLED_BUCKETS.len(),
        "the reopen reported {} scan counts, not the {} it samples\n{}",
        reopened.all("scanned_count").len(),
        SAMPLED_BUCKETS.len(),
        reopened.transcript()
    );
    assert_eq!(
        reopened.all("indexed_count"),
        reopened.all("scanned_count"),
        "the index answered a different count than a collection scan of the same field\n{}",
        reopened.transcript()
    );
    assert_eq!(
        reopened.all("hinted_count"),
        reopened.all("scanned_count"),
        "the hinted count did not match the collection scan\n{}",
        reopened.transcript()
    );
}

/// Building an index on a collection this process found on disk used to abort it, through the
/// "Database for probe.records disappeared after successfully resolving <uuid>" invariant that
/// `IndexBuildsCoordinator` reaches by acquiring the collection by UUID. The probe asserts both
/// halves of the fix: the build completes, and the index it produced is one the next session
/// can actually query through.
#[test]
fn an_index_can_be_created_on_a_reopened_database() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let built = Child::spawn(Role::IndexExisting, &path, 0).finish();
    built.assert_exited_cleanly().report();
    assert_eq!(
        built.get("create_indexes"),
        "ok",
        "createIndexes on a reopened database failed\n{}",
        built.transcript()
    );

    let reopened = Child::spawn(Role::VerifyIndex, &path, PER_CYCLE).finish();
    reopened.assert_exited_cleanly().report();
    assert_eq!(
        reopened.get("has_index"),
        "true",
        "the index createIndexes reported building is not in the catalog\n{}",
        reopened.transcript()
    );
    assert_eq!(
        reopened.all("indexed_plan").len(),
        SAMPLED_BUCKETS.len(),
        "the reopen reported {} access paths, not the {} it samples\n{}",
        reopened.all("indexed_plan").len(),
        SAMPLED_BUCKETS.len(),
        reopened.transcript()
    );
    assert!(
        reopened
            .all("indexed_plan")
            .iter()
            .all(|plan| plan == "INDEXED"),
        "the index built on a reopened database is not used to answer queries\n{}",
        reopened.transcript()
    );
    assert_eq!(reopened.get("valid"), "true", "{}", reopened.transcript());
}

/// The one that matters most: a write into a collection loaded from disk maintains its indexes.
///
/// It used to maintain none of them, `_id_` included, while the durable catalog still advertised
/// `_id_` as ready — so the engine accepted two documents with the same `_id` and wrote that to
/// disk. The probe demands the thing a working `_id_` index cannot allow, and now asserts it is
/// refused: the write is visible through the index, and the duplicate comes back as error 11000.
#[test]
fn writes_to_a_reopened_collection_go_through_the_id_index() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let written = Child::spawn(Role::WriteAfterReopen, &path, 0).finish();
    written.assert_exited_cleanly().report();

    assert_eq!(
        written.get("id_plan"),
        "INDEXED",
        "an _id lookup after a reopen was answered by a collection scan\n{}",
        written.transcript()
    );
    assert_eq!(
        written.number("through_id_index"),
        written.number("through_scan"),
        "the _id index and a collection scan disagree about the document just written\n{}",
        written.transcript()
    );
    assert_eq!(
        written.get("duplicate"),
        "error",
        "the unique _id index accepted a duplicate _id\n{}",
        written.transcript()
    );
    assert_eq!(
        written.get("duplicate_code"),
        DUPLICATE_KEY,
        "the duplicate _id was refused, but not as a duplicate key error\n{}",
        written.transcript()
    );
    assert_eq!(
        written.number("total"),
        PER_CYCLE + 1,
        "the refused duplicate left a second copy in the collection\n{}",
        written.transcript()
    );
}
