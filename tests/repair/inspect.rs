//! Readings the repair test takes off a live engine.
//!
//! Small on purpose: every question these tests ask about a collection is asked through an
//! ordinary command, so what they assert is what a caller would see, not something the crate
//! told them about itself.

use embedded_mongodb::{
    Client,
    bson::{Bson, Document, doc},
};
use std::collections::BTreeSet;

pub fn count(client: &Client, database: &str, collection: &str, query: Document) -> i64 {
    let reply = client
        .database(database)
        .run_command(&doc! { "count": collection, "query": query })
        .unwrap();
    number(&reply, "n")
}

pub fn is_valid(client: &Client, database: &str, collection: &str) -> bool {
    let reply = client
        .database(database)
        .run_command(&doc! { "validate": collection })
        .unwrap();
    reply.get_bool("valid").unwrap_or(false)
}

/// The `customer` field of every document that once shared `_id` 1, wherever it now lives.
///
/// Which of the two the engine keeps in place and which it evicts is its own business; that
/// neither is deleted is the guarantee, so the assertion is over the union of the collection
/// and the lost and found rather than over either one.
pub fn surviving_customers(client: &Client) -> BTreeSet<String> {
    let mut customers = BTreeSet::new();
    for document in find(client, "shop", "orders", doc! { "_id": 1 }) {
        customers.insert(document.get_str("customer").unwrap_or_default().to_owned());
    }

    let collection = lost_and_found(client);
    let Some(collection) = collection.strip_prefix("local.") else {
        panic!("the lost and found collection is not in `local`: {collection}");
    };
    for document in find(client, "local", collection, doc! {}) {
        customers.insert(document.get_str("customer").unwrap_or_default().to_owned());
    }
    customers
}

/// The one `local.lost_and_found.<uuid>` collection the engine created, read back from
/// `listCollections`. The crate composes that name itself, from the collection UUID rather than
/// from the prose warning that also carries it, so the name it logged has to be checked against
/// the collection that actually exists.
pub fn lost_and_found(client: &Client) -> String {
    let listed = client
        .database("local")
        .run_command(&doc! { "listCollections": 1 })
        .unwrap();
    let names = batch(&listed)
        .into_iter()
        .filter_map(|entry| entry.get_str("name").map(ToOwned::to_owned).ok())
        .filter(|name| name.starts_with("lost_and_found."))
        .collect::<Vec<_>>();
    assert_eq!(
        names.len(),
        1,
        "expected exactly one lost and found collection, found {names:?}"
    );
    format!("local.{}", names[0])
}

fn find(client: &Client, database: &str, collection: &str, filter: Document) -> Vec<Document> {
    let reply = client
        .database(database)
        .run_command(&doc! { "find": collection, "filter": filter })
        .unwrap();
    batch(&reply)
}

fn batch(reply: &Document) -> Vec<Document> {
    let cursor = reply.get_document("cursor").unwrap();
    assert_eq!(
        number(cursor, "id"),
        0,
        "these tests read collections small enough to fit one batch"
    );
    cursor
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .filter_map(Bson::as_document)
        .cloned()
        .collect()
}

fn number(document: &Document, key: &str) -> i64 {
    match document.get(key) {
        Some(Bson::Int32(value)) => i64::from(*value),
        Some(Bson::Int64(value)) => *value,
        other => panic!("{key} is not an integer: {other:?}"),
    }
}
