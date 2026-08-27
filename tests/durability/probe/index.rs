//! What a child reports about a secondary index it found in a directory on disk.

use super::{
    COLLECTION, DATABASE, INDEX,
    child::result,
    inspect::{access_path, count, names, number, report_validation, summary},
};
use anyhow::{Context, Result};
use embedded_mongodb::{Client, Database, bson::doc};
use std::{path::Path, time::Instant};

/// Buckets sampled by [`verify_index`]. Every one of them is checked three ways; a handful is
/// enough to catch an index that lost entries without making the probe a benchmark.
pub const SAMPLED_BUCKETS: [i64; 8] = [0, 1, 7, 13, 31, 42, 58, 63];

/// Reopens a directory and reports whether the secondary index is there and, if it is, whether
/// it agrees with the data. Three counts per bucket: through the index, through a forced
/// collection scan of the same field, and through a field that never had an index. An index
/// that lost entries -- or a half-built one the engine still considered usable -- would make
/// them disagree.
pub fn verify_index(directory: &Path, documents: i64) -> Result<()> {
    let started = Instant::now();
    let client = Client::new(directory).context("reopening after the kill")?;
    result("reopen_millis", started.elapsed().as_millis());
    let database = client.database(DATABASE);

    let indexes = names(&database, doc! { "listIndexes": COLLECTION })?;
    result("has_index", indexes.split(',').any(|name| name == INDEX));
    result("indexes", indexes);
    let (total, _, _) = summary(&database)?;
    result("count", total);
    result("expected_count", documents);

    for bucket in SAMPLED_BUCKETS {
        result(
            "indexed_plan",
            access_path(&database, doc! { "k": bucket })?,
        );
        report_hinted(&database, bucket);
        let indexed = count(&database, doc! { "k": bucket })?;
        let scanned = number(
            &database.run_command(&doc! {
                "count": COLLECTION,
                "query": { "k": bucket },
                "hint": { "$natural": 1 },
            })?,
            "n",
        )?;
        let unindexed = count(&database, doc! { "v": bucket })?;
        // Also reported one number per key, in bucket order, so a probe can line the three
        // counts up against each other instead of parsing them back out of the prose below.
        result("indexed_count", indexed);
        result("scanned_count", scanned);
        result(
            "bucket",
            format_args!("{bucket} indexed={indexed} scanned={scanned} unindexed={unindexed}"),
        );
    }

    report_validation(&database)?;
    client.close()?;
    result("closed", "ok");
    Ok(())
}

/// What happens when the index is demanded rather than merely offered. The planner ignoring an
/// index and the engine refusing to use it at all are different failures, and only the second
/// one rules out ever answering a query from index entries a killed build left behind.
fn report_hinted(database: &Database<'_>, bucket: i64) {
    match database.run_command(&doc! {
        "count": COLLECTION,
        "query": { "k": bucket },
        "hint": { "k": 1 },
    }) {
        Ok(response) => {
            result("hinted", "ok");
            result("hinted_count", number(&response, "n").unwrap_or(-1));
        }
        Err(error) => {
            result("hinted", "rejected");
            result("hinted_message", error);
        }
    }
}
