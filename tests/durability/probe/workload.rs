//! What a child does to the engine before the parent takes it away.

use super::{
    COLLECTION, DATABASE, INDEX,
    child::{ack, phase, result},
    inspect::{access_path, count, directory_bytes, number, open_descriptors, report_error},
};
use anyhow::{Context, Result};
use embedded_mongodb::{
    Client, Database,
    bson::{Bson, doc},
};
use std::path::Path;

/// Big enough that a document is worth writing out rather than something the cache can hold
/// indefinitely, small enough that a few hundred thousand of them still fit a temp dir.
const PAYLOAD: &str = "durability probe payload, repeated to give every document real weight \
     so the storage engine has dirty pages to lose rather than a handful of tiny records";

/// Distinct values of the indexed field. Every bucket holds many documents, so a query that
/// went through a damaged index would return a visibly different count than a scan.
const BUCKETS: i64 = 64;

const BATCH: i64 = 500;

/// Documents written per open/close cycle in [`reopen_cycles`].
pub const PER_CYCLE: i64 = 25;

/// Whether every write waits for the journal before it is acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Journaling {
    Off,
    On,
}

/// Inserts one document at a time until the parent kills the process. Every `ack` the parent
/// manages to read was acknowledged by the engine before the kill, which is exactly the set
/// the reopen has to account for.
pub fn insert_until_killed(directory: &Path, journaling: Journaling) -> Result<()> {
    let client = Client::new(directory)?;
    let database = client.database(DATABASE);
    phase("open");

    let mut sequence = 0_i64;
    loop {
        let mut command = doc! {
            "insert": COLLECTION,
            "documents": [ doc! { "_id": sequence, "payload": PAYLOAD } ],
        };
        if journaling == Journaling::On {
            command.insert("writeConcern", doc! { "w": 1, "j": true });
        }
        database.run_command(&command)?;
        ack(sequence);
        sequence += 1;
    }
}

/// Fills a collection, then builds a secondary index on it. The kill lands between
/// `phase building` and `phase built`.
pub fn build_index(directory: &Path, documents: i64) -> Result<()> {
    let client = Client::new(directory)?;
    let database = client.database(DATABASE);
    load(&database, documents)?;
    phase("loaded");

    phase("building");
    database.run_command(&doc! {
        "createIndexes": COLLECTION,
        "indexes": [ doc! { "key": { "k": 1 }, "name": INDEX } ],
    })?;
    phase("built");
    client.close()?;
    Ok(())
}

/// Fills a collection, then closes. The kill lands between `phase closing` and `phase closed`.
pub fn insert_then_close(directory: &Path, documents: i64) -> Result<()> {
    let client = Client::new(directory)?;
    load(&client.database(DATABASE), documents)?;
    ack(documents - 1);

    phase("closing");
    client.close()?;
    phase("closed");
    Ok(())
}

/// Holds the directory open until the parent writes a line to stdin.
pub fn hold_open(directory: &Path) -> Result<()> {
    let client = Client::new(directory)?;
    load(&client.database(DATABASE), PER_CYCLE)?;
    phase("ready");

    let mut release = String::new();
    std::io::stdin()
        .read_line(&mut release)
        .context("waiting for the parent to release the directory")?;
    client.close()?;
    phase("closed");
    Ok(())
}

/// Builds a secondary index on a collection this process found on disk rather than created.
pub fn index_existing(directory: &Path) -> Result<()> {
    let client = Client::new(directory)?;
    phase("open");
    match client.database(DATABASE).run_command(&doc! {
        "createIndexes": COLLECTION,
        "indexes": [ doc! { "key": { "k": 1 }, "name": INDEX } ],
    }) {
        Ok(_) => result("create_indexes", "ok"),
        Err(error) => report_error("create_indexes", &error),
    }
    client.close()?;
    Ok(())
}

