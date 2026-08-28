//! The three commands the pass sends, and the one place their replies are trusted.
//!
//! `listDatabases` and `listCollections` decide what gets looked at; `validate` does the
//! looking, and with `repair: true` the fixing. Nothing else in this module talks to the
//! engine.

use super::Namespace;
use crate::{Client, Cursor, Error, Result};
use bson::{Bson, Document, doc};

/// Whether `validate` should fix what it finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Check,
    Repair,
}

/// `getMore` on a `listCollections` cursor names this instead of a collection.
const LIST_COLLECTIONS: &str = "$cmd.listCollections";

/// The one `listCollections` type that is not a collection.
const VIEW: &str = "view";

/// Deliberately not `full: true`. Full validation walks every index entry back to its document
/// and costs far more, while the default already reports the missing entries this defect
/// produces, in `_id_` and in secondary indexes alike.
pub(super) fn validate(client: &Client, namespace: &Namespace, mode: Mode) -> Result<Document> {
    let mut command = doc! { "validate": namespace.collection() };
    if mode == Mode::Repair {
        command.insert("repair", true);
    }
    client.run_command(namespace.database(), &command)
}

/// What the directory holds, as far as the engine will say.
pub(super) struct Catalog {
    /// Kept alongside the namespaces because the two disagreeing is diagnostic. A directory
    /// with databases but no collection in any of them is the signature of an engine from
    /// before the `DatabaseHolder::openDb` fix: `listCollections` gates on the database holder,
    /// so an unregistered database lists nothing while `listDatabases` still names it. That is
    /// not an empty directory, and recording it as checked would skip, permanently, exactly the
    /// damage this pass exists for.
    pub(super) databases: usize,
    pub(super) namespaces: Vec<Namespace>,
}

/// Every collection in the directory, resolved before any of them is touched.
///
/// A failure here ends the pass rather than skipping the database it happened in: not being
/// able to enumerate is a statement about the engine, not about one database, and continuing
/// would write a marker over collections that were never looked at.
pub(super) fn namespaces(client: &Client) -> Result<Catalog> {
    let reply = client.run_command("admin", &doc! { "listDatabases": 1, "nameOnly": true })?;
    let Some(Bson::Array(databases)) = reply.get("databases") else {
        return Err(Error::InvalidResponse(
            "listDatabases response has no databases array".to_owned(),
        ));
    };

    let mut namespaces = Vec::new();
    for entry in databases {
        let Some(name) = entry
            .as_document()
            .and_then(|entry| entry.get_str("name").ok())
        else {
            return Err(Error::InvalidResponse(
                "listDatabases response has an entry without a name".to_owned(),
            ));
        };
        namespaces.extend(collections(client, name)?);
    }
    Ok(Catalog {
        databases: databases.len(),
        namespaces,
    })
}

/// Whether a `listCollections` entry names something `validate` should be run on.
///
/// The command answers `collection`, `view` or `timeseries`. Only a view is not a collection:
/// `validate` refuses one outright, so it is dropped here rather than turned into a failure per
/// view further in. A viewless time-series collection comes back as `timeseries` and *is* a
/// real collection -- there is no `system.buckets.*` entry standing in for it -- so it stays.
///
/// Everything unrecognised stays too, a missing `type` included. Dropping is the dangerous
/// direction: a reply shape this does not know would otherwise turn into a pass that checked
/// nothing and still recorded the directory as done, while keeping an entry `validate` cannot
/// handle costs one error, and an error withholds the marker.
fn is_collection(entry: &Document) -> bool {
    !entry.get_str("type").is_ok_and(|kind| kind == VIEW)
}

fn collections(client: &Client, database: &str) -> Result<Vec<Namespace>> {
    let reply = client.run_command(database, &doc! { "listCollections": 1, "nameOnly": true })?;
    // Through the cursor rather than off `firstBatch`, so a directory with more collections
    // than fit one batch is checked to the end instead of halfway.
    let cursor =
        Cursor::<Document>::from_response(client, database, LIST_COLLECTIONS, reply, "firstBatch")?;

    let mut namespaces = Vec::new();
    for entry in cursor {
        let entry = entry?;
        if !is_collection(&entry) {
            continue;
        }
        let Ok(name) = entry.get_str("name") else {
            return Err(Error::InvalidResponse(
                "listCollections response has an entry without a name".to_owned(),
            ));
        };
        namespaces.push(Namespace::new(database, name));
    }
    Ok(namespaces)
}

#[cfg(test)]
mod tests {
    use super::is_collection;
    use bson::doc;

    #[test]
    fn a_plain_collection_is_checked() {
        assert!(is_collection(
            &doc! { "name": "orders", "type": "collection" }
        ));
    }

    /// A view is the one thing `validate` refuses, and the only reason this filter exists.
    #[test]
    fn a_view_is_not_checked() {
        assert!(!is_collection(
            &doc! { "name": "big_orders", "type": "view" }
        ));
    }

    /// A viewless time-series collection reports `timeseries` and is a real collection with no
    /// `system.buckets.*` entry standing in for it. Dropping it would leave it damaged with the
    /// directory recorded as checked.
    #[test]
    fn a_timeseries_collection_is_checked() {
        assert!(is_collection(
            &doc! { "name": "readings", "type": "timeseries" }
        ));
    }

    /// An unfamiliar reply must fail loudly at `validate`, not vanish here: a filter that drops
    /// what it does not recognise would mark a whole directory done having looked at nothing.
    #[test]
    fn an_entry_with_an_unknown_or_absent_type_is_checked() {
        assert!(is_collection(
            &doc! { "name": "orders", "type": "something new" }
        ));
        assert!(is_collection(&doc! { "name": "orders" }));
    }
}
