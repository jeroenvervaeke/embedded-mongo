//! That the free-disk floor a caller sets is the floor the engine enforces.
//!
//! Driven from the wrong end on purpose: asking for more free space than any device has is
//! the only way to watch the check fire without a filesystem small enough to fail on, and it
//! fails for exactly the reason a nearly-full phone would. The other direction is the same
//! test run backwards -- the floor is lowered again and the build that was refused succeeds.
//!
//! One test function, because the engine is a process-global singleton.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{
    Client, FreeDiskFloor, IndexBuildFloor, OpenOptions, QuerySpillingFloor, ReportedFloors,
    bson::doc, free_disk_floors,
};

/// Four tebibytes. Larger than the disk under any machine this runs on, so the check cannot
/// pass; small enough that the megabyte count still fits the knob behind it.
const MORE_THAN_ANY_DEVICE_HAS: u32 = 4 * 1024 * 1024;

/// Small enough to be reachable on any machine that can run the rest of the suite.
const REACHABLE: u32 = 32;

/// `ErrorCodes::OutOfDiskSpace`, from src/mongo/base/error_codes.yml.
const OUT_OF_DISK_SPACE: i64 = 14031;

#[test]
fn the_free_disk_floor_decides_whether_an_index_build_starts() {
    let temporary = scratch::directory("storage-limits-");
    let path = temporary.path().join("database");
    let unreachable = FreeDiskFloor::from_mebibytes(MORE_THAN_ANY_DEVICE_HAS)
        .expect("4 TiB is inside the range the floor accepts");

    let client = Client::with_options(&path, OpenOptions::new().free_disk_floor(unreachable))
        .expect("opening an empty directory");

    assert_eq!(
        free_disk_floors(&client).expect("the engine reports its floors"),
        ReportedFloors::new(
            IndexBuildFloor::from_mebibytes(i64::from(MORE_THAN_ANY_DEVICE_HAS)),
            QuerySpillingFloor::from_bytes(i64::from(MORE_THAN_ANY_DEVICE_HAS) * 1024 * 1024),
        ),
        "the floor the client asked for is not the one the engine is running with"
    );

    // The build is only reached once the collection resolves, so it has to exist first.
    let database = client.database("probe");
    database
        .run_command(&doc! { "insert": "places", "documents": [{ "_id": 1, "name": "a" }] })
        .expect("inserting one document");

    let refused = database
        .run_command(&doc! {
            "createIndexes": "places",
            "indexes": [{ "key": { "name": 1 }, "name": "name_1" }],
        })
        .expect_err("an index build cannot start below a 4 TiB floor");
    assert!(
        matches!(&refused, embedded_mongodb::Error::Server { code, .. }
                 if *code == Some(OUT_OF_DISK_SPACE)),
        "the index build failed for some reason other than the free-disk floor: {refused}"
    );

    // And back: the floor is the only thing that was in the way.
    let reachable = FreeDiskFloor::from_mebibytes(REACHABLE).expect("32 MiB is in range");
    embedded_mongodb::set_free_disk_floor(&client, reachable).expect("lowering the floor");
    database
        .run_command(&doc! {
            "createIndexes": "places",
            "indexes": [{ "key": { "name": 1 }, "name": "name_1" }],
        })
        .expect("the same index build, below a floor this device clears");

    client.close().expect("closing cleanly");
}