/// Writes one document into a collection this process found on disk, then asks the indexes
/// about it three ways: through `_id`, through a forced collection scan, and by offering the
/// engine a duplicate `_id` that a working unique index has to reject.
pub fn write_after_reopen(directory: &Path) -> Result<()> {
    let client = Client::new(directory)?;
    let database = client.database(DATABASE);
    // Far past anything the process that created the directory wrote, so a hit can only be
    // this document.
    let id = PER_CYCLE * 1_000;
    let written = doc! { "_id": id, "payload": PAYLOAD };
    database.run_command(&doc! {
        "insert": COLLECTION,
        "documents": [ written.clone() ],
        "writeConcern": { "w": 1, "j": true },
    })?;
    result("write", "ok");

    result("id_plan", access_path(&database, doc! { "_id": id })?);
    result("through_id_index", count(&database, doc! { "_id": id })?);
    result(
        "through_scan",
        number(
            &database.run_command(&doc! {
                "count": COLLECTION,
                "query": { "_id": id },
                "hint": { "$natural": 1 },
            })?,
            "n",
        )?,
    );

    match database.run_command(&doc! { "insert": COLLECTION, "documents": [written] }) {
        Ok(_) => result("duplicate", "accepted"),
        Err(error) => report_error("duplicate", &error),
    }
    result(
        "total",
        count(&database, doc! { "_id": { "$gte": 0_i64 } })?,
    );

    client.close()?;
    result("closed", "ok");
    Ok(())
}

/// Reports what a single open did, whether or not it worked.
pub fn open_once(directory: &Path) -> Result<()> {
    match Client::new(directory) {
        Ok(client) => {
            result("open", "ok");
            client.close()?;
        }
        Err(error) => report_error("open", &error),
    }
    Ok(())
}

/// Opens a second client while the first is still alive, then proves the refused open did not
/// consume the process-wide runtime slot.
pub fn open_twice(directory: &Path) -> Result<()> {
    let first = Client::new(directory).context("first open")?;
    result("first_open", "ok");

    match Client::new(directory) {
        Ok(second) => {
            result("second_open", "ok");
            second.close()?;
        }
        Err(error) => report_error("second_open", &error),
    }

    first.close()?;
    let reopened = Client::new(directory).context("reopen after close")?;
    result("reopen_after_close", "ok");
    reopened.close()?;
    Ok(())
}

/// open -> insert -> close, over and over, reporting what each cycle cost.
pub fn reopen_cycles(directory: &Path, cycles: i64) -> Result<()> {
    for cycle in 0..cycles {
        let client = Client::new(directory)?;
        let database = client.database(DATABASE);
        let before = count(&database, doc! {})?;
        let documents = (0..PER_CYCLE)
            .map(|offset| {
                Bson::Document(doc! { "_id": cycle * PER_CYCLE + offset, "payload": PAYLOAD })
            })
            .collect::<Vec<_>>();
        database.run_command(&doc! {
            "insert": COLLECTION,
            "documents": documents,
            "writeConcern": { "w": 1, "j": true },
        })?;
        let after = count(&database, doc! {})?;
        client.close()?;

        result(
            "cycle",
            format_args!(
                "{cycle} before={before} after={after} descriptors={} bytes={}",
                open_descriptors(),
                directory_bytes(directory)?
            ),
        );
    }
    Ok(())
}

/// Inserts `documents` documents with `_id` 0..documents, journalling the last batch so a
/// later reopen is guaranteed to see the whole collection.
fn load(database: &Database<'_>, documents: i64) -> Result<()> {
    let mut start = 0;
    while start < documents {
        let end = (start + BATCH).min(documents);
        let batch = (start..end)
            .map(|id| Bson::Document(doc! { "_id": id, "k": id % BUCKETS, "v": id % BUCKETS }))
            .collect::<Vec<_>>();
        let mut command = doc! { "insert": COLLECTION, "documents": batch };
        if end == documents {
            command.insert("writeConcern", doc! { "w": 1, "j": true });
        }
        database.run_command(&command)?;
        start = end;
    }
    Ok(())
}
