//! Readings a child takes off a live engine or off the data directory itself.

use super::{COLLECTION, child::result};
use anyhow::{Context, Result};
use embedded_mongodb::{
    Database, Error,
    bson::{Bson, Document, doc},
};
use std::{fs, path::Path};

/// `NamespaceNotFound`. A kill can take the collection with it — an unjournalled insert into a
/// collection that did not exist yet loses the implicit `create` along with the documents — so
/// this is one of the answers the probe is here to collect, not a failure to report it.
const NAMESPACE_NOT_FOUND: i64 = 26;

pub fn count(database: &Database<'_>, query: Document) -> Result<i64> {
    let response = database.run_command(&doc! { "count": COLLECTION, "query": query })?;
    number(&response, "n")
}

pub fn number(document: &Document, key: &str) -> Result<i64> {
    match document.get(key) {
        Some(Bson::Int32(value)) => Ok(i64::from(*value)),
        Some(Bson::Int64(value)) => Ok(*value),
        Some(Bson::Double(value)) => Ok(*value as i64),
        other => anyhow::bail!("{key} is not a number: {other:?}"),
    }
}

/// The variant name, so the parent can assert the failure arrived as a typed `Error` and not
/// as a panic or an abort. Deliberately exhaustive: a new variant has to be named here.
pub fn variant(error: &Error) -> &'static str {
    match error {
        Error::Bson(_) => "Bson",
        Error::Closed => "Closed",
        Error::InvalidArgument(_) => "InvalidArgument",
        Error::InvalidResponse(_) => "InvalidResponse",
        Error::Native(_) => "Native",
        Error::NonUtf8Path => "NonUtf8Path",
        Error::Server { .. } => "Server",
    }
}

pub fn report_error(key: &str, error: &Error) {
    result(key, "error");
    result(&format!("{key}_variant"), variant(error));
    // Reported separately from the message so a probe can assert on the code the server chose
    // rather than on the prose around it, which MongoDB is free to reword.
    if let Error::Server {
        code: Some(code), ..
    } = error
    {
        result(&format!("{key}_code"), code);
    }
    result(&format!("{key}_message"), error);
}

/// Open descriptors of this process, which on Linux is the cheapest leak detector there is.
/// Reported as `unavailable` elsewhere rather than guessed at.
pub fn open_descriptors() -> String {
    match fs::read_dir("/proc/self/fd") {
        Ok(entries) => entries.count().to_string(),
        Err(_) => "unavailable".to_owned(),
    }
}

pub fn directory_bytes(directory: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(directory).with_context(|| format!("reading {directory:?}"))? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total += if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}

/// `validate` with `full` walks every index entry back to its document, which is the engine's
/// own answer to "is this index silently wrong".
pub fn report_validation(database: &Database<'_>) -> Result<()> {
    let validation = database.run_command(&doc! { "validate": COLLECTION, "full": true })?;
    result("valid", validation.get_bool("valid").unwrap_or(false));
    result("validated_indexes", number(&validation, "nIndexes")?);
    result("validation_errors", joined(&validation, "errors"));
    result("validation_warnings", joined(&validation, "warnings"));
    Ok(())
}

/// How many documents there are and the lowest and highest `_id` among them, computed by the
/// engine so the probe does not have to stream a large collection back through the cursor to
/// answer three small questions.
pub fn summary(database: &Database<'_>) -> Result<(i64, i64, i64)> {
    let response = database.run_command(&doc! {
        "aggregate": COLLECTION,
        "pipeline": [ doc! {
            "$group": {
                "_id": Bson::Null,
                "total": { "$sum": 1 },
                "lowest": { "$min": "$_id" },
                "highest": { "$max": "$_id" },
            },
        } ],
        "cursor": Document::new(),
    })?;
    let Some(group) = batch(&response)?.into_iter().next() else {
        // An empty collection has no bounds; report a range that makes `holes` come out zero.
        return Ok((0, 1, 0));
    };
    Ok((
        number(&group, "total")?,
        number(&group, "lowest")?,
        number(&group, "highest")?,
    ))
}

/// The same as [`names`], except that a missing collection answers `None` instead of failing.
pub fn optional_names(database: &Database<'_>, command: Document) -> Result<Option<String>> {
    match database.run_command(&command) {
        Ok(response) => Ok(Some(named(&response)?)),
        Err(Error::Server {
            code: Some(NAMESPACE_NOT_FOUND),
            ..
        }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn names(database: &Database<'_>, command: Document) -> Result<String> {
    let response = database.run_command(&command)?;
    named(&response)
}

fn named(response: &Document) -> Result<String> {
    let names = batch(response)?
        .iter()
        .filter_map(|entry| entry.get_str("name").ok())
        .collect::<Vec<_>>()
        .join(",");
    Ok(names)
}

/// The first batch of a cursor-returning command, refusing to answer from a partial one.
fn batch(response: &Document) -> Result<Vec<Document>> {
    let cursor = response.get_document("cursor")?;
    let id = number(cursor, "id")?;
    anyhow::ensure!(id == 0, "cursor {id} did not fit its first batch");
    let documents = cursor
        .get_array("firstBatch")?
        .iter()
        .filter_map(Bson::as_document)
        .cloned()
        .collect();
    Ok(documents)
}

fn joined(document: &Document, key: &str) -> String {
    document
        .get_array(key)
        .map(|values| {
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

/// Which access path the planner picks for a filter. Without it, counts taken "through the
/// index" and counts taken from a collection scan could agree because both were scans, and the
/// comparison would say nothing about the index at all.
pub fn access_path(database: &Database<'_>, filter: Document) -> Result<&'static str> {
    let explained = database.run_command(&doc! {
        "explain": { "find": COLLECTION, "filter": filter },
        "verbosity": "queryPlanner",
    })?;
    let plan = format!("{:?}", explained.get_document("queryPlanner")?);
    Ok(
        if plan.contains("IXSCAN") || plan.contains("IDHACK") || plan.contains("EXPRESS") {
            "INDEXED"
        } else {
            "COLLSCAN"
        },
    )
}
