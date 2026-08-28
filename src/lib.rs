mod aggregate;
mod client;
mod collection;
mod database;
mod error;
mod find;
mod insert;
mod limits;
mod options;
mod repair;

pub use bson;
pub use client::Client;
pub use collection::Collection;
pub use database::Database;
// The two limits the native library validates, re-exported rather than redefined: a second
// copy here would be a second place for WiredTiger's bounds to be written down.
pub use embedded_mongodb_sys::{CacheSize, JournalFileSize, OutOfRange, Preallocation};
pub use error::{Error, Result};
pub use find::Cursor;
pub use insert::{InsertManyResult, InsertOneResult};
pub use limits::{FreeDiskFloor, ReportedFloors, free_disk_floors, set_free_disk_floor};
pub use options::OpenOptions;
