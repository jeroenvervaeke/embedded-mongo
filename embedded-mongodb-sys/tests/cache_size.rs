//! That `CacheSize` reaches WiredTiger, asked of WiredTiger rather than inferred.
//!
//! The unit tests next to `EngineOptions` prove a number reaches the FFI struct. They cannot
//! prove it survives `cacheSizeGB`, MongoDB's `getMainCacheSizeMB` and the `cache_size=` it
//! writes into the `wiredtiger_open` string -- that round trip goes through a `double`, and
//! MongoDB applies a 256 MB floor on one of its two branches. `serverStatus` reports what the
//! running engine settled on, which is the only answer that settles it.
//!
//! One test in its own binary, because the engine is one per process.

use bson::{Bson, Document, doc};
use embedded_mongodb_sys::{CacheSize, Client, EngineOptions};

/// Deliberately far below the 256 MB that `getMainCacheSizeMB` imposes when nothing is asked
/// for: a build that lost the request somewhere would report that floor instead, and the
/// assertion would catch it rather than passing on a coincidence.
const REQUESTED_MEBIBYTES: u32 = 48;

#[test]
fn the_cache_size_option_is_the_cache_wiredtiger_reports_running_with() {
    let temporary = tempfile::tempdir().expect("a temporary directory");
    let path = temporary.path().join("database");
    let options = EngineOptions::new()
        .cache_size(CacheSize::from_mebibytes(REQUESTED_MEBIBYTES).expect("48 MiB is in range"));

    let client = Client::open_with_options(path.to_str().expect("a UTF-8 temporary path"), options)
        .expect("opening an empty directory");

    let status = run(&client, doc! { "serverStatus": 1, "wiredTiger": 1 });
    let reported = status
        .get_document("wiredTiger")
        .and_then(|wt| wt.get_document("cache"))
        .ok()
        .and_then(|cache| cache.get("maximum bytes configured"))
        .expect("serverStatus reports the WiredTiger cache ceiling");
    // WiredTiger's statistics are plain counters, so BSON encodes each one at whatever width
    // its current value needs. A cache small enough to fit an i32 arrives as one.
    let configured = match reported {
        Bson::Int32(bytes) => i64::from(*bytes),
        Bson::Int64(bytes) => *bytes,
        other => panic!("the cache ceiling came back as {other:?}, not a number"),
    };

    assert_eq!(
        configured,
        i64::from(REQUESTED_MEBIBYTES) * 1024 * 1024,
        "WiredTiger is running with a different cache than the options asked for"
    );

    client.close().expect("closing cleanly");
}

fn run(client: &Client, command: Document) -> Document {
    let encoded = command.to_vec().expect("encoding a command");
    let response = client
        .run_command("admin", &encoded)
        .expect("the engine answered");
    Document::from_reader(response.as_slice()).expect("decoding the reply")
}
