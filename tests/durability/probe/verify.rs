//! What a child reports about the documents in a directory the previous child died on.

use super::{
    COLLECTION, DATABASE,
    child::result,
    inspect::{count, names, optional_names, report_validation, summary},
};
use anyhow::{Context, Result};
use embedded_mongodb::{Client, bson::doc};
use std::{path::Path, time::Instant};

/// Reopens after a kill and reports what survived: how long the reopen took, what the catalog
/// holds, whether `validate` is happy, and — the point of the whole probe — whether the
/// surviving `_id`s are an unbroken prefix of what the dead child had acknowledged.
pub fn verify_inserts(directory: &Path, acknowledged_through: i64) -> Result<()> {
    let started = Instant::now();
    let client = Client::new(directory).context("reopening after the kill")?;
    result("reopen_millis", started.elapsed().as_millis());
    let database = client.database(DATABASE);

    result(
        "collections",
        names(&database, doc! { "listCollections": 1 })?,
    );

    let Some(indexes) = optional_names(&database, doc! { "listIndexes": COLLECTION })? else {
        result("collection", "absent");
        report_nothing_survived(acknowledged_through);
        client.close()?;
        result("closed", "ok");
        return Ok(());
    };
    result("collection", "present");
    result("indexes", indexes);

    // Two counts, because they do not agree after a kill. `count` with no query answers from
    // the collection's stored record count, which an unclean shutdown leaves stale; the
    // aggregation actually walks the documents.
    result("fast_count", count(&database, doc! {})?);
    let (total, lowest, highest) = summary(&database)?;
    result("count", total);
    result("lowest_id", lowest);
    result("highest_id", highest);
    // Recovery is supposed to replay a prefix of the write history. A gap means a document
    // came back while an earlier one did not, which is the shape of real corruption.
    result("holes", highest - lowest + 1 - total);

    let acknowledged = count(&database, doc! { "_id": { "$lte": acknowledged_through } })?;
    result("acknowledged_present", acknowledged);
    result(
        "acknowledged_missing",
        acknowledged_through + 1 - acknowledged,
    );

    report_validation(&database)?;
    client.close()?;
    result("closed", "ok");
    Ok(())
}

/// What is left to report when the kill took the collection with it.
///
/// Deliberately short: every key that describes documents that are not there is left out
/// rather than filled with a zero. A probe that asks for `holes` on this path gets a loud
/// "reported no holes" instead of a zero it would have accepted as proof of anything.
fn report_nothing_survived(acknowledged_through: i64) {
    result("indexes", "");
    result("count", 0);
    result("acknowledged_present", 0);
    result("acknowledged_missing", acknowledged_through + 1);
}
