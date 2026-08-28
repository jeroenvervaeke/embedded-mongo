//! What a reopened database can no longer do.
//!
//! Every probe here pins a defect rather than a guarantee, and they share one cause:
//! `DatabaseHolder::openDb` is never called for a database that already exists on disk, and it
//! is the only code that both registers the `Database` in `_dbs` and calls `init()` down to
//! `IndexCatalog::init()`. `listCollections` gates on `DatabaseHolder::dbExists`, while `find`,
//! `count` and `listIndexes` read the `CollectionCatalog` — which is why some commands work,
//! some see nothing, and one aborts. Documents stay reachable throughout, which is what makes
//! all of this so easy to miss.
//!
//! # A FAILURE HERE IS GOOD NEWS
//!
//! These probes are written to go red when the engine is repaired. A fix is queued. When it
//! lands, all four probes in this file should fail, and so should
//! `storage::an_unwritable_database_directory_aborts_the_process` — five pinned defects in all.
//! Do not "repair" a red probe by restoring the behaviour it describes. Rewrite it as an
//! assertion that the engine now does the right thing.

use crate::probe::{Role, harness::Child, index::SAMPLED_BUCKETS, scratch, workload::PER_CYCLE};

/// Documents behind the index in [`secondary_indexes_are_unusable_after_reopening`]. Enough
/// that a planner with a working index would have every reason to prefer it.
const INDEXED_DOCUMENTS: i64 = 400_000;

/// A defect, pinned rather than blessed.
///
/// A reopened database is queryable — `find`, `count` and `listIndexes` all reach its
/// collections — but `listCollections` reports the database as empty. Only a collection
/// created in the current session shows up, so anything that enumerates the catalog sees
/// nothing after a restart. Same root cause as
/// [`creating_an_index_after_reopening_aborts_the_process`]: the reopened database is never
/// put back into the engine's database holder, and both commands go through it.
#[test]
fn the_collection_catalog_is_not_listable_after_reopening() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let reopened = Child::spawn(Role::VerifyInserts, &path, PER_CYCLE - 1).finish();
    reopened.assert_exited_cleanly().report();

    assert_eq!(
        reopened.get("collections"),
        "",
        "listCollections reported the collection — THE ENGINE HAS BEEN FIXED; make this probe assert that it lists \
         every collection in the database"
    );
    assert_eq!(
        reopened.get("indexes"),
        "_id_",
        "the collection is reachable by every route except the listing\n{}",
        reopened.transcript()
    );
    assert_eq!(reopened.number("count"), PER_CYCLE);
}

/// A defect, pinned rather than blessed.
///
/// A secondary index survives a restart in the catalog and nowhere else. `listIndexes` reports
/// it, and neither route into it works: the planner picks a collection scan for a query the
/// index covers, and `hint` on it is refused. Building the index and querying it in one session
/// gives `IXSCAN`, so this is about reopening, not about the index. Every read after a restart
/// is therefore a collection scan, whatever indexes the app thinks it has.
#[test]
fn secondary_indexes_are_unusable_after_reopening() {
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
        "the index did not survive a clean close, which is a different bug from this one\n{}",
        reopened.transcript()
    );
    // The count first. `all` answers an empty slice for a key that was never reported, and
    // `iter().all(..)` is true of an empty slice, so a probe that stopped reporting would sail
    // through the predicate below without ever having looked at an index.
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
            .all(|plan| plan == "COLLSCAN"),
        "the planner used an index after a reopen — THE ENGINE HAS BEEN FIXED; make this probe and \
         `crash::no_query_is_answered_from_the_index_a_killed_build_left` into the real \
         index-versus-scan cross-checks they look like\n{}",
        reopened.transcript()
    );
    assert_eq!(
        reopened.all("hinted").len(),
        SAMPLED_BUCKETS.len(),
        "the reopen reported {} hint results, not the {} it samples\n{}",
        reopened.all("hinted").len(),
        SAMPLED_BUCKETS.len(),
        reopened.transcript()
    );
    assert!(
        reopened.all("hinted").iter().all(|hint| hint == "rejected"),
        "a hint onto the index was accepted after a reopen — THE ENGINE HAS BEEN FIXED; make this probe assert \
         that the hinted count matches the collection scan\n{}",
        reopened.transcript()
    );
}

/// A defect, pinned rather than blessed.
///
/// Building an index on a collection the process found on disk kills the process:
/// `IndexBuildsCoordinator` registers the build, cannot find the database behind the UUID it
/// just resolved, and MongoDB answers that with `invariant()` — "Database for probe.records
/// disappeared after successfully resolving <uuid>" — which calls `abort()`. Building the same
/// index in the session that created the collection is fine, so this is about reopening, not
/// about index builds.
#[test]
fn creating_an_index_after_reopening_aborts_the_process() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let outcome = Child::spawn(Role::IndexExisting, &path, 0).finish();
    outcome.report();
    assert!(
        outcome.was_aborted(),
        "createIndexes on a reopened database no longer aborts — THE ENGINE HAS BEEN FIXED; make this probe assert \
         that the index is created and usable\n{}",
        outcome.transcript()
    );
}

/// A defect, pinned rather than blessed — and the most damaging one here.
///
/// A write into a collection loaded from disk updates no index, `_id_` included, because
/// `IndexCatalog::init()` never ran for it. Nothing reports a problem: the insert is
/// acknowledged, the document is readable by collection scan, and `validate` cannot see it
/// because on a reopened database it validates zero indexes.
///
/// The probe demands the one thing a working `_id_` index cannot allow. It writes a document,
/// then writes the *same* `_id` again — and the engine takes it, leaving two documents with one
/// `_id` in a collection where that is supposed to be unrepresentable. Unlike everything else
/// in this file, this damage is written to disk and outlives the fix.
#[test]
fn writes_to_a_reopened_collection_bypass_the_id_index() {
    let directory = scratch::directory();
    let path = directory.path().join("database");
    Child::spawn(Role::ReopenCycles, &path, 1)
        .finish()
        .assert_exited_cleanly();

    let written = Child::spawn(Role::WriteAfterReopen, &path, 0).finish();
    written.assert_exited_cleanly().report();

    assert_eq!(
        written.get("duplicate"),
        "accepted",
        "the unique _id index rejected a duplicate after a reopen — THE ENGINE HAS BEEN FIXED; \
         make this probe assert that the duplicate is refused with error 11000\n{}",
        written.transcript()
    );
    assert_eq!(
        written.number("total"),
        PER_CYCLE + 2,
        "both copies of the duplicate _id should be in the collection\n{}",
        written.transcript()
    );
    assert_eq!(
        written.get("id_plan"),
        "COLLSCAN",
        "an _id lookup used the _id index after a reopen — THE ENGINE HAS BEEN FIXED; make \
         this probe assert that a write is visible through the index\n{}",
        written.transcript()
    );
}
