//! `EMBEDDED_MONGODB_SKIP_INDEX_REPAIR`, end to end.
//!
//! Its own test target because the variable has to be in the environment before anything opens
//! an engine. `set_var` is only sound while no other thread can be reading the environment, and
//! once a client is open the engine has a checkpointer, an eviction pool and more running
//! beside it -- so the variable is set as the first statement of the first test of a process
//! that has opened nothing, and never unset.
//!
//! What is asserted is deliberately the unhelpful half: skipping leaves the damage exactly as
//! it was. A switch that quietly repaired anyway, or that recorded the directory as checked,
//! would be worse than not having one.

#[path = "repair/fixture.rs"]
mod fixture;

use embedded_mongodb::{
    Client,
    bson::{Bson, Document, doc},
};

#[test]
fn the_skip_switch_leaves_a_damaged_directory_alone() {
    // SAFETY: nothing else in this process reads the environment yet. This is the first
    // statement of the only test in this target, no engine has been opened, and libtest has
    // not yet handed any other test a thread.
    unsafe {
        std::env::set_var("EMBEDDED_MONGODB_SKIP_INDEX_REPAIR", "1");
    }

    let scratch = fixture::directory();
    let damaged = scratch.path().join("damaged");
    fixture::unpack_damaged(&damaged);

    let client = Client::new(&damaged).unwrap();

    assert!(
        !fixture::marker_exists(&damaged),
        "a skipped pass recorded the directory as checked, so the next open without the \
         variable set would skip it too"
    );
    let validation = client
        .database("shop")
        .run_command(&doc! { "validate": "orders" })
        .unwrap();
    assert_eq!(
        validation.get_bool("valid").ok(),
        Some(false),
        "the damage was repaired despite the skip switch: {validation:?}"
    );
    assert_eq!(
        validation
            .get_array("missingIndexEntries")
            .map(Vec::len)
            .unwrap_or_default(),
        6,
        "the damaged collection no longer has the entries the fixture was built around"
    );
    // The reading that matters to a caller: two documents are still unreachable through the
    // secondary index while a collection scan still returns them.
    assert_eq!(count(&client, doc! { "customer": "c5" }), 0);
    assert_eq!(count(&client, doc! {}), 7);

    client.close().unwrap();
}

fn count(client: &Client, query: Document) -> i64 {
    let reply = client
        .database("shop")
        .run_command(&doc! { "count": "orders", "query": query })
        .unwrap();
    match reply.get("n") {
        Some(Bson::Int32(value)) => i64::from(*value),
        Some(Bson::Int64(value)) => *value,
        other => panic!("n is not an integer: {other:?}"),
    }
}
